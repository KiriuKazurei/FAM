//! C ABI over the FAM core.
//!
//! JSON crosses the boundary rather than Rust type layouts, so the Qt side only ever handles
//! UTF-8 strings it can hand straight to `QJsonDocument`. Every entry point catches unwinding: a
//! Rust panic must never unwind into C++ frames.
//!
//! # Safety contract, shared by every `extern "C"` entry point
//!
//! The `unsafe` on each function is the caller's obligation, stated once here rather than
//! repeated thirteen times, and mirrored in `apps/fam-shell/include/famcore.h`:
//!
//! * string arguments are either null or a NUL-terminated UTF-8 buffer that stays valid for the
//!   duration of the call;
//! * `out` / `err` are either null or a writable `char*` slot, and a non-null result in them is
//!   owned by the caller, which must release it with [`fam_free_string`];
//! * a handle returned by [`fam_core_new`] is opaque, used from one thread at a time, and must be
//!   released with [`fam_core_free`] before the library is unloaded.
// The contract is documented above instead of per function.
#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uchar, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fam_core::{Config, Order, PathKey, SearchRequest as NameRequest, SortKey};
use fam_sched::{spawn, SchedulerConfig};
use fam_svc_audit::{Actor, AuditLog, Draft, Op, Options as AuditOptions};
use fam_svc_index::DirIndex;
use fam_svc_store::{NewTag, Source, TagSearch, TagStore};
use fam_svc_walk::Kind;
use serde::Serialize;

mod browse;
mod search;

/// The v2 byte contract, public so a caller in the same workspace - the tests here, and the MCP
/// server that will speak the same rows later - reads the layout from one place.
pub mod wire;

use browse::{Job, Loader, Views};
use fam_svc_index::SearchRouter;
use search::Searches;

const OK: c_int = 0;
const ERR: c_int = -1;

/// Rows one `fam_dir_fetch` will copy out, however large the requested `count`. A viewport is tens of
/// rows; the cap is there so one call cannot be made to allocate without bound, and the header's
/// truncated flag tells the caller to ask again.
const MAX_ROWS_PER_CALL: usize = 4096;

/// How many hits a `fam_search_begin` with `limit = 0` asks for. The number is inherited from the
/// legacy tag store's default, and it exists at all because a search accumulates its whole result
/// set on the owner thread: "no ceiling" against a root like `D:\` would be an unbounded allocation
/// that only a cancel can interrupt.
const DEFAULT_SEARCH_LIMIT: u32 = 500;

/// Ceiling on an explicit `limit`, so a caller cannot ask for the unbounded case by name.
const MAX_SEARCH_LIMIT: u32 = 20_000;

pub struct FamCore {
    config: Config,
    /// Shared with the tag search backend, which is a reader of this store on the search thread while
    /// the GUI thread writes through it. A mutex rather than a per-thread handle because SQLite is
    /// opened once, and locked per query rather than per search so a thousand `metadata` calls never
    /// hold up a `Tags…` dialog.
    store: Arc<Mutex<TagStore>>,
    /// Events discarded since the pipeline started, read by a v2 view against the baseline it was
    /// opened with. Nothing ever resets it: the only thing that makes a loss stop matter is a
    /// completed re-read, and that moves the view's baseline rather than this count.
    losses: Arc<AtomicU64>,
    /// v2: the cache every directory view is served from.
    index: DirIndex,
    v2: Views,
    /// v2's one pipeline, shared with the loader thread that registers and withdraws its watches.
    loader: Loader,
    /// Which backends can answer a name search, and what they say about themselves. Held here as
    /// well as in [`Searches`] because `fam_backends_json` answers without waiting for a search.
    router: Arc<SearchRouter>,
    /// v2's paged name search: the router, and the one thread allowed to run a search on it.
    searches: Searches,
    /// Directories with an open view, so the last one to close can stop its watch. `Handle` dedupes
    /// by identity already; this counts the views, which is what decides whether a watch is still
    /// wanted.
    watched: Mutex<HashMap<PathKey, u32>>,
    /// Every request this ABI answers leaves a line here. Held by the core rather than by a global
    /// because the session id and the configuration both belong to one handle.
    audit: AuditLog,
    /// The asynchronous transfers, keyed by the id `fam_transfer_begin` handed out. A transfer runs
    /// on its own thread - a cross-volume copy of a large file takes minutes, and the GUI thread
    /// calling `fam_path_op` synchronously is exactly the freeze increment 14 left behind. The
    /// registry also remembers what each job is, because the poll that first observes a terminal
    /// state writes the job's closing audit line with it.
    transfers: Mutex<(u64, HashMap<u64, TransferJob>)>,
}

/// One asynchronous transfer: the handles the copy thread and the beat share, and enough of the
/// request for the closing audit line to say what finished.
struct TransferJob {
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<JobState>>,
    from: PathBuf,
    to: PathBuf,
    copy: bool,
}

/// What the beat reads. `outcome` is set exactly when `phase` is `Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Running,
    Done,
    Cancelled,
    Failed,
}

struct JobState {
    phase: JobPhase,
    bytes: u64,
    total: u64,
    outcome: Option<fam_svc_fs::Transferred>,
    error: Option<String>,
}

impl JobState {
    fn new() -> Self {
        Self {
            phase: JobPhase::Running,
            bytes: 0,
            total: 0,
            outcome: None,
            error: None,
        }
    }
}

/// One request as the audit log sees it: which entry point, which path it names, and a summary of
/// the rest of the arguments.
///
/// Filled in by the call body rather than by the entry point on purpose. An argument is read once -
/// a `char*` the caller owns can be freed the moment the call returns, and reading it twice to build
/// a log line first would be a use-after-free the caller could time. So the line carries what the
/// body managed to read, and a call that failed on a null or malformed argument says exactly that
/// instead of echoing bytes this program has no business dereferencing twice.
struct Call {
    op: Op,
    target: Option<PathBuf>,
    args: Option<String>,
}

impl Call {
    fn new(op: Op) -> Self {
        Self {
            op,
            target: None,
            args: None,
        }
    }

    /// Names the path the request is about. `op` is left alone.
    fn names(&mut self, path: &str) -> &mut Self {
        self.target = Some(PathBuf::from(path));
        self
    }

    /// The view or search token the request addresses. Nearly every v2 call is about one, and a line
    /// that says `dir.fetch` without it cannot be joined to the `dir.open` that made it.
    fn with_token(mut self, token: u64) -> Self {
        self.args = Some(format!("token={token}"));
        self
    }

    /// Adds to the summary rather than replacing it, so a call can name its token and then its
    /// window without either one deciding the shape of the line.
    fn says(&mut self, summary: impl std::fmt::Display) -> &mut Self {
        self.args = Some(match &self.args {
            Some(already) => format!("{already} {summary}"),
            None => summary.to_string(),
        });
        self
    }
}

#[derive(Serialize)]
struct TagOut {
    tag: String,
    source: String,
    priority: i32,
    created_at: String,
}

#[derive(Serialize)]
struct VocabularyOut {
    tag: String,
    files: usize,
}

#[derive(Serialize)]
struct StatsOut {
    database: String,
    roots: Vec<String>,
    files: i64,
    tags: i64,
    manual: i64,
    auto: i64,
    dropped_events: u64,
    /// The audit log's own account of itself. A log that stopped writing has to be visible from the
    /// side that does not read files, or a full disk turns into an empty trail and nobody notices.
    audit: AuditOut,
}

#[derive(Serialize)]
struct AuditOut {
    directory: String,
    session: u64,
    records: u64,
    bytes_today: u64,
    dropped: u64,
    /// The session's whole toll, which outlives the count a record carries away.
    dropped_total: u64,
    dropped_because: Option<String>,
    writing: bool,
    record_reads: bool,
    /// What `bytes_today` has to be read against: a day at its ceiling is a day whose read
    /// stream stopped, and without this number a reader cannot tell the two apart.
    daily_limit_bytes: u64,
    last_error: Option<String>,
}

/// The read `key` owes right now: `None` when the cached rows are current, when a read is already
/// running, or when a failure is still cooling down. `claim` is the arbiter of the last two, so this
/// is the whole of the "should somebody re-read this directory" question everywhere it is asked.
fn owed_read(index: &DirIndex, key: &PathKey) -> Option<fam_svc_index::Claim> {
    if matches!(index.state(key), fam_svc_index::State::Ready) {
        return None;
    }
    index.claim(key)
}

