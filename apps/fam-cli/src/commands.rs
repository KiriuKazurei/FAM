use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use fam_core::{path_buf_from_units, AppPaths, Config, SearchHit, SearchRequest as NameRequest};
use fam_sched::{spawn, Batch, SchedulerConfig};
use fam_svc_index::SearchRouter;
use fam_svc_store::{NewTag, SearchRequest, Source, TagStore};
use fam_svc_walk::KIND_ORDER;
use fam_svc_walk::{collect, Cancellation, Entry, Kind, WalkPolicy, WalkReport};

use crate::{AuditAction, ConfigAction, TagAction};

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
    // CLI search and a shell search cannot drift apart over the same question.
    let router = match backend {
        None | Some("auto") | Some("walk") => SearchRouter::from_config(config),
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
    let request = SearchRequest {
        tags: tags.to_vec(),
        match_all: all,
        source,
        within,
        limit,
        offset,
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
        "walked: dirs={} emitted={} skipped={} errors={} in {}ms{}",
        report.directories,
        report.emitted,
        report.skipped,
        report.errors,
        report.elapsed_ms,
        if report.stopped_early() {
            " (stopped early)"
        } else {
            ""
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
