//! The lazy directory cache: the product's core.
//!
//! A directory is enumerated once, stored as a UTF-16 string arena plus fixed-size records, and
//! served in ranges. The cache itself never recurses and nothing here persists; the search router at
//! the end of this header does reach a disk, through the enumeration backend it is handed.
//! Opening is single-flight,
//! tables are evicted by byte budget, and a refresh keeps the previous rows on screen as stale
//! rather than blanking the view. Change events arrive through [`DirIndex::apply`], which places
//! what a row-level patch can express and marks the rest for a re-read.

mod cache;
mod collection;
mod dir_table;
mod natural;
mod patch;
mod router;

pub use cache::{Applied, Claim, DirIndex, Freshness, OpenError, Opened, Snapshot, State};
pub use collection::{Collection, FetchedRow};
pub use dir_table::DirTable;
pub use patch::{Patch, Patched};
pub use router::{Scope, SearchRouter, Searched};
