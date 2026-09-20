//! Win32 one-level enumeration: `FindFirstFileExW` / `FindNextFileW`.
//!
//! One call chain per directory yields name, attributes, size and timestamp for every entry. That
//! is the whole point of this module: the std reader it replaces paid two syscalls per entry
//! (`read_dir` for the name, then `DirEntry::metadata` for everything else), which is what made a
//! 78k-entry listing cost ~2.9us per entry no matter how the surrounding code was tuned.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, WIN32_FIND_DATAW,
};

use fam_core::{classify_units, Cancellation, ItemFlags};

use crate::lister::{EnumError, EnumReport, RawItem};

const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const DOT: u16 = b'.' as u16;
const COLON: u16 = b':' as u16;
const STAR: u16 = b'*' as u16;
/// `\\?\` — the verbatim prefix that switches off `MAX_PATH` and all parser magic.
const VERBATIM: [u16; 4] = [BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH];
const UNC_ALIAS: [u16; 4] = [b'U' as u16, b'N' as u16, b'C' as u16, BACKSLASH];
/// 100ns ticks between the FILETIME epoch (1601-01-01) and the Unix epoch.
const FILETIME_TO_UNIX_100NS: i64 = 116_444_736_000_000_000;

/// Builds the `lpFileName` argument for `dir`: a wide, NUL-terminated `\*` pattern.
///
/// Absolute paths go through `\\?\` so a child whose full path exceeds `MAX_PATH` still enumerates
/// — the API hands back one component at a time, so only the *input* string is length-capped.
/// A verbatim string gets no other treatment from the parser either, so separators must be
/// backslashes. Relative paths keep the ordinary parser, because `\\?\` in front of a relative
/// path is not valid and resolving one would need a current directory this function does not own.
pub(crate) fn search_pattern(dir: &Path) -> Vec<u16> {
    let raw: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .map(|unit| if unit == SLASH { BACKSLASH } else { unit })
        .collect();

    let mut units = if raw.starts_with(&VERBATIM) {
        raw
    } else if raw.starts_with(&[BACKSLASH, BACKSLASH]) {
        // UNC: "\\server\share" is spelled "\\?\UNC\server\share" verbatim.
        let mut prefixed = Vec::with_capacity(raw.len() + 4);
        prefixed.extend_from_slice(&VERBATIM);
        prefixed.extend_from_slice(&UNC_ALIAS);
        prefixed.extend_from_slice(&raw[2..]);
        prefixed
    } else if is_drive_absolute(&raw) {
        let mut prefixed = Vec::with_capacity(raw.len() + 4);
        prefixed.extend_from_slice(&VERBATIM);
        prefixed.extend_from_slice(&raw);
        prefixed
    } else {
        raw
    };

    // The wildcard is appended after trimming, so "D:\FAM\" and "D:\FAM" both land on
    // "D:\FAM\*"; a bare drive gets its root separator from the same append.
    while units.last() == Some(&BACKSLASH) {
        units.pop();
    }
    units.push(BACKSLASH);
    units.push(STAR);
    units.push(0);
    units
}

fn is_drive_absolute(units: &[u16]) -> bool {
    // Setting bit 5 folds 'A'..'Z' onto 'a'..'z' and pushes everything else out of that range,
    // because `u16` has no `is_ascii_alphabetic`.
    units.len() >= 2 && matches!(units[0] | 0x20, 0x61..=0x7a) && units[1] == COLON
}

fn name_of(record: &WIN32_FIND_DATAW) -> &[u16] {
    let end = record
        .cFileName
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(record.cFileName.len());
    &record.cFileName[..end]
}

/// "." and ".." are part of every `FindFirstFileExW` result; a directory listing must not contain
/// them, and descending into ".." would walk the tree back up.
fn is_self_or_parent(name: &[u16]) -> bool {
    name == [DOT] || name == [DOT, DOT]
}

fn flags_for(attributes: u32) -> ItemFlags {
    let mut flags = ItemFlags::default();
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        flags = flags.union(ItemFlags::DIRECTORY);
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        flags = flags.union(ItemFlags::REPARSE);
    }
    flags
}

fn file_size(record: &WIN32_FIND_DATAW) -> u64 {
    ((record.nFileSizeHigh as u64) << 32) | record.nFileSizeLow as u64
}

