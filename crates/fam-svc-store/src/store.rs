use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fam_core::path_key;
use rusqlite::ffi::ErrorCode;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, ToSql};

use crate::error::StoreError;
use crate::model::{
    descendant_range, validate_tag, FileTags, NewTag, SearchRequest, Source, TagFacet, TagRecord,
};

/// The schema this build writes and expects. Public because a caller that reports store health
/// (`fam stats`, `whoami`) should quote the same number the migration ran to, not its own guess.
pub const SCHEMA_VERSION: i64 = 3;

/// Rows returned by a search that does not ask for a page.
pub const DEFAULT_SEARCH_LIMIT: i64 = 500;

const ORDER_TAGS: &str = " ORDER BY path_norm, CASE source WHEN 'manual' THEN 0 ELSE 1 END, \
                          priority DESC, tag COLLATE NOCASE";

const UPSERT_FILE: &str = "INSERT INTO files (path_norm, path) VALUES (?1, ?2)
     ON CONFLICT(path_norm) DO UPDATE SET path = excluded.path,
                                           updated_at = datetime('now')";

const SCHEMA_V1: &str = r#"
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
"#;

/// Version 2 adds the notes table.
///
/// A note is deliberately **not** a tag and deliberately **not** in the index:
///   * it is one free-text body per path, so it has no vocabulary, no priority, no source, and no
///     place in `tags` - mixing them would make `tag.list` answer a question nobody asked;
///   * it is not in the directory index, so it cannot leak into a listing or a search result and the
///     agent-facing tools are the only way to read it back;
///   * `REFERENCES files ON DELETE CASCADE` means a note dies with the row it annotates, which is
///     the only way to stop a database full of notes about paths that no longer exist.
///
/// `IF NOT EXISTS` because a brand-new database runs v1 and v2 in one go, and a v1 database has
/// already had v1 applied - the migration below is a ladder, not a rebuild.
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
    path_norm  TEXT PRIMARY KEY REFERENCES files(path_norm) ON DELETE CASCADE,
    body       TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Version 3 reshapes the one tag-name index, and adds nothing else.
///
/// v1's `tags_tag_source_idx (tag, source)` answered a question nothing asks: `source` is only ever
/// filtered *under* a `path_norm` (the primary key's own prefix), never across the library. The two
/// reads that go out by tag name - a `tag:` search and the vocabulary count - both want the row's
/// `path_norm` after the name, and with `(tag, source)` each hit cost a second hop into the table.
/// `(tag, path_norm)` covers both: the search's `WHERE tag IN (...)` and the vocabulary's
/// `GROUP BY tag` walk the index, and the join to `files` starts from its own primary key. The
/// before/after numbers are in the increment 17d account - recorded on the same machine and build,
/// both schemas fed by the same bench cases.
///
/// `DROP INDEX` because an index that no read uses is pure write cost - the kind of thing that
/// survives because deleting it needs a migration and keeping it needs nothing.
const SCHEMA_V3: &str = r#"
CREATE INDEX IF NOT EXISTS tags_tag_path_idx ON tags(tag, path_norm);
DROP INDEX IF EXISTS tags_tag_source_idx;
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOutcome {
    pub inserted: usize,
    pub refreshed: usize,
}

impl AddOutcome {
    pub fn total(&self) -> usize {
        self.inserted + self.refreshed
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub files: i64,
    pub tags: i64,
    pub manual: i64,
    pub auto: i64,
}

/// What one vocabulary-wide merge did. `moved` rows truly changed their name; `collapsed` rows died
/// because the file already carried the target name from the same source; `files` is how many files
/// carry the target name after the merge. `moved + collapsed` is every row the source name had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeOutcome {
    pub moved: usize,
    pub collapsed: usize,
    pub files: usize,
}

/// What one prune found. Counted before anything is deleted, so a dry run and an applied run report
/// the same shape - and the counts are what `fam store prune` prints either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    pub files: usize,
    pub tags: usize,
    pub notes: usize,
}

pub struct TagStore {
    conn: Connection,
}

