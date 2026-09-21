use std::path::{Path, PathBuf};

const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const ASCII_A: u16 = b'a' as u16;
const ASCII_Z: u16 = b'z' as u16;
const ASCII_CASE_DELTA: u16 = b'a' as u16 - b'A' as u16;
const VERBATIM: [u16; 4] = [BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH];
const DEVICE_NAMESPACE: [u16; 4] = [BACKSLASH, BACKSLASH, b'.' as u16, BACKSLASH];

/// Exact, case-folded identity for a path, stored as UTF-16 code units.
///
/// NTFS names may contain lone surrogates, which cannot survive a Rust `String`: the old
/// `to_string_lossy` based key silently mapped them to U+FFFD, so two distinct files could share
/// one identity. `str::to_uppercase`, which the old key also used, had the same defect from the
/// other direction — it maps `ß` to `SS` and `ﬁ` to `FI`, merging names NTFS keeps apart.
///
/// The fold is therefore deliberately ASCII-only: separators unify, `a-z` upper-cases, everything
/// else is carried verbatim. When that is imprecise it splits one file into two keys (`café` and
/// `CAFÉ` take two cache slots) instead of merging two files into one, which is the only direction
/// that can misattribute a tag. Folding the rest of Unicode needs NTFS's upcase table, so it
/// belongs in a platform layer rather than here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathKey(Vec<u16>);

impl PathKey {
    pub fn of(path: &Path) -> Self {
        let mut units = units_of(path);
        fold(&mut units);
        Self(units)
    }

    /// Appends a raw name (as the filesystem reported it) without re-walking the parent.
    pub fn child_units(&self, name: &[u16]) -> Self {
        let mut units = Vec::with_capacity(self.0.len() + name.len() + 1);
        units.extend_from_slice(&self.0);
        units.push(SLASH);
        units.extend_from_slice(name);
        fold_name_only(&mut units[self.0.len() + 1..]);
        Self(units)
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    /// Display/wire form. Lossy by construction; never use it as an identity for new code.
    pub fn to_lossy_string(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }

    pub fn parent(&self) -> Option<PathKey> {
        let cut = self.0.iter().rposition(|unit| *unit == SLASH)?;
        if cut == 0 {
            return Some(Self(vec![SLASH]));
        }
        Some(Self(self.0[..cut].to_vec()))
    }

    pub fn name_units(&self) -> &[u16] {
        match self.0.iter().rposition(|unit| *unit == SLASH) {
            Some(cut) => &self.0[cut + 1..],
            None => &self.0,
        }
    }

    pub fn is_within(&self, child: &PathKey) -> bool {
        is_within_units(&self.0, &child.0)
    }

    pub fn to_path_buf(&self) -> std::path::PathBuf {
        path_buf_from_units(&self.0)
    }
}

/// Rebuilds a path from raw UTF-16 units. Exact on Windows, where that is what the filesystem
/// speaks; lossy elsewhere, where a `PathBuf` is bytes and only this project's test targets run.
///
/// Enumeration hands out entry names as units, so joining them onto a parent directory has to go
/// through here — routing a name through `String::from_utf16_lossy` first would rewrite every lone
/// surrogate into U+FFFD and produce a path that names a different file.
pub fn path_buf_from_units(units: &[u16]) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(units))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(String::from_utf16_lossy(units))
    }
}

/// Verbatim UTF-16 units of a path's final component, unfolded — what a directory row's name and a
/// change event's name carry, so the shell can display exactly what the filesystem reported.
pub fn raw_name_units(path: &Path) -> Vec<u16> {
    match path.file_name() {
        Some(name) => units_of(std::path::Path::new(name)),
        None => Vec::new(),
    }
}

/// Verbatim UTF-16 units of a whole path, separators as the platform writes them.
///
/// A search result set spans directories, so its rows have to carry the location, not just the name.
/// Taking that from [`PathKey`] would print `c:\users\…` for a folder the user calls `C:\Users\…`:
/// the fold lower-cases ASCII because two spellings are one *identity*, which is a different question
/// from what the filesystem reported.
pub fn raw_path_units(path: &Path) -> Vec<u16> {
    units_of(path)
}

