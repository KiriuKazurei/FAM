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

    /// The rules this product searches under: the roots it was configured with, and the subtrees it
    /// was told to keep out.
    ///
    /// A value rather than something each front end assembles, because the two halves have to agree
    /// for every backend and every entry point - a shell that enforces them and a CLI that does not
    /// is two answers to one question.
    pub fn from_config(config: &fam_core::Config) -> Self {
        Self::new(
            config.search_roots.clone(),
            fam_core::Prune::from_config(config),
        )
    }

    /// Whether a row may be reported, for a request that named `wanted` as the subtree to search.
    ///
    /// A named subtree **replaces** the root list rather than being intersected with it. The roots say
    /// where a search with no stated scope may look; someone standing in `E:\Downloads` and asking for
    /// one file has already restricted the search themselves, and intersecting that with the roots
    /// answers "there is nothing here" about the folder they are looking at - the one answer you cannot
    /// tell apart from an empty directory. The exclusions still apply, and apply below the named
    /// boundary the same way a walk applies them while descending.
    fn allows(&self, wanted: Option<&PathKey>, key: &PathKey) -> bool {
        match wanted {
            Some(scope) => scope.is_within(key) && !self.prune.excludes_below(scope, key),
            None => {
                !self.prune.excludes_key(key)
                    && (self.roots.is_empty() || self.roots.iter().any(|root| root.is_within(key)))
            }
        }
    }
}

/// What a search produced, and who produced it.
#[derive(Debug)]
pub struct Searched {
    pub provider: &'static str,
    pub outcome: SearchOutcome,
}

