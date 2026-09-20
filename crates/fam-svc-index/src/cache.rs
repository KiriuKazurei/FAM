//! The lazy directory cache.
//!
//! Four properties hold the design together, each with a test that fails if it breaks:
//!
//! 1. **A table is served whole.** Enumeration fills a detached builder and the slot flips once the
//!    read completes, so no caller can observe half a directory.
//! 2. **The view never blanks.** While a refresh runs, or cools down after failing, the previous
//!    rows keep being handed out marked stale. A directory that becomes unreadable keeps what it
//!    had rather than showing an empty folder.
//! 3. **The map is never held across filesystem work.** Under the lock there is only a hash lookup
//!    and a counter bump, which is what lets a pool of threads read different directories at once.
//!    Two threads can never both enumerate the same directory: `claim` gives that right to one.
//! 4. **A generation identifies rows, not attempts.** It advances when a table is installed and
//!    stays put across a failed or abandoned refresh, so a caller can tell new content from a retry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fam_core::{Cancellation, Config, DirId, Item, PathKey};
use fam_svc_enumerate::{list_one_level, EnumError};

use crate::dir_table::DirTable;

/// First retry after a failed read, doubled per attempt up to [`MAX_BACKOFF`]. A directory that
/// cannot be read - permissions, a spun-down drive - must not be rescanned on every repaint.
const BASE_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Enum(#[from] EnumError),
    #[error("enumeration stopped before the directory was fully read")]
    Cancelled,
}

/// How current a [`Snapshot`]'s rows are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Current,
    /// The rows predate an invalidation, a running refresh, or a failed one. They still display;
    /// they are simply no longer guaranteed to match the filesystem.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Rows cached and current.
    Ready,
    /// Rows cached but not guaranteed current.
    Stale,
    /// A read is running and there are no previous rows to show instead.
    Loading,
    /// The last read failed, nothing survived it, and the backoff has not elapsed.
    Failed,
    /// Nothing cached and nothing in flight.
    Empty,
}

/// Rows for one directory plus the version they came from.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub table: Arc<DirTable>,
    pub generation: u64,
    pub freshness: Freshness,
}

impl Snapshot {
    pub fn is_stale(&self) -> bool {
        self.freshness == Freshness::Stale
    }
}

/// What [`DirIndex::open`] found. Anything other than `Ready` means rows are unavailable right now,
/// and the variant says why.
#[derive(Debug, Clone)]
pub enum Opened {
    Ready(Snapshot),
    /// Another thread is reading this directory and there is nothing to show yet.
    Loading,
    /// The previous attempt failed; the payload is how long is left to wait.
    CoolingDown(Duration),
}

enum Slot {
    /// First read of this directory. `tries` carries failure history forward, so a retry that also
    /// fails keeps escalating its backoff instead of restarting at 100ms.
    Loading { claim: DirId, tries: u32 },
    /// Refresh of a directory whose rows are cached. `table` keeps serving, as stale, until the
    /// read finishes, fails, or is given up.
    Reloading {
        claim: DirId,
        tries: u32,
        table: Arc<DirTable>,
        generation: u64,
        seen: u64,
    },
    Ready {
        table: Arc<DirTable>,
        generation: u64,
        seen: u64,
        stale: bool,
    },
    Failed {
        table: Option<Arc<DirTable>>,
        generation: u64,
        seen: u64,
        tries: u32,
        not_before: Instant,
    },
}

impl Slot {
    /// Rows this slot may hand out, and whether they are still trustworthy.
    fn servable(&self) -> Option<(&Arc<DirTable>, Freshness)> {
        match self {
            Slot::Ready {
                table,
                stale: false,
                ..
            } => Some((table, Freshness::Current)),
            Slot::Ready { .. } | Slot::Reloading { .. } => Some((self.rows()?, Freshness::Stale)),
            Slot::Failed {
                table: Some(table), ..
            } => Some((table, Freshness::Stale)),
            _ => None,
        }
    }

    fn rows(&self) -> Option<&Arc<DirTable>> {
        match self {
            Slot::Ready { table, .. }
            | Slot::Reloading { table, .. }
            | Slot::Failed {
                table: Some(table), ..
            } => Some(table),
            _ => None,
        }
    }

