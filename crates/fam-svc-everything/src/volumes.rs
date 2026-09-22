//! Which volumes an outside index can actually answer for.
//!
//! Everything indexes local fixed NTFS volumes. That is a property of the *volume*, not of the
//! service, and it can be checked without one - which matters here, because the development machine
//! has no Everything installed and the routing decision still has to be tested rather than assumed.

use std::path::PathBuf;

use fam_core::{PathKey, SearchRequest};

/// `DRIVE_FIXED`, as `GetDriveTypeW` reports it. Network, removable, RAM and optical drives are all
/// something else, and Everything indexes none of them.
const DRIVE_FIXED: u32 = 3;

/// The decision, as a function of the two facts a volume reports.
///
/// Kept apart from the calls that fetch them so the table is testable: a network share that happens
/// to serve NTFS is not indexed, and a fixed volume formatted exFAT or ReFS is not either.
pub fn indexes_volume(drive_type: u32, filesystem: &str) -> bool {
    drive_type == DRIVE_FIXED && filesystem.eq_ignore_ascii_case("NTFS")
}

/// The volume roots an index covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Volumes {
    roots: Vec<PathKey>,
}

impl Volumes {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().map(|root| PathKey::of(&root)).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn roots(&self) -> &[PathKey] {
        &self.roots
    }

    /// Whether `path` sits on one of them.
    pub fn contains(&self, path: &PathKey) -> bool {
        self.roots.iter().any(|root| root.is_within(path))
    }

    /// Whether every path in `wanted` sits on one of them.
    ///
    /// Vacuously true for nothing wanted, which is the right answer for a caller that configured no
    /// roots to search: there is no scope there to fail to cover.
    pub fn covers_all(&self, wanted: &[PathKey]) -> bool {
        wanted.iter().all(|path| self.contains(path))
    }
}

/// Whether an index over `indexed` can answer `request` in full.
///
/// A scoped request is one subtree, and a subtree lives on one volume. An unscoped one stands for
/// every root the caller configured, and all of them have to be inside the index: answering for the
/// NTFS roots and quietly missing the exFAT one would produce a shorter list that claims to be the
/// whole answer, which is worse than a slower search that is not.
pub fn covers(indexed: &Volumes, roots: &[PathKey], request: &SearchRequest) -> bool {
    match &request.scope {
        Some(scope) => indexed.contains(&PathKey::of(scope)),
        None => indexed.covers_all(roots),
    }
}

/// The volume roots on this machine that an outside index could be expected to cover.
#[cfg(windows)]
pub fn scan() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| {
            let letter = char::from_u32(u32::from(b'A') + bit)?;
            let root = format!("{letter}:\\");
            if unsafe { GetDriveTypeW(wide(&root).as_ptr()) } != DRIVE_FIXED {
                return None;
            }
            let mut serial = 0u32;
            let mut component = 0u32;
            let mut flags = 0u32;
            // 32 wchars is what the API's own documentation reserves for the name field; the
            // filesystem name never approaches it.
            let mut name = [0u16; 32];
            let answered = unsafe {
                GetVolumeInformationW(
                    wide(&root).as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    &mut serial,
                    &mut component,
                    &mut flags,
                    name.as_mut_ptr(),
                    name.len() as u32,
                )
            };
            if answered == 0 {
                return None;
            }
            let end = name
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(name.len());
            let filesystem = String::from_utf16_lossy(&name[..end]);
            indexes_volume(DRIVE_FIXED, &filesystem).then_some(PathBuf::from(root))
        })
        .collect()
}

#[cfg(not(windows))]
pub fn scan() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn only_local_fixed_ntfs_is_indexed() {
        assert!(indexes_volume(DRIVE_FIXED, "NTFS"));
        // The name comes back upper-cased from the API, but the comparison is not case-sensitive.
        assert!(indexes_volume(DRIVE_FIXED, "ntfs"));
        for other in ["exFAT", "FAT32", "ReFS", "CDFS", ""] {
            assert!(
                !indexes_volume(DRIVE_FIXED, other),
                "{other} is not NTFS and is not indexed"
            );
        }
    }

    #[test]
    fn ntfs_on_any_other_kind_of_drive_is_still_not_indexed() {
        // 1 DRIVE_REMOVABLE, 2 DRIVE_REMOTE, 4 DRIVE_CDROM, 5 DRIVE_RAMDISK.
        for kind in [1, 2, 4, 5] {
            assert!(
                !indexes_volume(kind, "NTFS"),
                "drive type {kind} is not a local fixed volume"
            );
        }
    }

    #[test]
    fn a_volume_root_covers_its_whole_subtree() {
        let volumes = Volumes::new([PathBuf::from("C:\\"), PathBuf::from("D:\\")]);
        assert!(volumes.contains(&PathKey::of(Path::new("D:/docs/a/b.txt"))));
        assert!(!volumes.contains(&PathKey::of(Path::new("E:/games/steam"))));
        assert!(
            volumes.contains(&PathKey::of(Path::new("D:"))),
            "a volume written with and without its separator is the same volume: `PathKey` folds \
             the trailing one away, so both spellings must agree about being covered"
        );
    }

    #[test]
    fn a_shared_prefix_is_not_a_volume_root() {
        let volumes = Volumes::new([PathBuf::from("D:\\doc")]);
        assert!(!volumes.contains(&PathKey::of(Path::new("D:/docs/a.txt"))));
    }

    #[test]
    fn covering_everything_means_covering_each_root() {
        let volumes = Volumes::new([PathBuf::from("D:\\")]);
        let both = vec![
            PathKey::of(Path::new("D:/a")),
            PathKey::of(Path::new("E:/b")),
        ];
        assert!(
            !volumes.covers_all(&both),
            "one root is on a volume nobody indexes"
        );
        assert!(volumes.covers_all(&both[..1]));
        assert!(
            volumes.covers_all(&[]),
            "nothing asked for is nothing left uncovered"
        );
        assert!(Volumes::default().is_empty());
    }

    #[test]
    fn a_scoped_request_is_covered_by_the_volume_under_it() {
        let indexed = Volumes::new([PathBuf::from("D:\\")]);
        let roots = vec![PathKey::of(Path::new("D:/docs"))];
        let inside = SearchRequest::new("x").scope(PathBuf::from("D:/docs/deep"));
        let outside = SearchRequest::new("x").scope(PathBuf::from("E:/games"));
        assert!(covers(&indexed, &roots, &inside));
        assert!(!covers(&indexed, &roots, &outside));
    }

    #[test]
    fn an_unscoped_request_needs_every_configured_root_covered() {
        let indexed = Volumes::new([PathBuf::from("D:\\")]);
        let one = vec![PathKey::of(Path::new("D:/docs"))];
        let two = vec![
            PathKey::of(Path::new("D:/docs")),
            PathKey::of(Path::new("E:/games")),
        ];
        let unscoped = SearchRequest::new("x");
        assert!(covers(&indexed, &one, &unscoped));
        assert!(
            !covers(&indexed, &two, &unscoped),
            "one root sits on a volume nobody indexes, so the answer would be silently short"
        );
    }

    /// A fact about this machine, printed rather than asserted: whatever the drives are, the scan
    /// must return roots in the form the rest of the code compares by, and never a path that the
    /// decision table would reject.
    #[test]
    #[cfg(windows)]
    fn the_scan_returns_volume_roots_only() {
        let found = scan();
        println!("indexed volume roots on this machine: {found:?}");
        for root in &found {
            let text = root.to_string_lossy();
            assert!(
                text.ends_with(":\\") && text.len() == 3,
                "{text} is not a volume root"
            );
        }
    }
}
