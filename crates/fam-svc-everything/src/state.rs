//! What the probe can conclude about Everything, and how long to wait before asking again.
//!
//! The judgement is a function of data rather than a function that calls Win32, because the machine
//! this crate is developed and tested on has no Everything installed: the interesting cases are
//! "the DLL is there but no service answers" and "a service answers while its index is still
//! building", and both have to be pinned without a service to produce them. The probe that gathers
//! [`Probe`] lives with the binding, in `crate::client`.

use std::time::Duration;

/// `EVERYTHING_ERROR_IPC`: "Everything search client is not running" (`Everything.h:43`). The one
/// error code with a meaning this crate acts on differently from any other failure.
pub const ERROR_IPC: u32 = 2;

/// What a search can currently expect from Everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EverythingState {
    /// `Everything64.dll` could not be loaded at all. Nothing else is asked, so this is the only
    /// state that will never be re-probed into `Ready` without a new process.
    DllMissing,
    /// The DLL loaded and no service answered. Name search goes to enumeration instead.
    NotRunning,
    /// A service answered and its database is still being built.
    Indexing,
    /// A service answered and its database is loaded, so a query will come back.
    ///
    /// This says nothing about *which* volumes are answered for, because the client API cannot say:
    /// there is no "what do you index" call (`Everything_IsVolumeResult` is per result row). What is
    /// indexed is decided from the local drive scan in [`crate::volumes`], and that is what the
    /// router narrows by - not this state.
    Ready,
}

/// Everything one probe step can observe, as data.
pub struct Probe {
    pub dll_loaded: bool,
    /// `Everything_QueryW(FALSE)` - the non-waiting probe - returning true.
    pub query_succeeded: bool,
    /// `Everything_GetLastError()` taken right after it.
    pub last_error: u32,
    /// `Everything_IsDBLoaded()`.
    pub db_loaded: bool,
}

impl Probe {
    /// A probe against a machine with no Everything installed at all.
    pub fn no_dll() -> Self {
        Self {
            dll_loaded: false,
            query_succeeded: false,
            last_error: ERROR_IPC,
            db_loaded: false,
        }
    }
}

/// The judgement.
///
/// Any failure other than `ERROR_IPC` lands on `NotRunning` too: from a caller's side "cannot use
/// Everything right now" is the only distinction that changes behaviour, and inventing a state per
/// error code would be a list nobody branches on. The raw code stays in [`Probe`] for the log line.
pub fn judge(probe: &Probe) -> EverythingState {
    if !probe.dll_loaded {
        return EverythingState::DllMissing;
    }
    if !probe.query_succeeded {
        return EverythingState::NotRunning;
    }
    if !probe.db_loaded {
        return EverythingState::Indexing;
    }
    EverythingState::Ready
}

/// The first probe delay after start-up, and after a state change.
pub const PROBE_FIRST: Duration = Duration::from_secs(1);

/// The ceiling on the probe delay: a machine without Everything should not run an IPC round trip
/// more than twice a minute, and a service that starts up mid-session is noticed within that.
pub const PROBE_MAX: Duration = Duration::from_secs(30);

/// The next delay given the one just used: double it, capped.
pub fn next_probe_delay(previous: Duration) -> Duration {
    previous.saturating_mul(2).min(PROBE_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(db_loaded: bool) -> Probe {
        Probe {
            dll_loaded: true,
            query_succeeded: true,
            last_error: 0,
            db_loaded,
        }
    }

    #[test]
    fn a_missing_dll_ends_the_judgement_before_anything_else() {
        // Even a probe carrying success would be nonsense: nothing could have produced it.
        let probe = Probe {
            dll_loaded: false,
            query_succeeded: true,
            last_error: 0,
            db_loaded: true,
        };
        assert_eq!(judge(&probe), EverythingState::DllMissing);
        assert_eq!(judge(&Probe::no_dll()), EverythingState::DllMissing);
    }

    #[test]
    fn no_service_answering_is_the_ipc_error() {
        let probe = Probe {
            dll_loaded: true,
            query_succeeded: false,
            last_error: ERROR_IPC,
            db_loaded: false,
        };
        assert_eq!(judge(&probe), EverythingState::NotRunning);
    }

    #[test]
    fn any_other_failure_is_the_same_thing_to_a_caller() {
        // 1 is EVERYTHING_ERROR_MEMORY: different cause, same consequence - enumerate instead.
        for error in [1, 7, 9] {
            let probe = Probe {
                dll_loaded: true,
                query_succeeded: false,
                last_error: error,
                db_loaded: true,
            };
            assert_eq!(judge(&probe), EverythingState::NotRunning, "error {error}");
        }
    }

    #[test]
    fn an_answering_service_still_building_its_index_is_indexing() {
        assert_eq!(judge(&answered(false)), EverythingState::Indexing);
    }

    #[test]
    fn an_answering_service_with_its_database_loaded_is_ready() {
        assert_eq!(judge(&answered(true)), EverythingState::Ready);
    }

    #[test]
    fn probing_settles_towards_thirty_seconds() {
        let mut delay = PROBE_FIRST;
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(delay);
            delay = next_probe_delay(delay);
        }
        assert_eq!(
            seen,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
        // Saturating, so a caller that stores the delay cannot panic by carrying it too far.
        assert_eq!(next_probe_delay(Duration::MAX), PROBE_MAX);
    }
}
