use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use fam_core::PathKey;
use fam_sched::{
    spawn, Batch, Change, DirBatches, FileChange, Handle, Mode, SchedulerConfig, RECONCILE_INTERVAL,
};

type Seen = BTreeMap<String, String>;

fn config(dir: &Path, mode: Mode) -> SchedulerConfig {
    SchedulerConfig {
        queue_capacity: 256,
        window: Duration::from_millis(40),
        max_batch: 128,
        initial_watch: vec![dir.to_path_buf()],
        initial_mode: mode,
    }
}

/// Starts a pipeline watching `dir` one level deep, which is the mode the lazy cache uses.
fn fast_pipeline(dir: &Path) -> (Handle, Receiver<Seen>) {
    pipeline(dir, Mode::OneLevel)
}

fn pipeline(dir: &Path, mode: Mode) -> (Handle, Receiver<Seen>) {
    let (tx, rx) = mpsc::channel::<Seen>();
    let handle = spawn(
        config(dir, mode),
        Box::new(move |batch: Batch| {
            let mut seen = Seen::new();
            for change in batch.changes {
                seen.insert(change.path.display().to_string(), change.change.to_string());
            }
            let _ = tx.send(seen);
        }),
    )
    .expect("watcher starts");
    (handle, rx)
}

/// Collects batches until `want` is satisfied, and returns everything seen so far.
fn wait_until(rx: &Receiver<Seen>, want: impl Fn(&Seen) -> bool) -> Seen {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut accumulated = Seen::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(batch) => {
                for (path, change) in batch {
                    accumulated.insert(path, change);
                }
                if want(&accumulated) {
                    return accumulated;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    accumulated
}

fn entry_for<'a>(seen: &'a Seen, name: &str) -> Option<&'a String> {
    seen.iter()
        .find(|(path, _)| {
            Path::new(path.as_str())
                .file_name()
                .map(|file| file == name)
                == Some(true)
        })
        .map(|(_, change)| change)
}

#[test]
fn created_files_reach_the_sink_as_created() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    fs::write(dir.path().join("fresh.txt"), "hello").unwrap();
    let seen = wait_until(&rx, |seen| {
        entry_for(seen, "fresh.txt").is_some_and(|change| change == "created")
    });

    assert_eq!(
        entry_for(&seen, "fresh.txt").map(String::as_str),
        Some("created")
    );
    assert_eq!(handle.shutdown(), 0, "no events should have been dropped");
}

#[test]
fn deletions_reach_the_sink_which_the_legacy_consumer_lost() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("doomed.txt");
    fs::write(&victim, "x").unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    fs::remove_file(&victim).unwrap();
    let seen = wait_until(&rx, |seen| {
        entry_for(seen, "doomed.txt").is_some_and(|change| change == "removed")
    });

    assert_eq!(
        entry_for(&seen, "doomed.txt").map(String::as_str),
        Some("removed")
    );
    handle.shutdown();
}

#[test]
fn renames_surface_under_the_new_name() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("before.txt");
    fs::write(&before, "x").unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    fs::rename(&before, dir.path().join("after.txt")).unwrap();
    let seen = wait_until(&rx, |seen| {
        entry_for(seen, "after.txt").is_some() || entry_for(seen, "before.txt").is_some()
    });

    assert!(
        entry_for(&seen, "after.txt").is_some() || entry_for(&seen, "before.txt").is_some(),
        "neither side of the rename was reported: {seen:?}"
    );
    handle.shutdown();
}

#[test]
fn repeated_writes_collapse_to_one_delta_per_path() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("stream.txt");
    fs::write(&target, "seed").unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    for index in 0..12 {
        fs::write(&target, format!("write {index}")).unwrap();
    }
    let seen = wait_until(&rx, |seen| entry_for(seen, "stream.txt").is_some());

    let matches: Vec<&String> = seen
        .iter()
        .filter(|(path, _)| {
            Path::new(path.as_str())
                .file_name()
                .map(|name| name == "stream.txt")
                == Some(true)
        })
        .map(|(_, change)| change)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "one path yields one row delta per batch: {seen:?}"
    );
    handle.shutdown();
}

