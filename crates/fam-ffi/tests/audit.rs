//! The audit trail as an ABI caller experiences it.
//!
//! These tests go through the C entry points rather than through `FamCore` because the claim being
//! checked is about the boundary: one request, one line - including the requests that were refused,
//! including the ones that only read a window of rows somebody already asked for.

use std::ffi::{c_char, c_int, CString};
use std::fs;
use std::path::{Path, PathBuf};

use fam_svc_audit::{verify, Record, FILE_PREFIX, FILE_SUFFIX};
use famcore::{
    fam_abi_version, fam_core_free, fam_core_new, fam_dir_close, fam_dir_fetch, fam_dir_set_sort,
    fam_dir_state, fam_open_dir, fam_search_begin, fam_stats, FamCore,
};

/// Forward slashes keep TOML basic strings free of escape headaches, and Windows accepts them.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn text(pointer: *mut c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let owned = unsafe { CString::from_raw(pointer) };
    owned.to_string_lossy().into_owned()
}

struct Str(CString);

impl Str {
    fn of(value: &str) -> Str {
        Str(CString::new(value).expect("no interior NUL"))
    }
    fn ptr(&self) -> *const c_char {
        self.0.as_ptr()
    }
}

/// A core started against a temporary tree, with its audit directory next to that tree.
struct Session {
    _dir: tempfile::TempDir,
    root: PathBuf,
    audit: PathBuf,
    handle: *mut FamCore,
}

impl Session {
    /// `record_reads` is the only knob these tests turn on the way in; everything else is the
    /// default configuration for a two-file tree.
    fn start(record_reads: bool) -> Session {
        let staged = tempfile::tempdir().expect("create tree");
        let root = staged.path().join("tree");
        fs::create_dir_all(&root).expect("create tree");
        fs::write(root.join("a.txt"), "one").expect("write file");
        fs::write(root.join("b.txt"), "two").expect("write file");
        let config = staged.path().join("config.toml");
        let audit = staged.path().join("audit");
        fs::write(
            &config,
            format!(
                "database_path = \"{}\"\nsearch_roots = [\"{}\"]\naudit_dir = \"{}\"\naudit_record_reads = {}\n",
                toml_path(&staged.path().join("tags.sqlite")),
                toml_path(&root),
                toml_path(&audit),
                record_reads
            ),
        )
        .expect("write config");

        let path = Str::of(&toml_path(&config));
        let mut error: *mut c_char = std::ptr::null_mut();
        let handle = unsafe { fam_core_new(path.ptr(), &mut error) };
        assert!(!handle.is_null(), "core failed to start: {}", text(error));
        Session {
            root,
            audit,
            handle,
            _dir: staged,
        }
    }

    /// What a failure to start says, without a handle ever existing.
    fn error(config: &str) -> String {
        let staged = tempfile::tempdir().expect("create tree");
        let path = staged.path().join("config.toml");
        fs::write(&path, config).expect("write config");
        let path = Str::of(&toml_path(&path));
        let mut error: *mut c_char = std::ptr::null_mut();
        let handle = unsafe { fam_core_new(path.ptr(), &mut error) };
        assert!(handle.is_null(), "the core started when it should not have");
        text(error)
    }

    fn open(&self, dir: &Path) -> u64 {
        let path = Str::of(&toml_path(dir));
        let mut error: *mut c_char = std::ptr::null_mut();
        let token = unsafe { fam_open_dir(self.handle, path.ptr(), &mut error) };
        assert_ne!(token, 0, "open failed: {}", text(error));
        token
    }

    /// A header-sized read of the state, enough to have asked.
    fn state(&self, token: u64) -> c_int {
        let mut buffer = vec![0u8; 128];
        unsafe {
            fam_dir_state(
                self.handle,
                token,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null_mut(),
            )
        }
    }

    fn fetch(&self, token: u64, first: u32, count: u32) -> c_int {
        let mut buffer = vec![0u8; 64 * 1024];
        unsafe {
            fam_dir_fetch(
                self.handle,
                token,
                first,
                count,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null_mut(),
            )
        }
    }

    fn set_sort(&self, token: u64, sort: &str, order: &str) -> (c_int, String) {
        let sort = Str::of(sort);
        let order = Str::of(order);
        let mut error: *mut c_char = std::ptr::null_mut();
        let code =
            unsafe { fam_dir_set_sort(self.handle, token, sort.ptr(), order.ptr(), &mut error) };
        (code, text(error))
    }

    /// The void entry point: no `err` slot, so the audit line is the only thing it says.
    fn close(&self, token: u64) {
        unsafe { fam_dir_close(self.handle, token) };
    }