fn units_of(path: &Path) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().collect()
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().encode_utf16().collect()
    }
}

fn fold(units: &mut Vec<u16>) {
    if units.starts_with(&VERBATIM) || units.starts_with(&DEVICE_NAMESPACE) {
        units.drain(..VERBATIM.len());
    }
    fold_name_only(units);
    while units.len() > 1 && *units.last().expect("non-empty") == SLASH {
        units.pop();
    }
}

fn fold_name_only(units: &mut [u16]) {
    for unit in units.iter_mut() {
        *unit = fold_unit(*unit);
    }
}

/// The one-unit case of the fold: separators unify, `a-z` upper-cases, everything else is verbatim.
fn fold_unit(unit: u16) -> u16 {
    if unit == BACKSLASH {
        SLASH
    } else if (ASCII_A..=ASCII_Z).contains(&unit) {
        unit - ASCII_CASE_DELTA
    } else {
        unit
    }
}

/// Whether two raw UTF-16 names are the same name to the filesystem.
///
/// A change event reports a name and a cached table stores a name, and the two may differ in ASCII
/// case: `Foo.txt` is created by a program that asked for `FOO.TXT`, and NTFS keeps one file.
/// Comparing them byte-for-byte would then fail to take the row out of the view and leave a file on
/// screen that no longer exists. Same rule [`PathKey`] folds paths with, which is why it lives here
/// rather than being re-derived in the cache.
pub fn name_units_match(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| fold_unit(*left) == fold_unit(*right))
}

/// A name reduced to the form [`name_units_match`] compares. For a *set* of names, folding once per
/// name and keying a map beats comparing every candidate pair.
pub fn fold_name_units(name: &[u16]) -> Vec<u16> {
    name.iter().map(|unit| fold_unit(*unit)).collect()
}

fn is_within_units(parent: &[u16], child: &[u16]) -> bool {
    if child.len() < parent.len() {
        return false;
    }
    if child[..parent.len()] != *parent {
        return false;
    }
    child.len() == parent.len() || parent.last() == Some(&SLASH) || child[parent.len()] == SLASH
}

/// Legacy string form of [`PathKey`], kept because ten call sites and the tag table's primary key
/// are built on it. Lossy by construction — use [`PathKey`] for anything that must distinguish
/// names the filesystem distinguishes.
pub fn path_key(path: &Path) -> String {
    PathKey::of(path).to_lossy_string()
}