/// Hands that read to the loader thread over the queue the caller already holds. The sink cannot go
/// through [`Loader::post`] - it is built before the loader, which needs the pipeline it posts into -
/// so the queue is passed rather than the loader.
fn ask_for_read(index: &DirIndex, queue: &Arc<Mutex<std::sync::mpsc::Sender<Job>>>, key: &PathKey) {
    if let Some(claim) = owed_read(index, key) {
        if let Ok(sender) = queue.lock() {
            let _ = sender.send(Job::Load(claim));
        }
    }
}

impl FamCore {
    fn create(config_path: Option<&Path>) -> Result<Self, String> {
        let config = match config_path {
            Some(path) => Config::load_merging(path).map_err(|error| error.to_string())?,
            None => Config::load(),
        };
        config.validate().map_err(|error| error.to_string())?;
        // The audit log is not a feature that can be missing, so it is opened before anything else
        // that has a side effect: no view, no watch, no database handle exists if the trail cannot
        // be written.
        // The two directories the program writes its own state into must not contain a search root:
        // honouring such a root would mean indexing the audit trail or the credential file, and
        // excluding them would mean silently deleting a folder the configuration named. Asking the
        // audit crate is deliberate - the predicate is a path question, and `fam-svc-auth` is not and
        // must not become a dependency here, or the ABI would grow a way to reach credentials.
        let credential_directory = config
            .mcp_token_file
            .parent()
            .unwrap_or(config.mcp_token_file.as_path());
        for (what, directory) in [
            ("audit directory", config.audit_dir.as_path()),
            ("credential directory", credential_directory),
        ] {
            if let Some(root) = fam_svc_audit::conflicting_root(directory, &config.search_roots) {
                return Err(format!(
                    "the {what} {} would contain the search root {}; move one of them",
                    directory.display(),
                    root.display()
                ));
            }
        }
        let audit = AuditLog::open(AuditOptions {
            directory: config.audit_dir.clone(),
            actor: Actor::User,
            record_reads: config.audit_record_reads,
            daily_limit_bytes: config.audit_daily_limit_bytes(),
            clock: fam_svc_audit::system_clock(),
        })
        .map_err(|error| {
            format!(
                "cannot open the audit log in {}: {error}",
                config.audit_dir.display()
            )
        })?;
        let store = Arc::new(Mutex::new(
            TagStore::open(&config.database_path).map_err(|error| error.to_string())?,
        ));
        let losses = Arc::new(AtomicU64::new(0));
        let index = DirIndex::with_budget_bytes(config.cache_budget_bytes());
        let v2 = Views::new();
        let (sender, receiver) = Loader::channel();
        let queue = Arc::new(Mutex::new(sender));

        // One pipeline serves the whole ABI, so a directory someone both browses and searches is
        // watched once rather than once per entry point.
        let sink_index = DirIndex::clone(&index);
        let sink_views = Views::clone(&v2);
        let sink_queue = Arc::clone(&queue);
        let sink_losses = Arc::clone(&losses);
        let sink = move |batch: fam_sched::Batch| {
            sink_losses.fetch_add(batch.dropped, Ordering::Relaxed);
            let groups = batch.by_directory();
            for delta in groups.iter() {
                if delta.needs_rescan() {
                    // A lost watch or a moved write time: the only safe instruction is to re-read,
                    // and `claim` is what decides whether anyone is already doing it.
                    sink_index.invalidate(&delta.dir);
                    ask_for_read(&sink_index, &sink_queue, &delta.dir);
                }
                sink_views.absorb(&sink_index, delta);
                // A create or a modification is a row the patch cannot place - the event says that
                // something happened, not what it looks like - so absorbing it leaves the table
                // stale. Nothing else will ever re-read it: the sweep stays quiet about exactly the
                // changes an event already explained, and this is the one place that still knows
                // about the directory. Without this the new file appears when the folder is
                // reopened and never while it is being watched.
                ask_for_read(&sink_index, &sink_queue, &delta.dir);
            }
        };
        // The product's own backends, plus the tag store as one of them: a search that names a tag is
        // answered by the library rather than by the filesystem, and its rows arrive in the same shape,
        // which is what lets the shell show a tag hit as an ordinary file row.
        let router = Arc::new(
            SearchRouter::from_config(&config)
                .with_provider(Box::new(TagSearch::new(Arc::clone(&store)))),
        );
        let searches = Searches::start(Arc::clone(&router));

        let pipeline = Arc::new(Mutex::new(
            spawn(SchedulerConfig::from_config(&config), Box::new(sink))
                .map_err(|error| error.to_string())?,
        ));
        let loader = Loader::start(
            receiver,
            index.clone(),
            v2.clone(),
            queue,
            pipeline,
            Arc::clone(&losses),
        );
        Ok(Self {
            config,
            store,
            losses,
            index,
            v2,
            router,
            searches,
            loader,
            watched: Mutex::new(HashMap::new()),
            audit,
            transfers: Mutex::new((0, HashMap::new())),
        })
    }

    /// Writes the one line a call owes. Everything about the *outcome* comes from the shell that
    /// called this, which is why no body has to remember to report anything: an entry point that
    /// forgets a field still leaves a line.
    fn record(&self, call: &Call, result: impl std::fmt::Display) {
        let mut draft = Draft::new(call.op);
        if let Some(target) = &call.target {
            draft = draft.target(target);
        }
        if let Some(args) = &call.args {
            draft = draft.args(args);
        }
        self.audit.record(draft.result(result));
    }

    /// Lock poisoning can only follow a panic that an entry point already caught, so recovering
    /// the guard keeps one bad call from wedging the front-end.
    fn store(&self) -> std::sync::MutexGuard<'_, TagStore> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Events discarded so far. Monotonic for the life of the core: nothing resets it, which is what
    /// lets a v2 view compare it against the baseline it was opened with.
    fn loss_count(&self) -> u64 {
        self.losses.load(Ordering::Relaxed)
    }

    fn stats(&self) -> Result<String, String> {
        let store = self.store();
        let stats = store.stats().map_err(|error| error.to_string())?;
        let out = StatsOut {
            database: self.config.database_path.display().to_string(),
            roots: self
                .config
                .search_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
            files: stats.files,
            tags: stats.tags,
            manual: stats.manual,
            auto: stats.auto,
            dropped_events: self.loss_count(),
            audit: {
                let health = self.audit.health();
                AuditOut {
                    directory: self.config.audit_dir.display().to_string(),
                    session: health.session,
                    records: health.records,
                    bytes_today: health.bytes_today,
                    dropped: health.dropped,
                    dropped_because: health.dropped_because,
                    writing: health.writing,
                    record_reads: self.config.audit_record_reads,
                    daily_limit_bytes: health.daily_limit_bytes,
                    dropped_total: health.dropped_total,
                    last_error: health.last_error,
                }
            },
        };
        serde_json::to_string(&out).map_err(|error| error.to_string())
    }

    fn tag_add(
        &self,
        path: &Path,
        tags_json: &str,
        source: &str,
        priority: i32,
    ) -> Result<String, String> {
        let names = parse_tags(tags_json)?;
        let source = Source::parse(source).map_err(|error| error.to_string())?;
        let entries: Vec<NewTag> = names
            .iter()
            .cloned()
            .map(|tag| {
                NewTag::manual(tag)
                    .with_source(source)
                    .with_priority(priority)
            })
            .collect();
        let store = self.store();
        let outcome = store
            .add_tags(path, &entries)
            .map_err(|error| error.to_string())?;
        Ok(format!(
            "{{\"added\":{},\"present\":{}}}",
            outcome.inserted, outcome.refreshed
        ))
    }

    /// Move or copy one path. The rules are not here - they live in `fam_svc_fs`, which the agent's
    /// MCP tools call through the same function - so a refusal the person reading a status line sees
    /// is word-for-word the refusal an agent gets.
    fn path_op(&self, from: &Path, to: &Path, copy: bool) -> Result<String, String> {
        let action = if copy {
            fam_svc_fs::Action::Copy
        } else {
            fam_svc_fs::Action::Move
        };
        let done = fam_svc_fs::transfer(from, to, action).map_err(|error| error.to_string())?;
        serde_json::to_string(&serde_json::json!({
            "from": done.from.display().to_string(),
            "to": done.to.display().to_string(),
            "action": done.action.name(),
            "by": done.by.name(),
            "source_gone": done.by != fam_svc_fs::By::Copied,
        }))
        .map_err(|error| error.to_string())
    }

    fn tag_list(&self, path: &Path) -> Result<String, String> {
        let store = self.store();
        let records = store
            .tags_of(path, None)
            .map_err(|error| error.to_string())?;
        let tags: Vec<TagOut> = records
            .iter()
            .map(|record| TagOut {
                tag: record.tag.clone(),
                source: record.source.to_string(),
                priority: record.priority,
                created_at: record.created_at.clone(),
            })
            .collect();
        serde_json::to_string(&tags).map_err(|error| error.to_string())
    }

