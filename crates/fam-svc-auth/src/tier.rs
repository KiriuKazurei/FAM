//! The three tiers, as verb whitelists.
//!
//! A tier is a set of names, not a rung on a ladder: `Tier` has no `Ord`, no `at_least`, and no
//! `impl From<Tier> for u8`, because every one of those invites the mistake they are here to avoid -
//! writing `if token.tier >= Edit` and letting a verb added next month fall into the higher tier
//! without anyone deciding that it should. Each tier lists what it allows, and the matrix test in
//! this file enumerates all forty-two combinations.
//!
//! One asymmetry the plan asks for is not a verb: only `admin` may hold the `*` workspace, which
//! stands for "every root this program is configured to manage". On verbs, `admin` is `edit` today,
//! and `admin_has_no_verb_that_edit_lacks` says so out loud rather than letting the name imply more.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::verb::Verb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Browse,
    Edit,
    Admin,
}

impl Tier {
    pub const ALL: &'static [Tier] = &[Self::Browse, Self::Edit, Self::Admin];

    pub fn name(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Edit => "edit",
            Self::Admin => "admin",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|tier| tier.name() == text)
    }

    /// What this tier may do. Written as three explicit lists: a verb appears in a tier's arm only
    /// because someone put it there.
    pub fn allows(self, verb: Verb) -> bool {
        match self {
            Self::Browse => matches!(
                verb,
                Verb::FsList
                    | Verb::FsRead
                    | Verb::FsSearch
                    | Verb::TagsGet
                    | Verb::NoteGet
                    | Verb::WhoAmI
            ),
            Self::Edit => matches!(
                verb,
                Verb::FsList
                    | Verb::FsRead
                    | Verb::FsSearch
                    | Verb::TagsGet
                    | Verb::NoteGet
                    | Verb::WhoAmI
                    | Verb::FsWrite
                    | Verb::FsCreate
                    | Verb::FsDelete
                    | Verb::FsMove
                    | Verb::TagsSet
                    | Verb::TagsRemove
                    | Verb::NoteSet
                    | Verb::NoteClear
            ),
            Self::Admin => matches!(
                verb,
                Verb::FsList
                    | Verb::FsRead
                    | Verb::FsSearch
                    | Verb::TagsGet
                    | Verb::NoteGet
                    | Verb::WhoAmI
                    | Verb::FsWrite
                    | Verb::FsCreate
                    | Verb::FsDelete
                    | Verb::FsMove
                    | Verb::TagsSet
                    | Verb::TagsRemove
                    | Verb::NoteSet
                    | Verb::NoteClear
            ),
        }
    }

    /// Whether a token of this tier may hold `*` as a workspace.
    ///
    /// The wildcard is the one privilege that is not a verb: it is the whole configured set of roots
    /// rather than a directory someone named, and it is the only thing today that separates `admin`
    /// from `edit` on the grant side.
    pub fn may_scope_everything(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// What a person reading `fam mcp token list` should understand the name to mean.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Browse => "list, read, search, read tags and notes",
            Self::Edit => "browse + write inside its workspaces, and change tags and notes",
            Self::Admin => "edit, and may hold * as its workspace",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for Tier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Tier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom(format!("unknown tier {text:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole matrix, spelled out. A new `Verb` makes this fail until it is placed, which is the
    /// point: the compiler cannot decide a tier for a name it has never seen.
    #[test]
    fn every_tier_and_verb_pair_is_declared() {
        let expected: &[&[Verb]] = &[
            // Browse
            &[
                Verb::FsList,
                Verb::FsRead,
                Verb::FsSearch,
                Verb::TagsGet,
                Verb::NoteGet,
                Verb::WhoAmI,
            ],
            // Edit
            &[
                Verb::FsList,
                Verb::FsRead,
                Verb::FsSearch,
                Verb::TagsGet,
                Verb::NoteGet,
                Verb::WhoAmI,
                Verb::FsWrite,
                Verb::FsCreate,
                Verb::FsDelete,
                Verb::FsMove,
                Verb::TagsSet,
                Verb::TagsRemove,
                Verb::NoteSet,
                Verb::NoteClear,
            ],
            // Admin
            &[
                Verb::FsList,
                Verb::FsRead,
                Verb::FsSearch,
                Verb::TagsGet,
                Verb::NoteGet,
                Verb::WhoAmI,
                Verb::FsWrite,
                Verb::FsCreate,
                Verb::FsDelete,
                Verb::FsMove,
                Verb::TagsSet,
                Verb::TagsRemove,
                Verb::NoteSet,
                Verb::NoteClear,
            ],
        ];
        for (index, tier) in Tier::ALL.iter().copied().enumerate() {
            for verb in Verb::ALL {
                assert_eq!(
                    tier.allows(*verb),
                    expected[index].contains(verb),
                    "{tier} / {verb}"
                );
            }
            assert_eq!(
                Verb::ALL.iter().filter(|verb| tier.allows(**verb)).count(),
                expected[index].len(),
                "{tier} allows exactly its list"
            );
        }
    }

    /// `admin` has nothing extra on the verb axis yet, because the vocabulary operations the plan
    /// assigns it do not exist in this codebase - `rename_tag` and `set_priority` are per-file in the
    /// tag store, and increment 12's tool list carries no vocabulary tool.
    ///
    /// This is a snapshot, not an invariant: when a vocabulary verb is added to `Verb`, put it in the
    /// `Admin` arm and update this test and `Tier::summary` together, so the difference is decided
    /// rather than inherited.
    #[test]
    fn admin_has_no_verb_that_edit_lacks() {
        let extra: Vec<&str> = Verb::ALL
            .iter()
            .filter(|verb| Tier::Admin.allows(**verb) && !Tier::Edit.allows(**verb))
            .map(|verb| verb.name())
            .collect();
        assert!(extra.is_empty(), "admin-only verbs: {extra:?}");
        assert!(!Tier::Edit.may_scope_everything());
        assert!(Tier::Admin.may_scope_everything());
        assert!(!Tier::Browse.may_scope_everything());
    }

    #[test]
    fn every_tier_can_say_who_it_is() {
        for tier in Tier::ALL.iter().copied() {
            assert!(tier.allows(Verb::WhoAmI), "{tier} must answer whoami");
            assert_eq!(Tier::parse(tier.name()), Some(tier), "the name round-trips");
            assert_eq!(tier.to_string(), tier.name());
            assert!(!tier.summary().is_empty());
        }
        assert_eq!(Tier::parse("root"), None);
        assert_eq!(Tier::parse("Browse"), None, "the wire name is lower-case");
    }

    /// A tier that arrived from a hand-edited file must be refused by name, not defaulted: a typo in
    /// `tier = "admit"` reading as `browse` would be a quietly narrower grant, and the other way
    /// round would be a leak.
    #[test]
    fn an_unknown_tier_in_the_file_is_an_error_not_a_default() {
        let read = |text: &str| toml::from_str::<TierRow>(&format!("tier = {text:?}"));
        assert_eq!(read("edit").expect("a real tier").tier, Tier::Edit);
        let error = read("admit").expect_err("a misspelled tier is refused");
        assert!(
            error.to_string().contains("admit"),
            "the message names what it could not read: {error}"
        );
    }

    #[derive(Debug, Deserialize)]
    struct TierRow {
        tier: Tier,
    }
}
