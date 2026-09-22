//! Paged name search: one owner thread, one result set per token, rows handed out in windows.
//!
//! v1's search returned everything at once - a JSON array of every hit - because the backend it was
//! built on had no other shape to give. The
//! [`SearchProvider`](fam_core::SearchProvider) contract pushes hits at a sink instead, so a search
//! can be read *while it runs*: `fam_search_page` hands out the prefix found so far with
//! `state = LOADING`, and the set flips to `READY` when the provider says it covered the scope.
//!
//! Sorting happens once, at the end. While rows are arriving they stay in discovery order, and
//! re-sorting a growing list under a scrolling view would move rows out from under the pointer -
//! the same reason the directory cache does not sort progressively.
//!
//! One owner thread rather than one per search: the Everything client API is process-global with a
//! single result set, so two searches cannot run concurrently against it no matter how many threads
//! this side has.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use fam_core::{Cancellation, Item, ItemFlags, Kind, PathKey, SearchHit, SearchRequest};
use fam_svc_index::{Collection, DirTable, SearchRouter, Searched};

use crate::wire;

/// One row of a result set: the record, and the path its name field points at.
struct Row {
    item: Item,
    path: Vec<u16>,
}

/// Where a search's rows are, which changes once the provider finishes.
enum Rows {
    /// Still arriving, in discovery order.
    Growing(Vec<Row>),
    /// Finished, sorted by the same comparator a directory view uses.
    Sorted(Collection),
}

/// One search token's state.
pub struct Search {
    request: SearchRequest,
    cancel: Cancellation,
    rows: Rows,
    /// Set when the provider covered the scope. A page taken before that is a prefix, and the
    /// header says so with [`wire::FLAG_INCOMPLETE`].
    complete: bool,
    /// Nothing more will be pushed: the provider answered, the caller stopped it, or it failed. This
    /// is what separates `LOADING` from a final state; [`Self::complete`] alone would call a
    /// cancelled search "still running" forever.
    settled: bool,
    /// The provider reported an error. Its partial rows, if any, stay pageable.
    failed: bool,
}

impl Search {
    fn new(request: SearchRequest) -> Self {
        Self {
            request,
            cancel: Cancellation::new(),
            rows: Rows::Growing(Vec::new()),
            complete: false,
            settled: false,
            failed: false,
        }
    }

    fn push(&mut self, hit: SearchHit) {
        let Rows::Growing(rows) = &mut self.rows else {
            return;
        };
        let directory = hit.kind == Kind::Directory;
        rows.push(Row {
            item: Item {
                name_off: 0,
                name_units: 0,
                kind: hit.kind.index(),
                flags: if directory {
                    ItemFlags::DIRECTORY
                } else {
                    ItemFlags::default()
                }
                .0,
                size: if directory { 0 } else { hit.size },
                mtime_ms: hit.mtime_ms,
                attrs: 0,
                dir: 0,
            },
            // The whole path, verbatim, in the field a directory row keeps as its name: a result set
            // spans directories, so the name has to say where the file is as well as what it is.
            path: hit.path,
        });
    }

    /// Sorts once, on the owner thread, when the provider says it is done.
    fn finish(&mut self, searched: Searched) {
        let Rows::Growing(rows) = &mut self.rows else {
            return;
        };
        // A result set is a virtual folder: it is not in the cache and its key is never looked up,
        // but `DirTable` wants one and the scope is the closest honest thing to name it by.
        let key = PathKey::of(
            self.request
                .scope
                .as_deref()
                .unwrap_or(std::path::Path::new("/search")),
        );
        let mut table = DirTable::new(1, key, None);
        for row in rows.iter() {
            table.push(&row.path, row.item);
        }
        self.complete = searched.outcome.complete;
        self.settled = true;
        self.rows = Rows::Sorted(Collection::new(Arc::new(table)));
    }

    fn window(&self, first: usize, count: usize) -> Vec<(Item, Vec<u16>)> {
        match &self.rows {
            Rows::Growing(rows) => rows
                .iter()
                .skip(first)
                .take(count)
                .map(|row| (row.item, row.path.clone()))
                .collect(),
            Rows::Sorted(collection) => collection
                .fetch(first, count)
                .into_iter()
                .map(|row| (row.item, row.name))
                .collect(),
        }
    }

    fn total(&self) -> u32 {
        match &self.rows {
            Rows::Growing(rows) => rows.len() as u32,
            Rows::Sorted(collection) => collection.len() as u32,
        }
    }

