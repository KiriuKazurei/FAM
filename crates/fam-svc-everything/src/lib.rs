//! The Everything SDK client.
//!
//! The SDK is vendored at `third_party/everything-sdk/` (MIT). Its `lib/Everything64.lib` is an MSVC
//! import library while the core builds with the GNU toolchain for the shell's sake, so this crate
//! loads `Everything64.dll` at run time and resolves the functions it needs. That is what keeps the
//! dependency *optional*: a machine without Everything ends up in [`EverythingState::NotRunning`]
//! and answers searches by enumerating, rather than having a process that cannot start.
//!
//! Three layers, and only one of them touches the DLL:
//!
//! - [`query`] compiles a request into Everything's search language, and [`search`] decides what a
//!   result list means - paging, limits, cancellation, the kind bucket, and the re-check that turns
//!   the query layer's deliberate over-approximation back into an exact answer. Both are tested
//!   without a service, which matters because the development machine has none.
//! - [`client`] binds the exports and reads one page.
//! - [`provider`] joins them and owns the client, because the API is process-global with a single
//!   result set: one search at a time, on one thread.

/// The client is one piece of process-global state: whoever posts a query owns the answer. Tests that
/// bind or probe take this, or one test's teardown pulls the cursor out from under another's - which
/// showed up once as a single failure in a whole-workspace run and never reproduced.
#[cfg(all(test, windows))]
pub(crate) static CLIENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(not(windows))]
mod absent;
#[cfg(windows)]
mod client;
#[cfg(windows)]
mod provider;
mod query;
mod search;
mod state;

#[cfg(not(windows))]
pub use absent::EverythingProvider;
#[cfg(windows)]
pub use client::{Api, ClientError, CLIENT_DLL};
#[cfg(windows)]
pub use provider::EverythingProvider;
pub use query::compile;
/// The paging loop and the row conversion, public because a second implementation of the same
/// contract - the deterministic fake in this crate's tests - is exercised against these rather than
/// against the DLL.
pub use search::{drain, to_hit, RawRow, PAGE};
pub use state::{
    judge, next_probe_delay, EverythingState, Probe, ERROR_IPC, PROBE_FIRST, PROBE_MAX,
};