    /// The tags of many files at once, keyed by the path the caller asked with.
    ///
    /// Re-keying on the way out is the point. `path_norm` is this library's folding of a path into an
    /// identity, and making the caller fold its own paths the same way to join the answer would move
    /// identity rules across the wire - the class of bug that turned up as a search hit whose path had
    /// been folded twice. So the caller gets a map whose keys are the strings it sent, and only for
    /// the files that actually carry a tag: an absent key means "no tags", which is what a listing
    /// needs to hear rather than an empty array per row.
    fn tags_for_paths(&self, paths_json: &str) -> Result<String, String> {
        let paths = parse_paths(paths_json)?;
        let store = self.store();
        let keys: Vec<String> = paths
            .iter()
            .map(|path| fam_core::path_key(Path::new(path)))
            .collect();
        let grouped = store
            .tag_names_for(&keys)
            .map_err(|error| error.to_string())?;
        let mut answered: HashMap<String, Vec<String>> = HashMap::new();
        for (path, key) in paths.into_iter().zip(keys) {
            if let Some(rows) = grouped.get(&key) {
                /* One chip per label, not one per row. `tag_names_for` answers with the rows in
                 * display order, and a file carrying the same name under both sources has two of
                 * them - right for the store, where the sources differ and only `tag.list` can say
                 * which is which, and wrong for a listing, where the same word twice in a row of
                 * chips reads as two facts. The vocabulary counts files for the same reason; these
                 * two reads have to agree about what "a tag on a file" means. */
                let mut names: Vec<String> = Vec::with_capacity(rows.len());
                for name in rows {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
                answered.insert(path, names);
            }
        }
        serde_json::to_string(&answered).map_err(|error| error.to_string())
    }

    /// Every tag name in the store and how many files carry it, most used first.
    fn tag_vocabulary(&self) -> Result<String, String> {
        let facets = self
            .store()
            .vocabulary()
            .map_err(|error| error.to_string())?;
        let out: Vec<VocabularyOut> = facets
            .into_iter()
            .map(|facet| VocabularyOut {
                tag: facet.tag,
                files: facet.files,
            })
            .collect();
        serde_json::to_string(&out).map_err(|error| error.to_string())
    }

    fn tag_remove(
        &self,
        path: &Path,
        tags_json: &str,
        source: Option<&str>,
    ) -> Result<String, String> {
        let names: Vec<String> = if tags_json.trim().is_empty() {
            Vec::new()
        } else {
            parse_tags(tags_json)?
        };
        let source = source
            .map(Source::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        let store = self.store();
        let removed = store
            .remove_tags(path, &names, source)
            .map_err(|error| error.to_string())?;
        Ok(format!("{{\"removed\":{removed}}}"))
    }

    /// Renames a label across the whole library, collapsing duplicates as it goes. This is the
    /// operation the store calls a merge, and it is deliberately one operation rather than two:
    /// "rename" is what a merge looks like when the target name has no rows yet, and a caller that
    /// wanted them split would have to keep the difference alive in every face.
    fn tag_merge(&self, from: &str, into: &str) -> Result<String, String> {
        let outcome = self
            .store()
            .merge_tags(from, into)
            .map_err(|error| error.to_string())?;
        Ok(format!(
            "{{\"moved\":{},\"collapsed\":{},\"files\":{}}}",
            outcome.moved, outcome.collapsed, outcome.files
        ))
    }

    /// Which of a window of paths carries a note - the batch read a row's "has a note" marker is
    /// drawn from, keyed by the caller's spelling and absent for paths with nothing. The shape is
    /// `fam_tags_for_paths`'s exactly, and for the same reason: the caller never reproduces
    /// `path_norm`. An empty window is an empty answer, which is what a shell asks while it opens
    /// an empty folder.
    fn notes_for_paths(&self, paths_json: &str) -> Result<String, String> {
        let paths = parse_paths(paths_json)?;
        let store = self.store();
        let keys: Vec<String> = paths
            .iter()
            .map(|path| fam_core::path_key(Path::new(path)))
            .collect();
        let noted = store.has_notes(&keys).map_err(|error| error.to_string())?;
        let mut answered: HashMap<String, bool> = HashMap::new();
        for (path, key) in paths.into_iter().zip(keys) {
            if noted.contains(&key) {
                answered.insert(path, true);
            }
        }
        serde_json::to_string(&answered).map_err(|error| error.to_string())
    }

    /// Starts one asynchronous transfer and answers with its id before a byte has moved. The
    /// validation the synchronous call did on entry (never overwrites, parent exists) now happens
    /// inside the job, because it touches the filesystem - a refusal a person used to read at drop
    /// time arrives at the next poll as a `failed` state with the same sentence.
    fn transfer_begin(&self, from: &Path, to: &Path, copy: bool) -> Result<String, String> {
        let action = if copy {
            fam_svc_fs::Action::Copy
        } else {
            fam_svc_fs::Action::Move
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(JobState::new()));
        let thread_state = Arc::clone(&state);
        let thread_cancel = Arc::clone(&cancel);
        let thread_from = from.to_path_buf();
        let thread_to = to.to_path_buf();
        std::thread::spawn(move || {
            let result =
                fam_svc_fs::transfer_with(&thread_from, &thread_to, action, &mut |report| {
                    if let Ok(mut state) = thread_state.lock() {
                        state.bytes = report.bytes;
                        state.total = report.total;
                    }
                    !thread_cancel.load(Ordering::Relaxed)
                });
            if let Ok(mut state) = thread_state.lock() {
                match result {
                    Ok(transferred) => {
                        state.phase = JobPhase::Done;
                        state.outcome = Some(transferred);
                    }
                    Err(fam_svc_fs::Error::Cancelled { .. }) => {
                        state.phase = JobPhase::Cancelled;
                    }
                    Err(error) => {
                        state.phase = JobPhase::Failed;
                        state.error = Some(error.to_string());
                    }
                }
            }
        });
        let mut registry = self
            .transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.0 += 1;
        let id = registry.0;
        registry.1.insert(
            id,
            TransferJob {
                cancel,
                state,
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                copy,
            },
        );
        Ok(format!("{{\"job\":{id}}}"))
    }

    /// What one transfer is doing now. The first poll to observe a terminal state writes the job's
    /// closing audit line and takes the entry out - so a terminal state is answered exactly once,
    /// and a registry that only holds running work is also the answer to "who cleans up".
    fn transfer_poll(&self, job: u64) -> Result<String, String> {
        let mut registry = self
            .transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = registry
            .1
            .get(&job)
            .ok_or_else(|| format!("unknown transfer job {job}"))?;
        let (phase, bytes, total, outcome, error) = {
            let state = entry
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                state.phase,
                state.bytes,
                state.total,
                state.outcome.clone(),
                state.error.clone(),
            )
        };
        if phase == JobPhase::Running {
            return Ok(format!(
                "{{\"state\":\"running\",\"bytes\":{bytes},\"total\":{total}}}"
            ));
        }
        let entry = registry.1.remove(&job).expect("checked above");
        let outcome_sentence = match (&phase, &outcome, &error) {
            (JobPhase::Done, Some(done), _) => format!("done by={} bytes={total}", done.by.name()),
            (JobPhase::Cancelled, ..) => format!("cancelled after {bytes} of {total} bytes"),
            (JobPhase::Failed, _, Some(reason)) => format!("failed: {reason}"),
            _ => "ended".to_string(),
        };
        self.audit.record(
            fam_svc_audit::Draft::new(fam_svc_audit::Op::PathTransferEnd)
                .target(&entry.from)
                .args(format!(
                    "to={} action={}",
                    entry.to.display(),
                    if entry.copy { "copy" } else { "move" }
                ))
                .result(outcome_sentence),
        );
        let by = outcome
            .as_ref()
            .map(|done| format!(",\"by\":\"{}\"", done.by.name()))
            .unwrap_or_default();
        let state_name = match phase {
            JobPhase::Done => "done",
            JobPhase::Cancelled => "cancelled",
            JobPhase::Failed => "failed",
            JobPhase::Running => unreachable!("handled above"),
        };
        let error_field = error
            .map(|reason| {
                format!(
                    ",\"error\":{}",
                    serde_json::to_string(&reason).unwrap_or_default()
                )
            })
            .unwrap_or_default();
        Ok(format!(
            "{{\"state\":\"{state_name}\",\"bytes\":{bytes},\"total\":{total}{by}{error_field}}}"
        ))
    }

    /// Asks one running transfer to stop. The answer says the ask was made, not that it was
    /// obeyed - the copy stops at its next chunk, and the poll says when.
    fn transfer_cancel(&self, job: u64) -> Result<String, String> {
        let registry = self
            .transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = registry
            .1
            .get(&job)
            .ok_or_else(|| format!("unknown transfer job {job}"))?;
        entry.cancel.store(true, Ordering::Relaxed);
        Ok("{\"cancelling\":true}".to_string())
    }

    /// The note on one path, or `{"body": null}` when there is none - the shape a dialog wants,
    /// where "empty" and "absent" are the same fact and neither is an error.
    /// Removes one path - files and empty directories only, a symlink as the link - by the one
    /// implementation the MCP's `fs_delete` answers through. The undo of a copied transfer is a
    /// deletion, and a deletion that only the agent's tool could make would be an undo the human
    /// side cannot run; the refusals are the crate's own sentences, word for word.
    fn path_delete(&self, path: &Path) -> Result<String, String> {
        let removed = fam_svc_fs::remove_path(path).map_err(|error| error.to_string())?;
        Ok(format!(
            "{{\"removed\":{},\"kind\":\"{}\"}}",
            serde_json::to_string(&removed.path.display().to_string())
                .map_err(|error| error.to_string())?,
            removed.kind
        ))
    }

    fn note_get(&self, path: &Path) -> Result<String, String> {
        let store = self.store();
        let found = store.note(path).map_err(|error| error.to_string())?;
        match found {
            Some(record) => serde_json::to_string(&serde_json::json!({
                "body": record.body,
                "updated_at": record.updated_at,
            }))
            .map_err(|error| error.to_string()),
            None => Ok("{\"body\":null}".to_string()),
        }
    }

    /// Writes the note, replacing whatever was there. The store's own rules arrive as their
    /// sentences: over-long is `note.set`'s refusal, and a blank body is refused rather than read
    /// as a clear - the caller has `note_clear` for that, and a tool that deletes when handed
    /// nothing is a tool that loses a note over one dropped argument.
    fn note_set(&self, path: &Path, body: &str) -> Result<String, String> {
        self.store()
            .set_note(path, body)
            .map_err(|error| error.to_string())?;
        Ok("{\"set\":true}".to_string())
    }

    fn note_clear(&self, path: &Path) -> Result<String, String> {
        let removed = self
            .store()
            .clear_note(path)
            .map_err(|error| error.to_string())?;
        Ok(format!("{{\"cleared\":{removed}}}"))
    }

    /// Renames one label on one file - the per-file operation a tag dialog offers, which keeps the
    /// file's other tags and the tag's other files exactly where they are. `to` or `source` may be
    /// absent to leave that field alone; both absent is refused, because a call that changes
    /// nothing is a bug wearing a request's clothes.
    fn tag_rename(
        &self,
        path: &Path,
        from: &str,
        to: Option<&str>,
        source: Option<&str>,
    ) -> Result<String, String> {
        let source = source
            .map(Source::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        self.store()
            .rename_tag(path, from, to, source)
            .map_err(|error| error.to_string())?;
        // The store has already refused a no-op and a miss; by here exactly one row changed.
        Ok("{\"renamed\":1}".to_string())
    }

    // ---- v2: directory views, bounded fetch, and the delta poll -----------------------------

    /// Opens a directory. The token comes back immediately; the rows arrive when the loader gets
    /// there, and [`fam_dir_state`] is how a shell finds out they have.
    fn dir_open(&self, path: &Path) -> u64 {
        let key = PathKey::of(path);
        let token = self.v2.open(&self.index, &key, self.loss_count());
        self.request_load(&key);
        self.watch_one_more(&key, path);
        token
    }

    /// Asks for a read when the cached rows, if any, cannot answer for the present.
    ///
    /// `claim` is the arbiter of everything else: a read already running, or a failure still cooling
    /// down, gets `None` and this posts nothing.
    fn request_load(&self, key: &PathKey) {
        if let Some(claim) = owed_read(&self.index, key) {
            self.loader.post(Job::Load(claim));
        }
    }

    /// The first view of a directory arms its watch, the last one disarms it.
    ///
    /// Counted here rather than left to `Handle`, which dedupes watches but knows nothing about how
    /// many views wanted one. Only directories someone is looking at get watched at all: one costs
    /// ~900us on this machine, so arming one for every cached directory would cost more than reading
    /// it (`docs/notes/watch-sweep-costs-a-path-lookup.md`).
    fn watch_one_more(&self, key: &PathKey, path: &Path) {
        let mut counts = self.watched();
        let entry = counts.entry(key.clone()).or_insert(0);
        *entry += 1;
        if *entry == 1 {
            self.loader.post(Job::Watch(path.to_path_buf()));
        }
    }

    fn unwatch_one_less(&self, key: &PathKey) {
        let mut counts = self.watched();
        let Some(entry) = counts.get_mut(key) else {
            return;
        };
        *entry = entry.saturating_sub(1);
        if *entry > 0 {
            return;
        }
        counts.remove(key);
        // A watch's last view is a `PathKey`, and `Handle::unwatch` wants the path it was given.
        self.loader.post(Job::Unwatch(key.to_path_buf()));
    }

    fn dir_status(&self, token: u64) -> Result<wire::FamRowHeader, String> {
        let status = self
            .v2
            .status(&self.index, token, self.loss_count())
            .ok_or_else(|| format!("unknown view token {token}"))?;
        let mut flags = 0u8;
        if status.resync {
            flags |= wire::FLAG_RESYNC;
        }
        if status.loss {
            flags |= wire::FLAG_LOSS;
        }
        Ok(wire::status(
            status.state,
            status.total_rows,
            status.generation,
            status.seq,
            flags,
        ))
    }

    /// Copies one window of rows into the caller's buffer, whole rows or none.
    fn dir_fetch(
        &self,
        token: u64,
        first: u32,
        count: u32,
        buffer: &mut [u8],
    ) -> Result<usize, String> {
        let mut header = self.dir_status(token)?;
        let asked = (count as usize).min(MAX_ROWS_PER_CALL);
        // The window is read under the view's lock and encoded outside it: the copy into the
        // caller's buffer works off owned values, so no lock is held across it.
        let (window, visible) = {
            let view = self
                .v2
                .view(token)
                .ok_or_else(|| format!("unknown view token {token}"))?;
            let guard = view.lock().expect("view lock");
            match guard.window(first as usize, asked) {
                Some((rows, total)) => (rows, total),
                // Nothing to show yet; the header's state says whether that is a read, a failure, or
                // no request at all.
                None => return Ok(wire::encode(&[], wire::RECORDS, &[], header, buffer)),
            }
        };

        let mut names = Vec::new();
        let mut items = Vec::with_capacity(window.len());
        for row in window {
            let mut item = row.item;
            // The record's name fields address the blob at the end of this buffer, which is the wire
            // form of the arena they addressed in the table.
            item.name_off = names.len() as u32;
            item.name_units = row.name.len() as u16;
            names.extend_from_slice(&row.name);
            items.push(item);
        }
        // Rows remain past this window, so the caller has a next fetch to make.
        if (first as usize) + items.len() < visible as usize {
            header.flags |= wire::FLAG_TRUNCATED;
        }
        Ok(wire::encode(
            &wire::record_block(&items),
            wire::RECORDS,
            &names,
            header,
            buffer,
        ))
    }

    /// Hands over the row positions queued since the last poll.
    fn dir_poll(&self, token: u64, buffer: &mut [u8]) -> Result<usize, String> {
        let mut header = self.dir_status(token)?;
        let view = self
            .v2
            .view(token)
            .ok_or_else(|| format!("unknown view token {token}"))?;
        // A resync is reported by this poll and by no later one; the flag a status call shows is the
        // caller's cue to fetch, so it is recomputed from what the take just did.
        // Room for the cuts this buffer can hold, so a burst is drained over polls rather than
        // truncated into the bin.
        let fitting = (buffer.len() / core::mem::size_of::<wire::FamCut>()).saturating_sub(1);
        let (cuts, resync) = {
            let mut view = view.lock().expect("view lock");
            let taken = view.take_cuts(fitting);
            // The sequence the caller compares against next time, read after the take.
            header.seq = view.seq();
            taken
        };
        if resync {
            header.flags |= wire::FLAG_RESYNC;
        } else {
            header.flags &= !wire::FLAG_RESYNC;
            if !cuts.is_empty() && view.lock().expect("view lock").pending_cuts() > 0 {
                // More cuts are queued than this buffer could take.
                header.flags |= wire::FLAG_TRUNCATED;
            }
        }
        Ok(wire::encode(
            &wire::cut_block(&cuts),
            wire::CUTS,
            &[],
            header,
            buffer,
        ))
    }

    fn dir_set_sort(&self, token: u64, sort: &str, order: &str) -> Result<(), String> {
        let sort = parse_sort(sort)?;
        let order = parse_order(order)?;
        let view = self
            .v2
            .view(token)
            .ok_or_else(|| format!("unknown view token {token}"))?;
        view.lock()
            .expect("view lock")
            .reorder(|rows| rows.set_sort(sort, order));
        Ok(())
    }

    fn dir_set_filter(
        &self,
        token: u64,
        terms_json: &str,
        kind: Option<&str>,
    ) -> Result<(), String> {
        let terms: Vec<String> = if terms_json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(terms_json)
                .map_err(|error| format!("expected a JSON array of search terms: {error}"))?
        };
        let kind = kind.map(parse_kind).transpose()?;
        let view = self
            .v2
            .view(token)
            .ok_or_else(|| format!("unknown view token {token}"))?;
        view.lock()
            .expect("view lock")
            .reorder(|rows| rows.set_filter(terms, kind));
        Ok(())
    }

    fn dir_close(&self, token: u64) -> bool {
        match self.v2.close(token) {
            Some(key) => {
                self.unwatch_one_less(&key);
                true
            }
            None => false,
        }
    }

    /// What the core can tell a caller about itself, as JSON: which search backends exist and are
    /// usable now, and the vocabulary of the `kind` byte a v2 record carries.
    ///
    /// The kinds are in declaration order, which is the index the wire sends. A shell that hardcoded
    /// those names would start labelling rows wrong the day a bucket is inserted, so the list is part
    /// of the ABI rather than a comment.
    fn backends_json(&self) -> String {
        serde_json::to_string(&serde_json::json!({
            "backends": self.router.ids(),
            "available": self.router.available(),
            "kinds": fam_core::KIND_ORDER
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
        }))
        .unwrap_or_else(|_| "{\"backends\":[],\"available\":[],\"kinds\":[]}".into())
    }

    fn watched(&self) -> MutexGuard<'_, HashMap<PathKey, u32>> {
        self.watched
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- v2: paged name search -------------------------------------------------------------

    /// Starts a search and hands back its token without waiting. Rows appear as the provider finds
    /// them; the header's state says when they are final.
    ///
    /// `limit` is how many hits to accumulate: 0 asks for [`DEFAULT_SEARCH_LIMIT`], and anything
    /// larger is clamped to [`MAX_SEARCH_LIMIT`]. There is deliberately no "no ceiling" spelling -
    /// the rows live in this process until the caller closes the token, and an unbounded walk of
    /// `D:\` is an unbounded allocation only a cancel can interrupt.
    fn search_begin(
        &self,
        query: &str,
        within: Option<&Path>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<u64, String> {
        let mut request = NameRequest::new(query);
        if request.is_empty() {
            return Err("query carries no search terms".into());
        }
        if let Some(kind) = kind.map(parse_kind).transpose()? {
            request = request.kind(kind);
        }
        if let Some(within) = within {
            request = request.scope(within);
        }
        let ceiling = if limit == 0 {
            DEFAULT_SEARCH_LIMIT
        } else {
            limit.min(MAX_SEARCH_LIMIT)
        };
        Ok(self.searches.begin(request.limit(ceiling as usize)))
    }

    fn search_page(
        &self,
        token: u64,
        first: u32,
        count: u32,
        buffer: &mut [u8],
    ) -> Result<usize, String> {
        let asked = (count as usize).min(MAX_ROWS_PER_CALL);
        let Some(page) = self.searches.page(token, first as usize, asked) else {
            return Err(format!("unknown search token {token}"));
        };
        let mut head = page.head;
        let mut names = Vec::new();
        let mut items = Vec::with_capacity(page.rows.len());
        for (mut item, path) in page.rows {
            item.name_off = names.len() as u32;
            item.name_units = path.len() as u16;
            names.extend_from_slice(&path);
            items.push(item);
        }
        // Only a request that asked for rows can be told it got fewer of them. `count` 0 is the
        // cheap way to poll a search's state, and a flag on that would read as "rows were dropped
        // from the buffer you asked for" - and make the shell retry for a bigger buffer every poll.
        if count > 0 && (first as usize) + items.len() < head.total_rows as usize {
            head.flags |= wire::FLAG_TRUNCATED;
        }
        Ok(wire::encode(
            &wire::record_block(&items),
            wire::RECORDS,
            &names,
            head,
            buffer,
        ))
    }

    /// Stops a search. Its rows stay pageable - what was found before the stop is still true, and
    /// the header keeps saying the scope was not covered.
    fn search_cancel(&self, token: u64) -> Result<(), String> {
        if self.searches.cancel(token) {
            Ok(())
        } else {
            Err(format!("unknown search token {token}"))
        }
    }

    /// `false` for a token this core never handed out, or has already released.
    fn search_close(&self, token: u64) -> bool {
        self.searches.close(token)
    }
}

fn parse_tags(json: &str) -> Result<Vec<String>, String> {
    let tags: Vec<String> = serde_json::from_str(json)
        .map_err(|error| format!("expected a JSON array of tags: {error}"))?;
    if tags.is_empty() {
        return Err("no tags given".into());
    }
    Ok(tags)
}

/// A JSON array of paths. Unlike `parse_tags`, an empty array is an answer and not a mistake: a
/// window of a listing with nothing in it is a normal thing for the shell to ask about, and making it
/// an error would push a special case into every caller of a read that costs nothing anyway.
fn parse_paths(json: &str) -> Result<Vec<String>, String> {
    let paths: Vec<String> = serde_json::from_str(json)
        .map_err(|error| format!("expected a JSON array of paths: {error}"))?;
    Ok(paths)
}

fn parse_sort(value: &str) -> Result<SortKey, String> {
    match value {
        "name" | "" => Ok(SortKey::Name),
        "size" => Ok(SortKey::Size),
        "modified" => Ok(SortKey::Modified),
        "kind" => Ok(SortKey::Kind),
        other => Err(format!("unknown sort key {other:?}")),
    }
}

fn parse_order(value: &str) -> Result<Order, String> {
    match value {
        "ascending" | "" => Ok(Order::Ascending),
        "descending" => Ok(Order::Descending),
        other => Err(format!("unknown order {other:?}")),
    }
}

fn parse_kind(value: &str) -> Result<Kind, String> {
    Kind::parse(value).ok_or_else(|| format!("unknown kind {value:?}"))
}

fn write_cstring(target: *mut *mut c_char, value: impl Into<String>) {
    if target.is_null() {
        return;
    }
    let encoded = CString::new(value.into()).unwrap_or_else(|_| {
        CString::new("{\"error\":\"result contained a NUL byte\"}").expect("static text")
    });
    unsafe { *target = encoded.into_raw() };
}

unsafe fn arg_str<'a>(pointer: *const c_char) -> Result<Option<&'a str>, String> {
    if pointer.is_null() {
        return Ok(None);
    }
    CStr::from_ptr(pointer)
        .to_str()
        .map(Some)
        .map_err(|_| "argument is not valid UTF-8".to_string())
}

unsafe fn required_str<'a>(pointer: *const c_char, name: &str) -> Result<&'a str, String> {
    arg_str(pointer)?.ok_or_else(|| format!("{name} must not be null"))
}

unsafe fn borrow_core<'a>(pointer: *mut FamCore) -> Result<&'a FamCore, String> {
    if pointer.is_null() {
        return Err("core handle is null".into());
    }
    Ok(&*pointer)
}

/// What a caller sees when a body unwound, and what the audit line says about it.
const PANIC: &str = "FAM core panicked; the call was aborted";

/* The four shells below share one contract: a request leaves exactly one audit line, whatever the
 * body does with it - an answer, a refusal, or a panic. That is why the line is written here rather
 * than in the bodies: a method that forgets to report would otherwise forget to record as well.
 *
 * A call that never reaches a core handle - a null pointer, or a buffer too small to name - records
 * nothing, because the log belongs to the handle. Both are caller bugs rather than events in anyone's
 * browsing, and the ABI contract in the header is what addresses them. */

/// The shape of every call that answers with JSON.
unsafe fn with_core<F>(
    core: *mut FamCore,
    mut call: Call,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
    body: F,
) -> c_int
where
    F: FnOnce(&FamCore, &mut Call) -> Result<String, String>,
{
    match borrow_core(core) {
        Err(message) => {
            write_cstring(err, message);
            ERR
        }
        Ok(borrow) => match catch_unwind(AssertUnwindSafe(|| body(borrow, &mut call))) {
            Ok(Ok(json)) => {
                borrow.record(&call, json.as_str());
                write_cstring(out, json);
                OK
            }
            Ok(Err(message)) => {
                borrow.record(&call, format!("error {message}"));
                write_cstring(err, message);
                ERR
            }
            Err(_) => {
                borrow.record(&call, "panic");
                write_cstring(err, PANIC);
                ERR
            }
        },
    }
}

/// Same shape for calls whose useful return value is a token; `0` signals failure.
unsafe fn with_core_token<F>(
    core: *mut FamCore,
    mut call: Call,
    err: *mut *mut c_char,
    body: F,
) -> u64
where
    F: FnOnce(&FamCore, &mut Call) -> Result<u64, String>,
{
    match borrow_core(core) {
        Err(message) => {
            write_cstring(err, message);
            0
        }
        Ok(borrow) => match catch_unwind(AssertUnwindSafe(|| body(borrow, &mut call))) {
            Ok(Ok(token)) => {
                borrow.record(&call, format!("ok token={token}"));
                token
            }
            Ok(Err(message)) => {
                borrow.record(&call, format!("error {message}"));
                write_cstring(err, message);
                0
            }
            Err(_) => {
                borrow.record(&call, "panic");
                write_cstring(err, PANIC);
                0
            }
        },
    }
}

/// For calls that only need to succeed or fail.
unsafe fn with_core_status<F>(
    core: *mut FamCore,
    mut call: Call,
    err: *mut *mut c_char,
    body: F,
) -> c_int
where
    F: FnOnce(&FamCore, &mut Call) -> Result<(), String>,
{
    match borrow_core(core) {
        Err(message) => {
            write_cstring(err, message);
            ERR
        }
        Ok(borrow) => match catch_unwind(AssertUnwindSafe(|| body(borrow, &mut call))) {
            Ok(Ok(())) => {
                borrow.record(&call, "ok");
                OK
            }
            Ok(Err(message)) => {
                borrow.record(&call, format!("error {message}"));
                write_cstring(err, message);
                ERR
            }
            Err(_) => {
                borrow.record(&call, "panic");
                write_cstring(err, PANIC);
                ERR
            }
        },
    }
}

/// The shape of the two calls that release something and answer nothing.
///
/// They have no `err` slot in their signatures, so the audit line is the only record either one
/// leaves - which is the reason the line is written from what actually happened rather than flatly
/// `ok`: a close that found no token by that name has to read differently from one that released a
/// view, or the trail would claim releases that never occurred. It is also why the body runs under
/// `catch_unwind` like the other three shapes, rather than being the only two entry points that
/// could unwind straight into C++ frames.
unsafe fn with_core_void<F>(core: *mut FamCore, mut call: Call, body: F)
where
    F: FnOnce(&FamCore, &mut Call) -> Result<bool, String>,
{
    let Ok(borrow) = borrow_core(core) else {
        return;
    };
    match catch_unwind(AssertUnwindSafe(|| body(borrow, &mut call))) {
        Ok(Ok(true)) => borrow.record(&call, "ok"),
        Ok(Ok(false)) => borrow.record(&call, "error nothing was held by that token"),
        Ok(Err(message)) => borrow.record(&call, format!("error {message}")),
        Err(_) => borrow.record(&call, "panic"),
    }
}

/// Borrows a caller-supplied buffer. Length is checked before a single byte is written, and a null
/// pointer with a non-zero length is a bug in the caller rather than something to survive.
unsafe fn buffer_slice<'a>(pointer: *mut c_uchar, len: u32) -> Result<&'a mut [u8], String> {
    if len == 0 {
        return Err("buffer_len must be greater than zero".into());
    }
    if pointer.is_null() {
        return Err("buffer must not be null".into());
    }
    Ok(unsafe { std::slice::from_raw_parts_mut(pointer, len as usize) })
}

/// The shape of every row-carrying v2 call: bytes written, or -1 with a message in `err`.
unsafe fn with_core_buffer<F>(
    core: *mut FamCore,
    mut call: Call,
    buffer: *mut c_uchar,
    buffer_len: u32,
    err: *mut *mut c_char,
    body: F,
) -> c_int
where
    F: FnOnce(&FamCore, &mut Call, &mut [u8]) -> Result<usize, String>,
{
    let bytes = match buffer_slice(buffer, buffer_len) {
        Ok(bytes) => bytes,
        Err(message) => {
            write_cstring(err, message);
            return ERR;
        }
    };
    match borrow_core(core) {
        Err(message) => {
            write_cstring(err, message);
            ERR
        }
        Ok(borrow) => {
            // No `move` on purpose: the body takes the call by mutable reference so the line can
            // still be written from it afterwards, once the arguments have been read exactly once.
            match catch_unwind(AssertUnwindSafe(|| body(borrow, &mut call, bytes))) {
                // A buffer larger than `c_int` cannot be described by this signature, and no view
                // is asked for one: `MAX_ROWS_PER_CALL` records is well under half of what fits.
                Ok(Ok(written)) => {
                    borrow.record(&call, format!("ok {written}B"));
                    written.min(c_int::MAX as usize) as c_int
                }
                Ok(Err(message)) => {
                    borrow.record(&call, format!("error {message}"));
                    write_cstring(err, message);
                    ERR
                }
                Err(_) => {
                    borrow.record(&call, "panic");
                    write_cstring(err, PANIC);
                    ERR
                }
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn fam_core_new(
    config_path: *const c_char,
    err: *mut *mut c_char,
) -> *mut FamCore {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let path = arg_str(config_path)?;
        FamCore::create(path.map(Path::new))
    }));
    match outcome {
        Ok(Ok(core)) => Box::into_raw(Box::new(core)),
        Ok(Err(message)) => {
            write_cstring(err, message);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_cstring(err, "FAM core panicked while starting");
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn fam_core_free(core: *mut FamCore) {
    if core.is_null() {
        return;
    }
    let (loader, searches) = {
        let borrow = unsafe { &*core };
        // The loader thread holds the last clone of the pipeline, so it has to be joined before that
        // Handle is dropped - and `Handle::drop` is what stops the watcher and joins the consumer.
        (borrow.loader.clone(), borrow.searches.clone())
    };
    searches.shutdown();
    loader.shutdown();
    drop(Box::from_raw(core));
}

/// Releases a string returned by any other call. Null is allowed.
#[no_mangle]
pub unsafe extern "C" fn fam_free_string(value: *mut c_char) {
    if !value.is_null() {
        drop(CString::from_raw(value));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fam_stats(
    core: *mut FamCore,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::Stats), out, err, |borrow, _| {
        borrow.stats()
    })
}

#[no_mangle]
pub unsafe extern "C" fn fam_tag_add(
    core: *mut FamCore,
    path: *const c_char,
    tags_json: *const c_char,
    source: *const c_char,
    priority: i32,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagAdd), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        let tags = required_str(tags_json, "tags")?;
        let source = arg_str(source)?.unwrap_or("manual");
        call.names(path)
            .says(format!("tags={tags} source={source} priority={priority}"));
        borrow.tag_add(Path::new(path), tags, source, priority)
    })
}

/// With this bit set, `fam_path_op` copies instead of moving: the source stays where it is.
/// No other bits mean anything yet, and an unknown bit is ignored rather than refused - the shell and
/// the library are built together, and a flag nobody has defined yet should not break a call that
/// asked for something it does understand.
pub const FAM_PATH_COPY: u32 = 1;

/// The first entry point on this surface that changes what is on disk.
///
/// `from` and `to` are both full paths - the caller composes the destination name, because only the
/// caller knows whether the person meant "into this folder" or "rename it to this". The answer is a
/// JSON object naming what happened and *how* (`"by": "renamed"`, `"copied-then-removed"`,
/// `"copied"`); a refusal comes back in `err` with the rule that refused it.
///
/// This does not widen what an agent can reach: the MCP server has had `fs_move` since increment 12,
/// behind a credential and a four-question gate. What changed is that the *human's* shell can now
/// move a file without anyone adding a write path to the credential layer - which is what a person
/// dragging a file in their own file manager always was.
#[no_mangle]
pub unsafe extern "C" fn fam_path_op(
    core: *mut FamCore,
    from: *const c_char,
    to: *const c_char,
    flags: u32,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    let copy = flags & FAM_PATH_COPY != 0;
    with_core(
        core,
        Call::new(if copy { Op::PathCopy } else { Op::PathMove }),
        out,
        err,
        move |borrow, call| {
            let from = required_str(from, "from")?;
            let to = required_str(to, "to")?;
            call.names(from);
            call.says(format!("to={to}"));
            borrow.path_op(Path::new(from), Path::new(to), copy)
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn fam_tag_list(
    core: *mut FamCore,
    path: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagList), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        borrow.tag_list(Path::new(path))
    })
}

/// The tags of a window of a listing, in one call.
///
/// `paths_json` is a JSON array of paths as the caller spells them; the answer is a JSON object
/// keyed by those same strings, holding only the files that carry a tag. An empty array is a legal
/// question with an empty answer.
///
/// This exists because the alternative is one `fam_tag_list` per row, and a screenful is a hundred
/// and twenty-eight of them - measured at 7.5ms of store time per window against 1.2ms for this call.
/// The number is not the reason it is a heartbeat, though: the reason is that it happens on the
/// shell's poll beat for as long as a folder is on screen, so a line each would be thousands per
/// session and would date nothing. `Op::TagFetch` is `Weight::Poll` for exactly that, and
/// `writer.rs` drops a Poll before it reaches a file - which is also why this call carries no `args`
/// summary: there is no line for it to appear on.
#[no_mangle]
pub unsafe extern "C" fn fam_tags_for_paths(
    core: *mut FamCore,
    paths_json: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagFetch), out, err, |borrow, _| {
        let paths_json = required_str(paths_json, "paths")?;
        borrow.tags_for_paths(paths_json)
    })
}

/// Every tag name in the store with how many files carry it, most used first - the vocabulary a
/// filter bar is drawn from.
///
/// Not scoped to a directory, which is what makes it the largest read on this surface, and why it is
/// a `Read` rather than a heartbeat: `record_reads` decides whether it leaves a line, and the shell
/// asks it once at start-up and again whenever it changes a tag itself.
#[no_mangle]
pub unsafe extern "C" fn fam_tag_vocabulary(
    core: *mut FamCore,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(
        core,
        Call::new(Op::TagVocabulary),
        out,
        err,
        |borrow, call| {
            call.says("whole table");
            borrow.tag_vocabulary()
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn fam_tag_remove(
    core: *mut FamCore,
    path: *const c_char,
    tags_json: *const c_char,
    source: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagRemove), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        let tags = arg_str(tags_json)?.unwrap_or("");
        let source = arg_str(source)?;
        call.names(path)
            .says(format!("tags={tags} source={}", source.unwrap_or("-")));
        borrow.tag_remove(Path::new(path), tags, source)
    })
}

/// Renames a label **everywhere** - both sources, every file - collapsing the rows that land on an
/// identity the target already holds. One call, because a rename and a merge are the same operation
/// seen from two distances: with nothing yet named `into` this is a rename, and with rows already
/// there it is the merge that turns `pdf` and `PDF` back into one name. The answer says which
/// happened: `moved` rows changed their name, `collapsed` died as duplicates, `files` counts what
/// carries the target name now.
#[no_mangle]
pub unsafe extern "C" fn fam_tag_merge(
    core: *mut FamCore,
    from: *const c_char,
    into: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagMerge), out, err, |borrow, call| {
        let from = required_str(from, "from")?;
        let into = required_str(into, "into")?;
        call.says(format!("from={from} into={into}"));
        borrow.tag_merge(from, into)
    })
}