    fn head(&self) -> wire::FamRowHeader {
        let state = if self.failed {
            wire::state::FAILED
        } else if self.settled {
            wire::state::READY
        } else {
            wire::state::LOADING
        };
        // A search that stopped early - cancelled, or truncated at its limit - is final but partial,
        // and the flag is what tells those two apart from a complete answer.
        let flags = if self.complete {
            0
        } else {
            wire::FLAG_INCOMPLETE
        };
        wire::status(state, self.total(), 0, 0, flags)
    }
}

/// One page of a result set: the header that describes it, and rows as a record plus the units its
/// name field points at.
pub struct Page {
    pub head: wire::FamRowHeader,
    pub rows: Vec<(Item, Vec<u16>)>,
}

/// The registry plus the thread that runs searches. Cheap to clone.
#[derive(Clone)]
pub struct Searches {
    inner: Arc<Mutex<HashMap<u64, Arc<Mutex<Search>>>>>,
    next: Arc<AtomicU64>,
    queue: Arc<Mutex<Sender<u64>>>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// The token that tells the owner thread to leave its loop. `0` is not a token the registry hands
/// out, because [`Searches::begin`] starts counting at 1.
const STOP: u64 = 0;

impl Searches {
    pub fn start(router: Arc<SearchRouter>) -> Self {
        let (sender, receiver) = channel::<u64>();
        let this = Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicU64::new(1)),
            queue: Arc::new(Mutex::new(sender)),
            thread: Arc::new(Mutex::new(None)),
        };
        let running = this.clone();
        let thread = std::thread::Builder::new()
            .name("fam-search".into())
            .spawn(move || {
                while let Ok(token) = receiver.recv() {
                    if token == STOP {
                        break;
                    }
                    running.run(token, &router);
                }
            });
        if let Ok(mut slot) = this.thread.lock() {
            *slot = thread.ok();
        }
        this
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Arc<Mutex<Search>>>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn state(search: &Arc<Mutex<Search>>) -> MutexGuard<'_, Search> {
        search
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers a search and queues it. Returns the token immediately; rows appear as the provider
    /// pushes them, and the header's state says when they are final.
    pub fn begin(&self, request: SearchRequest) -> u64 {
        let token = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock()
            .insert(token, Arc::new(Mutex::new(Search::new(request))));
        if let Ok(queue) = self.queue.lock() {
            let _ = queue.send(token);
        }
        token
    }

    /// Rows `[first, first + count)` as they stand right now, with the header that describes them.
    /// `count` 0 is the cheap way to poll state: the header comes back with no rows behind it.
    pub fn page(&self, token: u64, first: usize, count: usize) -> Option<Page> {
        let search = self.get(token)?;
        let state = Self::state(&search);
        Some(Page {
            head: state.head(),
            rows: state.window(first, count),
        })
    }

    pub fn get(&self, token: u64) -> Option<Arc<Mutex<Search>>> {
        self.lock().get(&token).map(Arc::clone)
    }

    /// Stops a running search. The provider polls the same signal between directories and between
    /// entries, so this really does stop the walk - v1's timeout only made the caller give up while
    /// the scan kept running.
    pub fn cancel(&self, token: u64) -> bool {
        let Some(search) = self.get(token) else {
            return false;
        };
        Self::state(&search).cancel.cancel();
        true
    }

    pub fn close(&self, token: u64) -> bool {
        match self.lock().remove(&token) {
            Some(search) => {
                Self::state(&search).cancel.cancel();
                true
            }
            None => false,
        }
    }

    fn run(&self, token: u64, router: &SearchRouter) {
        let Some(search) = self.get(token) else {
            return;
        };
        let (request, cancel) = {
            let state = Self::state(&search);
            (state.request.clone(), state.cancel.clone())
        };
        let searched = router.search(&request, &cancel, &mut |hit| {
            let mut state = Self::state(&search);
            if state.cancel.is_stopped() {
                return false;
            }
            state.push(hit);
            true
        });
        let mut state = Self::state(&search);
        match searched {
            Ok(searched) if !cancel.is_stopped() => state.finish(searched),
            // A cancelled search is final but partial, not an error: what it found before the stop is
            // still true, and `complete` staying false is what tells the caller the scope was not
            // covered.
            Ok(_) => state.settled = true,
            Err(_) => {
                state.failed = true;
                state.settled = true;
            }
        }
    }

    /// Stops the thread and waits for its current search to notice.
    pub fn shutdown(&self) {
        self.cancel_all();
        if let Ok(queue) = self.queue.lock() {
            let _ = queue.send(STOP);
        }
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = thread.join();
        }
    }

    fn cancel_all(&self) {
        for (_, search) in self.lock().iter() {
            Self::state(search).cancel.cancel();
        }
    }
}
