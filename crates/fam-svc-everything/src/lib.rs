//! The Everything SDK client.
//!
//! The SDK is vendored at `third_party/everything-sdk/` (MIT). Its `lib/Everything64.lib` is an MSVC
//! import library while the shell's core builds with the GNU toolchain, so this crate loads
//! `Everything64.dll` at run time and resolves the handful of functions it needs. That is what keeps
//! the dependency optional: with Everything absent or not running, name search falls back to
//! enumeration instead of failing to start.
//!
//! Two of its layers need no service to talk to, and are tested without one — which matters because
//! the development machine has no Everything installed: [`query`] compiles a request into
//! Everything's search language, and [`state`] decides what a probe's answers mean.

#[cfg(windows)]
mod client;
mod query;
mod search;
mod state;

#[cfg(windows)]
pub use client::{Api, ClientError, CLIENT_DLL};
pub use query::compile;
/// The paging loop and the row conversion, public because a second implementation of the same
/// contract - the deterministic fake in this crate's tests - is exercised against these rather than
/// against the DLL.
pub use search::{drain, to_hit, RawRow, PAGE};
pub use state::{
    judge, next_probe_delay, EverythingState, Probe, ERROR_IPC, PROBE_FIRST, PROBE_MAX,
};

use fam_core::{
    Cancellation, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

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
            reason: "the DLL binding is not wired yet; name search falls back to enumeration"
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider being unavailable is the *contract* on a machine without Everything, not a gap:
    /// the router must then answer from enumeration rather than report a failure.
    #[test]
    fn an_unwired_provider_reports_unavailable_rather_than_empty_results() {
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
                assert!(reason.contains("falls back"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
