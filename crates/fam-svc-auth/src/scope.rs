//! What a token may reach: the directories it was granted, or every root the program manages.
//!
//! Every containment question here goes through `PathKey::of` and `PathKey::is_within`, never a
//! string prefix. A prefix compare on the raw spelling is the bug that turns `D:\notes` into a grant
//! over `D:\notes-secret`, and it also gets Windows' own equivalences wrong - separators, the
//! verbatim `\\?\` form, the device namespace - which the folded key already settles.
//!
//! A granted workspace is a directory *path*, so it composes with the exclusion rules in
//! `fam_core::Prune`: a path under an exclusion is refused before scope is ever asked.

use std::fmt;
use std::path::{Path, PathBuf};

use fam_core::PathKey;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The spelling that means "every configured root", and only `admin` may spell it.
pub const WILDCARD: &str = "*";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Workspace {
    Path(PathBuf),
    All,
}

impl Workspace {
    /// Reads one `--workspace` argument.
    ///
    /// The absolute-path rule is not fussiness: `path_key` of a relative path is a key that no
    /// enumerated file will ever produce, because the walker builds its paths from the roots it was
    /// given. A relative workspace would therefore grant nothing at all, quietly.
    pub fn parse(text: &str, tier: crate::tier::Tier) -> Result<Self, crate::Error> {
        if text == WILDCARD {
            if !tier.may_scope_everything() {
                return Err(crate::Error::WildcardNeedsAdmin(tier));
            }
            return Ok(Self::All);
        }
        let path = PathBuf::from(text);
        if path.as_os_str().is_empty() {
            return Err(crate::Error::EmptyWorkspace);
        }
        if !path.is_absolute() {
            return Err(crate::Error::RelativeWorkspace { path });
        }
        Ok(Self::Path(path))
    }

    /// The spelling that goes into the file and back out again.
    pub fn text(&self) -> String {
        match self {
            Self::All => WILDCARD.into(),
            Self::Path(path) => path.display().to_string(),
        }
    }

    /// Whether `path` is at or below this workspace.
    ///
    /// `roots` is only consulted by [`Workspace::All`], which is defined as "at or below one of the
    /// configured roots" rather than "anywhere on any volume" - the wildcard widens a token across
    /// what the program already manages, not across the machine.
    pub fn covers(&self, path: &Path, roots: &[PathBuf]) -> bool {
        let key = PathKey::of(path);
        match self {
            Self::All => roots.iter().any(|root| PathKey::of(root).is_within(&key)),
            Self::Path(workspace) => PathKey::of(workspace).is_within(&key),
        }
    }

    /// Whether this workspace is a directory right now. Only used to warn at grant time: a workspace
    /// may legitimately name a folder that does not exist yet, and refusing that would make the
    /// verb useless.
    pub fn exists(&self) -> bool {
        match self {
            Self::All => true,
            Self::Path(path) => path.is_dir(),
        }
    }
}

impl fmt::Display for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl Serialize for Workspace {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for Workspace {
    /// Reading a file back is not the same act as being handed the same text on the command line: a
    /// `*` already in the file is accepted whatever tier its row claims, and `TokenStore::load`
    /// rejects that combination. Refusing here would be the weaker outcome, because the row would
    /// fail to parse and a store that cannot parse what it was given tends to get rewritten.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == WILDCARD {
            return Ok(Self::All);
        }
        let path = PathBuf::from(&text);
        if path.is_absolute() {
            Ok(Self::Path(path))
        } else {
            Err(D::Error::custom(format!(
                "workspace {text:?} is not an absolute path"
            )))
        }
    }
}

