//! One-level directory enumeration: the primitive a lazy directory cache is built on.
//!
//! Increment 2 replaces the std-based reader with `FindFirstFileExW`/`FindNextFileW`, which yields
//! size, timestamps and attributes from the same enumeration record and so removes the second
//! syscall per entry that the current implementation pays.

mod lister;

pub use lister::{list_one_level, EnumError, EnumReport, RawItem};
