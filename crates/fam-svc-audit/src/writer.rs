//! The append-only half: one JSON line per call, one file per UTC day, and a chain that makes an
//! edit show up.
//!
//! Writes happen on the calling thread, against a handle this log opened once and kept. There is no
//! buffering and no worker thread, and that is deliberate: a bounded queue in front of a hash chain
//! has to drop records when it fills, and a dropped record is a hole in the very thing the chain is
//! for. The cost of the choice is one `WriteFile` per audited call on the calling thread, which is
//! what the measurement in the note for this increment is about.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::actor::Actor;
use crate::chain::{self, GENESIS};
use crate::record::{clip, epoch_ms, format_utc, utc_date, Draft, Op, Record, Weight};

/// What the files are called, and what `inspect` looks for. The date is UTC, so a file boundary is
/// not the user's midnight.
pub const FILE_PREFIX: &str = "audit-";
pub const FILE_SUFFIX: &str = ".jsonl";

pub fn file_name_for(year: i64, month: u32, day: u32) -> String {
    format!("{FILE_PREFIX}{year:04}-{month:02}-{day:02}{FILE_SUFFIX}")
}

/// Where the audit lives: a directory of its own, never inside anything the tool indexes. Keeping it
/// out of the indexed set is `fam_core::Prune`'s job, which adds this path to the exclusion prefixes
/// so enumeration, the walk and the search router all skip it.
pub fn default_directory(data_dir: &Path) -> PathBuf {
    data_dir.join("audit")
}

/// A clock the writer can be handed, so rotation and timestamps are testable without waiting for
/// midnight.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(SystemTime::now)
}

pub struct Options {
    pub directory: PathBuf,
    pub actor: Actor,
    /// Whether fetching a window of rows and paging a search get a line each. The questions and the
    /// changes are always recorded; the heartbeat never is. See [`Weight`].
    pub record_reads: bool,
    /// Ceiling on what one day's file grows to before the `Read` stream stops.
    pub daily_limit_bytes: u64,
    pub clock: Clock,
}

impl Options {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            actor: Actor::User,
            record_reads: true,
            daily_limit_bytes: 64 * 1024 * 1024,
            clock: system_clock(),
        }
    }
}

/// What the log can say about itself. Surfaced through `fam_stats` so a log that stopped writing is
/// visible without reading the file - which is the only way a full disk does not become silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub session: u64,
    pub file: Option<PathBuf>,
    pub records: u64,
    pub bytes_today: u64,
    pub dropped: u64,
    pub dropped_because: Option<String>,
    pub writing: bool,
    pub last_error: Option<String>,
}

pub struct AuditLog {
    session: u64,
    actor: Actor,
    writer: Mutex<Writer>,
}

struct Writer {
    options: Options,
    file: Option<File>,
    path: Option<PathBuf>,
    day: Option<(i64, u32, u32)>,
    bytes_today: u64,
    seq: u64,
    prev: String,
    dropped: u64,
    dropped_because: Option<&'static str>,
    records: u64,
    last_error: Option<String>,
    /// Whether the console has been told yet. A log that cannot write says so once, because saying
    /// so every time would be an error stream of its own.
    warned: bool,
}

