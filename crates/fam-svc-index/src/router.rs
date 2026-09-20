use fam_core::{
    Cancellation, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

/// Tries providers in order and returns the first that succeeds, keeping the ordered-fallback
/// shape the old `SearchService` already had.
///
/// Routing by query scope (fixed NTFS volume → Everything, anything else → enumeration) is added
/// with the Everything client in increment 6; until then callers pass the providers they want.
pub struct SearchRouter {
    providers: Vec<Box<dyn SearchProvider>>,
}

impl SearchRouter {
    pub fn new(providers: Vec<Box<dyn SearchProvider>>) -> Self {
        Self { providers }
    }

    pub fn available(&self) -> Vec<&'static str> {
        self.providers
            .iter()
            .filter(|provider| provider.is_available())
            .map(|provider| provider.id())
            .collect()
    }

    pub fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        let mut last: Option<SearchError> = None;
        for provider in &self.providers {
            if !provider.is_available() {
                continue;
            }
            match provider.search(request, cancel, sink) {
                Ok(outcome) => return Ok(outcome),
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
            Box::new(EverythingProvider),
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
                    names.push(String::from_utf16_lossy(&hit.name));
                    true
                },
            )
            .unwrap();

        assert_eq!(names, vec!["needle-report.txt"]);
        assert_eq!(outcome.matched, 1);
        assert!(outcome.complete);
    }

    #[test]
    fn with_no_provider_available_the_error_is_explicit() {
        let router = SearchRouter::new(vec![Box::new(EverythingProvider)]);
        let error = router
            .search(
                &fam_core::SearchRequest::new("x"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(matches!(error, SearchError::Unavailable { .. }));
    }
}
