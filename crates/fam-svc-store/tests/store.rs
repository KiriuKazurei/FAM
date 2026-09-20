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
