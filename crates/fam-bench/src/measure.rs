use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fam_core::{Cancellation, Config, Item, Kind, PathKey};
use fam_svc_enumerate::list_one_level;
use fam_svc_index::{Collection, DirIndex, DirTable, Freshness, Opened, Patch, Snapshot};
use fam_svc_walk::{collect, WalkPolicy};
use serde::{Deserialize, Serialize};

const TIMEOUT: Duration = Duration::from_secs(60);

/// Which driver a case runs.
///
/// `ReadWin32` and `ReadStd` price one reader against the other on a single warm directory, with no
/// thread pool, channel or collected `Vec` between them. They exist because "a listing pays two
/// syscalls per entry, and that is what makes a big folder slow" had to be checked rather than
/// assumed — and because the same comparison over a 78k-entry tree turned out to be ungateable: the
/// spread within one variant (864-1207us) was wider than the gap between variants.
///
/// `PatchWindow` and `RebuildView` need no corpus: they price the two things a re-read is made of, so
/// that "patch these rows in place, or read the folder again" can be answered with numbers instead of
/// an estimate.
#[derive(Clone, Copy)]
pub enum Mode {
    Walk,
    ReadWin32,
    ReadStd,
    /// Answering a viewport out of the cache, which is what a folder open costs once the directory
    /// has been read. The cold read is what the other cases measure.
    CachedViewport,
    /// One batch of departures applied to a cached table, and the view told to follow.
    PatchWindow,
    /// Building a view from scratch over a table that was just read: the sort a re-read pays on top
    /// of its enumeration.
    RebuildView,
    /// One line appended to the audit log. Priced on its own because the aspect runs on the thread
    /// that answered the request, so this number is what a scroll costs on top of the scroll.
    AuditAppend,
}