/// Renames one label on one file. `to` and `source` are each optional; passing neither is refused -
/// the store answers `nothing to change`, which is the sentence a call that would do nothing
/// deserves.
#[no_mangle]
pub unsafe extern "C" fn fam_tag_rename(
    core: *mut FamCore,
    path: *const c_char,
    from: *const c_char,
    to: *const c_char,
    source: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagRename), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        let from = required_str(from, "from")?;
        let to = arg_str(to)?;
        let source = arg_str(source)?;
        call.names(path).says(format!(
            "from={from} to={} source={}",
            to.unwrap_or("-"),
            source.unwrap_or("-")
        ));
        borrow.tag_rename(Path::new(path), from, to, source)
    })
}

/// Which paths of a window carry a note: `{"<path>":true}` for the ones that do, keys spelled as
/// the caller spelled them, absent keys meaning "no note" - the twin of `fam_tags_for_paths`, and
/// a heartbeat like it: a listing asks this of its window on the shell's poll beat, so
/// `note.fetch` never reaches the audit trail, and an empty window is an empty answer rather than
/// an error.
#[no_mangle]
pub unsafe extern "C" fn fam_notes_for_paths(
    core: *mut FamCore,
    paths_json: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::NoteFetch), out, err, |borrow, _| {
        let paths_json = required_str(paths_json, "paths")?;
        borrow.notes_for_paths(paths_json)
    })
}