impl AuditLog {
    /// Opens the log and writes the first line.
    ///
    /// Failure is fatal to the caller rather than something to run on without: a program that
    /// browses the filesystem while unable to record it has exactly the property this layer exists
    /// to prevent, so `fam-ffi` propagates the error out of `fam_core_new` and the shell refuses to
    /// start instead of running dark.
    pub fn open(options: Options) -> Result<Self, crate::Error> {
        let session = new_session_id();
        let actor = options.actor.clone();
        let mut writer = Writer {
            options,
            file: None,
            path: None,
            day: None,
            bytes_today: 0,
            seq: 0,
            prev: GENESIS.into(),
            dropped: 0,
            dropped_because: None,
            records: 0,
            last_error: None,
            warned: false,
        };
        // Opening the file is part of opening the log.
        let at = epoch_ms((writer.options.clock)()).ok_or(crate::Error::Clock)?;
        writer.rotate_to(utc_date(at))?;
        let log = Self {
            session,
            actor,
            writer: Mutex::new(writer),
        };
        log.record(Draft::new(Op::SessionOpen).args(format!("pid={}", std::process::id())));
        Ok(log)
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    /// Appends one line, unless the call is one this configuration does not want a line for.
    ///
    /// Neither suppression counts as `dropped`: that counter is for records the log wanted to write
    /// and could not, and mixing the two would make every session with `record_reads = false` look
    /// like one that lost data.
    pub fn record(&self, draft: Draft) {
        let mut writer = self.lock();
        match draft.op().weight() {
            // The heartbeat is never a record. See [`Weight`].
            Weight::Poll => return,
            Weight::Read if !writer.options.record_reads => return,
            _ => {}
        }
        writer.append(&self.actor, self.session, draft);
    }

    pub fn health(&self) -> Health {
        let writer = self.lock();
        writer.health(self.session)
    }

    /// Lock poisoning can only follow a panic an entry point already caught, and a log that wedges
    /// the process over one bad call would be worse than the missing line.
    fn lock(&self) -> std::sync::MutexGuard<'_, Writer> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Writer {
    fn health(&self, session: u64) -> Health {
        Health {
            session,
            file: self.path.clone(),
            records: self.records,
            bytes_today: self.bytes_today,
            dropped: self.dropped,
            dropped_because: self.dropped_because.map(str::to_owned),
            writing: self.file.is_some(),
            last_error: self.last_error.clone(),
        }
    }

    fn append(&mut self, actor: &Actor, session: u64, draft: Draft) {
        let at = match epoch_ms((self.options.clock)()) {
            Some(at) => at,
            // A clock before the epoch cannot be dated, and an undated audit line is worse than a
            // dropped one: it sits in the chain and every link after it inherits the lie.
            None => return self.lost("clock", "the system clock is before 1970"),
        };
        let day = utc_date(at);
        if self.file.is_none() || self.day != Some(day) {
            if let Err(error) = self.rotate_to(day) {
                return self.lost("io", &error.to_string());
            }
        }
        if draft.op().weight() == Weight::Read && self.bytes_today >= self.options.daily_limit_bytes
        {
            self.dropped += 1;
            self.dropped_because = Some("cap");
            return;
        }

        let record = Record {
            seq: self.seq + 1,
            ts: format_utc(at),
            actor: actor.clone(),
            op: draft.op().name().to_owned(),
            target: draft.target,
            args: draft.args,
            result: draft.result,
            session,
            prev: self.prev.clone(),
            dropped: self.dropped,
            dropped_because: self.dropped_because.map(str::to_owned),
        };
        let Ok(mut payload) = record.to_line().map(String::into_bytes) else {
            return self.lost("io", "the record could not be serialised");
        };
        // The link is over the line as written, minus the newline: what the next record will hash to
        // find this one. Nothing advances until the bytes are actually on their way.
        let link = chain::link_of(&payload);
        payload.push(b'\n');
        let wrote = match self.file.as_mut() {
            Some(file) => file.write_all(&payload),
            None => Err(std::io::Error::other("no handle")),
        };
        match wrote {
            Ok(()) => {
                self.seq = record.seq;
                self.prev = link;
                self.records += 1;
                self.bytes_today += payload.len() as u64;
                self.dropped = 0;
                self.dropped_because = None;
            }
            Err(error) => {
                // Drop the handle rather than keep writing into a failed one; the next call opens a
                // fresh one, which is also what recovers if the directory was removed underneath us.
                self.file = None;
                self.lost("io", &format!("write failed: {error}"));
            }
        }
    }

    /// Records that a line was wanted and not written. The count rides on the next record that does
    /// land, so the hole in the log says how big it is and why.
    fn lost(&mut self, because: &'static str, message: &str) {
        self.dropped += 1;
        self.dropped_because = Some(because);
        self.last_error = Some(clip(message, 240));
        if !self.warned {
            self.warned = true;
            log::warn!("audit log is not writing: {message}");
        }
    }

    fn rotate_to(&mut self, day: (i64, u32, u32)) -> Result<(), crate::Error> {
        std::fs::create_dir_all(&self.options.directory).map_err(|source| crate::Error::Io {
            path: self.options.directory.clone(),
            source,
        })?;
        let path = self
            .options
            .directory
            .join(file_name_for(day.0, day.1, day.2));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| crate::Error::Io {
                path: path.clone(),
                source,
            })?;
        // The length already on disk counts against the cap, so a second session in the same day
        // inherits the first one's volume instead of starting a fresh budget.
        let bytes_today = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        self.file = Some(file);
        self.path = Some(path);
        self.day = Some(day);
        self.bytes_today = bytes_today;
        self.last_error = None;
        Ok(())
    }
}

