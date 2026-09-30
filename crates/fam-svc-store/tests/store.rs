use std::path::Path;

use fam_svc_store::{NewTag, SearchRequest, Source, StoreError, TagStore};

fn store() -> TagStore {
    TagStore::in_memory().expect("in-memory store")
}

fn tags(names: &[&str]) -> Vec<NewTag> {
    names.iter().map(|name| NewTag::manual(*name)).collect()
}

#[test]
fn adding_the_same_tag_twice_stores_one_row() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");

    let first = store.add_tags(file, &[NewTag::manual("reading")]).unwrap();
    let second = store.add_tags(file, &[NewTag::manual("reading")]).unwrap();

    assert_eq!((first.inserted, first.refreshed), (1, 0));
    assert_eq!((second.inserted, second.refreshed), (0, 1));
    assert_eq!(second.total(), 1);
    assert_eq!(store.tags_of(file, None).unwrap().len(), 1);
}

#[test]
fn one_invalid_tag_rolls_back_the_whole_call() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");

    let error = store
        .add_tags(file, &tags(&["valid", "bad/tag"]))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::InvalidTag { .. }),
        "got {error:?}"
    );
    assert!(store.tags_of(file, None).unwrap().is_empty());
    assert!(!store.has_file(file).unwrap());
    assert_eq!(store.stats().unwrap().files, 0);
}

#[test]
fn case_and_separator_variants_alias_the_same_file() {
    let store = store();
    store
        .add_tags(Path::new(r"d:\Library\book.pdf"), &tags(&["x"]))
        .unwrap();

    let aliased = store
        .tags_of(Path::new("D:/LIBRARY/BOOK.PDF"), None)
        .unwrap();

    assert_eq!(
        aliased.iter().map(|t| t.tag.as_str()).collect::<Vec<_>>(),
        vec!["x"]
    );
    assert_eq!(store.stats().unwrap().files, 1);
}

#[test]
fn manual_tags_outrank_auto_then_priority_wins() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    let entries = vec![
        NewTag::manual("manual-low"),
        NewTag::auto("auto-high").with_priority(9),
        NewTag::manual("manual-high").with_priority(5),
    ];
    store.add_tags(file, &entries).unwrap();

    let order: Vec<String> = store
        .tags_of(file, None)
        .unwrap()
        .into_iter()
        .map(|t| t.tag)
        .collect();

    assert_eq!(order, vec!["manual-high", "manual-low", "auto-high"]);
}

#[test]
fn removing_by_source_only_touches_that_source() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    store
        .add_tags(
            file,
            &[
                NewTag::manual("keep"),
                NewTag::auto("drop-a"),
                NewTag::auto("drop-b"),
            ],
        )
        .unwrap();

    let removed = store.remove_tags(file, &[], Some(Source::Auto)).unwrap();

    assert_eq!(removed, 2);
    assert_eq!(store.tags_of(file, None).unwrap().len(), 1);
}

#[test]
fn removing_named_tags_leaves_the_rest() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    store.add_tags(file, &tags(&["a", "b", "c"])).unwrap();

    let removed = store
        .remove_tags(file, &["a".to_string(), "c".to_string()], None)
        .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(store.tags_of(file, None).unwrap()[0].tag, "b");
}

#[test]
fn rename_reports_missing_tags_and_collisions() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    store.add_tags(file, &tags(&["draft", "final"])).unwrap();

    let missing = store
        .rename_tag(file, "absent", Some("x"), None)
        .unwrap_err();
    assert!(
        matches!(missing, StoreError::NotFound { .. }),
        "got {missing:?}"
    );

    let clash = store
        .rename_tag(file, "draft", Some("final"), None)
        .unwrap_err();
    assert!(
        matches!(clash, StoreError::Conflict { .. }),
        "got {clash:?}"
    );

    store
        .rename_tag(file, "draft", Some("revised"), None)
        .unwrap();
    let names: Vec<String> = store
        .tags_of(file, None)
        .unwrap()
        .into_iter()
        .map(|t| t.tag)
        .collect();
    assert_eq!(names, vec!["final", "revised"]);
}

