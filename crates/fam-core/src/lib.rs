//! Shared foundation for FAM: application paths, one merged configuration system,
//! and Windows-accurate path keys.
//!
//! `docs/legacy-specs.md` §3 records three coexisting legacy config implementations that read the
//! same file name with incompatible schemas; this crate is the single replacement.

mod config;
mod pathkey;

pub use config::{
    AppPaths, Config, ConfigError, DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_SCAN_TIMEOUT_SECS,
    DEFAULT_UI_BATCH_MS, DEFAULT_WALK_THREADS, MEMORY_LIMIT_MB_MAX, MEMORY_LIMIT_MB_MIN,
};
pub use pathkey::{is_within, is_within_path, path_key};