pub struct Case {
    id: &'static str,
    env_var: &'static str,
    default_corpus: &'static str,
    iterations: usize,
    /// For a walk, how many entries it may emit before stopping. For a synthetic case, how many rows
    /// the table it builds holds.
    entry_cap: usize,
    /// Names one batch of events carries, for the cases that apply a batch. Unused elsewhere.
    window: usize,
    mode: Mode,
    note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseResult {
    pub id: String,
    pub corpus: String,
    pub note: String,
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub entries: u64,
    pub dirs: u64,
    pub errors: u64,
    pub entries_per_s: u64,
}

impl Case {
    /// Corpora verified to exist on the development machine; each is overridable so the harness
    /// still runs a subset elsewhere.
    pub fn all() -> Vec<Case> {
        vec![
            Case {
                id: "open_flat_dir",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 300,
                entry_cap: 200_000,
                window: 0,
                mode: Mode::Walk,
                note: "flat ~188-entry directory: the folder-open headline",
            },
            Case {
                id: "open_mixed_dir",
                env_var: "FAM_BENCH_MIXED",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/plugins",
                iterations: 200,
                entry_cap: 200_000,
                window: 0,
                mode: Mode::Walk,
                note: "files plus 15 subdirectories one level down",
            },
            Case {
                id: "walk_large_tree",
                env_var: "FAM_BENCH_TREE",
                default_corpus: "D:/Applications/Qt",
                iterations: 9,
                entry_cap: 200_000,
                window: 0,
                mode: Mode::Walk,
                note: "77,891 entries across 4,730 directories",
            },
            Case {
                id: "volume_scan_capped",
                env_var: "FAM_BENCH_VOLUME",
                default_corpus: "D:/",
                iterations: 3,
                entry_cap: 200_000,
                window: 0,
                mode: Mode::Walk,
                note: "whole-volume scan that stops at the entry cap",
            },
            Case {
                id: "read_flat_win32",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 4000,
                entry_cap: 0,
                window: 0,
                mode: Mode::ReadWin32,
                note:
                    "one warm directory via FindFirstFileExW; floor over entries is per-entry cost",
            },
            Case {
                id: "viewport_from_cache",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 4000,
                entry_cap: 0,
                window: 0,
                mode: Mode::CachedViewport,
                note: "one scroll of a cached directory: 20 lookups plus every 60-row viewport",
            },
            Case {
                id: "read_flat_std",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 4000,
                entry_cap: 0,
                window: 0,
                mode: Mode::ReadStd,
                note: "the same directory via std::fs, the reader the walk used until increment 2",
            },
            Case {
                id: "patch_256_of_20k",
                env_var: "",
                default_corpus: "",
                // A 20k-row table copy plus one walk of it is ~0.5ms, so a few hundred rounds is a
                // second of work - enough samples to gate on without a warm-up that lies.
                iterations: 300,
                entry_cap: 20_000,
                window: 256,
                mode: Mode::PatchWindow,
                note:
                    "one 256-name departure batch on a 20k-row table, and the view that follows it",
            },
            Case {
                id: "rebuild_view_20k",
                env_var: "",
                default_corpus: "",
                // The sort is the expensive half of a re-read: 20k rows with a natural comparator,
                // so a few dozen rounds already costs most of a second.
                iterations: 40,
                entry_cap: 20_000,
                window: 0,
                mode: Mode::RebuildView,
                note:
                    "ordering a freshly read 20k-row table from scratch - a re-read without its I/O",
            },
            Case {
                id: "audit_append",
                env_var: "",
                default_corpus: "",
                // One line per round, into one already-open file: this is the cost the aspect adds
                // to a call, and it is paid on whatever thread answered the request.
                iterations: 2_000,
                entry_cap: 0,
                window: 0,
                mode: Mode::AuditAppend,
                note:
                    "one audit record: build the summary, serialise it, hash-chain it, WriteFile it",
            },
        ]
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    /// Measures repeatedly and keeps the pass with the lowest floor. Background activity on this
    /// machine moves p50 by more than 2x between runs of the same binary, so only `min_us` — the
    /// fastest observation, which code can raise but not lower — is gated. Ranking passes by p50
    /// would keep a pass whose *floor* was worse, which is what the recorded baseline then carries.
    pub fn measure_best_of(
        &self,
        iterations: Option<usize>,
        passes: usize,
    ) -> Result<CaseResult, PathBuf> {
        let mut best: Option<CaseResult> = None;
        for _ in 0..passes.max(1) {
            let candidate = self.measure(iterations)?;
            best = Some(match best {
                None => candidate,
                Some(current) => lowest_floor(current, candidate),
            });
        }
        Ok(best.expect("at least one pass"))
    }

    fn corpus(&self) -> PathBuf {
        match std::env::var_os(self.env_var) {
            Some(override_path) => PathBuf::from(override_path),
            None => PathBuf::from(self.default_corpus),
        }
    }

    /// Returns `Err(corpus)` when the directory is not present on this machine.
    pub fn measure(&self, iterations: Option<usize>) -> Result<CaseResult, PathBuf> {
        let synthetic = matches!(
            self.mode,
            Mode::PatchWindow | Mode::RebuildView | Mode::AuditAppend
        );
        let corpus = self.corpus();
        if !synthetic && !corpus.is_dir() {
            return Err(corpus);
        }
        let rounds = iterations.unwrap_or(self.iterations).max(2);

        let mut samples = match self.mode {
            Mode::Walk => measure_walk(&corpus, rounds, self.entry_cap),
            Mode::ReadWin32 => measure_scan(&corpus, rounds, read_flat_win32),
            Mode::ReadStd => measure_scan(&corpus, rounds, read_flat_std),
            Mode::CachedViewport => measure_cached_viewport(&corpus, rounds, VIEWPORT, PASSES),
            Mode::PatchWindow => measure_patch(self.entry_cap, self.window, rounds),
            Mode::RebuildView => measure_rebuild(self.entry_cap, rounds),
            Mode::AuditAppend => measure_audit_append(rounds),
        };
        samples.durations.sort_unstable();

        let seconds = samples.total.as_secs_f64().max(f64::EPSILON);
        let durations = samples.durations;
        Ok(CaseResult {
            id: self.id.to_string(),
            corpus: if synthetic {
                match self.mode {
                    Mode::AuditAppend => "synthetic: one log left open".to_string(),
                    _ => format!("synthetic: {} rows", self.entry_cap),
                }
            } else {
                corpus.display().to_string()
            },
            note: self.note.to_string(),
            samples: durations.len(),
            p50_us: percentile(&durations, 0.50),
            p95_us: percentile(&durations, 0.95),
            min_us: durations[0],
            max_us: durations[durations.len() - 1],
            entries: samples.entries,
            dirs: samples.dirs,
            errors: samples.errors,
            // A walk case reports what one capped scan emitted; a reader case re-reads the same
            // directory over and over, so its throughput has to count every round.
            entries_per_s: match self.mode {
                Mode::Walk => (samples.entries as f64 / seconds) as u64,
                _ => (samples.entries as f64 * rounds as f64 / seconds) as u64,
            },
        })
    }
}

struct Samples {
    durations: Vec<u64>,
    total: Duration,
    entries: u64,
    dirs: u64,
    errors: u64,
}

fn new_samples(rounds: usize) -> Samples {
    Samples {
        durations: Vec::with_capacity(rounds),
        total: Duration::ZERO,
        entries: 0,
        dirs: 0,
        errors: 0,
    }
}

fn measure_walk(corpus: &Path, rounds: usize, entry_cap: usize) -> Samples {
    let mut policy = WalkPolicy::from_config(&Config::default());
    policy.roots = vec![corpus.to_path_buf()];
    policy.max_entries = entry_cap;
    let cancel = Cancellation::new();

    // Warm-up pass: page cache and any lazily built state, excluded from the statistics.
    collect(&policy, &cancel, TIMEOUT);

    let mut samples = new_samples(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let (rows, report) = collect(&policy, &cancel, TIMEOUT);
        let elapsed = started.elapsed();
        // Keep the rows alive so the compiler cannot elide the enumeration.
        std::hint::black_box(rows.len());
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        samples.entries = report.emitted;
        samples.dirs = report.directories;
        samples.errors = report.errors;
    }
    samples
}

/// What one read of a corpus reports. `bytes` exists only so the work behind it cannot be elided.
struct Tally {
    entries: u64,
    dirs: u64,
    errors: u64,
    bytes: u64,
}

fn measure_scan(corpus: &Path, rounds: usize, read: fn(&Path) -> Tally) -> Samples {
    read(corpus);
    let mut samples = new_samples(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let tally = read(corpus);
        let elapsed = started.elapsed();
        std::hint::black_box(tally.bytes);
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        samples.entries = tally.entries;
        samples.dirs = tally.dirs;
        samples.errors = tally.errors;
    }
    samples
}

/// Rows a viewport asks for; a virtualised list never materialises a whole directory.
const VIEWPORT: usize = 60;
/// Scroll positions per measured round. One viewport takes ~2us, which the microsecond clock the
/// harness reports in cannot resolve well enough to gate on; twenty of them can.
const PASSES: usize = 20;

/// The cost of answering a scroll from cache: look the directory up, hand out one window of rows.
/// The sorted view is built once outside the loop because that is what the FFI holds per open
/// directory - rebuilding it every round would be measuring the sort, which `open_flat_dir` covers.
fn measure_cached_viewport(
    corpus: &Path,
    rounds: usize,
    viewport: usize,
    passes: usize,
) -> Samples {
    let index = DirIndex::new();
    let key = PathKey::of(corpus);
    let Opened::Ready(snapshot) = index.open(&key, &Cancellation::new()).unwrap() else {
        panic!("a fresh directory must read on the first open");
    };
    let collection = Collection::new(snapshot.table);
    std::hint::black_box(collection.len());
    // Warm-up pass, excluded from the statistics like every other case does.
    std::hint::black_box(index.peek(&key).map(|snapshot| snapshot.generation));
    std::hint::black_box(collection.fetch(0, viewport).len());

    let mut samples = new_samples(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        // Only the rows that reach the view are counted, so `ns/entry` is the cost of handing a
        // row over rather than a blend of lookups and copies.
        let mut handed = 0usize;
        for page in 0..passes {
            let cached = index.peek(&key).expect("cached by the open above");
            std::hint::black_box(cached.generation);
            handed += collection.fetch(page * viewport, viewport).len();
        }
        let elapsed = started.elapsed();
        std::hint::black_box(handed);
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        samples.entries = handed as u64;
        samples.dirs = 1;
    }
    samples
}

/// A row name in the shape enumeration hands back. Fixed width, so the natural comparator has the
/// same digit runs to chew on that a real build-output folder has.
fn name_units(index: usize) -> Vec<u16> {
    format!("f{index:07}.dat").encode_utf16().collect()
}

/// A table the size of a big folder: 20k records and their names, with no filesystem behind them.
fn synthetic_table(rows: usize) -> DirTable {
    let mut table = DirTable::new(1, PathKey::of(Path::new("synthetic:/table")), None);
    for index in 0..rows {
        table.push(
            &name_units(index),
            Item {
                name_off: 0,
                name_units: 0,
                kind: Kind::Document.index(),
                flags: 0,
                size: index as u64,
                mtime_ms: 1_700_000_000_000 + index as i64,
                attrs: 0,
                dir: 0,
            },
        );
    }
    table
}

/// The names one batch of departure events carries, spread every `rows / window`-th row rather than
/// in one run: a contiguous block is the shape the bucket walk is cheapest in, and a burst deleted
/// out of a sorted folder is rarely one run.
fn spread_names(rows: usize, window: usize) -> Vec<Vec<u16>> {
    let step = (rows / window).max(1);
    (0..window).map(|index| name_units(index * step)).collect()
}

/// What one batch of departures costs: the copy-and-walk of the table, plus the view that follows it.
///
/// The view is rebuilt outside the timer because patching consumes the ordering it computed - every
/// round needs a whole one - and that rebuild is `rebuild_view_20k`, not this case.
fn measure_patch(rows: usize, window: usize, rounds: usize) -> Samples {
    let table = Arc::new(synthetic_table(rows));
    let patch = Patch {
        removed: spread_names(rows, window),
        unread: 0,
    };
    let mut samples = new_samples(rounds);
    let mut placed = 0usize;
    let mut cut = 0usize;
    for _ in 0..rounds {
        let mut view = Collection::new(Arc::clone(&table));
        let started = Instant::now();
        let patched = table.patched(&patch);
        let snapshot = Snapshot {
            table: Arc::new(patched.table),
            generation: 1,
            freshness: Freshness::Current,
        };
        let positions = view.patch(&snapshot, &patched.removed);
        let elapsed = started.elapsed();
        placed = patched.removed.len();
        cut = positions.len();
        std::hint::black_box((placed, positions.first().copied(), view.len()));
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        // Per row *walked*, not per row removed: the batch costs a pass over the table whichever way
        // the names fall, and that is the number the patch-or-re-read choice turns on.
        samples.entries = rows as u64;
        samples.dirs = 1;
    }
    assert_eq!(
        placed, window,
        "a patch that names rows the table does not hold measures nothing"
    );
    assert_eq!(
        cut, window,
        "and a view that did not follow them is not the path"
    );
    samples
}

/// Ordering a table that was just read - the half of a re-read that has nothing to do with the
/// filesystem, and the half an in-place patch avoids entirely.
fn measure_rebuild(rows: usize, rounds: usize) -> Samples {
    let table = Arc::new(synthetic_table(rows));
    std::hint::black_box(Collection::new(Arc::clone(&table)).len());
    let mut samples = new_samples(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let view = Collection::new(Arc::clone(&table));
        // Reading the first row's record index is what proves the sort ran: the index at the head of
        // the order is the one thing a skipped comparison would get wrong.
        let head = view.fetch(0, 1).first().map(|row| row.index);
        let elapsed = started.elapsed();
        std::hint::black_box(head.expect("a table this size has a first row"));
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        samples.entries = rows as u64;
        samples.dirs = 1;
    }
    samples
}

/// The line an open folder writes on every repaint, priced against one handle left open for the
/// whole pass - which is what the ABI does, and the only honest shape for the number: opening the
/// file per record would be measuring a path lookup rather than an append.
fn measure_audit_append(rounds: usize) -> Samples {
    use fam_svc_audit::{Actor, AuditLog, Draft, Op, Options};

    let scratch = tempfile::tempdir().expect("audit scratch");
    let log = AuditLog::open(Options {
        directory: scratch.path().join("audit"),
        actor: Actor::User,
        record_reads: true,
        daily_limit_bytes: 64 * 1024 * 1024,
        clock: fam_svc_audit::system_clock(),
    })
    .expect("the audit log opens");
    let mut samples = new_samples(rounds);
    for index in 0..rounds {
        let started = Instant::now();
        log.record(
            Draft::new(Op::DirFetch)
                .args(format!("token=1 {index}+60"))
                .result("ok 2400B"),
        );
        let elapsed = started.elapsed();
        samples.durations.push(elapsed.as_micros() as u64);
        samples.total += elapsed;
        samples.entries = 1;
        samples.dirs = 1;
    }
    samples
}

fn read_flat_win32(dir: &Path) -> Tally {
    let cancel = Cancellation::new();
    let mut tally = Tally {
        entries: 0,
        dirs: 1,
        errors: 0,
        bytes: 0,
    };
    if list_one_level(dir, &cancel, &mut |item| {
        tally.entries += 1;
        tally.bytes += item.size;
        true
    })
    .is_err()
    {
        tally.errors += 1;
    }
    tally
}

/// The same shape through `std::fs`. `DirEntry::metadata` is not the second query it looks like:
/// std fills it from the enumeration record it already holds, which is why this reader came out
/// within a few percent of the Win32 one instead of at twice its cost.
fn read_flat_std(dir: &Path) -> Tally {
    let mut tally = Tally {
        entries: 0,
        dirs: 1,
        errors: 0,
        bytes: 0,
    };
    match fs::read_dir(dir) {
        Ok(items) => {
            for item in items {
                let Ok(item) = item else {
                    tally.errors += 1;
                    continue;
                };
                tally.entries += 1;
                match item.metadata() {
                    Ok(metadata) => tally.bytes += metadata.len(),
                    Err(_) => tally.errors += 1,
                }
            }
        }
        Err(_) => tally.errors += 1,
    }
    tally
}

/// The pass worth keeping across repeated measurement: the one that saw the fastest single round.
/// `min_us` is the only thing the gate compares, so ranking passes by p50 can keep a worse floor.
fn lowest_floor(current: CaseResult, candidate: CaseResult) -> CaseResult {
    if candidate.min_us < current.min_us {
        candidate
    } else {
        current
    }
}

fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).floor() as usize;
    sorted[index]
}

