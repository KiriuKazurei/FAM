use std::path::Path;

const VERBATIM_PREFIX: &str = "\\\\?\\";
const DEVICE_NAMESPACE_PREFIX: &str = "\\\\.\\";

/// Separator- and case-insensitive comparison key for Windows paths.
///
/// Legacy exclusion filters compared a `path::Clean` key against a `filepath::Clean` value and
/// never matched anything (`docs/legacy-specs.md` §2.2-3). Every comparison in FAM goes through
/// this key instead, so callers never hand-roll separators or casing.
pub fn path_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let raw = raw
        .strip_prefix(VERBATIM_PREFIX)
        .or_else(|| raw.strip_prefix(DEVICE_NAMESPACE_PREFIX))
        .unwrap_or(&raw);
    let mut key = raw.replace('\\', "/").to_uppercase();
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    key
}

/// True when `child` is `parent` itself or nested under it. The boundary test is what stops
/// `D:/FAM` from matching `D:/FAMOTHER`, the failure recorded in §1.2.
pub fn is_within(parent_key: &str, child_key: &str) -> bool {
    if child_key.len() < parent_key.len() {
        return false;
    }
    let parent_bytes = parent_key.as_bytes();
    if child_key.as_bytes()[..parent_bytes.len()] != *parent_bytes {
        return false;
    }
    child_key.len() == parent_bytes.len()
        || parent_bytes.last() == Some(&b'/')
        || child_key.as_bytes()[parent_bytes.len()] == b'/'
}

pub fn is_within_path(parent: &Path, child: &Path) -> bool {
    is_within(&path_key(parent), &path_key(child))
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
    fn trailing_separators_collapse() {
        assert_eq!(path_key(Path::new("D:/FAM/")), "D:/FAM");
        assert_eq!(path_key(Path::new(r"D:")), "D:");
        assert_eq!(path_key(Path::new(r"D:\")), "D:");
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
    fn exclusion_works_across_separator_styles() {
        let excluded = Path::new(r"D:\proj\node_modules");
        let hit = Path::new("D:/proj/NODE_MODULES/pkg/index.js");
        let sibling = Path::new("D:/proj/node_modules_backup/index.js");
        assert!(is_within_path(excluded, hit));
        assert!(!is_within_path(excluded, sibling));
    }
}
