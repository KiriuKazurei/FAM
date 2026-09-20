use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fam_svc_walk::{
    collect, walk, Cancellation, Kind, SearchBackend, SearchError, SearchQuery, SearchService,
    WalkBackend, WalkPolicy,
};

fn write_file(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Builds a small tree:
/// root/{a.png, notes.txt, readmes, sub/{b.JPG, deep/c.mp3, .git/config}},
/// root/node_modules/junk/index.js and root/excluded/tmp.bin
fn sample_tree(root: &Path) -> Vec<PathBuf> {
    let files = [
        "a.png",
        "notes.txt",
        "readmes",
        "sub/b.JPG",
        "sub/deep/c.mp3",
        ".git/config",
        "node_modules/junk/index.js",
        "excluded/tmp.bin",
    ];
    files
        .iter()
        .map(|name| {
            let path = root.join(name);
            write_file(&path, name);
            path
        })
        .collect()
}

fn policy(root: &Path) -> WalkPolicy {
    WalkPolicy {
        roots: vec![root.to_path_buf()],
        excluded_names: vec!["NODE_MODULES".into(), ".GIT".into()],
        excluded_prefixes: vec![fam_core::path_key(&root.join("excluded"))],
        threads: 4,
        max_entries: 10_000,
        include_directories: true,
    }
}

fn collect_paths(root: &Path) -> BTreeSet<String> {
    let (entries, _) = collect(
        &policy(root),
        &Cancellation::new(),
        Duration::from_secs(30),
    );
    entries
        .iter()
        .map(|entry| fam_core::path_key(&entry.path))
        .collect()
}

#[test]
fn walk_finds_files_and_directories_and_honours_exclusions() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());

    let found = collect_paths(dir.path());

    assert!(found.contains(&fam_core::path_key(&dir.path().join("a.png"))));
    assert!(found.contains(&fam_core::path_key(&dir.path().join("sub/deep/c.mp3"))));
    assert!(found.contains(&fam_core::path_key(&dir.path().join("sub"))));
    assert!(
        !found.iter().any(|key| key.contains("NODE_MODULES")),
        "node_modules must be skipped: {found:?}"
    );
    assert!(!found.iter().any(|key| key.contains(".GIT")));
    assert!(!found.iter().any(|key| key.contains("EXCLUDED")));
}

#[test]
fn walk_reports_no_errors_on_a_clean_tree() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());

    let (entries, report) = collect(
        &policy(dir.path()),
        &Cancellation::new(),
        Duration::from_secs(30),
    );

    assert_eq!(report.errors, 0);
    assert!(!report.stopped_early());
    assert_eq!(report.emitted as usize, entries.len());
    // Only `sub` and `sub/deep` are counted: `.git`, `node_modules` and `excluded` are pruned
    // before they are ever visited, and the root is seeded rather than discovered.
    assert_eq!(report.directories, 2);
    assert!(report.skipped >= 1, "pruned entries are reported: {report:?}");
}

#[test]
fn missing_roots_are_counted_as_errors_not_panics() {
    let dir = tempfile::tempdir().unwrap();
    let policy = WalkPolicy {
        roots: vec![dir.path().join("does-not-exist")],
        ..Default::default()
    };

    let (entries, report) = collect(&policy, &Cancellation::new(), Duration::from_secs(5));

    assert!(entries.is_empty());
    assert_eq!(report.errors, 1);
}

#[test]
fn kinds_are_assigned_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());

    let (entries, _) = collect(
        &policy(dir.path()),
        &Cancellation::new(),
        Duration::from_secs(30),
    );
    let kind_of = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.file_name() == name)
            .map(|entry| entry.kind)
    };

    assert_eq!(kind_of("a.png"), Some(Kind::Image));
    assert_eq!(kind_of("b.JPG"), Some(Kind::Image));
    assert_eq!(kind_of("c.mp3"), Some(Kind::Audio));
    assert_eq!(kind_of("notes.txt"), Some(Kind::Document));
    assert_eq!(kind_of("sub"), Some(Kind::Directory));
    assert_eq!(kind_of("readmes"), Some(Kind::Other));
}

#[test]
fn sizes_and_timestamps_come_from_metadata() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("payload.txt"), "1234567890");

    let (entries, _) = collect(
        &policy(dir.path()),
        &Cancellation::new(),
        Duration::from_secs(30),
    );
    let entry = entries
        .iter()
        .find(|entry| entry.file_name() == "payload.txt")
        .unwrap();

    assert_eq!(entry.size, 10);
    assert!(entry.modified_ms.unwrap() > 0);
}

