use std::path::Path;

use fam_core::{Cancellation, ItemFlags, Kind};

/// One entry as the filesystem reported it, borrowed from the reader's own buffer.
///
/// The name is a slice, not a `String` or `Vec<u16>`: `FindNextFileW` refills a single record on
/// every step, so copying a name out of it would be one allocation per entry for something the
/// caller consumes before the next step anyway. Implementations must therefore hand the record to
/// the sink and not reuse it until the sink returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawItem<'a> {
    /// Verbatim UTF-16 name as the filesystem reported it, without the parent directory.
    pub name: &'a [u16],
    pub kind: Kind,
    pub flags: ItemFlags,
    /// 0 for directories, matching what the UI shows and what the arena stores.
    pub size: u64,
    /// `i64::MIN` means unknown.
    pub mtime_ms: i64,
    /// Raw `dwFileAttributes` on Windows; 0 on the portable reader, which has no such thing.
    pub attrs: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnumReport {
    pub entries: u64,
    pub errors: u64,
    /// The stop signal fired instead of the directory running out.
    pub cancelled: bool,
    /// The sink asked to stop, or the reader failed mid-directory, so the tail was never seen.
    pub truncated: bool,
    pub elapsed_ms: u64,
}

impl EnumReport {
    /// Only a complete read may be cached: a partial table would look like a directory that lost
    /// its tail.
    pub fn complete(&self) -> bool {
        !self.cancelled && !self.truncated
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnumError {
    #[error("cannot read directory: {0}")]
    Io(#[from] std::io::Error),
}

/// Reads exactly one level of `dir`, pushing each entry to `sink`. Returning `false` from the sink
/// stops the enumeration; `cancel` is polled between entries.
///
/// On Windows this goes through `FindFirstFileExW`/`FindNextFileW`, which reports size, timestamps
/// and attributes from the same enumeration record. Reparse points (symlinks, junctions) are
/// reported but must never be descended into by a cache-building caller, which keeps cycles and
/// reparse storms out of the index.
pub fn list_one_level(
    dir: &Path,
    cancel: &Cancellation,
    sink: &mut dyn FnMut(RawItem<'_>) -> bool,
) -> Result<EnumReport, EnumError> {
    #[cfg(windows)]
    {
        crate::win32::list_one_level(dir, cancel, sink)
    }
    #[cfg(not(windows))]
    {
        crate::portable::list_one_level(dir, cancel, sink)
    }
}
