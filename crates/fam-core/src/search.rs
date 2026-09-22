use std::io;
use std::path::PathBuf;

use crate::cancel::Cancellation;
use crate::kind::Kind;
use crate::pathkey::PathKey;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchRequest {
    /// Lower-cased; every term must appear in the candidate name.
    pub terms: Vec<String>,
    /// Directory subtree to confine the search to, spelled as the caller spelled it.
    ///
    /// Not a [`PathKey`], even though a key is the natural type for a directory: a provider that has
    /// to restart enumeration uses this as its root, and a root taken from a folded key reports the
    /// user's own folders back to them in lower case. Providers fold it themselves for the identity
    /// comparisons.
    pub scope: Option<PathBuf>,
    pub kind: Option<Kind>,
    pub limit: usize,
}

impl SearchRequest {
    pub fn new(text: &str) -> Self {
        Self {
            terms: text
                .split_whitespace()
                .map(str::to_lowercase)
                .filter(|term| !term.is_empty())
                .collect(),
            scope: None,
            kind: None,
            limit: 0,
        }
    }

    pub fn scope(mut self, scope: impl Into<PathBuf>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// `0` means unbounded.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// `name` must already be lower-cased.
    pub fn matches_name(&self, name: &str) -> bool {
        self.terms.iter().all(|term| name.contains(term))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Folded identity, for a caller that has to match the hit against the directory cache.
    pub key: PathKey,
    /// Verbatim UTF-16 path as the filesystem reported it, separators included.
    ///
    /// This is not a convenience on top of `key`: a `PathKey` folds ASCII case because two spellings
    /// of one name are one file, and a filename label drawn from the fold tells the user their own
    /// folders are spelled in lower case. The base name is the part after the last separator.
    pub path: Vec<u16>,
    pub kind: Kind,
    pub size: u64,
    pub mtime_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOutcome {
    pub matched: u64,
    /// Entries looked at, including ones the query filtered out; never conflate with `matched`.
    pub visited: u64,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("query carries no search terms")]
    Empty,
    #[error("provider {provider} is unavailable: {reason}")]
    Unavailable {
        provider: &'static str,
        reason: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A name-search backend that pushes results instead of returning a collection.
///
/// A contract whose answer *is* a `Vec` makes streaming impossible by construction: no backend could
/// hand a result to the caller before its whole search finished, however soon the first match was
/// known, and a search of `D:\` is not something to make a user wait out. Returning `false` from
/// `sink` stops the provider, and `limit` on the request bounds how much a provider may visit.
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_available(&self) -> bool;

    /// Whether this backend can answer **all** of `request`.
    ///
    /// A backend that reaches only part of a scope says no rather than answering what it can and
    /// calling that complete: a shorter list that looks exhaustive is worse than a slower one that is
    /// not. (Merging several partial answers is a different thing, and is not implemented anywhere
    /// yet - so there is no third state to name.)
    fn covers(&self, request: &SearchRequest) -> bool {
        let _ = request;
        true
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_split_and_lowercased() {
        let request = SearchRequest::new("  Q1  Report ");
        assert_eq!(request.terms, vec!["q1", "report"]);
        assert!(!request.is_empty());
        assert!(SearchRequest::new("   ").is_empty());
    }

    #[test]
    fn every_term_must_appear() {
        let request = SearchRequest::new("q1 report");
        assert!(request.matches_name("q1-report.pdf"));
        assert!(!request.matches_name("report.pdf"));
    }

    #[test]
    fn builders_compose() {
        let request = SearchRequest::new("a")
            .scope(PathBuf::from("D:/MixedCase"))
            .kind(Kind::Document)
            .limit(50);
        assert_eq!(request.limit, 50);
        assert_eq!(request.kind, Some(Kind::Document));
        assert_eq!(
            request.scope,
            Some(PathBuf::from("D:/MixedCase")),
            "the scope is kept as it was given, not folded"
        );
    }
}
