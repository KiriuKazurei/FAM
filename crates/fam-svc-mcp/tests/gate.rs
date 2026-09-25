//! The gate: what a credential may reach, and the four ways it can be refused.
//!
//! These are the tests the whole increment is for. The protocol works or it does not; what has to be
//! *proved* is that a token cannot be talked into a path it was not granted - by spelling, by
//! `..`, by a junction, or by being an admin.

mod common;

use common::{call, spelled, Bench, Place};
use fam_svc_auth::Tier;
use serde_json::json;
use std::path::Path;
use std::process::Command;

#[test]
fn a_sibling_folder_that_shares_a_prefix_is_not_reachable() {
    // `inbox` and `inbox-secret`: the pair a string-prefix comparison gets wrong, which is why the
    // gate compares path identities. The granted workspace is spelled so that the ungranted one is a
    // literal extension of it.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let neighbour = bench.root.join("inbox-secret");
    std::fs::create_dir_all(&neighbour).expect("a sibling");
    std::fs::write(neighbour.join("hidden.txt"), "not granted\n").expect("write");
    assert!(spelled(&neighbour).starts_with(&spelled(&bench.inbox)));

    let mut server = bench.server();
    let (answer, failure) = call(&mut server, "fs_list", json!({"path": spelled(&neighbour)}));
    assert!(failure, "the prefix trick worked: {answer}");
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("workspace"),
        "{answer}"
    );
}

#[test]
fn a_path_that_has_not_been_resolved_is_not_reachable() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let escapes = [
        format!("{}/../private", spelled(&bench.inbox)),
        format!("{}/../../etc", spelled(&bench.inbox)),
        spelled(&bench.root.join("inbox/./../private")),
    ];
    for path in escapes {
        let (answer, failure) = call(&mut server, "fs_list", json!({"path": path}));
        assert!(failure, "{path} was accepted: {answer}");
    }

    // And the trail must not tidy the attempt away. A target is stored as a path identity, which
    // folds case and separators - so the case that matters is whether it also *resolves* `..`. If
    // it did, an escape attempt would be recorded as a visit to a granted folder, and the one
    // surface a person uses to ask "what was it reaching for?" would answer wrong.
    let recorded: Vec<String> = bench
        .agent_lines()
        .iter()
        .filter(|line| line["op"] == "fs_list")
        .map(|line| line["target"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(recorded.len(), 3, "{recorded:?}");
    assert!(
        recorded.iter().all(|target| target.contains("/../")),
        "the trail resolved the dots away: {recorded:?}"
    );
    let granted = spelled(&bench.inbox).to_uppercase();
    assert!(
        recorded
            .iter()
            .all(|target| target.starts_with(&granted) && target != &granted),
        "a line stopped being the folder it was asked for: {recorded:?}"
    );
}

#[test]
fn even_an_admin_with_the_wildcard_cannot_read_the_programs_own_files() {
    // The item increment 11's review left for this moment: `Prune` is not consulted by a one-level
    // directory read, so once `fs_read` exists the credential file and the trail are reachable
    // through it unless the gate asks the question. Admin + `*` is the strongest credential there is,
    // and it still gets "no".
    let bench = Bench::at(Tier::Admin, &[Place::Wildcard]);
    let mut server = bench.server();
    let credential_file = bench.config.mcp_token_file.clone();
    let trail = fam_svc_audit::files(&bench.config.audit_dir)
        .expect("the trail has a file by now")
        .into_iter()
        .next()
        .expect("one file");

    for path in [credential_file.as_path(), trail.as_path()] {
        let (answer, failure) = call(&mut server, "fs_read", json!({"path": spelled(path)}));
        assert!(failure, "{} was readable: {answer}", path.display());
        assert!(
            answer["error"]
                .as_str()
                .expect("a sentence")
                .contains("own state"),
            "the refusal should say why it is not a scope problem: {answer}"
        );
        let (answer, failure) = call(
            &mut server,
            "fs_list",
            json!({"path": spelled(path.parent().expect("a parent"))}),
        );
        assert!(failure, "listing the parent worked: {answer}");
    }

    // The tag database is the third piece of state, and it is the one whose folder is not excluded:
    // it sits beside the configuration, so a *directory* rule either misses it or swallows the
    // folder the user is working in. Three names then, with their parent still listable - which is
    // where the difference from the two above lives.
    let database = bench.config.database_path.clone();
    let sidecars = ["-wal", "-shm"];
    let mut own = vec![database.clone()];
    own.extend(sidecars.iter().map(|suffix| {
        let name = format!(
            "{}{suffix}",
            database.file_name().expect("a file name").to_string_lossy()
        );
        database.with_file_name(name)
    }));
    for path in &own {
        let (answer, failure) = call(&mut server, "fs_read", json!({"path": spelled(path)}));
        assert!(failure, "{} was readable: {answer}", path.display());
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|message| message.contains("own state")),
            "{answer}"
        );
    }
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(database.parent().expect("a parent"))}),
    );
    assert!(
        !failure,
        "the folder holding the database has to stay listable: {answer}"
    );

    // And the refusals are in the trail, which is the point of refusing loudly rather than silently:
    // every `fs_read` this test issued was refused, so every one of them is an error line, and
    // nothing is claimed that did not happen.
    let lines = bench.agent_lines();
    let reads = lines
        .iter()
        .filter(|line| line["op"] == "fs_read")
        .collect::<Vec<_>>();
    assert_eq!(
        reads.len(),
        2 + own.len(),
        "one read per state file, plus the credential and the trail"
    );
    assert!(
        reads.iter().all(|line| line["result"]
            .as_str()
            .expect("a result")
            .starts_with("error ")),
        "a read of this program's own state was allowed: {reads:?}"
    );
}

