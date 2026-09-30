//! The protocol surface: the handshake, the tool list, and what the server is allowed to advertise.
//!
//! The snapshot test is the conformance check the plan asks for - "a handshake and a tools list
//! snapshot" - and the reverse assertion beside it is the one the security argument rests on: there
//! is no credential, permission or audit tool here, shown by the list containing nothing of the kind
//! rather than by the server promising to refuse one.

mod common;

use common::{call, request, spelled, Bench, Place};
use fam_svc_auth::{Tier, Verb};
use fam_svc_mcp::{Server, PROTOCOL_VERSION, TOOLS};
use serde_json::{json, Value};

/// The path as the tools spell it: one separator style, whatever the caller used.
fn path_string_local(path: &std::path::Path) -> String {
    let rebuilt: std::path::PathBuf = path.components().collect();
    rebuilt.display().to_string()
}

/// The names a client sees, in the order the table declares them.
const NAMES: [&str; 15] = [
    "fs_list",
    "fs_read",
    "fs_search",
    "fs_write",
    "fs_create",
    "fs_delete",
    "fs_move",
    "tags_get",
    "tags_set",
    "tags_remove",
    "tags_merge",
    "note_get",
    "note_set",
    "note_clear",
    "whoami",
];

#[test]
fn every_schema_is_json_and_shaped_like_an_input_schema() {
    for tool in TOOLS {
        let schema = tool.schema_json();
        assert_eq!(schema["type"], "object", "{}", tool.name);
        assert!(
            schema.get("properties").is_some(),
            "{} advertises no properties",
            tool.name
        );
        assert_eq!(
            schema["additionalProperties"], false,
            "{} accepts arguments it does not describe",
            tool.name
        );
        assert!(
            !tool.description.is_empty(),
            "{} has no description",
            tool.name
        );
    }
}

/// The invariant the "credentials are human-only" argument rests on, stated in code: the verb
/// whitelist, the tool table and the audit operation names are one vocabulary, not three.
#[test]
fn the_tool_table_is_the_verb_list_spelled_the_same_way() {
    assert_eq!(
        TOOLS.len(),
        Verb::ALL.len(),
        "a verb with no tool, or a tool with no verb"
    );
    for verb in Verb::ALL {
        let tool = TOOLS
            .iter()
            .find(|tool| tool.verb == *verb)
            .unwrap_or_else(|| panic!("no tool for {}", verb.name()));
        assert_eq!(tool.name, verb.name(), "{} named differently", verb.name());
    }
}

#[test]
fn the_advertised_tools_are_the_files_and_nothing_else() {
    let bench = Bench::at(Tier::Admin, &[Place::Wildcard]);
    let mut server = bench.server();
    let envelope = request(&mut server, "tools/list", json!({}));
    let tools = envelope["result"]["tools"]
        .as_array()
        .expect("a list of tools");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a named tool"))
        .collect();
    assert_eq!(names, NAMES.to_vec(), "the advertised list changed shape");

    // The reverse test the plan asks for. These are the words that would appear if a credential or a
    // trail tool had ever been added here; the argument for leaving them out is that the thing being
    // restricted must not hold the lever.
    for forbidden in [
        "token",
        "permission",
        "grant",
        "revoke",
        "rotate",
        "audit",
        "exclude",
        "config",
        "secret",
        "password",
        "credential",
        "mcp",
    ] {
        for name in &names {
            assert!(
                !name.to_lowercase().contains(forbidden),
                "a tool named {name:?} contains {forbidden:?}"
            );
        }
    }
    // And in the descriptions, which a client may act on: no tool may mention a credential verb even
    // in passing.
    let listing = serde_json::to_string(&tools)
        .expect("serialises")
        .to_lowercase();
    // "credential" is deliberately not in this list: the path descriptions say "one of this
    // credential's workspaces", which is the correct thing to tell a caller and is not a tool that
    // touches credentials.
    for forbidden in ["revoke", "rotate", "mcp token", "audit"] {
        assert!(
            !listing.contains(forbidden),
            "the advertised listing mentions {forbidden:?}"
        );
    }
}

