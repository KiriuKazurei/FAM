//! What one v2 directory token holds, and who is allowed to change it.
//!
//! A view is a [`Collection`] - a sorted, filtered ordering over one cached table - plus the cuts the
//! change pipeline has queued for it. Two threads other than the caller touch it:
//!
//! * the pipeline consumer, which folds a row-level patch into the cache and then into every view of
//!   that directory, queueing positions for the model to cut ([`Views::absorb`]);
//! * the loader, which installs a freshly read table and asks the views to start over
//!   ([`Views::refresh`]).
//!
//! The calling thread - the one painting a list - only ever reads a view under its own lock. That is
//! what keeps the v2 promise that no entry point touches the filesystem: everything expensive happens
//! behind these two.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use fam_core::{Cancellation, PathKey};
use fam_sched::{Change, DirDelta, Handle, Mode};
use fam_svc_index::{Applied, Claim, Collection, DirIndex, Patch, Snapshot, State};

use crate::wire;

/// One token's worth of view state.
pub struct DirView {
    /// Rows on offer, or `None` while the first read is still running: there is nothing to order yet.
    rows: Option<Collection>,
    /// Positions to cut, in the order they must be applied, queued since the last poll.
    cuts: Vec<usize>,
    seq: u64,
    /// Set when the rows changed in a way positions cannot express - a re-read, a new sort, a patch
    /// that arrived while this view was on a superseded generation.
    resync: bool,
    /// The core's dropped-event count as of the last moment this view was known whole: when it was
    /// opened, or just before the read that last replaced its rows. A count above this is a loss
    /// this view actually lived through; one below it happened to somebody else, or before this view
    /// existed, and is not this view's business.
    loss_baseline: u64,
}

impl DirView {
    fn new(snapshot: Option<Snapshot>, losses: u64) -> Self {
        Self {
            rows: snapshot.map(Collection::from_snapshot),
            cuts: Vec::new(),
            seq: 0,
            resync: false,
            loss_baseline: losses,
        }
    }

    /// Moves the baseline up to a count taken *before* the read that just replaced these rows.
    ///
    /// Before rather than after, so a loss that lands while the read is in flight stays visible:
    /// the read cannot have accounted for it.
    fn advance_loss_baseline(&mut self, losses: u64) {
        self.loss_baseline = losses;
    }

    /// Whether this view has outlived a dropped event without being re-read since.
    fn lost_events(&self, losses: u64) -> bool {
        losses > self.loss_baseline
    }

    /// Applies a sort or filter change. The ordering is about to be rebuilt, so queued positions are
    /// meaningless and the model needs the whole view again.
    pub fn reorder(&mut self, apply: impl FnOnce(&mut Collection)) {
        if let Some(collection) = self.rows.as_mut() {
            apply(collection);
        }
        self.cuts.clear();
        self.resync = true;
    }

    /// Rows for one window, plus the total the view holds. `None` means the directory has no rows yet.
    pub fn window(
        &self,
        first: usize,
        count: usize,
    ) -> Option<(Vec<fam_svc_index::FetchedRow>, u32)> {
        self.rows
            .as_ref()
            .map(|rows| (rows.fetch(first, count), rows.len() as u32))
    }

    pub fn total_rows(&self) -> u32 {
        self.rows.as_ref().map_or(0, |rows| rows.len() as u32)
    }

