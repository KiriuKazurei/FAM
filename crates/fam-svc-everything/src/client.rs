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
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, FILETIME, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
};

use crate::search::RawRow;
use crate::state::{judge, EverythingState, Probe};

/// The file name Everything ships its client under, resolved next to the application.
pub const CLIENT_DLL: &str = "Everything64.dll";

/// `EVERYTHING_SORT_NAME_ASCENDING`. Set on every request rather than left alone: the client holds
/// one process-global cursor, so the last query's sort would otherwise steer this one.
const SORT_NAME_ASCENDING: u32 = 1;

/// The `EVERYTHING_REQUEST_*` bits for the four fields a hit needs. `path` is load-bearing - without
/// it a row's directory comes back empty and the row cannot be opened.
const REQUEST_FIELDS: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0010 | 0x0000_0040;

/// `TRUE`, as the waiting form of `Everything_QueryW` expects.
const WAIT: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("`{dll}` could not be loaded (Win32 error {code})")]
    LoadFailed { dll: String, code: u32 },
    #[error("`{symbol}` is not exported by `{dll}`")]
    MissingSymbol { symbol: &'static str, dll: String },
    #[error("the search service refused the query (Everything error {code})")]
    QueryFailed { code: u32, dll: String },
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
    dll: String,
    set_search: unsafe extern "system" fn(*const u16),
    set_max: unsafe extern "system" fn(u32),
    set_offset: unsafe extern "system" fn(u32),
    set_sort: unsafe extern "system" fn(u32),
    set_request_flags: unsafe extern "system" fn(u32),
    query: unsafe extern "system" fn(i32) -> i32,
    last_error: unsafe extern "system" fn() -> u32,
    num_results: unsafe extern "system" fn() -> u32,
    is_folder_result: unsafe extern "system" fn(u32) -> i32,
    result_file_name: unsafe extern "system" fn(u32) -> *const u16,
    result_path: unsafe extern "system" fn(u32) -> *const u16,
    /// `LARGE_INTEGER` is eight signed bytes, which is what a `*mut i64` points at.
    result_size: unsafe extern "system" fn(u32, *mut i64) -> i32,
    result_date_modified: unsafe extern "system" fn(u32, *mut FILETIME) -> i32,
    is_db_loaded: unsafe extern "system" fn() -> i32,
    target_machine: unsafe extern "system" fn() -> u32,
    clean_up: unsafe extern "system" fn(),
}

// See the type's safety note: the owner-thread discipline is what makes this sound.
unsafe impl Send for Api {}

fn terminate(units: &[u16]) -> Vec<u16> {
    units.iter().copied().chain(std::iter::once(0)).collect()
}

fn wide(text: &str) -> Vec<u16> {
    terminate(&text.encode_utf16().collect::<Vec<u16>>())
}

/// Copies a NUL-terminated wide string the client owns into units of our own.
///
/// The pointer is only good until the next query, which is why the row conversion copies rather than
/// passing slices around. The length comes from scanning for the terminator - the API gives no other
/// bound, and a DLL we chose to load is already trusted further than this.
unsafe fn wide_string(pointer: *const u16) -> Vec<u16> {
    if pointer.is_null() {
        return Vec::new();
    }
    let mut units = Vec::new();
    for offset in 0.. {
        let unit = unsafe { *pointer.add(offset) };
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    units
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
            dll: dll.to_string(),
            set_search: Self::need(library, "Everything_SetSearchW", dll)?,
            set_max: Self::need(library, "Everything_SetMax", dll)?,
            set_offset: Self::need(library, "Everything_SetOffset", dll)?,
            set_sort: Self::need(library, "Everything_SetSort", dll)?,
            set_request_flags: Self::need(library, "Everything_SetRequestFlags", dll)?,
            query: Self::need(library, "Everything_QueryW", dll)?,
            last_error: Self::need(library, "Everything_GetLastError", dll)?,
            num_results: Self::need(library, "Everything_GetNumResults", dll)?,
            is_folder_result: Self::need(library, "Everything_IsFolderResult", dll)?,
            result_file_name: Self::need(library, "Everything_GetResultFileNameW", dll)?,
            result_path: Self::need(library, "Everything_GetResultPathW", dll)?,
            result_size: Self::need(library, "Everything_GetResultSize", dll)?,
            result_date_modified: Self::need(library, "Everything_GetResultDateModified", dll)?,
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
    /// `Ready` says a query would come back, not which volumes it would come back for: the client
    /// API has no "which volumes do you index" call (`Everything_IsVolumeResult` is per result row),
    /// so that set comes from the local drive scan in `crate::volumes` instead.
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
        };
        judge(&probe)
    }

    /// One bounded page of the result list: `[offset, offset + want)` of `query`, converted to raw
    /// rows. This is the whole of this crate's contact with the service; everything that decides
    /// what a search *means* lives in [`crate::search`].
    ///
    /// # Safety
    ///
    /// The client holds one result list for the whole process, so two calls overlapping would read
    /// each other's rows. The provider's mutex is what keeps them exclusive; this is `pub(crate)` so
    /// that only it can reach the cursor.
    pub(crate) unsafe fn page(
        &self,
        query: &[u16],
        offset: u32,
        want: u32,
    ) -> Result<Vec<RawRow>, ClientError> {
        let search = terminate(query);
        (self.set_search)(search.as_ptr());
        (self.set_offset)(offset);
        (self.set_max)(want);
        (self.set_sort)(SORT_NAME_ASCENDING);
        (self.set_request_flags)(REQUEST_FIELDS);
        if (self.query)(WAIT) == 0 {
            return Err(ClientError::QueryFailed {
                code: (self.last_error)(),
                dll: self.dll.clone(),
            });
        }
        // The service should not answer with more rows than it was asked for, and a corrupt count
        // must not become an allocation of that size. `want` is the bound the caller chose.
        let count = (self.num_results)().min(want);
        let mut rows = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut size = 0i64;
            let mut time = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            rows.push(RawRow {
                name: wide_string((self.result_file_name)(index)),
                path: wide_string((self.result_path)(index)),
                folder: (self.is_folder_result)(index) != 0,
                size: if (self.result_size)(index, &mut size) != 0 {
                    size.max(0) as u64
                } else {
                    0
                },
                modified_100ns: ((self.result_date_modified)(index, &mut time) != 0).then(|| {
                    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
                }),
            });
        }
        Ok(rows)
    }

    /// Tears down the client's process-global state (its message window and reply thread) and
    /// unmaps the DLL.
    ///
    /// Once per process, from the thread that owns the client, with no other [`Api`] alive: the
    /// state this cleans up is shared by every handle in the process, so a second one left holding
    /// function pointers into an unmapped DLL is a crash rather than a leak.
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

