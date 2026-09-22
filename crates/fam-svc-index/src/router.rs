use fam_core::{
    Cancellation, PathKey, Prune, SearchError, SearchHit, SearchOutcome, SearchProvider,
    SearchRequest,
};

/// What a search may report, regardless of which backend answered.
///
/// A backend that lives outside this program - an index like Everything's - knows nothing about the
/// user's exclusions or about which roots they configured, and would happily answer from the whole
/// volume. Enforcing that here, on the way out, is what keeps "the same search" meaning the same
/// thing whichever backend served it.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Roots an unscoped search may answer for. Empty means no restriction: a caller that configured
    /// no roots is in no position to narrow anyone.
    roots: Vec<PathKey>,
    prune: Prune,
}

impl Scope {
    pub fn new(roots: impl IntoIterator<Item = std::path::PathBuf>, prune: Prune) -> Self {
        Self {
            roots: roots.into_iter().map(|root| PathKey::of(&root)).collect(),
            prune,
        }
    }

    pub fn unrestricted() -> Self {
        Self::default()
    }

    fn allows(&self, key: &PathKey) -> bool {
        if self.prune.excludes_key(key) {
            return false;
        }
        self.roots.is_empty() || self.roots.iter().any(|root| root.is_within(key))
    }
}

/// What a search produced, and who produced it.
#[derive(Debug)]
pub struct Searched {
    pub provider: &'static str,
    pub outcome: SearchOutcome,
}

/// Tries providers in order and returns the first that succeeds, keeping the ordered-fallback
/// shape the old `SearchService` already had.
///
/// Routing by query scope (fixed NTFS volume → Everything, anything else → enumeration) is the next
/// step of this increment; until then callers pass the providers they want, in the order they want
/// them tried.
pub struct SearchRouter {
    providers: Vec<Box<dyn SearchProvider>>,
    scope: Scope,
}

impl SearchRouter {
    /// A router that reports whatever its providers answer.
    pub fn new(providers: Vec<Box<dyn SearchProvider>>) -> Self {
        Self {
            providers,
            scope: Scope::unrestricted(),
        }
    }

    /// A router that enforces the product's own scope rules on every answer, because a backend that
    /// indexes the volume does not know them.
    pub fn scoped(providers: Vec<Box<dyn SearchProvider>>, scope: Scope) -> Self {
        Self { providers, scope }
    }

