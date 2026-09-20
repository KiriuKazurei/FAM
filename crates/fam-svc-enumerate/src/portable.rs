//! Reader built on `std::fs`. On Windows it exists for the differential test in `win32.rs` to have
//! something to disagree with; the platform path is the one production uses, because this shape
//! pays two syscalls per entry — `read_dir` for the name and a separate stat for everything else.

use std::fs;
use std::path::Path;
use std::time::Instant;

use fam_core::{classify, raw_name_units, to_epoch_ms, Cancellation, ItemFlags};

use crate::lister::{EnumError, EnumReport, RawItem};

pub(crate) fn list_one_level(
    dir: &Path,
    cancel: &Cancellation,
    sink: &mut dyn FnMut(RawItem<'_>) -> bool,
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
        if item
            .file_type()
            .map(|file_type| file_type.is_symlink())
            .unwrap_or(false)
        {
            flags = flags.union(ItemFlags::REPARSE);
        }
        let name = raw_name_units(&path);
        report.entries += 1;
        let keep_going = sink(RawItem {
            name: &name,
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
    use fam_core::Kind;

    fn names(dir: &Path) -> (Vec<String>, EnumReport) {
        let mut collected = Vec::new();
        let report = list_one_level(dir, &Cancellation::new(), &mut |item| {
            collected.push(String::from_utf16_lossy(item.name));
            true
        })
        .unwrap();
        collected.sort();
        (collected, report)
    }

    #[test]
    fn lists_one_level_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("top.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("nested/deeper")).unwrap();
        std::fs::write(dir.path().join("nested/inner.txt"), "x").unwrap();

        let (collected, report) = names(dir.path());

        assert_eq!(report.entries, 2, "only this level, not nested/inner.txt");
        assert_eq!(report.errors, 0);
        assert!(report.complete());
        assert_eq!(collected, vec!["nested", "top.txt"]);
    }

    #[test]
    fn directories_carry_the_directory_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("file.png"), "x").unwrap();

        let mut items = Vec::new();
        list_one_level(dir.path(), &Cancellation::new(), &mut |item| {
            items.push((
                String::from_utf16_lossy(item.name),
                item.kind,
                item.flags,
                item.size,
            ));
            true
        })
        .unwrap();

        let sub = items.iter().find(|(name, ..)| name == "sub").unwrap();
        assert!(sub.2.is_directory());
        assert_eq!(sub.1, Kind::Directory);

        let file = items.iter().find(|(name, ..)| name == "file.png").unwrap();
        assert!(!file.2.is_directory());
        assert_eq!(file.1, Kind::Image);
        assert_eq!(file.3, 1);
    }

    #[test]
    fn a_missing_directory_is_an_error_not_an_empty_report() {
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        assert!(matches!(
            list_one_level(&missing, &Cancellation::new(), &mut |_| true),
            Err(EnumError::Io(_))
        ));
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
}
