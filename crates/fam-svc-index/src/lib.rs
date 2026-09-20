//! The lazy directory cache: the product's core.
//!
//! A directory is enumerated once, stored as a string arena plus fixed-size records, and served in
//! ranges. Filesystem events invalidate or patch a table; nothing here recurses and nothing here
//! persists. Single-flight opening, LRU eviction and surgical event patching arrive in increment 3
//! — this skeleton is deliberately the simplest correct version of each piece.

mod cache;
mod collection;
mod dir_table;
mod router;

pub use cache::{DirIndex, IndexError};
pub use collection::{Collection, FetchedRow};
pub use dir_table::DirTable;
pub use router::SearchRouter;