/// [`fam_tag_vocabulary`] scoped to one directory: tags whose files live in the subtree,
/// counted over the files in it. A browsing page shows "the tags of where I am" - a view that
/// changes with every descent, which is why this is a heartbeat-family read like the windowed
/// tag fetch, not a whole-table ask.
#[no_mangle]
pub unsafe extern "C" fn fam_tag_vocabulary_within(
    core: *mut FamCore,
    path: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagFetch), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        let facets = borrow
            .store()
            .vocabulary_within(Path::new(path))
            .map_err(|error| error.to_string())?;
        let out_json: Vec<VocabularyOut> = facets
            .into_iter()
            .map(|facet| VocabularyOut {
                tag: facet.tag,
                files: facet.files,
            })
            .collect();
        serde_json::to_string(&out_json).map_err(|error| error.to_string())
    })
}

/// Removes every row named `tag` across all files and both sources. The tag manager's delete:
/// a name that is wrong or duplicated everywhere is edited here rather than file by file. The
/// answer counts the rows that went - zero says the name was not in the library, which the
/// caller reports instead of celebrating.
#[no_mangle]
pub unsafe extern "C" fn fam_tag_delete(
    core: *mut FamCore,
    name: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagDelete), out, err, |borrow, call| {
        let name = required_str(name, "name")?;
        call.says(format!("name={name}"));
        let removed = borrow
            .store()
            .delete_tags(name)
            .map_err(|error| error.to_string())?;
        Ok(format!("{{\"removed\":{removed}}}"))
    })
}