#[test]
fn an_unscoped_search_needs_a_credential_that_covers_every_root() {
    // A token granted one folder must not be able to learn the file names in another by simply
    // leaving `within` out.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let (answer, failure) = call(&mut server, "fs_search", json!({"query": "secret"}));
    assert!(failure, "an unscoped search was allowed: {answer}");
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("every configured root"),
        "{answer}"
    );

    // The same call inside the granted folder is fine.
    let (answer, failure) = call(
        &mut server,
        "fs_search",
        json!({"query": "alpha", "within": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
}

#[test]
fn a_wildcard_credential_reaches_every_configured_root_and_no_further() {
    let bench = Bench::at(Tier::Admin, &[Place::Wildcard]);
    // A second temporary directory, which is by construction not under the first one: that is what
    // "outside every configured root" has to mean here, since the bench's own root is its temp dir.
    let elsewhere = tempfile::tempdir().expect("a folder outside every root");
    let outside = elsewhere.path().to_path_buf();
    std::fs::write(outside.join("private.txt"), "no\n").expect("write");

    let mut server = bench.server();
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.private)}),
    );
    assert!(
        !failure,
        "the wildcard should reach a sibling folder: {answer}"
    );
    let (answer, failure) = call(&mut server, "fs_list", json!({"path": spelled(&outside)}));
    assert!(failure, "the wildcard reached outside the roots: {answer}");
}

#[test]
fn a_delete_will_not_take_a_directory_that_still_has_something_in_it() {
    let bench = Bench::at(Tier::Edit, &[Place::Inbox]);
    let mut server = bench.server();
    let full = bench.inbox.join("full");
    let (answer, failure) = call(&mut server, "fs_delete", json!({"path": spelled(&full)}));
    assert!(failure, "a non-empty directory was deleted: {answer}");
    assert!(full.exists(), "and it is still on disk");
    // The sentence is the point, not just the refusal. `remove_dir` would fail on this call too, with
    // a system message in the machine's locale, so an assertion that only checks "it failed" passes
    // with or without the tool's own guard - and the guard is what keeps the recursive call from
    // being written later by someone treating the system error as the bug.
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|message| message.contains("still has entries in it")),
        "that failure came from the operating system, not from the tool: {answer}"
    );

    // The empty one goes, because there is nothing left to lose.
    let (answer, failure) = call(
        &mut server,
        "fs_delete",
        json!({"path": spelled(&bench.inbox.join("empty"))}),
    );
    assert!(!failure, "{answer}");
    assert!(!bench.inbox.join("empty").exists());

    // A move never overwrites.
    let target = bench.inbox.join("alpha.txt");
    let (answer, failure) = call(
        &mut server,
        "fs_move",
        json!({"from": spelled(&bench.inbox.join("beta.md")), "to": spelled(&target)}),
    );
    assert!(failure, "an existing target was overwritten: {answer}");
    assert!(
        bench.inbox.join("beta.md").exists(),
        "and the source is intact"
    );

    // The same call onto a name that is free: the file is really not there any more, and the answer
    // says which way the bytes moved. Nothing else in this suite exercised the success path, so
    // before this line the whole of `fs_move` past its two gates was untested.
    let moved = bench.inbox.join("renamed.md");
    let (answer, failure) = call(
        &mut server,
        "fs_move",
        json!({"from": spelled(&bench.inbox.join("beta.md")), "to": spelled(&moved)}),
    );
    assert!(!failure, "{answer}");
    assert_eq!(answer["moved"], true, "{answer}");
    assert_eq!(answer["by"], "renamed", "one volume: {answer}");
    assert!(!bench.inbox.join("beta.md").exists(), "the source is gone");
    assert_eq!(
        std::fs::read_to_string(&moved).expect("read"),
        "# heading\n\nbody text\n"
    );
}

