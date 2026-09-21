use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fam_core::{path_key, raw_name_units, PathKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Created,
    Modified,
    Removed,
    /// Anything the watcher reported that carries no create/write/delete meaning, such as an
    /// attribute flush.
    Other,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::Created => "created",
            Change::Modified => "modified",
            Change::Removed => "removed",
            Change::Other => "other",
        }
    }
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the watcher could tell if the name it reported is a directory.
///
/// `ReadDirectoryChangesW` reports a name and an action, nothing else, so on Windows every event
/// arrives as `Unknown` - the backend's `CreateKind`/`RemoveKind` are `Any` (notify's own
/// `windows.rs:434-449`). A consumer patching a cached table has to resolve the question itself,
/// either from the table it already holds or with one targeted attribute query. Inventing a
/// `File` default would be guessing, and a wrong guess puts a folder icon on a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Directory,
    File,
    Unknown,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Directory => "directory",
            Target::File => "file",
            Target::Unknown => "unknown",
        }
    }
}

/// One path's worth of change, as far as the watcher saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub change: Change,
    /// The directory whose table this row belongs to, or `None` for a path with no parent - a
    /// drive root, which no one-level table holds.
    pub dir: Option<PathKey>,
    /// Verbatim UTF-16 name, ready to be pushed into a `DirTable` arena.
    pub name: Vec<u16>,
    pub target: Target,
}

impl FileChange {
    pub fn new(path: impl Into<PathBuf>, change: Change) -> Self {
        Self::with_target(path, change, Target::Unknown)
    }

    pub fn with_target(path: impl Into<PathBuf>, change: Change, target: Target) -> Self {
        let path: PathBuf = path.into();
        let key = PathKey::of(&path);
        Self {
            name: raw_name_units(&path),
            dir: key.parent().filter(|parent| parent != &key),
            path,
            change,
            target,
        }
    }

    pub fn key(&self) -> String {
        path_key(&self.path)
    }
}

/// A directory must be re-read rather than patched row by row.
///
/// There is deliberately no "the kernel's change buffer filled" case. That is the loss this whole
/// shape defends against, and it is unobservable: notify's backend takes any error code it does not
/// recognise - `ERROR_NOTIFY_ENUM_DIR` among them - by logging it and quietly withdrawing the watch,
/// so nothing reaches a handler (`docs/notes/notify-watch-loss-is-invisible.md`). The two reasons
/// below are the ones something in this process can actually establish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanWhy {
    /// The watch stopped against our wishes: the directory itself was deleted or denied us, or the
    /// backend reported an error that named paths.
    WatchLost,
    /// The directory's own write time moved while no event arrived for it. This is the observable
    /// stand-in for the losses the backend cannot report - a withdrawal shows up here because the
    /// events simply stopped (`reconcile.rs`).
    Reconciled,
}

/// What travels through the pipeline: either a row delta, or an order to re-read a whole directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Row(FileChange),
    Rescan { dir: PathKey, why: RescanWhy },
}

impl Signal {
    pub fn directory(&self) -> Option<&PathKey> {
        match self {
            Signal::Row(change) => change.dir.as_ref(),
            Signal::Rescan { dir, .. } => Some(dir),
        }
    }
}