impl CaseResult {
    pub fn print_line(&self) {
        println!(
            "{:<24} p50 {:>8}us  p95 {:>9}us  min {:>8}us  entries {:>6}  dirs {:>5}  errors {:>3}  {:>5}ns/entry",
            self.id,
            self.p50_us,
            self.p95_us,
            self.min_us,
            self.entries,
            self.dirs,
            self.errors,
            self.floor_ns_per_entry()
        );
    }

    /// The floor divided by the entries seen — the unit the plan gates enumeration on.
    pub fn floor_ns_per_entry(&self) -> u64 {
        if self.entries == 0 {
            return 0;
        }
        self.min_us * 1000 / self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor is the only thing the gate compares, so it is what the kept pass must have - even
    /// when that pass looked worse in the middle of its own distribution.
    #[test]
    fn the_kept_pass_is_the_one_with_the_lowest_floor() {
        let quiet = CaseResult {
            id: "a".into(),
            corpus: String::new(),
            note: String::new(),
            samples: 2,
            p50_us: 900,
            p95_us: 900,
            min_us: 100,
            max_us: 900,
            entries: 1,
            dirs: 1,
            errors: 0,
            entries_per_s: 0,
        };
        let busy = CaseResult {
            min_us: 200,
            p50_us: 500,
            ..quiet.clone()
        };
        assert_eq!(lowest_floor(quiet.clone(), busy.clone()).min_us, 100);
        assert_eq!(lowest_floor(busy.clone(), quiet.clone()).min_us, 100);
        assert_eq!(lowest_floor(quiet.clone(), quiet.clone()).min_us, 100);
    }

    #[test]
    fn percentile_picks_the_expected_ranks() {
        let samples: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&samples, 0.50), 50);
        assert_eq!(percentile(&samples, 0.95), 95);
        assert_eq!(percentile(&samples, 1.0), 100);
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn a_missing_corpus_is_reported_rather_than_measured() {
        let case = Case {
            id: "absent",
            env_var: "FAM_BENCH_DEFINITELY_ABSENT",
            default_corpus: "Q:/no/such/directory/anywhere",
            iterations: 2,
            entry_cap: 10,
            window: 0,
            mode: Mode::Walk,
            note: "test",
        };
        let missing = case.measure(Some(2)).unwrap_err();
        assert!(!missing.is_dir());
    }

