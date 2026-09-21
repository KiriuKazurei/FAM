use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use fam_core::{AppPaths, Config};
use fam_sched::{spawn, Batch, SchedulerConfig};
use fam_svc_store::{NewTag, SearchRequest, Source, TagStore};
use fam_svc_walk::KIND_ORDER;
use fam_svc_walk::{
    collect, Cancellation, Entry, EverythingBackend, Kind, SearchBackend, SearchError, SearchQuery,
    SearchResults, SearchService, WalkBackend, WalkPolicy, WalkReport,
};

use crate::{ConfigAction, TagAction};

/// Wrapper that reports itself as available so an explicitly requested backend explains its own
/// failure rather than being skipped as unusable.
struct Forced<B: SearchBackend>(B);

impl<B: SearchBackend> SearchBackend for Forced<B> {
    fn id(&self) -> &'static str {
        self.0.id()
    }

    fn is_available(&self) -> bool {
        true
    }

    fn search(
        &self,
        query: &SearchQuery,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> std::result::Result<SearchResults, SearchError> {
        self.0.search(query, cancel, timeout)
    }
}

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
    let service = match backend {
        None | Some("auto") | Some("walk") => SearchService::default_with(walk_policy(config)),
        Some("walk-only") => {
            SearchService::new(vec![Box::new(WalkBackend::new(walk_policy(config)))])
        }
        Some("everything") => SearchService::new(vec![Box::new(Forced(EverythingBackend))]),
        Some(other) => {
            bail!("unknown backend {other:?}; expected auto, walk, walk-only or everything")
        }
    };

    let mut request = SearchQuery::new(query);
    if let Some(kind) = kind {
        request = request.kind(kind);
    }
    if let Some(within) = within {
        request = request.within(within);
    }
    if let Some(limit) = limit {
        request = request.limit(limit);
    }

    let started = Instant::now();
    let results = service.search(&request, &Cancellation::new(), config.scan_timeout())?;
    println!(
        "backend={} hits={} in {}ms",
        results.backend,
        results.entries.len(),
        started.elapsed().as_millis()
    );
    render_entries(&results.entries);
    if results.incomplete {
        println!("incomplete: the scope was not fully covered");
    }
    render_report(&results.report);
    Ok(())
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