    pub fn available(&self) -> Vec<&'static str> {
        self.providers
            .iter()
            .filter(|provider| provider.is_available())
            .map(|provider| provider.id())
            .collect()
    }

    /// Runs the first available provider that succeeds, and says **which one** answered.
    ///
    /// The id is part of the result because which backend served a search is user-visible and
    /// load-bearing: enumeration covers any volume while Everything only indexes local fixed NTFS
    /// ones, so "complete" means different scopes depending on who ran.
    ///
    /// Rows the [`Scope`] rejects never reach the caller's sink. Note that `outcome.matched` is what
    /// the *backend* matched, which can be more than the caller received - `visited` and `complete`
    /// are unaffected, and the rows the caller counts are the ones it was handed.
    pub fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<Searched, SearchError> {
        let mut last: Option<SearchError> = None;
        for provider in &self.providers {
            if !provider.is_available() {
                continue;
            }
            let scope = &self.scope;
            match provider.search(request, cancel, &mut |hit| {
                if scope.allows(&hit.key) {
                    sink(hit)
                } else {
                    true
                }
            }) {
                Ok(outcome) => {
                    return Ok(Searched {
                        provider: provider.id(),
                        outcome,
                    })
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or(SearchError::Unavailable {
            provider: "none",
            reason: "no search provider is available".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fam_svc_everything::EverythingProvider;
    use fam_svc_walk::{WalkPolicy, WalkProvider};

    #[test]
    fn an_unavailable_index_falls_through_to_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("needle-report.txt"), "x").unwrap();
        let router = SearchRouter::new(vec![
            Box::new(EverythingProvider::without_client()),
            Box::new(WalkProvider::new(
                WalkPolicy::default().rooted_at(dir.path()),
                std::time::Duration::from_secs(30),
            )),
        ]);

        assert_eq!(router.available(), vec!["walk"]);

        let mut names = Vec::new();
        let outcome = router
            .search(
                &fam_core::SearchRequest::new("needle"),
                &Cancellation::new(),
                &mut |hit| {
                    let path = fam_core::path_buf_from_units(&hit.path);
                    names.push(path.file_name().unwrap().to_string_lossy().into_owned());
                    true
                },
            )
            .unwrap();

        assert_eq!(names, vec!["needle-report.txt"]);
        assert_eq!(outcome.outcome.matched, 1);
        assert!(outcome.outcome.complete);
        assert_eq!(outcome.provider, "walk");
    }

    #[test]
    fn with_no_provider_available_the_error_is_explicit() {
        let router = SearchRouter::new(vec![Box::new(EverythingProvider::without_client())]);
        let error = router
            .search(
                &fam_core::SearchRequest::new("x"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(matches!(error, SearchError::Unavailable { .. }));
    }
    /// A backend that answers from a fixed list of paths. It stands in for the one thing
    /// enumeration is not: a searcher that reaches beyond the roots and knows nothing about the
    /// user's exclusions.
    struct FakeProvider {
        id: &'static str,
        hits: Vec<std::path::PathBuf>,
    }

    impl FakeProvider {
        fn answering(paths: &[&str]) -> Self {
            Self {
                id: "fake",
                hits: paths.iter().map(std::path::PathBuf::from).collect(),
            }
        }
    }

    impl SearchProvider for FakeProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn is_available(&self) -> bool {
            true
        }

        fn search(
            &self,
            _request: &SearchRequest,
            _cancel: &Cancellation,
            sink: &mut dyn FnMut(SearchHit) -> bool,
        ) -> Result<SearchOutcome, SearchError> {
            let mut outcome = SearchOutcome {
                matched: self.hits.len() as u64,
                visited: self.hits.len() as u64,
                complete: true,
            };
            for path in &self.hits {
                let hit = SearchHit {
                    key: PathKey::of(path),
                    path: path.to_string_lossy().encode_utf16().collect(),
                    kind: fam_core::Kind::Document,
                    size: 1,
                    mtime_ms: 0,
                };
                if !sink(hit) {
                    outcome.complete = false;
                    break;
                }
            }
            Ok(outcome)
        }
    }

    fn delivered(router: &SearchRouter) -> (Vec<String>, SearchOutcome) {
        let mut names = Vec::new();
        let searched = router
            .search(
                &SearchRequest::new("anything"),
                &Cancellation::new(),
                &mut |hit| {
                    names.push(String::from_utf16_lossy(&hit.path));
                    true
                },
            )
            .expect("the fake always answers");
        (names, searched.outcome)
    }

    /// The hole this closes: an index of the whole volume answers with files the user excluded, and
    /// with files outside every root they configured.
    #[test]
    fn the_scope_rejects_rows_the_backend_should_never_have_found() {
        let outside = std::path::PathBuf::from("E:/elsewhere/away.txt");
        let excluded = std::path::PathBuf::from("D:/root/node_modules/left-pad/index.js");
        let kept = std::path::PathBuf::from("D:/root/keep.txt");
        let router = SearchRouter::scoped(
            vec![Box::new(FakeProvider::answering(&[
                "E:/elsewhere/away.txt",
                "D:/root/node_modules/left-pad/index.js",
                "D:/root/keep.txt",
            ]))],
            Scope::new(
                [std::path::PathBuf::from("D:/root")],
                fam_core::Prune::new(Vec::new(), vec!["NODE_MODULES".into()]),
            ),
        );

        let (names, outcome) = delivered(&router);
        assert_eq!(
            names,
            vec![kept.to_string_lossy()],
            "the two others were dropped without stopping the drain"
        );
        assert_eq!(
            outcome.matched, 3,
            "`matched` is what the backend matched, not what the caller was handed"
        );
        assert!(
            outcome.complete,
            "rejecting rows is not the same as not finishing"
        );
        assert!(!names.contains(&outside.to_string_lossy().into_owned()));
        assert!(!names.contains(&excluded.to_string_lossy().into_owned()));
    }

    #[test]
    fn a_router_without_a_scope_reports_what_it_is_given() {
        let router = SearchRouter::new(vec![Box::new(FakeProvider::answering(&[
            "E:/elsewhere/away.txt",
            "D:/root/node_modules/left-pad/index.js",
        ]))]);
        let (names, _) = delivered(&router);
        assert_eq!(names.len(), 2, "the default scope restricts nobody");
    }

    #[test]
    fn roots_restrict_an_unscoped_search_without_touching_a_scoped_one() {
        let scope = Scope::new(
            [std::path::PathBuf::from("D:/docs")],
            fam_core::Prune::default(),
        );
        let inside = PathKey::of(std::path::Path::new("D:/docs/a/b.txt"));
        let neighbour = PathKey::of(std::path::Path::new("D:/docs_sibling/a.txt"));
        assert!(scope.allows(&inside));
        assert!(
            !scope.allows(&neighbour),
            "a shared prefix is not a parent - the same boundary `is_within` keeps everywhere else"
        );
    }
}