    pub fn generation(&self) -> u64 {
        self.rows.as_ref().map_or(0, Collection::generation)
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Takes up to `fitting` queued cuts, or reports a resync.
    ///
    /// A prefix rather than all-or-nothing, because a cut list the caller's buffer cannot hold must
    /// not be thrown away: the rest stays queued and the truncated flag tells the caller to poll
    /// again. Positions are only meaningful in order, which is what makes a prefix legal and a
    /// middle slice not. The sequence advances only when there was something to say: a poll that
    /// changed nothing must not move the number a caller compares against.
    pub fn take_cuts(&mut self, fitting: usize) -> (Vec<usize>, bool) {
        if self.resync {
            self.resync = false;
            self.cuts.clear();
            self.seq += 1;
            return (Vec::new(), true);
        }
        if self.cuts.is_empty() || fitting == 0 {
            return (Vec::new(), false);
        }
        let cuts = self.cuts.drain(..fitting.min(self.cuts.len())).collect();
        self.seq += 1;
        (cuts, false)
    }

    /// Cuts waiting to be handed over, which is also what decides whether a poll has anything to say.
    pub fn pending_cuts(&self) -> usize {
        self.cuts.len()
    }

    pub fn pending_resync(&self) -> bool {
        self.resync
    }

    pub fn has_rows(&self) -> bool {
        self.rows.is_some()
    }

    fn patch_with(&mut self, snapshot: &Snapshot, removed: &[u32]) {
        if self.rows.is_none() {
            self.rows = Some(Collection::from_snapshot(snapshot.clone()));
        }
        let cuts = self
            .rows
            .as_mut()
            .expect("just installed")
            .patch(snapshot, removed);
        self.cuts.extend(cuts);
    }

    fn resync_with(&mut self, snapshot: &Snapshot) {
        let had_rows = self.has_rows();
        self.rows = Some(Collection::from_snapshot(snapshot.clone()));
        self.cuts.clear();
        // Going from nothing to something is not a rebuild: the model has never been told there were
        // rows, so the state change is the announcement. Only a view that was showing something
        // needs "throw it out".
        self.resync |= had_rows;
    }

    /// The cached rows are now suspect but unchanged: keep showing them, and say so in the header.
    fn mark_stale(&mut self, snapshot: &Snapshot) {
        if self.rows.is_none() {
            self.rows = Some(Collection::from_snapshot(snapshot.clone()));
        }
        self.resync = true;
    }
}

/// Every live view, with one lock per view so a slow fetch cannot stall another directory.
///
/// The directory a token belongs to is stored beside it rather than read out of the view, so "which
/// views watch this directory" never takes a view lock. The only lock order in this module is
/// registry-then-view, and keeping it that way is what makes the fan-out below free of deadlock.
#[derive(Default)]
struct Registry {
    by_token: HashMap<u64, (PathKey, Arc<Mutex<DirView>>)>,
}

/// The token registry. Cheap to clone; every clone sees the same views.
#[derive(Clone)]
pub struct Views {
    inner: Arc<Mutex<Registry>>,
    next_token: Arc<AtomicU64>,
}

/// Tokens start at 1 because `0` is what every token-returning entry point reports as failure.
impl Default for Views {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Registry::default())),
            next_token: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl Views {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts a view of `key` at whatever the cache already holds, and hands back its token. Two
    /// tokens on one directory are allowed, and each gets its own sort, filter and cut queue while
    /// sharing the cached rows. `losses` is the core's dropped-event count right now, which becomes
    /// this token's baseline: a loss that happened before the view existed is not something the view
    /// was promised and then denied.
    pub fn open(&self, index: &DirIndex, key: &PathKey, losses: u64) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let view = Arc::new(Mutex::new(DirView::new(index.peek(key), losses)));
        self.lock().by_token.insert(token, (key.clone(), view));
        token
    }

    pub fn close(&self, token: u64) -> Option<PathKey> {
        self.lock().by_token.remove(&token).map(|(key, _)| key)
    }

    /// Borrows one view without holding the registry lock, so a caller's fetch runs while every other
    /// token stays free. `None` is the answer for a token this core never handed out, or has closed.
    pub fn view(&self, token: u64) -> Option<Arc<Mutex<DirView>>> {
        self.locate(token).map(|(_, view)| view)
    }

    /// A token's directory and its view, or `None` for a token this core never handed out.
    pub fn locate(&self, token: u64) -> Option<(PathKey, Arc<Mutex<DirView>>)> {
        self.lock().by_token.get(&token).cloned()
    }

    fn each_view_of(&self, key: &PathKey) -> Vec<Arc<Mutex<DirView>>> {
        self.lock()
            .by_token
            .values()
            .filter(|(view_key, _)| view_key == key)
            .map(|(_, view)| Arc::clone(view))
            .collect()
    }

    /// Folds one directory's worth of watcher rows into the cache, then into every view of it.
    ///
    /// Returns whether anything visible moved, which is what tells the shell a repaint is owed. A view
    /// still on the generation the patch was computed against follows it in place; one that is behind
    /// gets a resync, because positions cut against a different table would remove the wrong rows.
    pub fn absorb(&self, index: &DirIndex, delta: &DirDelta) -> bool {
        let Some(before) = index.peek(&delta.dir) else {
            // Nothing cached, so nothing to patch: the rows will come from a read, and a read that
            // has not happened yet is not a change this view was promised.
            return false;
        };
        let views = self.each_view_of(&delta.dir);
        if views.is_empty() {
            return false;
        }
        match index.apply(&delta.dir, before.generation, &to_patch(delta)) {
            Applied::Patched { snapshot, removed } => {
                for view in views {
                    let mut view = view.lock().expect("view lock");
                    if view.generation() == before.generation {
                        view.patch_with(&snapshot, &removed);
                    } else {
                        view.resync_with(&snapshot);
                    }
                }
                true
            }
            Applied::Nothing { marked_stale: true } => {
                for view in views {
                    view.lock().expect("view lock").mark_stale(&before);
                }
                false
            }
            Applied::Nothing { .. } => false,
        }
    }

    /// Points every view of `key` at the rows a read just installed, and tells their models to start
    /// over: a read is a new table, and its record indices mean something different.
    ///
    /// `seen` is the dropped-event count taken before that read, and is `Some` only when the read
    /// succeeded. A completed read is the one thing that makes earlier losses moot - the table below
    /// is whole again - so it moves each view's baseline up. A failed read leaves the old rows in
    /// place and the warning with them.
    pub fn refresh(&self, index: &DirIndex, key: &PathKey, seen: Option<u64>) {
        let Some(snapshot) = index.peek(key) else {
            return;
        };
        for view in self.each_view_of(key) {
            let mut view = view.lock().expect("view lock");
            view.resync_with(&snapshot);
            if let Some(losses) = seen {
                view.advance_loss_baseline(losses);
            }
        }
    }

    /// What one token can serve right now, which is what every header is built from. `losses` is the
    /// core's dropped-event count as of this call.
    pub fn status(&self, index: &DirIndex, token: u64, losses: u64) -> Option<Status> {
        let (key, view) = self.locate(token)?;
        let view = view.lock().expect("view lock");
        let on_offer = index.peek(&key);
        let state = match &on_offer {
            // Nothing to show yet: the directory's own slot says whether a read is running, failed,
            // or has not been asked for.
            None => wire_state(index.state(&key)),
            Some(rows) => {
                if !view.has_rows() || rows.generation > view.generation() {
                    // Either this view is showing nothing where the cache has something, or a read
                    // replaced the table underneath it. What is on screen is not what is on disk.
                    wire::state::STALE
                } else if rows.is_stale() {
                    // Same rows, but the cache says they are no longer the whole truth - which is
                    // where a create or a modification lands: the patch could not place them, so
                    // only a re-read can.
                    wire::state::STALE
                } else {
                    wire::state::READY
                }
            }
        };
        Some(Status {
            state,
            total_rows: view.total_rows(),
            generation: view.generation(),
            seq: view.seq(),
            resync: view.pending_resync(),
            loss: view.lost_events(losses),
        })
    }
}

