//! The server: the protocol loop's body, and the tools that only read.
//!
//! One process, one credential, one session. [`Server::authenticate`] is the whole startup cost -
//! it refuses to exist if the credential does not verify, which is the same "no log, no program"
//! rule the core applies to browsing, applied to answering an agent.
//!
//! Every path out of [`Server::handle`] writes exactly one audit line for a `tools/call`, including
//! the refusals and the malformed arguments: a call that produced no line is a call the trail cannot
//! answer for. The line is written *after* the work, so it reports what actually happened rather
//! than what was attempted - and for the refusals the attempt is the whole answer, so those lines
//! say `error <why>`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use fam_core::{Cancellation, Config, Item, Kind, Order, PathKey, Prune, SearchRequest, SortKey};
use fam_svc_audit::{Actor, AuditLog, Draft, Op, Options as AuditOptions};
use fam_svc_auth::{Token, TokenStore, Verb};
use fam_svc_index::{Collection, DirIndex, SearchRouter};
use fam_svc_store::TagSearch;
use serde_json::{json, Value};

use crate::gate::Gate;
use crate::proto::{Id, ProtocolError, Request, Response};
use crate::tools;
use crate::Error;

/// The newest protocol version this server speaks. A client asking for anything else gets this back
/// and decides whether it can continue - which is the handshake's whole point.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "fam";

/// The largest file `fs_read` will return, and the largest body `fs_write` will accept.
///
/// A number chosen to be useful for source and documents and small enough that no tool call can make
/// the server allocate on an agent's say-so. Both are reported back rather than truncated: a caller
/// that gets `size` and `limit` knows what to do next, and a caller handed the first megabyte of a
/// file it asked for in full would not.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The default and ceiling for a listing or a search, in rows.
const DEFAULT_ROWS: i64 = 200;
const MAX_ROWS: i64 = 2000;

/// A tool's answer, before it is wrapped in the protocol's `content` array.
#[derive(Debug)]
pub struct ToolAnswer {
    /// The payload, serialised as text for the client.
    pub value: Value,
    /// True when the tool refused or failed. This is *not* a JSON-RPC error: MCP puts tool failures
    /// in the result with `isError`, so a client can tell "the server understood me and the tool
    /// said no" from "the server did not understand me".
    pub failure: bool,
}

impl ToolAnswer {
    pub(crate) fn ok(value: Value) -> Self {
        Self {
            value,
            failure: false,
        }
    }

    pub(crate) fn failed(value: Value) -> Self {
        Self {
            value,
            failure: true,
        }
    }

    pub(crate) fn error(message: impl AsRef<str>) -> Self {
        Self::failed(json!({ "error": message.as_ref() }))
    }
}

pub struct Server {
    config: Config,
    token: Token,
    audit: AuditLog,
    gate: Gate,
    index: DirIndex,
    router: Arc<SearchRouter>,
    tags: Arc<Mutex<fam_svc_store::TagStore>>,
    /// A fingerprint of the credential file as it was read at startup. A change makes every later
    /// call refuse, because serving on a table that may have lost a revoke is the one thing this
    /// layer is not allowed to do.
    credentials: u64,
    initialized: bool,
}