/// Removes one path - a file, an empty directory, or a symlink as the link itself. A directory
/// that still has entries is refused by name; the recursive delete is the one irreversible call
/// there is, and this surface does not make it quietly. This is the inverse half of a copied
/// transfer: the undo the shell runs is this entry, audited as `path.delete` like any other act.
#[no_mangle]
pub unsafe extern "C" fn fam_path_delete(
    core: *mut FamCore,
    path: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::PathDelete), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        borrow.path_delete(Path::new(path))
    })
}

/// Starts one asynchronous transfer and answers `{"job":N}` before a byte has moved. The flags are
/// `fam_path_op`'s (`FAM_PATH_COPY`, no other bit defined); the validations that the synchronous
/// call made on entry now happen inside the job, so a refusal arrives at poll time as
/// `{"state":"failed","error":...}` with the same sentence it always had. Two audit lines per
/// transfer: this call writes `path.transfer.begin`, and the poll that first sees the end writes
/// `path.transfer.end` with the outcome.
#[no_mangle]
pub unsafe extern "C" fn fam_transfer_begin(
    core: *mut FamCore,
    from: *const c_char,
    to: *const c_char,
    flags: u32,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    let copy = flags & FAM_PATH_COPY != 0;
    with_core(
        core,
        Call::new(Op::PathTransferBegin),
        out,
        err,
        move |borrow, call| {
            let from = required_str(from, "from")?;
            let to = required_str(to, "to")?;
            call.names(from).says(format!(
                "to={to} action={} async",
                if copy { "copy" } else { "move" }
            ));
            borrow.transfer_begin(Path::new(from), Path::new(to), copy)
        },
    )
}

