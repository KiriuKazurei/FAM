//! Reconciles "the watch is still delivering" against the watched directory's own write time.
//!
//! The reason this exists at all: a `ReadDirectoryChangesW` buffer overflow is not observable from
//! outside the backend. notify's Windows loop treats any error code it does not recognise - which
//! includes `ERROR_NOTIFY_ENUM_DIR` - by logging and *silently withdrawing the watch*
//! (`notify-8.2.0/src/windows.rs:357-367`), so the events simply stop and no `Err` ever reaches a
//! caller. A directory that stops reporting is worse than one that reports an error, because the
//! cache keeps serving rows it can no longer know about.
//!
//! The observable substitute: creating, deleting or renaming an entry updates the *directory's*
//! own last-write time. So each watched directory is sampled periodically, and a sample that moved
//! without any event arriving in the same window is treated as lost coverage - the directory gets a
//! rescan request, which is the same action an overflow ought to trigger. What this catches for
//! real, end to end against the live backend, is a watch target that was deleted from under us:
//! `tests/watch.rs::a_deleted_watch_target_surfaces_as_a_rescan_though_the_backend_says_nothing`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use fam_core::PathKey;

use crate::batch::{RescanWhy, Signal};

/// One watched directory plus what the last sample said about it.
struct Watch {
    path: PathBuf,
    mtime: SystemTime,
    /// Set when any event for this directory reached the pipeline since the previous sample.
    seen: bool,
    /// Set when the last sample could not be read at all, meaning the directory is gone or refuses
    /// us. Loss is reported once and then held quiet until a sample succeeds again: a consumer that
    /// acts on the rescan drops the watch, and one that does not would otherwise be told again on
    /// every rotation for as long as the directory stays missing.
    lost: bool,
}

/// The set of watches a pipeline holds, shared between whoever manages them and the consumer
/// thread that samples them.
///
/// Ordering is insertion order rather than hash order, so a budgeted sweep visits a rotating
/// window of directories and every one of them gets sampled eventually - a `HashMap` iteration
/// order that shifts between calls could starve a directory indefinitely.
pub struct WatchSet {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    by_key: HashMap<PathKey, Watch>,
    order: Vec<PathKey>,
    cursor: usize,
}

/// What one [`WatchSet::sweep`] pass did.
///
/// `samples` can be below the requested budget when watches were removed between the budget being
/// computed and the pass running, and the pacer needs the honest count to price a sample.
pub struct SweepReport {
    pub signals: Vec<Signal>,
    pub samples: usize,
}

impl Default for WatchSet {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchSet {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }
    /// Registers a watch. `mtime` is the baseline the next sample is compared against; a caller
    /// that could not read it passes [`SystemTime::UNIX_EPOCH`] plus `seen = true`, which makes the
    /// next sample re-baseline instead of raising a spurious rescan.
    pub fn add(&self, dir: &Path, mtime: SystemTime, seen: bool) -> bool {
        let key = PathKey::of(dir);
        let mut inner = self.lock();
        if inner.by_key.contains_key(&key) {
            return false;
        }
        inner.by_key.insert(
            key.clone(),
            Watch {
                path: dir.to_path_buf(),
                mtime,
                seen,
                lost: false,
            },
        );
        inner.order.push(key);
        true
    }

    pub fn remove(&self, dir: &Path) -> bool {
        let key = PathKey::of(dir);
        let mut inner = self.lock();
        if inner.by_key.remove(&key).is_none() {
            return false;
        }
        if let Some(position) = inner.order.iter().position(|candidate| candidate == &key) {
            inner.order.swap_remove(position);
            if inner.cursor > inner.order.len() {
                inner.cursor = 0;
            }
        }
        true
    }

    pub fn contains(&self, dir: &Path) -> bool {
        self.lock().by_key.contains_key(&PathKey::of(dir))
    }

