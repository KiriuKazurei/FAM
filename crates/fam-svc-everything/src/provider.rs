//! The Everything search backend: the client, its cached state, and one search at a time.
//!
//! The router asks [`SearchProvider::is_available`] from whichever thread is starting a search, while
//! a search in flight holds the client for its whole page sequence - so availability is answered from
//! a copy of the last probe rather than by waiting on the client, and the probe itself runs on the
//! thread that is about to use the answer.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fam_core::{
    Cancellation, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

use crate::client::Api;
use crate::query::compile;
use crate::search::drain;
use crate::state::{next_probe_delay, EverythingState, PROBE_FIRST};

/// The client, and what was last known about it.
struct Session {
    api: Api,
    state: EverythingState,
    /// The probe interval, doubling until the ceiling. A state *change* resets it, so a service that
    /// starts up is noticed at the fast rate rather than at the one the absence had settled to.
    delay: Duration,
    next_probe: Instant,
}

pub struct EverythingProvider {
    session: Mutex<Option<Session>>,
    /// The last observed state, readable without touching the client.
    observed: Mutex<EverythingState>,
}

impl EverythingProvider {
    /// Binds the client if this machine has one deployed beside the application. Not having one is
    /// the ordinary case, and it is a state rather than an error: every search reports itself
    /// unavailable and the router answers from enumeration.
    pub fn load() -> Self {
        match unsafe { Api::load_from_application_dir() } {
            Ok(api) => Self::with_client(api),
            Err(_) => Self::without_client(),
        }
    }

    /// A provider over an already-bound client, for a caller that found the DLL somewhere else.
    pub fn with_client(api: Api) -> Self {
        let state = api.probe();
        Self {
            session: Mutex::new(Some(Session {
                api,
                state: state.clone(),
                delay: PROBE_FIRST,
                next_probe: Instant::now() + PROBE_FIRST,
            })),
            observed: Mutex::new(state),
        }
    }

    /// A provider for a machine with no client at all.
    pub fn without_client() -> Self {
        Self {
            session: Mutex::new(None),
            observed: Mutex::new(EverythingState::DllMissing),
        }
    }

    fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
        slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What the client can do, probing at most once per window.
    ///
    /// Never blocks: a search in flight holds the client for its whole page sequence, and an
    /// availability question asked behind it deserves the last known answer, not a wait. The probe
    /// itself is the non-waiting query, so the work it does costs an IPC post rather than a reply.
    pub fn state(&self) -> EverythingState {
        let Ok(mut guard) = self.session.try_lock() else {
            return Self::read(&self.observed);
        };
        let Some(session) = guard.as_mut() else {
            return EverythingState::DllMissing;
        };
        let now = Instant::now();
        if session.next_probe > now {
            return session.state.clone();
        }
        let observed = session.api.probe();
        session.delay = if observed == session.state {
            next_probe_delay(session.delay)
        } else {
            PROBE_FIRST
        };
        session.next_probe = now + session.delay;
        session.state = observed.clone();
        *Self::read_mut(&self.observed) = observed.clone();
        observed
    }

    fn read(slot: &Mutex<EverythingState>) -> EverythingState {
        Self::lock(slot).clone()
    }

    fn read_mut(slot: &Mutex<EverythingState>) -> MutexGuard<'_, EverythingState> {
        Self::lock(slot)
    }
}

impl SearchProvider for EverythingProvider {
    fn id(&self) -> &'static str {
        "everything"
    }

    fn is_available(&self) -> bool {
        // Goes through `state`, which refreshes on its own schedule and never waits for the client:
        // the router asks this on the way to a search, and a first-ever ask has to be able to find
        // out the truth rather than read a cache nobody has filled yet.
        matches!(self.state(), EverythingState::Ready { .. })
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        if request.is_empty() {
            return Err(SearchError::Empty);
        }
        let Some(query) = compile(request) else {
            return Err(SearchError::Empty);
        };
        // Holding this for the whole page sequence is what the single process-global result list
        // requires: two searches interleaved through one cursor would each read the other's rows.
        // Poisoned only by a panic inside a previous search, which the core's entry point already
        // caught and reported; the client itself is still usable, so recover the guard.
        let mut guard = Self::lock(&self.session);
        let Some(session) = guard.as_mut() else {
            return Err(SearchError::Unavailable {
                provider: self.id(),
                reason: "the client DLL is not deployed with this application".into(),
            });
        };
        let api = &session.api;
        // Ask before blocking. The first page is a *waiting* query, and the cached availability the
        // router just consulted can be up to a probe window old: a service that died in between
        // would otherwise be waited on instead of worked around.
        let state = api.probe();
        session.state = state.clone();
        *Self::lock(&self.observed) = state.clone();
        if !matches!(state, EverythingState::Ready { .. }) {
            return Err(SearchError::Unavailable {
                provider: self.id(),
                reason: format!("the service is {state:?}"),
            });
        }
        let outcome = drain(request, cancel, sink, |offset, want| {
            unsafe { api.page(&query, offset, want) }.map_err(|error| SearchError::Unavailable {
                provider: self.id(),
                reason: error.to_string(),
            })
        })?;
        // Rows that arrived are the freshest evidence there is about the service: the state the
        // pre-flight probe recorded stands, and the next look is due at the fast rate.
        session.delay = PROBE_FIRST;
        session.next_probe = Instant::now() + PROBE_FIRST;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state a machine without the client deployed settles into, and what a search does about
    /// it: `Unavailable`, which is the router's cue to enumerate - never an empty result set that
    /// would look like "nothing on this disk matches".
    #[test]
    fn no_client_means_unavailable_searches_and_dll_missing_state() {
        let provider = EverythingProvider::without_client();
        assert!(!provider.is_available());
        assert_eq!(provider.state(), EverythingState::DllMissing);

        let error = provider
            .search(&SearchRequest::new("x"), &Cancellation::new(), &mut |_| {
                true
            })
            .unwrap_err();
        match error {
            SearchError::Unavailable { provider, reason } => {
                assert_eq!(provider, "everything");
                assert!(reason.contains("not deployed"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_request_is_refused_before_the_client_is_touched() {
        let provider = EverythingProvider::without_client();
        let error = provider
            .search(
                &SearchRequest::new("   "),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(matches!(error, SearchError::Empty), "{error:?}");
    }

    /// A query whose every term is unquotable-to-anything compiles to nothing, and asking Everything
    /// for "nothing" would return its whole index.
    #[test]
    fn a_query_that_compiles_to_nothing_is_not_sent() {
        let provider = EverythingProvider::without_client();
        let error = provider
            .search(&SearchRequest::new("\""), &Cancellation::new(), &mut |_| {
                true
            })
            .unwrap_err();
        assert!(matches!(error, SearchError::Empty), "{error:?}");
    }

    /// The degradation path, run against the real client artifact on a machine with no service: the
    /// state says so, availability says no without waiting on the client, and a search declines the
    /// job instead of returning zero rows - which would look to the caller like an empty disk.
    #[test]
    fn a_bound_client_with_no_service_reports_itself_unavailable() {
        let api = unsafe { Api::load_from(&crate::client::vendored()) }
            .expect("the vendored client must bind");
        let provider = EverythingProvider::with_client(api);

        assert_eq!(provider.state(), EverythingState::NotRunning);
        assert!(!provider.is_available());

        let error = provider
            .search(&SearchRequest::new("x"), &Cancellation::new(), &mut |_| {
                true
            })
            .unwrap_err();
        match error {
            SearchError::Unavailable { provider, reason } => {
                assert_eq!(provider, "everything");
                assert!(reason.contains("NotRunning"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
