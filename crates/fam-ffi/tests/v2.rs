//! The v2 ABI, end to end: open without blocking, bounded fetches, change events that come back as
//! positions to cut, and searches a running result set can be paged through.
//!
//! Everything here goes through the C surface rather than the Rust internals on purpose - the
//! contract being tested is what a shell can actually see, including the parts that only the
//! pipeline can make happen.

use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::os::raw::c_uchar;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use famcore::{
    fam_abi_version, fam_backends_json, fam_core_free, fam_core_new, fam_dir_close, fam_dir_fetch,
    fam_dir_poll, fam_dir_set_filter, fam_dir_set_sort, fam_dir_state, fam_free_string,
    fam_open_dir, fam_search_begin, fam_search_cancel, fam_search_close, fam_search_page,
    fam_stats, FamCore,
};

use famcore::wire::{
    blob_offset, FLAG_INCOMPLETE, FLAG_RESYNC, FLAG_TRUNCATED, HEADER_BYTES, RECORD_BYTES,
};

const ABI: u32 = 2;
const READY: u8 = famcore::wire::state::READY;
const STALE: u8 = famcore::wire::state::STALE;
const LOADING: u8 = famcore::wire::state::LOADING;

fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Core {
    handle: *mut FamCore,
}

impl Core {
    fn with(tree: &Tree) -> Self {
        let config = tree.config.clone();
        let db = tree.db.clone();
        fs::write(
            &config,
            format!(
                "database_path = \"{}\"\nsearch_roots = [\"{}\"]\nui_batch_ms = 20\n",
                toml_path(&db),
                toml_path(&tree.root)
            ),
        )
        .expect("write config");
        let path = CString::new(toml_path(&config)).unwrap();
        let mut error = Error::new();
        let handle = unsafe { fam_core_new(path.as_ptr(), &mut error.value) };
        assert!(!handle.is_null(), "core failed to start: {}", error.text());
        Self { handle }
    }

    fn open(&self, dir: &Path) -> u64 {
        let path = CString::new(toml_path(dir)).unwrap();
        let mut error = Error::new();
        let token = unsafe { fam_open_dir(self.handle, path.as_ptr(), &mut error.value) };
        assert_ne!(token, 0, "open failed: {}", error.text());
        token
    }

    fn state(&self, token: u64) -> Header {
        let mut buffer = vec![0u8; HEADER_BYTES];
        let written = self.call(token, &mut buffer, |core, token, buffer, len, err| unsafe {
            fam_dir_state(core, token, buffer, len, err)
        });
        Header::decode(&buffer[..written])
    }

