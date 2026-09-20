use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fam_core::{path_key, Config};

use crate::kind::{classify, Kind};

pub const DEFAULT_MAX_ENTRIES: usize = 200_000;
pub const DEFAULT_THREADS: usize = 10;

const QUEUE_POLL: Duration = Duration::from_micros(400);
const RECV_POLL: Duration = Duration::from_millis(25);

/// Cooperative stop signal: enumeration polls it between directories and between emitted entries,
/// which is what the legacy scan never had — its timeout only made the caller give up while the
/// walk kept running (`docs/legacy-specs.md` §2.2-4).
#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    stop: Arc<AtomicBool>,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct WalkPolicy {
    pub roots: Vec<PathBuf>,
    /// `path_key` prefixes; anything at or below one of them is skipped.
    pub excluded_prefixes: Vec<String>,
    /// Upper-cased directory names skipped at any depth.
    pub excluded_names: Vec<String>,
    pub threads: usize,
    pub max_entries: usize,
    pub include_directories: bool,
}

impl Default for WalkPolicy {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            excluded_prefixes: Vec::new(),
            excluded_names: Vec::new(),
            threads: DEFAULT_THREADS,
            max_entries: DEFAULT_MAX_ENTRIES,
            include_directories: true,
        }
    }
}

impl WalkPolicy {
    pub fn from_config(config: &Config) -> Self {
        Self {
            roots: config.search_roots.clone(),
            excluded_prefixes: config.excluded_dir_keys(),
            excluded_names: config.excluded_name_keys(),
            threads: config.walk_threads.max(1),
            ..Default::default()
        }
    }

    pub fn rooted_at(mut self, dir: impl AsRef<Path>) -> Self {
        self.roots = vec![dir.as_ref().to_path_buf()];
        self
    }

    pub fn without_directories(mut self) -> Self {
        self.include_directories = false;
        self
    }

    /// True when a walk would never reach `path`: either it sits under an excluded prefix, or one
    /// of its components carries an excluded directory name. The walk itself prunes at the
    /// directory level, which is cheaper than asking per file.
    pub fn excludes(&self, path: &Path) -> bool {
        self.prefix_excluded(&path_key(path))
            || path.components().any(|component| {
                let name = component.as_os_str().to_string_lossy().to_uppercase();
                self.excluded_names.contains(&name)
            })
    }

    fn prefix_excluded(&self, key: &str) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| fam_core::is_within(prefix, key))
    }

    fn name_excluded(&self, path: &Path) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        let name = name.to_string_lossy().to_uppercase();
        self.excluded_names.contains(&name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub modified_ms: Option<i64>,
}

impl Entry {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn build(path: PathBuf, is_directory: bool, metadata: Option<fs::Metadata>) -> Self {
        Self {
            kind: classify(&path, is_directory),
            size: metadata.as_ref().map(|meta| meta.len()).unwrap_or(0),
            modified_ms: metadata
                .as_ref()
                .and_then(|meta| meta.modified().ok())
                .and_then(to_epoch_ms),
            path,
        }
    }
}