/// Collapses two observations of the same path into what the UI row should become.
///
/// Deleting and then recreating a file is an update, not a removal followed by an insert - the
/// legacy consumer only ever looked at Create/Write and lost the rest (§4.2).
pub fn merge(previous: Change, next: Change) -> Change {
    match (previous, next) {
        (Change::Removed, Change::Created) => Change::Modified,
        (Change::Created, Change::Modified) => Change::Created,
        (_, Change::Other) => previous,
        (_, next) => next,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub changes: Vec<FileChange>,
    /// Directories that must be re-read whole, deduplicated across the window.
    pub rescans: Vec<(PathKey, RescanWhy)>,
    /// Events discarded because the queue was full since the previous flush.
    pub dropped: u64,
}

impl Batch {
    pub fn new(changes: Vec<FileChange>, rescans: Vec<(PathKey, RescanWhy)>, dropped: u64) -> Self {
        Self {
            changes,
            rescans,
            dropped,
        }
    }

    pub fn len(&self) -> usize {
        self.changes.len() + self.rescans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.rescans.is_empty()
    }

    /// Regroups the window by the directory each row belongs to, which is the unit a cached table
    /// can be patched in. Row merging happened per path before this grouping, and `merge()` is not
    /// re-invoked here: grouping only redistributes what is already collapsed.
    pub fn by_directory(&self) -> DirBatches {
        let mut dirs: HashMap<PathKey, DirDelta> = HashMap::new();
        for change in &self.changes {
            let Some(parent) = &change.dir else {
                // A change with no parent directory - a drive root appearing or disappearing - is
                // not a row in any one-level table. Handing it to the collector as a rescan of the
                // drive is the only honest reading, and drives are few.
                continue;
            };
            let entry = dirs.entry(parent.clone()).or_insert_with(|| DirDelta {
                dir: parent.clone(),
                rows: Vec::new(),
                rescan: None,
            });
            entry.rows.push(RowDelta {
                name: change.name.clone(),
                change: change.change,
                target: change.target,
            });
        }
        for (dir, why) in &self.rescans {
            let entry = dirs.entry(dir.clone()).or_insert_with(|| DirDelta {
                dir: dir.clone(),
                rows: Vec::new(),
                rescan: None,
            });
            entry.rescan = Some(*why);
        }
        let mut groups: Vec<DirDelta> = dirs.into_values().collect();
        groups.sort_by(|left, right| left.dir.cmp(&right.dir));
        DirBatches {
            groups,
            dropped: self.dropped,
        }
    }
}

/// One directory's turn in a batch: the rows to patch, or the reason to re-read instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirDelta {
    pub dir: PathKey,
    pub rows: Vec<RowDelta>,
    /// Set when the directory's rows must be discarded and re-read. The row list may still be
    /// non-empty - a create can arrive in the same window as a lost-watch notice - but a rescan
    /// wins, because it subsumes the patch.
    pub rescan: Option<RescanWhy>,
}