    /// Waits for a directory's rows to arrive, and fails loudly if they never do.
    fn wait_ready(&self, token: u64) -> Header {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let header = self.state(token);
            if header.state == READY || Instant::now() > deadline {
                return header;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn fetch(&self, token: u64, first: u32, count: u32, room: usize) -> (Header, Vec<Row>) {
        let mut buffer = vec![0u8; room];
        let written = self.call(token, &mut buffer, |core, token, buffer, len, err| unsafe {
            fam_dir_fetch(core, token, first, count, buffer, len, err)
        });
        let header = Header::decode(&buffer[..written]);
        let rows = decode_rows(&buffer, &header);
        (header, rows)
    }

    fn poll(&self, token: u64, room: usize) -> (Header, Vec<u32>) {
        let mut buffer = vec![0u8; room];
        let written = self.call(token, &mut buffer, |core, token, buffer, len, err| unsafe {
            fam_dir_poll(core, token, buffer, len, err)
        });
        let header = Header::decode(&buffer[..written]);
        let cuts = buffer[HEADER_BYTES..written]
            .chunks_exact(8)
            .map(|record| u32::from_le_bytes(record[..4].try_into().unwrap()))
            .collect();
        (header, cuts)
    }

    /// One of the buffer-taking calls, with the error channel checked.
    fn call<F>(&self, token: u64, buffer: &mut [u8], run: F) -> usize
    where
        F: FnOnce(*mut FamCore, u64, *mut c_uchar, u32, *mut *mut c_char) -> i32,
    {
        let mut error = Error::new();
        let written = run(
            self.handle,
            token,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut error.value,
        );
        assert!(written >= 0, "call failed: {}", error.text());
        written as usize
    }

    fn set_sort(&self, token: u64, sort: &str, order: &str) {
        let sort = CString::new(sort).unwrap();
        let order = CString::new(order).unwrap();
        let mut error = Error::new();
        let code = unsafe {
            fam_dir_set_sort(
                self.handle,
                token,
                sort.as_ptr(),
                order.as_ptr(),
                &mut error.value,
            )
        };
        assert_eq!(code, 0, "set_sort failed: {}", error.text());
    }

    fn set_filter(&self, token: u64, terms: &str, kind: Option<&str>) -> i32 {
        let terms = CString::new(terms).unwrap();
        let kind = kind.map(CString::new).transpose().unwrap();
        let mut error = Error::new();
        unsafe {
            fam_dir_set_filter(
                self.handle,
                token,
                terms.as_ptr(),
                kind.map_or(ptr::null_mut(), |name| name.into_raw()),
                &mut error.value,
            )
        }
    }

    fn close(&self, token: u64) {
        unsafe { fam_dir_close(self.handle, token) };
    }

    /// Starts a search and reports what the error channel said with it, so a refusal can be asserted
    /// on rather than swallowed by an `assert_ne!`.
    fn try_search(
        &self,
        query: &str,
        within: Option<&Path>,
        kind: Option<&str>,
        limit: u32,
    ) -> (u64, String) {
        let query = CString::new(query).unwrap();
        let within = within.map(|path| CString::new(toml_path(path)).unwrap());
        let kind = kind.map(CString::new).transpose().unwrap();
        let mut error = Error::new();
        let token = unsafe {
            fam_search_begin(
                self.handle,
                query.as_ptr(),
                within.as_ref().map_or(ptr::null(), |path| path.as_ptr()),
                kind.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                limit,
                &mut error.value,
            )
        };
        (token, error.text())
    }

    fn search(&self, query: &str, within: Option<&Path>, kind: Option<&str>, limit: u32) -> u64 {
        let (token, message) = self.try_search(query, within, kind, limit);
        assert_ne!(token, 0, "search failed: {message}");
        token
    }

    fn search_page(&self, token: u64, first: u32, count: u32, room: usize) -> (Header, Vec<Row>) {
        let mut buffer = vec![0u8; room];
        let written = self.call(token, &mut buffer, |core, token, buffer, len, err| unsafe {
            fam_search_page(core, token, first, count, buffer, len, err)
        });
        let header = Header::decode(&buffer[..written]);
        let rows = decode_rows(&buffer, &header);
        (header, rows)
    }

    /// An empty page is the cheap way to ask where a search stands.
    fn search_state(&self, token: u64) -> Header {
        self.search_page(token, 0, 0, HEADER_BYTES).0
    }

    /// Waits for a search to stop running, and fails if it never does.
    fn wait_settled(&self, token: u64) -> Header {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let header = self.search_state(token);
            if header.state != LOADING || Instant::now() > deadline {
                return header;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn cancel_search(&self, token: u64) -> i32 {
        let mut error = Error::new();
        let code = unsafe { fam_search_cancel(self.handle, token, &mut error.value) };
        assert_eq!(code, 0, "cancel failed: {}", error.text());
        code
    }

    fn close_search(&self, token: u64) {
        unsafe { fam_search_close(self.handle, token) };
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe { fam_core_free(self.handle) };
    }
}

struct Error {
    value: *mut c_char,
}

impl Error {
    fn new() -> Self {
        Self {
            value: ptr::null_mut(),
        }
    }

    fn text(&mut self) -> String {
        if self.value.is_null() {
            return String::from("(no message)");
        }
        let text = unsafe { CStr::from_ptr(self.value) }
            .to_string_lossy()
            .into_owned();
        unsafe { fam_free_string(self.value) };
        self.value = ptr::null_mut();
        text
    }
}

#[derive(Debug)]
struct Header {
    abi: u16,
    record_size: u16,
    row_count: u32,
    state: u8,
    flags: u8,
    total_rows: u32,
    generation: u64,
    seq: u64,
}

impl Header {
    fn decode(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() >= HEADER_BYTES,
            "a v2 buffer always carries its header, got {} bytes",
            bytes.len()
        );
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let half = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let long = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        assert_eq!(word(0), 0x4D41_5246, "magic: not a buffer this core wrote");
        Self {
            abi: half(4),
            record_size: half(6),
            row_count: word(8),
            state: bytes[12],
            flags: bytes[13],
            total_rows: word(16),
            generation: long(24),
            seq: long(32),
        }
    }
}

/// One row as the wire carries it: the record, plus the name it points into the blob for.
struct Row {
    name: String,
    size: u64,
    directory: bool,
}

fn decode_rows(bytes: &[u8], header: &Header) -> Vec<Row> {
    assert_eq!(
        header.record_size as usize, RECORD_BYTES,
        "a row buffer's records are `Item`"
    );
    // Through the core's own helper, so a test that decoded a shifted blob would notice the
    // formula and the writer disagreeing.
    let blob = blob_offset(header.row_count as usize, header.record_size);
    (0..header.row_count as usize)
        .map(|index| {
            let at = HEADER_BYTES + index * RECORD_BYTES;
            let record = &bytes[at..at + RECORD_BYTES];
            let name_off = u32::from_le_bytes(record[0..4].try_into().unwrap()) as usize;
            let name_units = u16::from_le_bytes(record[4..6].try_into().unwrap()) as usize;
            let size = u64::from_le_bytes(record[8..16].try_into().unwrap());
            let flags = record[7];
            let units: Vec<u16> = bytes[blob + name_off * 2..blob + name_off * 2 + name_units * 2]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes(pair.try_into().unwrap()))
                .collect();
            Row {
                name: String::from_utf16_lossy(&units),
                size,
                directory: flags & 0b1 != 0,
            }
        })
        .collect()
}

/// A search row's name field is a whole path; this is the part a listing shows.
fn base_of(path: &str) -> &str {
    let cut = path.rfind(['/', '\\']).map_or(0, |at| at + 1);
    &path[cut..]
}

struct Tree {
    /// Deleting the directory is `TempDir`'s job; holding it keeps the tree alive for the test.
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

/// A directory of `count` files, named so that natural and ordinal order agree.
fn tree(count: usize) -> Tree {
    let staged = tempfile::tempdir().expect("create tree");
    // The watched directory is a child of the temp dir, so the config and the tag database - which
    // SQLite grows with -wal and -shm files - are not rows in the listing being asserted on.
    let root = staged.path().join("watched");
    fs::create_dir_all(&root).expect("create watched");
    for index in 0..count {
        fs::write(
            root.join(format!("f{index:05}.txt")),
            vec![b'x'; index % 7 + 1],
        )
        .expect("write file");
    }
    fs::create_dir(root.join("sub")).expect("create subdir");
    Tree {
        db: staged.path().join("tags.sqlite"),
        config: staged.path().join("config.toml"),
        _dir: staged,
        root,
    }
}

#[test]
fn the_abi_advertises_two_and_every_header_says_so() {
    let tree = tree(3);
    let core = Core::with(&tree);

    assert_eq!(fam_abi_version(), ABI);
    let token = core.open(&tree.root);
    let header = core.wait_ready(token);
    assert_eq!(header.abi, ABI as u16);
    assert_eq!(header.state, READY);
    assert_eq!(
        header.total_rows, 4,
        "three files and one subdirectory, all listed"
    );
}

#[test]
fn opening_a_directory_returns_before_its_rows_exist() {
    let tree = tree(1500);
    let core = Core::with(&tree);

    let started = Instant::now();
    let token = core.open(&tree.root);
    let opened = started.elapsed();
    let header = core.state(token);

    /* What proves "open did not read the directory" is the assertion below it - the rows were not
     * there when open returned - not this number, which is a guard against a hang. Timed bars are not
     * evidence on a machine running the whole workspace's tests in parallel: 1s was once blown by
     * scheduling alone. The reading is printed because it is the number the note cites. */
    assert!(
        opened < Duration::from_secs(2),
        "open took {opened:?}; it must not read the directory itself"
    );
    assert_ne!(
        header.state, READY,
        "and the answer it gave was already 'rows are here' - the read ran inline"
    );
    let started = Instant::now();
    let ready = core.wait_ready(token);
    assert_eq!(ready.state, READY);
    assert_eq!(ready.total_rows, 1501);
    println!(
        "1501 rows were on offer {}ms after open (open itself took {:?})",
        started.elapsed().as_millis(),
        opened
    );
}

#[test]
fn a_window_of_rows_arrives_in_order_with_its_names() {
    let tree = tree(10);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    let (header, rows) = core.fetch(token, 0, 3, 4096);

    assert_eq!(header.row_count, 3, "exactly the requested window");
    assert_eq!(header.state, READY);
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        ["f00000.txt", "f00001.txt", "f00002.txt"],
        "name order, and a directory sorts among its own kind"
    );
    assert_eq!(rows[0].size, 1, "the record's own fields survived the copy");

    let (next, rows) = core.fetch(token, 3, 3, 4096);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "f00003.txt");
    assert_eq!(
        next.generation, header.generation,
        "same rows, same generation"
    );

    // Ten files and one directory: the last row is the directory, and asking past it is not an error.
    let (header, tail) = core.fetch(token, 10, 100, 4096);
    assert_eq!(tail.len(), 1, "a window past the end gives what is there");
    assert_eq!(
        header.flags & FLAG_TRUNCATED,
        0,
        "and nothing is owed, so nothing is flagged"
    );
    assert!(tail[0].directory, "and the last row is the subdirectory");
}

#[test]
fn a_buffer_too_small_for_the_whole_window_sends_nothing() {
    let tree = tree(10);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    // Room for the header and one record and its name, not three: half a row would be a corrupt
    // record at the far end, so the answer is "ask for something that fits".
    let (header, rows) = core.fetch(token, 0, 3, HEADER_BYTES + RECORD_BYTES + 64);
    assert!(
        rows.is_empty(),
        "a short buffer must not yield a partial row"
    );
    assert_eq!(header.row_count, 0);
    assert_eq!(
        header.flags & FLAG_TRUNCATED,
        FLAG_TRUNCATED,
        "and the header says so"
    );
    assert_eq!(
        header.state, READY,
        "the directory is fine; the buffer was not"
    );

    let room = HEADER_BYTES + 3 * (RECORD_BYTES + 22 * 2);
    let (header, rows) = core.fetch(token, 0, 3, room);
    assert_eq!(
        header.row_count, 3,
        "the same window fits in a buffer sized for it"
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn a_file_deleted_behind_the_view_comes_back_as_a_cut() {
    let tree = tree(4);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    fs::remove_file(tree.root.join("f00001.txt")).expect("delete one file");

    let (cuts, seq) = wait_for_cuts(&core, token);
    assert_eq!(
        seq, 1,
        "one poll reported something, so the sequence moved once"
    );
    assert_eq!(
        cuts,
        vec![1],
        "the second row of a name-ordered view is `f00001.txt`"
    );
    let (header, rows) = core.fetch(token, 0, 10, 4096);
    assert_eq!(header.total_rows, 4, "three files left, plus the directory");
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter().all(|row| row.name != "f00001.txt"),
        "and the deleted name is gone: {:?}",
        rows.iter().map(|row| &row.name).collect::<Vec<_>>()
    );
}

#[test]
fn two_cuts_in_one_window_come_back_in_the_order_they_must_be_applied() {
    let tree = tree(6);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    for name in ["f00001.txt", "f00003.txt", "f00005.txt"] {
        fs::remove_file(tree.root.join(name)).expect("delete");
    }

    let (cuts, _) = wait_for_cuts(&core, token);
    assert_eq!(
        cuts,
        vec![1, 2, 3],
        "f00001 was at 1; f00003 sat at 2 once it was gone; f00005 at 3 once both were"
    );
    let (_, rows) = core.fetch(token, 0, 10, 4096);
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["f00000.txt", "f00002.txt", "f00004.txt", "sub"]);
}

#[test]
fn a_file_created_behind_the_view_is_a_resync_not_an_invented_row() {
    let tree = tree(3);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    fs::write(tree.root.join("brand-new.txt"), "x").expect("create a file");
    // A create cannot be placed from an event - the event carries no size, no time and no answer to
    // "directory?" - so the rows are marked suspect rather than patched.
    let deadline = Instant::now() + Duration::from_secs(10);
    while core.state(token).state != STALE && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        core.state(token).state,
        STALE,
        "a row the event cannot describe leaves the table stale"
    );