    /// The version of the rows on offer, or 0 when there are none.
    fn generation(&self) -> u64 {
        match self {
            Slot::Loading { .. } => 0,
            Slot::Reloading { generation, .. }
            | Slot::Ready { generation, .. }
            | Slot::Failed { generation, .. } => *generation,
        }
    }

    fn seen(&self) -> u64 {
        match self {
            Slot::Loading { .. } => 0,
            Slot::Reloading { seen, .. } | Slot::Ready { seen, .. } | Slot::Failed { seen, .. } => {
                *seen
            }
        }
    }

    fn set_seen(&mut self, seen: u64) {
        match self {
            Slot::Loading { .. } => {}
            Slot::Reloading { seen: slot, .. }
            | Slot::Ready { seen: slot, .. }
            | Slot::Failed { seen: slot, .. } => *slot = seen,
        }
    }

    fn owner(&self) -> Option<DirId> {
        match self {
            Slot::Loading { claim, .. } | Slot::Reloading { claim, .. } => Some(*claim),
            _ => None,
        }
    }

    fn tries(&self) -> u32 {
        match self {
            Slot::Loading { tries, .. }
            | Slot::Reloading { tries, .. }
            | Slot::Failed { tries, .. } => *tries,
            Slot::Ready { .. } => 0,
        }
    }

    /// `tries` counts failures including this one, so the first waits `BASE_BACKOFF`.
    fn backoff_for(tries: u32) -> Duration {
        BASE_BACKOFF
            .saturating_mul(1u32 << tries.saturating_sub(1).min(6))
            .min(MAX_BACKOFF)
    }

    /// Whether eviction may take this slot. A table another owner holds is left alone: pulling it
    /// out from under an active reader costs a refetch of the whole directory. A slot with a live
    /// claim is also spared, because its owner expects to write the result back into it.
    fn evictable(&self) -> bool {
        self.owner().is_none()
            && self
                .rows()
                .is_some_and(|table| Arc::strong_count(table) == 1)
    }

    fn bytes(&self) -> u64 {
        self.rows().map_or(0, |table| table.approx_bytes() as u64)
    }
}

/// Cache state behind one lock, so the byte total can never disagree with the slots.
///
/// Victim selection scans the slots rather than maintaining a separate LRU index. What it iterates
/// is the count of cached *directories* - not rows - and it only walks while the cache is over
/// budget. One source of truth beat a ticket map that has to be updated in lockstep from five
/// places, which is the kind of accounting that drifts.
struct Map {
    slots: HashMap<PathKey, Slot>,
    next_seen: u64,
    held_bytes: u64,
}

impl Map {
    fn new() -> Self {
        Self {
            slots: HashMap::new(),
            next_seen: 1,
            held_bytes: 0,
        }
    }

    fn take(&mut self, key: &PathKey) -> Option<Slot> {
        let slot = self.slots.remove(key)?;
        self.held_bytes -= slot.bytes();
        Some(slot)
    }

    fn replace(&mut self, key: PathKey, slot: Slot) {
        let previous = self.slots.get(&key).map(Slot::bytes).unwrap_or(0);
        self.held_bytes = self.held_bytes - previous + slot.bytes();
        self.slots.insert(key, slot);
    }

    fn ticket(&mut self) -> u64 {
        let ticket = self.next_seen;
        self.next_seen += 1;
        ticket
    }
}

/// Reads `key` out of the map, marks it recently used, and hands back its rows if it has any.
fn serve(map: &mut Map, key: &PathKey) -> Option<Snapshot> {
    let ticket = map.ticket();
    let slot = map.slots.get_mut(key)?;
    let snapshot = {
        let (table, freshness) = slot.servable()?;
        Snapshot {
            table: Arc::clone(table),
            generation: slot.generation(),
            freshness,
        }
    };
    slot.set_seen(ticket);
    Some(snapshot)
}

/// A handle to the cache. Cloning is cheap and every clone sees the same tables.
#[derive(Clone)]
pub struct DirIndex {
    inner: Arc<Inner>,
}

struct Inner {
    map: Mutex<Map>,
    next_id: AtomicUsize,
    budget_bytes: AtomicU64,
    /// Reads that actually reached the filesystem. Diagnostics, and how the single-flight tests
    /// show that only one of two racing callers did any work.
    loads: AtomicU64,
}

