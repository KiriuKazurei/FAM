//! The read path. Only `fam audit` links this - `fam-ffi` never does, so nothing a shell or an agent
//! can call reaches it, and the log is write-only from the inside.
//!
//! [`verify`] is the reason the chain exists: an audit trail nobody checks is a file that gets
//! quietly shorter.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::chain::{self, GENESIS};
use crate::record::Record;
use crate::writer::{FILE_PREFIX, FILE_SUFFIX};

/// Where a line of the log disagrees with the log around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Not a record at all: a hand-edited line, or a partial write from a power cut mid-append.
    Unparsable { detail: String },
    /// A session's first line links to something other than nothing.
    DoesNotStartAtGenesis { found: String },
    /// The previous line of this session is not what this line says it is.
    BrokenLink { expected: String, found: String },
    /// A gap in the per-session numbering, which is what a deleted line looks like from here.
    Sequence { expected: u64, found: u64 },
    /// The session never wrote its seal: a crash, a kill, or a removed tail.
    Unsealed,
    /// The seal's own count disagrees with how many lines survived. This is the finding that catches
    /// a rewritten history: the links all match because whoever edited the file recomputed them, and
    /// the count is the one thing they had no reason to touch.
    Seal { expected: u64, found: u64 },
}

impl Problem {
    fn describe(&self) -> String {
        match self {
            Self::Unparsable { detail } => format!("unreadable line: {detail}"),
            Self::DoesNotStartAtGenesis { found } => {
                format!("first line of the session links to {found}, not {GENESIS}")
            }
            Self::BrokenLink { expected, found } => format!(
                "link is {found}, should be {expected} - this line or the one before it was rewritten"
            ),
            Self::Sequence { expected, found } => {
                format!("record number is {found}, should be {expected} - a line is missing")
            }
            Self::Unsealed => {
                "the session has no closing record: it crashed, or its tail was removed".into()
            }
            Self::Seal { expected, found } => {
                format!("the closing record counts {expected} lines, {found} are here")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// `None` for a line that is not a record, which is the only case that has no session.
    pub session: Option<u64>,
    pub file: PathBuf,
    /// One-based, so it points at a line a reader can find in an editor.
    pub line: usize,
    pub problem: Problem,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let session = match self.session {
            Some(session) => session.to_string(),
            None => "?".into(),
        };
        write!(
            f,
            "{}:{} session {session}: {}",
            self.file.display(),
            self.line,
            self.problem.describe()
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub files: usize,
    pub records: usize,
    pub sessions: usize,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn intact(&self) -> bool {
        self.findings.is_empty()
    }
}

/// The audit files, oldest first. The name is a UTC date, so sorting the names sorts the days.
pub fn files(directory: &Path) -> Result<Vec<PathBuf>, crate::Error> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(crate::Error::Missing {
                path: directory.to_path_buf(),
            })
        }
        Err(source) => {
            return Err(crate::Error::Io {
                path: directory.to_path_buf(),
                source,
            })
        }
    };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX)).then(|| entry.path())
        })
        .collect();
    names.sort();
    Ok(names)
}

/// The last `count` lines, oldest first, unparsed.
///
/// This is the one read path that does not need to understand the log, which is what makes it usable
/// on a file `verify` has to complain about.
pub fn tail(directory: &Path, count: usize) -> Result<Vec<String>, crate::Error> {
    let mut lines: Vec<String> = Vec::new();
    for file in files(directory)?.into_iter().rev() {
        let text = read(&file)?;
        let mut here: Vec<String> = text.lines().map(str::to_owned).collect();
        here.reverse();
        let take = count.saturating_sub(lines.len()).min(here.len());
        lines.extend(here.into_iter().take(take));
        if lines.len() >= count {
            break;
        }
    }
    lines.reverse();
    Ok(lines)
}