    fn search(&self, query: &str) -> u64 {
        let query = Str::of(query);
        let mut error: *mut c_char = std::ptr::null_mut();
        let token = unsafe {
            fam_search_begin(
                self.handle,
                query.ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut error,
            )
        };
        assert_ne!(token, 0, "search failed: {}", text(error));
        token
    }

    fn stats(&self) -> serde_json::Value {
        let mut out: *mut c_char = std::ptr::null_mut();
        let mut error: *mut c_char = std::ptr::null_mut();
        let code = unsafe { fam_stats(self.handle, &mut out, &mut error) };
        assert_eq!(code, 0, "stats failed: {}", text(error));
        let json = text(out);
        serde_json::from_str(&json).expect("stats is JSON")
    }

    /// Every line the session wrote, oldest first, once the log has been closed and sealed.
    fn sealed(mut self) -> Vec<Record> {
        let audit = self.audit.clone();
        self.free();
        let report = verify(&audit).expect("the log reads");
        assert!(report.intact(), "findings: {:#?}", report.findings);
        let mut out = Vec::new();
        for path in fam_svc_audit::files(&audit).expect("files") {
            for line in fs::read_to_string(path).expect("readable").lines() {
                out.push(serde_json::from_str(line).expect("a record"));
            }
        }
        out
    }