/// Milliseconds since the Unix epoch, negative for timestamps before it.
pub fn to_epoch_ms(at: SystemTime) -> Option<i64> {
    match at.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).ok(),
        Err(error) => i64::try_from(error.duration().as_millis()).map(|value| -value).ok(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkReport {
    pub emitted: u64,
    pub directories: u64,
    pub skipped: u64,
    pub errors: u64,
    pub cancelled: bool,
    pub timed_out: bool,
    pub truncated: bool,
    pub elapsed_ms: u64,
}

impl WalkReport {
    pub fn stopped_early(&self) -> bool {
        self.cancelled || self.timed_out || self.truncated
    }
}

/// Walks `policy.roots` breadth-first across worker threads and calls `on_entry` on the caller's
/// thread for each hit. Returning `false` from `on_entry` stops the walk.
pub fn walk<F>(policy: &WalkPolicy, cancel: &Cancellation, timeout: Duration, mut on_entry: F) -> WalkReport
where
    F: FnMut(&Entry) -> bool,
{
    let started = Instant::now();
    let deadline = started + timeout;
    let mut seed = VecDeque::new();
    let mut root_errors = 0u64;
    for root in &policy.roots {
        if is_walkable_dir(root) {
            seed.push_back(root.clone());
        } else {
            root_errors += 1;
        }
    }
    let (tx, rx) = mpsc::channel::<Entry>();
    let shared = Arc::new(Shared {
        queue: Mutex::new(seed),
        sender: Mutex::new(Some(tx)),
        active: AtomicUsize::new(0),
        directories: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });

    let workers = start_workers(policy, cancel, &shared);

    let mut report = WalkReport::default();
    loop {
        if cancel.is_stopped() {
            report.cancelled = true;
            break;
        }
        if Instant::now() >= deadline {
            report.timed_out = true;
            break;
        }
        match rx.recv_timeout(RECV_POLL) {
            Ok(entry) => {
                if !on_entry(&entry) {
                    report.cancelled = true;
                    break;
                }
                report.emitted += 1;
                if report.emitted as usize >= policy.max_entries {
                    report.truncated = true;
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.idle() {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    shared.stop.store(true, Ordering::Relaxed);
    for worker in workers {
        let _ = worker.join();
    }
    *shared.sender.lock().expect("walk sender") = None;

    report.directories = shared.directories.load(Ordering::Relaxed);
    report.skipped = shared.skipped.load(Ordering::Relaxed);
    report.errors = root_errors + shared.errors.load(Ordering::Relaxed);
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

/// Convenience wrapper collecting everything into a `Vec`.
pub fn collect(policy: &WalkPolicy, cancel: &Cancellation, timeout: Duration) -> (Vec<Entry>, WalkReport) {
    let mut entries = Vec::new();
    let report = walk(policy, cancel, timeout, |entry| {
        entries.push(entry.clone());
        true
    });
    (entries, report)
}

struct Shared {
    queue: Mutex<VecDeque<PathBuf>>,
    sender: Mutex<Option<mpsc::Sender<Entry>>>,
    active: AtomicUsize,
    directories: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn idle(&self) -> bool {
        self.active.load(Ordering::Relaxed) == 0 && self.queue.lock().expect("walk queue").is_empty()
    }

    fn next_dir(&self) -> Option<PathBuf> {
        let mut queue = self.queue.lock().expect("walk queue");
        let dir = queue.pop_front()?;
        self.active.fetch_add(1, Ordering::Relaxed);
        Some(dir)
    }
}

fn start_workers(
    policy: &WalkPolicy,
    cancel: &Cancellation,
    shared: &Arc<Shared>,
) -> Vec<thread::JoinHandle<()>> {
    let wanted = policy.threads.clamp(1, 32);
    let policy = Arc::new(WalkPolicy {
        roots: Vec::new(),
        excluded_prefixes: policy.excluded_prefixes.clone(),
        excluded_names: policy.excluded_names.clone(),
        threads: wanted,
        max_entries: policy.max_entries,
        include_directories: policy.include_directories,
    });
    (0..wanted)
        .map(|_| {
            let shared = Arc::clone(shared);
            let policy = Arc::clone(&policy);
            let cancel = cancel.clone();
            thread::spawn(move || worker_loop(&policy, &cancel, &shared))
        })
        .collect()
}

fn worker_loop(policy: &WalkPolicy, cancel: &Cancellation, shared: &Shared) {
    while !shared.stop.load(Ordering::Relaxed) && !cancel.is_stopped() {
        let Some(dir) = shared.next_dir() else {
            if shared.idle() {
                return;
            }
            thread::sleep(QUEUE_POLL);
            continue;
        };
        let read = fs::read_dir(&dir);
        match read {
            Ok(entries) => {
                for item in entries {
                    if shared.stop.load(Ordering::Relaxed) || cancel.is_stopped() {
                        break;
                    }
                    shared.handle_item(policy, item);
                }
            }
            Err(_) => {
                shared.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        shared.active.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Shared {
    fn handle_item(&self, policy: &WalkPolicy, item: Result<fs::DirEntry, std::io::Error>) {
        let Ok(item) = item else {
            self.errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let path = item.path();
        let file_type = match item.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                self.errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        // Symlinked directories are reported but never descended into, which keeps cycles and
        // reparse-point storms off the walk (`follow_symlinks` is intentionally not a knob yet).
        let is_directory = file_type.is_dir();
        if is_directory && policy.name_excluded(&path) {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if policy.prefix_excluded(&path_key(&path)) {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if is_directory {
            self.directories.fetch_add(1, Ordering::Relaxed);
            self.queue.lock().expect("walk queue").push_back(path.clone());
        }
        let metadata = item.metadata().ok();
        if is_directory && !policy.include_directories {
            return;
        }
        let entry = Entry::build(path, is_directory, metadata);
        let sender_guard = self.sender.lock().expect("walk sender");
        if let Some(sender) = sender_guard.as_ref() {
            let _ = sender.send(entry);
        }
    }
}

fn is_walkable_dir(path: &Path) -> bool {
    fs::metadata(path).map(|meta| meta.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensionless_files_classify_as_other() {
        assert_eq!(classify(Path::new("D:/LICENSE"), false), Kind::Other);
    }

    #[test]
    fn negative_epoch_offsets_survive_conversion() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1_000);
        assert_eq!(to_epoch_ms(before_epoch), Some(-1_000_000));
    }

    #[test]
    fn policy_exclusions_match_names_and_prefixes_across_separators() {
        let policy = WalkPolicy {
            excluded_names: vec!["NODE_MODULES".to_string()],
            excluded_prefixes: vec![path_key(Path::new(r"D:\cache"))],
            ..Default::default()
        };
        assert!(policy.excludes(Path::new("D:/proj/node_modules/pkg/index.js")));
        assert!(policy.excludes(Path::new("D:/cache/tmp.bin")));
        assert!(!policy.excludes(Path::new("D:/proj/src/main.rs")));
        assert!(!policy.excludes(Path::new("D:/cache_sibling/x")));
    }
}