#[test]
fn max_entries_truncates_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..20 {
        write_file(&dir.path().join(format!("file-{index:02}.txt")), "x");
    }
    let policy = WalkPolicy {
        max_entries: 5,
        ..policy(dir.path())
    };

    let (entries, report) = collect(&policy, &Cancellation::new(), Duration::from_secs(30));

    assert_eq!(entries.len(), 5);
    assert!(report.truncated);
    assert!(report.stopped_early());
}

#[test]
fn a_expired_deadline_stops_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..200 {
        write_file(&dir.path().join(format!("file-{index:03}.txt")), "x");
    }

    let (entries, report) = collect(&policy(dir.path()), &Cancellation::new(), Duration::ZERO);

    assert!(report.timed_out);
    assert!(entries.len() < 200);
}

#[test]
fn cancelling_mid_walk_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..200 {
        write_file(&dir.path().join(format!("file-{index:03}.txt")), "x");
    }
    let cancel = Cancellation::new();
    let mut seen = 0usize;

    let report = walk(&policy(dir.path()), &cancel, Duration::from_secs(30), |_| {
        seen += 1;
        if seen >= 10 {
            cancel.cancel();
        }
        true
    });

    assert!(report.cancelled);
    assert!(seen < 200, "walk kept going after cancel: {seen}");
}

#[test]
fn callback_can_stop_the_walk_immediately() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let mut seen = 0usize;

    let report = walk(&policy(dir.path()), &Cancellation::new(), Duration::from_secs(30), |_| {
        seen += 1;
        false
    });

    assert_eq!(seen, 1);
    assert!(report.cancelled);
}

#[test]
fn search_matches_substrings_and_filters_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let service = SearchService::walk_only(policy(dir.path()));

    let hits = service
        .search(
            &SearchQuery::new("c.mp3"),
            &Cancellation::new(),
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(hits.entries.len(), 1);
    assert_eq!(hits.entries[0].file_name(), "c.mp3");
    assert!(!hits.incomplete);
    assert_eq!(hits.backend, "walk");

    let images = service
        .search(
            &SearchQuery::new("").kind(Kind::Image),
            &Cancellation::new(),
            Duration::from_secs(30),
        )
        .unwrap_err();
    assert!(matches!(images, SearchError::Empty));

    let scoped = service
        .search(
            &SearchQuery::new("b").within(dir.path().join("sub")),
            &Cancellation::new(),
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(scoped.entries.len(), 1);
    assert_eq!(scoped.entries[0].file_name(), "b.JPG");
}

#[test]
fn search_stops_at_the_limit_without_marking_results_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..30 {
        write_file(&dir.path().join(format!("shot-{index:02}.png")), "x");
    }
    let service = SearchService::walk_only(policy(dir.path()));

    let hits = service
        .search(
            &SearchQuery::new("shot").limit(7),
            &Cancellation::new(),
            Duration::from_secs(30),
        )
        .unwrap();

    assert_eq!(hits.entries.len(), 7);
    assert!(!hits.incomplete, "reaching the limit is not an incomplete search");
}

#[test]
fn service_skips_unavailable_backends_and_reaches_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let service = SearchService::default_with(policy(dir.path()));

    assert_eq!(service.available_backends(), vec!["walk"]);
    let hits = service
        .search(
            &SearchQuery::new("notes"),
            &Cancellation::new(),
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(hits.backend, "walk");
    assert_eq!(hits.entries.len(), 1);
}

#[test]
fn a_search_without_any_scope_says_so() {
    let backend = WalkBackend::new(WalkPolicy::default());
    let cancel = Cancellation::new();

    let error = backend
        .search(&SearchQuery::new("x"), &cancel, Duration::from_secs(1))
        .unwrap_err();
    assert!(matches!(error, SearchError::Unavailable { .. }), "got {error:?}");
    assert!(error.to_string().contains("--in"), "{error}");

    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("scoped.txt"), "x");
    let scoped = backend
        .search(
            &SearchQuery::new("scoped").within(dir.path()),
            &cancel,
            Duration::from_secs(5),
        )
        .unwrap();

    assert_eq!(scoped.entries.len(), 1);
    assert_eq!(scoped.entries[0].file_name(), "scoped.txt");
}
