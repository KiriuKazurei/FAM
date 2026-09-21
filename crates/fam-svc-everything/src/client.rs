//! Loading the Everything client DLL, and asking it what it can currently do.
//!
//! `lib/Everything64.lib` is an MSVC import library while the core builds with the GNU toolchain for
//! the shell's sake, so the DLL is loaded at run time instead. That is also what makes the
//! dependency *optional*: a machine without Everything ends up in [`EverythingState::NotRunning`]
//! and answers searches by enumerating, rather than having a process that cannot start.
//!
//! The symbol table below is deliberately as wide as the code that uses it and no wider: every entry
//! is resolved the moment the DLL is opened, so "Everything renamed a function" is reported by name
//! at load time instead of surfacing mid-search. The rest of the ~20 functions in the API join the
//! table when the provider starts calling them.

use std::ffi::CString;
use std::mem::{size_of, transmute_copy};
use std::path::Path;

// `FreeLibrary` and `GetLastError` live in `Foundation` in windows-sys 0.61; the loader calls do not.
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
};

use crate::state::{judge, EverythingState, Probe};

/// The file name Everything ships its client under, resolved next to the application.
pub const CLIENT_DLL: &str = "Everything64.dll";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("`{dll}` could not be loaded (Win32 error {code})")]
    LoadFailed { dll: String, code: u32 },
    #[error("`{symbol}` is not exported by `{dll}`")]
    MissingSymbol { symbol: &'static str, dll: String },
}

/// The bound client.
///
/// # Safety
///
/// The Everything client API is **process-global state with one result set** (`Everything.h` has no
/// handles: `SetSearchW` then `QueryW` then `GetResult*` mutate and read one shared cursor), so two
/// searches cannot run through it at once. The contract is that exactly one thread - the search owner
/// in `fam-ffi` - holds an `Api` and calls it, which is also why this is `Send` and deliberately not
/// `Sync`.
pub struct Api {
    library: HMODULE,
    set_search: unsafe extern "system" fn(*const u16),
    query: unsafe extern "system" fn(i32) -> i32,
    last_error: unsafe extern "system" fn() -> u32,
    is_db_loaded: unsafe extern "system" fn() -> i32,
    target_machine: unsafe extern "system" fn() -> u32,
    clean_up: unsafe extern "system" fn(),
}

// See the type's safety note: the owner-thread discipline is what makes this sound.
unsafe impl Send for Api {}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Resolves one exported function by name.
///
/// # Safety
///
/// `T` must be the function pointer type the export actually has; the DLL is one whose exports are
/// the Everything client API.
unsafe fn resolve<T>(library: HMODULE, symbol: &'static str) -> Option<T> {
    let name = CString::new(symbol).ok()?;
    let address = GetProcAddress(library, name.as_ptr().cast())?;
    assert_eq!(
        size_of::<T>(),
        size_of::<*const ()>(),
        "an export address is one pointer wide"
    );
    Some(transmute_copy(&address))
}

