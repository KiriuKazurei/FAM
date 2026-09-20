//! Enumeration and name search.
//!
//! Covers the fallback path described in `docs/legacy-specs.md` §2: everything the Everything
//! index cannot answer — non-NTFS volumes, unindexed roots — has to be walked, and this time the
//! walk can genuinely be cancelled and timed out.

mod kind;
mod search;
mod walk;

pub use kind::{classify, extension_of, kind_of_extension, Kind, KIND_ORDER};
pub use search::{
    EverythingBackend, SearchBackend, SearchError, SearchQuery, SearchResults, SearchService,
    WalkBackend, DEFAULT_LIMIT,
};
pub use walk::{
    collect, to_epoch_ms, walk, Cancellation, Entry, WalkPolicy, WalkReport, DEFAULT_MAX_ENTRIES,
    DEFAULT_THREADS,
};