    // Opening again is what asks for the re-read; the loader does it, and the first view is then
    // told to rebuild because its rows were replaced rather than patched.
    let second = core.open(&tree.root);
    core.wait_ready(second);

    let (header, cuts) = core.poll(token, 4096);
    assert!(
        header.flags & FLAG_RESYNC != 0,
        "expected the resync flag: {header:?}"
    );
    assert!(cuts.is_empty(), "a resync reports no positions");
    let (_, rows) = core.fetch(token, 0, 10, 4096);
    assert!(
        rows.iter().any(|row| row.name == "brand-new.txt"),
        "and the view follows the new table: {:?}",
        rows.iter().map(|row| &row.name).collect::<Vec<_>>()
    );
    core.close(second);
}

#[test]
fn a_sort_change_asks_the_model_to_rebuild() {
    let tree = tree(4);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    core.set_sort(token, "size", "descending");

    let header = core.state(token);
    assert!(header.flags & FLAG_RESYNC != 0, "a new order is a new view");
    let (header, rows) = core.fetch(token, 0, 10, 4096);
    assert_eq!(header.state, READY);
    let sizes: Vec<u64> = rows.iter().map(|row| row.size).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted, "descending by size, largest first");
}

#[test]
fn a_filter_narrows_the_window_it_is_applied_to() {
    let tree = tree(5);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    assert_eq!(core.set_filter(token, "[\"f0000\"]", None), 0);
    let (header, rows) = core.fetch(token, 0, 10, 4096);
    assert_eq!(header.total_rows, 5, "five of six names contain the term");
    assert_eq!(rows.len(), 5);

    assert_eq!(core.set_filter(token, "[]", None), 0);
    assert_eq!(core.state(token).total_rows, 6);
    assert!(
        core.set_filter(token, "not json", None) < 0,
        "a bad filter is refused"
    );
}

