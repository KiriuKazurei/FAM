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
/// A provider is asked only when it is up *and* able to answer the whole request, so an index over
/// part of the machine is skipped rather than believed. Which one answered comes back with the
/// results, because the answer's scope depends on the one that gave it.
pub struct SearchRouter {
    providers: Vec<Box<dyn SearchProvider>>,
    scope: Scope,
}

impl SearchRouter {
    /// A router that reports whatever its providers answer, in the order they were given.
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

    /// Every provider the router knows about, usable or not, in the order it tries them.
    ///
    /// A different question from [`available`](Self::available), and asked separately because a shell
    /// shows both: "this build can search two ways" and "one of them is up right now" are not the same
    /// sentence, and only the second one changes while the program runs.
    pub fn ids(&self) -> Vec<&'static str> {
        self.providers
            .iter()
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
    ///
    /// A backend that hands over rows and *then* fails is a failure, and the router moves on - but the
    /// sink is append-only, so what it already received stays received. The answer is then a union,
    /// labelled with the backend that finished. That is sound only because every row a backend offers
    /// has been checked against the request by that backend, so the surplus rows are matches rather
    /// than guesses; what no one can tell afterwards is which row came from where.
    pub fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<Searched, SearchError> {
        let mut last: Option<SearchError> = None;
        for provider in &self.providers {
            // Available and able to answer the whole request: a backend that covers part of a
            // scope would return a shorter list that reads as exhaustive.
            if !provider.is_available() || !provider.covers(request) {
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
    /// A running Everything, as the router sees it: a searcher that reaches past the caller's roots
    /// and knows nothing about their exclusions. The paging, the re-check and the row shape are the
    /// real ones - see [`FakeEverything`].
    use fam_svc_everything::testing::FakeEverything;
    use fam_svc_everything::PAGE;

    /// A result list `rows` long, each name carrying `paged` so the re-check keeps it.
    fn listing(rows: usize) -> Vec<String> {
        (0..rows)
            .map(|index| format!("D:/index/paged-report-{index}.txt"))
            .collect()
    }

    /// Enumeration over `dir`, as the other half of every routing decision.
    fn walking(dir: &std::path::Path) -> Box<dyn SearchProvider> {
        Box::new(WalkProvider::new(
            WalkPolicy::default().rooted_at(dir),
            std::time::Duration::from_secs(30),
        ))
    }

    /// The paths a router hands its caller, and what it says about the answer.
    fn delivered(router: &SearchRouter, request: &SearchRequest) -> (Vec<String>, Searched) {
        let mut names = Vec::new();
        let searched = router
            .search(request, &Cancellation::new(), &mut |hit| {
                names.push(String::from_utf16_lossy(&hit.path));
                true
            })
            .expect("a provider answers");
        (names, searched)
    }

    /// The hole this closes: an index of the whole volume answers with files the user excluded, and
    /// with files outside every root they configured.
    #[test]
    fn the_scope_rejects_rows_the_backend_should_never_have_found() {
        let router = SearchRouter::scoped(
            vec![Box::new(FakeEverything::indexing(&[
                "E:/elsewhere/away-report.txt",
                "D:/root/node_modules/left-pad/index-report.js",
                "D:/root/keep-report.txt",
            ]))],
            Scope::new(
                [std::path::PathBuf::from("D:/root")],
                fam_core::Prune::new(Vec::new(), vec!["NODE_MODULES".into()]),
            ),
        );

        let (names, searched) = delivered(&router, &SearchRequest::new("report"));
        assert_eq!(
            names,
            vec![r"D:\root\keep-report.txt"],
            "the two others were dropped without stopping the drain"
        );
        assert_eq!(
            searched.outcome.matched, 3,
            "`matched` is what the backend matched, not what the caller was handed"
        );
        assert!(
            searched.outcome.complete,
            "rejecting rows is not the same as not finishing"
        );
    }

    #[test]
    fn a_router_without_a_scope_reports_what_it_is_given() {
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&[
            "E:/elsewhere/away-report.txt",
            "D:/root/node_modules/left-pad/index-report.js",
        ]))]);
        let (names, _) = delivered(&router, &SearchRequest::new("report"));
        assert_eq!(names.len(), 2, "the default scope restricts nobody");
    }

    /// The point of the whole increment: with the service up, a search is answered from the index and
    /// the disk is never walked. The assertion is about *which* rows arrived, which is what a caller
    /// can observe; the walk's own file is in the temp directory the whole time.
    #[test]
    fn an_up_index_answers_and_enumeration_is_not_consulted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("walked-report.txt"), "x").unwrap();
        let router = SearchRouter::new(vec![
            Box::new(FakeEverything::indexing(&["D:/index/indexed-report.txt"])),
            walking(dir.path()),
        ]);

        let (names, searched) = delivered(&router, &SearchRequest::new("report"));
        assert_eq!(
            names,
            vec![r"D:\index\indexed-report.txt"],
            "the walk's file is not in the answer, so the walk never ran"
        );
        assert_eq!(searched.provider, "everything");
        assert!(searched.outcome.complete);
    }

    #[test]
    fn a_result_list_longer_than_one_page_arrives_in_full() {
        let rows = PAGE as usize * 2 + 7;
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&listing(rows)))]);

        let (names, searched) = delivered(&router, &SearchRequest::new("paged"));
        assert_eq!(names.len(), rows);
        assert_eq!(searched.outcome.matched, searched.outcome.visited);
        assert!(
            searched.outcome.complete,
            "the list was read to its end, in {} pages",
            searched.outcome.visited / PAGE as u64 + 1
        );
    }

    /// A ceiling is an answer the caller asked for, not a truncated one: it is final, and it is the
    /// `complete` flag - not the row count - that says the scope may hold more.
    #[test]
    fn a_ceiling_shorter_than_the_list_is_final_but_not_complete() {
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&listing(
            PAGE as usize * 2,
        )))]);

        let (names, searched) = delivered(&router, &SearchRequest::new("paged").limit(5));
        assert_eq!(names.len(), 5);
        assert_eq!(searched.outcome.matched, 5);
        assert!(!searched.outcome.complete);
    }

    #[test]
    fn a_sink_that_stops_takes_no_more_rows_than_it_asked_for() {
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&listing(
            PAGE as usize * 2,
        )))]);

        let mut taken = 0;
        let searched = router
            .search(
                &SearchRequest::new("paged"),
                &Cancellation::new(),
                &mut |_| {
                    taken += 1;
                    taken < 3
                },
            )
            .unwrap();

        assert_eq!(taken, 3);
        assert_eq!(searched.outcome.matched, 3);
        assert!(!searched.outcome.complete);
    }

    /// The cancellation granularity, stated as a fact rather than hoped for: a page the service
    /// already sent is delivered in full, and only the *next* request is skipped. A caller that wants
    /// the rows it saw has them; `complete` is false.
    #[test]
    fn a_stopped_signal_is_honoured_between_pages_and_is_not_an_error() {
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&listing(
            PAGE as usize * 3,
        )))]);

        let cancel = Cancellation::new();
        let mut seen = 0u64;
        let searched = router
            .search(&SearchRequest::new("paged"), &cancel, &mut |_| {
                seen += 1;
                if seen == 3 {
                    cancel.cancel();
                }
                true
            })
            .expect("a cancelled search is not a failed one");

        assert_eq!(seen, u64::from(PAGE), "the first page was delivered whole");
        assert_eq!(searched.provider, "everything");
        assert!(!searched.outcome.complete);
    }

    /// What a service that dies mid-search actually costs: the rows it had already sent stay with the
    /// caller, because the sink is append-only and this layer cannot take them back. The fallback then
    /// adds its own, and the answer says which backend finished. The rows are all real matches - the
    /// re-check ran on them - so the union is honest even though the label is only half the story.
    #[test]
    fn a_service_that_dies_partway_keeps_its_rows_and_adds_the_fallback_s() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dying-report-walked.txt"), "x").unwrap();
        let dying: Vec<String> = (0..(PAGE as usize * 2))
            .map(|index| format!("D:/index/dying-report-{index}.txt"))
            .collect();
        let router = SearchRouter::new(vec![
            Box::new(FakeEverything::indexing(&dying).breaks_at(PAGE)),
            walking(dir.path()),
        ]);

        let (names, searched) = delivered(&router, &SearchRequest::new("dying"));
        assert_eq!(searched.provider, "walk", "the fallback is what answered");
        assert_eq!(
            names.len(),
            PAGE as usize + 1,
            "the index's first page plus the one file on disk"
        );
        assert_eq!(
            names.last().unwrap(),
            &dir.path().join("dying-report-walked.txt").to_string_lossy(),
            "and the walk's row is the last thing the caller was handed"
        );
    }

    /// The coverage decision, made by the real rule rather than a stub that says no: an index of one
    /// volume is not asked about a search whose roots span two.
    #[test]
    fn an_index_that_cannot_cover_every_root_is_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("needle.txt"), "x").unwrap();
        let router = SearchRouter::new(vec![
            Box::new(
                FakeEverything::indexing(&["D:/index/needle-indexed-by-everything.txt"])
                    .indexed_on(&["D:\\"])
                    .rooted_at(&[
                        std::path::PathBuf::from("D:/docs"),
                        std::path::PathBuf::from("E:/games"),
                    ]),
            ),
            walking(dir.path()),
        ]);

        let (names, searched) = delivered(&router, &SearchRequest::new("needle"));
        assert_eq!(names.len(), 1, "the index's row never arrived");
        assert_eq!(searched.provider, "walk");
        assert_eq!(
            searched.outcome.matched, 1,
            "and what the caller saw is one backend's answer, not two merged"
        );
        assert_eq!(
            router.available(),
            vec!["everything", "walk"],
            "being available is not the same as being able to answer"
        );
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
    /// A scoped request is the other half of the same rule: an index of `D:` is not asked about a
    /// subtree of `E:`, however sure it is that the file is there.
    #[test]
    fn a_scope_on_a_volume_the_index_does_not_hold_goes_to_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("needle.txt"), "x").unwrap();
        let router = SearchRouter::new(vec![
            Box::new(
                FakeEverything::indexing(&["D:/index/needle-indexed-by-everything.txt"])
                    .indexed_on(&["D:\\"]),
            ),
            walking(dir.path()),
        ]);

        let (names, searched) = delivered(&router, &SearchRequest::new("needle").scope(dir.path()));
        assert_eq!(searched.provider, "walk");
        assert_eq!(names, vec![dir.path().join("needle.txt").to_string_lossy()]);
    }
}