#[test]
fn the_handshake_answers_in_the_shape_a_client_expects() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let answer = request(
        &mut server,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test-harness", "version": "0"}
        }),
    );
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "fam");
    assert_eq!(
        result["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "and it is this build's version, not a string someone typed"
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "tools are the only capability this server has"
    );

    // A notification gets no line at all - not an empty one, not an error one.
    assert_eq!(
        server.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        None
    );
    let ping = request(&mut server, "ping", json!({}));
    assert_eq!(ping["result"]["protocolVersion"], "2025-06-18");

    // A client asking for a version this server does not speak is told which version it is getting.
    // The assertion above cannot tell this apart from the bug, because it asks for the one version
    // that answers: echoing whatever was typed would let both sides believe a protocol nobody
    // implements, and the client would never learn to leave.
    let other = request(
        &mut server,
        "initialize",
        json!({
            "protocolVersion": "1999-01-01",
            "capabilities": {},
            "clientInfo": {"name": "test-harness", "version": "0"}
        }),
    );
    assert_eq!(
        other["result"]["protocolVersion"], PROTOCOL_VERSION,
        "the answer was the client's own string, not the server's"
    );
}

#[test]
fn a_tool_call_without_an_id_is_answered_rather_than_dropped() {
    // A notification asks for nothing and gets nothing - correct for `notifications/initialized`.
    // A `tools/call` with no id is not asking for nothing: it named a tool and passed arguments.
    // Dropping it in silence leaves the client waiting for a reply that will never come.
    let bench = Bench::at(Tier::Edit, &[Place::Inbox]);
    let mut server = bench.server();
    let answer = server
        .handle(
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"note_set","arguments":{}}}"#,
        )
        .expect("answered with a protocol error rather than with nothing");
    let envelope: Value = serde_json::from_str(&answer).expect("one JSON line");
    assert_eq!(envelope["error"]["code"], -32600, "{envelope}");
    assert!(envelope.get("result").is_none(), "{envelope}");
    // Nothing ran, so nothing is claimed: the session's own opening line is there, and no tool op.
    let lines = bench.agent_lines();
    let tools = lines
        .iter()
        .filter(|line| {
            !line["op"]
                .as_str()
                .unwrap_or_default()
                .starts_with("session.")
        })
        .collect::<Vec<_>>();
    assert!(
        tools.is_empty(),
        "an unanswerable call still reached a tool: {tools:?}"
    );
}

#[test]
fn a_query_in_any_language_is_clipped_not_fatal() {
    // The audit line carries a summary of the caller's own text. Clipping it on a *byte* boundary
    // panics inside the code that writes that line, which means the call leaves no trace and the
    // process dies - from a `browse` credential, on the cheapest request there is.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    for length in [199, 200, 201, 500] {
        let query = "字".repeat(length);
        let (answer, _) = call(
            &mut server,
            "fs_search",
            json!({"query": query, "within": spelled(&bench.inbox)}),
        );
        let _ = answer;
    }
    // The line for the longest one exists, is clipped, and does not carry the whole call.
    let lines = bench.agent_lines();
    let args = lines
        .iter()
        .filter(|line| line["op"] == "fs_search")
        .filter_map(|line| line["args"].as_str())
        .next_back()
        .expect("a search line with args");
    // The clip is by characters - which for these queries is far fewer bytes than the ceiling, and
    // that is exactly the point: the byte-counting version panicked trying to land on 200.
    assert!(
        args.chars().count() <= 200,
        "{} characters is over the ceiling",
        args.chars().count()
    );
    assert!(
        args.ends_with('~'),
        "not clipped the way the writer clips: {args}"
    );
}

#[test]
fn a_folder_that_does_not_fit_one_page_still_leads_with_its_folders() {
    // Grouping has to be a property of the ordering, not of the page: group after slicing and the
    // folders that sort past the first window never appear in any page, while some rows repeat
    // across two. This folder is built so that two of its directories sort *after* most files -
    // which is the case the broken shape hides, because the first page looks fine.
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    for name in ["b1.txt", "b2.txt", "b3.txt", "b4.txt"] {
        std::fs::write(bench.inbox.join(name), "x\n").expect("write");
    }
    for name in ["z-first", "z-second"] {
        std::fs::create_dir_all(bench.inbox.join(name)).expect("dir");
    }
    let mut server = bench.server();

    let mut seen: Vec<(String, bool)> = Vec::new();
    let mut total = 0;
    for page in 0..20 {
        let (answer, failure) = call(
            &mut server,
            "fs_list",
            json!({"path": spelled(&bench.inbox), "limit": 3, "offset": page * 3}),
        );
        assert!(!failure, "{answer}");
        total = answer["total"].as_u64().expect("a total") as usize;
        let rows = answer["entries"].as_array().expect("entries");
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let name = row["name"].as_str().expect("a name").to_owned();
            assert!(
                !seen.iter().any(|(seen, _)| *seen == name),
                "{name} came back on two pages"
            );
            seen.push((name, row["directory"].as_bool().unwrap_or(false)));
        }
    }
    assert_eq!(
        seen.len(),
        total,
        "the pages did not cover the folder exactly once: {seen:?}"
    );
    let folders: Vec<&str> = seen
        .iter()
        .filter(|(_, is_dir)| *is_dir)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        folders.len(),
        4,
        "a directory went unreachably off the pages: {seen:?}"
    );
    assert!(
        seen.iter().take(folders.len()).all(|(_, is_dir)| *is_dir),
        "the first page started with a file: {seen:?}"
    );
    assert!(
        seen.iter().skip(folders.len()).all(|(_, is_dir)| !*is_dir),
        "a folder appeared after the files: {seen:?}"
    );
}

