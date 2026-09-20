use std::path::PathBuf;
use std::time::Duration;

use fam_core::{
    raw_name_units, Cancellation, Kind, PathKey, SearchError as ProviderError, SearchHit,
    SearchOutcome, SearchProvider, SearchRequest,
};

use crate::walk::{walk, Entry, WalkPolicy, WalkReport};

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
    Unavailable {
        backend: &'static str,
        reason: String,
    },
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

/// The recursive-walk provider behind the new sink-based contract.
///
/// The `SearchBackend`/`SearchQuery`/`SearchResults` types above are the transitional v1 surface
/// still used by `fam-ffi` and `fam-cli`; they are deleted in increment 6 once the Everything
/// client and the index router take over. This provider is the shape both will implement.
pub struct WalkProvider {
    policy: WalkPolicy,
    timeout: Duration,
}

impl WalkProvider {
    pub fn new(policy: WalkPolicy, timeout: Duration) -> Self {
        Self { policy, timeout }
    }
}

impl SearchProvider for WalkProvider {
    fn id(&self) -> &'static str {
        "walk"
    }

    /// Enumeration is always possible, so availability is not scope-dependent; a request without a
    /// usable scope reports `Unavailable` from `search` instead.
    fn is_available(&self) -> bool {
        true
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, ProviderError> {
        if request.is_empty() {
            return Err(ProviderError::Empty);
        }
        let mut policy = self.policy.clone();
        if let Some(scope) = &request.scope {
            policy.roots = vec![scope.to_path_buf()];
        }
        if policy.roots.is_empty() {
            return Err(ProviderError::Unavailable {
                provider: self.id(),
                reason: "no search roots configured; pass a scope".into(),
            });
        }

        let mut outcome = SearchOutcome::default();
        let limit = request.limit;
        let report = walk(&policy, cancel, self.timeout, |entry| {
            outcome.visited += 1;
            if !request.matches_name(&entry.file_name().to_lowercase()) {
                return true;
            }
            if request.kind.is_some_and(|kind| entry.kind != kind) {
                return true;
            }
            let key = PathKey::of(&entry.path);
            if let Some(scope) = &request.scope {
                if !scope.is_within(&key) {
                    return true;
                }
            }
            outcome.matched += 1;
            let dir = key.parent().unwrap_or_else(|| key.clone());
            let hit = SearchHit {
                name: raw_name_units(&entry.path),
                key,
                dir,
                kind: entry.kind,
                size: entry.size,
                mtime_ms: entry.modified_ms.unwrap_or(i64::MIN),
            };
            sink(hit) && (limit == 0 || outcome.matched < limit as u64)
        });
        // Stopping on the sink counts as incomplete too: the caller asked for a bounded answer, so
        // there may be matches it never saw.
        outcome.complete = !report.stopped_early();
        Ok(outcome)
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

    fn provider(root: &std::path::Path) -> WalkProvider {
        WalkProvider::new(
            WalkPolicy::default().rooted_at(root),
            Duration::from_secs(5),
        )
    }

    fn write(root: &std::path::Path, name: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, name).unwrap();
    }

    #[test]
    fn the_provider_streams_hits_and_counts_every_entry_it_saw() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Report-2026.pdf");
        write(dir.path(), "notes.txt");

        let mut hits = Vec::new();
        let outcome = provider(dir.path())
            .search(
                &SearchRequest::new("report"),
                &Cancellation::new(),
                &mut |hit| {
                    hits.push(hit);
                    true
                },
            )
            .unwrap();

        assert_eq!(outcome.matched, 1);
        assert_eq!(outcome.visited, 2, "non-matching entries are still visited");
        assert!(outcome.complete);

        let hit = &hits[0];
        assert_eq!(String::from_utf16_lossy(&hit.name), "Report-2026.pdf");
        assert_eq!(hit.kind, Kind::Document);
        assert_eq!(hit.size, "Report-2026.pdf".len() as u64);
        assert_eq!(hit.dir, PathKey::of(dir.path()));
    }

    #[test]
    fn returning_false_from_the_sink_stops_the_search_and_marks_it_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a-report.txt", "b-report.txt", "c-report.txt"] {
            write(dir.path(), name);
        }

        let mut seen = 0;
        let outcome = provider(dir.path())
            .search(
                &SearchRequest::new("report"),
                &Cancellation::new(),
                &mut |_| {
                    seen += 1;
                    false
                },
            )
            .unwrap();

        assert_eq!(seen, 1);
        assert_eq!(outcome.matched, 1);
        assert!(!outcome.complete);
    }

    #[test]
    fn a_bounded_request_stops_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a-report.txt", "b-report.txt", "c-report.txt"] {
            write(dir.path(), name);
        }

        let mut hits = Vec::new();
        let outcome = provider(dir.path())
            .search(
                &SearchRequest::new("report").limit(2),
                &Cancellation::new(),
                &mut |hit| {
                    hits.push(hit);
                    true
                },
            )
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(outcome.matched, 2);
        assert!(!outcome.complete);
    }

    #[test]
    fn a_scope_restricts_the_walk_to_that_subtree() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "top-report.txt");
        write(dir.path(), "sub/inner-report.txt");

        let scope = PathKey::of(&dir.path().join("sub"));
        let mut hits = Vec::new();
        let outcome = provider(dir.path())
            .search(
                &SearchRequest::new("report").scope(scope),
                &Cancellation::new(),
                &mut |hit| {
                    hits.push(hit);
                    true
                },
            )
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(String::from_utf16_lossy(&hits[0].name), "inner-report.txt");
        assert_eq!(outcome.matched, 1);
        assert_eq!(
            outcome.visited, 1,
            "the walk never descends outside the requested scope"
        );
    }

    #[test]
    fn an_empty_request_is_rejected_before_any_io() {
        let dir = tempfile::tempdir().unwrap();
        let error = provider(dir.path())
            .search(
                &SearchRequest::new("   "),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(matches!(error, ProviderError::Empty));
    }

    #[test]
    fn a_request_without_roots_or_scope_reports_unavailable() {
        let provider = WalkProvider::new(WalkPolicy::default(), Duration::from_secs(1));
        let error = provider
            .search(
                &SearchRequest::new("anything"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        match error {
            ProviderError::Unavailable { provider, reason } => {
                assert_eq!(provider, "walk");
                assert!(reason.contains("scope"));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