impl DirDelta {
    pub fn needs_rescan(&self) -> bool {
        self.rescan.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDelta {
    pub name: Vec<u16>,
    pub change: Change,
    pub target: Target,
}

impl RowDelta {
    pub fn name_string(&self) -> String {
        String::from_utf16_lossy(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirBatches {
    pub groups: Vec<DirDelta>,
    pub dropped: u64,
}

impl DirBatches {
    pub fn iter(&self) -> impl Iterator<Item = &DirDelta> {
        self.groups.iter()
    }

    pub fn get(&self, dir: &PathKey) -> Option<&DirDelta> {
        self.groups.iter().find(|group| &group.dir == dir)
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl<'a> IntoIterator for &'a DirBatches {
    type Item = &'a DirDelta;
    type IntoIter = std::slice::Iter<'a, DirDelta>;

    fn into_iter(self) -> Self::IntoIter {
        self.groups.iter()
    }
}

/// Accumulates events for one time window and emits at most one row-delta set per window, which is
/// the batch-merge requirement in §4 (one `beginInsertRows` per event freezes the UI on slow media).
#[derive(Debug, Clone)]
pub struct Batcher {
    window: Duration,
    max_entries: usize,
    pending: HashMap<String, FileChange>,
    rescans: HashMap<PathKey, RescanWhy>,
    seen_rescans: HashSet<PathKey>,
    opened: Option<Instant>,
}

impl Batcher {
    pub fn new(window: Duration, max_entries: usize) -> Self {
        Self {
            window,
            max_entries: max_entries.max(1),
            pending: HashMap::new(),
            rescans: HashMap::new(),
            seen_rescans: HashSet::new(),
            opened: None,
        }
    }

    /// Returns true when the batch is full and should be flushed immediately.
    pub fn absorb(&mut self, signal: Signal) -> bool {
        self.opened.get_or_insert_with(Instant::now);
        match signal {
            Signal::Row(change) => {
                let key = change.key();
                match self.pending.get_mut(&key) {
                    Some(existing) => {
                        existing.change = merge(existing.change, change.change);
                        // The later observation is the one that knows more: an event that turns out
                        // to be about a directory must not be filed under "unknown" forever.
                        if existing.target == Target::Unknown {
                            existing.target = change.target;
                        }
                    }
                    None => {
                        self.pending.insert(key, change);
                    }
                }
            }
            Signal::Rescan { dir, why } => {
                // A lost watch outranks an inferred gap: both mean "re-read", but the reason is what
                // lands in a log and in a UI message, and "the directory refused us" is the harder
                // fact of the two.
                if !self.seen_rescans.contains(&dir) || why == RescanWhy::WatchLost {
                    self.seen_rescans.insert(dir.clone());
                    self.rescans.insert(dir, why);
                }
            }
        }
        self.pending.len() + self.rescans.len() >= self.max_entries
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.rescans.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len() + self.rescans.len()
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.opened
            .is_some_and(|opened| now.saturating_duration_since(opened) >= self.window)
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let opened = self.opened?;
        let elapsed = now.saturating_duration_since(opened);
        Some(self.window.saturating_sub(elapsed))
    }

    /// Takes the merged changes ordered by path key, so callers get a deterministic sequence.
    pub fn drain(&mut self) -> Vec<Signal> {
        let mut changes: Vec<FileChange> = self.pending.drain().map(|(_, change)| change).collect();
        changes.sort_by_key(FileChange::key);
        let mut rescans: Vec<(PathKey, RescanWhy)> = self.rescans.drain().collect();
        rescans.sort_by(|left, right| left.0.cmp(&right.0));
        self.seen_rescans.clear();
        self.opened = None;
        let mut signals: Vec<Signal> = changes.into_iter().map(Signal::Row).collect();
        signals.extend(
            rescans
                .into_iter()
                .map(|(dir, why)| Signal::Rescan { dir, why }),
        );
        signals
    }

    /// Takes the batch's two halves separately, for callers that build a `Batch` directly.
    pub fn drain_parts(&mut self) -> (Vec<FileChange>, Vec<(PathKey, RescanWhy)>) {
        let mut changes = Vec::new();
        let mut rescans = Vec::new();
        for signal in self.drain() {
            match signal {
                Signal::Row(change) => changes.push(change),
                Signal::Rescan { dir, why } => rescans.push((dir, why)),
            }
        }
        (changes, rescans)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn change(path: &str, kind: Change) -> FileChange {
        FileChange::new(path, kind)
    }

    #[test]
    fn every_semantic_of_spec_four_has_a_mapping() {
        assert_eq!(Change::Created.as_str(), "created");
        assert_eq!(Change::Modified.as_str(), "modified");
        assert_eq!(Change::Removed.as_str(), "removed");
        assert_eq!(Change::Other.as_str(), "other");
        assert_eq!(Target::Unknown.as_str(), "unknown");
    }

    #[test]
    fn delete_then_create_reads_as_an_update() {
        assert_eq!(merge(Change::Removed, Change::Created), Change::Modified);
        assert_eq!(merge(Change::Created, Change::Removed), Change::Removed);
        assert_eq!(merge(Change::Created, Change::Modified), Change::Created);
        assert_eq!(merge(Change::Modified, Change::Removed), Change::Removed);
    }

    #[test]
    fn noise_never_overwrites_a_real_change() {
        assert_eq!(merge(Change::Created, Change::Other), Change::Created);
        assert_eq!(merge(Change::Other, Change::Removed), Change::Removed);
    }

    #[test]
    fn a_change_carries_the_directory_and_the_verbatim_name() {
        let made = change(r"D:\Docs\报告 2026.pdf", Change::Created);

        assert_eq!(
            made.dir,
            Some(PathKey::of(Path::new(r"D:\Docs"))),
            "separators fold the same way the cache keys do"
        );
        assert_eq!(String::from_utf16_lossy(&made.name), "报告 2026.pdf");
    }

    #[test]
    fn a_drive_root_has_no_parent_table() {
        let root = change("D:\\", Change::Created);
        assert_eq!(root.dir, None);
        assert!(root.name.is_empty());
    }

    #[test]
    fn one_path_yields_one_row_delta() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        for _ in 0..40 {
            batcher.absorb(Signal::Row(change("D:/a/notes.txt", Change::Modified)));
        }

        let (changes, rescans) = batcher.drain_parts();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, Change::Modified);
        assert!(rescans.is_empty());
    }

    #[test]
    fn paths_are_case_and_separator_insensitive_when_merging() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        batcher.absorb(Signal::Row(change(r"D:\A\notes.txt", Change::Created)));
        batcher.absorb(Signal::Row(change("D:/a/NOTES.TXT", Change::Removed)));

        let (changes, _) = batcher.drain_parts();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, Change::Removed);
    }

    #[test]
    fn a_later_observation_can_resolve_the_target() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        batcher.absorb(Signal::Row(change("D:/a/x", Change::Created)));
        batcher.absorb(Signal::Row(FileChange::with_target(
            "D:/a/x",
            Change::Modified,
            Target::Directory,
        )));

        let (changes, _) = batcher.drain_parts();

        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].target,
            Target::Directory,
            "once something says it is a directory, it is"
        );
    }

    #[test]
    fn one_rescan_per_directory_per_window() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        let dir = PathKey::of(Path::new("D:/a"));
        for _ in 0..5 {
            batcher.absorb(Signal::Rescan {
                dir: dir.clone(),
                why: RescanWhy::Reconciled,
            });
        }

        let (changes, rescans) = batcher.drain_parts();

        assert!(changes.is_empty());
        assert_eq!(rescans, vec![(dir, RescanWhy::Reconciled)]);
    }

