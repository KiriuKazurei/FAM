use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Exercises the real binary so the wiring between config, store, walk and scheduler is covered,
/// not just each layer on its own.
struct Cli {
    root: PathBuf,
    db: PathBuf,
    /// Isolated `audit_dir` and `mcp_token_file`. Nothing in this file may touch the real ones: a
    /// test that wrote into the user's audit log would be a test that can forge a user's trail, and
    /// one that read their credential file would be reading their live grants.
    config: PathBuf,
    audit: PathBuf,
    tokens: PathBuf,
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
        let audit = temp.path().join("state/audit");
        fs::create_dir_all(temp.path().join("state")).unwrap();
        let tokens = temp.path().join("state/auth/mcp-tokens.toml");
        let config = temp.path().join("state/config.toml");
        fs::write(
            &config,
            // Forward slashes: a Windows path in a TOML *basic* string would be read as escapes,
            // and both `toml` and the Windows APIs accept the other spelling.
            format!(
                "database_path = \"{}\"
search_roots = [\"{}\"]
audit_dir = \"{}\"
mcp_token_file = \"{}\"
",
                toml_path(&temp.path().join("data/tags.db")),
                toml_path(&root),
                toml_path(&audit),
                toml_path(&tokens)
            ),
        )
        .unwrap();
        Self {
            audit,
            config,
            db: temp.path().join("data/tags.db"),
            root,
            tokens,
            _temp: temp,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fam"));
        command
            .arg("--config")
            .arg(&self.config)
            .arg("--db")
            .arg(&self.db);
        command
    }

    /// The audit lines the isolated trail holds, newest session last.
    fn audit_lines(&self) -> Vec<String> {
        let entries = match fs::read_dir(&self.audit) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if let Ok(text) = fs::read_to_string(entry.path()) {
                out.extend(text.lines().map(str::to_owned));
            }
        }
        out
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

/// Forward slashes: a Windows path in a TOML *basic* string would be read as escapes, and both
/// `toml` and the Windows APIs accept the other spelling.
fn toml_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace(std::path::MAIN_SEPARATOR_STR, "/")
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

    for command in [
        "config", "scan", "search", "tag", "find", "watch", "stats", "mcp", "audit",
    ] {
        assert!(
            text.contains(command),
            "{command} missing from help:\n{text}"
        );
    }
}

/// The whole human-facing credential story, through the real binary.
#[test]
fn a_token_is_issued_listed_rotated_and_revoked() {
    let cli = Cli::new();

    let issued = cli.stdout(&[
        "mcp",
        "token",
        "add",
        "notes-reader",
        "--workspace",
        &toml_path(&cli.root),
    ]);
    let credential = credential_in(&issued);
    let id = credential.split('_').nth(1).expect("fam_id_secret");

    let listed = cli.stdout(&["mcp", "token", "list"]);
    assert!(listed.contains("notes-reader"), "{listed}");
    assert!(listed.contains("browse"), "{listed}");
    assert!(listed.contains(id), "{listed}");
    assert!(
        !listed.contains(&credential),
        "the secret is shown once, and `list` is not that time: {listed}"
    );

    let rotated = cli.stdout(&["mcp", "token", "rotate", id]);
    let second = credential_in(&rotated);
    assert_ne!(second, credential, "a rotation is a new secret");
    assert!(second.contains(id), "and the same identity: {second}");
    assert!(rotated.contains("the previous secret stopped working"));

    let revoked = cli.stdout(&["mcp", "token", "revoke", id]);
    assert!(revoked.contains("revoked"), "{revoked}");
    let listed = cli.stdout(&["mcp", "token", "list"]);
    assert!(
        listed.contains("gone"),
        "the row stays and says so: {listed}"
    );
    assert!(
        !listed.contains(&second),
        "and the new secret is not readable back out either"
    );

    cli.failure(&["mcp", "token", "revoke", id]);
    cli.failure(&["mcp", "token", "rotate", id]);
    cli.failure(&["mcp", "token", "add", "notes-reader", "--workspace", "D:/x"]);
}

/// What the file itself holds, which is the difference between "hashed" as a claim and as a fact.
#[test]
fn the_credential_file_holds_a_digest_and_never_a_secret() {
    let cli = Cli::new();
    let issued = cli.stdout(&["mcp", "token", "add", "quiet", "--workspace", "D:/notes"]);
    let credential = credential_in(&issued);
    let secret = credential
        .rsplit('_')
        .next()
        .expect("the tail is the secret");

    let text = fs::read_to_string(&cli.tokens).expect("the file was written");
    assert!(text.contains("secret_hash"), "{text}");
    assert!(
        text.contains("salt"),
        "per-token, so rows cannot be compared"
    );
    assert!(
        !text.contains(secret),
        "the plaintext is nowhere in the file"
    );
    assert!(!text.contains(&secret[..24]), "nor is any prefix of it");
    assert!(!cli.audit_lines().iter().any(|line| line.contains(secret)));
}

/// A grant that was refused still gets its line, and the trail as a whole still verifies - which is
/// the same rule the ABI follows, applied to the commands that change who may ask.
#[test]
fn a_refused_grant_is_recorded_and_the_trail_still_verifies() {
    let cli = Cli::new();
    let error = cli.failure(&[
        "mcp",
        "token",
        "add",
        "wide",
        "--tier",
        "edit",
        "--workspace",
        "*",
    ]);
    assert!(error.contains("only an admin token may hold"), "{error}");

    let lines = cli.audit_lines();
    let refused: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"token.add\""))
        .collect();
    assert_eq!(refused.len(), 1, "one attempt, one line: {lines:?}");
    assert!(refused[0].contains("error only an admin"), "{}", refused[0]);
    assert!(
        refused[0].contains("label=wide") && refused[0].contains("tier=edit"),
        "the line says what was asked for: {}",
        refused[0]
    );

    cli.stdout(&["mcp", "token", "add", "real", "--workspace", "D:/notes"]);
    let text = cli.stdout(&["audit", "verify"]);
    assert!(text.contains("chain is unbroken"), "{text}");
    assert!(
        cli.audit_lines()
            .iter()
            .filter(|line| line.contains("\"session.open\""))
            .count()
            >= 2,
        "each command is its own session, and each one sealed itself"
    );
}

