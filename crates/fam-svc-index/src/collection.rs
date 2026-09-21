use std::cmp::Ordering;
use std::sync::Arc;

use fam_core::{Item, Kind, Order, SortKey};

use crate::cache::Snapshot;
use crate::dir_table::DirTable;
use crate::natural::compare_units;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedRow {
    pub index: u32,
    pub item: Item,
    pub name: Vec<u16>,
}

/// An ordered, filtered, range-addressable view over one cached directory.
///
/// A virtualised UI only ever asks for `fetch(offset, count)`, so a 100k-row directory costs one
/// viewport's worth of work per paint. The same type backs search results, which is what keeps
/// "collection" from being a directory-specific invention.
///
/// A view is *positioned*: the UI animates rows by index, so the view owns its own ordering list of
/// record indices and can follow a table that changed underneath it without rebuilding
/// ([`Collection::patch`]). That is only possible because record indices are stable within a table.
pub struct Collection {
    table: Arc<DirTable>,
    /// The version of the rows [`Collection::fetch`] serves. A view that has not been told about a
    /// newer generation is showing rows the cache has already replaced.
    generation: u64,
    order: Vec<u32>,
    sort: SortKey,
    direction: Order,
    terms: Vec<String>,
    kind: Option<Kind>,
}

impl Collection {
    pub fn new(table: Arc<DirTable>) -> Self {
        let mut collection = Self {
            table,
            generation: 0,
            order: Vec::new(),
            sort: SortKey::Name,
            direction: Order::Ascending,
            terms: Vec::new(),
            kind: None,
        };
        collection.rebuild();
        collection
    }

    /// A view over a snapshot the cache handed out, at the generation that snapshot came with.
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut collection = Self::new(snapshot.table);
        collection.generation = snapshot.generation;
        collection
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn sort(&self) -> (SortKey, Order) {
        (self.sort, self.direction)
    }

    pub fn set_sort(&mut self, sort: SortKey, direction: Order) {
        if (self.sort, self.direction) == (sort, direction) {
            return;
        }
        self.sort = sort;
        self.direction = direction;
        self.rebuild();
    }

    pub fn set_filter(&mut self, terms: Vec<String>, kind: Option<Kind>) {
        if self.terms == terms && self.kind == kind {
            return;
        }
        self.terms = terms;
        self.kind = kind;
        self.rebuild();
    }

    /// Follows the table one row-level patch produced, without rebuilding the ordering.
    ///
    /// Only a [`Patched`](crate::Patched) snapshot belongs here: patching copies a table and
    /// tombstones rows where they stand, so the record indices this ordering holds still name the
    /// same rows. Rows from a **read** are a different table with different indices, and a view takes
    /// those with [`Collection::from_snapshot`] instead — which is why the cache drops a patch whose
    /// generation has been superseded rather than applying it
    /// ([`DirIndex::apply`](crate::DirIndex::apply)).
    ///
    /// The positions returned are where to cut, each against the view as the earlier cuts already
    /// left it — which is what a model that removes rows one range at a time needs. They come back
    /// in view order, because one walk of the ordering computes them; the order the events arrived in
    /// is not preserved and cannot be, since an earlier cut shifts every position after it.
    /// Only departures can be followed this way: the events that produced a patch say
    /// nothing about the size, time or folder-or-not of a row that appeared, so an added row means
    /// re-reading the directory and the view sees the result as a newer generation.
    pub fn patch(&mut self, snapshot: &Snapshot, removed: &[u32]) -> Vec<usize> {
        self.table = Arc::clone(&snapshot.table);
        self.generation = snapshot.generation;
        if removed.is_empty() {
            return Vec::new();
        }
        let mut gone = vec![false; self.table.len()];
        for &index in removed {
            if let Some(slot) = gone.get_mut(index as usize) {
                *slot = true;
            }
        }
        let mut kept = Vec::with_capacity(self.order.len());
        let mut positions = Vec::with_capacity(removed.len());
        for (view_at, &index) in self.order.iter().enumerate() {
            if gone.get(index as usize).copied().unwrap_or(false) {
                positions.push(view_at - positions.len());
            } else {
                kept.push(index);
            }
        }
        self.order = kept;
        positions
    }

    pub fn fetch(&self, offset: usize, count: usize) -> Vec<FetchedRow> {
        self.order
            .iter()
            .skip(offset)
            .take(count)
            .map(|&index| {
                let item = *self.table.item(index).expect("order references a live row");
                FetchedRow {
                    index,
                    name: self.table.name_of(&item).to_vec(),
                    item,
                }
            })
            .collect()
    }

