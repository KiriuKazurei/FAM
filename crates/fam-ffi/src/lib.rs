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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fam_core::{Config, Order, PathKey, SearchRequest as NameRequest, SortKey};
use fam_sched::{spawn, SchedulerConfig};
use fam_svc_audit::{Actor, AuditLog, Draft, Op, Options as AuditOptions};
use fam_svc_index::DirIndex;
use fam_svc_store::{NewTag, SearchRequest, Source, TagStore};
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
    store: Mutex<TagStore>,
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
struct FileTagsOut {
    path: String,
    tags: Vec<String>,
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
        if let Some(root) = fam_svc_audit::conflicting_root(&config.audit_dir, &config.search_roots)
        {
            return Err(format!(
                "the audit directory {} would contain the search root {}; move one of them",
                config.audit_dir.display(),
                root.display()
            ));
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
        let store = TagStore::open(&config.database_path).map_err(|error| error.to_string())?;
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
        let router = Arc::new(SearchRouter::from_config(&config));
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
            store: Mutex::new(store),
            losses,
            index,
            v2,
            router,
            searches,
            loader,
            watched: Mutex::new(HashMap::new()),
            audit,
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

    fn find(
        &self,
        tags_json: &str,
        match_all: bool,
        within: Option<&Path>,
        source: Option<&str>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let tags = parse_tags(tags_json)?;
        let source = source
            .map(Source::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        let request = SearchRequest {
            tags,
            match_all,
            source,
            within: within.map(PathBuf::from),
            limit,
            offset: None,
        };
        let store = self.store();
        let hits = store.search(&request).map_err(|error| error.to_string())?;
        let files: Vec<FileTagsOut> = hits
            .iter()
            .map(|hit| FileTagsOut {
                path: hit.path.display().to_string(),
                tags: hit.tags.iter().map(|record| record.tag.clone()).collect(),
            })
            .collect();
        serde_json::to_string(&files).map_err(|error| error.to_string())
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

#[no_mangle]
pub unsafe extern "C" fn fam_find(
    core: *mut FamCore,
    tags_json: *const c_char,
    match_all: c_int,
    within: *const c_char,
    source: *const c_char,
    limit: u32,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, Call::new(Op::TagFind), out, err, |borrow, call| {
        let tags = required_str(tags_json, "tags")?;
        let within = arg_str(within)?;
        let source = arg_str(source)?;
        let limit = (limit > 0).then_some(limit as usize);
        if let Some(within) = within {
            call.names(within);
        }
        call.says(format!(
            "tags={tags} all={} source={} limit={}",
            match_all != 0,
            source.unwrap_or("-"),
            limit.unwrap_or(0)
        ));
        borrow.find(tags, match_all != 0, within.map(Path::new), source, limit)
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
