//! FAM tag store: SQLite persistence for tags, sources and priorities.
//!
//! Implements the CRUD contracts in `docs/legacy-specs.md` §1.2 while resolving the four defects
//! listed in §1.3, and carries the source/priority ordering semantics of §1.4.

mod error;
mod model;
mod notes;
mod store;

pub use error::StoreError;
pub use model::{
    display_order, validate_tag, FileTags, NewTag, SearchRequest, Source, TagRecord, MAX_TAG_CHARS,
};
pub use notes::{NoteRecord, MAX_NOTE_BYTES};
pub use store::{AddOutcome, StoreStats, TagStore, DEFAULT_SEARCH_LIMIT, SCHEMA_VERSION};