#[test]
fn unknown_tokens_and_bad_buffers_are_refused_without_a_panic() {
    let tree = tree(2);
    let core = Core::with(&tree);
    let token = core.open(&tree.root);
    core.wait_ready(token);

    let mut buffer = vec![0u8; 4096];
    let mut error = Error::new();
    assert_eq!(
        unsafe {
            fam_dir_state(
                core.handle,
                1 << 40,
                buffer.as_mut_ptr(),
                4096,
                &mut error.value,
            )
        },
        -1,
        "a token this core never handed out"
    );
    assert!(error.text().contains("unknown view token"));

    core.close(token);
    let mut error = Error::new();
    assert_eq!(
        unsafe {
            fam_dir_poll(
                core.handle,
                token,
                buffer.as_mut_ptr(),
                4096,
                &mut error.value,
            )
        },
        -1,
        "and the same, once closed"
    );
    let _ = error.text();

    let mut error = Error::new();
    assert_eq!(
        unsafe {
            fam_dir_fetch(
                core.handle,
                token,
                0,
                1,
                ptr::null_mut(),
                4096,
                &mut error.value,
            )
        },
        -1
    );
    assert!(error.text().contains("null"));

    let token = core.open(&tree.root);
    core.wait_ready(token);
    let mut error = Error::new();
    assert_eq!(
        unsafe { fam_dir_state(core.handle, token, buffer.as_mut_ptr(), 0, &mut error.value) },
        -1
    );
    assert!(error.text().contains("greater than zero"));
    core.close(token);
}