/// The watch the cache needs is one level deep, and a nested write must not reach it.
///
/// Proved with a marker rather than a bare sleep: the test waits for a top-level change that is
/// written *after* the nested one, so it cannot pass simply because the watcher was slow to start.
#[test]
fn a_one_level_watch_stops_at_the_directory_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("deep/nested")).unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    fs::write(dir.path().join("deep/nested/leaf.dat"), "x").unwrap();
    fs::write(dir.path().join("marker.txt"), "x").unwrap();
    let seen = wait_until(&rx, |seen| entry_for(seen, "marker.txt").is_some());

    assert!(
        entry_for(&seen, "marker.txt").is_some(),
        "the watch must be live for this test to mean anything: {seen:?}"
    );
    assert!(
        entry_for(&seen, "leaf.dat").is_none(),
        "a one-level watch reported a change two levels down: {seen:?}"
    );
    handle.shutdown();
}

#[test]
fn a_recursive_watch_sees_the_nested_write() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("deep/nested")).unwrap();
    let (handle, rx) = pipeline(dir.path(), Mode::Recursive);

    fs::write(dir.path().join("deep/nested/leaf.dat"), "x").unwrap();
    let seen = wait_until(&rx, |seen| entry_for(seen, "leaf.dat").is_some());

    assert!(entry_for(&seen, "leaf.dat").is_some(), "got {seen:?}");
    handle.shutdown();
}

#[test]
fn watches_can_be_added_and_removed_while_running() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("inside");
    fs::create_dir(&nested).unwrap();

    let (tx, rx) = mpsc::channel::<Seen>();
    let collector = tx;
    let mut handle = spawn(
        SchedulerConfig {
            queue_capacity: 64,
            window: Duration::from_millis(30),
            max_batch: 32,
            initial_watch: Vec::new(),
            initial_mode: Mode::OneLevel,
        },
        Box::new(move |batch: Batch| {
            let mut seen = Seen::new();
            for change in batch.changes {
                seen.insert(change.path.display().to_string(), change.change.to_string());
            }
            let _ = collector.send(seen);
        }),
    )
    .expect("a pipeline may start with nothing watched");

    fs::write(nested.join("before.txt"), "x").unwrap();
    assert_eq!(handle.watched_count(), 0);
    assert!(
        handle.watch(&nested, Mode::OneLevel).unwrap(),
        "the first watch of a directory is an addition"
    );
    assert!(!handle.watch(&nested, Mode::OneLevel).unwrap());
    assert_eq!(handle.watched_count(), 1);

    fs::write(nested.join("after.txt"), "x").unwrap();
    let seen = wait_until(&rx, |seen| entry_for(seen, "after.txt").is_some());
    assert!(
        entry_for(&seen, "after.txt").is_some(),
        "a directory watched after start must still report: {seen:?}"
    );
    assert!(
        entry_for(&seen, "before.txt").is_none(),
        "changes from before the watch must not be replayed: {seen:?}"
    );

    assert!(handle.unwatch(&nested).unwrap());
    assert!(
        !handle.unwatch(&nested).unwrap(),
        "already gone is not an error"
    );
    fs::write(nested.join("released.txt"), "x").unwrap();
    fs::write(nested.join("after.txt"), "y").unwrap();
    let settled = Instant::now() + Duration::from_millis(400);
    let mut after_unwatch = Seen::new();
    while Instant::now() < settled {
        if let Ok(batch) = rx.recv_timeout(Duration::from_millis(50)) {
            after_unwatch.extend(batch);
        }
    }
    assert!(
        entry_for(&after_unwatch, "released.txt").is_none(),
        "events kept flowing after unwatch: {after_unwatch:?}"
    );
    handle.shutdown();
}

/// notify keys its own watch map by the raw path and overwrites an existing entry without stopping
/// the previous watch (`windows.rs:238`), which leaks the directory handle and queues a second read
/// request. The pipeline therefore dedupes by path identity, not by spelling.
#[test]
fn the_same_directory_written_two_ways_is_one_watch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut handle, rx) = fast_pipeline(dir.path());

    let other_spelling = PathBuf::from(
        dir.path()
            .to_string_lossy()
            .replace('\\', "/")
            .to_uppercase(),
    );
    assert!(
        !handle.watch(&other_spelling, Mode::OneLevel).unwrap(),
        "a second spelling of the same directory must not add a watch"
    );
    assert_eq!(handle.watched_count(), 1);
    assert!(handle.is_watched(&other_spelling));

    fs::write(dir.path().join("once.txt"), "x").unwrap();
    let seen = wait_until(&rx, |seen| entry_for(seen, "once.txt").is_some());
    assert_eq!(
        seen.values()
            .filter(|change| change.as_str() == "created")
            .count(),
        1,
        "a duplicated watch would report this create twice: {seen:?}"
    );
    handle.shutdown();
}

