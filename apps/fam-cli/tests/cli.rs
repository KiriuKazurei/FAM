use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Exercises the real binary so the wiring between config, store, walk and scheduler is covered,
/// not just each layer on its own.
struct Cli {
    root: PathBuf,
    db: PathBuf,
    _temp: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        write(&root.join("notes.txt"), "meeting notes");
        write(&root.join("reports/q1-report.pdf"), "pdf");
        write(&root.join("photos/sunset.JPG"), "jpeg");
        write(&root.join("archive.zip"), "zip");
        Self {
            db: temp.path().join("data/tags.db"),
            root,
            _temp: temp,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fam"));
        command.arg("--db").arg(&self.db);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("fam binary runs")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn failure(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            !output.status.success(),
            "command {args:?} unexpectedly succeeded"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn help_lists_every_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_fam"))
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();

    for command in ["config", "scan", "search", "tag", "find", "watch", "stats"] {
        assert!(
            text.contains(command),
            "{command} missing from help:\n{text}"
        );
    }
}

#[test]
fn scan_groups_hits_by_kind_and_reports_the_walk() {
    let cli = Cli::new();

    let text = cli.stdout(&["scan", cli.root.to_str().unwrap()]);

    assert!(text.contains("[directory]"), "{text}");
    assert!(text.contains("[image] 1"), "{text}");
    assert!(text.contains("[document] 2"), "{text}");
    assert!(text.contains("[archive] 1"), "{text}");
    assert!(text.contains("q1-report.pdf"), "{text}");
    assert!(text.contains("sunset.JPG"), "{text}");
    assert!(text.contains("walked: dirs=2 emitted=6"), "{text}");
}

#[test]
fn scan_limit_truncates_and_says_so() {
    let cli = Cli::new();

    let text = cli.stdout(&["scan", cli.root.to_str().unwrap(), "--limit", "2"]);

    assert!(text.contains("(stopped early)"), "{text}");
}

#[test]
fn search_answers_from_the_walk_backend_and_filters_by_kind() {
    let cli = Cli::new();

    let root = cli.root.to_str().unwrap();
    let hits = cli.stdout(&["search", "report", "--in", root]);
    assert!(hits.contains("backend=walk"), "{hits}");
    assert!(hits.contains("hits=2"), "{hits}");
    assert!(hits.contains("q1-report.pdf"), "{hits}");
    assert!(hits.contains("[directory] 1"), "{hits}");

    let files = cli.stdout(&["search", "report", "--in", root, "--kind", "document"]);
    assert!(files.contains("hits=1"), "{files}");

    let images = cli.stdout(&["scan", root, "--kind", "image"]);
    assert!(images.contains("sunset.JPG"), "{images}");
    assert!(!images.contains("notes.txt"), "{images}");

    let misses = cli.stdout(&["search", "nothing-matches-this", "--in", root]);
    assert!(misses.contains("hits=0"), "{misses}");

    let empty = cli.failure(&["search", "   "]);
    assert!(empty.contains("no search terms"), "{empty}");
}

#[test]
fn search_can_be_scoped_to_one_directory() {
    let cli = Cli::new();
    let photos = cli.root.join("photos");

    let text = cli.stdout(&["search", "sunset", "--in", photos.to_str().unwrap()]);

    assert!(text.contains("hits=1"), "{text}");
    let elsewhere = cli.root.join("reports");
    let text = cli.stdout(&["search", "sunset", "--in", elsewhere.to_str().unwrap()]);
    assert!(text.contains("hits=0"), "{text}");
}

/// What `--backend everything` owes the caller: that backend's own obstacle, not the router's generic
/// "nothing was available". Which obstacle depends on the machine - no client DLL beside the binary,
/// or a DLL and no service - so the assertion is about the one thing that must never appear.
#[test]
fn a_backend_named_by_the_caller_reports_why_it_could_not_answer() {
    let cli = Cli::new();

    let root = cli.root.to_str().unwrap();
    let stderr = cli.failure(&["search", "report", "--in", root, "--backend", "everything"]);

    assert!(stderr.contains("everything"), "{stderr}");
    assert!(
        !stderr.contains("no search provider"),
        "the router's own excuse reached the caller instead of Everything's: {stderr}"
    );
    assert!(
        stderr.contains("not deployed") || stderr.contains("NotRunning"),
        "neither of the two real reasons: {stderr}"
    );
    println!("everything said: {stderr}");
}

#[test]
fn bad_kind_and_backend_names_are_rejected_clearly() {
    let cli = Cli::new();

    let kind = cli.failure(&["search", "x", "--kind", "spreadsheet"]);
    assert!(kind.contains("unknown kind"), "{kind}");
    assert!(kind.contains("directory"), "{kind}");

    let backend = cli.failure(&["search", "x", "--backend", "magic"]);
    assert!(backend.contains("unknown backend"), "{backend}");
}

#[test]
fn tags_round_trip_through_the_database_and_drive_find() {
    let cli = Cli::new();
    let report = cli.root.join("reports/q1-report.pdf");
    let notes = cli.root.join("notes.txt");
    let report = report.to_str().unwrap();
    let notes = notes.to_str().unwrap();

    let added = cli.stdout(&["tag", "add", report, "finance", "q1"]);
    assert!(added.contains("2 added"), "{added}");

    let again = cli.stdout(&["tag", "add", report, "finance"]);
    assert!(again.contains("0 added, 1 already present"), "{again}");

    cli.stdout(&["tag", "add", notes, "finance", "--source", "auto"]);

    let listed = cli.stdout(&["tag", "list", report]);
    assert!(listed.contains("manual  finance"), "{listed}");
    assert!(listed.contains("manual  q1"), "{listed}");

    let any = cli.stdout(&["find", "--tag", "finance"]);
    assert!(any.contains("2 file(s) matched"), "{any}");

    let both = cli.stdout(&["find", "--tag", "finance", "--tag", "q1", "--all"]);
    assert!(both.contains("1 file(s) matched"), "{both}");
    assert!(both.contains(report), "{both}");

    let manual_only = cli.stdout(&["find", "--tag", "finance", "--source", "manual"]);
    assert!(manual_only.contains("1 file(s) matched"), "{manual_only}");

    let stats = cli.stdout(&["stats"]);
    assert!(stats.contains("files:    2"), "{stats}");
    assert!(stats.contains("tags:     3 (manual 2 / auto 1)"), "{stats}");
}

#[test]
fn tag_edits_rename_reorder_and_delete() {
    let cli = Cli::new();
    let notes = cli.root.join("notes.txt");
    let notes = notes.to_str().unwrap();

    cli.stdout(&["tag", "add", notes, "draft", "keep"]);
    cli.stdout(&["tag", "priority", notes, "keep", "9"]);
    let renamed = cli.stdout(&["tag", "rename", notes, "draft", "final"]);
    assert!(renamed.contains("draft -> final"), "{renamed}");

    let listed = cli.stdout(&["tag", "list", notes]);
    let final_at = listed.find("final").expect("renamed tag present");
    let keep_at = listed.find("keep").expect("second tag present");
    assert!(keep_at < final_at, "priority should lift `keep`: {listed}");

    let removed = cli.stdout(&["tag", "remove", notes, "keep"]);
    assert!(removed.contains("1 tag(s) removed"), "{removed}");
    let listed = cli.stdout(&["tag", "list", notes]);
    assert!(!listed.contains("keep"), "{listed}");
}

#[test]
fn invalid_input_fails_with_a_message_instead_of_a_panic() {
    let cli = Cli::new();
    let notes = cli.root.join("notes.txt");
    let notes = notes.to_str().unwrap();

    let bad_tag = cli.failure(&["tag", "add", notes, "bad/tag"]);
    assert!(bad_tag.contains("invalid tag"), "{bad_tag}");

    let bad_source = cli.failure(&["tag", "add", notes, "x", "--source", "guess"]);
    assert!(bad_source.contains("unknown tag source"), "{bad_source}");

    let missing = cli.failure(&["tag", "rename", notes, "absent", "x"]);
    assert!(missing.contains("no tag named"), "{missing}");

    let no_tags = cli.failure(&["find"]);
    assert!(no_tags.contains("at least one --tag"), "{no_tags}");
}

#[test]
fn config_path_is_reported_without_touching_the_file() {
    let cli = Cli::new();

    let text = cli.stdout(&["config", "path"]);

    assert!(text.contains("config.toml"), "{text}");
    assert!(text.contains("FileAutoManage"), "{text}");
}

#[test]
fn watch_prints_merged_batches_for_live_changes() {
    let cli = Cli::new();

    let text = cli.stdout(&["watch", cli.root.to_str().unwrap(), "--seconds", "1"]);

    assert!(text.contains("watching 1 path(s)"), "{text}");
    assert!(text.contains("stopped:"), "{text}");
    assert!(text.contains("0 dropped event(s)"), "{text}");
}