/// A view's answer to "what have you got".
pub struct Status {
    pub state: u8,
    pub total_rows: u32,
    pub generation: u64,
    pub seq: u64,
    pub resync: bool,
    /// Events were discarded while this view was open and it has not been re-read since, so what is
    /// on screen may be missing rows.
    pub loss: bool,
}

/// Turns a scheduler's row list into what the cache can act on.
///
/// Removals are the only rows a cached table can place without another read: a `Created` or
/// `Modified` event names a row whose size, time and folder-or-not are not in the event, and one
/// attribute query costs about what re-reading the whole directory does on this machine
/// (`docs/notes/watch-sweep-costs-a-path-lookup.md`).
fn to_patch(delta: &DirDelta) -> Patch {
    let mut patch = Patch::default();
    for row in &delta.rows {
        if row.change == Change::Removed {
            patch.removed.push(row.name.clone());
        } else {
            patch.unread += 1;
        }
    }
    patch
}

/// Maps the cache's own idea of a slot onto the wire's state byte.
pub fn wire_state(state: State) -> u8 {
    match state {
        State::Ready => wire::state::READY,
        State::Stale => wire::state::STALE,
        State::Loading => wire::state::LOADING,
        State::Failed => wire::state::FAILED,
        State::Empty => wire::state::EMPTY,
    }
}

/// What the loader thread does, and the only thing that may touch the filesystem for a v2 caller.
pub enum Job {
    /// Read this directory and install the result. The claim *is* the right to do it: one was won for
    /// this job, so no other thread is reading the same directory.
    Load(Claim),
    /// Start watching a directory one level deep, now that a view needs it kept current.
    Watch(PathBuf),
    /// Stop watching it, because the last view that needed it closed.
    Unwatch(PathBuf),
    /// Leave the loop. Explicit, rather than "wait until every sender is gone", because the sink
    /// thread holds a sender too and would otherwise have to be torn down first, in order.
    Stop,
}