impl Default for DirIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl DirIndex {
    pub fn new() -> Self {
        Self::with_budget_bytes(Config::default().cache_budget_bytes())
    }

    pub fn with_budget_bytes(budget_bytes: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                map: Mutex::new(Map::new()),
                next_id: AtomicUsize::new(1),
                budget_bytes: AtomicU64::new(budget_bytes.max(1)),
                loads: AtomicU64::new(0),
            }),
        }
    }

    pub fn set_budget_bytes(&self, budget_bytes: u64) {
        self.inner
            .budget_bytes
            .store(budget_bytes.max(1), Ordering::Relaxed);
        self.prune_to_budget();
    }

    pub fn budget_bytes(&self) -> u64 {
        self.inner.budget_bytes.load(Ordering::Relaxed)
    }

    /// Directories with rows available, whether current or stale.
    pub fn cached_count(&self) -> usize {
        self.lock()
            .slots
            .values()
            .filter(|slot| slot.rows().is_some())
            .count()
    }

    pub fn bytes_held(&self) -> u64 {
        self.lock().held_bytes
    }

    pub fn loads(&self) -> u64 {
        self.inner.loads.load(Ordering::Relaxed)
    }

    fn lock(&self) -> MutexGuard<'_, Map> {
        self.inner
            .map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Cached rows for `key`, if any, without triggering a read. Marks the table recently used.
    pub fn peek(&self, key: &PathKey) -> Option<Snapshot> {
        serve(&mut self.lock(), key)
    }

    pub fn state(&self, key: &PathKey) -> State {
        let map = self.lock();
        match map.slots.get(key) {
            None => State::Empty,
            Some(Slot::Loading { .. }) => State::Loading,
            Some(Slot::Ready { stale: false, .. }) => State::Ready,
            Some(Slot::Ready { .. }) | Some(Slot::Reloading { .. }) => State::Stale,
            Some(Slot::Failed { table: None, .. }) => State::Failed,
            Some(Slot::Failed { .. }) => State::Stale,
        }
    }

    /// Cached rows for `key`, reading the directory when necessary. The caller's own thread does
    /// the reading; from a thread that must not block, use [`DirIndex::claim`] instead.
    ///
    /// Stale rows are refreshed rather than handed back as-is, on the grounds that a caller wanting
    /// whatever is on disk would not have asked to `open`.
    pub fn open(&self, key: &PathKey, cancel: &Cancellation) -> Result<Opened, OpenError> {
        match self.peek(key) {
            Some(snapshot) if !snapshot.is_stale() => Ok(Opened::Ready(snapshot)),
            Some(snapshot) => match self.claim(key) {
                // Already being refreshed: those stale rows are the best available answer.
                None => Ok(Opened::Ready(snapshot)),
                Some(claim) => self.finish(claim, Some(snapshot), cancel),
            },
            None => match self.claim(key) {
                Some(claim) => self.finish(claim, None, cancel),
                None => Ok(match self.state(key) {
                    State::Failed => Opened::CoolingDown(self.retry_in(key)),
                    _ => Opened::Loading,
                }),
            },
        }
    }

    /// Runs one claimed read. `backup` is what to hand back if the read is cancelled part way -
    /// stale rows for a refresh, nothing for a cold directory, which is the error case.
    fn finish(
        &self,
        claim: Claim,
        backup: Option<Snapshot>,
        cancel: &Cancellation,
    ) -> Result<Opened, OpenError> {
        match claim.read(cancel) {
            Ok(table) => Ok(Opened::Ready(claim.fill(table))),
            Err(OpenError::Cancelled) => {
                // A cancelled read is not the directory's fault: leave no backoff behind, so the
                // next open tries again immediately.
                claim.abandon();
                match backup {
                    Some(snapshot) => Ok(Opened::Ready(snapshot)),
                    None => Err(OpenError::Cancelled),
                }
            }
            Err(error) => {
                claim.fail();
                Err(error)
            }
        }
    }

    /// How long before a failed directory may be tried again.
    pub fn retry_in(&self, key: &PathKey) -> Duration {
        let map = self.lock();
        match map.slots.get(key) {
            Some(Slot::Failed { not_before, .. }) => {
                not_before.saturating_duration_since(Instant::now())
            }
            _ => Duration::ZERO,
        }
    }

    /// Wins the right to read `key`, or `None` when a read is already running or a failure is still
    /// cooling down. The filesystem work happens after this returns and never under the lock, so
    /// two threads can never both enumerate the same directory.
    pub fn claim(&self, key: &PathKey) -> Option<Claim> {
        let mut map = self.lock();
        let (previous, generation, tries) = match map.slots.get(key) {
            None => (None, 0, 0),
            Some(Slot::Loading { .. } | Slot::Reloading { .. }) => return None,
            Some(Slot::Ready {
                table, generation, ..
            }) => (Some(Arc::clone(table)), *generation, 0),
            Some(Slot::Failed {
                table,
                generation,
                tries,
                not_before,
                ..
            }) => {
                if *not_before > Instant::now() {
                    return None;
                }
                // Rows still on offer mean the last read did useful work; only a directory that
                // never came back keeps escalating its backoff.
                (
                    table.clone(),
                    *generation,
                    if table.is_some() { 0 } else { *tries },
                )
            }
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) as DirId;
        let next = generation + 1;
        let slot = match previous {
            None => Slot::Loading { claim: id, tries },
            Some(table) => Slot::Reloading {
                seen: map.slots[key].seen(),
                claim: id,
                tries,
                table,
                generation,
            },
        };
        map.replace(key.clone(), slot);
        Some(Claim {
            index: self.clone(),
            key: key.clone(),
            id,
            generation: next,
            settled: false,
        })
    }

    /// Marks cached rows as no longer trustworthy. They keep serving until a read replaces them,
    /// which is what stops a burst of filesystem events from blanking the view.
    pub fn invalidate(&self, key: &PathKey) -> bool {
        let mut map = self.lock();
        // Only a fresh table has something to invalidate: a reloading or failed slot already serves
        // stale rows. A read that started before this instant may still come back with rows that
        // predate the event - increment 4 closes that window by pairing events with the generation
        // they apply to.
        match map.slots.get_mut(key) {
            Some(Slot::Ready { stale: false, .. }) => {
                if let Some(Slot::Ready { stale, .. }) = map.slots.get_mut(key) {
                    *stale = true;
                }
                true
            }
            _ => false,
        }
    }

    /// Drops everything held for `key`, including rows too old to display: the directory is gone,
    /// or a lease is being released. Prefer [`DirIndex::invalidate`] when stale rows may stay.
    pub fn forget(&self, key: &PathKey) -> bool {
        self.lock().take(key).is_some()
    }

    /// Frees whole tables, least-recently-touched first, until the cache is back inside its budget.
    fn prune_to_budget(&self) {
        let budget = self.budget_bytes();
        let mut map = self.lock();
        while map.held_bytes > budget {
            let victim = map
                .slots
                .iter()
                .filter(|(_, slot)| slot.evictable())
                .min_by_key(|(_, slot)| slot.seen())
                .map(|(key, _)| key.clone());
            let Some(key) = victim else { break };
            map.take(&key);
        }
    }

    /// Gives up an in-flight claim. Rows that a reload was standing in for go back to cached-and
    /// stale, so an abandoned read cannot cost a caller a directory they were already looking at.
    fn release(&self, key: &PathKey, claim: DirId) {
        let mut map = self.lock();
        let owned = matches!(map.slots.get(key), Some(slot) if slot.owner() == Some(claim));
        if !owned {
            return;
        }
        let restored = match map.slots.get(key).and_then(Slot::rows) {
            Some(table) => Some((
                Arc::clone(table),
                map.slots[key].generation(),
                map.slots[key].seen(),
            )),
            None => None,
        };
        match restored {
            Some((table, generation, seen)) => map.replace(
                key.clone(),
                Slot::Ready {
                    table,
                    generation,
                    seen,
                    stale: true,
                },
            ),
            None => {
                map.take(key);
            }
        }
    }
}

