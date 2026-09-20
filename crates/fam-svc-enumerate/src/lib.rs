//! One-level directory enumeration: the primitive a lazy directory cache is built on.
//!
//! Opening a directory must not recurse, must not allocate per entry beyond the caller's own copy,
//! and must say so when it stopped early. On Windows the work is done by
//! `FindFirstFileExW`/`FindNextFileW`, whose single enumeration record carries name, attributes,
//! size and timestamps; the `std::fs` reader behind it exists as the non-Windows implementation and
//! as the oracle the Win32 path is diffed against in tests.

mod lister;
#[cfg(any(not(windows), test))]
mod portable;
#[cfg(windows)]
mod win32;

pub use lister::{list_one_level, EnumError, EnumReport, RawItem};
