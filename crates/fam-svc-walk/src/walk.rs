use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fam_core::{path_buf_from_units, path_key, Cancellation, Config, Kind};
use fam_svc_enumerate::{list_one_level, RawItem};

pub const DEFAULT_MAX_ENTRIES: usize = 200_000;
pub const DEFAULT_THREADS: usize = 10;

const QUEUE_POLL: Duration = Duration::from_micros(400);
/// Backstop for the deadline check only. Completion is signalled by the channel going
/// `Disconnected` once the last worker exits, so a finished walk no longer waits a full poll.
const DEADLINE_POLL: Duration = Duration::from_millis(25);

/// Queue depth per worker at which extra parallelism starts paying for itself. A flat directory
/// queues no children at all, so opening one never creates a thread beyond the first.
const GROW_QUEUE: usize = 4;

#[derive(Debug, Clone)]
pub struct WalkPolicy {
    pub roots: Vec<PathBuf>,
    /// `path_key` prefixes; anything at or below one of them is skipped.
    pub excluded_prefixes: Vec<String>,
    /// Upper-cased directory names skipped at any depth.
    pub excluded_names: Vec<String>,
    pub threads: usize,
    pub max_entries: usize,
    pub include_directories: bool,
}

impl Default for WalkPolicy {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            excluded_prefixes: Vec::new(),
            excluded_names: Vec::new(),
            threads: DEFAULT_THREADS,
            max_entries: DEFAULT_MAX_ENTRIES,
            include_directories: true,
        }
    }
}

impl WalkPolicy {
    pub fn from_config(config: &Config) -> Self {
        Self {
            roots: config.search_roots.clone(),
            excluded_prefixes: config.excluded_dir_keys(),
            excluded_names: config.excluded_name_keys(),
            threads: config.walk_threads.max(1),
            ..Default::default()
        }
    }

    pub fn rooted_at(mut self, dir: impl AsRef<Path>) -> Self {
        self.roots = vec![dir.as_ref().to_path_buf()];
        self
    }

    pub fn without_directories(mut self) -> Self {
        self.include_directories = false;
        self
    }

    /// True when a walk would never reach `path`: either it sits under an excluded prefix, or one
    /// of its components carries an excluded directory name. The walk itself prunes at the
    /// directory level, which is cheaper than asking per file.
    pub fn excludes(&self, path: &Path) -> bool {
        self.prefix_excluded(&path_key(path))
            || path.components().any(|component| {
                let name = component.as_os_str().to_string_lossy().to_uppercase();
                self.excluded_names.contains(&name)
            })
    }

    fn prefix_excluded(&self, key: &str) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| fam_core::is_within(prefix, key))
    }

    fn name_excluded(&self, path: &Path) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        let name = name.to_string_lossy().to_uppercase();
        self.excluded_names.contains(&name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub modified_ms: Option<i64>,
}