/// Permission to read one directory and install the result. Dropping a claim without settling it
/// returns the directory to the state before the read, so a cancelled or panicking load cannot wedge
/// it forever.
#[must_use = "an unsettled claim leaves the directory marked as loading"]
pub struct Claim {
    index: DirIndex,
    key: PathKey,
    id: DirId,
    generation: u64,
    settled: bool,
}

impl Claim {
    pub fn key(&self) -> &PathKey {
        &self.key
    }

    /// The id the resulting table will carry, so rows can be built before the table exists.
    pub fn id(&self) -> DirId {
        self.id
    }

    /// The generation [`Claim::fill`] will produce, if it succeeds.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// An empty table stamped with this claim's id, for a caller that enumerates on its own.
    pub fn builder(&self) -> DirTable {
        DirTable::new(self.id, self.key.clone(), None)
    }

    /// Reads the directory through `fam-svc-enumerate`. A read that stopped early is an error
    /// rather than a short table: caching one would make a directory look like it lost its tail.
    pub fn read(&self, cancel: &Cancellation) -> Result<DirTable, OpenError> {
        self.index.inner.loads.fetch_add(1, Ordering::Relaxed);
        let mut table = self.builder();
        let report = list_one_level(&self.key.to_path_buf(), cancel, &mut |raw| {
            let item = Item {
                name_off: 0,
                name_units: 0,
                kind: raw.kind.index(),
                flags: raw.flags.0,
                size: raw.size,
                mtime_ms: raw.mtime_ms,
                attrs: raw.attrs,
                dir: 0,
            };
            table.push(raw.name, item);
            true
        })?;
        if !report.complete() {
            return Err(OpenError::Cancelled);
        }
        Ok(table)
    }