impl Server {
    /// Opens everything and refuses to exist without a working credential and a writable trail.
    pub fn authenticate(config: Config, credential: &str) -> Result<Self, Error> {
        let store = TokenStore::open(&config)?;
        let token = store
            .verify(credential)
            .cloned()
            .ok_or(Error::NotAuthorized)?;
        // The store keeps its own log, opened as `user`, because that is who runs the credential
        // commands; the line below opens a second log whose actor is `agent:<label>`, and every tool
        // call goes to that one. Two sessions per server run is the cost of not letting an agent's
        // work be attributed to the person who owns the account.
        let audit = AuditLog::open(AuditOptions {
            directory: config.audit_dir.clone(),
            actor: Actor::agent(&token.label),
            // Forced on: `audit_record_reads` exists to quiet the shell's repeated page reads, and an
            // agent's `fs_read` is precisely the event the trail is here to keep.
            record_reads: true,
            daily_limit_bytes: config.audit_daily_limit_bytes(),
            clock: fam_svc_audit::system_clock(),
        })?;
        let credentials = fingerprint(&config.mcp_token_file);
        let index = DirIndex::with_budget_bytes(config.cache_budget_bytes());
        // The store first, because one of the router's backends is a reader of it: a search that
        // names a tag is answered by the library, under the same scope rules and the same gate as a
        // name search - the router's own `Scope` drops any hit outside the configured roots.
        let tags = Arc::new(Mutex::new(fam_svc_store::TagStore::open(
            &config.database_path,
        )?));
        let router = Arc::new(
            SearchRouter::from_config(&config)
                .with_provider(Box::new(TagSearch::new(Arc::clone(&tags)))),
        );
        let gate = Gate::new(
            &config.search_roots,
            Prune::from_config(&config),
            &Gate::program_files(&config),
        );
        Ok(Self {
            config,
            token,
            audit,
            gate,
            index,
            router,
            tags,
            credentials,
            initialized: false,
        })
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The tag and note store, for the sibling module that implements the tools that change it.
    /// Shared with the search backend, which reads it on the router's thread; a poisoned mutex is
    /// recovered the way the core recovers its own, because a panic in one tool call must not take
    /// the store away from every later one.
    pub(crate) fn tags(&self) -> std::sync::MutexGuard<'_, fam_svc_store::TagStore> {
        self.tags
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One line in, one line out. `None` means nothing is written - which is correct for a
    /// notification, and only for one.
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let request = match crate::proto::parse_line(line) {
            Ok(None) => return None,
            Ok(Some(request)) => request,
            Err(error) => return Response::error(None, error).to_line(),
        };
        let id = request.id.clone();
        let response = match request.method.as_str() {
            "initialize" => self.initialize(&request),
            "notifications/initialized" => {
                // The client is telling us it is done negotiating. Answering it would put a response
                // on the wire that no request asked for.
                self.initialized = true;
                return None;
            }
            "ping" => Response::ok(id?, json!({ "protocolVersion": PROTOCOL_VERSION })),
            "tools/list" => Response::ok(
                id?,
                json!({ "tools": tools::TOOLS.iter().map(|tool| tool.to_json()).collect::<Vec<_>>() }),
            ),
            "tools/call" => match id {
                Some(id) => self.call(&request, id),
                // A tool call with no id is not a notification. A notification asks for nothing;
                // this one asked to change a file. Dropping it in silence leaves the client waiting
                // for an answer that will never come and puts no line in the trail, so say what is
                // wrong instead - and say nothing more, because nothing was executed.
                None => Response::error(
                    None,
                    ProtocolError::new(
                        crate::proto::INVALID_REQUEST,
                        "tools/call needs an id, because it is the one method that must be answered",
                    ),
                ),
            },
            other => Response::error(
                id,
                ProtocolError::new(
                    crate::proto::METHOD_NOT_FOUND,
                    format!("this server has no method {other:?}"),
                ),
            ),
        };
        response.to_line()
    }

    fn initialize(&mut self, request: &Request) -> Response {
        let Some(id) = request.id.clone() else {
            // `initialize` without an id is not a handshake, it is a notification wearing one; there
            // is nothing to answer and inventing an answer would put a stray response on the wire.
            return Response::Nothing;
        };
        self.initialized = true;
        Response::ok(
            id,
            json!({
                // The version this server speaks, not the one that was asked for. Echoing the client
                // back to itself would let both sides believe a version nobody implements: a client
                // that asked for something older has to be told what it is actually getting, which is
                // the only way it can decide to leave.
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Fourteen tools over one folder tree, bounded by the credential \
                                that opened this session. whoami says what that credential may do.",
            }),
        )
    }