/// What one transfer is doing. The terminal state is answered exactly once - the poll that observes
/// it writes `path.transfer.end` and retires the job, and a poll past that point says
/// `unknown transfer job N`. `bytes`/`total` ride every answer while the job is known.
#[no_mangle]
pub unsafe extern "C" fn fam_transfer_poll(
    core: *mut FamCore,
    job: u64,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::DirPoll), out, err, |borrow, _| {
        borrow.transfer_poll(job)
    })
}

/// Asks one transfer to stop at its next chunk. The answer says the ask was made, not that it was
/// obeyed - the poll says when, with `{"state":"cancelled"}` and the partial removed.
#[no_mangle]
pub unsafe extern "C" fn fam_transfer_cancel(
    core: *mut FamCore,
    job: u64,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(
        core,
        Call::new(Op::PathTransferBegin),
        out,
        err,
        |borrow, _| borrow.transfer_cancel(job),
    )
}

/// The free-text note beside one path - or `{"body":null}` when there is none. A note is not a tag
/// and never enters the directory index, so this call on the exact path is the only way to read one
/// back; that is what keeps "what did anyone see" answerable from the audit trail alone.
#[no_mangle]
pub unsafe extern "C" fn fam_note_get(
    core: *mut FamCore,
    path: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::NoteGet), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        borrow.note_get(Path::new(path))
    })
}

