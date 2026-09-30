//! The tag store answering a search: what one hit carries, what gets refused, and what happens to a
//! label whose file is gone.
//!
//! These run against real files in a temporary tree rather than a fixture, because the whole claim
//! under test is that a tag hit arrives with the same fields a name hit does - size, modified time,
//! kind, the folder bit - and those come from the filesystem, not from the library.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fam_core::{
    path_buf_from_units, Cancellation, Kind, SearchError, SearchHit, SearchProvider, SearchRequest,
};
use fam_svc_store::{NewTag, TagSearch, TagStore};

fn write(path: &Path, body: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    path.to_path_buf()
}

fn backend(store: TagStore) -> TagSearch {
    TagSearch::new(Arc::new(Mutex::new(store)))
}

fn tag(store: &TagStore, path: &Path, name: &str) {
    store
        .add_tags(path, &[NewTag::manual(name)])
        .expect("the tag is stored");
}

/// One run, and whether the backend says it looked at everything it could have shown.
fn run(provider: &dyn SearchProvider, request: &SearchRequest) -> (Vec<SearchHit>, bool) {
    let mut hits = Vec::new();
    let outcome = provider
        .search(request, &Cancellation::new(), &mut |hit| {
            hits.push(hit);
            true
        })
        .expect("the tag backend answers a tag request");
    (hits, outcome.complete)
}

fn base(hit: &SearchHit) -> String {
    path_buf_from_units(&hit.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn mut_names(hits: &[SearchHit]) -> Vec<String> {
    let mut names: Vec<String> = hits.iter().map(base).collect();
    names.sort();
    names
}

#[test]
fn a_tag_hit_arrives_with_the_fields_a_row_paints() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    let contract = write(&root.path().join("2024-合同.pdf"), "one hundred bytes!");
    let folder = root.path().join("归档");
    fs::create_dir_all(&folder).unwrap();

    tag(&store, &contract, "contract");
    tag(&store, &folder, "contract");

    let (hits, complete) = run(
        &backend(store),
        &SearchRequest::new("tag:contract").limit(10),
    );

    assert!(complete, "a library of two rows is fully examined");
    assert_eq!(mut_names(&hits), vec!["2024-合同.pdf", "归档"]);
    let file = hits.iter().find(|hit| base(hit).ends_with(".pdf")).unwrap();
    // The bytes are the file's own, not zero and not the store's guess: the store has no size column,
    // so a row that printed `0 B` here would be the defect increment 13 already fixed once.
    assert_eq!(file.size, "one hundred bytes!".len() as u64);
    assert_eq!(file.kind, Kind::Document);
    assert!(file.mtime_ms > 0, "a real modified time, not the sentinel");
    let directory = hits.iter().find(|hit| base(hit) == "归档").unwrap();
    assert_eq!(directory.kind, Kind::Directory, "a folder says it is one");
    assert_eq!(
        directory.size, 0,
        "and its entry size is not printed as a byte count"
    );
}

#[test]
fn a_name_term_narrows_the_tagged_set() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    for name in ["a.pdf", "b.pdf", "c.txt"] {
        let path = write(&root.path().join(name), "x");
        tag(&store, &path, "contract");
    }

    let (hits, _) = run(&backend(store), &SearchRequest::new("tag:contract b.p"));

    assert_eq!(mut_names(&hits), vec!["b.pdf"]);
}

#[test]
fn a_scope_reaches_one_folder_and_its_descendants() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    let inside = write(&root.path().join("in/note.txt"), "x");
    let outside = write(&root.path().join("out/note.txt"), "x");
    tag(&store, &inside, "contract");
    tag(&store, &outside, "contract");

    let (hits, _) = run(
        &backend(store),
        &SearchRequest::new("tag:contract").scope(root.path().join("in")),
    );

    assert_eq!(mut_names(&hits), vec!["note.txt"]);
    assert_eq!(path_buf_from_units(&hits[0].path), inside);
}

#[test]
fn a_label_whose_file_is_gone_reports_nothing_and_still_says_it_was_complete() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    let gone = write(&root.path().join("temp.txt"), "x");
    tag(&store, &gone, "contract");
    fs::remove_file(&gone).unwrap();

    let (hits, complete) = run(&backend(store), &SearchRequest::new("tag:contract"));

    // Deleting a file leaves its tag rows behind - measured in the increment 16 account, where two
    // `files` rows outlived the one tag. The answer is to skip the row, not to invent one.
    assert!(hits.is_empty(), "a path that does not exist is not a file");
    assert!(
        complete,
        "nothing was left unexamined: the library held one label and it resolved to nothing"
    );
}

#[test]
fn two_named_tags_mean_both_of_them() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    let both = write(&root.path().join("both.txt"), "x");
    let one = write(&root.path().join("one.txt"), "x");
    tag(&store, &both, "contract");
    tag(&store, &both, "urgent");
    tag(&store, &one, "contract");

    let (hits, _) = run(
        &backend(store),
        &SearchRequest::new("tag:contract tag:urgent"),
    );

    assert_eq!(mut_names(&hits), vec!["both.txt"]);
}

#[test]
fn a_request_that_named_no_tag_is_not_answered_here() {
    let store = TagStore::in_memory().unwrap();
    let provider = backend(store);

    let plain = SearchRequest::new("pdf");
    assert!(!plain.names_tags());
    assert!(
        !provider.covers(&plain),
        "answering a name search from the label library would report the tagged subset and call it \
         the whole filesystem"
    );

    let error = provider
        .search(&plain, &Cancellation::new(), &mut |_| true)
        .expect_err("and it refuses rather than runs");
    assert!(matches!(error, SearchError::Empty), "got {error:?}");
}

#[test]
fn a_tag_spelled_in_another_case_matches_nothing_because_tags_are_exact() {
    let root = tempfile::tempdir().unwrap();
    let store = TagStore::in_memory().unwrap();
    let path = write(&root.path().join("note.txt"), "x");
    tag(&store, &path, "PDF");

    let provider = backend(store);
    let (hits, _) = run(&provider, &SearchRequest::new("tag:pdf"));
    assert!(hits.is_empty(), "the store compares tags exactly");

    // The prefix is what carries the tag, so the case a person typed survives to the query. A parser
    // that lower-cased every token the way it does for name terms would make this the only answer.
    let (same, _) = run(&provider, &SearchRequest::new("tag:PDF"));
    assert_eq!(mut_names(&same), vec!["note.txt"]);
}