    fn call(&mut self, request: &Request, id: Id) -> Response {
        let name = match request.string_param("name") {
            Ok(name) => name,
            Err(error) => return Response::error(Some(id), error),
        };
        let Some(spec) = tools::find(&name) else {
            // An unknown tool is a client mistake about the protocol, not a tool that ran and said
            // no, so it is a JSON-RPC error and it is not audited as a call that happened.
            return Response::error(
                Some(id),
                ProtocolError::new(
                    crate::proto::METHOD_NOT_FOUND,
                    format!("there is no tool named {name:?}"),
                ),
            );
        };
        let verb = spec.verb;
        // The tool's own arguments live under `arguments`, so the dispatch layer is handed a request
        // whose params *are* those arguments and whose method is the tool's name. Without this step a
        // missing parameter is reported against "tools/call", which is not the call that failed.
        let arguments = match request.params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(value @ Value::Object(_)) => value.clone(),
            Some(other) => {
                return Response::error(
                    Some(id),
                    ProtocolError::new(
                        crate::proto::INVALID_PARAMS,
                        format!("tools/call: \"arguments\" must be an object, got {other}"),
                    ),
                )
            }
        };
        let call = Request {
            id: Some(id.clone()),
            method: name.clone(),
            params: arguments,
        };
        // Checked here, once, rather than inside the gate: `whoami` claims no path and so never asks
        // the gate's path questions, and a credential that has been revoked while this process was
        // running must not be able to answer anything at all.
        if let Err(why) = self.credentials_unchanged() {
            let answer = ToolAnswer::error(why);
            self.record(verb, &call, &answer);
            return self.envelope(id, &answer);
        }
        let answer = self.dispatch(verb, &call);
        self.record(verb, &call, &answer);
        self.envelope(id, &answer)
    }

    /// Wrap a tool's answer in the `content` array MCP expects.
    fn envelope(&self, id: Id, answer: &ToolAnswer) -> Response {
        let mut content = serde_json::Map::new();
        content.insert("type".into(), json!("text"));
        content.insert(
            "text".into(),
            json!(serde_json::to_string(&answer.value).unwrap_or_default()),
        );
        Response::ok(
            id,
            json!({
                "content": [Value::Object(content)],
                "isError": answer.failure,
            }),
        )
    }

    /// The credential file is the one input this server does not own and cannot pin.
    fn credentials_unchanged(&self) -> Result<(), String> {
        if crate::server::fingerprint(&self.config.mcp_token_file) != self.credentials {
            return Err(
                "the credential file changed after this server started; restart it to continue"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// The one place a call's outcome becomes a line.
    fn record(&self, verb: Verb, request: &Request, answer: &ToolAnswer) {
        let Some(op) = operation(verb) else {
            return;
        };
        let target = path_argument(verb, request);
        let mut draft = Draft::new(op).args(args_of(verb, request));
        if let Some(path) = target.as_deref() {
            draft = draft.target(path);
        }
        if answer.failure {
            let message = answer
                .value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the tool failed");
            draft = draft.result(format!("error {message}"));
        } else {
            draft = draft.result(summary(&answer.value));
        }
        self.audit.record(draft);
    }

    /// The gate's four questions plus the credential-file check, in one call. Every tool goes
    /// through here before it touches a path.
    pub(crate) fn admit(&self, verb: Verb, path: &Path) -> Result<(), String> {
        self.gate
            .admit(&self.token, verb, path)
            .map_err(|refusal| refusal.message())
    }

    /// The one question a no-path verb can still be asked. Same sentence as `admit`'s first
    /// refusal, because it is the same question.
    pub(crate) fn admit_verb(&self, verb: Verb) -> Result<(), String> {
        self.gate
            .admit_verb(&self.token, verb)
            .map_err(|refusal| refusal.message())
    }

    fn dispatch(&mut self, verb: Verb, request: &Request) -> ToolAnswer {
        match verb {
            Verb::WhoAmI => self.whoami(),
            Verb::FsList => self.arg_path(request, "path", |server, path| {
                server.fs_list(request, path)
            }),
            Verb::FsRead => self.arg_path(request, "path", |server, path| server.fs_read(&path)),
            Verb::FsSearch => self.fs_search(request),
            Verb::TagsGet => self.arg_path(request, "path", |server, path| server.tags_get(&path)),
            Verb::NoteGet => self.arg_path(request, "path", |server, path| server.note_get(&path)),
            Verb::FsWrite => self.arg_path(request, "path", |server, path| {
                let content = match request.string_param("content") {
                    Ok(content) => content,
                    Err(error) => return ToolAnswer::error(error.message),
                };
                server.fs_write(&path, &content)
            }),
            Verb::FsCreate => self.arg_path(request, "path", |server, path| {
                let kind = request
                    .params
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_owned();
                server.fs_create(&path, &kind)
            }),
            Verb::FsDelete => {
                self.arg_path(request, "path", |server, path| server.fs_delete(&path))
            }
            Verb::TagsSet => self.arg_path(request, "path", |server, path| {
                match string_list(request, "tags") {
                    Ok(tags) => server.tags_set(&path, &tags, request_source(request)),
                    Err(error) => ToolAnswer::error(error),
                }
            }),
            Verb::TagsRemove => self.arg_path(request, "path", |server, path| {
                match string_list(request, "tags") {
                    Ok(tags) => server.tags_remove(&path, &tags),
                    Err(error) => ToolAnswer::error(error),
                }
            }),
            Verb::TagsMerge => self.tags_merge(request),
            Verb::NoteSet => self.arg_path(request, "path", |server, path| {
                match request.string_param("body") {
                    Ok(body) => server.note_set(&path, &body),
                    Err(error) => ToolAnswer::error(error.message),
                }
            }),
            Verb::NoteClear => {
                self.arg_path(request, "path", |server, path| server.note_clear(&path))
            }
            Verb::FsMove => {
                let from = match request.string_param("from") {
                    Ok(from) => from,
                    Err(error) => return ToolAnswer::error(error.message),
                };
                let to = match request.string_param("to") {
                    Ok(to) => to,
                    Err(error) => return ToolAnswer::error(error.message),
                };
                self.fs_move(&PathBuf::from(from), &PathBuf::from(to))
            }
        }
    }

    /// Read the path parameter, refuse if it is missing, and only then run the tool.
    fn arg_path(
        &mut self,
        request: &Request,
        name: &str,
        then: impl FnOnce(&mut Self, PathBuf) -> ToolAnswer,
    ) -> ToolAnswer {
        let raw = match request.string_param(name) {
            Ok(raw) => raw,
            Err(error) => return ToolAnswer::error(error.message),
        };
        if raw.trim().is_empty() {
            return ToolAnswer::error(format!("{name} must not be empty"));
        }
        then(self, PathBuf::from(raw))
    }

    fn fs_list(&mut self, request: &Request, path: PathBuf) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsList, &path) {
            return ToolAnswer::error(why);
        }
        let limit = match request.number_param("limit", DEFAULT_ROWS) {
            Ok(limit) => limit.clamp(1, MAX_ROWS) as usize,
            Err(error) => return ToolAnswer::error(error.message),
        };
        let offset = match request.number_param("offset", 0) {
            Ok(offset) => offset.max(0) as usize,
            Err(error) => return ToolAnswer::error(error.message),
        };
        let sort = match request
            .params
            .get("sort")
            .and_then(Value::as_str)
            .unwrap_or("name")
        {
            "size" => SortKey::Size,
            "kind" => SortKey::Kind,
            "time" => SortKey::Modified,
            _ => SortKey::Name,
        };
        let cancel = Cancellation::new();
        let snapshot = match self.index.open(&PathKey::of(&path), &cancel) {
            Ok(fam_svc_index::Opened::Ready(snapshot)) => snapshot,
            Ok(fam_svc_index::Opened::Loading) => {
                return ToolAnswer::error("this directory is being read by someone else; try again")
            }
            Ok(fam_svc_index::Opened::CoolingDown(after)) => {
                return ToolAnswer::error(format!(
                    "this directory failed to read recently and is cooling down for {}s",
                    after.as_secs()
                ))
            }
            Err(fam_svc_index::OpenError::Cancelled) => {
                return ToolAnswer::error("the read was cancelled")
            }
            Err(fam_svc_index::OpenError::Enum(source)) => {
                return ToolAnswer::error(format!("cannot list {}: {source}", path.display()))
            }
        };
        let stale = snapshot.is_stale();
        let generation = snapshot.generation;
        let mut collection = Collection::from_snapshot(snapshot);
        collection.set_sort(sort, Order::Ascending);
        let total = collection.len();
        // Grouping is a key of the ordering, so the page boundary cannot cut between a folder and
        // the folder below it: grouping a window and then slicing the window shows the folders only
        // on whichever page happened to contain them. The window stays bounded by `limit`, which is
        // also the ceiling on a single call's rows.
        collection.set_grouped(true);
        let window = collection.fetch(offset, limit.min(total.saturating_sub(offset)));
        let entries: Vec<Value> = window.iter().map(|row| json_row(&path, row)).collect();
        ToolAnswer::ok(json!({
            "path": path.display().to_string(),
            "generation": generation,
            "stale": stale,
            "total": total,
            "returned": entries.len(),
            "offset": offset,
            "entries": entries,
        }))
    }

    fn fs_read(&mut self, path: &Path) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsRead, path) {
            return ToolAnswer::error(why);
        }
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(source) => {
                return ToolAnswer::error(format!("cannot open {}: {source}", path.display()))
            }
        };
        if meta.is_dir() {
            return ToolAnswer::error(format!(
                "{} is a directory; fs_list reads a directory",
                path.display()
            ));
        }
        if meta.len() > MAX_FILE_BYTES {
            return ToolAnswer::failed(json!({
                "error": format!(
                    "{} is {} bytes and fs_read returns at most {MAX_FILE_BYTES}",
                    path.display(),
                    meta.len()
                ),
                "size": meta.len(),
                "limit": MAX_FILE_BYTES,
            }));
        }
        // Read through a `take` window, not `fs::read`: the size check above is a `metadata` call,
        // and between that call and this one the file is free to grow. Reading whatever it now holds
        // would let a 1 MiB limit be exceeded by an allocation this server makes on a caller's
        // behalf. One extra byte is enough to tell "exactly at the limit" from "over it".
        let mut bytes: Vec<u8> = Vec::new();
        let opened = std::fs::File::open(path).and_then(|file| {
            use std::io::Read;
            file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)
        });
        if let Err(source) = opened {
            return ToolAnswer::error(format!("cannot read {}: {source}", path.display()));
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return ToolAnswer::ok(json!({
                "error": format!("{} grew past the read limit", path.display()),
                "limit": MAX_FILE_BYTES,
            }));
        }
        match String::from_utf8(bytes) {
            Ok(text) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "bytes": text.len(),
                "text": text,
            })),
            Err(_) => ToolAnswer::failed(json!({
                "error": format!("{} is not UTF-8 text", path.display()),
                "binary": true,
                "bytes": meta.len(),
            })),
        }
    }

    fn fs_search(&mut self, request: &Request) -> ToolAnswer {
        let query = match request.string_param("query") {
            Ok(query) => query,
            Err(error) => return ToolAnswer::error(error.message),
        };
        if query.trim().is_empty() {
            return ToolAnswer::error("query must say something");
        }
        let limit = match request.number_param("limit", DEFAULT_ROWS) {
            Ok(limit) => limit.clamp(1, MAX_ROWS) as usize,
            Err(error) => return ToolAnswer::error(error.message),
        };
        let mut search = SearchRequest::new(&query).limit(limit);
        if let Some(kind) = request.params.get("kind").and_then(Value::as_str) {
            match Kind::parse(kind) {
                Some(kind) => search = search.kind(kind),
                None => return ToolAnswer::error(format!("unknown kind {kind:?}")),
            }
        }
        // A scope narrows the roots; it never widens them, and the widening case is refused by the
        // gate before the router is ever asked.
        let scoped = match request.params.get("within") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(PathBuf::from(text)),
            Some(other) => {
                return ToolAnswer::error(format!("within must be a string, got {other}"))
            }
        };
        if let Some(within) = &scoped {
            if let Err(why) = self.admit(Verb::FsSearch, within) {
                return ToolAnswer::error(why);
            }
            search = search.scope(within.clone());
        } else if self.config.search_roots.is_empty() {
            // No roots means nothing to search, and indexing `search_roots[0]` here would be a panic
            // reachable from a configuration file - which is the one input this server is handed
            // before it knows anything about the caller.
            return ToolAnswer::error(
                "this program has no configured roots, so a search has nothing to cover",
            );
        } else if self
            .config
            .search_roots
            .iter()
            .any(|root| self.admit(Verb::FsSearch, root).is_err())
        {
            // An unscoped search reads **every** configured root - `Router::allows` lets any of them
            // produce rows once no scope is named - so the credential has to cover all of them.
            // Asking about `search_roots[0]` alone was the bug: with two roots, a token granted one
            // got file names out of the other. The refused root is not named in the answer either,
            // because a caller who has not earned the second root has not earned knowing its path.
            return ToolAnswer::error(
                "a search without `within` covers every configured root, and this credential does \
                 not",
            );
        }
        let mut hits: Vec<Value> = Vec::new();
        let mut overflowed = false;
        let cancel = Cancellation::new();
        let outcome = self.router.search(&search, &cancel, &mut |hit| {
            if hits.len() >= limit {
                overflowed = true;
                return false;
            }
            hits.push(json_hit(&hit));
            true
        });
        match outcome {
            Ok(searched) => ToolAnswer::ok(json!({
                "query": query,
                "provider": searched.provider,
                "matched": searched.outcome.matched,
                "visited": searched.outcome.visited,
                "complete": searched.outcome.complete && !overflowed,
                "returned": hits.len(),
                "available": self.router.available(),
                "hits": hits,
            })),
            Err(source) => ToolAnswer::error(format!("the search failed: {source}")),
        }
    }

    fn whoami(&self) -> ToolAnswer {
        ToolAnswer::ok(json!({
            "id": self.token.id,
            "label": self.token.label,
            "tier": self.token.tier.name(),
            "allows": Verb::ALL
                .iter()
                .filter(|verb| self.token.allows(**verb))
                .map(|verb| verb.name())
                .collect::<Vec<_>>(),
            "workspaces": self
                .token
                .workspaces
                .iter()
                .map(|workspace| workspace.text())
                .collect::<Vec<_>>(),
            "roots": self
                .config
                .search_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>(),
            "backends": self.router.available(),
            "actor": format!("agent:{}", self.token.label),
        }))
    }
}

