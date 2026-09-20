use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::pathkey::path_key;

pub const APP_ID: &str = "FileAutoManage";
pub const CONFIG_FILE_NAME: &str = "config.toml";

pub const DEFAULT_SCAN_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_WALK_THREADS: usize = 10;
pub const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const DEFAULT_UI_BATCH_MS: u64 = 120;

pub const MEMORY_LIMIT_MB_MIN: u32 = 100;
pub const MEMORY_LIMIT_MB_MAX: u32 = 8192;
/// Budget for the in-memory directory cache when `memory_limit_mb` is unset. Sized from the record
/// layout: a `DirTable` row is 32 bytes plus its name in the UTF-16 arena, so 256 MB holds roughly
/// 3.5M entries.
pub const DEFAULT_CACHE_BUDGET_MB: u32 = 256;

/// Directory names skipped at any depth during enumeration. Name-based exclusion is what the
/// legacy absolute-path list never achieved (`docs/legacy-specs.md` §2.2-3).
pub const DEFAULT_EXCLUDED_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "$recycle.bin",
    "system volume information",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Self {
        let suffix = PathBuf::from(APP_ID);
        let base = |dir: Option<PathBuf>| match dir {
            Some(root) => root.join(&suffix),
            None => PathBuf::from(".fam").join(&suffix),
        };
        Self {
            config_dir: base(dirs::config_dir()),
            data_dir: base(dirs::data_dir()),
            cache_dir: base(dirs::cache_dir()),
        }
    }

    pub fn config_file() -> PathBuf {
        Self::resolve().config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn default_database_file() -> PathBuf {
        Self::resolve().data_dir.join("tags.db")
    }
}

/// What a loader does when the file does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnMissing {
    Error,
    Defaults,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database_path: PathBuf,
    pub search_roots: Vec<PathBuf>,
    pub excluded_dirs: Vec<PathBuf>,
    pub excluded_names: Vec<String>,
    pub scan_timeout_secs: u64,
    pub walk_threads: usize,
    pub event_queue_capacity: usize,
    pub ui_batch_ms: u64,
    /// Ceiling on the in-memory directory cache, not a process working-set cap. The name carries
    /// over from the legacy stack, whose setting names `docs/legacy-specs.md` §3 says are worth
    /// preserving; the range (100-8192 MB) was inherited from a Node `env_loader` and is kept
    /// because it brackets sensible sizes for this purpose.
    pub memory_limit_mb: Option<u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_path: AppPaths::default_database_file(),
            search_roots: default_search_roots(),
            excluded_dirs: Vec::new(),
            excluded_names: DEFAULT_EXCLUDED_NAMES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            scan_timeout_secs: DEFAULT_SCAN_TIMEOUT_SECS,
            walk_threads: DEFAULT_WALK_THREADS,
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            ui_batch_ms: DEFAULT_UI_BATCH_MS,
            memory_limit_mb: None,
        }
    }
}

fn default_search_roots() -> Vec<PathBuf> {
    let roots = dirs::home_dir().map(|home| vec![home]).unwrap_or_default();
    if roots.is_empty() {
        log::warn!("no home directory available; search_roots stays empty");
    }
    roots
}

