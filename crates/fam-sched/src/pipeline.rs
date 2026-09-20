use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fam_core::Config;
use notify::event::{ModifyKind, RenameMode};
use notify::Config as NotifyConfig;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::batch::{Batch, Batcher, Change, FileChange};

pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;
pub const DEFAULT_MAX_BATCH: usize = 256;

const IDLE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub queue_capacity: usize,
    pub window: Duration,
    pub max_batch: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            window: Duration::from_millis(fam_core::DEFAULT_UI_BATCH_MS),
            max_batch: DEFAULT_MAX_BATCH,
        }
    }
}

impl SchedulerConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            queue_capacity: config.event_queue_capacity,
            window: config.ui_batch_window(),
            max_batch: DEFAULT_MAX_BATCH,
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
    #[error("no directories given to watch")]
    Empty,
}

/// Feeds a bounded queue without ever blocking the watcher callback: when the queue is full the
/// event is counted as dropped and discarded, as the legacy `Notify()` did (§4).
#[derive(Clone)]
struct Publisher {
    sender: SyncSender<FileChange>,
    dropped: Arc<AtomicU64>,
}

impl Publisher {
    fn publish(&self, changes: Vec<FileChange>) -> usize {
        let mut dropped = 0usize;
        for change in changes {
            if self.sender.try_send(change).is_err() {
                dropped += 1;
            }
        }
        if dropped > 0 {
            self.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
        }
        dropped
    }

