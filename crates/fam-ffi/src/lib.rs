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
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fam_core::Config;
use fam_sched::{spawn, Batcher, FileChange, Handle, SchedulerConfig, Signal};
use fam_svc_store::{NewTag, SearchRequest, Source, TagStore};
use fam_svc_walk::{Cancellation, Kind, SearchQuery, SearchService, WalkPolicy};
use fam_view::{row_for_path, Delta, Filter, Order, Row, SortKey, ViewModel};
use serde::Serialize;

const OK: c_int = 0;
const ERR: c_int = -1;

pub struct FamCore {
    config: Config,
    store: Mutex<TagStore>,
    views: Mutex<HashMap<u64, ViewModel>>,
    pending: Arc<Mutex<Vec<FileChange>>>,
    watchers: Mutex<HashMap<u64, Handle>>,
    losses: Arc<AtomicU64>,
    next_token: AtomicU64,
}

#[derive(Serialize)]
struct RowOut {
    key: String,
    path: String,
    name: String,
    kind: String,
    size: u64,
    modified_ms: i64,
    tags: Vec<String>,
}

impl From<&Row> for RowOut {
    fn from(row: &Row) -> Self {
        Self {
            key: row.key.clone(),
            path: row.path.display().to_string(),
            name: row.file_name(),
            kind: row.kind.as_str().to_string(),
            size: row.size,
            modified_ms: row.modified_ms,
            tags: row.tags.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum DeltaOut {
    Insert { index: usize, row: RowOut },
    Remove { index: usize, key: String },
    Change { index: usize, row: RowOut },
    Reset,
}

impl From<&Delta> for DeltaOut {
    fn from(delta: &Delta) -> Self {
        match delta {
            Delta::Inserted { index, row } => DeltaOut::Insert {
                index: *index,
                row: row.into(),
            },
            Delta::Removed { index, key } => DeltaOut::Remove {
                index: *index,
                key: key.clone(),
            },
            Delta::Changed { index, row } => DeltaOut::Change {
                index: *index,
                row: row.into(),
            },
            Delta::Reset => DeltaOut::Reset,
        }
    }
}

#[derive(Serialize)]
struct UpdateOut {
    deltas: Vec<DeltaOut>,
    dropped: u64,
    rows: usize,
}

#[derive(Serialize)]
struct RowsOut {
    backend: String,
    incomplete: bool,
    rows: Vec<RowOut>,
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
}

impl FamCore {
    fn create(config_path: Option<&Path>) -> Result<Self, String> {
        let config = match config_path {
            Some(path) => Config::load_merging(path).map_err(|error| error.to_string())?,
            None => Config::load(),
        };
        config.validate().map_err(|error| error.to_string())?;
        let store = TagStore::open(&config.database_path).map_err(|error| error.to_string())?;
        Ok(Self {
            config,
            store: Mutex::new(store),
            views: Mutex::new(HashMap::new()),
            pending: Arc::new(Mutex::new(Vec::new())),
            watchers: Mutex::new(HashMap::new()),
            losses: Arc::new(AtomicU64::new(0)),
            next_token: AtomicU64::new(1),
        })
    }

    /// Lock poisoning can only follow a panic that an entry point already caught, so recovering
    /// the guard keeps one bad call from wedging the front-end.
    fn store(&self) -> std::sync::MutexGuard<'_, TagStore> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn views(&self) -> std::sync::MutexGuard<'_, HashMap<u64, ViewModel>> {
        self.views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn watchers(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Handle>> {
        self.watchers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Vec<FileChange>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
            dropped_events: self.losses.load(Ordering::Relaxed),
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

    fn view_new(&self, sort: &str, order: &str) -> Result<u64, String> {
        let sort_key = parse_sort(sort)?;
        let order = parse_order(order)?;
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.views()
            .insert(token, ViewModel::with_sort(sort_key, order));
        Ok(token)
    }

    fn view_search(
        &self,
        token: u64,
        query: &str,
        within: Option<&Path>,
        kind: Option<&str>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let kind = kind.map(parse_kind).transpose()?;
        let request = SearchQuery::new(query);
        if request.is_empty() && kind.is_none() {
            return Err("query carries no search terms".into());
        }
        let mut request = request;
        if let Some(kind) = kind {
            request = request.kind(kind);
        }
        if let Some(within) = within {
            request = request.within(within);
        }
        if let Some(limit) = limit {
            request = request.limit(limit);
        }

        let service = SearchService::default_with(WalkPolicy::from_config(&self.config));
        let results = service
            .search(&request, &Cancellation::new(), self.config.scan_timeout())
            .map_err(|error| error.to_string())?;

        let mut rows: Vec<Row> = results
            .entries
            .iter()
            .map(|entry| {
                Row::new(
                    &entry.path,
                    entry.kind,
                    entry.size,
                    entry.modified_ms.unwrap_or(0),
                )
            })
            .collect();

        let keys: Vec<String> = rows.iter().map(|row| row.key.clone()).collect();
        let tags = {
            let store = self.store();
            store
                .tag_names_for(&keys)
                .map_err(|error| error.to_string())?
        };
        for row in rows.iter_mut() {
            if let Some(names) = tags.get(&row.key) {
                row.tags = names.clone();
            }
        }

        let mut views = self.views();
        let view = views
            .get_mut(&token)
            .ok_or_else(|| format!("unknown view token {token}"))?;
        view.set_filter(Filter {
            terms: request.terms.clone(),
            kind,
        });
        view.replace_all(rows);
        let out = RowsOut {
            backend: results.backend.to_string(),
            incomplete: results.incomplete,
            rows: view.rows().iter().map(RowOut::from).collect(),
        };
        serde_json::to_string(&out).map_err(|error| error.to_string())
    }

    /// Applies everything the watchers reported since the last call. The initial search resets the
    /// model once; live changes then stream as positioned deltas.
    fn view_drain(&self, token: u64) -> Result<String, String> {
        let queued = {
            let mut pending = self.pending();
            std::mem::take(&mut *pending)
        };
        if queued.is_empty() {
            let views = self.views();
            let rows = views
                .get(&token)
                .ok_or_else(|| format!("unknown view token {token}"))?
                .len();
            let out = UpdateOut {
                deltas: Vec::new(),
                dropped: 0,
                rows,
            };
            return serde_json::to_string(&out).map_err(|error| error.to_string());
        }
        let mut batcher = Batcher::new(std::time::Duration::ZERO, usize::MAX);
        for change in queued {
            batcher.absorb(Signal::Row(change));
        }
        let batch = fam_sched::Batch::new(
            batcher.drain_parts().0,
            Vec::new(),
            self.losses.swap(0, Ordering::Relaxed),
        );

        let mut views = self.views();
        let view = views
            .get_mut(&token)
            .ok_or_else(|| format!("unknown view token {token}"))?;
        let deltas = view.apply_with(&batch, |path| row_for_path(&path));
        let out = UpdateOut {
            deltas: deltas.iter().map(DeltaOut::from).collect(),
            dropped: batch.dropped,
            rows: view.len(),
        };
        serde_json::to_string(&out).map_err(|error| error.to_string())
    }

    fn view_free(&self, token: u64) {
        self.views().remove(&token);
    }

    fn watch_start(&self, dir: &Path) -> Result<u64, String> {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::clone(&self.pending);
        let losses = Arc::clone(&self.losses);
        let sink = move |batch: fam_sched::Batch| {
            // v1 has no per-directory "re-read this" channel, so a watch the kernel dropped is
            // reported the way a full queue already is: as loss, which is what tells the shell to
            // re-poll the view.
            losses.fetch_add(
                batch.dropped + batch.rescans.len() as u64,
                Ordering::Relaxed,
            );
            if let Ok(mut queue) = pending.lock() {
                queue.extend(batch.changes);
            }
        };
        let handle = spawn(
            SchedulerConfig::watching(&[dir.to_path_buf()], self.config.clone()),
            Box::new(sink),
        )
        .map_err(|error| error.to_string())?;
        self.watchers().insert(token, handle);
        Ok(token)
    }

    fn watch_stop(&self, token: u64) {
        let handle = self.watchers().remove(&token);
        if let Some(handle) = handle {
            let dropped = handle.shutdown();
            self.losses.fetch_add(dropped, Ordering::Relaxed);
        }
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

fn guard<F>(call: F, out: *mut *mut c_char, err: *mut *mut c_char) -> c_int
where
    F: FnOnce() -> Result<String, String>,
{
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(json)) => {
            write_cstring(out, json);
            OK
        }
        Ok(Err(message)) => {
            write_cstring(err, message);
            ERR
        }
        Err(_) => {
            write_cstring(err, "FAM core panicked; the call was aborted");
            ERR
        }
    }
}

unsafe fn with_core<F>(
    core: *mut FamCore,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
    body: F,
) -> c_int
where
    F: FnOnce(&FamCore) -> Result<String, String>,
{
    match borrow_core(core) {
        Ok(borrow) => guard(move || body(borrow), out, err),
        Err(message) => {
            write_cstring(err, message);
            ERR
        }
    }
}

/// Same shape for calls whose useful return value is a token; `0` signals failure.
unsafe fn with_core_token<F>(core: *mut FamCore, err: *mut *mut c_char, body: F) -> u64
where
    F: FnOnce(&FamCore) -> Result<u64, String>,
{
    let outcome = borrow_core(core).and_then(|borrow| {
        catch_unwind(AssertUnwindSafe(|| body(borrow)))
            .unwrap_or_else(|_| Err("FAM core panicked; the call was aborted".into()))
    });
    match outcome {
        Ok(token) => token,
        Err(message) => {
            write_cstring(err, message);
            0
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
    let watchers = {
        let borrow = unsafe { &*core };
        let mut watchers = borrow.watchers();
        std::mem::take(&mut *watchers)
    };
    for (_, handle) in watchers {
        handle.shutdown();
    }
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
    with_core(core, out, err, |borrow| borrow.stats())
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
    with_core(core, out, err, |borrow| {
        let path = required_str(path, "path")?;
        let tags = required_str(tags_json, "tags")?;
        let source = arg_str(source)?.unwrap_or("manual");
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
    with_core(core, out, err, |borrow| {
        let path = required_str(path, "path")?;
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
    with_core(core, out, err, |borrow| {
        let path = required_str(path, "path")?;
        let tags = arg_str(tags_json)?.unwrap_or("");
        let source = arg_str(source)?;
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
    with_core(core, out, err, |borrow| {
        let tags = required_str(tags_json, "tags")?;
        let within = arg_str(within)?;
        let source = arg_str(source)?;
        let limit = (limit > 0).then_some(limit as usize);
        borrow.find(tags, match_all != 0, within.map(Path::new), source, limit)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fam_view_new(
    core: *mut FamCore,
    sort: *const c_char,
    order: *const c_char,
    err: *mut *mut c_char,
) -> u64 {
    with_core_token(core, err, |borrow| {
        let sort = arg_str(sort)?.unwrap_or("");
        let order = arg_str(order)?.unwrap_or("");
        borrow.view_new(sort, order)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fam_view_search(
    core: *mut FamCore,
    token: u64,
    query: *const c_char,
    within: *const c_char,
    kind: *const c_char,
    limit: u32,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, out, err, |borrow| {
        let query = arg_str(query)?.unwrap_or("");
        let within = arg_str(within)?;
        let kind = arg_str(kind)?;
        let limit = (limit > 0).then_some(limit as usize);
        borrow.view_search(token, query, within.map(Path::new), kind, limit)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fam_view_drain(
    core: *mut FamCore,
    token: u64,
    out: *mut *mut c_char,
    err: *mut *mut c_char,
) -> c_int {
    with_core(core, out, err, move |borrow| borrow.view_drain(token))
}

#[no_mangle]
pub unsafe extern "C" fn fam_view_free(core: *mut FamCore, token: u64) {
    if let Ok(borrow) = unsafe { borrow_core(core) } {
        borrow.view_free(token);
    }
}

#[no_mangle]
pub unsafe extern "C" fn fam_watch_start(
    core: *mut FamCore,
    dir: *const c_char,
    err: *mut *mut c_char,
) -> u64 {
    with_core_token(core, err, |borrow| {
        let dir = required_str(dir, "dir")?;
        borrow.watch_start(Path::new(dir))
    })
}

#[no_mangle]
pub unsafe extern "C" fn fam_watch_stop(core: *mut FamCore, token: u64) {
    if let Ok(borrow) = unsafe { borrow_core(core) } {
        borrow.watch_stop(token);
    }
}

/// Lets a shell refuse to run against a stale core library.
#[no_mangle]
pub extern "C" fn fam_abi_version() -> u32 {
    1
}
