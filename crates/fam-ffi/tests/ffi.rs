use std::ffi::{c_char, CString};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use famcore::{
    fam_abi_version, fam_core_free, fam_core_new, fam_find, fam_free_string, fam_stats,
    fam_tag_add, fam_tag_list, fam_tag_remove, fam_view_drain, fam_view_free, fam_view_new,
    fam_view_search, fam_watch_start, fam_watch_stop, FamCore,
};
use serde_json::Value;

/// Forward slashes keep TOML basic strings free of escape headaches, and Windows accepts them.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Core(*mut FamCore);

impl Core {
    fn with_config(tree: &Path, db: &Path) -> Core {
        let config = tree.parent().unwrap().join("config.toml");
        fs::write(
            &config,
            format!(
                "database_path = \"{}\"\nsearch_roots = [\"{}\"]\nui_batch_ms = 20\nscan_timeout_secs = 20\n",
                toml_path(db),
                toml_path(tree)
            ),
        )
        .unwrap();
        let path = CString::new(toml_path(&config)).unwrap();
        let mut err: *mut c_char = std::ptr::null_mut();
        let pointer = unsafe { fam_core_new(path.as_ptr(), &mut err) };
        assert!(!pointer.is_null(), "core failed to start: {}", unsafe {
            take(err)
        });
        Core(pointer)
    }

    fn handle(&self) -> *mut FamCore {
        self.0
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe { fam_core_free(self.0) };
    }
}

unsafe fn take(pointer: *mut c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let text = std::ffi::CStr::from_ptr(pointer)
        .to_string_lossy()
        .into_owned();
    fam_free_string(pointer);
    text
}

fn text(value: &str) -> CString {
    CString::new(value).unwrap()
}

struct Out {
    value: *mut c_char,
    error: *mut c_char,
}

impl Out {
    fn new() -> Out {
        Out {
            value: std::ptr::null_mut(),
            error: std::ptr::null_mut(),
        }
    }

    fn ok(&self) -> Value {
        assert!(self.error.is_null(), "unexpected error: {}", unsafe {
            std::ffi::CStr::from_ptr(self.error).to_string_lossy()
        });
        assert!(!self.value.is_null(), "no result produced");
        let payload = unsafe { std::ffi::CStr::from_ptr(self.value) }
            .to_str()
            .unwrap()
            .to_string();
        serde_json::from_str(&payload)
            .unwrap_or_else(|error| panic!("bad JSON {payload:?}: {error}"))
    }