/// The verb's audit operation. Every verb has one; the mapping is total so a new verb cannot be
/// added without the compiler asking what it should be recorded as.
fn operation(verb: Verb) -> Option<Op> {
    Some(match verb {
        Verb::FsList => Op::McpFsList,
        Verb::FsRead => Op::McpFsRead,
        Verb::FsSearch => Op::McpFsSearch,
        Verb::FsWrite => Op::McpFsWrite,
        Verb::FsCreate => Op::McpFsCreate,
        Verb::FsDelete => Op::McpFsDelete,
        Verb::FsMove => Op::McpFsMove,
        Verb::TagsGet => Op::McpTagsGet,
        Verb::TagsSet => Op::McpTagsSet,
        Verb::TagsRemove => Op::McpTagsRemove,
        Verb::TagsMerge => Op::McpTagsMerge,
        Verb::NoteGet => Op::McpNoteGet,
        Verb::NoteSet => Op::McpNoteSet,
        Verb::NoteClear => Op::McpNoteClear,
        Verb::WhoAmI => Op::McpWhoAmI,
    })
}

fn path_argument(verb: Verb, request: &Request) -> Option<PathBuf> {
    let name = match verb {
        Verb::FsMove => "from",
        Verb::FsSearch => "within",
        _ => "path",
    };
    request
        .params
        .get(name)
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

/// What was asked, in as little space as will still answer "which call was this" a month later.
/// Never a file's contents: a `fs_write` line carries the byte count, not the bytes.
fn args_of(verb: Verb, request: &Request) -> String {
    let text = match verb {
        Verb::FsSearch => format!(
            "query={}",
            request
                .params
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        Verb::FsList => {
            let mut parts = Vec::new();
            for key in ["sort", "offset", "limit"] {
                if let Some(value) = request.params.get(key) {
                    parts.push(format!("{key}={value}"));
                }
            }
            parts.join(" ")
        }
        Verb::FsWrite => format!(
            "bytes={}",
            request
                .params
                .get("content")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or_default()
        ),
        Verb::FsMove => format!(
            "to={}",
            request
                .params
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        Verb::NoteSet => format!(
            "bytes={}",
            request
                .params
                .get("body")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or_default()
        ),
        Verb::TagsMerge => format!(
            "from={} into={}",
            request
                .params
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            request
                .params
                .get("into")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        Verb::TagsSet | Verb::TagsRemove => format!(
            "tags={}",
            string_list(request, "tags").unwrap_or_default().join(",")
        ),
        _ => String::new(),
    };
    // Clipped by characters, through the same helper the audit layer writes its own fields with.
    // `String::truncate` cuts on a *byte* index and panics when that index falls inside a character -
    // and the query and the move ends are caller text, so a 100-character non-ASCII query would have
    // killed this process on the way to writing the line about it.
    fam_svc_audit::clip(&text, 200)
}

/// The `result` of a successful call: the counts a reader cares about, not the payload.
fn summary(value: &Value) -> String {
    for (key, label) in [
        ("returned", "rows"),
        ("hits", "hits"),
        ("tags", "tags"),
        ("bytes", "bytes"),
    ] {
        if let Some(count) = value.get(key).and_then(Value::as_u64) {
            return format!("{label}={count}");
        }
    }
    if value.get("removed").is_some() || value.get("written").is_some() {
        return "ok".into();
    }
    match value.get("matched").and_then(Value::as_u64) {
        Some(matched) => format!("matched={matched}"),
        None => "ok".into(),
    }
}

fn json_row(directory: &Path, row: &fam_svc_index::FetchedRow) -> Value {
    let name = String::from_utf16_lossy(&row.name);
    let item: &Item = &row.item;
    let flags = item.flags();
    json!({
        "name": name,
        // Rebuilt through `components()` so the separators are one style: the directory may have
        // been spelled with slashes by the caller, and `join` would then produce
        // "C:/dirile" - a path that reads as a typo and is awkward to feed back in.
        "path": path_string(&directory.join(&name)),
        "kind": Kind::from_index(item.kind).map(|kind| kind.as_str()),
        "directory": flags.is_directory(),
        "size": item.size,
        "modified": (item.mtime_ms != i64::MIN).then_some(item.mtime_ms),
        // Reported because it is the fact the gate just used: a link is where a listing can leave the
        // workspace, and a caller that can see it can ask about it instead of guessing.
        "reparse": flags.is_reparse(),
    })
}

/// The path as one consistent spelling: `components()` collects back into a `PathBuf` with the
/// platform separator, whichever way the caller spelled the directory.
fn path_string(path: &Path) -> String {
    let rebuilt: PathBuf = path.components().collect();
    rebuilt.display().to_string()
}

fn json_hit(hit: &fam_core::SearchHit) -> Value {
    json!({
        "path": String::from_utf16_lossy(&hit.path),
        "kind": hit.kind.as_str(),
        "size": hit.size,
        "modified": (hit.mtime_ms != i64::MIN).then_some(hit.mtime_ms),
    })
}

fn string_list(request: &Request, name: &str) -> Result<Vec<String>, String> {
    let Some(value) = request.params.get(name) else {
        return Ok(Vec::new());
    };
    match value.as_array() {
        Some(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{name} must be an array of strings"))
            })
            .collect(),
        None => Err(format!("{name} must be an array of strings")),
    }
}

fn request_source(request: &Request) -> fam_svc_store::Source {
    match request
        .params
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("manual")
    {
        "auto" => fam_svc_store::Source::Auto,
        _ => fam_svc_store::Source::Manual,
    }
}

/// A cheap fingerprint of the credential file: its size, mtime and a hash of both. Enough to notice
/// that someone else changed it, without reading it again on every call.
fn fingerprint(path: &Path) -> u64 {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    let modified = meta
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let mut hasher = DefaultHasher::new();
    meta.len().hash(&mut hasher);
    modified.hash(&mut hasher);
    hasher.finish()
}
