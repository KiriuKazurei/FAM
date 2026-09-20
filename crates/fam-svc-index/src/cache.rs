use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use fam_core::{Cancellation, DirId, Item, PathKey};
use fam_svc_enumerate::{list_one_level, EnumError};

use crate::dir_table::DirTable;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error(transparent)]
    Enum(#[from] EnumError),
    #[error("enumeration stopped before the directory was fully read")]
    Cancelled,
}

/// Cache of enumerated directories, keyed by the exact path identity.
///
/// `open` currently lists synchronously and stores the table; the `Loading`/`Stale`/`Failed`
/// states, single-flight election and LRU eviction described in the rebuild plan are increment 3.
/// What is already true here and must stay true: a table is served whole, eviction will be a
/// single drop, and identity is `PathKey` so `D:\a\` and `d:/a` share one slot.
pub struct DirIndex {
    tables: Mutex<HashMap<PathKey, Arc<DirTable>>>,
    next_id: AtomicU32,
}

impl Default for DirIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl DirIndex {
    pub fn new() -> Self {
        Self {
            tables: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
        }
    }

    pub fn get(&self, key: &PathKey) -> Option<Arc<DirTable>> {
        self.tables.lock().expect("dir index").get(key).cloned()
    }

    pub fn open(&self, key: &PathKey, cancel: &Cancellation) -> Result<Arc<DirTable>, IndexError> {
        if let Some(table) = self.get(key) {
            return Ok(table);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) as DirId;
        let mut table = DirTable::new(id, key.clone(), None);
        let report = list_one_level(&key.to_path_buf(), cancel, &mut |raw| {
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
            table.push(&raw.name, item);
            true
        })?;
        // A half-read directory must never reach the cache: the next open would serve a listing
        // with a silently missing tail.
        if !report.complete() {
            return Err(IndexError::Cancelled);
        }
        let table = Arc::new(table);
        self.tables
            .lock()
            .expect("dir index")
            .insert(key.clone(), Arc::clone(&table));
        Ok(table)
    }

    /// Drops a cached table, e.g. after an event the cache cannot patch surgically.
    pub fn invalidate(&self, key: &PathKey) -> bool {
        self.tables.lock().expect("dir index").remove(key).is_some()
    }

    pub fn cached_count(&self) -> usize {
        self.tables.lock().expect("dir index").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fam_core::Kind;

    #[test]
    fn opening_twice_serves_the_same_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let key = PathKey::of(dir.path());
        let index = DirIndex::new();

        let first = index.open(&key, &Cancellation::new()).unwrap();
        let second = index.open(&key, &Cancellation::new()).unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.len(), 1);
        assert_eq!(index.cached_count(), 1);
    }

    #[test]
    fn separator_and_case_variants_share_one_slot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let index = DirIndex::new();
        let cancel = Cancellation::new();

        index.open(&PathKey::of(dir.path()), &cancel).unwrap();
        let upper = dir.path().to_string_lossy().to_uppercase();
        let again = index.open(&PathKey::of(std::path::Path::new(&upper)), &cancel);

        assert!(again.is_ok());
        assert_eq!(index.cached_count(), 1);
    }

    #[test]
    fn invalidation_drops_the_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let key = PathKey::of(dir.path());
        let index = DirIndex::new();
        index.open(&key, &Cancellation::new()).unwrap();

        assert!(index.invalidate(&key));
        assert!(!index.invalidate(&key));
        assert!(index.get(&key).is_none());
    }

    #[test]
    fn a_cancelled_open_caches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let index = DirIndex::new();
        let cancel = Cancellation::new();
        cancel.cancel();
        let key = PathKey::of(dir.path());

        let error = index.open(&key, &cancel).unwrap_err();
        assert!(matches!(error, IndexError::Cancelled));
        assert_eq!(index.cached_count(), 0);
        assert!(index.get(&key).is_none());
    }

    #[test]
    fn rows_carry_kind_and_size_from_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), "12345").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let index = DirIndex::new();

        let table = index
            .open(&PathKey::of(dir.path()), &Cancellation::new())
            .unwrap();

        assert_eq!(table.len(), 2);
        let kinds: Vec<Kind> = (0..table.len() as u32)
            .map(|i| fam_core::Kind::from_index(table.item(i).unwrap().kind).unwrap())
            .collect();
        assert!(kinds.contains(&Kind::Image));
        assert!(kinds.contains(&Kind::Directory));
    }
}