impl Config {
    /// Loads configuration without ever aborting the process. A missing file yields defaults;
    /// an unreadable or malformed file is reported and also yields defaults, because the legacy
    /// stack called `log::fatal` from the event hot path (`docs/legacy-specs.md` §2.2-5).
    pub fn load() -> Self {
        let path = AppPaths::config_file();
        if !path.exists() {
            return Self::default();
        }
        match Self::load_from(&path) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("falling back to default configuration: {error}");
                Self::default()
            }
        }
    }

    /// Strict: an absent file is an error.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        Self::load_with(path, OnMissing::Error)
    }

    /// Lenient: an absent file yields the defaults; a present file is merged over them by
    /// `#[serde(default)]`.
    pub fn load_merging(path: &Path) -> Result<Self, ConfigError> {
        Self::load_with(path, OnMissing::Defaults)
    }

    pub fn load_with(path: &Path, on_missing: OnMissing) -> Result<Self, ConfigError> {
        let text = match read_if_present(path)? {
            Some(text) => text,
            None => match on_missing {
                OnMissing::Error => return Err(ConfigError::Missing(path.to_path_buf())),
                OnMissing::Defaults => return Ok(Self::default()),
            },
        };
        let config = Self::parse(text, path)?;
        config.validate()?;
        Ok(config)
    }

    fn parse(text: String, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
    }

    pub fn write_template_to(path: &Path) -> Result<PathBuf, ConfigError> {
        let write_error = |error: io::Error, target: &Path| ConfigError::Write {
            path: target.to_path_buf(),
            source: error,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| write_error(error, parent))?;
        }
        let body = toml::to_string_pretty(&Self::default())
            .map_err(|error| ConfigError::Serialize(error.to_string()))?;
        let mut file = fs::File::create(path).map_err(|error| write_error(error, path))?;
        let header = b"# FAM configuration. Omitted keys fall back to built-in defaults.\r\n";
        file.write_all(header)
            .map_err(|error| write_error(error, path))?;
        file.write_all(body.as_bytes())
            .map_err(|error| write_error(error, path))?;
        Ok(path.to_path_buf())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database_path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid(
                "database_path must not be empty".into(),
            ));
        }
        if self.walk_threads == 0 || self.walk_threads > 64 {
            return Err(ConfigError::Invalid(format!(
                "walk_threads must be within 1..=64, got {}",
                self.walk_threads
            )));
        }
        if self.event_queue_capacity == 0 {
            return Err(ConfigError::Invalid(
                "event_queue_capacity must be greater than 0".into(),
            ));
        }
        if !(10..=5_000).contains(&self.ui_batch_ms) {
            return Err(ConfigError::Invalid(format!(
                "ui_batch_ms must be within 10..=5000, got {}",
                self.ui_batch_ms
            )));
        }
        if self.scan_timeout_secs == 0 {
            return Err(ConfigError::Invalid(
                "scan_timeout_secs must be greater than 0".into(),
            ));
        }
        if let Some(limit) = self.memory_limit_mb {
            if !(MEMORY_LIMIT_MB_MIN..=MEMORY_LIMIT_MB_MAX).contains(&limit) {
                return Err(ConfigError::Invalid(format!(
                    "memory_limit_mb must be within {MEMORY_LIMIT_MB_MIN}..={MEMORY_LIMIT_MB_MAX}, got {limit}"
                )));
            }
        }
        Ok(())
    }

    pub fn scan_timeout(&self) -> Duration {
        Duration::from_secs(self.scan_timeout_secs)
    }

    /// Bytes the directory cache may hold before whole tables start getting evicted.
    pub fn cache_budget_bytes(&self) -> u64 {
        u64::from(self.memory_limit_mb.unwrap_or(DEFAULT_CACHE_BUDGET_MB)) * 1024 * 1024
    }

    pub fn ui_batch_window(&self) -> Duration {
        Duration::from_millis(self.ui_batch_ms)
    }

    pub fn excluded_name_keys(&self) -> Vec<String> {
        self.excluded_names
            .iter()
            .map(|name| name.to_uppercase())
            .collect()
    }

    pub fn excluded_dir_keys(&self) -> Vec<String> {
        self.excluded_dirs
            .iter()
            .map(|dir| path_key(dir.as_path()))
            .collect()
    }
}

fn read_if_present(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    Missing(PathBuf),
    #[error("cannot read {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {}: {source}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot parse {}: {source}", .path.display())]
    Parse {
        path: PathBuf,
        // Boxed: `toml::de::Error` is far larger than the rest of this enum's payloads.
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("cannot serialise configuration: {0}")]
    Serialize(String),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn partial_file_keeps_defaults_for_omitted_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "scan_timeout_secs = 5\n").unwrap();

        let config = Config::load_merging(&path).unwrap();

        assert_eq!(config.scan_timeout_secs, 5);
        assert_eq!(config.walk_threads, DEFAULT_WALK_THREADS);
        assert_eq!(config.event_queue_capacity, DEFAULT_EVENT_QUEUE_CAPACITY);
    }

    #[test]
    fn malformed_file_is_reported_but_not_fatal_through_load_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "walk_threads = 0\n").unwrap();

        let error = Config::load_merging(&path).unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(_)), "got {error:?}");
    }

    #[test]
    fn missing_file_reads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_merging(&dir.path().join("absent.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn template_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(CONFIG_FILE_NAME);
        Config::write_template_to(&path).unwrap();

        let config = Config::load_from(&path).unwrap();

        assert_eq!(config.walk_threads, DEFAULT_WALK_THREADS);
        assert_eq!(config.excluded_names, Config::default().excluded_names);
    }

    #[test]
    fn memory_limit_range_is_enforced() {
        let too_small = Config {
            memory_limit_mb: Some(64),
            ..Config::default()
        };
        assert!(matches!(too_small.validate(), Err(ConfigError::Invalid(_))));
        let within_range = Config {
            memory_limit_mb: Some(1024),
            ..Config::default()
        };
        within_range.validate().unwrap();
    }

    #[test]
    fn the_cache_budget_defaults_and_honours_an_override() {
        let megabyte = 1024 * 1024;
        assert_eq!(
            Config::default().cache_budget_bytes(),
            u64::from(DEFAULT_CACHE_BUDGET_MB) * megabyte
        );
        assert_eq!(
            Config {
                memory_limit_mb: Some(1024),
                ..Config::default()
            }
            .cache_budget_bytes(),
            1024 * megabyte
        );
    }
}
