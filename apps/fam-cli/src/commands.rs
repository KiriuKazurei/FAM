use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use crate::{NoteAction, StoreAction};
use anyhow::{anyhow, bail, Result};
use fam_core::{path_buf_from_units, AppPaths, Config, SearchHit, SearchRequest as NameRequest};
use fam_sched::{spawn, Batch, SchedulerConfig};
use fam_svc_audit::{Draft, Op};
use fam_svc_auth::{Tier, TokenStore};
use fam_svc_index::SearchRouter;
use fam_svc_store::{NewTag, SearchRequest, Source, TagStore};
use fam_svc_walk::KIND_ORDER;
use fam_svc_walk::{collect, Cancellation, Entry, Kind, WalkPolicy, WalkReport};

use crate::{AuditAction, ConfigAction, McpAction, TagAction, TokenAction};

/// How many hits `search` accumulates unless `--limit` says otherwise. A ceiling rather than none,
/// because every row is held in memory until it is printed and a root like `D:\` does not run out.
const DEFAULT_SEARCH_LIMIT: usize = 200;

pub fn config(config: &Config, action: &ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Path => {
            println!("{}", AppPaths::config_file().display());
            Ok(())
        }
        ConfigAction::Show => {
            print!("{}", toml::to_string_pretty(config)?);
            Ok(())
        }
        ConfigAction::Init { force } => {
            let path = AppPaths::config_file();
            if path.exists() && !force {
                bail!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                );
            }
            let written = Config::write_template_to(&path)?;
            println!("wrote {}", written.display());
            Ok(())
        }
    }
}

pub fn scan(
    config: &Config,
    dir: Option<PathBuf>,
    limit: Option<usize>,
    kind: Option<&str>,
) -> Result<()> {
    let kind = parse_kind(kind)?;
    let mut policy = walk_policy(config);
    if let Some(dir) = dir {
        policy.roots = vec![dir];
    }
    if policy.roots.is_empty() {
        bail!("no search_roots configured; pass a directory or set search_roots");
    }
    if let Some(limit) = limit {
        policy.max_entries = limit.max(1);
    }
    let (entries, report) = collect(&policy, &Cancellation::new(), config.scan_timeout());
    let entries: Vec<Entry> = match kind {
        Some(kind) => entries
            .into_iter()
            .filter(|entry| entry.kind == kind)
            .collect(),
        None => entries,
    };
    render_entries(&entries);
    render_report(&report);
    Ok(())
}