#[test]
fn the_backends_report_themselves_as_json() {
    let tree = tree(1);
    let core = Core::with(&tree);
    let mut out = ptr::null_mut();
    let mut error = Error::new();
    let code = unsafe { fam_backends_json(core.handle, &mut out, &mut error.value) };
    assert_eq!(code, 0, "{}", error.text());
    let text = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    unsafe { fam_free_string(out) };
    assert!(text.contains("\"backends\""), "{text}");
    assert!(text.contains("walk"), "{text}");
    // The record's `kind` byte is an index, so the names have to come from the core that sends it.
    assert!(text.contains("\"kinds\":["), "{text}");
    assert!(text.contains("directory"), "{text}");
    assert!(text.contains("executable"), "{text}");
}

/// Polls until the pipeline has reported positions, and fails if none arrive. Returns the cuts and
/// the sequence the poll that carried them reported.
fn wait_for_cuts(core: &Core, token: u64) -> (Vec<u32>, u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let (header, cuts) = core.poll(token, 4096);
        if !cuts.is_empty() {
            seen.extend(cuts);
            return (seen, header.seq);
        }
        assert_eq!(
            header.flags & FLAG_RESYNC,
            0,
            "a delete must be expressible as a cut, not a rebuild: {header:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (seen, 0)
}

/// Keeps the `fam_stats` extern out of the dead-code path: the v1 half of the library still answers
/// to the same handle, and this is the cheapest proof that both halves coexist.
#[test]
fn v1_and_v2_share_one_core() {
    let tree = tree(2);
    let core = Core::with(&tree);
    let mut out = ptr::null_mut();
    let mut error = Error::new();
    assert_eq!(
        unsafe { fam_stats(core.handle, &mut out, &mut error.value) },
        0,
        "{}",
        error.text()
    );
    let stats = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    unsafe { fam_free_string(out) };
    assert!(stats.contains("dropped_events"), "{stats}");

    let token = core.open(&tree.root);
    assert_eq!(core.wait_ready(token).state, READY, "v2 saw the same tree");
}

#[test]
fn a_search_begins_without_waiting_and_pages_rows_that_carry_their_paths() {
    let tree = tree(6);
    let core = Core::with(&tree);

    let started = Instant::now();
    let token = core.search("f0000", None, None, 0);
    let begun = started.elapsed();
    /* A hang guard, not a performance bar - see the note in
     * `opening_a_directory_returns_before_its_rows_exist`. What the claim is actually resting on:
     * `a_cancelled_search_stops_and_frees_the_thread_that_ran_it` shows a second search can start
     * while the first is being worked off, which is only possible if `begin` queued it, and the
     * shell's own log says `queued at 84ms: state=1` (LOADING). */
    assert!(
        begun < Duration::from_secs(2),
        "begin took {begun:?}; it queues the search and leaves, it does not run it"
    );

    let header = core.wait_settled(token);
    assert_eq!(header.state, READY);
    assert_eq!(
        header.flags & FLAG_INCOMPLETE,
        0,
        "the whole scope was covered: {header:?}"
    );
    assert_eq!(header.total_rows, 6, "six files match, `sub` does not");

    let (header, rows) = core.search_page(token, 0, 10, 8192);
    assert_eq!(header.row_count, 6);
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names.iter().map(|name| base_of(name)).collect::<Vec<_>>(),
        [
            "f00000.txt",
            "f00001.txt",
            "f00002.txt",
            "f00003.txt",
            "f00004.txt",
            "f00005.txt"
        ],
        "one sort, at the end, by the order a folder view uses"
    );
    let root = toml_path(&tree.root);
    assert!(
        names.iter().all(|name| name.starts_with(&root)),
        "a search row's name is a path, so a hit two folders deep is distinguishable: {names:?}"
    );
    core.close_search(token);
}

