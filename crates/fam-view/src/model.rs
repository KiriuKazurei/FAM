use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use fam_core::{classify, path_key, to_epoch_ms, Kind};
use fam_sched::{Batch, Change};

// Sorting vocabulary is shared with the directory index; this crate must not own a second copy.
pub use fam_core::{Order, SortKey};

pub type RowKey = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: RowKey,
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub modified_ms: i64,
    pub tags: Vec<String>,
}

impl Row {
    pub fn new(path: &Path, kind: Kind, size: u64, modified_ms: i64) -> Self {
        Self {
            key: path_key(path),
            path: path.to_path_buf(),
            kind,
            size,
            modified_ms,
            tags: Vec::new(),
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Reads a row from the filesystem. `None` means the path is gone.
pub fn row_for_path(path: &Path) -> Option<Row> {
    let metadata = fs::metadata(path).ok()?;
    let is_directory = metadata.is_dir();
    Some(Row::new(
        path,
        classify(path, is_directory),
        if is_directory { 0 } else { metadata.len() },
        metadata.modified().ok().and_then(to_epoch_ms).unwrap_or(0),
    ))
}

fn compare(a: &Row, b: &Row, key: SortKey) -> Ordering {
    match key {
        SortKey::Name => a.key.cmp(&b.key),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Modified => a.modified_ms.cmp(&b.modified_ms),
        SortKey::Kind => a.kind.cmp(&b.kind),
    }
}

/// Sorts with the row key as final tie-breaker so descending order stays deterministic.
pub fn sort_rows(rows: &mut [Row], key: SortKey, order: Order) {
    rows.sort_by(|a, b| match order {
        Order::Ascending => compare(a, b, key).then_with(|| a.key.cmp(&b.key)),
        Order::Descending => compare(b, a, key).then_with(|| a.key.cmp(&b.key)),
    });
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub terms: Vec<String>,
    pub kind: Option<Kind>,
}

impl Filter {
    pub fn matching(text: &str) -> Self {
        Self {
            terms: text.split_whitespace().map(str::to_lowercase).collect(),
            kind: None,
        }
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn passes(&self, row: &Row) -> bool {
        if self.kind.is_some_and(|kind| row.kind != kind) {
            return false;
        }
        if self.terms.is_empty() {
            return true;
        }
        let name = row.file_name().to_lowercase();
        self.terms.iter().all(|term| name.contains(term))
    }
}

/// One positioned change, mirroring the signals a list model emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Inserted {
        index: usize,
        row: Row,
    },
    Removed {
        index: usize,
        key: RowKey,
    },
    Changed {
        index: usize,
        row: Row,
    },
    /// The projection was rebuilt; a model should wrap this in a reset.
    Reset,
}

/// A sorted, filtered projection over the rows a front-end displays.
///
/// A new row lands at its sorted position. Updating a row keeps its index: a file being written
/// must not make its row jump around, so reordering happens only through [`ViewModel::set_sort`]
/// or [`ViewModel::replace_all`], both of which emit `Reset`.
#[derive(Debug)]
pub struct ViewModel {
    rows: HashMap<RowKey, Row>,
    visible: Vec<Row>,
    positions: HashMap<RowKey, usize>,
    positions_current: bool,
    filter: Filter,
    sort_key: SortKey,
    order: Order,
}

impl Default for ViewModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewModel {
    pub fn new() -> Self {
        Self::with_sort(SortKey::Name, Order::Ascending)
    }

    pub fn with_sort(sort_key: SortKey, order: Order) -> Self {
        Self {
            rows: HashMap::new(),
            visible: Vec::new(),
            positions: HashMap::new(),
            positions_current: true,
            filter: Filter::default(),
            sort_key,
            order,
        }
    }

    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Rows known to the model, including those the current filter hides.
    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn rows(&self) -> &[Row] {
        &self.visible
    }

    pub fn get(&self, index: usize) -> Option<&Row> {
        self.visible.get(index)
    }

    pub fn visible_keys(&self) -> Vec<RowKey> {
        self.visible.iter().map(|row| row.key.clone()).collect()
    }

    pub fn position_of(&mut self, key: &str) -> Option<usize> {
        self.refresh_positions();
        self.positions.get(key).copied()
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub fn sort(&self) -> (SortKey, Order) {
        (self.sort_key, self.order)
    }

    pub fn replace_all(&mut self, rows: Vec<Row>) -> Vec<Delta> {
        self.rows = rows.into_iter().map(|row| (row.key.clone(), row)).collect();
        self.rebuild();
        vec![Delta::Reset]
    }

    pub fn set_filter(&mut self, filter: Filter) -> Vec<Delta> {
        if filter == self.filter {
            return Vec::new();
        }
        self.filter = filter;
        self.rebuild();
        vec![Delta::Reset]
    }

    pub fn set_sort(&mut self, sort_key: SortKey, order: Order) -> Vec<Delta> {
        if (sort_key, order) == self.sort() {
            return Vec::new();
        }
        self.sort_key = sort_key;
        self.order = order;
        self.rebuild();
        vec![Delta::Reset]
    }

    /// Applies a merged change batch, stat-ing paths through [`row_for_path`].
    pub fn apply(&mut self, batch: &Batch) -> Vec<Delta> {
        self.apply_with(batch, |path| row_for_path(&path))
    }

    /// Applies a batch through a caller-supplied row source, which keeps the view logic testable
    /// and lets a front-end reuse rows it already holds.
    pub fn apply_with(
        &mut self,
        batch: &Batch,
        mut lookup: impl FnMut(PathBuf) -> Option<Row>,
    ) -> Vec<Delta> {
        let mut deltas = Vec::new();
        for change in &batch.changes {
            let key = change.key();
            if change.change == Change::Removed {
                deltas.extend(self.drop(&key));
                continue;
            }
            match lookup(change.path.clone()) {
                Some(mut row) => {
                    if let Some(existing) = self.rows.get(&key) {
                        row.tags = existing.tags.clone();
                    }
                    deltas.extend(self.upsert(row));
                }
                // A create or rename-in whose file is already gone must not leave a phantom row.
                None => deltas.extend(self.drop(&key)),
            }
        }
        deltas
    }

    pub fn set_tags(&mut self, key: &str, tags: Vec<String>) -> Option<Delta> {
        let row = self.rows.get_mut(key)?;
        if row.tags == tags {
            return None;
        }
        row.tags = tags;
        let updated = row.clone();
        let index = self.position_of(key)?;
        self.visible[index] = updated.clone();
        Some(Delta::Changed {
            index,
            row: updated,
        })
    }

    fn drop(&mut self, key: &RowKey) -> Vec<Delta> {
        self.rows.remove(key);
        match self.position_of(key) {
            Some(index) => {
                let removed = self.visible.remove(index);
                self.shift_positions_after(index, -1);
                vec![Delta::Removed {
                    index,
                    key: removed.key,
                }]
            }
            None => Vec::new(),
        }
    }

    fn upsert(&mut self, row: Row) -> Vec<Delta> {
        let key = row.key.clone();
        self.rows.insert(key.clone(), row.clone());
        if !self.filter.passes(&row) {
            return self.drop(&key);
        }
        match self.position_of(&key) {
            Some(index) => {
                self.visible[index] = row.clone();
                vec![Delta::Changed { index, row }]
            }
            None => {
                let at = self.insert_position(&row);
                self.visible.insert(at, row.clone());
                self.positions_current = false;
                vec![Delta::Inserted { index: at, row }]
            }
        }
    }

    fn insert_position(&self, row: &Row) -> usize {
        self.visible
            .partition_point(|other| self.comes_before(other, row))
    }

    fn comes_before(&self, a: &Row, b: &Row) -> bool {
        let primary = match self.order {
            Order::Ascending => compare(a, b, self.sort_key),
            Order::Descending => compare(b, a, self.sort_key),
        };
        match primary {
            Ordering::Less => true,
            Ordering::Equal => a.key < b.key,
            Ordering::Greater => false,
        }
    }

    fn rebuild(&mut self) {
        let mut rows: Vec<Row> = self
            .rows
            .values()
            .filter(|row| self.filter.passes(row))
            .cloned()
            .collect();
        sort_rows(&mut rows, self.sort_key, self.order);
        self.visible = rows;
        self.positions_current = false;
    }

    fn refresh_positions(&mut self) {
        if self.positions_current {
            return;
        }
        self.positions.clear();
        for (index, row) in self.visible.iter().enumerate() {
            self.positions.insert(row.key.clone(), index);
        }
        self.positions_current = true;
    }

    fn shift_positions_after(&mut self, from: usize, by: isize) {
        if !self.positions_current {
            return;
        }
        for index in self.positions.values_mut() {
            if *index > from {
                *index = (*index as isize + by) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fam_sched::{FileChange, Sink};

    fn row(path: &str, kind: Kind, size: u64, modified_ms: i64) -> Row {
        Row::new(Path::new(path), kind, size, modified_ms)
    }

    fn batch(changes: Vec<FileChange>) -> Batch {
        Batch::new(changes, 0)
    }

    #[test]
    fn new_rows_insert_at_the_sorted_position() {
        let mut view = ViewModel::new();

        let deltas = view.apply_with(
            &batch(vec![
                FileChange::new("D:/dir/b.txt", Change::Created),
                FileChange::new("D:/dir/a.txt", Change::Created),
            ]),
            |path| Some(row(&path.display().to_string(), Kind::Document, 1, 1)),
        );

        assert_eq!(
            deltas,
            vec![
                Delta::Inserted {
                    index: 0,
                    row: row("D:/dir/b.txt", Kind::Document, 1, 1)
                },
                Delta::Inserted {
                    index: 0,
                    row: row("D:/dir/a.txt", Kind::Document, 1, 1)
                },
            ],
            "deltas arrive in processing order, each indexed against the state the previous left"
        );
        assert_eq!(
            view.visible_keys(),
            vec![
                path_key(Path::new("D:/dir/a.txt")),
                path_key(Path::new("D:/dir/b.txt"))
            ]
        );
    }

    #[test]
    fn incremental_inserts_land_where_a_rebuild_would_put_them() {
        let mut view = ViewModel::with_sort(SortKey::Size, Order::Descending);
        for (name, size) in [("small", 10u64), ("huge", 900), ("mid", 300)] {
            let path = format!("D:/x/{name}.bin");
            view.apply_with(
                &batch(vec![FileChange::new(path.clone(), Change::Created)]),
                |_| Some(row(&path, Kind::Other, size, 1)),
            );
        }

        let names: Vec<String> = view.rows().iter().map(|row| row.file_name()).collect();
        assert_eq!(names, vec!["huge.bin", "mid.bin", "small.bin"]);

        let mut rebuilt = ViewModel::with_sort(SortKey::Size, Order::Descending);
        rebuilt.replace_all(vec![
            row("D:/x/small.bin", Kind::Other, 10, 1),
            row("D:/x/huge.bin", Kind::Other, 900, 2),
            row("D:/x/mid.bin", Kind::Other, 300, 3),
        ]);
        assert_eq!(view.visible_keys(), rebuilt.visible_keys());
    }

    #[test]
    fn updates_keep_their_index_instead_of_jumping() {
        let mut view = ViewModel::new();
        view.apply_with(
            &batch(vec![FileChange::new("D:/a.txt", Change::Created)]),
            |_| Some(row("D:/a.txt", Kind::Document, 5, 5)),
        );
        view.apply_with(
            &batch(vec![FileChange::new("D:/a.txt", Change::Modified)]),
            |_| Some(row("D:/a.txt", Kind::Document, 5000, 9)),
        );

        assert_eq!(view.len(), 1);
        assert_eq!(view.get(0).unwrap().size, 5000);
        assert_eq!(view.position_of(&path_key(Path::new("D:/a.txt"))), Some(0));
    }

    #[test]
    fn removals_shift_the_rows_below_them() {
        let mut view = ViewModel::new();
        for name in ["a", "b", "c"] {
            view.apply_with(
                &batch(vec![FileChange::new(
                    format!("D:/{name}.txt"),
                    Change::Created,
                )]),
                move |path| Some(row(&path.display().to_string(), Kind::Document, 1, 1)),
            );
        }

        let deltas = view.apply_with(
            &batch(vec![FileChange::new("D:/b.txt", Change::Removed)]),
            |_| None,
        );

        assert_eq!(
            deltas,
            vec![Delta::Removed {
                index: 1,
                key: path_key(Path::new("D:/b.txt"))
            }]
        );
        assert_eq!(view.len(), 2);
        let last = path_key(Path::new("D:/c.txt"));
        assert_eq!(view.position_of(&last), Some(1));
    }

    #[test]
    fn removing_an_unknown_path_is_a_no_op() {
        let mut view = ViewModel::new();

        let deltas = view.apply_with(
            &batch(vec![FileChange::new("D:/ghost.txt", Change::Removed)]),
            |_| None,
        );

        assert!(deltas.is_empty());
        assert!(view.is_empty());
    }

    #[test]
    fn a_create_whose_target_already_vanished_leaves_no_row() {
        let mut view = ViewModel::new();

        view.apply_with(
            &batch(vec![FileChange::new("D:/blink.txt", Change::Created)]),
            |_| None,
        );

        assert!(view.is_empty());
        assert_eq!(view.total_rows(), 0);
    }

    #[test]
    fn filtered_rows_stay_hidden_until_the_filter_opens() {
        let mut view = ViewModel::new();
        view.apply_with(
            &batch(vec![FileChange::new("D:/img.png", Change::Created)]),
            |_| Some(row("D:/img.png", Kind::Image, 1, 1)),
        );

        let deltas = view.set_filter(Filter::matching("").kind(Kind::Document));

        assert_eq!(deltas, vec![Delta::Reset]);
        assert!(view.is_empty());
        assert_eq!(view.total_rows(), 1, "the row is kept but not displayed");

        view.set_filter(Filter::default());
        assert_eq!(view.len(), 1);
    }

    #[test]
    fn a_row_that_stops_matching_disappears_from_the_view() {
        let mut view = ViewModel::new();
        view.set_filter(Filter::matching("report"));
        view.apply_with(
            &batch(vec![FileChange::new("D:/report.pdf", Change::Created)]),
            |_| Some(row("D:/report.pdf", Kind::Document, 1, 1)),
        );
        assert_eq!(view.len(), 1);

        view.apply_with(
            &batch(vec![FileChange::new("D:/report.pdf", Change::Modified)]),
            |_| Some(row("D:/report.pdf", Kind::Image, 1, 1)),
        );

        assert_eq!(
            view.len(),
            1,
            "the term still matches, only the kind changed"
        );
        view.set_filter(Filter::matching("report").kind(Kind::Image));
        assert_eq!(view.len(), 1);
    }

    #[test]
    fn term_filter_matches_all_words_case_insensitively() {
        let filter = Filter::matching("Q1 REPORT");
        assert!(filter.passes(&row("D:/q1-report.pdf", Kind::Document, 1, 1)));
        assert!(!filter.passes(&row("D:/report.pdf", Kind::Document, 1, 1)));
        assert!(Filter::default().passes(&row("D:/any", Kind::Other, 1, 1)));
    }

    #[test]
    fn tags_update_in_place() {
        let mut view = ViewModel::new();
        view.apply_with(
            &batch(vec![FileChange::new("D:/a.txt", Change::Created)]),
            |_| Some(row("D:/a.txt", Kind::Document, 1, 1)),
        );
        let key = path_key(Path::new("D:/a.txt"));

        let delta = view.set_tags(&key, vec!["keep".into()]).unwrap();

        assert_eq!(view.get(0).unwrap().tags, vec!["keep"]);
        match delta {
            Delta::Changed { index, row } => assert_eq!((index, row.tags[0].as_str()), (0, "keep")),
            other => panic!("expected Changed, got {other:?}"),
        }
        assert!(view.set_tags("ABSENT", vec!["x".into()]).is_none());
        assert!(view.set_tags(&key, vec!["keep".into()]).is_none());
    }

    #[test]
    fn tags_survive_a_later_change_event() {
        let mut view = ViewModel::new();
        view.apply_with(
            &batch(vec![FileChange::new("D:/a.txt", Change::Created)]),
            |_| Some(row("D:/a.txt", Kind::Document, 1, 1)),
        );
        let key = path_key(Path::new("D:/a.txt"));
        view.set_tags(&key, vec!["keep".into()]);

        view.apply_with(
            &batch(vec![FileChange::new("D:/a.txt", Change::Modified)]),
            |_| Some(row("D:/a.txt", Kind::Document, 2, 2)),
        );

        assert_eq!(view.get(0).unwrap().tags, vec!["keep"]);
    }

    #[test]
    fn sorting_by_size_descending_is_stable_for_ties() {
        let mut rows = vec![
            row("D:/b.same", Kind::Other, 7, 1),
            row("D:/a.same", Kind::Other, 7, 1),
            row("D:/c.big", Kind::Other, 90, 1),
        ];

        sort_rows(&mut rows, SortKey::Size, Order::Descending);
        let order: Vec<String> = rows
            .iter()
            .map(|row| row.path.display().to_string())
            .collect();

        assert_eq!(order, vec!["D:/c.big", "D:/a.same", "D:/b.same"]);
    }

    #[test]
    fn set_sort_rebuilds_and_resets() {
        let mut view = ViewModel::new();
        view.apply_with(
            &batch(vec![
                FileChange::new("D:/1.txt", Change::Created),
                FileChange::new("D:/2.txt", Change::Created),
            ]),
            |path| {
                Some(row(
                    &path.display().to_string(),
                    Kind::Document,
                    if path.display().to_string().contains('1') {
                        100
                    } else {
                        1
                    },
                    1,
                ))
            },
        );

        let deltas = view.set_sort(SortKey::Size, Order::Ascending);

        assert_eq!(deltas, vec![Delta::Reset]);
        let names: Vec<String> = view.rows().iter().map(|row| row.file_name()).collect();
        assert_eq!(names, vec!["2.txt", "1.txt"]);
        assert!(view.set_sort(SortKey::Size, Order::Ascending).is_empty());
    }

    #[test]
    fn rows_read_from_the_filesystem_carry_real_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        fs::write(&file, b"12345").unwrap();

        let row = row_for_path(&file).unwrap();

        assert_eq!(row.kind, Kind::Image);
        assert_eq!(row.size, 5);
        assert!(row.modified_ms > 0);
        assert_eq!(row.file_name(), "photo.png");
        assert!(row_for_path(&dir.path().join("absent")).is_none());
        assert_eq!(row_for_path(dir.path()).unwrap().kind, Kind::Directory);
    }

    #[test]
    fn sink_can_be_implemented_for_a_view_forwarder() {
        struct Forwarder {
            view: ViewModel,
            seen: Vec<usize>,
        }
        impl Sink for Forwarder {
            fn flush(&mut self, batch: Batch) {
                let deltas = self.view.apply(&batch);
                self.seen.push(deltas.len());
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("written.txt");
        fs::write(&file, b"hi").unwrap();
        let mut forwarder = Forwarder {
            view: ViewModel::new(),
            seen: Vec::new(),
        };

        forwarder.flush(batch(vec![FileChange::new(file.clone(), Change::Created)]));

        assert_eq!(forwarder.seen, vec![1]);
        assert_eq!(forwarder.view.len(), 1);
    }
}