#[test]
fn rename_with_no_new_name_only_changes_source() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    store.add_tags(file, &tags(&["draft"])).unwrap();

    store
        .rename_tag(file, "draft", None, Some(Source::Auto))
        .unwrap();

    let record = &store.tags_of(file, None).unwrap()[0];
    assert_eq!(record.tag, "draft");
    assert_eq!(record.source, Source::Auto);
    assert!(store.rename_tag(file, "draft", None, None).is_err());
}

#[test]
fn set_priority_requires_an_existing_tag() {
    let store = store();
    let file = Path::new("D:/Library/book.pdf");
    store.add_tags(file, &tags(&["a"])).unwrap();

    store.set_priority(file, "a", Source::Manual, 7).unwrap();
    assert_eq!(store.tags_of(file, None).unwrap()[0].priority, 7);

    let error = store
        .set_priority(file, "absent", Source::Manual, 1)
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }));
}

#[test]
fn search_matches_all_versus_any_tag() {
    let store = store();
    store
        .add_tags(Path::new("D:/docs/both.pdf"), &tags(&["invoice", "2026"]))
        .unwrap();
    store
        .add_tags(Path::new("D:/docs/only-invoice.pdf"), &tags(&["invoice"]))
        .unwrap();
    store
        .add_tags(Path::new("D:/docs/only-year.pdf"), &tags(&["2026"]))
        .unwrap();

    let all = store
        .search(&SearchRequest::new(["invoice", "2026"]).match_all())
        .unwrap();
    let any = store
        .search(&SearchRequest::new(["invoice", "2026"]))
        .unwrap();

    assert_eq!(all.len(), 1);
    assert_eq!(all[0].path, Path::new("D:/docs/both.pdf"));
    assert_eq!(any.len(), 3);
}

#[test]
fn directory_scope_excludes_shared_prefix_siblings() {
    let store = store();
    store
        .add_tags(Path::new("D:/FAM/Docs/deep/inside.pdf"), &tags(&["t"]))
        .unwrap();
    store
        .add_tags(Path::new("D:/FAMOther/outside.pdf"), &tags(&["t"]))
        .unwrap();
    store
        .add_tags(Path::new("D:/FAM.Docs/dot.pdf"), &tags(&["t"]))
        .unwrap();

    let hits = store
        .search(&SearchRequest::new(["t"]).within("D:/FAM"))
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, Path::new("D:/FAM/Docs/deep/inside.pdf"));
    assert_eq!(hits[0].tag_names(), vec!["t"]);

    let narrower = store
        .search(&SearchRequest::new(["t"]).within(r"D:\FAM\Docs"))
        .unwrap();
    assert_eq!(narrower.len(), 1);
}

#[test]
fn directory_scope_includes_the_directory_itself_when_tagged() {
    let store = store();
    let dir = Path::new("D:/FAM/Docs");
    store.add_tags(dir, &tags(&["t"])).unwrap();
    store
        .add_tags(Path::new("D:/FAM/Docs/a.txt"), &tags(&["t"]))
        .unwrap();

    let hits = store
        .search(&SearchRequest::new(["t"]).within(dir))
        .unwrap();

    assert_eq!(hits.len(), 2);
}

#[test]
fn search_pages_with_limit_and_offset() {
    let store = store();
    for index in 0..5 {
        let name = format!("D:/docs/file-{index}.pdf");
        store.add_tags(Path::new(&name), &tags(&["batch"])).unwrap();
    }

    let page = store
        .search(&SearchRequest {
            tags: vec!["batch".to_string()],
            limit: Some(2),
            offset: Some(2),
            ..Default::default()
        })
        .unwrap();

    let names: Vec<String> = page
        .iter()
        .map(|hit| hit.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["file-2.pdf", "file-3.pdf"]);
}