/// The containment question for a set of workspaces, which is "any of them".
pub fn covers_any(workspaces: &[Workspace], path: &Path, roots: &[PathBuf]) -> bool {
    workspaces
        .iter()
        .any(|workspace| workspace.covers(path, roots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::Tier;
    use fam_core::path_key;

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    #[test]
    fn a_workspace_covers_its_subtree_and_not_its_neighbour() {
        let granted = Workspace::parse("D:/notes", Tier::Edit).expect("absolute");
        let roots = vec![path("D:/notes")];
        assert!(granted.covers(&path("D:/notes/todo.md"), &roots));
        assert!(
            granted.covers(&path("D:/notes"), &roots),
            "the directory itself"
        );
        assert!(granted.covers(&path("D:/notes/a/b/c.md"), &roots));
        assert!(
            !granted.covers(&path("D:/notes-secret/todo.md"), &roots),
            "the prefix compare this replaces would have said yes here"
        );
        assert!(!granted.covers(&path("D:/other/todo.md"), &roots));
    }

    /// The two spellings of one directory are one workspace, and a differently-cased path is the
    /// same path - which is what makes a grant unambiguous on a case-insensitive filesystem.
    #[test]
    fn containment_ignores_separators_and_case() {
        let granted = Workspace::parse("D:\\NOTES\\a", Tier::Edit).expect("absolute");
        let roots = vec![path("D:/notes")];
        assert!(granted.covers(&path("d:/notes/a/b.txt"), &roots));
        assert!(!granted.covers(&path("d:/notes/b.txt"), &roots));
        assert_eq!(
            path_key(&path("D:/notes/a")),
            path_key(&path("d:\\NOTES\\a"))
        );
    }

    #[test]
    fn the_wildcard_is_an_admin_privilege_and_means_the_configured_roots() {
        let wildcard = Workspace::parse("*", Tier::Admin).expect("admin may use *");
        assert!(matches!(
            Workspace::parse("*", Tier::Edit),
            Err(crate::Error::WildcardNeedsAdmin(Tier::Edit))
        ));
        let roots = vec![path("D:/notes"), path("E:/photos")];
        assert!(wildcard.covers(&path("D:/notes/a.txt"), &roots));
        assert!(wildcard.covers(&path("E:/photos/b.jpg"), &roots));
        assert!(
            !wildcard.covers(&path("C:/Users/someone/desktop.txt"), &roots),
            "the wildcard is the managed set, not the machine"
        );
        assert_eq!(wildcard.text(), "*");
    }

    #[test]
    fn a_relative_or_empty_workspace_is_refused_on_the_way_in() {
        assert!(matches!(
            Workspace::parse("notes", Tier::Admin),
            Err(crate::Error::RelativeWorkspace { .. })
        ));
        assert!(matches!(
            Workspace::parse("", Tier::Admin),
            Err(crate::Error::EmptyWorkspace)
        ));
        assert!(matches!(
            Workspace::parse("./notes", Tier::Admin),
            Err(crate::Error::RelativeWorkspace { .. })
        ));
    }

    #[test]
    fn the_file_round_trips_both_spellings() {
        // A workspace is a value inside a table, which is how the file holds it; `toml` will not
        // serialise a bare string at the top level.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Row {
            workspace: Workspace,
        }

        for workspace in [
            Workspace::parse("D:/notes", Tier::Edit).expect("granted"),
            Workspace::parse("*", Tier::Admin).expect("granted"),
        ] {
            let text = toml::to_string(&Row {
                workspace: workspace.clone(),
            })
            .expect("serialises");
            let back: Row = toml::from_str(&text).expect("parses back");
            assert_eq!(back.workspace, workspace);
        }
        let relative = toml::from_str::<Row>("workspace = \"notes\"");
        assert!(relative.is_err(), "a relative path cannot be read back");
    }

    #[test]
    fn any_of_several_workspaces_is_the_union() {
        let granted = vec![
            Workspace::parse("D:/notes", Tier::Edit).expect("one"),
            Workspace::parse("D:/code", Tier::Edit).expect("two"),
        ];
        let roots = vec![path("D:/")];
        assert!(covers_any(&granted, &path("D:/code/src/main.rs"), &roots));
        assert!(covers_any(&granted, &path("D:/notes/x.md"), &roots));
        assert!(!covers_any(&granted, &path("D:/secrets/x"), &roots));
        assert!(
            !covers_any(&[], &path("D:/notes/x"), &roots),
            "no ground at all"
        );
    }

    #[test]
    fn a_missing_directory_is_reported_not_refused() {
        let granted = Workspace::parse("D:/not-created-yet", Tier::Edit).expect("absolute");
        assert!(!granted.exists(), "the CLI warns with this, and only this");
    }
}