/// Tries providers in order and returns the first that succeeds.
///
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

    /// The router this product searches with: the outside index first, enumeration behind it, and the
    /// configuration's roots and exclusions enforced on whichever one answers.
    ///
    /// Every front end is built from here rather than assembling the list itself. The order is a claim
    /// about which answer is better, the timeout a claim about how long a walk may take, and the scope
    /// a claim about what may be reported - three things worth agreeing about instead of restating.
    pub fn from_config(config: &fam_core::Config) -> Self {
        Self::scoped(
            vec![
                // Enumeration has to stay in the list rather than being a last resort: Everything
                // indexes local fixed NTFS volumes and nothing else.
                Box::new(fam_svc_everything::EverythingProvider::load(
                    &config.search_roots,
                )),
                Box::new(walking_provider(config)),
            ],
            Scope::from_config(config),
        )
    }

    /// A router built from the product's own list, with one more backend in front of it.
    ///
    /// For a caller that holds something no name index holds - the tag store, which answers a field
    /// rather than a subtree. It goes first because the request that needs it needs nothing else:
    /// [`handles_tags`](SearchProvider::handles_tags) keeps a tag request away from the name backends,
    /// and a plain name search walks straight past it because `covers` says no.
    pub fn with_provider(mut self, provider: Box<dyn SearchProvider>) -> Self {
        self.providers.insert(0, provider);
        self
    }

    /// Enumeration and nothing else, for a caller that wants the walk's own answer.
    pub fn walking(config: &fam_core::Config) -> Self {
        Self::scoped(
            vec![Box::new(walking_provider(config))],
            Scope::from_config(config),
        )
    }

    /// The index and nothing else, **and asked even when it says it cannot help**.
    ///
    /// Routing around a backend that declined is the right default and the wrong answer to "search
    /// with Everything": the caller named it, so what they need is its own reason - not deployed, not
    /// running, a subtree on a share nobody indexes - rather than a list that quietly came from
    /// somewhere else.
    pub fn indexed(config: &fam_core::Config) -> Self {
        Self::scoped(
            vec![Box::new(Insisted(Box::new(
                fam_svc_everything::EverythingProvider::load(&config.search_roots),
            )))],
            Scope::from_config(config),
        )
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
        // Folded once, because every row has to be compared against it and the request keeps the
        // caller's own spelling for the sake of anything that restarts the walk.
        let named = request.scope.as_deref().map(PathKey::of);
        for provider in &self.providers {
            // A request about tags goes only to a backend that has tags. Asked before availability and
            // before `covers`, so the one wrapper that forces both of those cannot force this one.
            if request.names_tags() && !provider.handles_tags() {
                continue;
            }
            // Available and able to answer the whole request: a backend that covers part of a
            // scope would return a shorter list that reads as exhaustive.
            if !provider.is_available() || !provider.covers(request) {
                continue;
            }
            let scope = &self.scope;
            match provider.search(request, cancel, &mut |hit| {
                if scope.allows(named.as_ref(), &hit.key) {
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

/// Both halves of the enumeration backend come from the same config: which roots and which rules, and
/// how long one walk may run before it reports what it has.
fn walking_provider(config: &fam_core::Config) -> fam_svc_walk::WalkProvider {
    fam_svc_walk::WalkProvider::new(
        fam_svc_walk::WalkPolicy::from_config(config),
        config.scan_timeout(),
    )
}

/// A provider that insists on being asked. See [`SearchRouter::indexed`].
struct Insisted(Box<dyn SearchProvider>);

impl SearchProvider for Insisted {
    fn id(&self) -> &'static str {
        self.0.id()
    }

    /// The point of the wrapper.
    fn is_available(&self) -> bool {
        true
    }

    /// Also forced, and for the same reason: a backend that is up but cannot cover this request would
    /// otherwise be stepped over before it could say so, and the caller would hear "nothing is
    /// available" instead of the actual obstacle.
    fn covers(&self, _request: &SearchRequest) -> bool {
        true
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        self.0.search(request, cancel, sink)
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

    /// The routing rule the tag store stands on. A name index asked "which files carry the label 合同"
    /// answers with the files *named* 合同, confidently and completely, and that is a different question
    /// with the same shape - so it is not asked at all, and the caller hears "nobody here knows tags"
    /// rather than a list it cannot tell apart from the answer.
    #[test]
    fn a_tag_request_is_not_asked_of_a_name_backend() {
        let router = SearchRouter::new(vec![Box::new(FakeEverything::indexing(&[
            "D:/index/合同.txt",
        ]))]);
        let error = router
            .search(
                &fam_core::SearchRequest::new("tag:合同"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .expect_err("an index of names does not know what anybody labelled");
        assert!(
            matches!(&error, SearchError::Unavailable { provider, .. } if *provider == "none"),
            "got {error:?}"
        );
    }

    /// Insisting on a backend makes it *asked*, not able. `Insisted` exists so a caller who named
    /// Everything hears its own reason for failing, and that reason cannot become an answer about tags
    /// it does not have.
    #[test]
    fn insisting_does_not_give_a_backend_tags_it_lacks() {
        let router = SearchRouter::new(vec![Box::new(Insisted(Box::new(
            FakeEverything::indexing(&["D:/index/合同.txt"]),
        )))]);
        assert!(router
            .search(
                &fam_core::SearchRequest::new("tag:合同"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .is_err());
    }

    /// The other side of the same rule, so the refusal above is not simply "tags never work": with a
    /// backend that handles them in the list, a tag request reaches it and a plain name request still
    /// reaches the index.
    #[test]
    fn a_tag_request_reaches_the_one_backend_that_has_them() {
        let router = SearchRouter::new(vec![
            Box::new(FakeEverything::indexing(&["D:/index/report.txt"])),
            Box::new(Labels),
        ]);

        let (names, searched) = delivered(&router, &fam_core::SearchRequest::new("tag:合同"));
        assert_eq!(searched.provider, "labels");
        assert_eq!(names.len(), 1, "the label library answered");

        let (names, searched) = delivered(&router, &fam_core::SearchRequest::new("report"));
        assert_eq!(searched.provider, "everything");
        assert_eq!(names.len(), 1, "and a plain name search never touched it");
    }

    /// The smallest backend that could exist with the one capability the rule is about.
    struct Labels;

    impl SearchProvider for Labels {
        fn id(&self) -> &'static str {
            "labels"
        }

        fn is_available(&self) -> bool {
            true
        }

        fn handles_tags(&self) -> bool {
            true
        }

        fn covers(&self, request: &SearchRequest) -> bool {
            request.names_tags()
        }

        fn search(
            &self,
            _request: &SearchRequest,
            _cancel: &Cancellation,
            sink: &mut dyn FnMut(SearchHit) -> bool,
        ) -> Result<SearchOutcome, SearchError> {
            let path = std::path::PathBuf::from("D:/library/合同.pdf");
            sink(SearchHit {
                key: fam_core::PathKey::of(&path),
                path: fam_core::raw_path_units(&path),
                kind: fam_core::Kind::Document,
                size: 7,
                mtime_ms: 1,
            })
            .then_some(())
            .ok_or(SearchError::Empty)?;
            Ok(SearchOutcome {
                matched: 1,
                visited: 1,
                complete: true,
            })
        }
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
        assert!(scope.allows(None, &inside));
        assert!(
            !scope.allows(None, &neighbour),
            "a shared prefix is not a parent - the same boundary `is_within` keeps everywhere else"
        );
    }

    /// The CLI's `--in`, and every caller that names a folder of its own: the configured roots are not
    /// a fence. Intersecting a named scope with them answers "nothing here" about a directory the user
    /// is standing in, which is the one answer indistinguishable from an empty folder.
    #[test]
    fn a_named_subtree_replaces_the_configured_roots() {
        let scope = Scope::new(
            [std::path::PathBuf::from("D:/docs")],
            fam_core::Prune::default(),
        );
        let downloads = PathKey::of(std::path::Path::new("E:/downloads"));
        let wanted = PathKey::of(std::path::Path::new("E:/downloads/setup-report.txt"));
        assert!(
            !scope.allows(None, &wanted),
            "the same row is out of bounds for a search that named no scope"
        );
        assert!(
            scope.allows(Some(&downloads), &wanted),
            "…and in bounds once the caller says where to look"
        );
        assert!(
            !scope.allows(
                Some(&downloads),
                &PathKey::of(std::path::Path::new("E:/other/x.txt"))
            ),
            "and the named subtree is a bound of its own, not a hint"
        );
    }

    #[test]
    fn a_scoped_search_outside_the_configured_roots_still_answers() {
        let router = SearchRouter::scoped(
            vec![Box::new(FakeEverything::indexing(&[
                "E:/downloads/setup-report.txt",
                "D:/docs/keep-report.txt",
            ]))],
            Scope::new(
                [std::path::PathBuf::from("D:/docs")],
                fam_core::Prune::default(),
            ),
        );

        let (names, _) = delivered(&router, &SearchRequest::new("report").scope("E:/downloads"));
        assert_eq!(
            names,
            vec![r"E:\downloads\setup-report.txt"],
            "the row inside the named scope survives, and the one in the roots does not"
        );
    }

    /// Both directions at once, because the two are one rule seen from either side: a directory the
    /// caller named is not a directory they descended into, and one they passed *through* on the way
    /// down still is.
    #[test]
    fn a_scope_rooted_inside_an_excluded_directory_keeps_its_own_rows() {
        let router = SearchRouter::scoped(
            vec![Box::new(FakeEverything::indexing(&[
                "D:/proj/node_modules/react/index-report.js",
                "D:/proj/node_modules/react/.git/hooks/report.sh",
            ]))],
            Scope::new(
                [std::path::PathBuf::from("D:/proj")],
                fam_core::Prune::new(Vec::new(), vec!["NODE_MODULES".into(), ".GIT".into()]),
            ),
        );

        let (names, _) = delivered(
            &router,
            &SearchRequest::new("report").scope("D:/proj/node_modules"),
        );
        assert_eq!(
            names,
            vec![r"D:\proj\node_modules\react\index-report.js"],
            "the boundary's own name is not a descent; the `.git` below it still is"
        );
    }
    /// A scoped request is the other half of the same rule: an index of one volume is not asked about
    /// a subtree of another, however sure it is that the file is there. The volumes here are spelled
    /// `Z:` rather than `D:`/`C:` so the temporary directory cannot land inside the index on some
    /// other machine and quietly make the test pass for the wrong reason.
    #[test]
    fn a_scope_on_a_volume_the_index_does_not_hold_goes_to_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("needle.txt"), "x").unwrap();
        let router = SearchRouter::new(vec![
            Box::new(
                FakeEverything::indexing(&["Z:/index/needle-indexed-by-everything.txt"])
                    .indexed_on(&["Z:\\"]),
            ),
            walking(dir.path()),
        ]);

        let (names, searched) = delivered(&router, &SearchRequest::new("needle").scope(dir.path()));
        assert_eq!(searched.provider, "walk");
        assert_eq!(
            names,
            vec![dir.path().join("needle.txt").to_string_lossy()],
            "and the index's row never arrived"
        );
    }

    /// The three routings a config can name, and the order the product claims is better.
    #[test]
    fn a_config_selects_the_backends_once_for_every_front_end() {
        let config = fam_core::Config::default();
        assert_eq!(
            SearchRouter::from_config(&config).ids(),
            vec!["everything", "walk"],
            "the index first, enumeration behind it - restated by no caller"
        );
        assert_eq!(SearchRouter::walking(&config).ids(), vec!["walk"]);
        assert_eq!(SearchRouter::indexed(&config).ids(), vec!["everything"]);
    }

    /// What "the caller named this backend" has to mean: the answer is that backend's own reason, not
    /// the generic one the router falls back to when nothing was even tried.
    #[test]
    fn a_backend_named_by_the_caller_explains_its_own_failure() {
        let config = fam_core::Config::default();
        let error = SearchRouter::indexed(&config)
            .search(
                &SearchRequest::new("anything"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        match error {
            SearchError::Unavailable { provider, reason } => {
                assert_eq!(provider, "everything");
                assert!(
                    !reason.contains("no search provider"),
                    "the router's own excuse reached the caller instead: {reason}"
                );
            }
            other => panic!("expected Everything's own reason, got {other:?}"),
        }
    }
}
