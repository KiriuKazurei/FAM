//! Recursive enumeration and the transitional name-search surface.
//!
//! Covers the fallback path described in `docs/legacy-specs.md` §2: everything the Everything
//! index cannot answer — non-NTFS volumes, unindexed roots — has to be walked, and this time the
//! walk can genuinely be cancelled and timed out.
//!
//! The vocabulary (`Kind`, `Cancellation`, `path_key`, the fixed-size record) lives in `fam-core`;
//! this crate re-exports it so existing callers keep compiling while the vocabulary moves.

mod search;
mod walk;

pub use fam_core::{
    classify, extension_of, kind_of_extension, to_epoch_ms, Cancellation, Kind, KIND_ORDER,
};
pub use search::{
    EverythingBackend, SearchBackend, SearchError, SearchQuery, SearchResults, SearchService,
    WalkBackend, WalkProvider, DEFAULT_LIMIT,
};
pub use walk::{
    collect, walk, Entry, WalkPolicy, WalkReport, DEFAULT_MAX_ENTRIES, DEFAULT_THREADS,
};