#[test]
fn a_scope_is_walked_and_reported_as_it_was_spelled() {
    let tree = tree(3);
    let mixed = tree.root.join("MixedCase");
    fs::create_dir(&mixed).expect("create scope");
    fs::write(mixed.join("q1-note.txt"), "x").expect("write file");
    fs::write(mixed.join("q2-note.txt"), "x").expect("write file");
    let core = Core::with(&tree);

    let token = core.search("q1", Some(&mixed), None, 0);
    let header = core.wait_settled(token);
    assert_eq!(header.total_rows, 1, "scoped to the subtree: {header:?}");

    let (_, rows) = core.search_page(token, 0, 10, 4096);
    assert!(
        rows[0].name.contains("MixedCase"),
        "the directory part keeps the caller's casing - a search that answered `MIXEDCASE` was \
         reading the folded identity: {}",
        rows[0].name
    );
}

/// The configured roots used to be a fence: a search scoped to any folder outside them answered
/// "nothing here" and reported the scope as covered, which is the one answer a user cannot tell apart
/// from an empty folder. A named scope is the caller restricting themselves, not the configuration
/// narrowing them further.
#[test]
fn a_search_scoped_outside_the_configured_roots_still_finds_its_rows() {
    let configured = tree(3);
    let elsewhere = tree(5);
    let core = Core::with(&configured);

    let token = core.search("f000", Some(&elsewhere.root), None, 0);
    let header = core.wait_settled(token);
    assert_eq!(
        header.total_rows, 5,
        "the folder holds five matching files and is not one of the roots: {header:?}"
    );
    assert_eq!(
        header.flags & FLAG_INCOMPLETE,
        0,
        "and it did cover the scope it was handed: {header:?}"
    );

    let second = core.search("f000", None, None, 0);
    assert_eq!(
        core.wait_settled(second).total_rows,
        3,
        "the same query with no scope is still confined to the configured roots"
    );
}