impl Entry {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Assembled from an enumeration record whose name the caller has already joined onto its
    /// parent. Everything here except that join is already known, which is the point of reading
    /// through `fam-svc-enumerate` instead of `std::fs`.
    fn from_raw(path: PathBuf, raw: &RawItem<'_>) -> Self {
        Self {
            path,
            kind: raw.kind,
            size: raw.size,
            modified_ms: (raw.mtime_ms != i64::MIN).then_some(raw.mtime_ms),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkReport {
    pub emitted: u64,
    pub directories: u64,
    pub skipped: u64,
    pub errors: u64,
    pub cancelled: bool,
    pub timed_out: bool,
    pub truncated: bool,
    pub elapsed_ms: u64,
}

impl WalkReport {
    pub fn stopped_early(&self) -> bool {
        self.cancelled || self.timed_out || self.truncated
    }
}

/// Walks `policy.roots` breadth-first across worker threads and calls `on_entry` on the caller's
/// thread for each hit. Returning `false` from `on_entry` stops the walk.
pub fn walk<F>(
    policy: &WalkPolicy,
    cancel: &Cancellation,
    timeout: Duration,
    mut on_entry: F,
) -> WalkReport
where
    F: FnMut(&Entry) -> bool,
{
    let started = Instant::now();
    let deadline = started + timeout;
    let mut seed = VecDeque::new();
    let mut root_errors = 0u64;
    for root in &policy.roots {
        if is_walkable_dir(root) {
            seed.push_back(root.clone());
        } else {
            root_errors += 1;
        }
    }

    let max_workers = policy.threads.clamp(1, 32);
    let seeded = seed.len();
    let shared = Arc::new(Shared {
        queue: Mutex::new(seed),
        active: AtomicUsize::new(0),
        directories: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        spawned: AtomicUsize::new(0),
        max_workers,
        siblings: Mutex::new(Vec::new()),
    });

    let (tx, rx) = mpsc::channel::<Entry>();
    // One worker per seeded directory, capped by the policy; more fork themselves once the queue of
    // discovered directories backs up.
    let workers = start_workers(policy, cancel, &shared, tx, seeded.max(1).min(max_workers));

    let mut report = WalkReport::default();
    loop {
        if cancel.is_stopped() {
            report.cancelled = true;
            break;
        }
        if Instant::now() >= deadline {
            report.timed_out = true;
            break;
        }
        match rx.recv_timeout(DEADLINE_POLL) {
            Ok(entry) => {
                if !on_entry(&entry) {
                    report.cancelled = true;
                    break;
                }
                report.emitted += 1;
                if report.emitted as usize >= policy.max_entries {
                    report.truncated = true;
                    break;
                }
            }
            // Every worker holds a sender and the caller holds none, so this fires as soon as the
            // last one exits — the walk no longer ends on a poll timeout.
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if shared.idle() {
                    break;
                }
            }
        }
    }

    shared.stop.store(true, Ordering::Relaxed);
    drain_joins(&shared);
    for worker in workers {
        let _ = worker.join();
    }

    report.directories = shared.directories.load(Ordering::Relaxed);
    report.skipped = shared.skipped.load(Ordering::Relaxed);
    report.errors = root_errors + shared.errors.load(Ordering::Relaxed);
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

/// Joins self-spawned workers, including siblings that forked after the first drain, so the
/// counters read by the caller are final.
fn drain_joins(shared: &Shared) {
    loop {
        let batch: Vec<thread::JoinHandle<()>> = shared
            .siblings
            .lock()
            .expect("walk sibling handles")
            .drain(..)
            .collect();
        if batch.is_empty() {
            return;
        }
        for handle in batch {
            let _ = handle.join();
        }
    }
}

/// Convenience wrapper collecting everything into a `Vec`.
pub fn collect(
    policy: &WalkPolicy,
    cancel: &Cancellation,
    timeout: Duration,
) -> (Vec<Entry>, WalkReport) {
    let mut entries = Vec::new();
    let report = walk(policy, cancel, timeout, |entry| {
        entries.push(entry.clone());
        true
    });
    (entries, report)
}

struct Shared {
    queue: Mutex<VecDeque<PathBuf>>,
    active: AtomicUsize,
    directories: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
    stop: AtomicBool,
    /// Workers alive or starting, including siblings forked from inside the loop.
    spawned: AtomicUsize,
    max_workers: usize,
    siblings: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl Shared {
    fn idle(&self) -> bool {
        self.active.load(Ordering::Relaxed) == 0
            && self.queue.lock().expect("walk queue").is_empty()
    }

    fn next_dir(&self) -> Option<PathBuf> {
        let mut queue = self.queue.lock().expect("walk queue");
        let dir = queue.pop_front()?;
        self.active.fetch_add(1, Ordering::Relaxed);
        Some(dir)
    }

    fn push_dir(&self, path: PathBuf) -> usize {
        let mut queue = self.queue.lock().expect("walk queue");
        queue.push_back(path);
        queue.len()
    }

    fn pending(&self) -> usize {
        self.queue.lock().expect("walk queue").len()
    }
}

/// Everything a worker needs to keep going, and to fork a sibling.
struct Ctx {
    shared: Arc<Shared>,
    policy: Arc<WalkPolicy>,
    cancel: Cancellation,
    sender: Sender<Entry>,
}

fn start_workers(
    policy: &WalkPolicy,
    cancel: &Cancellation,
    shared: &Arc<Shared>,
    first_sender: Sender<Entry>,
    initial: usize,
) -> Vec<thread::JoinHandle<()>> {
    let worker_policy = Arc::new(WalkPolicy {
        roots: Vec::new(),
        excluded_prefixes: policy.excluded_prefixes.clone(),
        excluded_names: policy.excluded_names.clone(),
        threads: shared.max_workers,
        max_entries: policy.max_entries,
        include_directories: policy.include_directories,
    });
    let mut senders = vec![first_sender];
    for _ in 1..initial.max(1) {
        let clone = senders[0].clone();
        senders.push(clone);
    }
    senders
        .into_iter()
        .map(|sender| {
            shared.spawned.fetch_add(1, Ordering::Relaxed);
            let ctx = Ctx {
                shared: Arc::clone(shared),
                policy: Arc::clone(&worker_policy),
                cancel: cancel.clone(),
                sender,
            };
            thread::spawn(move || worker_loop(ctx))
        })
        .collect()
}

fn worker_loop(ctx: Ctx) {
    while !ctx.shared.stop.load(Ordering::Relaxed) && !ctx.cancel.is_stopped() {
        let Some(dir) = ctx.shared.next_dir() else {
            if ctx.shared.idle() {
                return;
            }
            thread::sleep(QUEUE_POLL);
            continue;
        };
        // One `FindFirstFileExW`/`FindNextFileW` chain per directory. The record it fills already
        // carries size, timestamps and attributes, so no second query is issued for a child —
        // that pairing is what used to put a large listing at ~2.9us per entry.
        match list_one_level(&dir, &ctx.cancel, &mut |raw| ctx.handle_raw(&dir, raw)) {
            Ok(report) => {
                if report.errors > 0 {
                    ctx.shared
                        .errors
                        .fetch_add(report.errors, Ordering::Relaxed);
                }
            }
            Err(_) => {
                ctx.shared.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        ctx.shared.active.fetch_sub(1, Ordering::Relaxed);
        ctx.grow_if_needed();
    }
}

impl Ctx {
    fn handle_raw(&self, parent: &Path, raw: RawItem<'_>) -> bool {
        if self.shared.stop.load(Ordering::Relaxed) || self.cancel.is_stopped() {
            return false;
        }
        let path = parent.join(path_buf_from_units(raw.name));
        let is_directory = raw.flags.is_directory();
        // Both predicates allocate — an uppercase fold and a path key — so they are skipped
        // entirely when nothing is configured to exclude.
        if is_directory
            && !self.policy.excluded_names.is_empty()
            && self.policy.name_excluded(&path)
        {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if !self.policy.excluded_prefixes.is_empty()
            && self.policy.prefix_excluded(&path_key(&path))
        {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        // A reparse point — junction or directory symlink — is reported but never descended into.
        // The walk keeps no visited set, so following one can loop forever, and the enumeration
        // record already carries the attribute that says so.
        if is_directory {
            if !raw.flags.is_reparse() {
                self.shared.directories.fetch_add(1, Ordering::Relaxed);
                self.shared.push_dir(path.clone());
            }
            if !self.policy.include_directories {
                return true;
            }
        }
        let entry = Entry::from_raw(path, &raw);
        let _ = self.sender.send(entry);
        true
    }

    /// Recruit up to one worker per `GROW_QUEUE` queued directories, so a wide directory reaches its
    /// parallelism immediately while a flat one stays single-threaded.
    fn grow_if_needed(&self) {
        let pending = self.shared.pending();
        let wanted = pending
            .div_ceil(GROW_QUEUE)
            .clamp(1, self.shared.max_workers);
        while self.shared.spawned.load(Ordering::Relaxed) < wanted {
            if !self.try_fork() {
                return;
            }
        }
    }

    fn try_fork(&self) -> bool {
        let previous = self.shared.spawned.fetch_add(1, Ordering::Relaxed);
        if previous >= self.shared.max_workers {
            self.shared.spawned.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        let sibling = Ctx {
            shared: Arc::clone(&self.shared),
            policy: Arc::clone(&self.policy),
            cancel: self.cancel.clone(),
            sender: self.sender.clone(),
        };
        let handle = thread::spawn(move || worker_loop(sibling));
        self.shared
            .siblings
            .lock()
            .expect("walk sibling handles")
            .push(handle);
        true
    }
}

fn is_walkable_dir(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_exclusions_match_names_and_prefixes_across_separators() {
        let policy = WalkPolicy {
            excluded_names: vec!["NODE_MODULES".to_string()],
            excluded_prefixes: vec![path_key(Path::new(r"D:\cache"))],
            ..Default::default()
        };
        assert!(policy.excludes(Path::new("D:/proj/node_modules/pkg/index.js")));
        assert!(policy.excludes(Path::new("D:/cache/tmp.bin")));
        assert!(!policy.excludes(Path::new("D:/proj/src/main.rs")));
        assert!(!policy.excludes(Path::new("D:/cache_sibling/x")));
    }

    #[test]
    fn a_finished_walk_does_not_wait_for_the_poll_interval() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("only.txt"), "x").unwrap();
        let policy = WalkPolicy::default().rooted_at(dir.path());

        let started = Instant::now();
        let (entries, report) = collect(&policy, &Cancellation::new(), Duration::from_secs(30));
        let elapsed = started.elapsed();

        assert_eq!(entries.len(), 1);
        assert_eq!(report.emitted, 1);
        assert!(
            elapsed < DEADLINE_POLL,
            "a one-entry walk took {elapsed:?}, so completion is still detected by timeout"
        );
    }

    #[test]
    fn deep_queues_recruit_more_workers_than_a_flat_start() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..GROW_QUEUE * 3 {
            fs::create_dir_all(dir.path().join(format!("branch-{index:02}"))).unwrap();
        }
        let policy = WalkPolicy {
            threads: 4,
            ..WalkPolicy::default().rooted_at(dir.path())
        };

        let (_, report) = collect(&policy, &Cancellation::new(), Duration::from_secs(30));

        assert_eq!(report.directories, (GROW_QUEUE * 3) as u64);
        assert!(!report.stopped_early());
    }
}
