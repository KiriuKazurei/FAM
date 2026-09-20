use std::fs;
use std::path::Path;
use std::time::Instant;

use fam_core::{classify, raw_name_units, to_epoch_ms, Cancellation, ItemFlags, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawItem {
    /// Verbatim UTF-16 name as the filesystem reported it.
    pub name: Vec<u16>,
    pub kind: Kind,
    pub flags: ItemFlags,
    pub size: u64,
    /// `i64::MIN` means unknown.
    pub mtime_ms: i64,
    /// Raw `dwFileAttributes` once the Win32 reader lands; 0 for now.
    pub attrs: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnumReport {
    pub entries: u64,
    pub errors: u64,
    /// The stop signal fired instead of the directory running out.
    pub cancelled: bool,
    /// The sink asked to stop, so the tail of the directory was never read.
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
/// Reparse points are reported but never descended into by callers that build a cache, which keeps
/// cycles and reparse storms out of the index.
pub fn list_one_level(
    dir: &Path,
    cancel: &Cancellation,
    sink: &mut dyn FnMut(RawItem) -> bool,
) -> Result<EnumReport, EnumError> {
    let started = Instant::now();
    let mut report = EnumReport::default();
    for item in fs::read_dir(dir)? {
        if cancel.is_stopped() {
            report.cancelled = true;
            break;
        }
        let Ok(item) = item else {
            report.errors += 1;
            continue;
        };
        let path = item.path();
        let metadata = match item.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                report.errors += 1;
                continue;
            }
        };
        let is_directory = metadata.is_dir();
        let mut flags = ItemFlags::default();
        if is_directory {
            flags = flags.union(ItemFlags::DIRECTORY);
        }
        if item.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            flags = flags.union(ItemFlags::REPARSE);
        }
        report.entries += 1;
        let keep_going = sink(RawItem {
            name: raw_name_units(&path),
            kind: classify(&path, is_directory),
            flags,
            size: if is_directory { 0 } else { metadata.len() },
            mtime_ms: metadata
                .modified()
                .ok()
                .and_then(to_epoch_ms)
                .unwrap_or(i64::MIN),
            attrs: 0,
        });
        if !keep_going {
            report.truncated = true;
            break;
        }
    }
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_one_level_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("top.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("nested/deeper")).unwrap();
        std::fs::write(dir.path().join("nested/inner.txt"), "x").unwrap();

        let mut names = Vec::new();
        let report = list_one_level(dir.path(), &Cancellation::new(), &mut |item| {
            names.push(String::from_utf16_lossy(&item.name));
            true
        })
        .unwrap();

        assert_eq!(report.entries, 2, "only this level, not nested/inner.txt");
        assert_eq!(report.errors, 0);
        assert!(report.complete());
        names.sort();
        assert_eq!(names, vec!["nested", "top.txt"]);
    }

    #[test]
    fn a_sink_that_stops_early_marks_the_read_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..5 {
            std::fs::write(dir.path().join(format!("f{index}.txt")), "x").unwrap();
        }

        let mut seen = 0;
        let report = list_one_level(dir.path(), &Cancellation::new(), &mut |_| {
            seen += 1;
            seen < 2
        })
        .unwrap();

        assert_eq!(seen, 2);
        assert_eq!(report.entries, 2);
        assert!(!report.cancelled);
        assert!(report.truncated);
        assert!(!report.complete());
    }

    #[test]
    fn directories_carry_the_directory_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("file.png"), "x").unwrap();

        let mut items = Vec::new();
        list_one_level(dir.path(), &Cancellation::new(), &mut |item| {
            items.push(item);
            true
        })
        .unwrap();

        let sub = items
            .iter()
            .find(|item| String::from_utf16_lossy(&item.name) == "sub")
            .unwrap();
        assert!(sub.flags.is_directory());
        assert_eq!(sub.kind, Kind::Directory);

        let file = items
            .iter()
            .find(|item| String::from_utf16_lossy(&item.name) == "file.png")
            .unwrap();
        assert!(!file.flags.is_directory());
        assert_eq!(file.kind, Kind::Image);
        assert_eq!(file.size, 1);
    }

    #[test]
    fn a_missing_directory_is_an_error_not_an_empty_report() {
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        let result = list_one_level(&missing, &Cancellation::new(), &mut |_| true);
        assert!(matches!(result, Err(EnumError::Io(_))));
    }

    #[test]
    fn cancellation_stops_the_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..50 {
            std::fs::write(dir.path().join(format!("f{index:02}.txt")), "x").unwrap();
        }
        let cancel = Cancellation::new();
        cancel.cancel();

        let report = list_one_level(dir.path(), &cancel, &mut |_| true).unwrap();

        assert!(report.cancelled);
        assert!(!report.complete());
        assert_eq!(report.entries, 0);
    }
}