    /// Installs a table and returns it as a snapshot, replacing whatever the slot held. Success
    /// clears the failure history, because a slot that reads fine is not on a backoff curve.
    pub fn fill(mut self, table: DirTable) -> Snapshot {
        let table = Arc::new(table);
        let snapshot = Snapshot {
            table: Arc::clone(&table),
            generation: self.generation,
            freshness: Freshness::Current,
        };
        {
            let mut map = self.index.lock();
            // A reload reuses the slot's LRU position; a first read takes a fresh ticket.
            let seen = match map.slots.get(&self.key) {
                Some(slot) if slot.rows().is_some() => slot.seen(),
                _ => map.ticket(),
            };
            map.replace(
                self.key.clone(),
                Slot::Ready {
                    table,
                    generation: self.generation,
                    seen,
                    stale: false,
                },
            );
        }
        self.settled = true;
        self.index.prune_to_budget();
        snapshot
    }

    /// Records the failure and opens the backoff window. Rows from an earlier good read stay
    /// servable, marked stale.
    pub fn fail(mut self) {
        {
            let mut map = self.index.lock();
            let owned =
                matches!(map.slots.get(&self.key), Some(slot) if slot.owner() == Some(self.id));
            if !owned {
                return;
            }
            let slot = &map.slots[&self.key];
            let (table, generation, seen, tries) = (
                slot.rows().cloned(),
                slot.generation(),
                slot.seen(),
                slot.tries() + 1,
            );
            map.replace(
                self.key.clone(),
                Slot::Failed {
                    table,
                    generation,
                    seen,
                    tries,
                    not_before: Instant::now() + Slot::backoff_for(tries),
                },
            );
        }
        self.settled = true;
    }

