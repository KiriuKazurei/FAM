//! The Everything SDK client. Skeleton only.
//!
//! The SDK is vendored at `third_party/everything-sdk/` (MIT). Its `lib/Everything64.lib` is an
//! MSVC import library while this crate builds with the GNU toolchain, so the real client will
//! load `Everything64.dll` at run time with `LoadLibraryExW` and resolve the ~20 functions it
//! needs — that lands in increment 6, together with the `DllMissing`/`NotRunning`/`Indexing`/
//! `Ready` probe. Until then the provider reports itself unavailable, which is also the correct
//! state on any machine where Everything is not running.

use fam_core::{
    Cancellation, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EverythingState {
    DllMissing,
    NotRunning,
    Indexing,
    Ready,
}

pub struct EverythingProvider;

impl SearchProvider for EverythingProvider {
    fn id(&self) -> &'static str {
        "everything"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn search(
        &self,
        _request: &SearchRequest,
        _cancel: &Cancellation,
        _sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        Err(SearchError::Unavailable {
            provider: self.id(),
            reason: "the Everything SDK client is wired in increment 6".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skeleton_reports_unavailable_rather_than_empty_results() {
        let provider = EverythingProvider;
        assert!(!provider.is_available());

        let error = provider
            .search(&SearchRequest::new("x"), &Cancellation::new(), &mut |_| {
                true
            })
            .unwrap_err();

        match error {
            SearchError::Unavailable { provider, reason } => {
                assert_eq!(provider, "everything");
                assert!(reason.contains("increment 6"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