    fn message(&self) -> String {
        assert!(!self.error.is_null(), "no error reported");
        unsafe { std::ffi::CStr::from_ptr(self.error) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        unsafe {
            fam_free_string(self.value);
            fam_free_string(self.error);
        }
    }
}

fn sample_tree(root: &Path) {
    for (name, body) in [
        ("notes.txt", "meeting notes"),
        ("reports/q1-report.pdf", "pdf"),
        ("photos/sunset.jpg", "jpeg"),
    ] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    tree: std::path::PathBuf,
    core: Core,
}

fn harness() -> Harness {
    let temp = tempfile::tempdir().unwrap();
    let tree = temp.path().join("tree");
    fs::create_dir_all(&tree).unwrap();
    sample_tree(&tree);
    let db = temp.path().join("data/tags.db");
    let core = Core::with_config(&tree, &db);
    Harness {
        _temp: temp,
        tree,
        core,
    }
}

#[test]
fn abi_version_is_advertised() {
    assert_eq!(fam_abi_version(), 1);
}

#[test]
fn a_null_handle_is_reported_not_dereferenced() {
    let mut out = Out::new();
    let code = unsafe { fam_stats(std::ptr::null_mut(), &mut out.value, &mut out.error) };

    assert_eq!(code, -1);
    assert_eq!(out.message(), "core handle is null");
    unsafe { fam_view_free(std::ptr::null_mut(), 7) };
    unsafe { fam_free_string(std::ptr::null_mut()) };
}

#[test]
fn tags_round_trip_through_the_abi() {
    let h = harness();
    let file = text(&h.tree.join("notes.txt").display().to_string());
    let mut out = Out::new();

    let code = unsafe {
        fam_tag_add(
            h.core.handle(),
            file.as_ptr(),
            text(r#"["keep","归档"]"#).as_ptr(),
            std::ptr::null(),
            3,
            &mut out.value,
            &mut out.error,
        )
    };
    assert_eq!(code, 0, "{}", out.message());
    assert_eq!(out.ok()["added"], 2);
    assert_eq!(out.ok()["present"], 0);

    let mut again = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            file.as_ptr(),
            text(r#"["keep"]"#).as_ptr(),
            std::ptr::null(),
            3,
            &mut again.value,
            &mut again.error,
        )
    };
    assert_eq!(again.ok()["present"], 1, "re-adding is idempotent");

    let mut listed = Out::new();
    unsafe {
        fam_tag_list(
            h.core.handle(),
            file.as_ptr(),
            &mut listed.value,
            &mut listed.error,
        )
    };
    let tags = listed.ok();
    assert_eq!(tags.as_array().unwrap().len(), 2);
    assert_eq!(tags[0]["source"], "manual");
    assert_eq!(tags[0]["priority"], 3);

    let mut removed = Out::new();
    unsafe {
        fam_tag_remove(
            h.core.handle(),
            file.as_ptr(),
            text(r#"["keep"]"#).as_ptr(),
            std::ptr::null(),
            &mut removed.value,
            &mut removed.error,
        )
    };
    assert_eq!(removed.ok()["removed"], 1);
}

#[test]
fn malformed_tag_payloads_are_rejected_with_a_message() {
    let h = harness();
    let file = text(h.tree.join("notes.txt").to_str().unwrap());
    let mut out = Out::new();

    let code = unsafe {
        fam_tag_add(
            h.core.handle(),
            file.as_ptr(),
            text("not json").as_ptr(),
            std::ptr::null(),
            0,
            &mut out.value,
            &mut out.error,
        )
    };

    assert_eq!(code, -1);
    assert!(out.message().contains("JSON array"), "{}", out.message());
    assert!(
        out.value.is_null(),
        "no result should be written on failure"
    );

    let mut invalid_tag = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            file.as_ptr(),
            text(r#"["bad/tag"]"#).as_ptr(),
            std::ptr::null(),
            0,
            &mut invalid_tag.value,
            &mut invalid_tag.error,
        )
    };
    assert_eq!(invalid_tag.value, std::ptr::null_mut());
    assert!(
        invalid_tag.message().contains("invalid tag"),
        "{}",
        invalid_tag.message()
    );
}

#[test]
fn find_returns_paths_with_their_tags() {
    let h = harness();
    let first = text(h.tree.join("notes.txt").to_str().unwrap());
    let second = text(h.tree.join("reports/q1-report.pdf").to_str().unwrap());
    let mut out = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            first.as_ptr(),
            text(r#"["finance"]"#).as_ptr(),
            std::ptr::null(),
            0,
            &mut out.value,
            &mut out.error,
        )
    };
    let mut second_out = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            second.as_ptr(),
            text(r#"["finance","q1"]"#).as_ptr(),
            std::ptr::null(),
            0,
            &mut second_out.value,
            &mut second_out.error,
        )
    };

    let mut any = Out::new();
    let code = unsafe {
        fam_find(
            h.core.handle(),
            text(r#"["finance"]"#).as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut any.value,
            &mut any.error,
        )
    };
    assert_eq!(code, 0, "{}", any.message());
    assert_eq!(any.ok().as_array().unwrap().len(), 2);

    let mut all = Out::new();
    unsafe {
        fam_find(
            h.core.handle(),
            text(r#"["finance","q1"]"#).as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut all.value,
            &mut all.error,
        )
    };
    let hits = all.ok();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert!(hits[0]["path"].as_str().unwrap().ends_with("q1-report.pdf"));
}

#[test]
fn stats_describe_the_open_database_and_roots() {
    let h = harness();
    let mut out = Out::new();

    let code = unsafe { fam_stats(h.core.handle(), &mut out.value, &mut out.error) };

    assert_eq!(code, 0, "{}", out.message());
    let stats = out.ok();
    assert!(stats["database"].as_str().unwrap().ends_with("tags.db"));
    assert!(!stats["roots"].as_array().unwrap().is_empty());
    assert_eq!(stats["files"], 0);
}

#[test]
fn a_view_search_resets_and_live_events_stream_as_deltas() {
    let h = harness();
    let mut err: *mut c_char = std::ptr::null_mut();
    let token = unsafe {
        fam_view_new(
            h.core.handle(),
            text("name").as_ptr(),
            text("ascending").as_ptr(),
            &mut err,
        )
    };
    assert!(token > 0, "{}", unsafe { take(err) });

    let mut out = Out::new();
    let code = unsafe {
        fam_view_search(
            h.core.handle(),
            token,
            text("report").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut out.value,
            &mut out.error,
        )
    };
    assert_eq!(code, 0, "{}", out.message());
    let rows = out.ok();
    assert_eq!(rows["backend"], "walk");
    assert_eq!(
        rows["rows"].as_array().unwrap().len(),
        2,
        "directory and file both match"
    );

    let watcher = unsafe {
        fam_watch_start(
            h.core.handle(),
            text(h.tree.to_str().unwrap()).as_ptr(),
            &mut err,
        )
    };
    assert!(watcher > 0, "{}", unsafe { take(err) });

    fs::write(h.tree.join("late-report.txt"), "x").unwrap();

    let mut inserted = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !inserted {
        let mut drained = Out::new();
        unsafe {
            fam_view_drain(
                h.core.handle(),
                token,
                &mut drained.value,
                &mut drained.error,
            )
        };
        let update = drained.ok();
        inserted =
            update["deltas"].as_array().unwrap().iter().any(|delta| {
                delta["type"] == "insert" && delta["row"]["name"] == "late-report.txt"
            });
        if !inserted {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    unsafe { fam_watch_stop(h.core.handle(), watcher) };
    assert!(inserted, "the new file never arrived as an insert delta");

    let mut after = Out::new();
    unsafe { fam_view_drain(h.core.handle(), token, &mut after.value, &mut after.error) };
    assert!(after.ok()["rows"].as_i64().unwrap() >= 3);

    let mut unknown = Out::new();
    let code = unsafe {
        fam_view_search(
            h.core.handle(),
            9999,
            text("report").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut unknown.value,
            &mut unknown.error,
        )
    };
    assert_eq!(code, -1);
    assert!(
        unknown.message().contains("unknown view token"),
        "{}",
        unknown.message()
    );

    unsafe { fam_view_free(h.core.handle(), token) };
}

#[test]
fn bad_arguments_and_sorts_fail_cleanly() {
    let h = harness();
    let mut err: *mut c_char = std::ptr::null_mut();

    let token = unsafe {
        fam_view_new(
            h.core.handle(),
            text("nonsense").as_ptr(),
            std::ptr::null(),
            &mut err,
        )
    };
    assert_eq!(token, 0);
    assert!(unsafe { take(err) }.contains("unknown sort key"));

    let mut out = Out::new();
    let code = unsafe {
        fam_view_search(
            h.core.handle(),
            1,
            text("   ").as_ptr(),
            std::ptr::null(),
            text("spreadsheet").as_ptr(),
            0,
            &mut out.value,
            &mut out.error,
        )
    };
    assert_eq!(code, -1);
    assert!(out.message().contains("unknown kind"), "{}", out.message());

    let mut empty = Out::new();
    let code = unsafe {
        fam_view_search(
            h.core.handle(),
            1,
            text("   ").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut empty.value,
            &mut empty.error,
        )
    };
    assert_eq!(code, -1);
    assert!(
        empty.message().contains("no search terms"),
        "{}",
        empty.message()
    );
}

#[test]
fn non_utf8_arguments_are_refused() {
    let h = harness();
    let raw = CString::new(vec![0xff_u8, 0xfe_u8]).unwrap();
    let mut out = Out::new();

    let code = unsafe {
        fam_tag_list(
            h.core.handle(),
            raw.as_ptr(),
            &mut out.value,
            &mut out.error,
        )
    };

    assert_eq!(code, -1);
    assert!(
        out.message().contains("not valid UTF-8"),
        "{}",
        out.message()
    );
}

#[test]
fn search_rows_carry_their_tags_in_display_order() {
    let h = harness();
    let notes = text(h.tree.join("notes.txt").to_str().unwrap());
    let mut tagged = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            notes.as_ptr(),
            text(r#"["zed","alpha"]"#).as_ptr(),
            std::ptr::null(),
            0,
            &mut tagged.value,
            &mut tagged.error,
        )
    };
    assert_eq!(tagged.ok()["added"], 2);

    let mut err: *mut c_char = std::ptr::null_mut();
    let view = unsafe {
        fam_view_new(
            h.core.handle(),
            text("name").as_ptr(),
            text("ascending").as_ptr(),
            &mut err,
        )
    };
    assert!(view > 0);

    let mut result = Out::new();
    let code = unsafe {
        fam_view_search(
            h.core.handle(),
            view,
            text("notes").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut result.value,
            &mut result.error,
        )
    };
    assert_eq!(code, 0, "{}", result.message());

    let rows = result.ok();
    let hit = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "notes.txt")
        .expect("notes.txt should be in the search result");
    assert_eq!(hit["tags"], serde_json::json!(["alpha", "zed"]));
}
