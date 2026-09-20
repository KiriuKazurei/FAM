use std::path::PathBuf;
use std::time::Duration;

use crate::kind::Kind;
use crate::walk::{walk, Cancellation, Entry, WalkPolicy, WalkReport};

pub const DEFAULT_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Lower-cased terms; every one must appear somewhere in the file name.
    pub terms: Vec<String>,
    pub within: Option<PathBuf>,
    pub kind: Option<Kind>,
    pub limit: usize,
}

impl SearchQuery {
    pub fn new(text: &str) -> Self {
        Self {
            terms: text
                .split_whitespace()
                .map(str::to_lowercase)
                .filter(|term| !term.is_empty())
                .collect(),
            within: None,
            kind: None,
            limit: DEFAULT_LIMIT,
        }
    }

    pub fn within(mut self, dir: impl Into<PathBuf>) -> Self {
        self.within = Some(dir.into());
        self
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    fn matches(&self, entry: &Entry) -> bool {
        if let Some(kind) = self.kind {
            if entry.kind != kind {
                return false;
            }
        }
        let name = entry.file_name().to_lowercase();
        self.terms.iter().all(|term| name.contains(term))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    pub backend: &'static str,
    pub entries: Vec<Entry>,
    pub report: WalkReport,
    /// True when the search stopped before covering the requested scope.
    pub incomplete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("query carries no search terms")]
    Empty,
    #[error("backend {backend} is unavailable: {reason}")]
    Unavailable { backend: &'static str, reason: String },
}

pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_available(&self) -> bool;
    fn search(
        &self,
        query: &SearchQuery,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> Result<SearchResults, SearchError>;
}

/// Always-available fallback: enumerate the configured roots and match names. Everything only
/// indexes local fixed NTFS volumes, so this path has to stay (`docs/legacy-specs.md` §2).
pub struct WalkBackend {
    policy: WalkPolicy,
}

impl WalkBackend {
    pub fn new(policy: WalkPolicy) -> Self {
        Self { policy }
    }
}

impl SearchBackend for WalkBackend {
    fn id(&self) -> &'static str {
        "walk"
    }

    /// Enumeration can always answer for an explicitly scoped query, so availability does not
    /// depend on configured roots; the scope check happens in `search`.
    fn is_available(&self) -> bool {
        true
    }

    fn search(
        &self,
        query: &SearchQuery,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> Result<SearchResults, SearchError> {
        if query.is_empty() {
            return Err(SearchError::Empty);
        }
        let mut policy = self.policy.clone();
        if let Some(within) = &query.within {
            policy.roots = vec![within.clone()];
        }
        if policy.roots.is_empty() {
            return Err(SearchError::Unavailable {
                backend: self.id(),
                reason: "no search_roots configured; pass --in <dir>".into(),
            });
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut at_limit = false;
        let report = walk(&policy, cancel, timeout, |entry| {
            if !query.matches(entry) {
                return true;
            }
            entries.push(entry.clone());
            if entries.len() >= query.limit {
                at_limit = true;
                return false;
            }
            true
        });
        Ok(SearchResults {
            backend: self.id(),
            entries,
            incomplete: report.stopped_early() && !at_limit,
            report,
        })
    }
}

/// Integration point for the Everything SDK. Left deliberately inert: the SDK client (shared-memory
/// request/response against the running Everything service) is not written yet, so callers must not
/// mistake a stub for results.
pub struct EverythingBackend;

impl SearchBackend for EverythingBackend {
    fn id(&self) -> &'static str {
        "everything"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn search(
        &self,
        _query: &SearchQuery,
        _cancel: &Cancellation,
        _timeout: Duration,
    ) -> Result<SearchResults, SearchError> {
        Err(SearchError::Unavailable {
            backend: self.id(),
            reason: "the Everything SDK client is not implemented".into(),
        })
    }
}

pub struct SearchService {
    backends: Vec<Box<dyn SearchBackend>>,
}

impl SearchService {
    /// Backends are tried in the given order, so prefer the index and keep enumeration last.
    pub fn new(backends: Vec<Box<dyn SearchBackend>>) -> Self {
        Self { backends }
    }

    pub fn walk_only(policy: WalkPolicy) -> Self {
        Self::new(vec![Box::new(WalkBackend::new(policy))])
    }

    pub fn default_with(policy: WalkPolicy) -> Self {
        Self::new(vec![
            Box::new(EverythingBackend),
            Box::new(WalkBackend::new(policy)),
        ])
    }

    pub fn backend_ids(&self) -> Vec<&'static str> {
        self.backends.iter().map(|backend| backend.id()).collect()
    }

    pub fn available_backends(&self) -> Vec<&'static str> {
        self.backends
            .iter()
            .filter(|backend| backend.is_available())
            .map(|backend| backend.id())
            .collect()
    }

    pub fn search(
        &self,
        query: &SearchQuery,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> Result<SearchResults, SearchError> {
        let mut last: Option<SearchError> = None;
        for backend in &self.backends {
            if !backend.is_available() {
                continue;
            }
            match backend.search(query, cancel, timeout) {
                Ok(results) => return Ok(results),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or(SearchError::Unavailable {
            backend: "none",
            reason: "no search backend is available".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_are_lowercased_and_split() {
        let query = SearchQuery::new("Contract  2026");
        assert_eq!(query.terms, vec!["contract", "2026"]);
        assert!(SearchQuery::new("   ").is_empty());
    }

    #[test]
    fn every_term_must_appear_in_the_name() {
        let query = SearchQuery::new("pdf invoice");
        let hit = |name: &str| {
            query.matches(&Entry {
                path: PathBuf::from(name),
                kind: Kind::Document,
                size: 1,
                modified_ms: None,
            })
        };
        assert!(hit("INVOICE-2026.PDF"));
        assert!(!hit("invoice-list.txt"));
    }

    #[test]
    fn kind_filter_narrows_results() {
        let query = SearchQuery::new("a").kind(Kind::Image);
        let image = Entry {
            path: PathBuf::from("a.png"),
            kind: Kind::Image,
            size: 1,
            modified_ms: None,
        };
        let text = Entry {
            path: PathBuf::from("a.txt"),
            kind: Kind::Document,
            size: 1,
            modified_ms: None,
        };
        assert!(query.matches(&image));
        assert!(!query.matches(&text));
    }

    #[test]
    fn limit_never_collapses_to_zero() {
        assert_eq!(SearchQuery::new("x").limit(0).limit, 1);
    }
}
