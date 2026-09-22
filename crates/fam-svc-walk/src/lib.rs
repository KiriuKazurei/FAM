//! Recursive enumeration, and that enumeration as one of the two name-search backends.
//!
//! Covers the fallback path described in `docs/legacy-specs.md` §2: everything the Everything
//! index cannot answer — non-NTFS volumes, unindexed roots — has to be walked, and this time the
//! walk can genuinely be cancelled and timed out.
//!
//! The vocabulary (`Kind`, `Cancellation`, `path_key`, the fixed-size record) lives in `fam-core`;
//! this crate re-exports the parts its callers name, so a front end depends on one crate for the
//! walk rather than on two for the walk and its types.

mod search;
mod walk;

pub use fam_core::{
    classify, extension_of, kind_of_extension, to_epoch_ms, Cancellation, Kind, KIND_ORDER,
};
pub use search::WalkProvider;
pub use walk::{
    collect, walk, Entry, WalkPolicy, WalkReport, DEFAULT_MAX_ENTRIES, DEFAULT_THREADS,
};
