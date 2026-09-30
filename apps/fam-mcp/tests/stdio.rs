//! The transport, end to end: a real `fam-mcp` process, a real pipe, a real trail on disk.
//!
//! The unit tests in `crates/fam-svc-mcp` call `Server::handle` directly, which cannot get the
//! transport wrong. This file starts the built binary and talks to it the way a client does, so the
//! things that only break here - a log line landing on stdout, a flush that never happens, an exit
//! that hangs instead of ending - have a test.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::SystemTime;

use serde_json::{json, Value};

struct Client {
    child: Child,
    /// `None` once the session is over: closing the write end is what tells the server its client
    /// has gone, and it has to happen before waiting for the child or the two would deadlock.
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    stderr: Vec<String>,
}

/// One temporary directory: the tree, the state (audit + credentials + database), and the config
/// that points at them. The state lives *inside* the configured root, because that is the default
/// layout and the only one where the exclusion rules are the thing standing between an agent and the
/// credential file.
struct Stage {
    temp: tempfile::TempDir,
    config: PathBuf,
    inbox: PathBuf,
    credential: String,
}

/// What the credential is granted, spelled after the stage has built the tree.
#[derive(Clone, Copy)]
enum Grant {
    Inbox,
    EveryRoot,
    NothingYet,
}

impl Stage {
    fn new(tier: &str, grant: Grant) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let inbox = root.join("tree/inbox");
        let state = root.join("state");
        fs::create_dir_all(&inbox).expect("inbox");
        fs::write(inbox.join("alpha.txt"), "first file\n").expect("write");
        fs::create_dir_all(state.join("auth")).expect("auth");
        let spelled = |path: &Path| path.display().to_string().replace('\\', "/");
        let config = state.join("config.toml");
        fs::write(
            &config,
            format!(
                "database_path = \"{}\"\nsearch_roots = [\"{}\"]\naudit_dir = \"{}\"\nmcp_token_file = \"{}\"\n",
                spelled(&state.join("tags.db")),
                spelled(&root),
                spelled(&state.join("audit")),
                spelled(&state.join("auth/mcp-tokens.toml")),
            ),
        )
        .expect("config");

        let workspaces: Vec<String> = match grant {
            Grant::Inbox => vec![spelled(&inbox)],
            Grant::EveryRoot => vec!["*".to_owned()],
            Grant::NothingYet => vec!["D:/nowhere".to_owned()],
        };
        let mut store = fam_svc_auth::TokenStore::open(&load(&config)).expect("the store opens");
        let issued = store
            .add(
                "e2e",
                fam_svc_auth::Tier::parse(tier).expect("a tier"),
                &workspaces,
                SystemTime::now(),
            )
            .expect("granted");
        Self {
            temp,
            config,
            inbox,
            credential: issued.credential(),
        }
    }

    fn root(&self) -> PathBuf {
        self.temp.path().to_path_buf()
    }
}

fn load(config: &Path) -> fam_core::Config {
    fam_core::Config::load_from(config).expect("the stage's config reads")
}

fn spelled(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

impl Client {
    fn spawn(stage: &Stage) -> Self {
        Self::spawn_with(stage, Some(&stage.credential))
    }

    fn spawn_with(stage: &Stage, credential: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fam-mcp"));
        command
            .arg("--config")
            .arg(&stage.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match credential {
            Some(token) => {
                command.env("FAM_MCP_TOKEN", token);
            }
            None => {
                command.env_remove("FAM_MCP_TOKEN");
            }
        }
        let mut child = command.spawn().expect("the server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr: Vec::new(),
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("the pipe is open");
        writeln!(stdin, "{message}").expect("writable");
        stdin.flush().expect("flushed");
    }

    /// One response line. A hang here is the test failing on time rather than forever, because the
    /// server is asked to answer and then the pipe is closed if it does not.
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line);
        assert!(
            read.expect("the pipe is readable") > 0,
            "the server closed stdout"
        );
        let value: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|source| panic!("stdout is not JSON: {line:?} ({source})"));
        value
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.receive()
    }

    fn call(&mut self, id: i64, tool: &str, arguments: Value) -> (Value, bool) {
        let envelope = self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        );
        let failure = envelope["result"]["isError"].as_bool().unwrap_or(false);
        let text = envelope["result"]["content"][0]["text"]
            .as_str()
            .expect("a text block");
        (
            serde_json::from_str(text).unwrap_or(json!({"raw": text})),
            failure,
        )
    }

    fn finish(mut self) -> Vec<String> {
        drop(self.stdin.take());
        let mut notes = Vec::new();
        if let Some(stderr) = self.child.stderr.take() {
            for line in BufReader::new(stderr).lines() {
                notes.push(line.expect("stderr is readable"));
            }
        }
        let _ = self.child.wait();
        self.stderr = notes;
        self.stderr
    }
}

