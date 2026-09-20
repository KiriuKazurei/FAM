use std::cmp::Ordering;
use std::sync::Arc;

use fam_core::{Item, Kind, Order, SortKey};

use crate::dir_table::DirTable;

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
pub struct Collection {
    table: Arc<DirTable>,
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
            order: Vec::new(),
            sort: SortKey::Name,
            direction: Order::Ascending,
            terms: Vec::new(),
            kind: None,
        };
        collection.rebuild();
        collection
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
        let item = self.table.item(index).expect("index in range");
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
        // Ordinal for now: the Explorer-compatible natural comparator (StrCmpLogicalW) arrives in
        // increment 3 and must slot in here without changing this call shape.
        let primary = match self.sort {
            SortKey::Name => self.table.name_of(left).cmp(self.table.name_of(right)),
            SortKey::Size => left.size.cmp(&right.size),
            SortKey::Modified => left.mtime_ms.cmp(&right.mtime_ms),
            SortKey::Kind => left.kind.cmp(&right.kind),
        };
        let by_name = || self.table.name_of(left).cmp(self.table.name_of(right));
        match self.direction {
            Order::Ascending => primary.then_with(by_name),
            Order::Descending => primary.reverse().then_with(by_name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