#[test]
fn a_limited_search_is_final_but_says_the_scope_was_not_covered() {
    let tree = tree(10);
    let core = Core::with(&tree);

    let token = core.search("f000", None, None, 3);
    let header = core.wait_settled(token);

    assert_eq!(header.state, READY, "a search at its ceiling is over");
    assert_ne!(
        header.flags & FLAG_INCOMPLETE,
        0,
        "and the caller is told it stopped early: {header:?}"
    );
    let (_, rows) = core.search_page(token, 0, 10, 8192);
    assert_eq!(rows.len(), 3, "exactly the ceiling, no more");
}

#[test]
fn a_cancelled_search_stops_and_frees_the_thread_that_ran_it() {
    let tree = tree(1500);
    let core = Core::with(&tree);

    let token = core.search("f000", None, None, 0);
    // Cancelled without waiting for any row: what is being proved is that the provider notices, not
    // that a caller can give up on it.
    core.cancel_search(token);

    let header = core.wait_settled(token);
    assert_eq!(
        header.state, READY,
        "a stopped search is final - leaving it LOADING would be v1's timeout all over again"
    );
    assert_ne!(
        header.flags & FLAG_INCOMPLETE,
        0,
        "and it keeps saying its rows are a prefix: {header:?}"
    );
    let (page, rows) = core.search_page(token, 0, 50, 8192);
    assert_eq!(
        page.state, READY,
        "what it found before the stop is still pageable"
    );
    assert!((rows.len() as u32) <= page.total_rows);

    // One owner thread serves every token, so the only way a second search can start is if the first
    // one really let go of it.
    let second = core.search("f0001", None, None, 5);
    assert_eq!(
        core.wait_settled(second).state,
        READY,
        "the search thread moved on instead of finishing a cancelled walk"
    );
}

#[test]
fn a_kind_filter_answers_with_the_kinds_it_was_asked_for() {
    let tree = tree(4);
    let core = Core::with(&tree);

    let token = core.search("sub", None, Some("directory"), 0);
    let header = core.wait_settled(token);
    assert_eq!(header.total_rows, 1);

    let (_, rows) = core.search_page(token, 0, 10, 4096);
    assert!(rows[0].directory, "the kind decided the record's flags");
    assert_eq!(
        rows[0].size, 0,
        "and a directory reported no size of its own"
    );
    assert!(
        rows[0].name.ends_with("sub"),
        "path ends at the name: {}",
        rows[0].name
    );
}

#[test]
fn an_empty_query_and_a_closed_token_are_refused() {
    let tree = tree(2);
    let core = Core::with(&tree);

    let (token, message) = core.try_search("   ", None, None, 0);
    assert_eq!(token, 0, "a query with no terms is not a search");
    assert!(message.contains("no search terms"), "{message}");

    let token = core.search("f", None, None, 0);
    core.wait_settled(token);
    core.close_search(token);

    let mut buffer = vec![0u8; 4096];
    let mut error = Error::new();
    assert_eq!(
        unsafe {
            fam_search_page(
                core.handle,
                token,
                0,
                1,
                buffer.as_mut_ptr(),
                4096,
                &mut error.value,
            )
        },
        -1,
        "closing a search releases its rows"
    );
    assert!(error.text().contains("unknown search token"));

    let mut error = Error::new();
    assert_eq!(
        unsafe { fam_search_cancel(core.handle, token, &mut error.value) },
        -1,
        "and there is nothing left to cancel"
    );
    let _ = error.text();
    assert_eq!(
        unsafe { fam_search_cancel(core.handle, 1 << 40, ptr::null_mut()) },
        -1,
        "a null error pointer is not a reason to answer 0"
    );
}
