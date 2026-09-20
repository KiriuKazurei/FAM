use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fam_core::{Cancellation, Config, PathKey};
use fam_svc_enumerate::list_one_level;
use fam_svc_index::{Collection, DirIndex, Opened};
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
#[derive(Clone, Copy)]
pub enum Mode {
    Walk,
    ReadWin32,
    ReadStd,
    /// Answering a viewport out of the cache, which is what a folder open costs once the directory
    /// has been read. The cold read is what the other cases measure.
    CachedViewport,
}

pub struct Case {
    id: &'static str,
    env_var: &'static str,
    default_corpus: &'static str,
    iterations: usize,
    entry_cap: usize,
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
                mode: Mode::Walk,
                note: "flat ~188-entry directory: the folder-open headline",
            },
            Case {
                id: "open_mixed_dir",
                env_var: "FAM_BENCH_MIXED",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/plugins",
                iterations: 200,
                entry_cap: 200_000,
                mode: Mode::Walk,
                note: "files plus 15 subdirectories one level down",
            },
            Case {
                id: "walk_large_tree",
                env_var: "FAM_BENCH_TREE",
                default_corpus: "D:/Applications/Qt",
                iterations: 9,
                entry_cap: 200_000,
                mode: Mode::Walk,
                note: "77,891 entries across 4,730 directories",
            },
            Case {
                id: "volume_scan_capped",
                env_var: "FAM_BENCH_VOLUME",
                default_corpus: "D:/",
                iterations: 3,
                entry_cap: 200_000,
                mode: Mode::Walk,
                note: "whole-volume scan that stops at the entry cap",
            },
            Case {
                id: "read_flat_win32",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 4000,
                entry_cap: 0,
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
                mode: Mode::CachedViewport,
                note: "one scroll of a cached directory: 20 lookups plus every 60-row viewport",
            },
            Case {
                id: "read_flat_std",
                env_var: "FAM_BENCH_FLAT",
                default_corpus: "D:/Applications/Qt/6.11/6.11.2/mingw_64/bin",
                iterations: 4000,
                entry_cap: 0,
                mode: Mode::ReadStd,
                note: "the same directory via std::fs, the reader the walk used until increment 2",
            },
        ]
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    /// Measures repeatedly and keeps the pass with the lowest floor. Background activity on this
    /// machine moves p50 by more than 2x between runs of the same binary, so only `min_us` — the
    /// fastest observation, which code can raise but not lower — is gated.
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
                Some(current) => {
                    if candidate.p50_us < current.p50_us {
                        candidate
                    } else {
                        current
                    }
                }
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
        let corpus = self.corpus();
        if !corpus.is_dir() {
            return Err(corpus);
        }
        let rounds = iterations.unwrap_or(self.iterations).max(2);

        let mut samples = match self.mode {
            Mode::Walk => measure_walk(&corpus, rounds, self.entry_cap),
            Mode::ReadWin32 => measure_scan(&corpus, rounds, read_flat_win32),
            Mode::ReadStd => measure_scan(&corpus, rounds, read_flat_std),
            Mode::CachedViewport => measure_cached_viewport(&corpus, rounds, VIEWPORT, PASSES),
        };
        samples.durations.sort_unstable();

        let seconds = samples.total.as_secs_f64().max(f64::EPSILON);
        let durations = samples.durations;
        Ok(CaseResult {
            id: self.id.to_string(),
            corpus: corpus.display().to_string(),
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