    pub fn len(&self) -> usize {
        self.lock().by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Notes that an event for `dir` arrived, so a moved write time is explained by it.
    pub fn mark_seen(&self, dir: &PathKey) {
        if let Some(watch) = self.lock().by_key.get_mut(dir) {
            watch.seen = true;
        }
    }

    /// Samples up to `budget` directories, starting where the previous sweep stopped.
    ///
    /// `read_mtime` is injected so the decision rule is testable without a real watcher, real
    /// filesystem, or the seconds a live test would need (there is one of those too, in `watch.rs`).
    pub fn sweep(
        &self,
        budget: usize,
        mut read_mtime: impl FnMut(&Path) -> io::Result<SystemTime>,
    ) -> SweepReport {
        let mut inner = self.lock();
        let mut signals = Vec::new();
        // A pass never samples one directory twice: once the cursor wraps it would re-read entries
        // the same pass already decided about, and a single unreadable directory would report a
        // rescan per remaining budget slot.
        let slots = budget.min(inner.order.len());
        for _ in 0..slots {
            let index = inner.cursor % inner.order.len();
            inner.cursor = (index + 1) % inner.order.len();
            let key = inner.order[index].clone();
            let Some(watch) = inner.by_key.get_mut(&key) else {
                continue;
            };
            let path = watch.path.clone();
            match read_mtime(&path) {
                // The directory is gone or refuses us. Either way its rows are no longer known -
                // but only the first time we notice.
                Err(_) => {
                    if !watch.lost {
                        signals.push(Signal::Rescan {
                            dir: key.clone(),
                            why: RescanWhy::WatchLost,
                        });
                    }
                    watch.lost = true;
                }
                Ok(mtime) => {
                    watch.lost = false;
                    if mtime != watch.mtime && !watch.seen {
                        signals.push(Signal::Rescan {
                            dir: key.clone(),
                            why: RescanWhy::Reconciled,
                        });
                    }
                    watch.mtime = mtime;
                }
            }
            watch.seen = false;
        }
        SweepReport {
            signals,
            samples: slots,
        }
    }
}

impl WatchSet {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The directory's own last-write time, which is what the sweep compares.
pub fn write_time(dir: &Path) -> io::Result<SystemTime> {
    std::fs::metadata(dir).and_then(|metadata| metadata.modified())
}

/// How many directories the next sweep may sample, sized from what the last one actually cost.
///
/// A sample is a path lookup, and a path lookup on the development machine costs ~190µs whichever
/// API is asked for it (`GetFileAttributesExW`, `fs::metadata` and a `FindFirstFileExW` of the
/// directory itself agree within 15%; see `docs/notes/watch-sweep-costs-a-path-lookup.md`). The
/// sweep runs on the thread that delivers batches to the UI, so a budget stated as a *count* turns
/// UI latency into a function of machine state - a slower filter stack would stall flushes. Stating
/// it as a time and measuring the price keeps the consumer's duty cycle fixed, and coverage then
/// adapts by itself: on a machine where the lookup is 2µs, the same budget samples a hundred times
/// more directories.
pub struct SweepPacer {
    window: Duration,
    ns_per_sample: u64,
}

/// Seeded with the measured cost of one lookup on the development machine, so the very first pass
/// is already sized rather than paying for an optimistic one.
const SEEDED_NS_PER_SAMPLE: u64 = 200_000;

impl SweepPacer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            ns_per_sample: SEEDED_NS_PER_SAMPLE,
        }
    }

    pub fn budget(&self, watched: usize) -> usize {
        let ns_per_sample = self.ns_per_sample.max(1);
        (self.window.as_nanos() as u64 / ns_per_sample)
            .min(watched as u64)
            // One sample per pass even when the price says the window is already spent: a pass that
            // never samples never measures, and the estimate would never recover.
            .max(u64::from(watched > 0)) as usize
    }

    /// Folds in what the pass cost. Weighted so one unlucky pass cannot size the next one alone.
    pub fn observe(&mut self, samples: usize, elapsed: Duration) {
        if samples == 0 {
            return;
        }
        let measured = elapsed.as_nanos() as u64 / samples as u64;
        self.ns_per_sample = (3 * self.ns_per_sample + measured) / 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(dir: &str) -> PathKey {
        PathKey::of(Path::new(dir))
    }

    fn rescans(signals: &[Signal]) -> Vec<(String, RescanWhy)> {
        signals
            .iter()
            .map(|signal| match signal {
                Signal::Rescan { dir, why } => (dir.to_lossy_string(), *why),
                Signal::Row(change) => panic!("sweep must not invent rows: {change:?}"),
            })
            .collect()
    }

    /// A clock the test moves by hand, so "time moved forward" is not a sleep.
    struct Steady {
        start: SystemTime,
    }

    impl Steady {
        fn new() -> Self {
            Self {
                start: SystemTime::now(),
            }
        }

        fn at(&self, seconds: u64) -> SystemTime {
            self.start + Duration::from_secs(seconds)
        }
    }

    #[test]
    fn a_moved_write_time_without_events_asks_for_a_rescan_once() {
        let set = WatchSet::new();
        let clock = Steady::new();
        set.add(Path::new("D:/docs"), clock.at(0), false);

        // Same sample: nothing to report.
        let quiet = set.sweep(8, |_| Ok(clock.at(0))).signals;
        assert!(quiet.is_empty(), "unchanged: {:?}", rescans(&quiet));

        let moved = set.sweep(8, |_| Ok(clock.at(60))).signals;
        assert_eq!(
            rescans(&moved),
            vec![("D:/DOCS".to_string(), RescanWhy::Reconciled)]
        );
        // The baseline moved with it, so the same gap is not reported twice.
        let again = set.sweep(8, |_| Ok(clock.at(60))).signals;
        assert!(again.is_empty());
    }

    #[test]
    fn a_moved_write_time_explained_by_events_is_not_a_loss() {
        let set = WatchSet::new();
        let clock = Steady::new();
        set.add(Path::new("D:/docs"), clock.at(0), false);

        set.mark_seen(&key("D:/docs"));
        let signals = set.sweep(8, |_| Ok(clock.at(120))).signals;
        assert!(
            signals.is_empty(),
            "events arrived, so the write time is expected to move"
        );
        // `seen` resets, so the *next* unexplained move is reported.
        set.sweep(8, |_| Ok(clock.at(120)));
        let later = set.sweep(8, |_| Ok(clock.at(300))).signals;
        assert_eq!(later.len(), 1);
    }

    #[test]
    fn an_unreadable_directory_reports_a_lost_watch() {
        let set = WatchSet::new();
        let clock = Steady::new();
        set.add(Path::new("D:/gone"), clock.at(0), false);

        let report = set.sweep(8, |_| Err(io::Error::from(io::ErrorKind::NotFound)));

        assert_eq!(report.samples, 1, "one watch, so one sample");
        assert_eq!(
            rescans(&report.signals),
            vec![("D:/GONE".to_string(), RescanWhy::WatchLost)]
        );
    }

    /// A reader for a directory that is gone. A `fn` item rather than a closure: the latter pins a
    /// lifetime `sweep`'s `impl FnMut(&Path)` bound does not accept.
    fn gone(_: &Path) -> io::Result<SystemTime> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    #[test]
    fn a_missing_directory_is_reported_once_until_it_comes_back() {
        let set = WatchSet::new();
        let clock = Steady::new();
        set.add(Path::new("D:/gone"), clock.at(0), false);

        assert_eq!(rescans(&set.sweep(4, gone).signals).len(), 1);
        // Rotations keep sampling - that is how recovery is noticed - but stop re-reporting.
        for _ in 0..3 {
            let report = set.sweep(4, gone);
            assert_eq!(report.samples, 1, "still sampled");
            assert!(
                report.signals.is_empty(),
                "told twice: {:?}",
                report.signals
            );
        }

        // It returns with a moved write time, which is itself a loss worth a rescan.
        let back = set.sweep(4, |_| Ok(clock.at(500))).signals;
        assert_eq!(
            rescans(&back),
            vec![("D:/GONE".to_string(), RescanWhy::Reconciled)]
        );
        // And a second disappearance is reported again.
        assert_eq!(rescans(&set.sweep(4, gone).signals).len(), 1);
    }

    #[test]
    fn a_sweep_visits_a_rotating_window_not_the_same_directories() {
        let set = WatchSet::new();
        let clock = Steady::new();
        for index in 0..200 {
            set.add(Path::new(&format!("D:/d{index:03}")), clock.at(0), false);
        }

        let mut visited = Vec::new();
        for _ in 0..4 {
            // Every directory is unchanged, so a sweep returns nothing: count visits by making the
            // first directory of each window move and observing which one gets reported.
            let report = set.sweep(64, |path| {
                visited.push(path.to_string_lossy().into_owned());
                Ok(clock.at(0))
            });
            assert!(report.signals.is_empty());
            assert_eq!(report.samples, 64);
        }

        assert_eq!(visited.len(), 4 * 64, "each sweep honours the budget");
        // The property that matters: one full cycle covers every directory exactly once before any
        // of them is sampled again, so no directory can starve however large the set grows.
        let first_cycle = &visited[..200];
        let distinct: std::collections::HashSet<&String> = first_cycle.iter().collect();
        assert_eq!(
            distinct.len(),
            200,
            "a cycle sampled the same directory twice"
        );
    }

    #[test]
    fn registration_is_idempotent_and_removal_releases_the_slot() {
        let set = WatchSet::new();
        let clock = Steady::new();

        assert!(set.add(Path::new(r"D:\docs"), clock.at(0), false));
        assert!(
            !set.add(Path::new("D:/docs"), clock.at(0), false),
            "a second spelling of the same directory is the same watch"
        );
        assert_eq!(set.len(), 1);
        assert!(set.contains(Path::new("D:/DOCS")));

        assert!(set.remove(Path::new(r"D:\docs")));
        assert!(!set.remove(Path::new("D:/docs")));
        assert!(set.is_empty());
    }

    #[test]
    fn a_budget_is_priced_from_measured_cost_not_a_fixed_count() {
        let mut pacer = SweepPacer::new(Duration::from_millis(4));
        // Seeded at 200us a sample: 4ms buys 20 of them.
        assert_eq!(pacer.budget(1000), 20);
        assert_eq!(
            pacer.budget(6),
            6,
            "a pass never samples more than there is"
        );

        // A machine where the lookup is far cheaper (20 samples in 40us, so 2us each): the same
        // window covers many more directories, which is the point of pacing by time.
        for _ in 0..20 {
            pacer.observe(20, Duration::from_micros(40));
        }
        assert!(
            pacer.ns_per_sample < 5_000,
            "the estimate did not follow the measurement: {}ns",
            pacer.ns_per_sample
        );
        assert_eq!(
            pacer.budget(1000),
            1000,
            "a cheap sample should cover the whole set in one pass"
        );
        let wide = pacer.budget(30_000);
        assert!(
            (1000..4000).contains(&wide),
            "coverage should track the measurement without overshooting it: got {wide}"
        );

        // And it cannot fall to zero while directories remain, or it would stop measuring.
        let mut slow = SweepPacer::new(Duration::from_micros(1));
        slow.observe(20, Duration::from_millis(20));
        assert_eq!(slow.budget(1000), 1, "one sample keeps the estimate alive");
        assert_eq!(slow.budget(0), 0, "nothing watched, nothing to sample");
    }
}
