use std::path::Path;

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
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&self.0))
        }
        #[cfg(not(windows))]
        {
            std::path::PathBuf::from(self.to_lossy_string())
        }
    }
}

/// Verbatim UTF-16 units of a path's final component, unfolded — what a search hit carries so the
/// shell can display exactly what the filesystem reported.
pub fn raw_name_units(path: &Path) -> Vec<u16> {
    match path.file_name() {
        Some(name) => units_of(std::path::Path::new(name)),
        None => Vec::new(),
    }
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
        if *unit == BACKSLASH {
            *unit = SLASH;
        } else if matches!(*unit, ASCII_A..=ASCII_Z) {
            *unit -= ASCII_CASE_DELTA;
        }
    }
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
}