/// True when `child_key` is `parent_key` itself or nested under it. Takes already-computed keys.
pub fn is_within(parent_key: &str, child_key: &str) -> bool {
    let parent: Vec<u16> = parent_key.encode_utf16().collect();
    let child: Vec<u16> = child_key.encode_utf16().collect();
    is_within_units(&parent, &child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_case_normalise_to_one_key() {
        assert_eq!(
            path_key(Path::new(r"D:\FAM\Docs")),
            path_key(Path::new("d:/fam/docs"))
        );
        assert_eq!(path_key(Path::new(r"D:\FAM\Docs")), "D:/FAM/DOCS");
    }

    #[test]
    fn non_ascii_case_never_merges_two_names() {
        // `str::to_uppercase`, which the old key used, maps ß to SS and gave "straße" and
        // "STRASSE" a single identity even though NTFS keeps them apart. The ASCII-only fold
        // restores that distinction; the price is that é and É stay distinct too, which splits one
        // directory across two cache slots rather than merging two files into one key.
        assert_ne!(
            path_key(Path::new(r"D:\straße")),
            path_key(Path::new(r"D:\STRASSE"))
        );
        assert_ne!(
            path_key(Path::new(r"D:\café")),
            path_key(Path::new(r"D:\CAFÉ"))
        );
        assert_eq!(
            path_key(Path::new(r"D:\照片")),
            path_key(Path::new("d:/照片"))
        );
    }

    #[test]
    fn trailing_separators_collapse() {
        assert_eq!(path_key(Path::new("D:/FAM/")), "D:/FAM");
        assert_eq!(path_key(Path::new(r"D:")), "D:");
        assert_eq!(path_key(Path::new(r"D:\")), "D:");
        assert_eq!(path_key(Path::new("/")), "/");
    }

    #[test]
    fn device_prefixes_are_stripped() {
        assert_eq!(path_key(Path::new(r"\\?\D:\FAM")), "D:/FAM");
        assert_eq!(path_key(Path::new(r"\\.\D:\FAM")), "D:/FAM");
    }

    #[test]
    fn sibling_with_shared_prefix_is_not_inside() {
        assert!(is_within("D:/FOO/BAR", "D:/FOO/BAR"));
        assert!(is_within("D:/FOO/BAR", "D:/FOO/BAR/FILE.TXT"));
        assert!(!is_within("D:/FOO/BAR", "D:/FOO/BARS"));
        assert!(!is_within("D:/FOO/BAR", "D:/FOO/BA"));
    }

    #[test]
    fn drive_and_unix_roots_contain_their_children() {
        assert!(is_within("D:", "D:/FAM/DOCS"));
        assert!(is_within("/", "/FAM/DOCS"));
        assert!(!is_within("/FAM", "/FAMILY"));
    }

    #[test]
    fn child_appends_without_rewalking() {
        let parent = PathKey::of(Path::new(r"D:\fam"));
        let child = parent.child_units(&"Docs".encode_utf16().collect::<Vec<u16>>());
        assert_eq!(child.to_lossy_string(), "D:/FAM/DOCS");
        assert_eq!(child.parent(), Some(parent.clone()));
        assert_eq!(
            child.name_units(),
            "DOCS".encode_utf16().collect::<Vec<u16>>()
        );
    }

    #[cfg(windows)]
    #[test]
    fn lone_surrogates_keep_distinct_identities() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let base: Vec<u16> = r"D:\weird\".encode_utf16().collect();
        let mut first = base.clone();
        first.extend([0xD800, b'a' as u16]);
        let mut second = base.clone();
        second.extend([0xD801, b'a' as u16]);

        let first_key = PathKey::of(Path::new(&OsString::from_wide(&first)));
        let second_key = PathKey::of(Path::new(&OsString::from_wide(&second)));

        assert_ne!(first_key, second_key, "identity must not collide");
        assert_eq!(
            first_key.to_lossy_string(),
            second_key.to_lossy_string(),
            "the lossy string still collides, which is why it is display-only"
        );
    }

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    #[test]
    fn names_match_where_ntfs_would_and_only_where_it_would() {
        assert!(name_units_match(&units("Report.PDF"), &units("report.pdf")));
        assert!(
            name_units_match(&units("a\\b"), &units("a/b")),
            "a name has no separator to begin with, but the fold is one rule for all inputs"
        );
        assert!(name_units_match(&units("照片"), &units("照片")));
        assert!(!name_units_match(&units("café"), &units("CAFÉ")));
        assert!(!name_units_match(&units("straße"), &units("STRASSE")));
        assert!(!name_units_match(&units("a.txt"), &units("a.txtx")));
        assert!(!name_units_match(&units("a.txt"), &units("a.tx")));
    }

    /// The compared form and the folded form are two ways of stating one rule, and the cache uses
    /// both - a hash bucket built from the fold, a verification by comparison. If they ever
    /// disagreed, a row could survive an event that named it.
    #[test]
    fn folding_and_comparing_a_name_always_agree() {
        let names = [
            "Report.PDF",
            "report.pdf",
            "REPORT.PDF",
            "café.doc",
            "CAFÉ.doc",
            "straße.md",
            "STRASSE.md",
            "照片.png",
            "a b c.txt",
        ];
        for first in names {
            for second in names {
                assert_eq!(
                    fold_name_units(&units(first)) == fold_name_units(&units(second)),
                    name_units_match(&units(first), &units(second)),
                    "`{first}` against `{second}`"
                );
            }
        }
    }
}
