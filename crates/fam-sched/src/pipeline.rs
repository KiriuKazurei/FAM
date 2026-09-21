use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use fam_core::{Config, PathKey};
use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::Config as NotifyConfig;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::batch::{Batch, Batcher, Change, FileChange, RescanWhy, Signal, Target};
use crate::reconcile::{write_time, SweepPacer, WatchSet};

pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;
pub const DEFAULT_MAX_BATCH: usize = 256;

const IDLE_POLL: Duration = Duration::from_millis(50);

/// How often the consumer samples watched directories for a write time that moved with no event to
/// explain it. The backend cannot report the losses this catches; see `reconcile.rs`.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
/// How long one sampling pass may take. The sweep shares the thread that delivers batches to the
/// UI, so this bounds UI latency directly; how many directories fit into it is measured rather than
/// fixed, because a path lookup costs ~190us on the development machine and near-nothing on a
/// filter-free one (`docs/notes/watch-sweep-costs-a-path-lookup.md`).
pub const SWEEP_WINDOW: Duration = Duration::from_millis(4);

/// How deep a watch reaches.
///
/// The lazy cache holds one-level tables, so it watches one level: a recursive watch on every
/// cached directory would overlap and report each change once per ancestor that covers it, which is
/// how the legacy stack got duplicate deltas and duplicate loss counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// What the lazy cache uses: it holds one-level tables, so it watches one level.
    #[default]
    OneLevel,
    Recursive,
}

impl Mode {
    fn recursive_mode(self) -> RecursiveMode {
        match self {
            Mode::OneLevel => RecursiveMode::NonRecursive,
            Mode::Recursive => RecursiveMode::Recursive,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub queue_capacity: usize,
    pub window: Duration,
    pub max_batch: usize,
    /// Directories to watch as soon as the pipeline starts. The cache-driven use case starts empty
    /// and adds watches as directories are opened, so an empty list is not an error.
    pub initial_watch: Vec<PathBuf>,
    pub initial_mode: Mode,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            window: Duration::from_millis(fam_core::DEFAULT_UI_BATCH_MS),
            max_batch: DEFAULT_MAX_BATCH,
            initial_watch: Vec::new(),
            initial_mode: Mode::OneLevel,
        }
    }
}

impl SchedulerConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            queue_capacity: config.event_queue_capacity,
            window: config.ui_batch_window(),
            max_batch: DEFAULT_MAX_BATCH,
            initial_watch: Vec::new(),
            initial_mode: Mode::OneLevel,
        }
    }

    /// The legacy shape: watch these roots recursively from the start.
    pub fn watching(dirs: &[PathBuf], config: Config) -> Self {
        Self {
            initial_watch: dirs.to_vec(),
            initial_mode: Mode::Recursive,
            ..Self::from_config(&config)
        }
    }
}

/// Receives merged row deltas. Implementations must return promptly; the pipeline has exactly one
/// consumer and a slow sink stalls everything behind it.
pub trait Sink: Send {
    fn flush(&mut self, batch: Batch);
}