#[test]
fn a_method_this_server_does_not_have_is_a_protocol_error_not_a_tool_failure() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    for method in ["prompts/list", "resources/list"] {
        let answer = request(&mut server, method, json!({}));
        assert_eq!(answer["error"]["code"], -32601, "{method}: {answer}");
    }
    let answer = request(
        &mut server,
        "tools/call",
        json!({"name": "fs_drop_table", "arguments": {}}),
    );
    assert_eq!(answer["error"]["code"], -32601, "{answer}");

    // A garbage line is refused with a null id, because no id was ever readable in it.
    let line = server.handle("{not json").expect("answered");
    let envelope: Value = serde_json::from_str(&line).expect("one JSON line");
    assert_eq!(envelope["error"]["code"], -32700);
    assert!(envelope["id"].is_null());
    // And a call whose arguments do not match the schema is a protocol-level invalid params, not a
    // tool that ran and failed.
    let answer = request(&mut server, "tools/call", json!({"arguments": {}}));
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
}

#[test]
fn whoami_answers_for_the_credential_and_claims_no_path() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let (answer, failure) = call(&mut server, "whoami", json!({}));
    assert!(!failure, "{answer}");
    assert_eq!(answer["label"], "tester");
    assert_eq!(answer["tier"], "browse");
    assert_eq!(answer["actor"], "agent:tester");
    let names: Vec<&str> = answer["allows"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|verb| verb.as_str().expect("a name"))
        .collect();
    assert!(names.contains(&"fs_list"));
    assert!(
        !names.contains(&"fs_write"),
        "a browse credential must not be told it can write: {names:?}"
    );
    assert_eq!(names.len(), 6, "browse is six verbs: {names:?}");

    // The line it leaves names no target, because nothing was claimed about a path.
    let line = bench
        .agent_lines()
        .into_iter()
        .find(|record| record["op"] == "whoami")
        .expect("whoami is in the trail");
    assert!(line.get("target").is_none(), "{line}");
    assert_eq!(line["actor"], "agent:tester");
}

#[test]
fn a_listing_reports_what_the_index_that_answered_actually_saw() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let (answer, failure) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
    assert_eq!(answer["stale"], false);
    assert!(answer["generation"].as_u64().expect("a generation") > 0);
    let entries = answer["entries"].as_array().expect("entries");
    let names: Vec<&str> = entries
        .iter()
        .map(|row| row["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        names,
        vec!["empty", "full", "alpha.txt", "beta.md", "payload.bin"],
        "folders first, then by name"
    );
    assert_eq!(entries[0]["directory"], true);
    let alpha = entries
        .iter()
        .find(|row| row["name"] == "alpha.txt")
        .expect("alpha.txt");
    assert_eq!(alpha["size"], 11);
    assert_eq!(alpha["kind"], "document");
    assert_eq!(alpha["reparse"], false);
    assert_eq!(
        alpha["path"],
        path_string_local(&bench.inbox.join("alpha.txt"))
    );

    // Paging: the same folder, two rows at a time, and `total` does not move.
    let (second, _) = call(
        &mut server,
        "fs_list",
        json!({"path": spelled(&bench.inbox), "limit": 2, "offset": 2}),
    );
    assert_eq!(second["total"], 5);
    assert_eq!(second["returned"], 2);
    assert_eq!(second["entries"][0]["name"], "alpha.txt");
}

