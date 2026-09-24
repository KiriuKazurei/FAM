//! Shared foundation for FAM: the vocabulary every other crate speaks.
//!
//! Configuration, path identity, the display buckets, the fixed-size directory record, the search
//! contract and SHA-256 all live here so that no crate below the UI needs to depend on another for a
//! type. This crate deliberately carries no platform dependencies.

mod cancel;
mod config;
mod hash;
mod kind;
mod list;
mod pathkey;
mod prune;
mod search;
mod sort;
mod time;

pub use cancel::Cancellation;
pub use config::{
    AppPaths, Config, ConfigError, OnMissing, DEFAULT_CACHE_BUDGET_MB,
    DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_EXCLUDED_NAMES, DEFAULT_SCAN_TIMEOUT_SECS,
    DEFAULT_UI_BATCH_MS, DEFAULT_WALK_THREADS, MEMORY_LIMIT_MB_MAX, MEMORY_LIMIT_MB_MIN,
};
pub use hash::{hex_lower, sha256, sha256_hex};
pub use kind::{classify, classify_units, extension_of, kind_of_extension, Kind, KIND_ORDER};
pub use list::{DirId, Item, ItemFlags, ListRequest};
pub use pathkey::{
    fold_name_units, is_within, name_units_match, path_buf_from_units, path_key, raw_name_units,
    raw_path_units, PathKey,
};
pub use prune::Prune;
pub use search::{SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest};
pub use sort::{Order, SortKey};
pub use time::{epoch_ms, format_utc, to_epoch_ms, utc_date};