#[test]
fn source_filter_scopes_matching_not_display() {
    let store = store();
    let file = Path::new("D:/docs/dual.pdf");
    store
        .add_tags(file, &[NewTag::manual("same"), NewTag::auto("same")])
        .unwrap();

    let hits = store
        .search(&SearchRequest::new(["same"]).source(Source::Auto))
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].tags.len(), 2, "both sources stay visible on a hit");
}

#[test]
fn empty_and_invalid_searches_are_refused() {
    let store = store();
    let error = store.search(&SearchRequest::default()).unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidSearch(_)),
        "got {error:?}"
    );
    let bad_tag = store.search(&SearchRequest::new(["bad/tag"])).unwrap_err();
    assert!(matches!(bad_tag, StoreError::InvalidTag { .. }));
}

#[test]
fn removing_a_file_cascades_its_tags() {
    let store = store();
    let file = Path::new("D:/docs/gone.pdf");
    store.add_tags(file, &tags(&["a", "b"])).unwrap();

    assert_eq!(store.remove_file(file).unwrap(), 1);

    assert!(store.tags_of(file, None).unwrap().is_empty());
    let stats = store.stats().unwrap();
    assert_eq!(stats.files, 0);
    assert_eq!(stats.tags, 0);
}

#[test]
fn stats_split_tags_by_source() {
    let store = store();
    store
        .add_tags(
            Path::new("D:/docs/mixed.pdf"),
            &[
                NewTag::manual("m1"),
                NewTag::manual("m2"),
                NewTag::auto("a1"),
            ],
        )
        .unwrap();

    let stats = store.stats().unwrap();

    assert_eq!(
        stats,
        fam_svc_store::StoreStats {
            files: 1,
            tags: 3,
            manual: 2,
            auto: 1
        }
    );
}

#[test]
fn a_reopened_database_keeps_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("tags.db");
    let file = Path::new("D:/docs/persist.pdf");

    {
        let store = TagStore::open(&path).unwrap();
        store.add_tags(file, &tags(&["kept"])).unwrap();
    }
    let reopened = TagStore::open(&path).unwrap();

    assert_eq!(reopened.tags_of(file, None).unwrap().len(), 1);
    assert!(path.exists());
    assert!(dir.path().join("nested").exists());
}

#[test]
fn legacy_ai_source_spelling_still_reads_as_auto() {
    assert_eq!(Source::parse("AI").unwrap(), Source::Auto);
    let store = store();
    let file = Path::new("D:/docs/src.pdf");
    store.add_tags(file, &[NewTag::auto("generated")]).unwrap();
    let record = &store.tags_of(file, None).unwrap()[0];
    assert_eq!(record.source.as_str(), "auto");
}

#[test]
fn tag_names_for_returns_display_order_per_file() {
    let store = store();
    let first = Path::new("D:/docs/a.pdf");
    let second = Path::new("D:/docs/b.pdf");
    store
        .add_tags(
            first,
            &[
                NewTag::auto("auto-tag"),
                NewTag::manual("zed"),
                NewTag::manual("alpha"),
            ],
        )
        .unwrap();
    store.add_tags(second, &[NewTag::manual("solo")]).unwrap();

    let keys = vec![
        fam_core::path_key(first),
        fam_core::path_key(second),
        fam_core::path_key(Path::new("D:/docs/absent.pdf")),
    ];
    let grouped = store.tag_names_for(&keys).unwrap();

    assert_eq!(grouped[&keys[0]], vec!["alpha", "zed", "auto-tag"]);
    assert_eq!(grouped[&keys[1]], vec!["solo"]);
    assert!(!grouped.contains_key(&keys[2]));
    assert!(store.tag_names_for(&[]).unwrap().is_empty());
}

