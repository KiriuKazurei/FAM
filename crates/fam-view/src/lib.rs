//! Row-oriented view model shared by any front-end.
//!
//! `fam-sched` already merges events into one delta per path per window; this crate turns those
//! deltas into positioned row changes against a sorted, filtered list, which is exactly the
//! `beginInsertRows` / `dataChanged` / `beginResetModel` sequence a `QAbstractListModel` needs.
//! Keeping it here means the list logic is unit-tested without a GUI.

mod model;

pub use model::{row_for_path, sort_rows, Delta, Filter, Order, Row, RowKey, SortKey, ViewModel};