    fn note_loss(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Maps a watcher event onto create / write / remove / rename deltas.
///
/// The old consumer switched on Create and Write only, so deletions and renames never reached the
/// UI (§4.2). Rename is emitted as a removal of the old path plus a creation of the new one,
/// because `ReadDirectoryChangesW` reports the pair as two volatile records.
fn translate(event: &Event) -> Vec<FileChange> {
    let change = match event.kind {
        EventKind::Create(_) => Change::Created,
        EventKind::Remove(_) => Change::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Change::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Change::Created,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            let mut pairs = Vec::with_capacity(event.paths.len());
            for (index, path) in event.paths.iter().enumerate() {
                let change = if index == 0 {
                    Change::Removed
                } else {
                    Change::Created
                };
                pairs.push(FileChange::new(path, change));
            }
            return pairs;
        }
        EventKind::Modify(_) => Change::Modified,
        _ => Change::Other,
    };
    event
        .paths
        .iter()
        .map(|path| FileChange::new(path, change))
        .collect()
}

fn flush(batcher: &mut Batcher, sink: &mut dyn Sink, dropped: &AtomicU64) {
    let changes: Vec<FileChange> = batcher
        .drain()
        .into_iter()
        .filter(|change| change.change != Change::Other)
        .collect();
    if changes.is_empty() {
        return;
    }
    let discarded = dropped.swap(0, Ordering::Relaxed);
    sink.flush(Batch::new(changes, discarded));
}

pub struct Handle {
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    watcher: Option<RecommendedWatcher>,
    sender: Option<SyncSender<FileChange>>,
    consumer: Option<JoinHandle<()>>,
}

impl Handle {
    /// Events discarded because the queue was full — surfaced so a UI can warn about stale rows.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
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

/// Starts a watcher thread plus one serial consumer that batches deltas into `sink`.
pub fn spawn(
    dirs: &[PathBuf],
    config: SchedulerConfig,
    sink: Box<dyn Sink>,
) -> Result<Handle, SchedError> {
    if dirs.is_empty() {
        return Err(SchedError::Empty);
    }
    let (sender, receiver) = sync_channel::<FileChange>(config.queue_capacity.max(1));
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
            Err(_) => publisher.note_loss(),
        },
        NotifyConfig::default(),
    )?;
    for dir in dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|source| SchedError::AddPath {
                path: dir.clone(),
                source,
            })?;
    }

    let consumer_stop = Arc::clone(&stop);
    let consumer_dropped = Arc::clone(&dropped);
    let consumer = thread::spawn(move || {
        let mut sink = sink;
        let mut batcher = Batcher::new(config.window, config.max_batch);
        loop {
            let now = Instant::now();
            if !batcher.is_empty() && batcher.expired(now) {
                flush(&mut batcher, sink.as_mut(), &consumer_dropped);
                continue;
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
                Ok(change) => {
                    if batcher.absorb(change) {
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
    })
}

fn drain_into(receiver: &Receiver<FileChange>, batcher: &mut Batcher) {
    while let Ok(change) = receiver.try_recv() {
        batcher.absorb(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};
    use std::sync::Mutex;

    fn event(kind: EventKind, paths: &[&str]) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    fn kinds(changes: &[FileChange]) -> Vec<&str> {
        changes
            .iter()
            .map(|change| change.change.as_str())
            .collect()
    }

    #[test]
    fn create_and_write_and_remove_each_produce_one_row_delta() {
        let created = translate(&event(
            EventKind::Create(CreateKind::File),
            &["D:/docs/a.txt"],
        ));
        let written = translate(&event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            &["D:/docs/a.txt"],
        ));
        let removed = translate(&event(
            EventKind::Remove(RemoveKind::File),
            &["D:/docs/a.txt"],
        ));

        assert_eq!(kinds(&created), vec!["created"]);
        assert_eq!(kinds(&written), vec!["modified"]);
        assert_eq!(kinds(&removed), vec!["removed"]);
        assert_eq!(created[0].path, PathBuf::from("D:/docs/a.txt"));
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

        assert_eq!(kinds(&both), vec!["removed", "created"]);
        assert_eq!(kinds(&from), vec!["removed"]);
        assert_eq!(kinds(&to), vec!["created"]);
    }

    #[test]
    fn unmatched_event_kinds_mark_noise() {
        let noise = translate(&event(EventKind::Any, &["D:/docs/a.txt"]));
        assert_eq!(kinds(&noise), vec!["other"]);
    }

    #[test]
    fn a_full_queue_drops_events_instead_of_blocking() {
        let (sender, receiver) = sync_channel::<FileChange>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let publisher = Publisher {
            sender,
            dropped: Arc::clone(&dropped),
        };

        let discarded = publisher.publish(vec![
            FileChange::new("D:/a", Change::Created),
            FileChange::new("D:/b", Change::Created),
            FileChange::new("D:/c", Change::Created),
        ]);

        assert_eq!(discarded, 2);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert_eq!(receiver.recv().unwrap().path, PathBuf::from("D:/a"));
        assert_eq!(publisher.publish(Vec::new()), 0);
    }

    #[test]
    fn watcher_errors_count_as_loss() {
        let (sender, _receiver) = sync_channel::<FileChange>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let publisher = Publisher {
            sender,
            dropped: Arc::clone(&dropped),
        };

        publisher.note_loss();

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn noise_is_filtered_out_at_flush() {
        let seen = Arc::new(Mutex::new(Vec::<Batch>::new()));
        let collector = Arc::clone(&seen);
        let mut batcher = Batcher::new(Duration::from_secs(1), 10);
        batcher.absorb(FileChange::new("D:/a", Change::Other));
        let mut sink = move |batch: Batch| collector.lock().unwrap().push(batch);

        flush(&mut batcher, &mut sink, &AtomicU64::new(0));

        assert!(seen.lock().unwrap().is_empty());

        batcher.absorb(FileChange::new("D:/b", Change::Created));
        flush(&mut batcher, &mut sink, &AtomicU64::new(7));
        let batches = seen.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].dropped, 7);
        assert_eq!(kinds(&batches[0].changes), vec!["created"]);
    }

    #[test]
    fn spawn_refuses_an_empty_watch_list() {
        let error = spawn(&[], SchedulerConfig::default(), Box::new(|_: Batch| {}))
            .err()
            .expect("an empty watch list must be refused");
        assert!(matches!(error, SchedError::Empty));
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
    }
}