/// Every line in the log with the place it came from, oldest file first.
fn read(path: &Path) -> Result<String, crate::Error> {
    fs::read_to_string(path).map_err(|source| crate::Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Recompute every link and every count the log claims.
///
/// The checks are per session, which is what makes them survive two programs writing one day's
/// file: a shared chain would fork the moment two sessions interleaved, and a fork and a tampering
/// would look identical from here.
pub fn verify(directory: &Path) -> Result<Report, crate::Error> {
    let paths = files(directory)?;
    let mut report = Report {
        files: paths.len(),
        ..Report::default()
    };
    // Which lines belong to which session, as file-and-number positions. A `BTreeMap` keyed by
    // session id keeps the findings in the order the sessions started, so two runs of `verify` on
    // the same file print the same thing.
    let mut lines: Vec<(PathBuf, usize, String)> = Vec::new();
    for path in &paths {
        for (index, raw) in read(path)?.lines().enumerate() {
            lines.push((path.clone(), index + 1, raw.to_owned()));
        }
    }
    let mut sessions: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (index, (_, _, raw)) in lines.iter().enumerate() {
        match serde_json::from_str::<Record>(raw) {
            Ok(record) => {
                report.records += 1;
                sessions.entry(record.session).or_default().push(index);
            }
            // Reported and then left out of every session. The line after it will also fail its
            // link, and it should: the bytes it hashes to are bytes nobody can read.
            Err(error) => report.findings.push(Finding {
                session: None,
                file: lines[index].0.clone(),
                line: lines[index].1,
                problem: Problem::Unparsable {
                    detail: error.to_string(),
                },
            }),
        }
    }
    report.sessions = sessions.len();

    for (session, group) in sessions {
        let mut previous: Option<usize> = None;
        for current in group.iter().copied() {
            let record = record_of(&lines[current]);
            if let Some(before) = previous {
                let expected = chain::link_of(lines[before].2.as_bytes());
                if record.prev != expected {
                    report.findings.push(Finding {
                        session: Some(session),
                        file: lines[current].0.clone(),
                        line: lines[current].1,
                        problem: Problem::BrokenLink {
                            expected,
                            found: record.prev.clone(),
                        },
                    });
                }
                let expected_seq = record_of(&lines[before]).seq + 1;
                if record.seq != expected_seq {
                    report.findings.push(Finding {
                        session: Some(session),
                        file: lines[current].0.clone(),
                        line: lines[current].1,
                        problem: Problem::Sequence {
                            expected: expected_seq,
                            found: record.seq,
                        },
                    });
                }
            } else if record.prev != GENESIS {
                report.findings.push(Finding {
                    session: Some(session),
                    file: lines[current].0.clone(),
                    line: lines[current].1,
                    problem: Problem::DoesNotStartAtGenesis {
                        found: record.prev.clone(),
                    },
                });
            }
            previous = Some(current);
        }

        let last = *group.last().expect("a session has at least one line");
        let record = record_of(&lines[last]);
        if record.op != "session.close" {
            report.findings.push(Finding {
                session: Some(session),
                file: lines[last].0.clone(),
                line: lines[last].1,
                problem: Problem::Unsealed,
            });
            continue;
        }
        // A seal this version cannot read is not a clean bill of health: reading it as zero lines
        // makes the session fail the count check below.
        let sealed = seal_count(&record).unwrap_or(0);
        if sealed != group.len() as u64 {
            report.findings.push(Finding {
                session: Some(session),
                file: lines[last].0.clone(),
                line: lines[last].1,
                problem: Problem::Seal {
                    expected: sealed,
                    found: group.len() as u64,
                },
            });
        }
    }
    Ok(report)
}

fn record_of(line: &(PathBuf, usize, String)) -> Record {
    serde_json::from_str(&line.2).expect("only parsed lines are indexed by session")
}

/// The line count written into a session's seal: how many records that session believes it wrote,
/// the seal included.
fn seal_count(record: &Record) -> Option<u64> {
    let args = record.args.as_deref()?;
    args.split_whitespace()
        .find_map(|pair| pair.strip_prefix("records="))
        .and_then(|text| text.parse().ok())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub files: Vec<FileSummary>,
    pub records: u64,
    pub bytes: u64,
    pub sessions: usize,
    pub sealed_sessions: usize,
    pub per_op: Vec<(String, u64)>,
    pub per_actor: Vec<(String, u64)>,
    pub per_target: Vec<(String, u64)>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    /// Lines the writers gave up on, as reported by the records that carried the count forward.
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    pub file: PathBuf,
    pub bytes: u64,
    pub records: u64,
}

impl Summary {
    pub fn lines(&self) -> Vec<String> {
        let per_record = if self.records == 0 {
            0.0
        } else {
            self.bytes as f64 / self.records as f64
        };
        let mut out = vec![format!(
            "{} files, {} records, {} bytes ({per_record:.0} bytes per record)",
            self.files.len(),
            self.records,
            self.bytes
        )];
        for file in &self.files {
            out.push(format!(
                "  {} - {} records, {} bytes",
                file.file.display(),
                file.records,
                file.bytes
            ));
        }
        out.push(format!(
            "{} sessions, {} sealed, {} records reported dropped",
            self.sessions, self.sealed_sessions, self.dropped
        ));
        if let (Some(first), Some(last)) = (&self.first_ts, &self.last_ts) {
            out.push(format!("span {first} .. {last}"));
        }
        out.push("by op".into());
        out.extend(self.rows(&self.per_op));
        out.push("by actor".into());
        out.extend(self.rows(&self.per_actor));
        out.push("top targets".into());
        out.extend(self.rows(&self.per_target));
        out
    }

    fn rows(&self, pairs: &[(String, u64)]) -> Vec<String> {
        if pairs.is_empty() {
            return vec!["  (none)".into()];
        }
        pairs
            .iter()
            .map(|(name, count)| format!("  {count:>8} {name}"))
            .collect()
    }
}

/// What is in the log: sizes, counts, and which paths it talks about most. `bytes` is the file size,
/// which is what the daily cap is measured against.
pub fn summary(directory: &Path, top: usize) -> Result<Summary, crate::Error> {
    let mut summary = Summary::default();
    let mut ops: BTreeMap<String, u64> = BTreeMap::new();
    let mut actors: BTreeMap<String, u64> = BTreeMap::new();
    let mut targets: BTreeMap<String, u64> = BTreeMap::new();
    let mut sessions: BTreeMap<u64, u64> = BTreeMap::new();

    for file in files(directory)? {
        let bytes = fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
        let text = read(&file)?;
        let mut records_in_file = 0u64;
        for (index, raw) in text.lines().enumerate() {
            let record =
                serde_json::from_str::<Record>(raw).map_err(|source| crate::Error::Line {
                    path: file.clone(),
                    line: index + 1,
                    reason: source.to_string(),
                })?;
            records_in_file += 1;
            summary.records += 1;
            *ops.entry(record.op.clone()).or_default() += 1;
            *actors.entry(record.actor.encoded()).or_default() += 1;
            if let Some(target) = record.target {
                *targets.entry(target).or_default() += 1;
            }
            *sessions.entry(record.session).or_default() += 1;
            summary.dropped += record.dropped;
            if summary.first_ts.is_none() {
                summary.first_ts = Some(record.ts.clone());
            }
            summary.last_ts = Some(record.ts.clone());
        }
        if let Some(last) = text.lines().last() {
            if let Ok(record) = serde_json::from_str::<Record>(last) {
                if record.op == "session.close" {
                    summary.sealed_sessions += 1;
                }
            }
        }
        summary.bytes += bytes;
        summary.files.push(FileSummary {
            file,
            bytes,
            records: records_in_file,
        });
    }
    summary.sessions = sessions.len();
    summary.per_op = ranked(ops, top);
    summary.per_actor = ranked(actors, top);
    summary.per_target = ranked(targets, top);
    Ok(summary)
}

fn ranked(counts: BTreeMap<String, u64>, top: usize) -> Vec<(String, u64)> {
    let mut pairs: Vec<(String, u64)> = counts.into_iter().collect();
    pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    pairs.truncate(top);
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::Actor;
    use crate::record::{Draft, Op};
    use crate::writer::{AuditLog, Options};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    const DAY: u64 = 1_758_600_000_000;
    const DAY_STAMP: &str = "2025-09-23T04:00:00.000Z";

    struct Scratch {
        root: tempfile::TempDir,
        millis: Arc<AtomicU64>,
    }

    impl Scratch {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().expect("tempdir"),
                millis: Arc::new(AtomicU64::new(DAY)),
            }
        }

        fn directory(&self) -> PathBuf {
            self.root.path().join("audit")
        }

        fn options(&self) -> Options {
            let millis = self.millis.clone();
            Options {
                directory: self.directory(),
                actor: Actor::User,
                record_reads: true,
                daily_limit_bytes: 64 * 1024 * 1024,
                clock: Arc::new(move || {
                    UNIX_EPOCH + Duration::from_millis(millis.load(Ordering::SeqCst))
                }),
            }
        }

        /// A session that did a little browsing. Dropping the handle is what writes its seal.
        fn log(&self) -> AuditLog {
            let log = AuditLog::open(self.options()).expect("opens");
            log.record(Draft::new(Op::DirOpen).target(Path::new("D:/Datas/tree")));
            log.record(Draft::new(Op::DirFetch).args("0+80").result("ok rows=80"));
            log.record(
                Draft::new(Op::SearchBegin)
                    .args("query=probe within=- limit=500")
                    .result("ok token=7"),
            );
            log
        }

        fn only_file(&self) -> PathBuf {
            files(&self.directory())
                .expect("files")
                .into_iter()
                .next()
                .expect("one file")
        }

        fn lines(&self) -> Vec<String> {
            read(&self.only_file())
                .expect("readable")
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn write_lines(&self, lines: &[String]) {
            let mut file = fs::File::create(self.only_file()).expect("creatable");
            for line in lines {
                writeln!(file, "{line}").expect("writable");
            }
            file.flush().expect("flushed");
        }

        fn replace(&self, number: usize, text: &str) {
            let mut lines = self.lines();
            lines[number - 1] = text.to_owned();
            self.write_lines(&lines);
        }

        fn remove(&self, number: usize) {
            let mut lines = self.lines();
            lines.remove(number - 1);
            self.write_lines(&lines);
        }

        fn truncate_after(&self, keep: usize) {
            let lines: Vec<String> = self.lines().into_iter().take(keep).collect();
            self.write_lines(&lines);
        }
    }

    fn parse(raw: &str) -> Record {
        serde_json::from_str(raw).expect("a record")
    }

    #[test]
    fn a_log_that_was_left_alone_verifies() {
        let scratch = Scratch::new();
        drop(scratch.log());
        let report = verify(&scratch.directory()).expect("reads");
        assert_eq!(report.files, 1);
        assert_eq!(report.records, 5);
        assert_eq!(report.sessions, 1);
        assert!(report.intact(), "{:#?}", report.findings);
    }

    #[test]
    fn two_sessions_in_one_day_verify_side_by_side() {
        let scratch = Scratch::new();
        drop(scratch.log());
        scratch.millis.store(DAY + 60_000, Ordering::SeqCst);
        drop(scratch.log());
        let report = verify(&scratch.directory()).expect("reads");
        assert_eq!(report.sessions, 2);
        assert_eq!(report.records, 10);
        assert!(report.intact(), "{:#?}", report.findings);
        // Both in the same file, which is the whole reason the chain is per session.
        assert_eq!(files(&scratch.directory()).expect("files").len(), 1);
    }

    #[test]
    fn tail_hands_back_the_last_lines_in_order() {
        let scratch = Scratch::new();
        drop(scratch.log());
        let lines = tail(&scratch.directory(), 2).expect("reads");
        assert_eq!(lines.len(), 2);
        assert_eq!(parse(&lines[0]).op, "search.begin");
        assert_eq!(parse(&lines[1]).op, "session.close");
        assert_eq!(tail(&scratch.directory(), 99).expect("reads").len(), 5);
    }

    /// Editing one field of one line: the next line's link stops matching, and the finding names the
    /// line after the edit rather than just "bad".
    #[test]
    fn an_edited_line_breaks_the_link_after_it() {
        let scratch = Scratch::new();
        drop(scratch.log());
        let original = scratch.lines()[2].clone();
        scratch.replace(3, &original.replace("dir.fetch", "dir.open"));

        let report = verify(&scratch.directory()).expect("reads");
        let broken: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|finding| matches!(finding.problem, Problem::BrokenLink { .. }))
            .collect();
        assert_eq!(broken.len(), 1, "found {:?}", report.findings);
        assert_eq!(broken[0].line, 4);
        assert_eq!(broken[0].session, Some(parse(&original).session));
        // Only the one link: the edit did not touch what line 2 hashes to, or what line 4 does.
        assert_eq!(report.findings.len(), 1, "{:#?}", report.findings);
    }

    /// Deleting a line from the middle: the numbering jumps and the link no longer matches, which is
    /// two independent signals for the same hole.
    #[test]
    fn a_deleted_line_shows_as_a_gap_and_a_broken_link() {
        let scratch = Scratch::new();
        drop(scratch.log());
        scratch.remove(2);
        let report = verify(&scratch.directory()).expect("reads");
        assert!(report.findings.iter().any(|finding| matches!(
            finding.problem,
            Problem::Sequence {
                expected: 2,
                found: 3
            }
        )));
        assert!(report
            .findings
            .iter()
            .any(|finding| matches!(finding.problem, Problem::BrokenLink { .. })));
    }

    /// Cutting the tail off: the seal is gone, so the session reads as unfinished even though every
    /// remaining link is intact.
    #[test]
    fn a_removed_tail_leaves_an_unsealed_session() {
        let scratch = Scratch::new();
        drop(scratch.log());
        scratch.truncate_after(3);
        let report = verify(&scratch.directory()).expect("reads");
        assert!(!report.intact(), "{:?}", report.findings);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].problem, Problem::Unsealed);
    }

    /// The case the seal is for: a history rewritten to chain perfectly, whose line count is the one
    /// thing left to give it away.
    #[test]
    fn a_seal_that_disagrees_with_the_lines_is_caught() {
        let scratch = Scratch::new();
        drop(scratch.log());
        let seal = scratch.lines()[4].replace("records=5", "records=9");
        scratch.replace(5, &seal);
        let report = verify(&scratch.directory()).expect("reads");
        // The seal is the last line, so rewriting it breaks nothing above it: the count is the only
        // finding, which is exactly why the count is there.
        assert_eq!(report.findings.len(), 1, "{:#?}", report.findings);
        assert_eq!(
            report.findings[0].problem,
            Problem::Seal {
                expected: 9,
                found: 5
            }
        );
    }

    #[test]
    fn a_garbage_line_is_reported_rather_than_skipped() {
        let scratch = Scratch::new();
        drop(scratch.log());
        scratch.replace(2, "this is not json");
        let report = verify(&scratch.directory()).expect("reads");
        let kinds: Vec<&Problem> = report.findings.iter().map(|f| &f.problem).collect();
        assert!(
            kinds
                .iter()
                .any(|p| matches!(p, Problem::Unparsable { .. })),
            "got {kinds:?}"
        );
        assert!(
            kinds
                .iter()
                .any(|p| matches!(p, Problem::BrokenLink { .. })),
            "the line after an unreadable one cannot link to it, and says so: {kinds:?}"
        );
        // `tail` still works, because it does not parse.
        assert_eq!(tail(&scratch.directory(), 5).expect("reads").len(), 5);
        // `summary` refuses: a count that quietly leaves a line out would be worse than an error.
        assert!(summary(&scratch.directory(), 5).is_err());
    }

    #[test]
    fn a_missing_directory_says_so() {
        let scratch = Scratch::new();
        let error = verify(&scratch.directory()).expect_err("nothing was ever written");
        assert!(
            matches!(error, crate::Error::Missing { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn the_summary_counts_what_a_session_did() {
        let scratch = Scratch::new();
        drop(scratch.log());
        let summary = summary(&scratch.directory(), 5).expect("reads");
        assert_eq!(summary.records, 5);
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.sealed_sessions, 1);
        assert!(summary.bytes > 300, "bytes were {}", summary.bytes);
        assert_eq!(summary.first_ts.as_deref(), Some(DAY_STAMP));
        assert_eq!(summary.last_ts.as_deref(), Some(DAY_STAMP));
        assert_eq!(summary.dropped, 0);
        assert_eq!(summary.per_op.len(), 5, "five distinct ops, one line each");
        assert_eq!(summary.per_actor[0].0, "user");
        let text = summary.lines().join("\n");
        assert!(text.contains("dir.fetch"), "text was {text}");
        assert!(text.contains("D:/DATAS/TREE"), "text was {text}");
    }

    #[test]
    fn the_summary_ranks_targets_by_how_often_they_came_up() {
        let scratch = Scratch::new();
        {
            let log = scratch.log();
            for _ in 0..3 {
                log.record(Draft::new(Op::DirOpen).target(Path::new("D:/Datas/tree")));
            }
        }
        let summary = summary(&scratch.directory(), 5).expect("reads");
        assert_eq!(summary.per_target[0].0, "D:/DATAS/TREE");
        assert_eq!(
            summary.per_target[0].1, 4,
            "the three plus the one in the session"
        );
        assert_eq!(summary.sealed_sessions, 1);
        assert_eq!(summary.records, 8);
    }
}