    fn free(&mut self) {
        if !self.handle.is_null() {
            unsafe { fam_core_free(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.free();
    }
}

#[test]
fn a_session_brackets_what_it_did_and_verifies_afterwards() {
    let session = Session::start(true);
    let token = session.open(&session.root);
    session.state(token);
    session.fetch(token, 0, 10);
    session.search("a");
    let records = session.sealed();

    let ops: Vec<&str> = records.iter().map(|record| record.op.as_str()).collect();
    assert_eq!(ops[0], "session.open");
    assert_eq!(*ops.last().expect("a last op"), "session.close");
    assert_eq!(
        ops,
        vec![
            "session.open",
            "dir.open",
            "dir.fetch",
            "search.begin",
            "session.close"
        ],
        "`dir.state` is the view asking whether anything changed yet, which is a beat and not a read"
    );
    // The seal is the last line and counts the session, so a removed tail becomes a finding.
    let seal = records.last().expect("a seal");
    assert_eq!(
        seal.args.as_deref(),
        Some(format!("records={} dropped=0", records.len()).as_str())
    );
    assert_eq!(seal.actor.to_string(), "system");
}

#[test]
fn one_request_is_one_line_even_when_it_is_refused() {
    let session = Session::start(true);
    let token = session.open(&session.root);
    let (code, message) = session.set_sort(token, "sideways", "ascending");
    assert_eq!(code, -1);
    assert!(!message.is_empty());
    session.set_sort(token, "name", "sideways");
    let records = session.sealed();

    let refusals: Vec<&Record> = records
        .iter()
        .filter(|record| record.result.starts_with("error"))
        .collect();
    assert_eq!(refusals.len(), 2, "both refusals are lines");
    assert_eq!(refusals[0].op, "dir.sort");
    assert_eq!(
        refusals[0].args.as_deref(),
        Some("token=1 sort=sideways order=ascending")
    );
    assert!(
        refusals[0].result.contains("sideways"),
        "the line says what was refused: {}",
        refusals[0].result
    );
    assert_eq!(refusals[1].result, "error unknown order \"sideways\"");
}

/// The path is recorded as the identity the rest of the program uses, so an audit line joins to a
/// listing without anyone having to normalize two spellings first.
#[test]
fn the_target_of_a_line_is_the_same_key_the_index_uses() {
    let session = Session::start(true);
    session.open(&session.root);
    let key = fam_core::path_key(&session.root);
    let records = session.sealed();
    let opened = records
        .iter()
        .find(|record| record.op == "dir.open")
        .expect("an open");
    assert_eq!(opened.target.as_deref(), Some(key.as_str()));
    assert_eq!(opened.result, "ok token=1");
    assert!(opened.args.is_none(), "the token is in the result");
}

#[test]
fn turning_record_reads_off_keeps_every_question_and_drops_only_the_answers() {
    let session = Session::start(false);
    let token = session.open(&session.root);
    session.state(token);
    session.fetch(token, 0, 10);
    session.fetch(token, 10, 10);
    let records = session.sealed();
    let ops: Vec<&str> = records.iter().map(|record| record.op.as_str()).collect();
    assert!(ops.contains(&"dir.open"), "{ops:?}");
    assert!(
        !ops.contains(&"dir.fetch"),
        "the answer reads are the ones switched off: {ops:?}"
    );
    assert!(!ops.contains(&"dir.state"), "so is the poll: {ops:?}");
    // And the session still seals itself, so the log is not silently shorter than it claims.
    assert_eq!(*ops.last().expect("a last op"), "session.close");
    let seal = records.last().expect("a seal");
    assert_eq!(seal.args.as_deref(), Some("records=3 dropped=0"));
}

#[test]
fn stats_says_whether_the_log_is_actually_being_written() {
    let session = Session::start(true);
    session.open(&session.root);
    let stats = session.stats();
    let audit = &stats["audit"];
    assert_eq!(audit["writing"], true);
    assert_eq!(audit["record_reads"], true);
    assert_eq!(audit["records"].as_u64().expect("a count"), 2);
    assert_eq!(audit["dropped"].as_u64().expect("a count"), 0);
    assert!(audit["last_error"].is_null(), "got {}", audit["last_error"]);
    assert!(audit["directory"]
        .as_str()
        .expect("a path")
        .ends_with("audit"));
    // The id `stats` reports is the one on every line, which is what lets a reader match a session
    // in the log to the process that was running.
    let session_id = audit["session"].as_u64().expect("an id");
    assert_ne!(session_id, 0);
    let records = session.sealed();
    assert!(records.iter().all(|record| record.session == session_id));
}

/// The hard constraint: a search root inside the audit directory would mean writing the log into
/// the tree the tool is told to index, so the core refuses to exist.
#[test]
fn a_search_root_inside_the_audit_directory_refuses_to_start() {
    let staged = tempfile::tempdir().expect("create tree");
    let audit = staged.path().join("audit");
    fs::create_dir_all(audit.join("nested")).expect("create");
    let message = Session::error(&format!(
        "search_roots = [\"{}\"]\naudit_dir = \"{}\"\n",
        toml_path(&audit.join("nested")),
        toml_path(&audit)
    ));
    assert!(
        message.contains("would contain the search root"),
        "message was {message}"
    );
    assert!(message.contains("nested"), "message names it: {message}");
}

/// A directory that cannot hold the log stops the program rather than running it unlogged.
#[test]
fn an_audit_directory_that_cannot_be_written_refuses_to_start() {
    let staged = tempfile::tempdir().expect("create tree");
    let blocker = staged.path().join("audit");
    fs::write(&blocker, b"not a directory").expect("a file in the way");
    let message = Session::error(&format!(
        "search_roots = [\"{}\"]\naudit_dir = \"{}\"\n",
        toml_path(staged.path()),
        toml_path(&blocker)
    ));
    assert!(
        message.starts_with("cannot open the audit log"),
        "message was {message}"
    );
}

/// A configuration with no audit directory is not a configuration: there is no "log nothing"
/// setting for a program that is about to be driven by an agent.
#[test]
fn an_empty_audit_directory_is_rejected_before_anything_opens() {
    let message = Session::error("audit_dir = \"\"\n");
    assert!(
        message.contains("audit_dir must not be empty"),
        "message was {message}"
    );
}

/// A session that asked for nothing still leaves two lines - the opening and the seal - which is
/// what makes "this process ran and touched no files" a statement the log can actually support.
/// `fam_abi_version` and `fam_free_string` are not requests about files and are not recorded: the
/// first has no handle to record into, and the second would double the line count of every answer
/// to say something the answer's own line already says.
#[test]
fn a_session_that_asked_for_nothing_is_still_an_accountable_session() {
    assert_eq!(fam_abi_version(), 2);
    let session = Session::start(true);
    let records = session.sealed();
    let ops: Vec<&str> = records.iter().map(|record| record.op.as_str()).collect();
    assert_eq!(ops, vec!["session.open", "session.close"]);
}

/// A close that released nothing must not read like one that did. These two entry points have no
/// `err` slot, so the audit line is the only difference a reader ever sees between them.
#[test]
fn a_close_that_released_nothing_says_so() {
    let session = Session::start(true);
    let token = session.open(&session.root);
    session.close(token + 1000);
    session.close(token);
    let records = session.sealed();
    let closes: Vec<&Record> = records
        .iter()
        .filter(|record| record.op == "dir.close")
        .collect();
    assert_eq!(closes.len(), 2, "both calls are lines: {records:?}");
    assert_eq!(closes[0].result, "error nothing was held by that token");
    assert_eq!(closes[1].result, "ok");
}

/// The file-name constants are the contract between the writer and a reader that is a different
/// program, so they are asserted here where both are in scope.
#[test]
fn the_log_files_are_named_by_their_utc_day() {
    assert_eq!(FILE_PREFIX, "audit-");
    assert_eq!(FILE_SUFFIX, ".jsonl");
    let session = Session::start(true);
    let files = fam_svc_audit::files(&session.audit).expect("files");
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().expect("a file name");
    let name = name.to_string_lossy().into_owned();
    assert!(
        name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX),
        "{name}"
    );
    assert_eq!(name.len(), "audit-YYYY-MM-DD.jsonl".len());
}