/// One background thread doing the path work no v2 entry point may do on its caller's thread.
///
/// Reads and watch registrations share a queue on purpose: both are path lookups, both cost about a
/// millisecond here, and keeping them in one place is what lets `fam_open_dir` return immediately and
/// be honest about it. The queue is unbounded because the alternative is dropping a claim, and a
/// dropped claim leaves a directory reporting "loading" until something else opens it.
#[derive(Clone)]
pub struct Loader {
    queue: Arc<Mutex<Sender<Job>>>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Loader {
    /// The queue every poster shares. Built apart from the thread because the pipeline sink needs to
    /// post into it, and the sink has to exist before the pipeline it is spawned with.
    pub fn channel() -> (Sender<Job>, Receiver<Job>) {
        channel::<Job>()
    }

    pub fn start(
        receiver: Receiver<Job>,
        index: DirIndex,
        views: Views,
        queue: Arc<Mutex<Sender<Job>>>,
        pipeline: Arc<Mutex<Handle>>,
        losses: Arc<AtomicU64>,
    ) -> Self {
        let thread = std::thread::Builder::new()
            .name("fam-loader".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let watching = matches!(job, Job::Watch(_));
                    match job {
                        Job::Load(claim) => {
                            let key = claim.key().clone();
                            let seen = losses.load(Ordering::Relaxed);
                            match claim.read(&Cancellation::new()) {
                                Ok(table) => {
                                    claim.fill(table);
                                    views.refresh(&index, &key, Some(seen));
                                }
                                // `fail` records the backoff, so a directory that refuses us is not
                                // re-read on every repaint. The rows a view is showing did not
                                // change, so neither does its loss baseline.
                                Err(_) => {
                                    claim.fail();
                                    views.refresh(&index, &key, None);
                                }
                            }
                        }
                        Job::Watch(dir) | Job::Unwatch(dir) => {
                            let mut handle = pipeline
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            let outcome = if watching {
                                handle.watch(&dir, Mode::OneLevel).map(|_| ())
                            } else {
                                handle.unwatch(&dir).map(|_| ())
                            };
                            if let Err(error) = outcome {
                                log::warn!("cannot change the watch on {}: {error}", dir.display());
                            }
                        }
                        Job::Stop => break,
                    }
                }
            });
        Self {
            queue,
            thread: Arc::new(Mutex::new(thread.ok())),
        }
    }

    pub fn post(&self, job: Job) {
        let queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = queue.send(job);
    }

    /// Stops the thread and waits for its last read to finish, so a core cannot be freed underneath
    /// it. Safe to call more than once; only the first call joins.
    pub fn shutdown(&self) {
        self.post(Job::Stop);
        let mut thread = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(handle) = thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a real directory into the cache the way the loader does, so the views below have a
    /// table to serve rather than nothing.
    fn cached(index: &DirIndex, key: &PathKey) {
        let claim = index.claim(key).expect("nothing else is reading it");
        let table = claim.read(&Cancellation::new()).expect("temp dir reads");
        claim.fill(table);
    }

    fn fixture() -> (tempfile::TempDir, DirIndex, PathKey) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("payload.txt"), "x").unwrap();
        let index = DirIndex::new();
        let key = PathKey::of(dir.path());
        cached(&index, &key);
        (dir, index, key)
    }

    /// The bug this pins: `losses` is one counter for the whole core and it never goes down, so a
    /// view that simply compared it against zero reported "events dropped" on every poll for the
    /// rest of the process - including views opened long afterwards, which is what made the warning
    /// worthless. A baseline taken at open is what separates "lived through a loss" from "exists".
    #[test]
    fn a_loss_lands_on_the_view_open_for_it_and_not_on_the_one_that_came_later() {
        let (_dir, index, key) = fixture();
        let views = Views::new();

        let witness = views.open(&index, &key, 0);
        let latecomer = views.open(&index, &key, 1);

        assert!(
            views.status(&index, witness, 1).unwrap().loss,
            "one event was dropped while this token was open"
        );
        assert!(
            !views.status(&index, latecomer, 1).unwrap().loss,
            "this token was handed its rows after the loss, so it was promised nothing it missed"
        );
    }

    #[test]
    fn a_completed_read_clears_the_loss_it_postdates_and_a_failed_read_does_not() {
        let (_dir, index, key) = fixture();
        let views = Views::new();
        let token = views.open(&index, &key, 0);

        assert!(views.status(&index, token, 3).unwrap().loss);

        // The count a read carries in is the one taken *before* it started, so a loss during the
        // read is still standing afterwards.
        views.refresh(&index, &key, Some(3));
        assert!(
            !views.status(&index, token, 3).unwrap().loss,
            "the rows on offer were read after those events went missing"
        );
        assert!(views.status(&index, token, 4).unwrap().loss);

        // A read that failed left the old rows in place, so nothing about them got more certain.
        views.refresh(&index, &key, None);
        assert!(views.status(&index, token, 4).unwrap().loss);
    }

    #[test]
    fn closing_a_view_does_not_move_the_baseline_of_the_one_still_open() {
        let (_dir, index, key) = fixture();
        let views = Views::new();
        let staying = views.open(&index, &key, 0);
        let leaving = views.open(&index, &key, 0);

        views.close(leaving);

        assert!(views.status(&index, staying, 1).unwrap().loss);
        assert!(views.status(&index, leaving, 1).is_none());
    }
}