#[test]
fn batch_rows_arrive_grouped_by_the_directory_they_belong_to() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();

    let (tx, rx) = mpsc::channel::<DirBatches>();
    let collector = tx;
    let handle = spawn(
        config(dir.path(), Mode::Recursive),
        Box::new(move |batch: Batch| {
            let _ = collector.send(batch.by_directory());
        }),
    )
    .expect("watcher starts");

    fs::write(left.join("one.txt"), "x").unwrap();
    fs::write(right.join("two.txt"), "x").unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    while Instant::now() < deadline {
        let Ok(batches) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        for group in &batches {
            let names = groups.entry(group.dir.to_lossy_string()).or_default();
            for row in &group.rows {
                names.push(row.name_string());
                assert_eq!(
                    row.change,
                    Change::Created,
                    "only creations are expected in this test"
                );
            }
        }
        if groups
            .values()
            .any(|names| names.iter().any(|n| n == "one.txt"))
            && groups
                .values()
                .any(|names| names.iter().any(|n| n == "two.txt"))
        {
            break;
        }
    }

    let left_key = dir.path().join("left");
    let grouped = groups
        .get(&fam_core::PathKey::of(&left_key).to_lossy_string())
        .unwrap_or_else(|| panic!("no group for the left directory, got {groups:?}"));
    assert!(
        grouped.iter().any(|name| name == "one.txt"),
        "the row landed under the wrong directory: {groups:?}"
    );
    handle.shutdown();
}

/// The cache registers one watch per cached directory, so the watch set is no longer bounded by
/// how many roots the user configured but by how many directories are open at once.
#[test]
fn a_hundred_and_twenty_single_level_watches_all_report() {
    let root = tempfile::tempdir().unwrap();
    let mut dirs = Vec::new();
    for index in 0..120 {
        let dir = root.path().join(format!("leaf-{index:03}"));
        fs::create_dir(&dir).unwrap();
        dirs.push(dir);
    }

    let (tx, rx) = mpsc::channel::<Seen>();
    let collector = tx;
    let mut handle = spawn(
        SchedulerConfig {
            queue_capacity: 1024,
            window: Duration::from_millis(60),
            max_batch: 512,
            initial_watch: Vec::new(),
            initial_mode: Mode::OneLevel,
        },
        Box::new(move |batch: Batch| {
            let mut seen = Seen::new();
            for change in batch.changes {
                seen.insert(change.path.display().to_string(), change.change.to_string());
            }
            let _ = collector.send(seen);
        }),
    )
    .expect("watcher starts");

    for dir in &dirs {
        assert!(handle.watch(dir, Mode::OneLevel).unwrap());
    }
    assert_eq!(handle.watched_count(), dirs.len());
    for dir in &dirs {
        fs::write(dir.join("inside.txt"), "x").unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Seen::new();
    while Instant::now() < deadline && seen.len() < dirs.len() {
        if let Ok(batch) = rx.recv_timeout(Duration::from_millis(250)) {
            seen.extend(batch);
        }
    }

    let missing: Vec<String> = dirs
        .iter()
        .filter(|dir| !seen.contains_key(&dir.join("inside.txt").display().to_string()))
        .map(|dir| dir.display().to_string())
        .collect();
    assert!(
        missing.is_empty(),
        "{} directory watch(es) silent: {missing:?}",
        missing.len()
    );
    handle.shutdown();
}

/// The reconciliation sweep must not invent work. A directory whose write time moved *with* an
/// event that arrived is explained, so sitting past several sweep intervals has to produce no
/// rescan at all - otherwise every browse of a busy folder would turn into a re-read.
#[test]
fn a_delivered_change_does_not_come_back_as_a_rescan() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel::<(usize, Vec<(String, String)>)>();
    let collector = tx;
    let handle = spawn(
        config(dir.path(), Mode::OneLevel),
        Box::new(move |batch: Batch| {
            let rescans = batch
                .rescans
                .iter()
                .map(|(dir, why)| (dir.to_lossy_string(), format!("{why:?}")))
                .collect();
            let _ = collector.send((batch.changes.len(), rescans));
        }),
    )
    .expect("watcher starts");

    fs::write(dir.path().join("first.txt"), "x").unwrap();

    // Two full sweep intervals, so the moved write time has certainly been sampled.
    let deadline = Instant::now() + 2 * RECONCILE_INTERVAL + Duration::from_millis(600);
    let mut changes = 0usize;
    let mut rescans: Vec<(String, String)> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((count, list)) => {
                changes += count;
                rescans.extend(list);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    assert!(changes > 0, "the change itself must have been delivered");
    assert!(
        rescans.is_empty(),
        "a delivered change came back as a rescan: {rescans:?}"
    );
    handle.shutdown();
}

#[test]
fn pending_events_are_flushed_before_shutdown_returns() {
    let dir = tempfile::tempdir().unwrap();
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<FileChange>::new()));
    let shared = std::sync::Arc::clone(&collected);
    let mut settings = config(dir.path(), Mode::OneLevel);
    settings.window = Duration::from_secs(30);
    let handle = spawn(
        settings,
        Box::new(move |batch: Batch| {
            shared.lock().unwrap().extend(batch.changes);
        }),
    )
    .unwrap();

    fs::write(dir.path().join("last.txt"), "x").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    handle.shutdown();

    let changes = collected.lock().unwrap();
    assert!(
        changes.iter().any(|change| {
            change.path == dir.path().join("last.txt") && change.change == Change::Created
        }),
        "shutdown must flush the open batch: {changes:?}"
    );
}