#[test]
fn a_note_is_not_a_tag_and_neither_of_them_is_a_search_result() {
    let bench = Bench::at(Tier::Edit, &[Place::Inbox]);
    let mut server = bench.server();
    let target = bench.inbox.join("alpha.txt");
    call(
        &mut server,
        "note_set",
        json!({"path": spelled(&target), "body": "the one with the diagram"}),
    );
    let (answer, failure) = call(&mut server, "tags_get", json!({"path": spelled(&target)}));
    assert!(!failure, "{answer}");
    assert_eq!(answer["tags"].as_array().expect("a list").len(), 0);

    let (answer, failure) = call(
        &mut server,
        "fs_search",
        json!({"query": "diagram", "within": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
    assert_eq!(
        answer["hits"].as_array().expect("hits").len(),
        0,
        "a note body leaked into a name search"
    );

    // An empty body is refused rather than treated as a clear, and the note stays.
    let (answer, failure) = call(
        &mut server,
        "note_set",
        json!({"path": spelled(&target), "body": "   "}),
    );
    assert!(failure, "{answer}");
    let (answer, _) = call(&mut server, "note_get", json!({"path": spelled(&target)}));
    assert_eq!(answer["body"], "the one with the diagram");
    let (answer, failure) = call(&mut server, "note_clear", json!({"path": spelled(&target)}));
    assert!(!failure, "{answer}");
    assert_eq!(answer["removed"], true);
}

#[test]
fn a_changed_credential_file_stops_the_running_server() {
    // Revocation is immediate for a new connection. For one already open, the server refuses rather
    // than answering from a table it can no longer be sure matches the file - because the way that
    // goes wrong is a revoked agent still working.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let (answer, failure) = call(&mut server, "whoami", json!({}));
    assert!(!failure, "{answer}");

    let mut store = fam_svc_auth::TokenStore::open(&bench.config).expect("the store opens");
    store
        .revoke(&bench.token_id, std::time::SystemTime::now())
        .expect("revoked while the server is running");
    drop(store);

    let (answer, failure) = call(&mut server, "whoami", json!({}));
    assert!(
        failure,
        "the server kept serving after the file changed: {answer}"
    );
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("restart"),
        "{answer}"
    );
}

#[test]
fn an_unscoped_search_is_refused_when_a_second_root_is_not_granted() {
    // `Router::allows` lets *any* configured root produce rows once no scope is named, so the gate
    // has to ask about all of them. Asking about `search_roots[0]` was the bug this pins - and the
    // bench is built so that the first root *is* covered by the credential, because a token that
    // covers neither root is refused by both the broken and the fixed code and the test would prove
    // nothing either way.
    let bench = Bench::at(Tier::Browse, &[Place::Root]);
    let second = tempfile::tempdir().expect("a second configured root");
    std::fs::write(second.path().join("quarterly-report.txt"), "x\n").expect("write");
    let mut two = bench.config.clone();
    two.search_roots = vec![bench.root.clone(), second.path().to_path_buf()];
    let mut server = fam_svc_mcp::Server::authenticate(two, &bench.credential)
        .expect("the credential this bench issued still opens a session");

    let (answer, failure) = call(&mut server, "fs_search", json!({"query": "quarterly"}));
    assert!(failure, "the ungranted root was searchable: {answer}");
    assert!(
        !answer.to_string().contains("quarterly-report"),
        "the hit came back with the refusal: {answer}"
    );
    let message = answer["error"].as_str().expect("a sentence");
    assert!(
        !message.contains(&spelled(second.path())),
        "the refusal named a root this credential was not granted: {message}"
    );

    // Scoped to what was granted, the same call answers normally - the refusal is about the scope,
    // not about searching.
    let (answer, failure) = call(
        &mut server,
        "fs_search",
        json!({"query": "alpha", "within": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
}

#[cfg(windows)]
#[test]
fn a_junction_is_not_a_door_into_the_programs_own_state() {
    // The exclusion is asked about the caller's spelling, and for a path with no links on it that
    // spelling *is* the path. Put a junction in the granted folder pointing at `state/` and the
    // answer to "is this my own state?" changes halfway through the request - which is the same
    // mistake that let the credential file be read at all one increment ago, arrived at from the
    // other side.
    let bench = Bench::at(Tier::Admin, &[Place::Wildcard]);
    let link = bench.inbox.join("state-door");
    let made = Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(bench.state())
        .output()
        .expect("cmd.exe runs");
    assert!(
        made.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );

    let mut server = bench.server();
    for path in [
        link.join("auth/mcp-tokens.toml"),
        link.join("audit"),
        link.join("tags.db"),
    ] {
        let (answer, failure) = call(&mut server, "fs_read", json!({"path": spelled(&path)}));
        assert!(failure, "through the junction it was readable: {answer}");
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|message| message.contains("own state")),
            "the refusal should be the exclusion, not a scope complaint: {answer}"
        );
    }
    // The junction itself is still listed - it is a name inside the granted folder - with its
    // reparse flag set, so the caller is told rather than silently blocked.
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
    let row = answer["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|row| row["name"] == "state-door")
        .expect("the junction is listed");
    assert_eq!(row["reparse"], true, "{row}");
    let _ = std::fs::remove_dir(&link);
}

#[cfg(windows)]
#[test]
fn a_link_below_a_link_is_still_a_link() {
    // The walk used to stop at the first reparse point and ask one question about it: does *that*
    // link stay inside? Two links answer it - the first points into a folder that is still granted,
    // the second points out - and the components below the first were never looked at.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let outside = bench.temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("a folder to point at");
    std::fs::write(outside.join("crown-jewel.txt"), "not granted\n").expect("write");
    let inner = bench.inbox.join("inner");
    std::fs::create_dir_all(&inner).expect("inner");
    let mklink = |name: &std::path::Path, target: &std::path::Path| {
        let made = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(name)
            .arg(target)
            .output()
            .expect("cmd.exe runs");
        assert!(
            made.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
    };
    let hop_one = bench.inbox.join("via-one");
    mklink(&hop_one, &inner);
    let hop_two = inner.join("via-two");
    mklink(&hop_two, &outside);

    let mut server = bench.server();
    let escaping = hop_one.join("via-two/crown-jewel.txt");
    let (answer, failure) = call(&mut server, "fs_read", json!({"path": spelled(&escaping)}));
    assert!(
        failure,
        "two links carried the token out of the grant: {answer}"
    );
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|message| message.contains("leaves the granted workspace")),
        "{answer}"
    );

    // The same escape spelled the way the verbatim Windows API wants it. `covers` folds `\\?\`
    // away, so this is still in scope, but the component walk cannot line it up with the granted
    // root by counting parts - when the two do not align the gate resolves the whole path and
    // judges that, rather than finding no links to check.
    let verbatim = format!(r"\\?\{}", escaping.display());
    let (answer, failure) = call(&mut server, "fs_read", json!({"path": verbatim}));
    assert!(
        failure,
        "a verbatim spelling walked past the check: {answer}"
    );

    // And a chain that never leaves stays allowed: the check is about leaving, not about links.
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.inbox.join("via-one"))}),
    );
    assert!(!failure, "a link that stays inside was refused: {answer}");
    let _ = std::fs::remove_dir(&hop_two);
    let _ = std::fs::remove_dir(&hop_one);
    let outside: &Path = outside.as_path();
    assert!(outside.exists());
}
/// The junction test. A junction is a Windows reparse point that needs no privilege to create, and
/// it is exactly the shape the `covers` check cannot see: the path still starts with the granted
/// folder.
///
/// If `mklink /J` ever fails on a machine, this test fails rather than skipping: a gate whose only
/// link test quietly does not run is a gate nobody has tested.
#[cfg(windows)]
#[test]
fn a_junction_out_of_the_workspace_does_not_open_it() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let outside = bench.temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("a folder to point at");
    std::fs::write(outside.join("crown-jewel.txt"), "not granted\n").expect("write");

    let link = bench.inbox.join("via-junction");
    let made = Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .expect("cmd.exe runs");
    assert!(
        made.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    assert!(link.is_dir(), "the junction was not created");

    let mut server = bench.server();
    // The link itself is inside the workspace and is listed as a reparse point - the caller is told,
    // not silently shown a folder that is really somewhere else.
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
    let row = answer["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|row| row["name"] == "via-junction")
        .expect("the junction is listed");
    assert_eq!(row["reparse"], true, "{row}");

    // Through it is not.
    let (answer, failure) = call(&mut server, "fs_list", json!({"path": spelled(&link)}));
    assert!(failure, "the junction let the token out: {answer}");
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("leaves the granted workspace"),
        "{answer}"
    );
    let (answer, failure) = call(
        &mut server,
        "fs_read",
        json!({"path": spelled(&link.join("crown-jewel.txt"))}),
    );
    assert!(failure, "a file beyond the junction was read: {answer}");

    // A junction that stays inside the workspace is not refused - the check is about leaving, not
    // about links as such.
    let inside = bench.inbox.join("inner-link");
    let made = Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&inside)
        .arg(bench.inbox.join("full"))
        .output()
        .expect("cmd.exe runs");
    assert!(made.status.success());
    let (answer, failure) = call(&mut server, "fs_list", json!({"path": spelled(&inside)}));
    assert!(!failure, "a link that stays inside was refused: {answer}");
    assert_eq!(answer["entries"][0]["name"], "inner.txt");
    let _ = std::fs::remove_dir(&inside);
    let _ = std::fs::remove_dir(&link);
    let outside: &Path = outside.as_path();
    assert!(outside.exists());
}