    #[test]
    fn measuring_a_real_directory_produces_samples() {
        let case = Case {
            id: "self",
            env_var: "FAM_BENCH_SELF",
            default_corpus: env!("CARGO_MANIFEST_DIR"),
            iterations: 2,
            entry_cap: 500,
            window: 0,
            mode: Mode::Walk,
            note: "this crate's own directory",
        };
        let result = case.measure(Some(2)).expect("corpus exists");
        assert_eq!(result.samples, 2);
        assert!(result.entries > 0);
        assert!(result.p50_us > 0);
    }

    /// The two readers must see the same directory the same way, or the case that compares them
    /// measures nothing. Only `attrs` may differ, and that field is not part of a tally.
    #[test]
    fn both_readers_count_the_same_directory_the_same_way() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("top.txt"), "123").unwrap();
        std::fs::write(root.path().join("photo.png"), "4567").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();

        let win32 = read_flat_win32(root.path());
        let std = read_flat_std(root.path());

        assert_eq!(win32.entries, 3, "two files and one subdirectory");
        assert_eq!(win32.errors, 0);
        assert_eq!(std.entries, win32.entries);
        assert_eq!(std.bytes, win32.bytes);
    }

    /// The two synthetic cases have no corpus to be missing, so nothing outside them checks that they
    /// are doing the work they claim to be timing. Their own assertions are that check; this runs them
    /// small enough to belong in the test suite rather than in a gate run.
    #[test]
    fn a_synthetic_case_measures_the_walk_it_says_it_walks() {
        let patched = measure_patch(2000, 16, 2);
        assert_eq!(patched.durations.len(), 2);
        assert_eq!(
            patched.entries, 2000,
            "the cost is per row walked, not per row removed"
        );
        assert!(patched.durations[0] > 0, "a batch of 16 names is not free");

        let rebuilt = measure_rebuild(2000, 2);
        assert_eq!(rebuilt.durations.len(), 2);
        assert_eq!(rebuilt.entries, 2000);
    }

    #[test]
    fn floor_per_entry_divides_the_fastest_observation() {
        let result = CaseResult {
            id: "x".into(),
            corpus: "c".into(),
            note: "n".into(),
            samples: 1,
            p50_us: 300,
            p95_us: 400,
            min_us: 240,
            max_us: 400,
            entries: 120,
            dirs: 1,
            errors: 0,
            entries_per_s: 0,
        };
        assert_eq!(result.floor_ns_per_entry(), 2_000);
        assert_eq!(
            CaseResult {
                entries: 0,
                ..result
            }
            .floor_ns_per_entry(),
            0
        );
    }
}
