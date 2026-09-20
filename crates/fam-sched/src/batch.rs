use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fam_core::path_key;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub change: Change,
}

impl FileChange {
    pub fn new(path: impl Into<PathBuf>, change: Change) -> Self {
        Self {
            path: path.into(),
            change,
        }
    }

    pub fn key(&self) -> String {
        path_key(&self.path)
    }
}

/// Collapses two observations of the same path into what the UI row should become.
///
/// Deleting and then recreating a file is an update, not a removal followed by an insert — the
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
    /// Events discarded because the queue was full since the previous flush.
    pub dropped: u64,
}

impl Batch {
    pub fn new(changes: Vec<FileChange>, dropped: u64) -> Self {
        Self { changes, dropped }
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Accumulates events for one time window and emits at most one row-delta set per window, which is
/// the batch-merge requirement in §4 (one `beginInsertRows` per event freezes the UI on slow media).
#[derive(Debug, Clone)]
pub struct Batcher {
    window: Duration,
    max_entries: usize,
    pending: HashMap<String, FileChange>,
    opened: Option<Instant>,
}

impl Batcher {
    pub fn new(window: Duration, max_entries: usize) -> Self {
        Self {
            window,
            max_entries: max_entries.max(1),
            pending: HashMap::new(),
            opened: None,
        }
    }

    /// Returns true when the batch is full and should be flushed immediately.
    pub fn absorb(&mut self, change: FileChange) -> bool {
        let key = change.key();
        self.opened.get_or_insert_with(Instant::now);
        match self.pending.get_mut(&key) {
            Some(existing) => existing.change = merge(existing.change, change.change),
            None => {
                self.pending.insert(key, change);
            }
        }
        self.pending.len() >= self.max_entries
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
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
    pub fn drain(&mut self) -> Vec<FileChange> {
        let mut changes: Vec<FileChange> = self.pending.drain().map(|(_, change)| change).collect();
        changes.sort_by_key(|change| change.key());
        self.opened = None;
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_semantic_of_spec_four_has_a_mapping() {
        assert_eq!(Change::Created.as_str(), "created");
        assert_eq!(Change::Modified.as_str(), "modified");
        assert_eq!(Change::Removed.as_str(), "removed");
        assert_eq!(Change::Other.as_str(), "other");
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
    fn one_path_yields_one_row_delta() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        for _ in 0..40 {
            batcher.absorb(FileChange::new("D:/a/notes.txt", Change::Modified));
        }

        let changes = batcher.drain();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, Change::Modified);
    }

    #[test]
    fn paths_are_case_and_separator_insensitive_when_merging() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        batcher.absorb(FileChange::new(r"D:\A\notes.txt", Change::Created));
        batcher.absorb(FileChange::new("D:/a/NOTES.TXT", Change::Removed));

        let changes = batcher.drain();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, Change::Removed);
    }

    #[test]
    fn a_full_batch_reports_for_immediate_flush() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 3);
        assert!(!batcher.absorb(FileChange::new("D:/a", Change::Created)));
        assert!(!batcher.absorb(FileChange::new("D:/b", Change::Created)));
        assert!(batcher.absorb(FileChange::new("D:/c", Change::Created)));
        assert_eq!(batcher.drain().len(), 3);
        assert!(batcher.is_empty());
    }

    #[test]
    fn batches_sort_by_path_key() {
        let mut batcher = Batcher::new(Duration::from_secs(5), 100);
        for path in ["D:/c.txt", "D:/a.txt", "D:/b.txt"] {
            batcher.absorb(FileChange::new(path, Change::Created));
        }

        let batch = batcher.drain();
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
        batcher.absorb(FileChange::new("D:/a", Change::Created));
        assert!(!batcher.expired(Instant::now()));
        assert!(batcher.expired(Instant::now() + Duration::from_millis(40)));
        assert!(batcher.remaining(Instant::now()).is_some());
    }
}