impl<F> Sink for F
where
    F: FnMut(Batch) + Send,
{
    fn flush(&mut self, batch: Batch) {
        (self)(batch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchedError {
    #[error("cannot create file watcher: {0}")]
    Watcher(#[from] notify::Error),
    #[error("cannot watch {}: {source}", .path.display())]
    AddPath {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
    #[error("cannot unwatch {}: {source}", .path.display())]
    RemovePath {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

/// Feeds a bounded queue without ever blocking the watcher callback: when the queue is full the
/// signal is counted as dropped and discarded, as the legacy `Notify()` did (§4).
#[derive(Clone)]
struct Publisher {
    sender: SyncSender<Signal>,
    dropped: Arc<AtomicU64>,
}

impl Publisher {
    fn publish(&self, signals: Vec<Signal>) -> usize {
        let mut dropped = 0usize;
        for signal in signals {
            if self.sender.try_send(signal).is_err() {
                dropped += 1;
            }
        }
        if dropped > 0 {
            self.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
        }
        dropped
    }

    /// A backend error outranks "we lost a row": for every directory the error names, the only safe
    /// instruction is to re-read it. The count still goes up, because events were lost either way.
    fn publish_loss(&self, error: &notify::Error) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        for path in &error.paths {
            let _ = self.sender.try_send(Signal::Rescan {
                dir: PathKey::of(path),
                why: RescanWhy::WatchLost,
            });
        }
    }
}

/// Which side of a directory a change touches, as far as the event itself says.
///
/// `ReadDirectoryChangesW` gives a name and an action and nothing else, so on Windows this returns
/// `Unknown` for every event - notify's backend hardcodes `CreateKind::Any` / `RemoveKind::Any`
/// (`windows.rs:434-449`). The other kinds are matched so a backend that does report the
/// distinction is believed, and so the assumption is written down rather than implied.
fn target_of(kind: &EventKind) -> Target {
    match kind {
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => {
            Target::Directory
        }
        EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => Target::File,
        _ => Target::Unknown,
    }
}

/// Maps a watcher event onto create / write / remove / rename deltas.
///
/// The old consumer switched on Create and Write only, so deletions and renames never reached the
/// UI (§4.2). Rename is emitted as a removal of the old path plus a creation of the new one,
/// because `ReadDirectoryChangesW` reports the pair as two volatile records.
fn translate(event: &Event) -> Vec<Signal> {
    let target = target_of(&event.kind);
    let change = match event.kind {
        EventKind::Create(_) => Change::Created,
        EventKind::Remove(_) => Change::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Change::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Change::Created,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            return event
                .paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    let change = if index == 0 {
                        Change::Removed
                    } else {
                        Change::Created
                    };
                    Signal::Row(FileChange::with_target(path, change, target))
                })
                .collect();
        }
        // Covers data writes, attribute writes and write-time metadata: all of them can change the
        // size or mtime a row displays.
        EventKind::Modify(_) => Change::Modified,
        _ => Change::Other,
    };
    event
        .paths
        .iter()
        .map(|path| Signal::Row(FileChange::with_target(path, change, target)))
        .collect()
}

fn flush(batcher: &mut Batcher, sink: &mut dyn Sink, dropped: &AtomicU64) {
    let signals: Vec<Signal> = batcher
        .drain()
        .into_iter()
        .filter(|signal| match signal {
            Signal::Row(change) => change.change != Change::Other,
            Signal::Rescan { .. } => true,
        })
        .collect();
    if signals.is_empty() {
        return;
    }
    let mut changes = Vec::new();
    let mut rescans = Vec::new();
    for signal in signals {
        match signal {
            Signal::Row(change) => changes.push(change),
            Signal::Rescan { dir, why } => rescans.push((dir, why)),
        }
    }
    let discarded = dropped.swap(0, Ordering::Relaxed);
    sink.flush(Batch::new(changes, rescans, discarded));
}

/// A running watch set plus the batching consumer.
pub struct Handle {
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    watcher: Option<RecommendedWatcher>,
    sender: Option<SyncSender<Signal>>,
    consumer: Option<JoinHandle<()>>,
    /// Watched directories, keyed by identity so `D:\a` and `d:/a` are recognised as the same
    /// watch. notify keys its own map by the raw path and `insert`s straight over any existing
    /// entry (`windows.rs:238`), so two spellings of one directory mean two live handles reporting
    /// the same change twice, and a repeat of one spelling drops the earlier `WatchState` without
    /// closing its handle - leaving a read request that still delivers and can never be cancelled.
    /// Shared with the consumer thread, which samples it for lost coverage.
    watch_set: Arc<WatchSet>,
}

impl Handle {
    /// Events discarded because the queue was full — surfaced so a UI can warn about stale rows.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    pub fn watched_count(&self) -> usize {
        self.watch_set.len()
    }

    pub fn is_watched(&self, dir: &Path) -> bool {
        self.watch_set.contains(dir)
    }

    /// Adds a watch, or returns `false` when that directory is already watched. Idempotency matters
    /// because the cache adds a watch per cached directory and eviction may race a reopen.
    pub fn watch(&mut self, dir: &Path, mode: Mode) -> Result<bool, SchedError> {
        if self.watch_set.contains(dir) {
            return Ok(false);
        }
        let watcher = self.watcher.as_mut().ok_or_else(|| SchedError::AddPath {
            path: dir.to_path_buf(),
            source: notify::Error::generic("pipeline is shut down"),
        })?;
        watcher
            .watch(dir, mode.recursive_mode())
            .map_err(|source| SchedError::AddPath {
                path: dir.to_path_buf(),
                source,
            })?;
        // Baseline the write time now, so the first sweep compares against something real. Failing
        // the watch because a sample could not be read would be the wrong trade; instead mark it
        // explained, and let the next pass re-baseline rather than raise a spurious rescan.
        let (mtime, seen) = match write_time(dir) {
            Ok(mtime) => (mtime, false),
            Err(_) => (SystemTime::UNIX_EPOCH, true),
        };
        Ok(self.watch_set.add(dir, mtime, seen))
    }

    /// Drops a watch. Returns `false` when this pipeline was not watching the directory, which is
    /// the normal outcome of a double eviction and never an error worth propagating.
    pub fn unwatch(&mut self, dir: &Path) -> Result<bool, SchedError> {
        if !self.watch_set.remove(dir) {
            return Ok(false);
        }
        let watcher = self
            .watcher
            .as_mut()
            .ok_or_else(|| SchedError::RemovePath {
                path: dir.to_path_buf(),
                source: notify::Error::generic("pipeline is shut down"),
            })?;
        match watcher.unwatch(dir) {
            Ok(()) => Ok(true),
            // notify reports a watch it no longer has; that is the state we wanted anyway.
            Err(notify::Error {
                kind: notify::ErrorKind::WatchNotFound,
                ..
            }) => Ok(false),
            Err(source) => Err(SchedError::RemovePath {
                path: dir.to_path_buf(),
                source,
            }),
        }
    }

    /// Stops watching, drains what is already queued, and delivers the final batch.
    pub fn shutdown(mut self) -> u64 {
        self.stop_all()
    }

    fn stop_all(&mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.watcher.take());
        drop(self.sender.take());
        if let Some(consumer) = self.consumer.take() {
            let _ = consumer.join();
        }
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Starts a watcher plus one serial consumer that batches deltas into `sink`.
///
/// Directories in `config.initial_watch` are observed in `config.initial_mode`; after that, use
/// [`Handle::watch`] and [`Handle::unwatch`] to follow the cache.
pub fn spawn(config: SchedulerConfig, sink: Box<dyn Sink>) -> Result<Handle, SchedError> {
    let (sender, receiver) = sync_channel::<Signal>(config.queue_capacity.max(1));
    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));

    let publisher = Publisher {
        sender: sender.clone(),
        dropped: Arc::clone(&dropped),
    };
    let mut watcher = RecommendedWatcher::new(
        move |outcome: Result<Event, notify::Error>| match outcome {
            Ok(event) => {
                publisher.publish(translate(&event));
            }
            Err(error) => publisher.publish_loss(&error),
        },
        NotifyConfig::default(),
    )?;

    let watch_set = Arc::new(WatchSet::new());
    for dir in &config.initial_watch {
        watcher
            .watch(dir, config.initial_mode.recursive_mode())
            .map_err(|source| SchedError::AddPath {
                path: dir.clone(),
                source,
            })?;
        let (mtime, seen) = match write_time(dir) {
            Ok(mtime) => (mtime, false),
            Err(_) => (SystemTime::UNIX_EPOCH, true),
        };
        watch_set.add(dir, mtime, seen);
    }

    let consumer_stop = Arc::clone(&stop);
    let consumer_dropped = Arc::clone(&dropped);
    let consumer_watches = Arc::clone(&watch_set);
    let consumer = thread::spawn(move || {
        let mut sink = sink;
        let mut batcher = Batcher::new(config.window, config.max_batch);
        let mut next_sweep = Instant::now() + RECONCILE_INTERVAL;
        let mut pacer = SweepPacer::new(SWEEP_WINDOW);
        loop {
            let now = Instant::now();
            if !batcher.is_empty() && batcher.expired(now) {
                flush(&mut batcher, sink.as_mut(), &consumer_dropped);
                continue;
            }
            if now >= next_sweep {
                next_sweep = now + RECONCILE_INTERVAL;
                let budget = pacer.budget(consumer_watches.len());
                let started = Instant::now();
                let report = consumer_watches.sweep(budget, write_time);
                pacer.observe(report.samples, started.elapsed());
                for signal in report.signals {
                    batcher.absorb(signal);
                }
            }
            if consumer_stop.load(Ordering::Relaxed) {
                drain_into(&receiver, &mut batcher);
                break;
            }
            let wait = batcher
                .remaining(now)
                .unwrap_or(IDLE_POLL)
                .clamp(Duration::from_millis(1), IDLE_POLL);
            match receiver.recv_timeout(wait) {
                Ok(signal) => {
                    // An event explains the write time it implies, so the sweep must know it landed.
                    if let Some(dir) = signal.directory() {
                        consumer_watches.mark_seen(dir);
                    }
                    if batcher.absorb(signal) {
                        flush(&mut batcher, sink.as_mut(), &consumer_dropped);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    drain_into(&receiver, &mut batcher);
                    break;
                }
            }
        }
        flush(&mut batcher, sink.as_mut(), &consumer_dropped);
    });

    Ok(Handle {
        stop,
        dropped,
        watcher: Some(watcher),
        sender: Some(sender),
        consumer: Some(consumer),
        watch_set,
    })
}

fn drain_into(receiver: &Receiver<Signal>, batcher: &mut Batcher) {
    while let Ok(signal) = receiver.try_recv() {
        batcher.absorb(signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, MetadataKind};
    use notify::ErrorKind;
    use std::sync::Mutex;

    fn event(kind: EventKind, paths: &[&str]) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    fn rows(signals: &[Signal]) -> Vec<&str> {
        signals
            .iter()
            .map(|signal| match signal {
                Signal::Row(change) => change.change.as_str(),
                Signal::Rescan { .. } => "rescan",
            })
            .collect()
    }

    fn changes(signals: Vec<Signal>) -> Vec<FileChange> {
        signals
            .into_iter()
            .filter_map(|signal| match signal {
                Signal::Row(change) => Some(change),
                Signal::Rescan { .. } => None,
            })
            .collect()
    }

    #[test]
    fn create_and_write_and_remove_each_produce_one_row_delta() {
        let created = translate(&event(
            EventKind::Create(CreateKind::File),
            &["D:/docs/a.txt"],
        ));
        let written = translate(&event(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &["D:/docs/a.txt"],
        ));
        let removed = translate(&event(
            EventKind::Remove(RemoveKind::File),
            &["D:/docs/a.txt"],
        ));

        assert_eq!(rows(&created), vec!["created"]);
        assert_eq!(rows(&written), vec!["modified"]);
        assert_eq!(rows(&removed), vec!["removed"]);
        assert_eq!(created[0].directory().unwrap().to_lossy_string(), "D:/DOCS");
        assert_eq!(changes(created)[0].path, PathBuf::from("D:/docs/a.txt"));
    }

    #[test]
    fn rename_becomes_removal_plus_creation() {
        let both = translate(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["D:/docs/old.txt", "D:/docs/new.txt"],
        ));
        let from = translate(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &["D:/docs/old.txt"],
        ));
        let to = translate(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            &["D:/docs/new.txt"],
        ));

        assert_eq!(rows(&both), vec!["removed", "created"]);
        assert_eq!(rows(&from), vec!["removed"]);
        assert_eq!(rows(&to), vec!["created"]);
    }

    #[test]
    fn unmatched_event_kinds_mark_noise() {
        assert_eq!(
            rows(&translate(&event(EventKind::Any, &["D:/a.txt"]))),
            vec!["other"]
        );
        let flushed = translate(&event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            &["D:/a.txt"],
        ));
        assert_eq!(rows(&flushed), vec!["modified"]);
    }

    #[test]
    fn a_backend_that_names_the_target_is_believed() {
        let folder = changes(translate(&event(
            EventKind::Create(CreateKind::Folder),
            &["D:/docs/sub"],
        )));
        assert_eq!(folder[0].target, Target::Directory);

        let file = changes(translate(&event(
            EventKind::Remove(RemoveKind::File),
            &["D:/docs/a.txt"],
        )));
        assert_eq!(file[0].target, Target::File);

        // The Windows backend reports `Any` for everything, which is why the consumer cannot
        // assume it was told.
        let opaque = changes(translate(&event(
            EventKind::Create(CreateKind::Any),
            &["D:/docs/whatever"],
        )));
        assert_eq!(opaque[0].target, Target::Unknown);
    }

    #[test]
    fn a_full_queue_drops_events_instead_of_blocking() {
        let (sender, receiver) = sync_channel::<Signal>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let publisher = Publisher {
            sender,
            dropped: Arc::clone(&dropped),
        };

        let discarded = publisher.publish(vec![
            Signal::Row(FileChange::new("D:/a", Change::Created)),
            Signal::Row(FileChange::new("D:/b", Change::Created)),
            Signal::Row(FileChange::new("D:/c", Change::Created)),
        ]);

        assert_eq!(discarded, 2);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert_eq!(
            receiver
                .recv()
                .unwrap()
                .directory()
                .unwrap()
                .to_lossy_string(),
            "D:"
        );
        assert_eq!(publisher.publish(Vec::new()), 0);
    }

    #[test]
    fn a_watcher_error_asks_for_a_rescan_of_the_directories_it_names() {
        let (sender, receiver) = sync_channel::<Signal>(4);
        let dropped = Arc::new(AtomicU64::new(0));
        let publisher = Publisher {
            sender,
            dropped: Arc::clone(&dropped),
        };

        publisher.publish_loss(
            &notify::Error::generic("backend said so").add_path(PathBuf::from("D:/docs")),
        );

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let signal = receiver.recv().unwrap();
        match signal {
            Signal::Rescan { dir, why } => {
                assert_eq!(dir.to_lossy_string(), "D:/DOCS");
                assert_eq!(why, RescanWhy::WatchLost);
            }
            other => panic!("expected a rescan, got {other:?}"),
        }
    }

    #[test]
    fn a_watcher_error_without_paths_still_counts_as_loss() {
        let (sender, receiver) = sync_channel::<Signal>(4);
        let dropped = Arc::new(AtomicU64::new(0));
        let publisher = Publisher {
            sender,
            dropped: Arc::clone(&dropped),
        };

        publisher.publish_loss(&notify::Error::new(ErrorKind::MaxFilesWatch));

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn noise_is_filtered_out_at_flush() {
        let seen = Arc::new(Mutex::new(Vec::<Batch>::new()));
        let collector = Arc::clone(&seen);
        let mut batcher = Batcher::new(Duration::from_secs(1), 10);
        batcher.absorb(Signal::Row(FileChange::new("D:/a", Change::Other)));
        let mut sink = move |batch: Batch| collector.lock().unwrap().push(batch);

        flush(&mut batcher, &mut sink, &AtomicU64::new(0));

        assert!(seen.lock().unwrap().is_empty());

        batcher.absorb(Signal::Row(FileChange::new("D:/b", Change::Created)));
        batcher.absorb(Signal::Rescan {
            dir: PathKey::of(Path::new("D:/c")),
            why: RescanWhy::Reconciled,
        });
        flush(&mut batcher, &mut sink, &AtomicU64::new(7));
        let batches = seen.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].dropped, 7);
        assert_eq!(batches[0].changes.len(), 1);
        assert_eq!(batches[0].rescans.len(), 1);
    }

    #[test]
    fn an_empty_watch_list_is_allowed() {
        // The cache starts watching nothing and adds directories as they are opened, so refusing an
        // empty set would rule out the product's main use.
        let handle = spawn(SchedulerConfig::default(), Box::new(|_: Batch| {}))
            .expect("a pipeline with no watches starts");

        assert_eq!(handle.watched_count(), 0);
        assert_eq!(handle.shutdown(), 0);
    }

    #[test]
    fn config_maps_onto_scheduler_settings() {
        let config = Config {
            event_queue_capacity: 12,
            ui_batch_ms: 250,
            ..Config::default()
        };

        let settings = SchedulerConfig::from_config(&config);

        assert_eq!(settings.queue_capacity, 12);
        assert_eq!(settings.window, Duration::from_millis(250));
        assert_eq!(settings.max_batch, DEFAULT_MAX_BATCH);
        assert!(settings.initial_watch.is_empty());
        assert_eq!(settings.initial_mode, Mode::OneLevel);
    }

    #[test]
    fn the_legacy_shape_watches_recursively_from_the_start() {
        let config = Config {
            event_queue_capacity: 8,
            ..Config::default()
        };

        let settings = SchedulerConfig::watching(&[PathBuf::from("D:/docs")], config);

        assert_eq!(settings.initial_watch, vec![PathBuf::from("D:/docs")]);
        assert_eq!(settings.initial_mode, Mode::Recursive);
        assert_eq!(settings.queue_capacity, 8);
    }
}