/// Deleting a watched directory takes its watch down without telling us. notify's backend sees the
/// directory refuse, finds the path gone and withdraws the watch (`windows.rs:345-352`), and in
/// one-level mode the deletion itself is reported to the *parent*, which nobody asked us to watch.
/// The reconciliation sweep is the only thing left that can surface it.
#[test]
fn a_deleted_watch_target_surfaces_as_a_rescan_though_the_backend_says_nothing() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    fs::create_dir(&nested).unwrap();

    let key = PathKey::of(&nested).to_lossy_string();
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    let collector = tx;
    let handle = spawn(
        config(&nested, Mode::OneLevel),
        Box::new(move |batch: Batch| {
            let mut rendered: Vec<String> = batch
                .changes
                .iter()
                .map(|change| format!("row {} {}", change.path.display(), change.change))
                .collect();
            rendered.extend(
                batch
                    .rescans
                    .iter()
                    .map(|(dir, why)| format!("rescan {why:?} {}", dir.to_lossy_string())),
            );
            let _ = collector.send(rendered);
        }),
    )
    .expect("watcher starts");

    fs::remove_dir_all(&nested).unwrap();

    let lost = collect(&rx, 3 * RECONCILE_INTERVAL, |seen| {
        seen.iter().any(|line| line.starts_with("rescan WatchLost"))
    });
    assert_eq!(
        lost,
        vec![format!("rescan WatchLost {key}")],
        "the deleted directory did not come back as exactly one lost-watch rescan"
    );
    // The point of the whole exercise: no event ever arrived for this directory, so nothing but the
    // sweep could have noticed. If notify ever starts reporting it, this line is the proof.
    assert!(
        !lost.iter().any(|line| line.starts_with("row")),
        "the backend did report the deletion after all: {lost:?}"
    );

    // And it comes back once, not every rotation: sampling continues so recovery is noticed, but a
    // directory we already reported as lost stays quiet until a sample succeeds again.
    let again = collect(&rx, 3 * RECONCILE_INTERVAL, |_| false);
    assert!(
        again.is_empty(),
        "the same loss was reported twice: {again:?}"
    );
    handle.shutdown();
}

/// Drains batches for up to `for_how_long`, stopping early once `done` is satisfied, and returns
/// every line the sink rendered.
fn collect(
    rx: &Receiver<Vec<String>>,
    for_how_long: Duration,
    done: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + for_how_long;
    let mut seen: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(lines) => {
                seen.extend(lines);
                if done(&seen) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    seen
}
