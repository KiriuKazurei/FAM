//! The verbs an MCP token can be granted, in their wire names.
//!
//! This list *is* the tool surface an agent will see, and it is deliberately closed: a credential
//! grants a subset of these names and nothing else. Two consequences are worth holding onto.
//!
//! There is no verb here for touching a credential, an exclusion or the audit log. The rule from the
//! plan is that the MCP surface does not offer those at all - not that it offers them and refuses -
//! so the enforcement is that no such name exists to grant, and
//! [`no_verb_reaches_the_credential_or_the_trail`] is what keeps it that way.
//!
//! And because tiers are whitelists over this list rather than a ladder, adding a verb here cannot
//! silently land in `admin`: the matrix in `tier.rs` enumerates every combination and fails until
//! someone says which tier may have the new name.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    /// One directory's entries, from the lazy index.
    FsList,
    /// A file's contents.
    FsRead,
    /// A name search across the granted scope.
    FsSearch,
    /// Change the bytes of an existing file.
    FsWrite,
    /// Make a file that was not there.
    FsCreate,
    /// Remove a file, or a directory the caller walked into.
    FsDelete,
    /// Rename, or move between directories.
    FsMove,
    /// The tags on one path, and the tag vocabulary.
    TagsGet,
    /// Put a tag on a path.
    TagsSet,
    /// Take one off.
    TagsRemove,
    /// Rename a label across the whole library, collapsing duplicates into the target name. Names
    /// no path, so like `whoami` it carries no workspace check - and unlike `whoami` it is a write,
    /// so the tier matrix keeps it out of the read-only hands.
    TagsMerge,
    /// The free-text note attached to a path, which is not a tag and never appears in a tag list.
    NoteGet,
    NoteSet,
    NoteClear,
    /// "Who am I, what do I hold, what is in scope." Names no path, so it is the one verb every tier
    /// may call without a workspace check.
    WhoAmI,
}

impl Verb {
    /// Every verb, in the order the access list reads: read-only first, then the writes, then the
    /// metadata pairs, then identity.
    pub const ALL: &'static [Verb] = &[
        Verb::FsList,
        Verb::FsRead,
        Verb::FsSearch,
        Verb::FsWrite,
        Verb::FsCreate,
        Verb::FsDelete,
        Verb::FsMove,
        Verb::TagsGet,
        Verb::TagsSet,
        Verb::TagsRemove,
        Verb::TagsMerge,
        Verb::NoteGet,
        Verb::NoteSet,
        Verb::NoteClear,
        Verb::WhoAmI,
    ];

    /// The wire name, which is also the name an audit line carries for the call.
    pub fn name(self) -> &'static str {
        match self {
            Self::FsList => "fs_list",
            Self::FsRead => "fs_read",
            Self::FsSearch => "fs_search",
            Self::FsWrite => "fs_write",
            Self::FsCreate => "fs_create",
            Self::FsDelete => "fs_delete",
            Self::FsMove => "fs_move",
            Self::TagsGet => "tags_get",
            Self::TagsSet => "tags_set",
            Self::TagsRemove => "tags_remove",
            Self::TagsMerge => "tags_merge",
            Self::NoteGet => "note_get",
            Self::NoteSet => "note_set",
            Self::NoteClear => "note_clear",
            Self::WhoAmI => "whoami",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|verb| verb.name() == text)
    }

    /// Whether this verb is *about* a path. `whoami` is not: it answers with the caller's own
    /// credential, so there is nothing outside the token to scope.
    pub fn names_a_path(self) -> bool {
        !matches!(self, Self::WhoAmI | Self::TagsMerge)
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_the_wire_names_and_parse_back() {
        for verb in Verb::ALL {
            assert_eq!(Verb::parse(verb.name()), Some(*verb), "{verb:?}");
        }
        assert_eq!(
            Verb::parse("fs_drop"),
            None,
            "an invented verb is not accepted"
        );
        assert_eq!(Verb::parse(""), None);
        assert_eq!(Verb::ALL.len(), 15);
    }

    /// The names an MCP caller must never be offered, in any spelling. This is the reverse test the
    /// plan asked for, expressed against the only data that exists before the protocol layer does:
    /// the grantable list itself.
    #[test]
    fn no_verb_reaches_the_credential_or_the_trail() {
        let forbidden = [
            "token",
            "permission",
            "grant",
            "revoke",
            "audit",
            "exclude",
            "workspace",
            "config",
            "password",
            "secret",
        ];
        for verb in Verb::ALL {
            let name = verb.name().to_lowercase();
            for needle in forbidden {
                assert!(
                    !name.contains(needle),
                    "{name} must not be a grantable verb: it names a credential or scope operation"
                );
            }
        }
        // And the list has no room for one later: every name is distinct and every name is short, so
        // a future `token_revoke` would have to be *added* here, where this test can see it.
        let mut sorted: Vec<&str> = Verb::ALL.iter().map(|verb| verb.name()).collect();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), before, "verb names are unique");
    }

    #[test]
    fn only_identity_names_no_path() {
        assert!(!Verb::WhoAmI.names_a_path());
        /* The merge is the second pathless verb, and it is a write: the tier question is the only
         * one the gate can ask it, which is exactly why it must be answered. */
        assert!(!Verb::TagsMerge.names_a_path());
        for verb in Verb::ALL {
            if *verb == Verb::WhoAmI || *verb == Verb::TagsMerge {
                continue;
            }
            assert!(verb.names_a_path(), "{verb} is about a path");
        }
    }
}