/// The shape a filter bar is drawn from: one entry per tag name, with how many files carry it.
///
/// Two things a bar gets visibly wrong, and this pins both. A name sitting on one file under both
/// sources counts as **one** file - the bar's number is a promise about what clicking it shows, and
/// `COUNT(*)` would promise two. And the order is by that count rather than by name, because the bar
/// has finite width and the tags nobody uses do not belong at its left end.
#[test]
fn vocabulary_counts_files_not_rows_and_orders_by_use() {
    let store = store();
    let both = Path::new("D:/docs/both.pdf");
    let one = Path::new("D:/docs/one.pdf");
    store
        .add_tags(
            both,
            &[NewTag::manual("contract"), NewTag::auto("contract")],
        )
        .unwrap();
    store
        .add_tags(one, &[NewTag::manual("contract"), NewTag::manual("rare")])
        .unwrap();

    let facets = store.vocabulary().unwrap();
    assert_eq!(
        facets
            .iter()
            .map(|facet| (facet.tag.as_str(), facet.files))
            .collect::<Vec<_>>(),
        vec![("contract", 2), ("rare", 1)],
        "a name on one file twice is still one file wearing one label"
    );

    // Removing the name from every file drops it from the vocabulary entirely - and note this call
    // takes no source, so it is the same across-the-board removal the shell's dialog warns about.
    store
        .remove_tags(both, &[String::from("contract")], None)
        .unwrap();
    store
        .remove_tags(one, &[String::from("contract")], None)
        .unwrap();
    let after = store.vocabulary().unwrap();
    let left: Vec<&str> = after.iter().map(|facet| facet.tag.as_str()).collect();
    assert_eq!(left, vec!["rare"]);
    // A store nobody has tagged answers with nothing, not with an error.
    assert!(TagStore::in_memory()
        .expect("in-memory store")
        .vocabulary()
        .unwrap()
        .is_empty());
}

/// The migration path a fresh in-memory database never takes: a v1 file on disk, with rows in it,
/// opened by a build that expects v2.
///
/// The v1 DDL is written out here rather than imported, on purpose - a test that builds the old
/// schema from the same constant the migration runs would pass even if the two drifted apart.
#[test]
fn a_v1_database_gains_the_notes_table_and_keeps_its_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("tags.db");
    {
        let old = rusqlite::Connection::open(&file).expect("create v1");
        old.execute_batch(
            r#"
CREATE TABLE files (
    path_norm  TEXT PRIMARY KEY,
    path       TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE tags (
    id         INTEGER PRIMARY KEY,
    path_norm  TEXT NOT NULL REFERENCES files(path_norm) ON DELETE CASCADE,
    tag        TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','auto')),
    priority   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (path_norm, tag, source)
);
CREATE INDEX tags_tag_source_idx ON tags(tag, source);
"#,
        )
        .expect("v1 schema");
        old.execute_batch(
            "INSERT INTO files (path_norm, path) VALUES ('D:/NOTES.OLD', 'D:/notes.old');
             INSERT INTO tags (path_norm, tag) VALUES ('D:/NOTES.OLD', 'kept');",
        )
        .expect("v1 rows");
        old.pragma_update(None, "user_version", 1_i64)
            .expect("stamp v1");
    }

    let store = TagStore::open(&file).expect("a v1 database opens");
    let tags = store
        .tags_of(Path::new("D:/notes.old"), None)
        .expect("the old tag is still there");
    assert_eq!(tags.len(), 1, "{:?}", tags);
    assert_eq!(tags[0].tag, "kept");

    store
        .set_note(Path::new("D:/notes.old"), "written after the upgrade")
        .expect("the new table is usable");
    assert_eq!(
        store
            .note(Path::new("D:/notes.old"))
            .expect("read")
            .expect("there is a note")
            .body,
        "written after the upgrade"
    );
    assert_eq!(
        store.stats().expect("stats").tags,
        1,
        "and nothing was duplicated"
    );

    /* A database from the future is refused rather than half-understood: opening it and quietly
     * ignoring the tables this build does not know about would let a downgrade delete them. */
    let newer = rusqlite::Connection::open(&file).expect("reopen");
    newer
        .pragma_update(None, "user_version", 99_i64)
        .expect("stamp ahead");
    drop(newer);
    match TagStore::open(&file) {
        Err(fam_svc_store::StoreError::SchemaTooNew(found)) => assert_eq!(found, 99),
        Err(other) => panic!("a v99 database must be refused as too new, got {other:?}"),
        Ok(_) => panic!("a v99 database opened; it should have been refused"),
    }
}

