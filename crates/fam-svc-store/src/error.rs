use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid tag {tag:?}: {reason}")]
    InvalidTag { tag: String, reason: &'static str },
    #[error("no tag named {tag:?} on {}", .path.display())]
    NotFound { path: PathBuf, tag: String },
    #[error("{} already has a tag named {tag:?}", .path.display())]
    Conflict { path: PathBuf, tag: String },
    #[error("unknown tag source {0:?}; expected manual or auto")]
    BadSource(String),
    #[error("database schema version {0} is newer than this build supports")]
    SchemaTooNew(i64),
    #[error("invalid search: {0}")]
    InvalidSearch(String),
    #[error("nothing to change")]
    NothingToChange,
}