#[test]
fn a_read_refuses_what_it_would_have_to_lie_about() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();

    let (answer, failure) = call(
        &mut server,
        "fs_read",
        json!({"path": spelled(&bench.inbox.join("beta.md"))}),
    );
    assert!(!failure, "{answer}");
    assert!(answer["text"].as_str().expect("text").starts_with('#'));

    // Binary: refused and named as binary, rather than returned as replacement characters.
    let (answer, failure) = call(
        &mut server,
        "fs_read",
        json!({"path": spelled(&bench.inbox.join("payload.bin"))}),
    );
    assert!(failure, "a binary file must not read as success: {answer}");
    assert_eq!(answer["binary"], true);

    // A directory: the refusal names the tool that can answer.
    let (answer, failure) = call(
        &mut server,
        "fs_read",
        json!({"path": spelled(&bench.inbox)}),
    );
    assert!(failure);
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("fs_list"),
        "{answer}"
    );
}

#[test]
fn a_write_from_a_browse_credential_is_refused_and_recorded_as_refused() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let target = bench.inbox.join("written-by-agent.txt");
    let (answer, failure) = call(
        &mut server,
        "fs_write",
        json!({"path": spelled(&target), "content": "hello"}),
    );
    assert!(failure, "a browse token must not be able to write");
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("browse"),
        "{answer}"
    );
    assert!(!target.exists(), "and nothing was written");

    let line = bench
        .agent_lines()
        .into_iter()
        .find(|record| record["op"] == "fs_write")
        .expect("the attempt is in the trail");
    assert!(
        line["result"]
            .as_str()
            .expect("a result")
            .starts_with("error "),
        "{line}"
    );
}

#[test]
fn a_session_leaves_one_line_per_call_and_the_chain_verifies_afterwards() {
    let bench = Bench::at(Tier::Edit, &[Place::Inbox]);
    let mut server = bench.server();
    let target = bench.inbox.join("notes.txt");
    let (written, failure) = call(
        &mut server,
        "fs_write",
        json!({"path": spelled(&target), "content": "hello 世界\nsecond line\n"}),
    );
    assert!(!failure, "{written}");
    // Bytes, not characters: the content has two three-byte CJK characters, and a trail that counted
    // characters would not match the file on disk.
    assert_eq!(written["written"], 25);
    let (read, _) = call(&mut server, "fs_read", json!({"path": spelled(&target)}));
    assert!(read["text"].as_str().expect("round trips").contains("世界"));
    call(
        &mut server,
        "tags_set",
        json!({"path": spelled(&target), "tags": ["keep"]}),
    );
    call(
        &mut server,
        "note_set",
        json!({"path": spelled(&target), "body": "a private remark"}),
    );
    drop(server);

    let lines = bench.agent_lines();
    let ops: Vec<&str> = lines
        .iter()
        .filter_map(|line| line["op"].as_str())
        // The session's own bracket is not a call; it is the log saying when this one started.
        .filter(|op| !op.starts_with("session."))
        .collect();
    assert_eq!(
        ops,
        vec!["fs_write", "fs_read", "tags_set", "note_set"],
        "one line per call, in the order they were made"
    );

    // The payload never appears in the trail: a write line counts bytes and a note line counts bytes.
    let text = serde_json::to_string(&lines).expect("serialises");
    assert!(!text.contains("世界"), "the trail quoted a file's contents");
    assert!(
        !text.contains("a private remark"),
        "the trail quoted the note"
    );
    assert!(
        text.contains("bytes=25"),
        "the write says what it did: {text}"
    );

    // The whole trail - the agent's session and the store's - still verifies.
    let report = fam_svc_audit::verify(&bench.config.audit_dir).expect("verify runs");
    assert!(
        report.findings.is_empty(),
        "the chain broke: {:?}",
        report.findings
    );
}

#[test]
fn a_credential_that_was_revoked_cannot_open_a_session() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let credential = bench.credential.clone();
    let mut store = fam_svc_auth::TokenStore::open(&bench.config).expect("the store opens");
    store
        .revoke(&bench.token_id, std::time::SystemTime::now())
        .expect("revoked");
    drop(store);
    match Server::authenticate(bench.config.clone(), &credential) {
        Err(fam_svc_mcp::Error::NotAuthorized) => {}
        Err(other) => panic!("a revoked credential failed for the wrong reason: {other}"),
        Ok(_) => panic!("a revoked credential was accepted"),
    }
}