    fn rebuild(&mut self) {
        let mut indices: Vec<u32> = (0..self.table.len() as u32)
            .filter(|&index| self.matches(index))
            .collect();
        indices.sort_by(|&a, &b| self.compare(a, b));
        self.order = indices;
    }

    fn matches(&self, index: u32) -> bool {
        // `item` answers `None` for a tombstone, which is how a row a change event removed stops
        // being displayable without the record itself moving.
        let Some(item) = self.table.item(index) else {
            return false;
        };
        if self.kind.is_some_and(|kind| item.kind != kind.index()) {
            return false;
        }
        if self.terms.is_empty() {
            return true;
        }
        let name = String::from_utf16_lossy(self.table.name_of(item)).to_lowercase();
        self.terms.iter().all(|term| name.contains(term))
    }

    fn compare(&self, a: u32, b: u32) -> Ordering {
        let left = self.table.item(a).expect("index in range");
        let right = self.table.item(b).expect("index in range");
        let primary = match self.sort {
            SortKey::Name => self.by_name(left, right),
            SortKey::Size => left.size.cmp(&right.size),
            SortKey::Modified => left.mtime_ms.cmp(&right.mtime_ms),
            SortKey::Kind => left.kind.cmp(&right.kind),
        };
        let by_name = || self.by_name(left, right);
        match self.direction {
            Order::Ascending => primary.then_with(by_name),
            Order::Descending => primary.reverse().then_with(by_name),
        }
    }