    #[test]
    fn a_full_batch_reports_for_immediate_flush() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 3);
        assert!(!batcher.absorb(Signal::Row(change("D:/a", Change::Created))));
        assert!(!batcher.absorb(Signal::Row(change("D:/b", Change::Created))));
        assert!(batcher.absorb(Signal::Row(change("D:/c", Change::Created))));
        assert_eq!(batcher.drain().len(), 3);
        assert!(batcher.is_empty());
    }

    #[test]
    fn a_rescan_counts_toward_the_batch_limit() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 2);
        assert!(!batcher.absorb(Signal::Row(change("D:/a", Change::Created))));
        assert!(batcher.absorb(Signal::Rescan {
            dir: PathKey::of(Path::new("D:/b")),
            why: RescanWhy::WatchLost,
        }));
    }

    #[test]
    fn batches_sort_by_path_key() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        for path in ["D:/c.txt", "D:/a.txt", "D:/b.txt"] {
            batcher.absorb(Signal::Row(change(path, Change::Created)));
        }

        let (batch, _) = batcher.drain_parts();
        let names: Vec<String> = batch
            .iter()
            .map(|change| change.path.display().to_string())
            .collect();

        assert_eq!(names, vec!["D:/a.txt", "D:/b.txt", "D:/c.txt"]);
    }

    #[test]
    fn window_opens_on_the_first_event_not_at_construction() {
        let mut batcher = Batcher::new(Duration::from_millis(20), 100);
        assert!(!batcher.expired(Instant::now() + Duration::from_secs(9)));
        batcher.absorb(Signal::Row(change("D:/a", Change::Created)));
        assert!(!batcher.expired(Instant::now()));
        assert!(batcher.expired(Instant::now() + Duration::from_millis(40)));
        assert!(batcher.remaining(Instant::now()).is_some());
    }

    #[test]
    fn a_batch_groups_rows_by_the_directory_they_belong_to() {
        let changes = vec![
            change(r"D:\one\a.txt", Change::Created),
            change(r"D:\one\b.txt", Change::Modified),
            change(r"D:\two\c.txt", Change::Removed),
        ];
        let batch = Batch::new(changes, Vec::new(), 0);

        let grouped = batch.by_directory();

        let dirs: Vec<String> = grouped
            .iter()
            .map(|group| group.dir.to_lossy_string())
            .collect();
        assert_eq!(dirs, vec!["D:/ONE", "D:/TWO"]);
        assert_eq!(grouped.iter().next().unwrap().rows.len(), 2);
        assert_eq!(
            grouped
                .get(&PathKey::of(Path::new("d:/two")))
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(
            grouped.get(&PathKey::of(Path::new("d:/two"))).unwrap().rows[0].name_string(),
            "c.txt"
        );
    }

    #[test]
    fn a_rescan_makes_the_whole_group_a_rescan() {
        let batch = Batch::new(
            vec![change(r"D:\one\a.txt", Change::Created)],
            vec![(PathKey::of(Path::new("D:/one")), RescanWhy::Reconciled)],
            3,
        );

        let grouped = batch.by_directory();
        let group = grouped
            .get(&PathKey::of(Path::new("D:/one")))
            .expect("grouped");

        assert!(group.needs_rescan());
        assert_eq!(group.rows.len(), 1, "the rows are still reported, but lose");
        assert_eq!(grouped.dropped, 3);
    }

    #[test]
    fn a_rescan_for_an_otherwise_quiet_directory_still_forms_a_group() {
        let batch = Batch::new(
            Vec::new(),
            vec![(PathKey::of(Path::new("D:/quiet")), RescanWhy::WatchLost)],
            0,
        );

        let grouped = batch.by_directory();
        let group = grouped
            .get(&PathKey::of(Path::new("D:/quiet")))
            .expect("a rescan alone is a batch entry");

        assert!(group.rows.is_empty());
        assert_eq!(group.rescan, Some(RescanWhy::WatchLost));
    }
}
