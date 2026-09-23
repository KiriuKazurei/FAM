use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fam_svc_walk::{collect, to_epoch_ms, walk, Cancellation, Kind, WalkPolicy};

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
        prune: fam_core::Prune::new(
            vec![fam_core::path_key(&root.join("excluded"))],
            vec!["NODE_MODULES".into(), ".GIT".into()],
        ),
        threads: 4,
        max_entries: 10_000,
        include_directories: true,
    }
}

fn collect_paths(root: &Path) -> BTreeSet<String> {
    let (entries, _) = collect(&policy(root), &Cancellation::new(), Duration::from_secs(30));
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
    assert!(
        report.skipped >= 1,
        "pruned entries are reported: {report:?}"
    );
}

/// The exclusion is a property of the directory being listed, not of each row in it. That is what
/// lets a walk ask once per directory instead of once per entry - and a root is listed without
/// anyone having descended into it, so it is the one case that has to be asked on its own.
#[test]
fn an_excluded_root_is_skipped_rather_than_walked() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let mut walked = policy(dir.path());
    walked.roots = vec![dir.path().join("excluded")];

    let (entries, report) = collect(&walked, &Cancellation::new(), Duration::from_secs(30));

    assert!(
        entries.is_empty(),
        "the root itself is excluded: {entries:?}"
    );
    assert_eq!(report.errors, 0, "an exclusion is not a broken path");
    assert_eq!(report.skipped, 1, "and the walk says it refused one");
}

/// The one thing the directory-level rule gives up: a **file** named in `excluded_dirs` is no
/// longer hidden from a walk, because nothing refused to list its parent. The list is of
/// directories, the search-side `Prune::excludes` still answers true for such a path, and the cost
/// of keeping the per-file behaviour was a path key for every entry in the tree.
#[test]
fn a_file_named_as_an_excluded_directory_is_not_hidden_from_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let mut walked = policy(dir.path());
    walked.prune = fam_core::Prune::new(
        vec![fam_core::path_key(&dir.path().join("notes.txt"))],
        Vec::new(),
    );

    let (entries, _) = collect(&walked, &Cancellation::new(), Duration::from_secs(30));
    let names: Vec<String> = entries
        .iter()
        .map(|entry| {
            entry
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.contains(&"notes.txt".to_string()),
        "the walk lists its parent, so the file appears: {names:?}"
    );
    assert!(
        walked.prune.excludes(&dir.path().join("notes.txt")),
        "a search still refuses to report it"
    );
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
fn sizes_and_timestamps_come_from_the_enumeration_record() {
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
    // The reader no longer stats each child, so compare against what a stat says: the
    // enumeration record has to carry the identical numbers, not merely plausible ones.
    let on_disk = fs::metadata(dir.path().join("payload.txt")).unwrap();

    assert_eq!(entry.size, on_disk.len());
    assert_eq!(
        entry.modified_ms,
        Some(to_epoch_ms(on_disk.modified().unwrap()).unwrap())
    );
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

    let report = walk(
        &policy(dir.path()),
        &cancel,
        Duration::from_secs(30),
        |_| {
            seen += 1;
            if seen >= 10 {
                cancel.cancel();
            }
            true
        },
    );

    assert!(report.cancelled);
    assert!(seen < 200, "walk kept going after cancel: {seen}");
}

#[test]
fn callback_can_stop_the_walk_immediately() {
    let dir = tempfile::tempdir().unwrap();
    sample_tree(dir.path());
    let mut seen = 0usize;

    let report = walk(
        &policy(dir.path()),
        &Cancellation::new(),
        Duration::from_secs(30),
        |_| {
            seen += 1;
            false
        },
    );

    assert_eq!(seen, 1);
    assert!(report.cancelled);
}

#[test]
fn a_junction_is_listed_but_never_descended() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    write_file(&target.join("inside.txt"), "x");
    let link = root.path().join("link");
    let made = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "New-Item",
            "-ItemType",
            "Junction",
            "-Path",
            &link.to_string_lossy(),
            "-Target",
            &target.to_string_lossy(),
        ])
        .output();
    assert!(
        made.map(|output| output.status.success()).unwrap_or(false),
        "this machine will not create a junction, so the test cannot run"
    );

    let policy = WalkPolicy {
        roots: vec![root.path().to_path_buf()],
        threads: 2,
        ..Default::default()
    };
    let (entries, report) = collect(&policy, &Cancellation::new(), Duration::from_secs(30));
    let names: BTreeSet<String> = entries.iter().map(|entry| entry.file_name()).collect();

    assert!(
        names.contains("link"),
        "the junction itself must be visible"
    );
    let below = entries
        .iter()
        .filter(|entry| entry.path != link && entry.path.starts_with(&link))
        .map(|entry| entry.path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        below.is_empty(),
        "descending into a reparse point is how a walk loops forever: {below:?}"
    );
    assert_eq!(report.directories, 1, "only the real directory is entered");
    assert!(!report.stopped_early());
}