#[test]
fn a_token_without_a_workspace_is_refused_before_anything_is_written() {
    let cli = Cli::new();
    let error = cli.failure(&["mcp", "token", "add", "homeless"]);
    assert!(error.contains("--workspace"), "{error}");
    assert!(!cli.tokens.exists(), "and there is no file to read back");
}

#[test]
fn a_relative_workspace_is_refused_because_it_could_never_match_a_path() {
    let cli = Cli::new();
    let error = cli.failure(&["mcp", "token", "add", "near", "--workspace", "notes"]);
    assert!(error.contains("not an absolute path"), "{error}");
}

/// Two refusals are made by the command's own argument checking, before the store is asked
/// anything: a tier that is not a tier, and no `--workspace` at all. Neither one reaches the store's
/// recording path, so unless the command writes the line itself the trail shows a session that did
/// nothing - which is exactly what a grant attempt that was turned away looks like.
#[test]
fn a_refusal_the_command_makes_itself_still_gets_a_line() {
    let cli = Cli::new();
    cli.failure(&["mcp", "token", "add", "homeless"]);
    cli.failure(&[
        "mcp",
        "token",
        "add",
        "super",
        "--tier",
        "superadmin",
        "--workspace",
        "D:/notes",
    ]);

    let lines = cli.audit_lines();
    let refused: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"token.add\"") && line.contains("\"error"))
        .collect();
    assert_eq!(refused.len(), 2, "two refusals, two lines: {lines:?}");
    assert!(
        refused
            .iter()
            .any(|line| line.contains("label=homeless") && line.contains("at least one workspace")),
        "the empty-workspace refusal says so: {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|line| line.contains("tier=superadmin") && line.contains("unknown tier")),
        "the bad tier says so: {refused:?}"
    );
}

/// What the two above add up to: a credential session always carries at least one op line, so a
/// bare `session.open` + `session.close` pair is a crash or a truncated tail, never normal traffic.
#[test]
fn no_credential_session_is_empty() {
    let cli = Cli::new();
    cli.stdout(&["mcp", "token", "add", "reader", "--workspace", "D:/notes"]);
    cli.stdout(&["mcp", "token", "list"]);
    cli.failure(&["mcp", "token", "revoke", "ffffffff"]);
    let id =
        credential_in(&cli.stdout(&["mcp", "token", "add", "gone", "--workspace", "D:/notes"]));
    let id = id.split('_').nth(1).unwrap().to_owned();
    cli.stdout(&["mcp", "token", "rotate", &id]);
    cli.stdout(&["mcp", "token", "revoke", &id]);

    let lines = cli.audit_lines();
    let mut ops: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut opens = 0u64;
    for line in &lines {
        let Some(session) = session_of(line) else {
            continue;
        };
        if line.contains("\"session.open\"") {
            opens += 1;
            continue;
        }
        if line.contains("\"session.close\"") {
            continue;
        }
        *ops.entry(session).or_default() += 1;
    }
    assert_eq!(opens, 6, "six commands, six sessions: {lines:?}");
    assert!(
        ops.values().all(|count| *count >= 1),
        "every session carries an op line: {ops:?}"
    );
    assert_eq!(ops.len() as u64, opens, "and none of them is bare: {ops:?}");
}

/// The `session` field of a line, as text - it is a number in the JSON, so it is read as one.
fn session_of(line: &str) -> Option<String> {
    let marker = "\"session\":";
    let rest = line.find(marker).map(|at| &line[at + marker.len()..])?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()).then_some(digits)
}

/// The one secret-shaped string in `add`'s output.
fn credential_in(text: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("credential "))
        .map(str::to_owned)
        .expect("a credential was printed")
        .trim()
        .to_owned()
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
