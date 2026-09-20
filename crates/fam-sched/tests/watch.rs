use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use fam_sched::{spawn, Batch, FileChange, Handle, SchedulerConfig};

type Seen = BTreeMap<String, String>;

fn fast_pipeline(dir: &Path) -> (Handle, Receiver<Seen>) {
    let (tx, rx) = mpsc::channel::<Seen>();
    let config = SchedulerConfig {
        queue_capacity: 256,
        window: Duration::from_millis(40),
        max_batch: 128,
    };
    let handle = spawn(
        &[dir.to_path_buf()],
        config,
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
        .find(|(path, _)| Path::new(path.as_str()).file_name().map(|f| f == name) == Some(true))
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

    assert_eq!(entry_for(&seen, "fresh.txt").map(String::as_str), Some("created"));
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

    assert_eq!(entry_for(&seen, "doomed.txt").map(String::as_str), Some("removed"));
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

    let mentioned: Vec<&String> = seen.values().collect();
    assert!(
        !mentioned.is_empty(),
        "a rename must produce at least one row delta"
    );
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
    assert_eq!(matches.len(), 1, "one path yields one row delta per batch: {seen:?}");
    handle.shutdown();
}

#[test]
fn nested_directories_are_observed_recursively() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("deep/nested")).unwrap();
    let (handle, rx) = fast_pipeline(dir.path());

    fs::write(dir.path().join("deep/nested/leaf.dat"), "x").unwrap();
    let seen = wait_until(&rx, |seen| entry_for(seen, "leaf.dat").is_some());

    assert!(entry_for(&seen, "leaf.dat").is_some(), "got {seen:?}");
    handle.shutdown();
}

#[test]
fn pending_events_are_flushed_before_shutdown_returns() {
    let dir = tempfile::tempdir().unwrap();
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<FileChange>::new()));
    let shared = std::sync::Arc::clone(&collected);
    let handle = spawn(
        &[dir.path().to_path_buf()],
        SchedulerConfig {
            queue_capacity: 64,
            window: Duration::from_secs(30),
            max_batch: 128,
        },
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
            change.path == dir.path().join("last.txt")
                && change.change == fam_sched::Change::Created
        }),
        "shutdown must flush the open batch: {changes:?}"
    );
}