    /// Explorer-compatible name order with an ordinal tie-break. The shell comparator ignores case,
    /// so without the extra ordinal compare two rows could compare equal and the sort would have no
    /// defined result.
    fn by_name(&self, left: &Item, right: &Item) -> Ordering {
        let (first, second) = (self.table.name_of(left), self.table.name_of(right));
        compare_units(first, second).then_with(|| first.cmp(second))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Freshness;
    use crate::patch::Patch;
    use fam_core::{ItemFlags, PathKey};

    fn table_with(entries: &[(impl AsRef<str>, Kind, u64)]) -> Arc<DirTable> {
        let mut table = DirTable::new(1, PathKey::of(std::path::Path::new("D:/x")), None);
        for (name, kind, size) in entries {
            let item = Item {
                name_off: 0,
                name_units: 0,
                kind: kind.index(),
                flags: ItemFlags::default().0,
                size: *size,
                mtime_ms: 0,
                attrs: 0,
                dir: 0,
            };
            table.push(&name.as_ref().encode_utf16().collect::<Vec<u16>>(), item);
        }
        Arc::new(table)
    }

    #[test]
    fn fetch_returns_only_the_requested_window() {
        let entries: Vec<(String, Kind, u64)> = (0..10)
            .map(|i| (format!("file{i:02}.txt"), Kind::Document, 1))
            .collect();
        let collection = Collection::new(table_with(&entries));

        assert_eq!(collection.len(), 10);
        let page = collection.fetch(3, 4);
        let names: Vec<String> = page
            .iter()
            .map(|row| String::from_utf16_lossy(&row.name))
            .collect();
        assert_eq!(
            names,
            vec!["file03.txt", "file04.txt", "file05.txt", "file06.txt"]
        );
        assert!(collection.fetch(8, 10).len() == 2);
    }

    #[test]
    fn descending_keeps_the_name_tie_break_ascending() {
        let collection = {
            let mut collection = Collection::new(table_with(&[
                ("a.txt", Kind::Document, 5),
                ("b.txt", Kind::Document, 5),
                ("big.bin", Kind::Archive, 90),
            ]));
            collection.set_sort(SortKey::Size, Order::Descending);
            collection
        };

        let names: Vec<String> = collection
            .fetch(0, 10)
            .iter()
            .map(|row| String::from_utf16_lossy(&row.name))
            .collect();
        assert_eq!(names, vec!["big.bin", "a.txt", "b.txt"]);
    }

    #[test]
    fn filters_narrow_the_visible_set() {
        let mut collection = Collection::new(table_with(&[
            ("photo.png", Kind::Image, 1),
            ("notes.txt", Kind::Document, 1),
            ("photo-backup.png", Kind::Image, 1),
        ]));

        collection.set_filter(vec!["photo".into()], Some(Kind::Image));
        assert_eq!(collection.len(), 2);

        collection.set_filter(vec!["photo".into()], None);
        assert_eq!(collection.len(), 2);

        collection.set_filter(Vec::new(), Some(Kind::Document));
        assert_eq!(collection.len(), 1);
    }

    /// `fetch_returns_only_the_requested_window` uses zero-padded names, where ordinal and natural
    /// order agree; this is the case that would silently regress if the comparator were dropped.
    #[cfg(windows)]
    #[test]
    fn name_sort_reads_numbers_as_numbers() {
        let collection = Collection::new(table_with(&[
            ("file10.txt", Kind::Document, 1),
            ("file2.txt", Kind::Document, 1),
            ("file1.txt", Kind::Document, 1),
            ("Chapter 3.md", Kind::Document, 1),
            ("Chapter 30.md", Kind::Document, 1),
        ]));

        let names: Vec<String> = collection
            .fetch(0, 10)
            .iter()
            .map(|row| String::from_utf16_lossy(&row.name))
            .collect();
        assert_eq!(
            names,
            vec![
                "Chapter 3.md",
                "Chapter 30.md",
                "file1.txt",
                "file2.txt",
                "file10.txt"
            ]
        );
    }

    fn snapshot_at(table: &Collection, generation: u64) -> Snapshot {
        Snapshot {
            table: table.table.clone(),
            generation,
            freshness: Freshness::Current,
        }
    }

    fn collection_of(names: &[&str]) -> Collection {
        let entries: Vec<(&str, Kind, u64)> = names
            .iter()
            .map(|name| (*name, Kind::Document, 1))
            .collect();
        Collection::new(table_with(&entries))
    }

    fn visible(collection: &Collection) -> Vec<String> {
        collection
            .fetch(0, collection.len())
            .iter()
            .map(|row| String::from_utf16_lossy(&row.name))
            .collect()
    }

    #[test]
    fn a_view_remembers_the_generation_it_was_built_from() {
        let plain = collection_of(&["a.txt"]);
        assert_eq!(plain.generation(), 0, "a loose table has no version");

        let tracked = Collection::from_snapshot(snapshot_at(&plain, 7));
        assert_eq!(tracked.generation(), 7);
        assert_eq!(visible(&tracked), ["a.txt"], "and it shows the same rows");
    }

    #[test]
    fn patch_positions_are_for_applying_in_the_order_returned() {
        let mut view = collection_of(&["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
        let patched = view.table.patched(&Patch {
            removed: vec![
                "d.txt".encode_utf16().collect(),
                "b.txt".encode_utf16().collect(),
            ],
            unread: 0,
        });
        // The names arrive in event order, so `removed` is [3, 1] - d before b.
        assert_eq!(patched.removed, vec![3, 1]);

        let positions = view.patch(
            &Snapshot {
                table: Arc::new(patched.table),
                generation: 1,
                freshness: Freshness::Current,
            },
            &patched.removed,
        );

        assert_eq!(
            positions,
            vec![1, 2],
            "one walk of the ordering, so positions come back in view order: b at 1, then d, which
             the first cut already moved from 3 to 2"
        );
        assert_eq!(visible(&view), ["a.txt", "c.txt", "e.txt"]);
        assert_eq!(view.generation(), 1);
    }

    #[test]
    fn a_row_cut_from_the_middle_keeps_the_rest_in_order() {
        let mut view = collection_of(&["alpha.txt", "mid.txt", "omega.txt"]);
        let patched = view.table.patched(&Patch {
            removed: vec!["mid.txt".encode_utf16().collect()],
            unread: 0,
        });

        let positions = view.patch(
            &Snapshot {
                table: Arc::new(patched.table),
                generation: 4,
                freshness: Freshness::Current,
            },
            &patched.removed,
        );

        assert_eq!(positions, vec![1]);
        assert_eq!(view.len(), 2);
        assert_eq!(visible(&view), ["alpha.txt", "omega.txt"]);
    }

    #[test]
    fn a_filter_hides_a_row_a_patch_took_out() {
        let mut view = Collection::new(table_with(&[
            ("aaa.txt", Kind::Document, 1),
            ("picture.png", Kind::Image, 1),
            ("mmm.txt", Kind::Document, 1),
        ]));
        view.set_filter(Vec::new(), Some(Kind::Document));
        assert_eq!(visible(&view), ["aaa.txt", "mmm.txt"], "two documents");

        let patched = view.table.patched(&Patch {
            removed: vec!["mmm.txt".encode_utf16().collect()],
            unread: 0,
        });
        let positions = view.patch(
            &Snapshot {
                table: Arc::new(patched.table),
                generation: 2,
                freshness: Freshness::Stale,
            },
            &patched.removed,
        );

        assert_eq!(
            positions,
            vec![1],
            "second in the filtered view, third of the three records"
        );
        assert_eq!(visible(&view), ["aaa.txt"]);
    }
}