#[test]
fn a_search_says_which_backend_answered_and_whether_it_finished() {
    let bench = Bench::at(Tier::Browse, &[Place::Inbox]);
    let mut server = bench.server();
    let (answer, failure) = call(
        &mut server,
        "fs_search",
        json!({"query": "alpha", "within": spelled(&bench.inbox)}),
    );
    assert!(!failure, "{answer}");
    assert_eq!(
        answer["provider"], "walk",
        "no Everything index covers a temp folder"
    );
    assert_eq!(answer["complete"], true);
    let hits = answer["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 1, "{answer}");
    assert!(hits[0]["path"]
        .as_str()
        .expect("a path")
        .ends_with("alpha.txt"));
    // The listing of the same folder and the search agree the file is there, and both spell it the
    // way the filesystem reported it rather than folded.
    assert!(
        hits[0]["path"]
            .as_str()
            .expect("case kept")
            .contains("inbox"),
        "{}",
        hits[0]["path"]
    );
}

/// Not a gate - a number, for the note that prices this layer. Run it with
/// `cargo test -p fam-svc-mcp --test mcp -- --ignored --nocapture`.
///
/// The two costs worth knowing are the gate by itself (it runs on every path-bearing call, and it
/// does syscalls to resolve links) and a whole request through `handle`. `whoami` is measured
/// separately from `fs_read` because it is the one tool that never reaches the gate - the gap
/// between the two numbers *is* the price of the syscalls, and printing one request as if it
/// contained both would hide it.
#[test]
#[ignore]
fn the_price_of_the_gate_and_a_request() {
    const ROUNDS: u32 = 20_000;
    let bench = Bench::at(Tier::Edit, &[Place::Inbox]);
    let deep = bench.inbox.join("full").join("inner.txt");
    let mut server = bench.server();
    let who_is = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#;
    let read = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"fs_read\",\
         \"arguments\":{{\"path\":\"{}\"}}}}}}",
        spelled(&deep)
    );

    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        // A guard, not a measurement: if the bench stops building the tree this test expects, the
        // numbers below are pricing a path that is not the one the note describes.
        assert!(
            !fam_svc_mcp::TOOLS.is_empty() && deep.starts_with(&bench.inbox),
            "the path stopped being what the bench built"
        );
    }
    let path_check = started.elapsed();

    // Protocol only: a `ping` writes no audit line and runs no tool, so the gap between it and the
    // next number is the tool plus the line, not the JSON.
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        assert!(server.handle(ping).is_some());
    }
    let protocol = started.elapsed();

    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        assert!(server.handle(who_is).is_some(), "whoami stopped answering");
    }
    let ungated = started.elapsed();

    // The same request shape, one tool that does claim a path.
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        let answer = server.handle(&read).expect("fs_read stopped answering");
        assert!(
            !answer.contains("\"isError\":true"),
            "the granted path stopped being readable: {answer}"
        );
    }
    let gated_request = started.elapsed();

    // The gate on its own, with the syscalls it makes: one `symlink_metadata` per component below
    // the granted root, and `canonicalize` only on a component that actually is a link. Built the
    // same way the server builds it, own-file list included, so this number is the one a request
    // pays and not a cheaper approximation of it.
    let gate = fam_svc_mcp::Gate::new(
        &bench.config.search_roots,
        fam_core::Prune::from_config(&bench.config),
        &fam_svc_mcp::Gate::program_files(&bench.config),
    );
    let token = server.token().clone();
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        gate.admit(&token, fam_svc_auth::Verb::FsRead, &deep)
            .expect("the granted path stays granted");
    }
    let gated = started.elapsed();

    // The gate's parts, because "the gate is slow" is not a number anyone can act on.
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        let _ = std::fs::symlink_metadata(&deep);
    }
    let one_stat = started.elapsed();
    let key = fam_core::PathKey::of(&deep);
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        assert!(token.covers(&deep, bench.config.search_roots.as_slice()));
        let _ = &key;
    }
    let covers = started.elapsed();
    let prune = fam_core::Prune::from_config(&bench.config);
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        assert!(!prune.excludes(&deep));
    }
    let excluded = started.elapsed();

    for (what, took) in [
        ("one symlink_metadata          ", one_stat),
        ("covers() by path identity     ", covers),
        ("prune.excludes()              ", excluded),
        ("protocol only (ping)          ", protocol),
        ("gate + syscalls, admit()      ", gated),
        ("whoami through handle, no gate", ungated),
        ("fs_read through handle        ", gated_request),
        ("the loop's own floor          ", path_check),
    ] {
        println!(
            "{what}: {:>7.2} us per round, {} rounds in {:.2?}",
            took.as_nanos() as f64 / f64::from(ROUNDS) / 1000.0,
            ROUNDS,
            took
        );
    }
}
