//! The note beside a path: one free-text body, not a tag.
//!
//! Notes live in their own table for a reason the plan spells out and this file enforces: a tag has
//! a vocabulary, a source and a priority, and `tag.list` is the list of names a person has chosen.
//! A note has none of those. Storing it in `tags` would put every note's text into the tag
//! vocabulary, which is the same mistake as putting a comment into a filename.
//!
//! What this deliberately is not: searchable. The note body never enters the directory index, so it
//! cannot appear in a listing or a search hit - the only way to read one back is `note_get` on the
//! exact path. That keeps "what did the agent see" answerable from the audit trail alone: a note is
//! never swept up by a search nobody asked for.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{params, params_from_iter, OptionalExtension};

use crate::error::StoreError;
use crate::store::TagStore;

/// The longest note body this store will hold. An agent composes these, so the limit is a wall
/// against a runaway write, not a design target.
pub const MAX_NOTE_BYTES: usize = 8 * 1024;

/// One note as stored, with the path it annotates and the database's own clock for when it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRecord {
    pub path: String,
    pub body: String,
    pub updated_at: String,
}

const SELECT_NOTE: &str = "SELECT n.body, n.updated_at, f.path FROM notes n JOIN files f \
                           ON f.path_norm = n.path_norm WHERE n.path_norm = ?1";

impl TagStore {
    /// Writes the note on `path`, replacing whatever was there.
    ///
    /// The `files` row is created first because the note's foreign key needs it, and an empty body
    /// is refused rather than treated as a clear: the caller has a `clear_note` for that, and a tool
    /// that deletes when it was handed nothing is a tool that loses a note over one dropped
    /// argument.
    pub fn set_note(&self, path: &Path, body: &str) -> Result<(), StoreError> {
        let bytes = body.len();
        if bytes > MAX_NOTE_BYTES {
            return Err(StoreError::NoteTooLong {
                path: path.to_path_buf(),
                bytes,
                limit: MAX_NOTE_BYTES,
            });
        }
        if body.trim().is_empty() {
            return Err(StoreError::EmptyNote {
                path: path.to_path_buf(),
            });
        }
        self.record_file(path)?;
        self.connection().execute(
            "INSERT INTO notes (path_norm, body, updated_at) VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(path_norm) DO UPDATE SET body = excluded.body,
                                                   updated_at = excluded.updated_at",
            params![fam_core::path_key(path), body],
        )?;
        Ok(())
    }

    /// The note on `path`, when there is one.
    pub fn note(&self, path: &Path) -> Result<Option<NoteRecord>, StoreError> {
        let mut statement = self.connection().prepare(SELECT_NOTE)?;
        let found = statement
            .query_row([fam_core::path_key(path).as_str()], |row| {
                Ok(NoteRecord {
                    body: row.get(0)?,
                    updated_at: row.get(1)?,
                    path: row.get(2)?,
                })
            })
            .optional()?;
        Ok(found)
    }

    /// Which of these keys carry a note - the batch read a listing's "has a note" marker is drawn
    /// from. Keyed by the caller's folding (`path_norm`), like `tag_names_for`; a path without a
    /// note is absent rather than present with a false, so the answer's size is the number of
    /// marked rows and nothing else.
    pub fn has_notes(&self, keys: &[String]) -> Result<HashSet<String>, StoreError> {
        if keys.is_empty() {
            return Ok(HashSet::new());
        }
        let placeholders = vec!["?"; keys.len()].join(",");
        let sql = format!("SELECT path_norm FROM notes WHERE path_norm IN ({placeholders})");
        let mut statement = self.connection().prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(keys.iter().map(String::as_str)), |row| {
                row.get::<_, String>(0)
            })?;
        Ok(rows.collect::<Result<HashSet<_>, _>>()?)
    }

    /// Drops the note on `path`. True when there was one to drop, so a caller can tell "cleared"
    /// from "there was nothing here" instead of reporting both as success.
    pub fn clear_note(&self, path: &Path) -> Result<bool, StoreError> {
        let removed = self.connection().execute(
            "DELETE FROM notes WHERE path_norm = ?1",
            params![fam_core::path_key(path)],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewTag, Source};
    use std::path::PathBuf;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("D:/notes/{name}"))
    }

    #[test]
    fn a_note_round_trips_and_reports_when_it_changed() {
        let store = TagStore::in_memory().expect("store");
        let target = path("todo.md");
        assert!(store.note(&target).expect("read").is_none(), "nothing yet");
        store.set_note(&target, "first line").expect("write");
        let once = store.note(&target).expect("read").expect("there is a note");
        assert_eq!(once.body, "first line");
        assert_eq!(once.path, target.display().to_string());

        store.set_note(&target, "second line").expect("rewrite");
        let twice = store.note(&target).expect("read").expect("still there");
        assert_eq!(twice.body, "second line");
        assert!(store.clear_note(&target).expect("clear"));
        assert!(
            !store.clear_note(&target).expect("clear again"),
            "the second one had nothing to drop, and says so"
        );
    }

    /// The point of a separate table: a note is not a tag, and must not turn up where tags are
    /// listed or counted.
    #[test]
    fn a_note_is_not_a_tag_and_not_in_the_tag_count() {
        let store = TagStore::in_memory().expect("store");
        let target = path("photo.jpg");
        store
            .add_tags(
                &target,
                &[NewTag {
                    tag: "keep".into(),
                    source: Source::Manual,
                    priority: 0,
                }],
            )
            .expect("tagged");
        store.set_note(&target, "keep").expect("noted");

        let tags = store.tags_of(&target, None).expect("tags");
        assert_eq!(tags.len(), 1, "only the real tag");
        assert_eq!(tags[0].tag, "keep");
        let stats = store.stats().expect("stats");
        assert_eq!(stats.tags, 1, "the note did not add a tag row");
        assert_eq!(stats.files, 1, "and did not add a file row either");
    }

    #[test]
    fn a_note_dies_with_the_path_it_annotates() {
        let store = TagStore::in_memory().expect("store");
        let target = path("gone.txt");
        store
            .set_note(&target, "about to be removed")
            .expect("write");
        store.remove_file(&target).expect("remove");
        assert!(store.note(&target).expect("read").is_none());
    }

    #[test]
    fn an_over_long_or_blank_body_is_refused_before_anything_is_written() {
        let store = TagStore::in_memory().expect("store");
        let target = path("big.md");
        let error = store
            .set_note(&target, &"字".repeat(MAX_NOTE_BYTES))
            .expect_err("too long");
        assert!(
            matches!(error, StoreError::NoteTooLong { bytes, limit, .. }
                if bytes == MAX_NOTE_BYTES * 3 && limit == MAX_NOTE_BYTES),
            "{error:?}"
        );
        assert!(
            store.note(&target).expect("read").is_none(),
            "nothing written"
        );

        for blank in ["", "   ", "\n\t"] {
            let error = store
                .set_note(&target, blank)
                .expect_err("a blank body is not a note");
            assert!(matches!(error, StoreError::EmptyNote { .. }), "{error:?}");
        }
        assert!(store.note(&target).expect("read").is_none());
        assert_eq!(
            store.stats().expect("stats").files,
            0,
            "and no orphan file row was created by the refused writes"
        );
    }
}