#[test]
fn a_merge_moves_the_name_and_collapses_where_the_target_already_sat() {
    let store = store();
    let both = Path::new("D:/Library/a.pdf");
    let other = Path::new("D:/Library/b.pdf");
    store
        .add_tags(both, &[NewTag::manual("pdf"), NewTag::manual("PDF")])
        .unwrap();
    store
        .add_tags(other, &[NewTag::manual("pdf"), NewTag::auto("PDF")])
        .unwrap();

    let outcome = store.merge_tags("pdf", "PDF").unwrap();

    // `a` already carried `PDF` from the same source, so its `pdf` row collapses into it; `b`'s
    // `pdf` truly moves, and lands beside the auto `PDF` - the two-sources-one-name shape the
    // shell's dialog has to draw, now produced by a real call rather than only by readers.
    assert_eq!(
        (outcome.moved, outcome.collapsed, outcome.files),
        (1, 1, 2),
        "{outcome:?}"
    );
    let on_a = store.tags_of(both, None).unwrap();
    assert_eq!(
        on_a.iter().map(|t| t.tag.as_str()).collect::<Vec<_>>(),
        ["PDF"]
    );
    let on_b = store.tags_of(other, None).unwrap();
    assert_eq!(
        on_b.iter()
            .map(|t| (t.tag.as_str(), t.source.as_str()))
            .collect::<Vec<_>>(),
        [("PDF", "manual"), ("PDF", "auto")]
    );
    assert!(store.vocabulary().unwrap().iter().all(|f| f.tag != "pdf"));
}

#[test]
fn a_merge_of_an_unknown_or_invalid_name_is_an_error_not_a_silent_nothing() {
    let store = store();
    let file = Path::new("D:/Library/a.pdf");
    store.add_tags(file, &[NewTag::manual("PDF")]).unwrap();

    let error = store.merge_tags("pdf ", "PDF").unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidTag { .. }),
        "the target is validated like any tag text: {error:?}"
    );
    let error = store.merge_tags("pdf", "PDF").unwrap_err();
    assert!(
        matches!(error, StoreError::TagNotFound(ref name) if name == "pdf"),
        "a typo merging onto a real name must not read as success: {error:?}"
    );
    let error = store.merge_tags("PDF", "PDF").unwrap_err();
    assert!(matches!(error, StoreError::NothingToChange), "{error:?}");
    assert_eq!(store.tags_of(file, None).unwrap().len(), 1, "nothing moved");
}