/// The client library vendored with the SDK. It is the real artifact - the same file that ships
/// next to the core - so binding it exercises the same names production resolves, on a machine that
/// has no Everything installed to talk to.
#[cfg(test)]
pub(crate) fn vendored() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/everything-sdk/dll/Everything64.dll")
        .canonicalize()
        .expect("the vendored client DLL is part of the repository")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_symbol_the_client_calls_resolves_in_the_shipped_dll() {
        let _guard = crate::CLIENT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The rename guard: this resolves each name the binding asks for against the artifact that
        // ships next to the core, so a renamed export fails here and names itself, not mid-search.
        // Binding is the whole assertion: `load_from` resolves every name in the table below, so a
        // rename or a bitness mix-up fails here and names itself.
        unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
    }

    /// What `Everything_GetTargetMachine` is **not**, pinned because the wrong reading of it would
    /// look like a bitness bug: the value describes the *installed application*, reached over IPC, so
    /// with no service running it is 0 even though the client DLL bound perfectly well. A 32/64
    /// mismatch is caught one step earlier instead - `LoadLibraryExW` refuses the wrong architecture
    /// and reports `LoadFailed`.
    #[test]
    fn target_machine_describes_the_service_not_the_dll() {
        let _guard = crate::CLIENT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let api = unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
        assert_eq!(
            api.target_machine(),
            0,
            "no Everything is installed on this machine, so there is no application to report"
        );
    }

    /// The degradation path, verified against a real service that is not there: this development
    /// machine has no Everything installed, so the client must report "not running" rather than an
    /// empty result set - and the router must then answer from enumeration.
    #[test]
    fn a_machine_without_the_service_probes_as_not_running() {
        let _guard = crate::CLIENT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let api = unsafe { Api::load_from(&vendored()).expect("the vendored client must bind") };
        let state = api.probe();
        println!("probe against the vendored client with no service: {state:?}");
        assert_eq!(
            state,
            EverythingState::NotRunning,
            "if Everything ever answers on this machine the test is telling the truth, not the design"
        );
    }

    /// The precondition the rest of this file's degradation tests stand on: nothing named
    /// `Everything64.dll` sits beside the test binary, so `load()` on this machine has no client to
    /// find. `DllMissing` is the one state no amount of waiting turns into `Ready`, which is why the
    /// owner thread stops probing when it sees it - and why it has to be reachable at all.
    ///
    /// This checks the directory rather than calling `load_from_application_dir` and asserting the
    /// error. The latter is what stood here until it failed once in about thirty workspace runs and
    /// never in a single-crate one, with no such file present: a base-name load can be answered by a
    /// module some *other* test in the same process already mapped by full path, and nothing in this
    /// test can prevent or detect that. The mechanism was never pinned down, which is itself the
    /// reason - an assertion whose failure cannot be explained does not belong in a gate. What the
    /// loader does when the file really is missing is covered by
    /// [`a_path_that_is_not_a_dll_fails_to_load_rather_than_panicking`], on a path no other test
    /// touches.
    #[test]
    fn no_client_is_deployed_beside_the_test_binary() {
        let exe = std::env::current_exe().expect("a test binary has a path");
        let directory = exe.parent().expect("a test binary sits in a directory");
        let deployed = directory.join(CLIENT_DLL);
        assert!(
            !deployed.exists(),
            "{deployed:?} is deployed beside the test binary, so `DllMissing` is not what this              machine is reporting"
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
            ClientError::QueryFailed { .. } => {
                panic!("nothing was queried: a bind failure must come first")
            }
        }
    }
}