#[cfg(not(windows))]
#[test]
fn a_symlink_out_of_the_workspace_does_not_open_it() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let outside = bench.temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("a folder to point at");
    std::fs::write(outside.join("crown-jewel.txt"), "not granted\n").expect("write");
    let link = bench.inbox.join("via-symlink");
    if std::os::unix::fs::symlink(&outside, &link).is_err() {
        return; // No permission to make a link here; the Windows branch covers the same code path.
    }
    let mut server = bench.server();
    let (answer, failure) = call(
        &mut server,
        "fs_read",
        json!({"path": spelled(&link.join("crown-jewel.txt"))}),
    );
    assert!(failure, "a symlink let the token out: {answer}");
}

/// The two guards in `fs_search` that answer from the *configuration* rather than from the request.
/// Both were written because indexing `search_roots[0]` on an empty list is a panic, and neither
/// means anything until a test builds that configuration - which is why this one does.
#[test]
fn a_configuration_with_no_roots_is_answered_not_panicked() {
    let bench = Bench::at(Tier::Admin, &[Place::Wildcard]);
    let mut rootless = bench.config.clone();
    rootless.search_roots.clear();
    let mut server = fam_svc_mcp::Server::authenticate(rootless, &bench.credential)
        .expect("a credential still verifies when there is nothing to search");

    let (answer, failure) = call(&mut server, "fs_search", json!({"query": "anything"}));
    assert!(failure, "an empty root list was searched: {answer}");
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|message| message.contains("no configured roots")),
        "that answer did not come from the guard: {answer}"
    );

    // The guard one line above it answers for a blank query, which would otherwise reach the router
    // and report "0 hits, complete" for a search that never asked anything.
    let (answer, failure) = call(&mut server, "fs_search", json!({"query": "   "}));
    assert!(failure, "a blank query was accepted: {answer}");
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|message| message.contains("must say something")),
        "that answer did not come from the guard: {answer}"
    );
}