#[test]
fn a_full_session_over_a_real_pipe_leaves_one_line_per_call_and_a_chain_that_verifies() {
    // Admin with `*`, so the credential file is inside the token's scope and the only thing standing
    // between this call and the file is the exclusion rule - which is the claim being tested. A
    // narrower grant would be refused by the scope check first, and the test would pass for the
    // wrong reason.
    let stage = Stage::new("admin", Grant::EveryRoot);
    let mut client = Client::spawn(&stage);

    let hello = client.request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "e2e", "version": "0"}}),
    );
    assert_eq!(hello["result"]["serverInfo"]["name"], "fam");
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = client.request(2, "tools/list", json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().expect("tools").len(),
        15,
        "the pipe advertises the same fifteen the library does"
    );

    let (who, failure) = client.call(3, "whoami", json!({}));
    assert!(!failure, "{who}");
    assert_eq!(who["label"], "e2e");

    let (listed, failure) = client.call(4, "fs_list", json!({"path": spelled(&stage.inbox)}));
    assert!(!failure, "{listed}");
    assert_eq!(listed["entries"][0]["name"], "alpha.txt");

    let written = stage.inbox.join("by-agent.txt");
    let (answer, failure) = client.call(
        5,
        "fs_write",
        json!({"path": spelled(&written), "content": "written over a pipe\n"}),
    );
    assert!(!failure, "{answer}");
    assert!(written.exists(), "the file the agent wrote is on disk");

    let (answer, failure) = client.call(
        6,
        "fs_read",
        json!({"path": spelled(&stage.root().join("state/auth/mcp-tokens.toml"))}),
    );
    assert!(
        failure,
        "the credential file was readable over the pipe: {answer}"
    );
    assert!(
        answer["error"]
            .as_str()
            .expect("a sentence")
            .contains("own state"),
        "{answer}"
    );

    let notes = client.finish();
    // Nothing but JSON ever reached stdout - `receive` would have panicked otherwise - and the
    // startup note went to stderr where a launcher can see it.
    assert!(
        notes
            .iter()
            .any(|line| line.contains("serving as agent:e2e")),
        "stderr said nothing: {notes:?}"
    );

    let config = load(&stage.config);
    let report = fam_svc_audit::verify(&config.audit_dir).expect("verify runs");
    assert!(
        report.findings.is_empty(),
        "the chain broke: {:?}",
        report.findings
    );
    let mut calls = Vec::new();
    for path in fam_svc_audit::files(&config.audit_dir).expect("files") {
        for line in fs::read(&path)
            .expect("readable")
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let record: Value = serde_json::from_slice(line).expect("a record");
            if record["actor"] == "agent:e2e" {
                let op = record["op"].as_str().expect("an op").to_owned();
                if !op.starts_with("session.") {
                    calls.push((op, record["result"].as_str().expect("a result").to_owned()));
                }
            }
        }
    }
    assert_eq!(
        calls.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(),
        vec!["whoami", "fs_list", "fs_write", "fs_read"],
        "one line per call, in order: {calls:?}"
    );
    assert!(
        calls[3].1.starts_with("error ") && calls[3].1.contains("own state"),
        "the refusal is in the trail as a refusal: {:?}",
        calls[3]
    );
    assert!(
        !calls
            .iter()
            .any(|(_, result)| result.contains("written over a pipe")),
        "a trail line quoted a file's contents: {calls:?}"
    );
}

#[test]
fn without_a_credential_the_process_refuses_to_read_stdin() {
    let stage = Stage::new("browse", Grant::NothingYet);
    let client = Client::spawn_with(&stage, None);
    let notes = client.finish();
    assert!(
        notes
            .iter()
            .any(|line| line.contains("FAM_MCP_TOKEN is not set")),
        "{notes:?}"
    );
}

#[test]
fn a_credential_that_does_not_verify_is_refused_at_startup() {
    let stage = Stage::new("browse", Grant::NothingYet);
    let client = Client::spawn_with(
        &stage,
        Some("fam_deadbeef_00000000000000000000000000000000"),
    );
    let notes = client.finish();
    assert!(
        notes.iter().any(|line| line.contains("was not accepted")),
        "{notes:?}"
    );
}

#[test]
fn a_browse_credential_over_the_pipe_cannot_write() {
    let stage = Stage::new("browse", Grant::Inbox);
    let mut client = Client::spawn(&stage);
    client.request(1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    let target = stage.inbox.join("nope.txt");
    let (answer, failure) = client.call(
        2,
        "fs_write",
        json!({"path": spelled(&target), "content": "x"}),
    );
    assert!(failure, "a browse token wrote a file: {answer}");
    assert!(!target.exists());
    let _ = client.finish();
}
