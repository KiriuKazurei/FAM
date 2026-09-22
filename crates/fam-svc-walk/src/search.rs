//! The enumeration search backend.
//!
//! Everything's contract is a pushed stream, not a returned collection, so a caller sees rows while
//! the walk is still running and can stop it. The vocabulary (`SearchRequest`, `SearchHit`,
//! `SearchOutcome`) is `fam-core`'s; this file is only the one implementation that reads a disk.
//!
//! The router that chooses between this and an outside index lives in `fam-svc-index`, and so do the
//! scope rules enforced on the way out - which is why neither the roots nor the exclusions are
//! re-checked here beyond the request's own scope.

use std::time::Duration;

use fam_core::{
    raw_path_units, Cancellation, PathKey, SearchError as ProviderError, SearchHit, SearchOutcome,
    SearchProvider, SearchRequest,
};

use crate::walk::{walk, WalkPolicy};

/// Enumeration as a search backend: walk the scope, match names, push what matches.
///
/// This is the answer that is always available, and the only one that reaches a volume no outside
/// index covers - which is why it sits behind the index in the router rather than replacing it.
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
        // The scope is the root as the caller spelled it, and its folded form is what decides whether
        // an entry is inside it: two uses, one string each, and swapping them would either lose the
        // caller's casing or compare paths the filesystem does not treat as equal.
        let mut inside = None;
        if let Some(scope) = &request.scope {
            policy.roots = vec![scope.clone()];
            inside = Some(PathKey::of(scope));
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
            if let Some(scope) = &inside {
                if !scope.is_within(&key) {
                    return true;
                }
            }
            outcome.matched += 1;
            let hit = SearchHit {
                path: raw_path_units(&entry.path),
                key,
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
    use fam_core::Kind;

    use super::*;

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

    /// A hit's display name: the last component of the verbatim path it carries.
    fn base(hit: &SearchHit) -> String {
        fam_core::path_buf_from_units(&hit.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
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
        assert_eq!(base(hit), "Report-2026.pdf");
        assert_eq!(
            fam_core::path_buf_from_units(&hit.path),
            dir.path().join("Report-2026.pdf"),
            "the path is what the filesystem reported, not the folded form the identity uses"
        );
        assert_eq!(hit.kind, Kind::Document);
        assert_eq!(hit.size, "Report-2026.pdf".len() as u64);
        assert_eq!(hit.key.parent().unwrap(), PathKey::of(dir.path()));
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

        let scope = dir.path().join("sub");
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
        assert_eq!(base(&hits[0]), "inner-report.txt");
        assert_eq!(outcome.matched, 1);
        assert_eq!(
            outcome.visited, 1,
            "the walk never descends outside the requested scope"
        );
    }

    /// The bucket filter, which the query layer deliberately does not push down as an `ext:` list: a
    /// search for a name is not a search for a suffix, so rows arrive and are sorted here.
    #[test]
    fn a_kind_narrows_the_hits_without_narrowing_the_query() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sunset.png");
        write(dir.path(), "sunset-plans.txt");

        let mut images = Vec::new();
        let outcome = provider(dir.path())
            .search(
                &SearchRequest::new("sunset").kind(Kind::Image),
                &Cancellation::new(),
                &mut |hit| {
                    images.push(hit);
                    true
                },
            )
            .unwrap();

        assert_eq!(images.len(), 1);
        assert_eq!(base(&images[0]), "sunset.png");
        assert_eq!(outcome.matched, 1);
        assert_eq!(
            outcome.visited, 2,
            "the .txt was looked at and then sorted out, never skipped"
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
