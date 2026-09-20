use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fam_core::path_key;
use rusqlite::ffi::ErrorCode;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, ToSql};

use crate::error::StoreError;
use crate::model::{
    descendant_range, validate_tag, FileTags, NewTag, SearchRequest, Source, TagRecord,
};

const SCHEMA_VERSION: i64 = 1;

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

pub struct TagStore {
    conn: Connection,
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
        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA_V1)?;
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
        let mut sql = String::from(
            "SELECT f.path_norm, f.path FROM tags t JOIN files f ON f.path_norm = t.path_norm \
             WHERE t.tag IN (",
        );
        let mut placeholders = Vec::with_capacity(request.tags.len());
        for tag in &request.tags {
            placeholders.push(format!("?{next}"));
            next += 1;
            args.push(Box::new(tag.clone()));
        }
        sql.push_str(&placeholders.join(","));
        sql.push(')');

        if let Some(source) = request.source {
            sql.push_str(&format!(" AND t.source = ?{next}"));
            args.push(Box::new(source.as_str().to_string()));
            next += 1;
        }
        if let Some(within) = &request.within {
            let key = path_key(within);
            if let Some((lower, upper)) = descendant_range(&key) {
                sql.push_str(&format!(
                    " AND (f.path_norm = ?{next} OR (f.path_norm >= ?{} AND f.path_norm < ?{}))",
                    next + 1,
                    next + 2
                ));
                args.push(Box::new(key));
                args.push(Box::new(lower));
                args.push(Box::new(upper));
                next += 3;
            }
        }
        sql.push_str(" GROUP BY t.path_norm");
        if request.match_all {
            sql.push_str(&format!(" HAVING COUNT(DISTINCT t.tag) = ?{next}"));
            args.push(Box::new(request.tags.len() as i64));
            next += 1;
        }
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