/// Writes the note on a path, replacing whatever was there. Blank is refused - `fam_note_clear`
/// exists, and the store refuses to read an empty body as "delete it for me".
#[no_mangle]
pub unsafe extern "C" fn fam_note_set(
    core: *mut FamCore,
    path: *const c_char,
    body: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::NoteSet), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        let body = required_str(body, "body")?;
        call.names(path).says(format!("bytes={}", body.len()));
        borrow.note_set(Path::new(path), body)
    })
}

/// Drops the note. The answer says whether there was one, so "cleared" and "there was nothing"
/// stay two facts.
#[no_mangle]
pub unsafe extern "C" fn fam_note_clear(
    core: *mut FamCore,
    path: *const c_char,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::NoteClear), out, err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        borrow.note_clear(Path::new(path))
    })
}

/// Lets a shell refuse to run against a stale core library.
#[no_mangle]
pub extern "C" fn fam_abi_version() -> u32 {
    wire::ABI as u32
}

/// Starts a view of one directory and hands back its token, without waiting for the rows.
///
/// A folder open is a hash lookup plus two queue posts: the read and the watch both happen on the
/// loader thread. `fam_dir_state` says when the rows are there.
#[no_mangle]
pub unsafe extern "C" fn fam_open_dir(
    core: *mut FamCore,
    path: *const c_char,
    err: *mut *mut c_char,
) -> u64 {
    with_core_token(core, Call::new(Op::DirOpen), err, |borrow, call| {
        let path = required_str(path, "path")?;
        call.names(path);
        Ok(borrow.dir_open(Path::new(path)))
    })
}

/// Writes a header and no rows: state, row count, generation and sequence.
#[no_mangle]
pub unsafe extern "C" fn fam_dir_state(
    core: *mut FamCore,
    token: u64,
    buffer: *mut c_uchar,
    buffer_len: u32,
    err: *mut *mut c_char,
) -> c_int {
    with_core_buffer(
        core,
        Call::new(Op::DirState).with_token(token),
        buffer,
        buffer_len,
        err,
        |borrow, _, bytes| {
            let header = borrow.dir_status(token)?;
            Ok(wire::encode(&[], wire::RECORDS, &[], header, bytes))
        },
    )
}

/// Copies up to `count` rows from `first` into the caller's buffer.
///
/// Returns bytes written, or -1. A short buffer is not an error: the header comes back with
/// `row_count` 0 and the truncated flag, and the caller retries with room for what it asked for.
#[no_mangle]
pub unsafe extern "C" fn fam_dir_fetch(
    core: *mut FamCore,
    token: u64,
    first: u32,
    count: u32,
    buffer: *mut c_uchar,
    buffer_len: u32,
    err: *mut *mut c_char,
) -> c_int {
    let mut call = Call::new(Op::DirFetch).with_token(token);
    call.says(format!("{first}+{count}"));
    with_core_buffer(
        core,
        call,
        buffer,
        buffer_len,
        err,
        move |borrow, _, bytes| borrow.dir_fetch(token, first, count, bytes),
    )
}

/// Hands over the row positions queued since the last poll of this token.
///
/// Positions are to be applied in the order returned, each against the rows the earlier cuts left
/// behind. When the header carries the resync flag there are none, because something changed that
/// positions cannot express - a re-read, a new sort - and the model has to be rebuilt from a fetch.
#[no_mangle]
pub unsafe extern "C" fn fam_dir_poll(
    core: *mut FamCore,
    token: u64,
    buffer: *mut c_uchar,
    buffer_len: u32,
    err: *mut *mut c_char,
) -> c_int {
    with_core_buffer(
        core,
        Call::new(Op::DirPoll).with_token(token),
        buffer,
        buffer_len,
        err,
        |borrow, _, bytes| borrow.dir_poll(token, bytes),
    )
}

#[no_mangle]
pub unsafe extern "C" fn fam_dir_set_sort(
    core: *mut FamCore,
    token: u64,
    sort: *const c_char,
    order: *const c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core_status(
        core,
        Call::new(Op::DirSort).with_token(token),
        err,
        |borrow, call| {
            let sort = arg_str(sort)?.unwrap_or("");
            let order = arg_str(order)?.unwrap_or("");
            call.says(format!("sort={sort} order={order}"));
            borrow.dir_set_sort(token, sort, order)
        },
    )
}

/// Narrows a view. `terms_json` is a JSON array of lower-cased substrings, or empty for no filter.
#[no_mangle]
pub unsafe extern "C" fn fam_dir_set_filter(
    core: *mut FamCore,
    token: u64,
    terms_json: *const c_char,
    kind: *const c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core_status(
        core,
        Call::new(Op::DirFilter).with_token(token),
        err,
        |borrow, call| {
            let terms = arg_str(terms_json)?.unwrap_or("");
            let kind = arg_str(kind)?;
            call.says(format!("terms={terms} kind={}", kind.unwrap_or("-")));
            borrow.dir_set_filter(token, terms, kind)
        },
    )
}

/// Ends a view. The directory stops being watched when this was the last view of it.
#[no_mangle]
pub unsafe extern "C" fn fam_dir_close(core: *mut FamCore, token: u64) {
    with_core_void(
        core,
        Call::new(Op::DirClose).with_token(token),
        |borrow, _| Ok(borrow.dir_close(token)),
    );
}

/// Which search backends exist and which are usable now, as JSON.
#[no_mangle]
pub unsafe extern "C" fn fam_backends_json(
    core: *mut FamCore,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::Backends), out, err, move |borrow, _| {
        Ok(borrow.backends_json())
    })
}

/// Starts a name search and returns its token, without waiting for any result. 0 is failure.
/// `limit` 0 means the core's default (500 hits); larger values are clamped to 20000, because the
/// result set is held in this process until `fam_search_close`.
#[no_mangle]
pub unsafe extern "C" fn fam_search_begin(
    core: *mut FamCore,
    query: *const c_char,
    within: *const c_char,
    kind: *const c_char,
    limit: u32,
    err: *mut *mut c_char,
) -> u64 {
    with_core_token(core, Call::new(Op::SearchBegin), err, |borrow, call| {
        let query = required_str(query, "query")?;
        let within = arg_str(within)?;
        let kind = arg_str(kind)?;
        if let Some(within) = within {
            call.names(within);
        }
        call.says(format!(
            "query={query} kind={} limit={limit}",
            kind.unwrap_or("-")
        ));
        borrow.search_begin(query, within.map(Path::new), kind, limit)
    })
}

/// Rows `[first, first + count)` of a search, in the same buffer shape as `fam_dir_fetch` except
/// that a record's name is the whole path. Returns bytes written, or -1.
#[no_mangle]
pub unsafe extern "C" fn fam_search_page(
    core: *mut FamCore,
    token: u64,
    first: u32,
    count: u32,
    buffer: *mut c_uchar,
    buffer_len: u32,
    err: *mut *mut c_char,
) -> c_int {
    let mut call = Call::new(Op::SearchPage).with_token(token);
    call.says(format!("{first}+{count}"));
    with_core_buffer(
        core,
        call,
        buffer,
        buffer_len,
        err,
        move |borrow, _, bytes| borrow.search_page(token, first, count, bytes),
    )
}

/// Asks a running search to stop. Cooperative: the provider notices between directories and between
/// entries, which is what v1's caller-side timeout never was. Returns -1 for an unknown token.
#[no_mangle]
pub unsafe extern "C" fn fam_search_cancel(
    core: *mut FamCore,
    token: u64,
    err: *mut *mut c_char,
) -> c_int {
    with_core_status(
        core,
        Call::new(Op::SearchCancel).with_token(token),
        err,
        |borrow, _| borrow.search_cancel(token),
    )
}

/// Ends a search and releases its rows.
#[no_mangle]
pub unsafe extern "C" fn fam_search_close(core: *mut FamCore, token: u64) {
    with_core_void(
        core,
        Call::new(Op::SearchClose).with_token(token),
        |borrow, _| Ok(borrow.search_close(token)),
    );
}