/// Builds the tag-search query: required names (AND), OR groups (each must contribute at least
/// one carried name), excluded names (a file carrying any of them is out), source, scope. The
/// exclusion filters the *file* set - exclusion is a property of the file, not of a tag row -
/// which is also why it is `NOT IN` over a subquery rather than a join condition.
fn build_tag_query(
    request: &SearchRequest,
    next: &mut usize,
    args: &mut Vec<Box<dyn ToSql>>,
) -> String {
    /* The wanted names: everything the request could match on - required names and every member
     * of every OR group. The join restricts tag rows to these; the HAVING clauses then decide
     * which of the joined files actually qualify. */
    let mut wanted: Vec<String> = request.tags.clone();
    for group in &request.groups {
        wanted.extend(group.iter().cloned());
    }
    let mut sql = String::from(
        "SELECT f.path_norm, f.path FROM tags t JOIN files f ON f.path_norm = t.path_norm",
    );
    let mut clauses: Vec<String> = Vec::new();
    if !wanted.is_empty() {
        let mut placeholders = Vec::with_capacity(wanted.len());
        for tag in &wanted {
            placeholders.push(format!("?{next}"));
            *next += 1;
            args.push(Box::new(tag.clone()));
        }
        clauses.push(format!("t.tag IN ({})", placeholders.join(",")));
    }
    if let Some(source) = request.source {
        clauses.push(format!("t.source = ?{next}"));
        *next += 1;
        args.push(Box::new(source.as_str().to_string()));
    }
    if let Some(within) = &request.within {
        let key = path_key(within);
        if let Some((lower, upper)) = descendant_range(&key) {
            clauses.push(format!(
                "(f.path_norm = ?{next} OR (f.path_norm >= ?{} AND f.path_norm < ?{}))",
                *next + 1,
                *next + 2
            ));
            args.push(Box::new(key));
            args.push(Box::new(lower));
            args.push(Box::new(upper));
            *next += 3;
        }
    }
    if !request.tags_not.is_empty() {
        let mut not_placeholders = Vec::with_capacity(request.tags_not.len());
        for tag in &request.tags_not {
            not_placeholders.push(format!("?{next}"));
            *next += 1;
            args.push(Box::new(tag.clone()));
        }
        clauses.push(format!(
            "f.path_norm NOT IN (SELECT path_norm FROM tags WHERE tag IN ({}))",
            not_placeholders.join(",")
        ));
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" GROUP BY t.path_norm");
    let mut having: Vec<String> = Vec::new();
    if !request.tags.is_empty() {
        /* Required names: the distinct count over just those rows must reach the whole list -
         * the AND of the request. With groups present this clause and the per-group ones are
         * ANDed: required names all carried, and every group satisfied. */
        let mut req_placeholders = Vec::with_capacity(request.tags.len());
        for tag in &request.tags {
            req_placeholders.push(format!("?{next}"));
            *next += 1;
            args.push(Box::new(tag.clone()));
        }
        having.push(format!(
            "COUNT(DISTINCT CASE WHEN t.tag IN ({}) THEN t.tag END) = ?{next}",
            req_placeholders.join(",")
        ));
        args.push(Box::new(request.tags.len() as i64));
        *next += 1;
    }
    for group in &request.groups {
        let mut group_placeholders = Vec::with_capacity(group.len());
        for tag in group {
            group_placeholders.push(format!("?{next}"));
            *next += 1;
            args.push(Box::new(tag.clone()));
        }
        having.push(format!(
            "SUM(CASE WHEN t.tag IN ({}) THEN 1 ELSE 0 END) >= 1",
            group_placeholders.join(",")
        ));
    }
    if !having.is_empty() {
        sql.push_str(" HAVING ");
        sql.push_str(&having.join(" AND "));
    }
    sql
}