#[test]
fn a_prune_counts_first_and_deletes_only_when_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stays = dir.path().join("stays.txt");
    let gone = dir.path().join("gone.txt");
    std::fs::write(
        &stays, "live
",
    )
    .expect("the live file exists on disk");
    std::fs::write(
        &gone, "doomed
",
    )
    .expect("the doomed file exists on disk");
    let store = store();
    store.add_tags(&stays, &[NewTag::manual("kept")]).unwrap();
    store.set_note(&stays, "still here").unwrap();
    store.add_tags(&gone, &[NewTag::manual("lost")]).unwrap();
    store.set_note(&gone, "the note goes with it").unwrap();
    std::fs::remove_file(&gone).expect("the file goes");

    let dry = store.prune_missing(false).unwrap();
    assert_eq!((dry.files, dry.tags, dry.notes), (1, 1, 1), "{dry:?}");
    // A dry run counts and stops: the rows are all still there.
    assert_eq!(store.stats().unwrap().files, 2);

    let applied = store.prune_missing(true).unwrap();
    assert_eq!((applied.files, applied.tags, applied.notes), (1, 1, 1));
    assert_eq!(
        store.stats().unwrap().files,
        1,
        "only the live path remains"
    );
    assert_eq!(store.stats().unwrap().tags, 1);
    assert!(
        store.note(&gone).unwrap().is_none(),
        "the cascade took the note"
    );
    assert_eq!(
        store.tags_of(&stays, None).unwrap().len(),
        1,
        "and the live file keeps its tag and its note"
    );
    assert_eq!(
        store.note(&stays).unwrap().expect("note survives").body,
        "still here"
    );
    // A second run finds nothing and deletes nothing.
    let again = store.prune_missing(true).unwrap();
    assert_eq!((again.files, again.tags, again.notes), (0, 0, 0));
}

#[test]
fn a_v2_database_gains_the_covering_index_and_keeps_its_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("tags.db");
    {
        let store = TagStore::open(&file).expect("a v3 store opens");
        store
            .add_tags(Path::new("D:/Library/a.pdf"), &[NewTag::manual("kept")])
            .unwrap();
    }
    {
        // Rewound to v2 with the old index back in place - exactly what a database written by the
        // previous build looks like.
        let old = rusqlite::Connection::open(&file).expect("reopen as v2");
        old.execute_batch(
            "DROP INDEX IF EXISTS tags_tag_path_idx;
             CREATE INDEX tags_tag_source_idx ON tags(tag, source);",
        )
        .expect("v2 index shape");
        old.pragma_update(None, "user_version", 2_i64)
            .expect("stamp v2");
    }

    let store = TagStore::open(&file).expect("the migration runs on open");
    assert_eq!(
        store
            .tags_of(Path::new("D:/Library/a.pdf"), None)
            .unwrap()
            .len(),
        1,
        "the rows survive the re-index"
    );
    let (has_new, has_old) = {
        let conn = store.connection();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'tags'")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (
            names.iter().any(|name| name == "tags_tag_path_idx"),
            names.iter().any(|name| name == "tags_tag_source_idx"),
        )
    };
    assert!(has_new, "the covering index is created");
    assert!(!has_old, "the index nothing used is gone");
}

#[test]
fn the_tag_search_walks_the_covering_index_not_the_table() {
    let store = store();
    let file = Path::new("D:/Library/a.pdf");
    store.add_tags(file, &[NewTag::manual("contract")]).unwrap();

    let mut stmt = store
        .connection()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT f.path_norm, f.path FROM tags t \
                  JOIN files f ON f.path_norm = t.path_norm WHERE t.tag IN ('contract')",
        )
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let plan = plan.join("; ");
    assert!(
        plan.contains("tags_tag_path_idx"),
        "the search must walk the covering index: {plan}"
    );
}

#[test]
fn has_notes_answers_which_paths_carry_one_and_nothing_for_the_rest() {
    let store = store();
    let noted = Path::new("D:/Library/with.txt");
    let bare = Path::new("D:/Library/without.txt");
    store.set_note(noted, "here").unwrap();
    // A path that exists in `files` (a tag was set) but carries no note, and one the store has
    // never heard of: all three shapes in one ask.
    store.add_tags(bare, &[NewTag::manual("tag")]).unwrap();
    let stranger = Path::new("D:/Library/stranger.txt");

    let keys: Vec<String> = [noted, bare, stranger]
        .iter()
        .map(|p| fam_core::path_key(p))
        .collect();
    let found = store.has_notes(&keys).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found.contains(&fam_core::path_key(noted)));
    // Empty ask, empty answer - the shape a window with no rows asks.
    assert!(store.has_notes(&[]).unwrap().is_empty());
}