impl std::fmt::Debug for AuditLog {
    /// Says what the log can say about itself, which is the health rather than the file handle.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("actor", &self.actor)
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}

impl Drop for AuditLog {
    fn drop(&mut self) {
        let Ok(writer) = self.writer.get_mut() else {
            // Somebody was mid-append when this handle went away, which the ABI contract rules out.
            // Leaving the session unsealed is the honest outcome: `verify` will say so.
            return;
        };
        // The seal counts itself, so `verify` can compare one number against the lines that survived
        // without knowing which side of the append the count was taken from.
        let summary = format!("records={} dropped={}", writer.records + 1, writer.dropped);
        writer.append(
            &Actor::System,
            self.session,
            Draft::new(Op::SessionClose).args(summary),
        );
    }
}

/// One core handle is one session. The id is what `verify` groups a chain by, so it has to be
/// distinct across processes and across two handles in one process.
fn new_session_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let index = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Three values that change together, mixed so the low bits are not all a timestamp.
    let mut mix =
        (nanos ^ (u64::from(std::process::id()) << 32) ^ index).wrapping_add(0x9e37_79b9_7f4a_7c15);
    mix = (mix ^ (mix >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mix = (mix ^ (mix >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mix ^ (mix >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_ONE: u64 = 1_758_600_000_000;
    const DAY_TWO: u64 = DAY_ONE + 86_400_000;

    fn clock_at(millis: Arc<AtomicU64>) -> Clock {
        Arc::new(move || {
            UNIX_EPOCH + std::time::Duration::from_millis(millis.load(Ordering::SeqCst))
        })
    }

    struct Scratch {
        root: tempfile::TempDir,
    }

    impl Scratch {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().expect("tempdir"),
            }
        }

        fn directory(&self) -> PathBuf {
            self.root.path().join("audit")
        }

        fn options(&self, millis: Arc<AtomicU64>) -> Options {
            Options {
                directory: self.directory(),
                actor: Actor::User,
                record_reads: true,
                daily_limit_bytes: 64 * 1024 * 1024,
                clock: clock_at(millis),
            }
        }

        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.directory())
                .expect("the audit directory exists")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX))
                .collect();
            names.sort();
            names
        }

        /// Every line in the log, oldest file first.
        fn lines(&self) -> Vec<String> {
            let mut out = Vec::new();
            for name in self.files() {
                let text = std::fs::read_to_string(self.directory().join(name)).expect("readable");
                out.extend(text.lines().map(str::to_owned));
            }
            out
        }
    }

    fn at(millis: u64) -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(millis))
    }

    fn record_at(lines: &[String], index: usize) -> Record {
        serde_json::from_str(&lines[index]).expect("a record")
    }

    fn records(lines: &[String]) -> Vec<Record> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("a record"))
            .collect()
    }

    #[test]
    fn opening_writes_a_first_line_that_chains_to_nothing() {
        let scratch = Scratch::new();
        let log = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        let lines = scratch.lines();
        assert_eq!(lines.len(), 1);
        let record = record_at(&lines, 0);
        assert_eq!(record.seq, 1);
        assert_eq!(record.prev, GENESIS);
        assert_eq!(record.op, "session.open");
        assert_eq!(record.ts, "2025-09-23T04:00:00.000Z");
        assert_eq!(record.session, log.session());
        assert_eq!(record.actor, Actor::User);
        assert_eq!(scratch.files(), vec!["audit-2025-09-23.jsonl"]);
    }

    #[test]
    fn closing_seals_the_session_with_what_it_wrote() {
        let scratch = Scratch::new();
        let session;
        {
            let log = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
            session = log.session();
            log.record(Draft::new(Op::DirOpen).target(Path::new("d:/x")));
            log.record(Draft::new(Op::DirFetch).args("0+80"));
        }
        let lines = scratch.lines();
        let last = record_at(&lines, lines.len() - 1);
        assert_eq!(last.op, "session.close");
        assert_eq!(last.actor, Actor::System);
        assert_eq!(last.args.as_deref(), Some("records=4 dropped=0"));
        assert_eq!(last.seq, 4);
        assert_eq!(last.session, session);
    }

    #[test]
    fn each_line_chains_to_the_one_before_it() {
        let scratch = Scratch::new();
        let log = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        for index in 0..5 {
            log.record(Draft::new(Op::DirOpen).args(index));
        }
        let lines = scratch.lines();
        assert_eq!(lines.len(), 6);
        assert_eq!(record_at(&lines, 0).prev, GENESIS);
        for index in 1..lines.len() {
            assert_eq!(
                record_at(&lines, index).prev,
                chain::link_of(lines[index - 1].as_bytes()),
                "link at {index}"
            );
        }
    }

    /// The rotation claim: a new day is a new file, and the chain does not restart - which is what
    /// makes a whole removed file something `verify` can notice.
    #[test]
    fn a_new_day_is_a_new_file_in_the_same_chain() {
        let scratch = Scratch::new();
        let millis = at(DAY_ONE);
        let log = AuditLog::open(scratch.options(millis.clone())).expect("opens");
        log.record(Draft::new(Op::DirOpen).target(Path::new("d:/one")));
        millis.store(DAY_TWO, Ordering::SeqCst);
        log.record(Draft::new(Op::DirOpen).target(Path::new("d:/two")));

        assert_eq!(
            scratch.files(),
            vec!["audit-2025-09-23.jsonl", "audit-2025-09-24.jsonl"]
        );
        let first = std::fs::read_to_string(scratch.directory().join("audit-2025-09-23.jsonl"))
            .expect("readable");
        assert_eq!(first.lines().count(), 2);
        let lines = scratch.lines();
        assert_eq!(record_at(&lines, 2).seq, 3);
        assert_eq!(
            record_at(&lines, 2).prev,
            chain::link_of(lines[1].as_bytes()),
            "the chain crossed the file boundary"
        );
    }

    /// The volume control, both halves: the reads stop, the questions do not, and nothing is
    /// reported as dropped because a policy chose not to write it.
    #[test]
    fn record_reads_off_keeps_the_asks_and_says_nothing_was_lost() {
        let scratch = Scratch::new();
        let mut options = scratch.options(at(DAY_ONE));
        options.record_reads = false;
        let log = AuditLog::open(options).expect("opens");
        log.record(Draft::new(Op::DirFetch).args("0+80"));
        log.record(Draft::new(Op::DirState));
        log.record(Draft::new(Op::DirOpen).target(Path::new("d:/x")));
        let lines = scratch.lines();
        assert_eq!(lines.len(), 2, "the opening line plus the one ask");
        assert_eq!(record_at(&lines, 1).op, "dir.open");
        assert_eq!(record_at(&lines, 1).dropped, 0);
        assert_eq!(record_at(&lines, 1).dropped_because, None);
    }

    /// The heartbeat is not evidence. Four beats a second per open column, each one asking whether
    /// anything has changed and most of them being told no: recorded, that is the largest single
    /// writer in the file and the least informative line in it.
    #[test]
    fn the_poll_never_gets_a_line_even_with_record_reads_on() {
        let scratch = Scratch::new();
        let log = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        for _ in 0..20 {
            log.record(Draft::new(Op::DirPoll).args("token=1"));
            log.record(Draft::new(Op::DirState).args("token=1"));
        }
        log.record(Draft::new(Op::DirFetch).args("token=1 0+60"));
        let lines = scratch.lines();
        assert_eq!(lines.len(), 2, "the opening line plus the one fetch");
        assert_eq!(record_at(&lines, 1).op, "dir.fetch");
        assert_eq!(
            record_at(&lines, 1).dropped,
            0,
            "a call that was never going to be a line is not a lost line"
        );
    }

    #[test]
    fn a_full_day_stops_the_read_stream_and_says_how_much_it_lost() {
        let scratch = Scratch::new();
        let mut options = scratch.options(at(DAY_ONE));
        options.daily_limit_bytes = 400;
        let log = AuditLog::open(options).expect("opens");
        for index in 0..40 {
            log.record(Draft::new(Op::DirFetch).args(format!("{index}+80")));
        }
        let before_cap = scratch.lines();
        assert!(
            before_cap.len() < 8,
            "the cap should have stopped the stream, saw {}",
            before_cap.len()
        );
        assert!(
            records(&before_cap)
                .iter()
                .all(|record| record.dropped == 0),
            "nothing has landed since the first suppression, so nothing can carry the count yet"
        );

        // An ask still lands after the cap: a change that leaves no trace is the whole thing this
        // log exists to prevent, and it carries the suppression count with it.
        log.record(Draft::new(Op::TagAdd).target(Path::new("d:/x")));
        let lines = scratch.lines();
        let carrier = record_at(&lines, lines.len() - 1);
        assert_eq!(carrier.op, "tag.add");
        assert!(carrier.dropped > 30, "carried {}", carrier.dropped);
        assert_eq!(carrier.dropped_because.as_deref(), Some("cap"));
    }

    /// The failure a full or read-only disk actually produces: the writes stop, every later call
    /// counts itself lost, and the next line that lands carries the total. Recovery is not a
    /// special case - the handle is dropped, so the following call opens one again.
    #[test]
    fn a_write_failure_is_counted_and_the_log_recovers_on_its_own() {
        let scratch = Scratch::new();
        let log = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        let blocking = scratch.root.path().join("in-the-way");
        std::fs::write(&blocking, b"not a directory").expect("a file");
        {
            let mut writer = log.lock();
            writer.file = None;
            writer.options.directory = blocking.clone();
        }
        for _ in 0..3 {
            log.record(Draft::new(Op::DirOpen).target(Path::new("d:/x")));
        }
        let health = log.health();
        assert!(!health.writing);
        assert_eq!(health.dropped, 3);
        assert_eq!(health.dropped_because.as_deref(), Some("io"));
        assert_eq!(health.records, 1, "only the opening line got through");
        let reason = health.last_error.expect("a reason");
        assert!(
            reason.contains("in-the-way"),
            "the message names what went wrong: {reason}"
        );

        {
            let mut writer = log.lock();
            writer.options.directory = scratch.directory();
        }
        log.record(Draft::new(Op::DirOpen).target(Path::new("d:/y")));
        let lines = scratch.lines();
        assert_eq!(lines.len(), 2, "the third failure was the last one");
        let carried = record_at(&lines, 1);
        assert_eq!(carried.dropped, 3);
        assert_eq!(carried.dropped_because.as_deref(), Some("io"));
        assert_eq!(carried.seq, 2, "a record that never landed leaves no gap");
    }

    #[test]
    fn an_unwritable_directory_refuses_to_open_the_log() {
        let scratch = Scratch::new();
        std::fs::write(scratch.directory(), b"not a directory").expect("a file in the way");
        let error = AuditLog::open(scratch.options(at(DAY_ONE))).expect_err("must fail");
        assert!(matches!(error, crate::Error::Io { .. }), "got {error:?}");
        assert!(
            error.to_string().contains("audit"),
            "the message names the path: {error}"
        );
    }

    /// Two handles in one process are two sessions, each with its own chain. A shared `prev` would
    /// let a second writer fork the first one's links, which `verify` could not tell from tampering.
    #[test]
    fn two_logs_in_one_process_chain_independently() {
        let scratch = Scratch::new();
        let first = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        let second = AuditLog::open(scratch.options(at(DAY_ONE))).expect("opens");
        assert_ne!(first.session(), second.session());
        first.record(Draft::new(Op::DirOpen).target(Path::new("d:/a")));
        second.record(Draft::new(Op::DirOpen).target(Path::new("d:/b")));
        let lines = scratch.lines();
        let all = records(&lines);
        assert_eq!(all.len(), 4);
        for session in [first.session(), second.session()] {
            let own: Vec<&Record> = all
                .iter()
                .filter(|record| record.session == session)
                .collect();
            assert_eq!(own.len(), 2, "one open and one ask per session");
            assert_eq!(own[0].seq, 1, "each session starts its own chain");
            assert_eq!(own[0].prev, GENESIS);
            assert_eq!(own[1].seq, 2);
            assert_eq!(
                own[1].prev,
                chain::link_of(own[0].to_line().unwrap().as_bytes()),
                "the link is to the line this session wrote before"
            );
        }
    }

    #[test]
    fn a_session_id_is_not_just_the_clock() {
        let mut ids: Vec<u64> = (0..64).map(|_| new_session_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 64, "64 ids from one nanosecond of one process");
    }
}
