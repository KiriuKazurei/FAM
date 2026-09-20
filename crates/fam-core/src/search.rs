use std::io;

use crate::cancel::Cancellation;
use crate::kind::Kind;
use crate::pathkey::PathKey;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchRequest {
    /// Lower-cased; every term must appear in the candidate name.
    pub terms: Vec<String>,
    /// Restrict results to this directory subtree.
    pub scope: Option<PathKey>,
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

    pub fn scope(mut self, scope: PathKey) -> Self {
        self.scope = Some(scope);
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
    pub key: PathKey,
    pub dir: PathKey,
    /// Verbatim UTF-16 name as the filesystem reported it.
    pub name: Vec<u16>,
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
/// The previous contract returned `SearchResults { entries: Vec<Entry> }`, which made streaming
/// impossible by construction: no provider could hand a result to the caller before the whole
/// search finished. Returning `false` from `sink` stops the provider, and `limit` on the request
/// bounds how much a provider may visit.
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_available(&self) -> bool;
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
            .scope(PathKey::of(std::path::Path::new("D:/x")))
            .kind(Kind::Document)
            .limit(50);
        assert_eq!(request.limit, 50);
        assert_eq!(request.kind, Some(Kind::Document));
        assert!(request.scope.is_some());
    }
}
