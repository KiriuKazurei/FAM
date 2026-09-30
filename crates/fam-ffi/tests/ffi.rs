use std::ffi::{c_char, CString};
use std::fs;
use std::path::Path;

use famcore::{
    fam_abi_version, fam_core_free, fam_core_new, fam_dir_close, fam_free_string, fam_note_clear,
    fam_note_get, fam_note_set, fam_notes_for_paths, fam_stats, fam_tag_add, fam_tag_list,
    fam_tag_merge, fam_tag_remove, fam_tag_rename, FamCore,
};
use serde_json::Value;

/// Forward slashes keep TOML basic strings free of escape headaches, and Windows accepts them.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Core(*mut FamCore);

impl Core {
    fn with_config(tree: &Path, db: &Path) -> Core {
        let root = tree.parent().unwrap();
        let config = root.join("config.toml");
        fs::write(
            &config,
            format!(
                "database_path = \"{}\"\nsearch_roots = [\"{}\"]\nui_batch_ms = 20\nscan_timeout_secs = 20\naudit_dir = \"{}\"\n",
                toml_path(db),
                toml_path(tree),
                toml_path(&root.join("audit"))
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

/// The half of the ABI that stayed JSON after the v2 rows went in: tags, `find`, `stats`, and the
/// guards every entry point shares. The v1 view trio that used to live here is gone - increment 9
/// deleted it, and `v2.rs` is where the row-shaped claims now live.
#[test]
fn abi_version_is_advertised() {
    assert_eq!(fam_abi_version(), 2);
}

#[test]
fn a_null_handle_is_reported_not_dereferenced() {
    let mut out = Out::new();
    let code = unsafe { fam_stats(std::ptr::null_mut(), &mut out.value, &mut out.error) };

    assert_eq!(code, -1);
    assert_eq!(out.message(), "core handle is null");
    /* A void entry point has no error channel to fail through, so the only claim worth making is
     * that it returns at all. `fam_dir_close` is v2's; the v1 one this test used is gone. */
    unsafe { fam_dir_close(std::ptr::null_mut(), 7) };
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
fn merge_moves_a_name_everywhere_and_rename_moves_it_on_one_file() {
    let h = harness();
    let notes = text(h.tree.join("notes.txt").to_str().unwrap());
    let report = text(h.tree.join("reports/q1-report.pdf").to_str().unwrap());
    for (path, name) in [(&notes, "finance"), (&report, "finace")] {
        let mut out = Out::new();
        let tags = CString::new(format!("[\"{name}\"]")).unwrap();
        let code = unsafe {
            fam_tag_add(
                h.core.handle(),
                path.as_ptr(),
                tags.as_ptr(),
                std::ptr::null(),
                0,
                &mut out.value,
                &mut out.error,
            )
        };
        assert_eq!(code, 0, "{}", out.message());
    }

    // The typo leaves the library: the row truly moves, and the vocabulary has one name after.
    let mut merged = Out::new();
    let code = unsafe {
        fam_tag_merge(
            h.core.handle(),
            text("finace").as_ptr(),
            text("finance").as_ptr(),
            &mut merged.value,
            &mut merged.error,
        )
    };
    assert_eq!(code, 0, "{}", merged.message());
    let outcome = merged.ok();
    assert_eq!(
        (
            outcome["moved"].as_i64(),
            outcome["collapsed"].as_i64(),
            outcome["files"].as_i64()
        ),
        (Some(1), Some(0), Some(2)),
        "{outcome}"
    );

    // The same call onto a name the file already carries collapses rather than doubles.
    let mut second = Out::new();
    unsafe {
        fam_tag_add(
            h.core.handle(),
            report.as_ptr(),
            text(r#"["finance"]"#).as_ptr(),
            std::ptr::null(),
            0,
            &mut second.value,
            &mut second.error,
        )
    };
    let mut collapse = Out::new();
    let code = unsafe {
        fam_tag_merge(
            h.core.handle(),
            text("finance").as_ptr(),
            text("billing").as_ptr(),
            &mut collapse.value,
            &mut collapse.error,
        )
    };
    assert_eq!(code, 0, "{}", collapse.message());
    let outcome = collapse.ok();
    assert_eq!(
        (
            outcome["moved"].as_i64(),
            outcome["collapsed"].as_i64(),
            outcome["files"].as_i64()
        ),
        (Some(2), Some(0), Some(2)),
        "a rename across two files moves both rows: {outcome}"
    );

    // The per-file entry point touches one row on one path and leaves the other file's name alone.
    let mut renamed = Out::new();
    let code = unsafe {
        fam_tag_rename(
            h.core.handle(),
            report.as_ptr(),
            text("billing").as_ptr(),
            text("q1").as_ptr(),
            std::ptr::null(),
            &mut renamed.value,
            &mut renamed.error,
        )
    };
    assert_eq!(code, 0, "{}", renamed.message());
    let mut listed = Out::new();
    unsafe {
        fam_tag_list(
            h.core.handle(),
            report.as_ptr(),
            &mut listed.value,
            &mut listed.error,
        )
    };
    let tags: Vec<String> = listed
        .ok()
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag["tag"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tags, ["q1"], "only this file's name moved: {tags:?}");
    let mut other = Out::new();
    unsafe {
        fam_tag_list(
            h.core.handle(),
            notes.as_ptr(),
            &mut other.value,
            &mut other.error,
        )
    };
    let tags: Vec<String> = other
        .ok()
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag["tag"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tags, ["billing"], "{tags:?}");

    // A call that would change nothing is refused, not answered with success.
    let mut nothing = Out::new();
    let code = unsafe {
        fam_tag_rename(
            h.core.handle(),
            report.as_ptr(),
            text("q1").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &mut nothing.value,
            &mut nothing.error,
        )
    };
    assert_ne!(code, 0, "a rename with no target must be refused");
}

#[test]
fn a_note_round_trips_and_a_blank_write_is_refused_not_read_as_a_clear() {
    let h = harness();
    let target = text(h.tree.join("notes.txt").to_str().unwrap());
    let mut out = Out::new();
    unsafe {
        fam_note_get(
            h.core.handle(),
            target.as_ptr(),
            &mut out.value,
            &mut out.error,
        )
    };
    assert_eq!(
        out.ok()["body"],
        Value::Null,
        "no note yet is null, not an error"
    );

    let mut set = Out::new();
    let set_code = unsafe {
        fam_note_set(
            h.core.handle(),
            target.as_ptr(),
            text("the port is 5900, not the one in the wiki").as_ptr(),
            &mut set.value,
            &mut set.error,
        )
    };
    assert_eq!(set_code, 0, "{}", set.message());

    let mut back = Out::new();
    unsafe {
        fam_note_get(
            h.core.handle(),
            target.as_ptr(),
            &mut back.value,
            &mut back.error,
        )
    };
    let note = back.ok();
    assert_eq!(note["body"], "the port is 5900, not the one in the wiki");
    assert!(note["updated_at"].is_string(), "{note}");

    // A blank body is the store's refusal, spelled the same way an agent hears it - and the note
    // that was there is still there, which is the whole point of refusing instead of clearing.
    let mut blank = Out::new();
    let code = unsafe {
        fam_note_set(
            h.core.handle(),
            target.as_ptr(),
            text("   ").as_ptr(),
            &mut blank.value,
            &mut blank.error,
        )
    };
    assert_ne!(code, 0, "a blank note must be refused");
    assert!(
        blank.message().contains("must say something"),
        "{}",
        blank.message()
    );

    let mut cleared = Out::new();
    let code = unsafe {
        fam_note_clear(
            h.core.handle(),
            target.as_ptr(),
            &mut cleared.value,
            &mut cleared.error,
        )
    };
    assert_eq!(code, 0);
    assert_eq!(cleared.ok()["cleared"], Value::Bool(true));
    // The second clear says there was nothing - "cleared" and "was empty" stay two facts.
    let mut again = Out::new();
    unsafe {
        fam_note_clear(
            h.core.handle(),
            target.as_ptr(),
            &mut again.value,
            &mut again.error,
        )
    };
    assert_eq!(again.ok()["cleared"], Value::Bool(false));

    // The batch read: a window of two paths, one carrying a note - keyed by the caller's own
    // spelling, the other absent rather than present with a false, and an empty window answered
    // with an empty object because a shell really asks that.
    let mut seeded = Out::new();
    let seed_code = unsafe {
        fam_note_set(
            h.core.handle(),
            target.as_ptr(),
            text("noted").as_ptr(),
            &mut seeded.value,
            &mut seeded.error,
        )
    };
    assert_eq!(seed_code, 0, "{}", seeded.message());
    let mut batch = Out::new();
    let code = unsafe {
        fam_notes_for_paths(
            h.core.handle(),
            text(&format!(
                "[\"{}\", \"{}\"]",
                toml_path(&h.tree.join("notes.txt")),
                toml_path(&h.tree.join("reports"))
            ))
            .as_ptr(),
            &mut batch.value,
            &mut batch.error,
        )
    };
    assert_eq!(code, 0, "{}", batch.message());
    let noted = batch.ok();
    assert_eq!(
        noted.as_object().unwrap().len(),
        1,
        "only the noted path is in the answer: {noted}"
    );
    assert_eq!(
        noted[toml_path(&h.tree.join("notes.txt"))],
        Value::Bool(true)
    );

    let mut empty = Out::new();
    let code = unsafe {
        fam_notes_for_paths(
            h.core.handle(),
            text("[]").as_ptr(),
            &mut empty.value,
            &mut empty.error,
        )
    };
    assert_eq!(code, 0);
    assert!(empty.ok().as_object().unwrap().is_empty());
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