/// Milliseconds since the Unix epoch, or `i64::MIN` when the entry carries no timestamp.
fn epoch_ms(stamp: FILETIME) -> i64 {
    let ticks = ((stamp.dwHighDateTime as u64) << 32) | stamp.dwLowDateTime as u64;
    if ticks == 0 {
        return i64::MIN;
    }
    (ticks as i64 - FILETIME_TO_UNIX_100NS).div_euclid(10_000)
}

pub(crate) fn list_one_level(
    dir: &Path,
    cancel: &Cancellation,
    sink: &mut dyn FnMut(RawItem<'_>) -> bool,
) -> Result<EnumReport, EnumError> {
    let started = Instant::now();
    let mut report = EnumReport::default();
    let pattern = search_pattern(dir);
    let mut record = WIN32_FIND_DATAW::default();

    // SAFETY: `pattern` is NUL-terminated and outlives the call; the third argument is a writable
    // `WIN32_FIND_DATAW`, which is what `FindExInfoBasic` fills.
    let handle: HANDLE = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            &mut record as *mut WIN32_FIND_DATAW as *mut core::ffi::c_void,
            FindExSearchNameMatch,
            core::ptr::null(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(EnumError::Io(std::io::Error::last_os_error()));
    }

    loop {
        if cancel.is_stopped() {
            report.cancelled = true;
            break;
        }
        let name = name_of(&record);
        if !is_self_or_parent(name) {
            let attributes = record.dwFileAttributes;
            let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            report.entries += 1;
            let keep_going = sink(RawItem {
                name,
                kind: classify_units(name, is_directory),
                flags: flags_for(attributes),
                size: if is_directory { 0 } else { file_size(&record) },
                mtime_ms: epoch_ms(record.ftLastWriteTime),
                attrs: attributes,
            });
            if !keep_going {
                report.truncated = true;
                break;
            }
        }
        // SAFETY: `handle` is the live search handle and `record` is writable.
        if unsafe { FindNextFileW(handle, &mut record) } == 0 {
            // "No more files" and a genuine failure look identical in the return value; only the
            // last-error code tells them apart, and a mid-directory failure means the read is
            // incomplete just as much as a cancellation does.
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                report.errors += 1;
                report.truncated = true;
            }
            break;
        }
    }

    // SAFETY: closing the handle this function opened, on every path out of the loop.
    unsafe { FindClose(handle) };
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::ERROR_PATH_NOT_FOUND;

    use super::*;

    /// The pattern as text, minus the terminating NUL the API needs.
    fn pattern(text: &str) -> String {
        let units = search_pattern(Path::new(text));
        String::from_utf16(&units[..units.len() - 1]).unwrap()
    }

    #[test]
    fn absolute_paths_become_verbatim_with_a_wildcard() {
        assert_eq!(pattern(r"D:\FAM\Docs"), r"\\?\D:\FAM\Docs\*");
        assert_eq!(pattern(r"D:/FAM/Docs"), r"\\?\D:\FAM\Docs\*");
        assert_eq!(pattern(r"D:\FAM\Docs\"), r"\\?\D:\FAM\Docs\*");
    }

    #[test]
    fn a_drive_root_gets_exactly_one_separator() {
        assert_eq!(pattern(r"D:\"), r"\\?\D:\*");
        assert_eq!(pattern("D:"), r"\\?\D:\*");
    }

    #[test]
    fn unc_roots_move_under_the_unc_alias() {
        assert_eq!(
            pattern(r"\\server\share\FAM"),
            r"\\?\UNC\server\share\FAM\*"
        );
    }

    #[test]
    fn already_verbatim_input_is_not_prefixed_twice() {
        assert_eq!(pattern(r"\\?\D:\FAM"), r"\\?\D:\FAM\*");
    }

    #[test]
    fn relative_paths_keep_the_ordinary_parser() {
        assert_eq!(pattern("FAM/Docs"), r"FAM\Docs\*");
    }

    #[test]
    fn the_pattern_always_ends_nul_terminated() {
        let units = search_pattern(Path::new(r"D:\FAM"));
        assert_eq!(*units.last().unwrap(), 0);
        assert!(!units[..units.len() - 1].contains(&0));
    }

    #[test]
    fn attribute_bits_map_onto_the_shared_flag_set() {
        let plain = flags_for(0);
        assert!(!plain.is_directory() && !plain.is_reparse());

        let dir = flags_for(FILE_ATTRIBUTE_DIRECTORY);
        assert!(dir.is_directory() && !dir.is_reparse());

        // A directory symlink or junction carries both, and callers must descend on neither.
        let junction = flags_for(FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT);
        assert!(junction.is_directory() && junction.is_reparse());
    }

    #[test]
    fn filetimes_convert_and_a_zero_stamp_means_unknown() {
        let stamp = |ticks: u64| FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        assert_eq!(epoch_ms(stamp(FILETIME_TO_UNIX_100NS as u64)), 0);
        assert_eq!(
            epoch_ms(stamp(FILETIME_TO_UNIX_100NS as u64 + 12_345 * 10_000)),
            12_345
        );
        // A file dated before 1970 stays negative rather than wrapping.
        assert_eq!(
            epoch_ms(stamp(FILETIME_TO_UNIX_100NS as u64 - 5_000 * 10_000)),
            -5_000
        );
        assert_eq!(epoch_ms(stamp(0)), i64::MIN);
    }

    #[test]
    fn self_and_parent_are_recognised_but_a_dotted_name_is_not() {
        assert!(is_self_or_parent(&[DOT]));
        assert!(is_self_or_parent(&[DOT, DOT]));
        assert!(!is_self_or_parent(&[DOT, DOT, DOT]));
        assert!(!is_self_or_parent(&[
            DOT,
            b'g' as u16,
            b'i' as u16,
            b't' as u16
        ]));
    }

    #[test]
    fn every_entry_the_win32_reader_reports_matches_the_portable_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("photo.PNG"), "xyz").unwrap();
        std::fs::write(dir.path().join("Makefile"), "x").unwrap();
        std::fs::write(dir.path().join("数据.bin"), "0123456789").unwrap();
        std::fs::create_dir(dir.path().join("子 目录")).unwrap();

        let mut win = Vec::new();
        let win_report = list_one_level(dir.path(), &Cancellation::new(), &mut |item| {
            win.push(Shape::of(&item));
            true
        })
        .unwrap();

        let mut std_shapes = Vec::new();
        let std_report =
            crate::portable::list_one_level(dir.path(), &Cancellation::new(), &mut |item| {
                std_shapes.push(Shape::of(&item));
                true
            })
            .unwrap();

        win.sort();
        std_shapes.sort();
        assert_eq!(win_report.entries, 5);
        assert_eq!(win_report.errors, std_report.errors);
        assert_eq!(win_report.entries, std_report.entries);
        assert_eq!(win, std_shapes);
    }

    /// The fields both readers must agree on. `attrs` is deliberately left out: only the Win32
    /// reader has a real `dwFileAttributes` to report.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Shape {
        name: String,
        kind: fam_core::Kind,
        is_directory: bool,
        is_reparse: bool,
        size: u64,
        mtime_ms: i64,
    }

    impl Shape {
        fn of(item: &RawItem) -> Self {
            Self {
                name: String::from_utf16_lossy(item.name),
                kind: item.kind,
                is_directory: item.flags.is_directory(),
                is_reparse: item.flags.is_reparse(),
                size: item.size,
                mtime_ms: item.mtime_ms,
            }
        }
    }

    /// The walk's no-cycles rule depends on the reparse bit, so it has to be set by whichever
    /// reader is in use. Measured rather than assumed: `std` maps `IO_REPARSE_TAG_MOUNT_POINT` onto
    /// `FileType::is_symlink`, so the portable reader sees junctions too — what the Win32 record
    /// adds is the directory bit, which is why a junction lists as a folder now instead of as an
    /// extensionless file.
    #[test]
    fn a_junction_is_flagged_as_a_reparse_point_by_both_readers() {
        let (dir, _root) = create_junction();

        let seen = |list: &mut Vec<(String, ItemFlags)>, name: &str| {
            list.iter()
                .find(|(entry, _)| entry == name)
                .unwrap_or_else(|| panic!("{name} was not listed"))
                .1
        };

        let mut win = Vec::new();
        list_one_level(&dir, &Cancellation::new(), &mut |item| {
            win.push((String::from_utf16_lossy(item.name), item.flags));
            true
        })
        .unwrap();
        let mut portable = Vec::new();
        crate::portable::list_one_level(&dir, &Cancellation::new(), &mut |item| {
            portable.push((String::from_utf16_lossy(item.name), item.flags));
            true
        })
        .unwrap();

        let win_link = seen(&mut win, "link");
        let portable_link = seen(&mut portable, "link");
        assert!(
            win_link.is_reparse() && portable_link.is_reparse(),
            "both readers have to surface the reparse bit, because the walk's no-cycle rule is \
             written against it: win={win_link:?} portable={portable_link:?}"
        );
        assert!(
            win_link.is_directory() && !portable_link.is_directory(),
            "the enumeration record calls a junction a directory and `DirEntry::metadata` does \
             not, so the two readers disagree on how it lists: win={win_link:?} \
             portable={portable_link:?}"
        );
    }

    fn create_junction() -> (PathBuf, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside.txt"), "x").unwrap();
        let link = root.path().join("link");
        // `mklink /J` needs no elevation; PowerShell's junction type maps to the same call.
        let made = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "New-Item",
                "-ItemType",
                "Junction",
                "-Path",
                &link.to_string_lossy(),
                "-Target",
                &target.to_string_lossy(),
            ])
            .output()
            .map(|output| output.status.success());
        assert!(
            made.unwrap_or(false),
            "cannot create a junction on this machine, so this test cannot run"
        );
        (root.path().to_path_buf(), root)
    }

    #[test]
    fn reading_a_missing_directory_is_an_error() {
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        assert!(matches!(
            list_one_level(&missing, &Cancellation::new(), &mut |_| true),
            Err(EnumError::Io(_))
        ));
    }

    /// A directory whose full path outgrows `MAX_PATH` while every component stays short. The
    /// `TempDir` comes back with it because dropping it would delete the tree the test reads.
    fn deep_tree(levels: usize) -> (PathBuf, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().to_path_buf();
        for level in 0..levels {
            deep = deep.join(format!("directory-{level:02}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        (deep, root)
    }

    /// The negative control for `a_child_beyond_max_path_is_still_enumerated`: without the
    /// verbatim prefix the same directory cannot be opened at all, which is what makes the prefix
    /// load-bearing rather than decorative. Rust's executable manifest does not declare
    /// `longPathAware`, so `MAX_PATH` keeps applying to plain strings.
    #[test]
    fn a_plain_pattern_cannot_reach_that_depth() {
        let (deep, _root) = deep_tree(24);

        let mut units: Vec<u16> = deep
            .as_os_str()
            .encode_wide()
            .map(|unit| if unit == SLASH { BACKSLASH } else { unit })
            .collect();
        let length = units.len();
        units.push(BACKSLASH);
        units.push(STAR);
        units.push(0);

        let mut record = WIN32_FIND_DATAW::default();
        // SAFETY: `units` is NUL-terminated and `record` is writable.
        let handle = unsafe {
            FindFirstFileExW(
                units.as_ptr(),
                FindExInfoBasic,
                &mut record as *mut WIN32_FIND_DATAW as *mut core::ffi::c_void,
                FindExSearchNameMatch,
                core::ptr::null(),
                0,
            )
        };
        // Last error has to be read before any other Win32 call, `FindClose` included.
        let error = std::io::Error::last_os_error();
        if handle != INVALID_HANDLE_VALUE {
            unsafe { FindClose(handle) };
        }
        assert!(
            length > 260,
            "the tree must outgrow MAX_PATH for this test to prove anything, got {length} units"
        );
        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_PATH_NOT_FOUND as i32),
            "expected the {length}-unit unprefixed path to be rejected, got {error:?}"
        );
    }

    #[test]
    fn a_child_beyond_max_path_is_still_enumerated() {
        let (deep, _root) = deep_tree(24);
        std::fs::write(deep.join("needle.txt"), "x").unwrap();

        let mut found = Vec::new();
        let report = list_one_level(&deep, &Cancellation::new(), &mut |item| {
            found.push(String::from_utf16_lossy(item.name));
            true
        })
        .unwrap();

        assert_eq!(found, vec!["needle.txt"]);
        assert!(report.complete());
    }
}