fn parse_kind(value: Option<&str>) -> Result<Option<Kind>> {
    let Some(value) = value else {
        return Ok(None);
    };
    Kind::parse(value).map(Some).ok_or_else(|| {
        anyhow!(
            "unknown kind {value:?}; expected one of {}",
            KIND_ORDER
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

pub fn search(
    config: &Config,
    query: &str,
    within: Option<PathBuf>,
    kind: Option<&str>,
    limit: Option<usize>,
    backend: Option<&str>,
) -> Result<()> {
    let kind = parse_kind(kind)?;
    // Which backends exist, in what order, and what may be reported is the router's own answer, so a
    // CLI search and a shell search cannot drift apart over the same question. The default router
    // carries the tag store with it: a `tag:` query is answered by the library under the same scope
    // rules as everywhere else. A caller who named a backend gets that backend and its refusals -
    // insisting on the walk does not make the walk know what anybody labelled.
    let router = match backend {
        None | Some("auto") | Some("walk") => {
            let store = fam_svc_store::TagStore::open(&config.database_path)?;
            SearchRouter::from_config(config).with_provider(Box::new(
                fam_svc_store::TagSearch::new(std::sync::Arc::new(std::sync::Mutex::new(store))),
            ))
        }
        Some("walk-only") => SearchRouter::walking(config),
        Some("everything") => SearchRouter::indexed(config),
        Some(other) => {
            bail!("unknown backend {other:?}; expected auto, walk, walk-only or everything")
        }
    };

    let request = NameRequest::new(query);
    if request.is_empty() {
        bail!("query carries no search terms");
    }
    let mut request = request;
    if let Some(kind) = kind {
        request = request.kind(kind);
    }
    if let Some(within) = within {
        request = request.scope(within);
    }
    request = request.limit(limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1));

    let started = Instant::now();
    let mut entries: Vec<Entry> = Vec::new();
    let searched = router.search(&request, &Cancellation::new(), &mut |hit| {
        entries.push(entry_of(&hit));
        true
    })?;
    let outcome = searched.outcome;
    println!(
        "backend={} hits={} in {}ms",
        searched.provider,
        entries.len(),
        started.elapsed().as_millis()
    );
    render_entries(&entries);
    if !outcome.complete {
        println!("incomplete: the scope was not fully covered");
    }
    // What the backend saw, as distinct from what it matched: the number that says a search was slow
    // rather than empty. v1 printed the walk's own counters here, which only enumeration had.
    println!(
        "searched: matched={} visited={} in {}ms{}",
        outcome.matched,
        outcome.visited,
        started.elapsed().as_millis(),
        if outcome.complete {
            ""
        } else {
            " (stopped early)"
        }
    );
    Ok(())
}

/// A search hit in the row shape `render_entries` prints.
fn entry_of(hit: &SearchHit) -> Entry {
    Entry {
        path: path_buf_from_units(&hit.path),
        kind: hit.kind,
        size: hit.size,
        // `i64::MIN` is how every backend in this program says "no date"; `Entry` says it with `None`.
        modified_ms: (hit.mtime_ms != i64::MIN).then_some(hit.mtime_ms),
    }
}

pub fn tag(config: &Config, action: &TagAction) -> Result<()> {
    let store = TagStore::open(&config.database_path)?;
    match action {
        TagAction::Add {
            path,
            tags,
            source,
            priority,
        } => {
            if tags.is_empty() {
                bail!("no tags given");
            }
            let source = Source::parse(source)?;
            let entries: Vec<NewTag> = tags
                .iter()
                .map(|tag| {
                    NewTag::manual(tag.clone())
                        .with_source(source)
                        .with_priority(*priority)
                })
                .collect();
            let outcome = store.add_tags(path, &entries)?;
            println!(
                "{}: {} added, {} already present",
                path.display(),
                outcome.inserted,
                outcome.refreshed
            );
        }
        TagAction::Remove { path, tags, source } => {
            let source = source.as_deref().map(Source::parse).transpose()?;
            let removed = store.remove_tags(path, tags, source)?;
            println!("{}: {} tag(s) removed", path.display(), removed);
        }
        TagAction::List { path, source } => {
            let source = source.as_deref().map(Source::parse).transpose()?;
            let records = store.tags_of(path, source)?;
            if records.is_empty() {
                println!("{}: no tags", path.display());
            }
            for record in records {
                println!(
                    "{}  {}  priority={}  {}",
                    record.source, record.tag, record.priority, record.created_at
                );
            }
        }
        TagAction::Rename {
            path,
            from,
            to,
            source,
        } => {
            let source = source.as_deref().map(Source::parse).transpose()?;
            store.rename_tag(path, from, Some(to), source)?;
            println!("{}: {from} -> {to}", path.display());
        }
        TagAction::Merge { from, into } => {
            let outcome = store.merge_tags(from, into)?;
            println!(
                "{from} -> {into}: {} row(s) moved, {} collapsed as duplicates, {} file(s) carry {into} now",
                outcome.moved, outcome.collapsed, outcome.files
            );
        }
        TagAction::Priority {
            path,
            tag,
            priority,
            source,
        } => {
            let source = Source::parse(source)?;
            store.set_priority(path, tag, source, *priority)?;
            println!("{}: {tag} ({source}) priority={priority}", path.display());
        }
    }
    Ok(())
}

/// The note beside a path - the same three sentences the agent's tools answer with, because the
/// store is one store and a rule with two wordings is a rule that drifted.
pub fn note(config: &Config, action: &NoteAction) -> Result<()> {
    let store = TagStore::open(&config.database_path)?;
    match action {
        NoteAction::Get { path } => match store.note(path)? {
            Some(record) => println!("{}", record.body),
            None => println!("no note"),
        },
        NoteAction::Set { path, body } => {
            store.set_note(path, body)?;
            println!("{}: note written ({} bytes)", path.display(), body.len());
        }
        NoteAction::Clear { path } => {
            let removed = store.clear_note(path)?;
            println!(
                "{}: {}",
                path.display(),
                if removed {
                    "note cleared"
                } else {
                    "no note to clear"
                }
            );
        }
    }
    Ok(())
}

/// The store's own upkeep. The dry run is the default and the applied run prints the same counts,
/// so the person running it sees what went before it went.
pub fn store(config: &Config, action: &StoreAction) -> Result<()> {
    let store = TagStore::open(&config.database_path)?;
    match action {
        StoreAction::Prune { write } => {
            let outcome = store.prune_missing(*write)?;
            println!(
                "{} file(s) gone, {} tag(s) and {} note(s) {}",
                outcome.files,
                outcome.tags,
                outcome.notes,
                if *write {
                    "removed"
                } else {
                    "would be removed (run with --write to apply)"
                }
            );
        }
    }
    Ok(())
}

pub fn find(
    config: &Config,
    tags: &[String],
    all: bool,
    within: Option<PathBuf>,
    source: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<()> {
    if tags.is_empty() {
        bail!("at least one --tag is required");
    }
    let source = source.map(Source::parse).transpose()?;
    /* `--all` makes the names a required AND list; the default is one OR group - the same two
     * shapes the `tag:` grammar spells with a comma and with `||`. */
    let request = if all {
        SearchRequest {
            tags: tags.to_vec(),
            source,
            within,
            limit,
            offset,
            ..Default::default()
        }
    } else {
        SearchRequest {
            groups: vec![tags.to_vec()],
            source,
            within,
            limit,
            offset,
            ..Default::default()
        }
    };
    let store = TagStore::open(&config.database_path)?;
    let hits = store.search(&request)?;
    for hit in &hits {
        let names: Vec<&str> = hit.tags.iter().map(|record| record.tag.as_str()).collect();
        println!("{}\t{}", hit.path.display(), names.join(" "));
    }
    println!("{} file(s) matched", hits.len());
    Ok(())
}

pub fn watch(config: &Config, dirs: &[PathBuf], seconds: u64) -> Result<()> {
    let roots: Vec<PathBuf> = if dirs.is_empty() {
        config.search_roots.clone()
    } else {
        dirs.to_vec()
    };
    if roots.is_empty() {
        bail!("no directories given and search_roots is empty");
    }
    let (tx, rx) = mpsc::channel::<Batch>();
    let handle = spawn(
        SchedulerConfig::watching(&roots, config.clone()),
        Box::new(move |batch: Batch| {
            let _ = tx.send(batch);
        }),
    )?;
    println!(
        "watching {} path(s) for {seconds}s (Ctrl-C to stop)",
        roots.len()
    );
    for root in &roots {
        println!("  {}", root.display());
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let mut batches = 0usize;
    let mut changes = 0usize;
    let mut rescans = 0usize;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(batch) => {
                batches += 1;
                changes += batch.changes.len();
                println!(
                    "t+{:>5}ms  batch #{}  {} change(s)  dropped={}",
                    started.elapsed().as_millis(),
                    batches,
                    batch.changes.len(),
                    batch.dropped
                );
                for change in &batch.changes {
                    println!("  {:<8} {}", change.change, change.path.display());
                }
                // A rescan line is the interesting one: it says the directory's coverage is gone,
                // so patching rows would have been wrong and the whole table gets re-read.
                for (dir, why) in &batch.rescans {
                    rescans += 1;
                    println!("  {:<8} {} ({why:?})", "rescan", dir.to_lossy_string());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let dropped = handle.shutdown();
    println!(
        "stopped: {batches} batch(es), {changes} change(s), {rescans} rescan(s), {dropped} dropped event(s)"
    );
    Ok(())
}

pub fn stats(config: &Config) -> Result<()> {
    let store = TagStore::open(&config.database_path)?;
    let stats = store.stats()?;
    println!("database: {}", config.database_path.display());
    println!("files:    {}", stats.files);
    println!(
        "tags:     {} (manual {} / auto {})",
        stats.tags, stats.manual, stats.auto
    );
    Ok(())
}

/// The human-only half of the credential story. Every branch here goes through `TokenStore`, which
/// owns the audit trail it writes into, so a grant cannot be made without a line saying so.
pub fn mcp(config: &Config, action: &McpAction) -> Result<()> {
    match action {
        McpAction::Token { action } => token(config, action),
    }
}

/// A grant attempt that never became a change still gets its line: "someone asked for this and the
/// store said no" is the fact a credential trail has to keep, and the store only records what it did.
fn refused(store: &TokenStore, op: Op, attempt: &str, error: &fam_svc_auth::Error) {
    store.audit().record(
        Draft::new(op)
            .args(attempt)
            .result(format!("error {error}")),
    );
}

fn token(config: &Config, action: &TokenAction) -> Result<()> {
    let now = SystemTime::now();
    let mut store = TokenStore::open(config)?;
    match action {
        TokenAction::Add {
            label,
            tier,
            workspace,
        } => {
            let attempt = format!(
                "label={label} tier={tier} workspaces={}",
                workspace.join(",")
            );
            // These two refusals are the CLI's own, ahead of the store: a tier that is not a `Tier`
            // never reaches it, and an empty workspace list is a call nobody meant to make. They get
            // a line anyway, in the same shape as a store refusal, because "asked for and refused" is
            // the fact the trail exists to keep.
            let Some(tier) = Tier::parse(tier) else {
                let error = fam_svc_auth::Error::UnknownTier(tier.clone());
                refused(&store, Op::TokenAdd, &attempt, &error);
                return Err(error.into());
            };
            if workspace.is_empty() {
                let error = fam_svc_auth::Error::NoWorkspaces(label.clone());
                refused(&store, Op::TokenAdd, &attempt, &error);
                bail!(
                    "a token needs a --workspace to act inside; without one it could read nothing at all"
                );
            }
            let outcome = store.add(label, tier, workspace, now);
            if let Err(error) = &outcome {
                refused(&store, Op::TokenAdd, &attempt, error);
            }
            let issued = outcome?;
            for granted in &issued.token.workspaces {
                if !granted.exists() {
                    // Not a refusal: a folder created next week is a legitimate grant today. It is a
                    // typo-catching warning, and it goes to stderr so scripted output stays clean.
                    eprintln!("warning: {} is not a directory right now", granted.text());
                }
            }
            println!("id         {}", issued.token.id);
            println!("label      {}", issued.token.label);
            println!(
                "tier       {} ({})",
                issued.token.tier,
                issued.token.tier.summary()
            );
            println!(
                "workspaces {}",
                issued
                    .token
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.text())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("created    {}", issued.token.created_at);
            println!();
            println!("credential {}", issued.credential());
            println!(
                "
This is the only time the secret is shown. It is stored as a salted digest, so it cannot be
recovered from {file} - to replace a lost one, run `fam mcp token rotate {id}`.",
                file = config.mcp_token_file.display(),
                id = issued.token.id
            );
        }
        TokenAction::List => {
            // The one credential command that changes nothing, recorded anyway: without it a
            // `token list` session is two bracket lines that do not say what ran, and a bare session
            // is then ambiguous between "someone listed their grants" and "something died early".
            let on_file = store.tokens().len();
            store
                .audit()
                .record(Draft::new(Op::TokenList).result(format!("{on_file} on file")));
            if store.tokens().is_empty() {
                println!("no credentials in {} yet", config.mcp_token_file.display());
                return Ok(());
            }
            println!(
                "{:<9} {:<20} {:<7} {:<6} workspaces",
                "id", "label", "tier", "state"
            );
            for token in store.tokens() {
                let workspaces: Vec<String> = token
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.text())
                    .collect();
                println!(
                    "{:<9} {:<20} {:<7} {:<6} {}{}",
                    token.id,
                    token.label,
                    token.tier,
                    if token.is_active() { "live" } else { "gone" },
                    workspaces.join(" "),
                    token
                        .revoked_at
                        .as_deref()
                        .map(|when| format!("   revoked {when}"))
                        .unwrap_or_default()
                );
            }
            println!(
                "
file: {}",
                config.mcp_token_file.display()
            );
        }
        TokenAction::Revoke { id } => {
            let outcome = store.revoke(id, now);
            if let Err(error) = &outcome {
                refused(&store, Op::TokenRevoke, &format!("id={id}"), error);
            }
            let revoked = outcome?;
            println!(
                "revoked {} ({}) at {}",
                revoked.id,
                revoked.label,
                revoked.revoked_at.as_deref().unwrap_or("an unknown time")
            );
            println!(
                "the row stays in {} so the audit trail keeps resolving its label",
                config.mcp_token_file.display()
            );
        }
        TokenAction::Rotate { id } => {
            let outcome = store.rotate(id);
            if let Err(error) = &outcome {
                refused(&store, Op::TokenRotate, &format!("id={id}"), error);
            }
            let rotated = outcome?;
            println!("id         {}", rotated.token.id);
            println!("label      {}", rotated.token.label);
            println!("tier       {}", rotated.token.tier);
            println!("credential {}", rotated.credential());
            println!("the previous secret stopped working at once");
        }
    }
    // A change that is on disk with no line about it is the one disagreement the file and the trail
    // are not allowed to have, and by now the store cannot take the change back - so it says so here,
    // where a person is reading. The count is the session's, not this command's alone.
    let dropped = store.trail_dropped();
    if dropped > 0 {
        eprintln!(
            "warning: {dropped} trail line(s) could not be written this session - what is in {} is \
             not all of it, `fam audit stats` has the reason",
            config.mcp_token_file.display()
        );
    }
    Ok(())
}

/// Reading the audit log. This is the only path to it: the ABI writes lines and never reads them,
/// so an agent that can browse the filesystem cannot ask the tool what it has already browsed.
///
/// `tail` and `stats` treat a log that has never been written as empty and succeed, because that is
/// the state of a fresh install and a script should be able to ask. `verify` does not: a directory
/// that is gone is one of the things verification exists to notice.
pub fn audit(config: &Config, action: &AuditAction) -> Result<()> {
    let directory = config.audit_dir.as_path();
    match action {
        AuditAction::Tail { lines } => {
            let found = match fam_svc_audit::tail(directory, *lines) {
                Ok(found) => found,
                Err(fam_svc_audit::Error::Missing { path }) => {
                    println!("no audit log in {} yet", path.display());
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            if found.is_empty() {
                println!("nothing in {} yet", directory.display());
            }
            for line in found {
                println!("{line}");
            }
        }
        AuditAction::Verify => {
            let report = fam_svc_audit::verify(directory)?;
            println!(
                "{} file(s), {} record(s), {} session(s)",
                report.files, report.records, report.sessions
            );
            for finding in &report.findings {
                println!("{finding}");
            }
            if !report.intact() {
                // Non-zero, so a tester or a scheduled check can ask the question without reading
                // prose back.
                bail!(
                    "{} finding(s) in the audit log at {}",
                    report.findings.len(),
                    directory.display()
                );
            }
            println!("the chain is unbroken and every session sealed its own count");
        }
        AuditAction::Stats { top } => {
            println!("audit directory: {}", directory.display());
            let summary = match fam_svc_audit::summary(directory, *top) {
                Ok(summary) => summary,
                Err(fam_svc_audit::Error::Missing { .. }) => {
                    println!("no audit log written yet");
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            for line in summary.lines() {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn walk_policy(config: &Config) -> WalkPolicy {
    WalkPolicy::from_config(config)
}

fn render_entries(entries: &[Entry]) {
    for kind in KIND_ORDER {
        let group: Vec<&Entry> = entries.iter().filter(|entry| entry.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        println!("[{}] {}", kind, group.len());
        for entry in group {
            println!("  {:>9}  {}", human_size(entry.size), entry.path.display());
        }
    }
}

fn render_report(report: &WalkReport) {
    println!(
        "walked: dirs={} emitted={} skipped={} errors={} in {}ms{}{}",
        report.directories,
        report.emitted,
        report.skipped,
        report.errors,
        report.elapsed_ms,
        if report.stopped_early() {
            " (stopped early)"
        } else {
            ""
        },
        // Separate from "stopped early" because it is a different answer: nothing ran out of budget,
        // one or more of the directories this was asked to search was refused by an exclusion.
        if report.excluded_roots > 0 {
            " ({} root(s) excluded: the scope was not covered)"
                .replace("{}", &report.excluded_roots.to_string())
        } else {
            String::new()
        }
    );
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}