    /// Gives up without recording a failure: the next caller may claim immediately.
    pub fn abandon(mut self) {
        let (key, id) = (self.key.clone(), self.id);
        self.settled = true;
        self.index.release(&key, id);
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let (key, id) = (self.key.clone(), self.id);
        self.index.release(&key, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fam_core::{ItemFlags, Kind};

    fn key_of(path: &std::path::Path) -> PathKey {
        PathKey::of(path)
    }

    fn fill_dir(dir: &tempfile::TempDir, count: usize) {
        for index in 0..count {
            std::fs::write(dir.path().join(format!("payload-{index:03}.bin")), "x").unwrap();
        }
    }

    fn table_for(key: &PathKey, names: &[&str]) -> DirTable {
        let mut table = DirTable::new(7, key.clone(), None);
        for name in names {
            table.push(
                &name.encode_utf16().collect::<Vec<u16>>(),
                Item {
                    name_off: 0,
                    name_units: 0,
                    kind: Kind::Other.index(),
                    flags: ItemFlags::default().0,
                    size: 1,
                    mtime_ms: 0,
                    attrs: 0,
                    dir: 0,
                },
            );
        }
        table
    }

    fn rows(opened: Opened) -> Snapshot {
        match opened {
            Opened::Ready(snapshot) => snapshot,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    fn open(index: &DirIndex, key: &PathKey) -> Snapshot {
        rows(index.open(key, &Cancellation::new()).unwrap())
    }

    #[test]
    fn opening_twice_serves_the_same_table() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 3);
        let index = DirIndex::new();
        let key = key_of(dir.path());

        let first = open(&index, &key);
        let second = open(&index, &key);

        assert!(Arc::ptr_eq(&first.table, &second.table));
        assert_eq!(index.cached_count(), 1);
        assert_eq!(index.loads(), 1, "the second open was served from cache");
    }

    #[test]
    fn separator_and_case_variants_share_one_slot() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let cancel = Cancellation::new();

        index.open(&key_of(dir.path()), &cancel).unwrap();
        let upper = dir.path().to_string_lossy().to_uppercase();
        index
            .open(&key_of(std::path::Path::new(&upper)), &cancel)
            .unwrap();

        assert_eq!(index.cached_count(), 1);
        assert_eq!(index.loads(), 1);
    }

    #[test]
    fn rows_carry_kind_and_size_from_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), "12345").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let index = DirIndex::new();

        let snapshot = open(&index, &key_of(dir.path()));

        assert_eq!(snapshot.table.len(), 2);
        let kinds: Vec<Kind> = (0..snapshot.table.len() as u32)
            .map(|i| Kind::from_index(snapshot.table.item(i).unwrap().kind).unwrap())
            .collect();
        assert!(kinds.contains(&Kind::Image));
        assert!(kinds.contains(&Kind::Directory));
        assert_eq!(snapshot.freshness, Freshness::Current);
        assert!(!snapshot.is_stale());
    }

    #[test]
    fn exactly_one_claim_wins_a_cold_directory() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());

        let first = index.claim(&key).expect("a cold directory is claimable");
        assert!(
            index.claim(&key).is_none(),
            "the directory is already being read"
        );
        assert_eq!(index.state(&key), State::Loading);
        assert!(index.peek(&key).is_none(), "no rows exist yet");

        let snapshot = first.fill(table_for(&key, &["a.txt"]));
        assert_eq!(snapshot.generation, 1);
        assert_eq!(index.state(&key), State::Ready);
        assert_eq!(index.loads(), 0, "fill() installs rows without reading");
    }

    #[test]
    fn an_abandoned_claim_frees_a_cold_directory() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());

        drop(index.claim(&key).expect("claim"));

        assert_eq!(index.state(&key), State::Empty);
        assert!(index.claim(&key).is_some(), "the next caller may try again");
    }

    #[test]
    fn abandoning_a_reload_keeps_the_old_rows() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        let before = open(&index, &key);

        index.claim(&key).unwrap().abandon();

        let after = index.peek(&key).expect("rows survive an abandoned reload");
        assert!(Arc::ptr_eq(&after.table, &before.table));
        assert_eq!(after.freshness, Freshness::Stale);
        assert_eq!(after.generation, before.generation);
        assert_eq!(index.retry_in(&key), Duration::ZERO, "no backoff");
        assert!(index.claim(&key).is_some());
    }

    #[test]
    fn a_cancelled_read_caches_nothing_and_leaves_no_backoff() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        let cancel = Cancellation::new();
        cancel.cancel();

        let error = index.open(&key, &cancel).unwrap_err();
        assert!(matches!(error, OpenError::Cancelled));
        assert_eq!(index.cached_count(), 0);
        assert_eq!(index.state(&key), State::Empty);
        assert!(index.peek(&key).is_none());
        assert_eq!(index.retry_in(&key), Duration::ZERO);
        assert!(index.claim(&key).is_some());
    }

    #[test]
    fn a_refresh_replaces_the_rows_and_advances_the_generation() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        let before = open(&index, &key);

        let claim = index
            .claim(&key)
            .expect("a cached directory can be refreshed");
        assert_eq!(index.state(&key), State::Stale);
        let during = index
            .peek(&key)
            .expect("rows stay available during the reload");
        assert_eq!(during.freshness, Freshness::Stale);
        assert!(Arc::ptr_eq(&during.table, &before.table));

        let after = claim.fill(table_for(&key, &["a.bin", "b.bin", "c.bin"]));
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.freshness, Freshness::Current);
        assert_eq!(after.table.len(), 3);
        assert_eq!(index.bytes_held(), after.table.approx_bytes() as u64);
    }

    #[test]
    fn open_refreshes_stale_rows_instead_of_handing_them_back() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        let before = open(&index, &key);
        assert!(index.invalidate(&key));

        let after = open(&index, &key);

        assert_eq!(index.loads(), 2, "the stale table was re-read");
        assert_eq!(after.freshness, Freshness::Current);
        assert_eq!(after.generation, before.generation + 1);
    }

    #[test]
    fn invalidation_marks_stale_without_dropping_rows() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        open(&index, &key);

        assert!(index.invalidate(&key));
        assert_eq!(index.state(&key), State::Stale);
        assert!(index.peek(&key).is_some(), "a stale table still serves");
        assert!(
            !index.invalidate(&key),
            "marking an already-stale table changes nothing"
        );

        assert!(index.forget(&key));
        assert_eq!(index.state(&key), State::Empty);
        assert!(index.peek(&key).is_none());
        assert!(!index.forget(&key));
    }

    #[test]
    fn a_failed_read_cools_down_then_lets_one_retry() {
        let index = DirIndex::new();
        let key = key_of(std::path::Path::new("Q:/definitely/not/here"));

        let error = index.open(&key, &Cancellation::new()).unwrap_err();
        assert!(matches!(error, OpenError::Enum(_)), "{error:?}");
        assert_eq!(index.state(&key), State::Failed);
        assert_eq!(index.loads(), 1);

        let cooled = index.open(&key, &Cancellation::new()).unwrap();
        assert!(
            matches!(&cooled, Opened::CoolingDown(wait) if !wait.is_zero()),
            "the second attempt must not reach the disk: {cooled:?}"
        );
        assert_eq!(index.loads(), 1);

        std::thread::sleep(Duration::from_millis(230));
        assert!(index.claim(&key).is_some(), "the backoff window elapsed");
        assert_eq!(index.loads(), 1, "claiming alone does not read");
    }

    #[test]
    fn a_failed_reload_keeps_rows_at_the_same_generation() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let index = DirIndex::new();
        let key = key_of(dir.path());
        let before = open(&index, &key);

        index.claim(&key).unwrap().fail();

        assert_eq!(index.state(&key), State::Stale);
        let after = index.peek(&key).expect("old rows survive a failed reload");
        assert!(Arc::ptr_eq(&after.table, &before.table));
        assert_eq!(
            after.generation, before.generation,
            "the rows did not change, so neither does their version"
        );
        assert_eq!(after.freshness, Freshness::Stale);
        assert_eq!(index.bytes_held(), after.table.approx_bytes() as u64);
    }

    #[test]
    fn repeated_failures_escalate_the_backoff() {
        let index = DirIndex::new();
        let key = key_of(std::path::Path::new("Q:/definitely/not/here"));

        let mut waits = Vec::new();
        for round in 0..3 {
            if round > 0 {
                std::thread::sleep(index.retry_in(&key) + Duration::from_millis(5));
            }
            index.claim(&key).expect("retry allowed").fail();
            waits.push(index.retry_in(&key));
        }
        assert!(
            waits[0] > BASE_BACKOFF / 2 && waits[0] <= BASE_BACKOFF,
            "the first failure waits about the base window, got {:?}",
            waits[0]
        );
        assert!(
            waits[0] < waits[1] && waits[1] < waits[2],
            "backoff must escalate: {waits:?}"
        );
        assert!(
            waits.iter().all(|wait| wait <= &MAX_BACKOFF),
            "the window is capped: {waits:?}"
        );
    }

    #[test]
    fn a_successful_read_clears_the_failure_history() {
        let index = DirIndex::new();
        let missing = key_of(std::path::Path::new("Q:/definitely/not/here"));
        index.claim(&missing).unwrap().fail();
        std::thread::sleep(index.retry_in(&missing) + Duration::from_millis(5));
        index.claim(&missing).unwrap().fail();
        let escalated = index.retry_in(&missing);

        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 2);
        let key = key_of(dir.path());
        open(&index, &key);
        index.claim(&key).unwrap().fail();

        assert!(
            index.retry_in(&key) < escalated,
            "a directory that read fine once restarts its backoff at the base window"
        );
        // During the window the stale rows keep serving and nothing re-reads; after it, a
        // successful open is what returns the directory to current.
        std::thread::sleep(index.retry_in(&key) + Duration::from_millis(5));
        open(&index, &key);
        assert_eq!(index.state(&key), State::Ready);
    }

    #[test]
    fn the_least_recently_used_table_is_the_one_evicted() {
        let older = tempfile::tempdir().unwrap();
        let newer = tempfile::tempdir().unwrap();
        fill_dir(&older, 12);
        fill_dir(&newer, 12);
        let (k_older, k_newer) = (key_of(older.path()), key_of(newer.path()));
        let index = DirIndex::new();

        let first = open(&index, &k_older);
        let second = open(&index, &k_newer);
        let one_table = index.bytes_held() / 2;
        drop(first);
        drop(second);

        // Touching the older directory makes the other one the eviction candidate.
        index.peek(&k_older).expect("still cached");
        index.set_budget_bytes(one_table);

        assert_eq!(
            index.state(&k_newer),
            State::Empty,
            "the untouched table should have been freed"
        );
        assert_eq!(index.state(&k_older), State::Ready);
        assert!(index.bytes_held() <= one_table);
    }

    #[test]
    fn a_table_someone_holds_is_not_evicted() {
        let held_dir = tempfile::tempdir().unwrap();
        let spare_dir = tempfile::tempdir().unwrap();
        fill_dir(&held_dir, 8);
        fill_dir(&spare_dir, 8);
        let (k_held, k_spare) = (key_of(held_dir.path()), key_of(spare_dir.path()));
        let index = DirIndex::with_budget_bytes(1);

        let lease = open(&index, &k_held);
        assert_eq!(
            index.state(&k_held),
            State::Ready,
            "the prune its own open triggered skipped the leased table"
        );
        let other = open(&index, &k_spare);
        assert!(
            index.bytes_held() > 1,
            "both are leased, so both stay despite a 1-byte budget"
        );

        drop(other);
        index.prune_to_budget();
        assert_eq!(index.state(&k_spare), State::Empty, "released rows go");
        assert_eq!(index.state(&k_held), State::Ready, "leased rows stay");

        drop(lease);
        index.prune_to_budget();
        assert_eq!(index.cached_count(), 0, "once released it can go too");
    }

    #[test]
    fn a_table_being_read_is_not_evicted() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 4);
        let index = DirIndex::with_budget_bytes(1);
        let key = key_of(dir.path());
        open(&index, &key);

        let claim = index.claim(&key).unwrap();
        index.prune_to_budget();
        assert_eq!(
            index.state(&key),
            State::Stale,
            "the in-flight reload owns this slot and will write back into it"
        );
        assert!(index.peek(&key).is_some(), "its rows still serve");

        claim.fill(table_for(&key, &["new.txt"]));
        assert_eq!(index.state(&key), State::Ready);
    }

    #[test]
    fn held_bytes_tracks_the_tables_actually_cached() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 5);
        let index = DirIndex::new();
        let key = key_of(dir.path());

        assert_eq!(index.bytes_held(), 0);
        let snapshot = open(&index, &key);
        let big = snapshot.table.approx_bytes() as u64;
        assert_eq!(index.bytes_held(), big);
        drop(snapshot);

        index
            .claim(&key)
            .unwrap()
            .fill(table_for(&key, &["only.txt"]));
        let after = index.peek(&key).expect("replaced");
        assert!(
            big > after.table.approx_bytes() as u64,
            "5 rows must cost more than 1"
        );
        assert_eq!(index.bytes_held(), after.table.approx_bytes() as u64);
    }

    #[test]
    fn two_callers_never_read_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        fill_dir(&dir, 30);
        let index = DirIndex::new();
        let key = key_of(dir.path());

        let claim = index.claim(&key).expect("cold directory");
        let second = index.clone();
        let key2 = key.clone();
        let opened = std::thread::spawn(move || {
            second
                .open(&key2, &Cancellation::new())
                .expect("no read attempted")
        })
        .join()
        .expect("thread");

        assert!(
            matches!(opened, Opened::Loading),
            "the second caller must not have read anything: {opened:?}"
        );
        assert_eq!(index.loads(), 0, "neither caller has read yet");

        let table = claim.read(&Cancellation::new()).unwrap();
        let snapshot = claim.fill(table);
        assert_eq!(index.loads(), 1, "exactly one read happened");
        assert_eq!(snapshot.table.len(), 30);
    }
}
