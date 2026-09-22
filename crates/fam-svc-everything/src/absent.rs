//! The provider on a platform with no client to load.
//!
//! Everything is a Windows application. This exists so the crate - and the core, which boxes a
//! provider unconditionally - still compiles elsewhere, answering the only question it can: not
//! available, so name search is served by enumeration.

use std::path::PathBuf;

use fam_core::{
    Cancellation, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

use crate::state::EverythingState;

pub struct EverythingProvider;

impl EverythingProvider {
    /// The same constructor the Windows provider offers, with the same argument ignored: there is no
    /// index here to decide coverage for, so which roots were configured cannot change any answer.
    pub fn load(_roots: &[PathBuf]) -> Self {
        Self
    }

    /// The Windows crate's "no client was found" provider. Same answer, no session to hold.
    pub fn without_client() -> Self {
        Self
    }

    pub fn state(&self) -> EverythingState {
        EverythingState::DllMissing
    }
}

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
            reason: "this build has no Everything client to load".into(),
        })
    }
}
