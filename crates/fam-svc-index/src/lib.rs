//! The lazy directory cache: the product's core.
//!
//! A directory is enumerated once, stored as a UTF-16 string arena plus fixed-size records, and
//! served in ranges. Nothing here recurses and nothing here persists. Opening is single-flight,
//! tables are evicted by byte budget, and a refresh keeps the previous rows on screen as stale
//! rather than blanking the view. What is still missing is the filesystem-event half: patching
//! individual rows and the one-level watch that drives it land in increment 4.

mod cache;
mod collection;
mod dir_table;
mod natural;
mod router;

pub use cache::{Claim, DirIndex, Freshness, OpenError, Opened, Snapshot, State};
pub use collection::{Collection, FetchedRow};
pub use dir_table::DirTable;
pub use router::SearchRouter;
