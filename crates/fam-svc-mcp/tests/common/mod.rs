//! (A shared module compiled once per test binary: no single file uses every helper, so the
//! lints that would fire on unused ones are silenced here rather than by deleting coverage.)
#![allow(dead_code)]

//! A server, a credential and a folder tree, all inside one temporary directory.
//!
//! Shared by the integration tests. Everything a test can touch lives under `tempdir()`: the tree
//! being browsed, the audit directory, the credential file and the tag database. A test that wrote
//! into the real `%APPDATA%\FileAutoManage` would be a test that can forge a person's trail and read
//! their live grants, so the config is built here and nowhere else.
//!
//! Workspaces are granted by [`Place`] rather than by path, for one reason: taking a path out of one
//! bench to grant it to another means the first bench's temporary directory is gone before the
//! second one runs, and the test then fails for a reason that has nothing to do with what it checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fam_core::Config;
use fam_svc_auth::{Tier, TokenStore};
use fam_svc_mcp::Server;
use serde_json::{json, Value};

/// The places a test can point a credential at, inside the tree this module builds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// The folder a well-behaved agent should be able to work in.
    Inbox,
    /// A sibling folder that was never granted.
    Private,
    /// The whole `tree/` folder, which is inside the configured root.
    Root,
    /// The wildcard, which only an admin may hold.
    Wildcard,
}

pub struct Bench {
    pub temp: tempfile::TempDir,
    pub root: PathBuf,
    pub inbox: PathBuf,
    pub private: PathBuf,
    pub config: Config,
    pub credential: String,
    pub token_id: String,
}

/// Forward slashes: a Windows path in a TOML *basic* string is read as escapes. Both spellings work
/// with `toml` and with the Windows APIs, and the same trick is in `apps/fam-cli/tests/cli.rs`.
fn toml(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

impl Bench {
    pub fn at(tier: Tier, places: &[Place]) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        // The configured root is the whole temporary directory, and the program's own state sits
        // under it: `tree/` is the data and `state/` is the audit trail and the credential file.
        // That is the default Windows layout - home is a root, and `%APPDATA%\FileAutoManage` is
        // under it - and it is the only arrangement in which the exclusion check is the thing
        // standing between an agent and the credential file. Put the state outside every root and
        // the scope check answers first, the test passes, and it proves nothing.
        let root = temp.path().to_path_buf();
        let tree = root.join("tree");
        let inbox = tree.join("inbox");
        let private = tree.join("private");
        write(&inbox.join("alpha.txt"), "first file\n");
        write(&inbox.join("beta.md"), "# heading\n\nbody text\n");
        write(&inbox.join("payload.bin"), [0u8, 159, 146, 159, 255]);
        fs::create_dir_all(inbox.join("empty")).expect("empty dir");
        write(&inbox.join("full").join("inner.txt"), "nested\n");
        write(&private.join("secret.txt"), "not for the agent\n");

        let state = temp.path().join("state");
        let config = Config {
            database_path: state.join("tags.db"),
            search_roots: vec![root.clone()],
            audit_dir: state.join("audit"),
            mcp_token_file: state.join("auth/mcp-tokens.toml"),
            ..Config::default()
        };

        fs::create_dir_all(state.join("auth")).expect("auth dir");
        let granted: Vec<String> = places
            .iter()
            .map(|place| match place {
                Place::Inbox => toml(&inbox),
                Place::Private => toml(&private),
                Place::Root => toml(&tree),
                Place::Wildcard => "*".to_owned(),
            })
            .collect();
        let mut store = TokenStore::open(&config).expect("the store opens");
        let issued = store
            .add("tester", tier, &granted, SystemTime::now())
            .expect("the grant is made");
        Self {
            temp,
            root: tree,
            inbox,
            private,
            credential: issued.credential(),
            token_id: issued.token.id.clone(),
            config,
        }
    }

    pub fn server(&self) -> Server {
        Server::authenticate(self.config.clone(), &self.credential)
            .expect("the credential this bench just issued is accepted")
    }

    /// Every audit line written so far, from every session in this bench's trail.
    pub fn trail(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for path in fam_svc_audit::files(&self.config.audit_dir).unwrap_or_default() {
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            for line in bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                out.push(serde_json::from_slice(line).expect("a line the writer produced"));
            }
        }
        out
    }

    /// Only the lines this bench's agent produced.
    pub fn agent_lines(&self) -> Vec<Value> {
        self.trail()
            .into_iter()
            .filter(|line| line["actor"] == "agent:tester")
            .collect()
    }

    pub fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }
}

fn write(path: &Path, body: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, body).expect("write");
}

/// Drive one request through `handle` and return the envelope.
pub fn request(server: &mut Server, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let answer = server
        .handle(&line.to_string())
        .unwrap_or_else(|| panic!("{method} was answered with nothing"));
    let envelope: Value = serde_json::from_str(&answer).expect("one JSON line");
    assert_eq!(answer.lines().count(), 1, "one line per message");
    envelope
}

/// A tool call's payload, with `isError` as the second half of the tuple.
pub fn call(server: &mut Server, name: &str, arguments: Value) -> (Value, bool) {
    let envelope = request(
        server,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    );
    assert!(
        envelope.get("error").is_none_or(Value::is_null),
        "a tool call should answer with a result, not a protocol error: {envelope}"
    );
    let failure = envelope["result"]["isError"].as_bool().unwrap_or(false);
    let text = envelope["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("the content block is not text: {envelope}"));
    let payload = match serde_json::from_str(text) {
        Ok(payload) => payload,
        // A tool that could not answer writes a sentence, so not everything is JSON.
        Err(_) => json!({"raw": text}),
    };
    (payload, failure)
}

/// The path as the tools spell it: forward slashes, which is what the credential was granted with.
pub fn spelled(path: &Path) -> String {
    toml(path)
}