impl Api {
    /// Loads the client from the application directory, which is where it is deployed next to the
    /// core - and the only place `LOAD_LIBRARY_SEARCH_APPLICATION_DIR` looks, so a stray
    /// `Everything64.dll` on `PATH` or in the current directory cannot be picked up by accident.
    ///
    /// # Safety
    ///
    /// Loading a DLL runs its initialiser, so this trusts whatever file is deployed beside the
    /// application; and the bound signatures are only correct for a DLL that exports the Everything
    /// client API.
    pub unsafe fn load_from_application_dir() -> Result<Self, ClientError> {
        Self::bind(CLIENT_DLL, LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    }

    /// Loads the client from an explicit path, for a machine where it is not deployed beside the
    /// application.
    ///
    /// # Safety
    ///
    /// As for [`Self::load_from_application_dir`]: the caller vouches for the file at `path`.
    pub unsafe fn load_from(path: &Path) -> Result<Self, ClientError> {
        // A full path needs no search flags; its own dependencies resolve through the default order.
        Self::bind(&path.display().to_string(), 0)
    }

    unsafe fn bind(dll: &str, flags: u32) -> Result<Self, ClientError> {
        let library = LoadLibraryExW(wide(dll).as_ptr(), std::ptr::null_mut(), flags);
        if library.is_null() {
            return Err(ClientError::LoadFailed {
                dll: dll.to_string(),
                code: GetLastError(),
            });
        }
        match Self::resolve_all(library, dll) {
            Ok(api) => Ok(api),
            // A half-bound client is not a client; do not leave the module mapped.
            Err(error) => {
                FreeLibrary(library);
                Err(error)
            }
        }
    }

    unsafe fn resolve_all(library: HMODULE, dll: &str) -> Result<Self, ClientError> {
        Ok(Self {
            library,
            set_search: Self::need(library, "Everything_SetSearchW", dll)?,
            query: Self::need(library, "Everything_QueryW", dll)?,
            last_error: Self::need(library, "Everything_GetLastError", dll)?,
            is_db_loaded: Self::need(library, "Everything_IsDBLoaded", dll)?,
            target_machine: Self::need(library, "Everything_GetTargetMachine", dll)?,
            // Spelled `CleanUp`, not `Cleanup`: that is what the header and the DLL export.
            clean_up: Self::need(library, "Everything_CleanUp", dll)?,
        })
    }

    unsafe fn need<T>(library: HMODULE, symbol: &'static str, dll: &str) -> Result<T, ClientError> {
        resolve(library, symbol).ok_or_else(|| ClientError::MissingSymbol {
            symbol,
            dll: dll.to_string(),
        })
    }

    /// Which machine the loaded client was built for.
    pub fn target_machine(&self) -> u32 {
        unsafe { (self.target_machine)() }
    }

    /// What the client can do right now.
    ///
    /// The probe is the non-waiting query the SDK documents for exactly this: an empty search is
    /// sent, and if nothing answered, `GetLastError` says whether that was because no service is
    /// running. Waiting is not an option - this runs on the search owner thread, and a service that
    /// is starting up must not stall it.
    ///
    /// `Ready { volumes }` comes back with an empty list: the client API has no "which volumes do you
    /// index" call (`Everything_IsVolumeResult` is per result row), so the volume set is gathered
    /// from the local drive scan in `crate::volumes` and attached by the caller.
    pub fn probe(&self) -> EverythingState {
        // Reset the shared search string first: the client holds one, and a probe that re-sent the
        // last real query would report whatever that one's answer was.
        let empty = wide("");
        unsafe { (self.set_search)(empty.as_ptr()) };
        let probe = Probe {
            dll_loaded: true,
            query_succeeded: unsafe { (self.query)(0) != 0 },
            last_error: unsafe { (self.last_error)() },
            db_loaded: unsafe { (self.is_db_loaded)() != 0 },
            volumes: Vec::new(),
        };
        judge(&probe)
    }

    /// Tears down the client's process-global state (its message window and reply thread) and
    /// unmaps the DLL. Call once, from the owner thread, before it stops.
    pub fn shutdown(self) {
        let library = self.library;
        unsafe {
            (self.clean_up)();
        }
        unsafe {
            FreeLibrary(library);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The client library vendored with the SDK. It is the real artifact - the same file that ships
    /// next to the core - so binding it exercises the same names production resolves.
    fn vendored() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/everything-sdk/dll/Everything64.dll")
            .canonicalize()
            .expect("the vendored client DLL is part of the repository")
    }

    #[test]
    fn every_symbol_the_client_calls_resolves_in_the_shipped_dll() {
        // The rename guard: this resolves each name the binding asks for against the artifact that
        // ships next to the core, so a renamed export fails here and names itself, not mid-search.
        let api = unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
        api.shutdown();
    }

    /// What `Everything_GetTargetMachine` is **not**, pinned because the wrong reading of it would
    /// look like a bitness bug: the value describes the *installed application*, reached over IPC, so
    /// with no service running it is 0 even though the client DLL bound perfectly well. A 32/64
    /// mismatch is caught one step earlier instead - `LoadLibraryExW` refuses the wrong architecture
    /// and reports `LoadFailed`.
    #[test]
    fn target_machine_describes_the_service_not_the_dll() {
        let api = unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
        assert_eq!(
            api.target_machine(),
            0,
            "no Everything is installed on this machine, so there is no application to report"
        );
        api.shutdown();
    }

    /// The degradation path, verified against a real service that is not there: this development
    /// machine has no Everything installed, so the client must report "not running" rather than an
    /// empty result set - and the router must then answer from enumeration.
    #[test]
    fn a_machine_without_the_service_probes_as_not_running() {
        let api = unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
        let state = api.probe();
        println!("probe against the vendored client with no service: {state:?}");
        assert_eq!(
            state,
            EverythingState::NotRunning,
            "if Everything ever answers on this machine the test is telling the truth, not the design"
        );
        api.shutdown();
    }

    /// The other half of the degradation story, and the half that needs no service to exist: a
    /// machine where the client is not deployed beside the application at all. `DllMissing` is the
    /// one state no amount of waiting turns into `Ready`, which is why the owner thread stops
    /// probing when it sees it.
    ///
    /// This asserts something about the test environment - no `Everything64.dll` beside the test
    /// binary - which is true of `target/<profile>/deps` and would only break if a build step started
    /// copying the client there.
    #[test]
    fn a_machine_without_the_client_deployed_fails_to_load() {
        assert!(
            matches!(
                unsafe { Api::load_from_application_dir() },
                Err(ClientError::LoadFailed { .. })
            ),
            "the client should not be sitting next to the test binary"
        );
    }

    #[test]
    fn a_path_that_is_not_a_dll_fails_to_load_rather_than_panicking() {
        let outcome = unsafe { Api::load_from(Path::new("C:/Windows/write.exe")) };
        let error = match outcome {
            Err(error) => error,
            Ok(_) => panic!("a system tool's DLL must not pass as the Everything client"),
        };
        match error {
            // Either failure mode is honest: the module may load and then not export our names.
            ClientError::LoadFailed { .. } => {}
            ClientError::MissingSymbol { symbol, .. } => {
                assert!(symbol.starts_with("Everything_"), "{symbol}");
            }
        }
    }
}