impl TagStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        Self::bootstrap(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::bootstrap(Connection::open_in_memory()?)
    }

    fn bootstrap(conn: Connection) -> Result<Self, StoreError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew(version));
        }
        /* A ladder, not a rebuild: v1's DDL cannot be replayed over a database that already has it
         * (`CREATE TABLE files` would fail), so each step runs only for a database below it. */
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            conn.execute_batch(SCHEMA_V3)?;
        }
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Attaches tags to a file. Validation runs before any write so one bad tag aborts the whole
    /// call (§1.2), and repeated calls are idempotent (§1.3-1).
    pub fn add_tags(&self, path: &Path, tags: &[NewTag]) -> Result<AddOutcome, StoreError> {
        for tag in tags {
            tag.validate()?;
        }
        let key = path_key(path);
        let display = path.display().to_string();
        let tx = self.conn.unchecked_transaction()?;

        let mut known: HashSet<(String, String)> = {
            let mut stmt = tx.prepare("SELECT tag, source FROM tags WHERE path_norm = ?1")?;
            let rows = stmt.query_map(params![key], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<HashSet<_>, _>>()?
        };

        tx.execute(UPSERT_FILE, params![key, display])?;

        let mut outcome = AddOutcome {
            inserted: 0,
            refreshed: 0,
        };
        for tag in tags {
            let identity = (tag.tag.clone(), tag.source.as_str().to_string());
            if known.insert(identity) {
                outcome.inserted += 1;
            } else {
                outcome.refreshed += 1;
            }
            tx.execute(
                "INSERT INTO tags (path_norm, tag, source, priority) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path_norm, tag, source) DO UPDATE SET priority = excluded.priority",
                params![key, tag.tag, tag.source.as_str(), tag.priority],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Empty `tags` removes every tag on the file; `source: None` spans both sources (§1.2).
    pub fn remove_tags(
        &self,
        path: &Path,
        tags: &[String],
        source: Option<Source>,
    ) -> Result<usize, StoreError> {
        let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(path_key(path))];
        let mut next = 2usize;
        let mut sql = String::from("DELETE FROM tags WHERE path_norm = ?1");
        if let Some(source) = source {
            sql.push_str(&format!(" AND source = ?{next}"));
            args.push(Box::new(source.as_str().to_string()));
            next += 1;
        }
        if !tags.is_empty() {
            let mut placeholders = Vec::with_capacity(tags.len());
            for tag in tags {
                placeholders.push(format!("?{next}"));
                next += 1;
                args.push(Box::new(tag.clone()));
            }
            sql.push_str(&format!(" AND tag IN ({})", placeholders.join(",")));
        }
        Ok(self.conn.execute(&sql, params_from_iter(sql_refs(&args)))?)
    }

    /// Renames a label **across the library**: every row named `from` - both sources, every file -
    /// ends up named `into`, and where a file already carried `into` from the same source the two
    /// rows collapse into the one that was already there (its priority stands; the moved row is
    /// dropped rather than doubled). With no `into` in the library this is just a rename; with one,
    /// it is the merge that consolidates a duplicate - `pdf` into `PDF`, `合同2024` into `合同`.
    /// The two names are validated like any other tag text, and a name that does not exist is an
    /// error rather than a silent nothing: a merge of a typo onto a real name is exactly the call
    /// where "0 rows affected" would read as success.
    pub fn merge_tags(&self, from: &str, into: &str) -> Result<MergeOutcome, StoreError> {
        validate_tag(from)?;
        validate_tag(into)?;
        if from == into {
            return Err(StoreError::NothingToChange);
        }
        let tx = self.conn.unchecked_transaction()?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tags WHERE tag = ?1",
            params![from],
            |row| row.get(0),
        )?;
        if existing == 0 {
            return Err(StoreError::TagNotFound(from.to_string()));
        }
        // The update skips the identities the target already holds - `OR IGNORE` over the
        // `(path_norm, tag, source)` unique - and the delete reaps exactly those, so `collapsed`
        // is the number of would-be duplicates and `moved` the number of rows that truly moved.
        let moved = tx.execute(
            "UPDATE OR IGNORE tags SET tag = ?1 WHERE tag = ?2",
            params![into, from],
        )?;
        let collapsed = tx.execute("DELETE FROM tags WHERE tag = ?1", params![from])?;
        let files: i64 = tx.query_row(
            "SELECT COUNT(DISTINCT path_norm) FROM tags WHERE tag = ?1",
            params![into],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(MergeOutcome {
            moved,
            collapsed,
            files: files as usize,
        })
    }

    /// Counts (when `apply` is false) or deletes (when true) every `files` row whose path the
    /// filesystem no longer has. The rows it takes with it are the schema's own decision, not this
    /// function's: `ON DELETE CASCADE` carries the tags and the note, because a tag on a path
    /// nothing can stat is a row no search can ever serve - the search backend skips it, and the
    /// `files` row is what made the library grow with every path that was ever labelled
    /// (measured in the increment 16 account).
    ///
    /// Deliberately a command, not a habit: a network drive that is merely *offline* looks exactly
    /// like one whose files are gone, so nothing here runs on open or on a timer. The caller - the
    /// `fam store prune` command - prints what a dry run found and asks before applying.
    pub fn prune_missing(&self, apply: bool) -> Result<PruneOutcome, StoreError> {
        let paths: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT path_norm, path FROM files")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let gone: Vec<String> = paths
            .into_iter()
            .filter(|(_, path)| fs::metadata(path).is_err())
            .map(|(key, _)| key)
            .collect();

        let mut outcome = PruneOutcome {
            files: gone.len(),
            ..PruneOutcome::default()
        };
        for chunk in gone.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let args: Vec<&str> = chunk.iter().map(String::as_str).collect();
            {
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT COUNT(*) FROM tags WHERE path_norm IN ({placeholders})"
                ))?;
                outcome.tags += stmt
                    .query_row(params_from_iter(args.iter()), |row| row.get::<_, i64>(0))?
                    as usize;
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT COUNT(*) FROM notes WHERE path_norm IN ({placeholders})"
                ))?;
                outcome.notes += stmt
                    .query_row(params_from_iter(args.iter()), |row| row.get::<_, i64>(0))?
                    as usize;
            }
            if apply {
                let sql = format!("DELETE FROM files WHERE path_norm IN ({placeholders})");
                self.conn.execute(&sql, params_from_iter(args.iter()))?;
            }
        }
        Ok(outcome)
    }

    pub fn tags_of(
        &self,
        path: &Path,
        source: Option<Source>,
    ) -> Result<Vec<TagRecord>, StoreError> {
        let key = path_key(path);
        let sql = format!(
            "SELECT tag, source, priority, created_at FROM tags WHERE path_norm = ?1{}{}",
            if source.is_some() {
                " AND source = ?2"
            } else {
                ""
            },
            ORDER_TAGS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match source {
            Some(source) => stmt.query_map(params![key, source.as_str()], read_tag_record)?,
            None => stmt.query_map(params![key], read_tag_record)?,
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `None` for `to` or `source` leaves that field untouched, matching §1.2's "empty means
    /// unchanged". A miss is an error, which the legacy code also treated as a rollback condition.
    pub fn rename_tag(
        &self,
        path: &Path,
        from: &str,
        to: Option<&str>,
        source: Option<Source>,
    ) -> Result<(), StoreError> {
        if to.is_none() && source.is_none() {
            return Err(StoreError::NothingToChange);
        }
        validate_tag(from)?;
        if let Some(to) = to {
            validate_tag(to)?;
        }
        let affected = self
            .conn
            .execute(
                "UPDATE tags SET tag = COALESCE(?2, tag), source = COALESCE(?3, source)
                 WHERE path_norm = ?1 AND tag = ?4",
                params![path_key(path), to, source.map(|s| s.as_str()), from],
            )
            .map_err(|error| unique_conflict(error, path, to.unwrap_or(from)))?;
        if affected == 0 {
            return Err(StoreError::NotFound {
                path: path.to_path_buf(),
                tag: from.to_string(),
            });
        }
        Ok(())
    }

    pub fn set_priority(
        &self,
        path: &Path,
        tag: &str,
        source: Source,
        priority: i32,
    ) -> Result<(), StoreError> {
        validate_tag(tag)?;
        let affected = self.conn.execute(
            "UPDATE tags SET priority = ?3 WHERE path_norm = ?1 AND tag = ?2 AND source = ?4",
            params![path_key(path), tag, priority, source.as_str()],
        )?;
        if affected == 0 {
            return Err(StoreError::NotFound {
                path: path.to_path_buf(),
                tag: tag.to_string(),
            });
        }
        Ok(())
    }

    /// Directory scoping is pushed into SQL as an indexed range over `path_norm`; the legacy
    /// version fetched whole rows and filtered them with `strings.HasPrefix` in Go (§1.3-2).
    pub fn search(&self, request: &SearchRequest) -> Result<Vec<FileTags>, StoreError> {
        request.validate()?;
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        let mut next = 1usize;
        let mut sql = build_tag_query(request, &mut next, &mut args);
        sql.push_str(" ORDER BY f.path");
        let limit = request.limit.unwrap_or(DEFAULT_SEARCH_LIMIT as usize) as i64;
        sql.push_str(&format!(" LIMIT ?{next}"));
        args.push(Box::new(limit));
        next += 1;
        if let Some(offset) = request.offset {
            sql.push_str(&format!(" OFFSET ?{next}"));
            args.push(Box::new(offset as i64));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let hits = stmt
            .query_map(params_from_iter(sql_refs(&args)), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        self.load_grouped(hits)
    }

    /// Tag names for many files in one query, keyed by `path_norm`, in display order.
    pub fn tag_names_for(
        &self,
        keys: &[String],
    ) -> Result<HashMap<String, Vec<String>>, StoreError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = vec!["?"; keys.len()].join(",");
        let sql = format!(
            "SELECT path_norm, tag FROM tags WHERE path_norm IN ({}){}",
            placeholders, ORDER_TAGS
        );
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(keys.iter().map(String::as_str)), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
            for row in rows {
                let (key, tag) = row?;
                grouped.entry(key).or_default().push(tag);
            }
        }
        Ok(grouped)
    }

    /// Every tag name in the store with the number of files carrying it, most used first.
    ///
    /// `COUNT(DISTINCT path_norm)` and not `COUNT(*)` because the same name can sit on one file twice
    /// - once `manual`, once `auto` - and a bar that said "3" for two files would be describing rows,
    ///   not the thing a person is about to filter.
    ///
    /// Unbounded by design: a vocabulary is small, and a cap here would silently hide a tag the
    /// person can see on a row, which is worse than a long list.
    /// The vocabulary as seen from one directory: tags whose files live in the subtree, counted
    /// over the files **in** the subtree. A browsing page that shows "the tags of where I am"
    /// asks this; the library-wide [`TagStore::vocabulary`] answers a different question.
    pub fn vocabulary_within(&self, within: &Path) -> Result<Vec<TagFacet>, StoreError> {
        let key = path_key(within);
        let mut sql = String::from("SELECT t.tag, COUNT(DISTINCT t.path_norm) FROM tags t WHERE ");
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        match descendant_range(&key) {
            Some((lower, upper)) => {
                sql.push_str("(t.path_norm = ?1 OR (t.path_norm >= ?2 AND t.path_norm < ?3))");
                args.push(Box::new(key.clone()));
                args.push(Box::new(lower));
                args.push(Box::new(upper));
            }
            None => {
                sql.push_str("t.path_norm = ?1");
                args.push(Box::new(key));
            }
        }
        sql.push_str(
            " GROUP BY t.tag ORDER BY COUNT(DISTINCT t.path_norm) DESC, t.tag COLLATE NOCASE",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(sql_refs(&args)), |row| {
            Ok(TagFacet {
                tag: row.get(0)?,
                files: row.get::<_, i64>(1)? as usize,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Removes every row named `tag` across all files and both sources - the tag manager's
    /// delete. The name is validated like any tag text (the manager cannot delete a malformed
    /// name that the store would never have accepted), and the answer counts the rows that went,
    /// both sources separately per the wire's own row semantics. Zero means the name was not in
    /// the library, which the caller reports rather than celebrating.
    pub fn delete_tags(&self, tag: &str) -> Result<usize, StoreError> {
        validate_tag(tag)?;
        Ok(self
            .conn
            .execute("DELETE FROM tags WHERE tag = ?1", params![tag])?)
    }

    pub fn vocabulary(&self) -> Result<Vec<TagFacet>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT tag, COUNT(DISTINCT path_norm) FROM tags GROUP BY tag \
             ORDER BY COUNT(DISTINCT path_norm) DESC, tag COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TagFacet {
                tag: row.get(0)?,
                files: row.get::<_, i64>(1)? as usize,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn record_file(&self, path: &Path) -> Result<(), StoreError> {
        self.conn.execute(
            UPSERT_FILE,
            params![path_key(path), path.display().to_string()],
        )?;
        Ok(())
    }

    pub fn has_file(&self, path: &Path) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM files WHERE path_norm = ?1",
                params![path_key(path)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Drops the file row; its tags follow through `ON DELETE CASCADE`.
    pub fn remove_file(&self, path: &Path) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            "DELETE FROM files WHERE path_norm = ?1",
            params![path_key(path)],
        )?)
    }

    pub fn stats(&self) -> Result<StoreStats, StoreError> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let tags: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))?;
        let mut stats = StoreStats {
            files,
            tags,
            ..Default::default()
        };
        let mut stmt = self
            .conn
            .prepare("SELECT source, COUNT(*) FROM tags GROUP BY source")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (source, count) = row?;
            match parse_source(&source) {
                Source::Manual => stats.manual = count,
                Source::Auto => stats.auto = count,
            }
        }
        Ok(stats)
    }

    fn load_grouped(&self, hits: Vec<(String, String)>) -> Result<Vec<FileTags>, StoreError> {
        let mut grouped: HashMap<String, Vec<TagRecord>> = HashMap::new();
        if !hits.is_empty() {
            let placeholders = vec!["?"; hits.len()].join(",");
            let sql = format!(
                "SELECT path_norm, tag, source, priority, created_at FROM tags \
                 WHERE path_norm IN ({}){}",
                placeholders, ORDER_TAGS
            );
            let keys: Vec<&str> = hits.iter().map(|(key, _)| key.as_str()).collect();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(params_from_iter(keys.iter().copied()), read_grouped_record)?;
            for row in rows {
                let (key, record) = row?;
                grouped.entry(key).or_default().push(record);
            }
        }
        Ok(hits
            .into_iter()
            .map(|(key, path)| FileTags {
                path: PathBuf::from(path),
                tags: grouped.remove(&key).unwrap_or_default(),
            })
            .collect())
    }
}

fn sql_refs(args: &[Box<dyn ToSql>]) -> Vec<&dyn ToSql> {
    args.iter().map(|arg| &**arg).collect()
}

fn read_tag_record(row: &Row<'_>) -> Result<TagRecord, rusqlite::Error> {
    Ok(TagRecord {
        tag: row.get(0)?,
        source: parse_source(&row.get::<_, String>(1)?),
        priority: row.get(2)?,
        created_at: row.get(3)?,
    })
}

fn read_grouped_record(row: &Row<'_>) -> Result<(String, TagRecord), rusqlite::Error> {
    Ok((
        row.get(0)?,
        TagRecord {
            tag: row.get(1)?,
            source: parse_source(&row.get::<_, String>(2)?),
            priority: row.get(3)?,
            created_at: row.get(4)?,
        },
    ))
}

fn parse_source(raw: &str) -> Source {
    match raw {
        "manual" => Source::Manual,
        _ => Source::Auto,
    }
}

fn unique_conflict(error: rusqlite::Error, path: &Path, tag: &str) -> StoreError {
    let constraint_violation = error
        .sqlite_error()
        .is_some_and(|ffi_error| ffi_error.code == ErrorCode::ConstraintViolation);
    if constraint_violation {
        StoreError::Conflict {
            path: path.to_path_buf(),
            tag: tag.to_string(),
        }
    } else {
        StoreError::Sql(error)
    }
}
