//! Who may ask this program to touch files, and what they may ask for.
//!
//! The layer exists because increment 12 hands the same browsing and editing surface to an agent. A
//! credential is the only thing standing between "an agent may look at my notes folder" and "an agent
//! may read my disk", so the rules here are written to fail closed and to be legible to the person
//! who set them:
//!
//! * **Grants are made by a human, through the CLI.** This crate is not a dependency of `fam-ffi`, so
//!   no C entry point can mint, widen or revoke a token; the MCP surface has no such verb either
//!   (see [`crate::verb`] - the grantable list contains no credential operation to grant).
//! * **A tier is a whitelist of verbs, not a rank.** There is no ordering on [`Tier`], so a verb
//!   added later cannot fall into a higher tier by arithmetic; it has to be named.
//! * **A workspace bounds every verb that names a path**, for every tier including `admin`. Only
//!   `admin` may hold `*`, which means the configured roots rather than the machine.
//! * **The secret is stored as a salted digest and shown once.** Revocation keeps the row, so the
//!   audit trail's `agent:<label>` keeps resolving to something with a date on it.
//!
//! The honest limit of all of this: an agent that can run arbitrary programs *as this user* can read
//! the credential file, call the CLI and do anything the user can. This layer is a barrier against
//! the surface it fronts - MCP requests, and mistakes by a well-behaved client - not against a
//! compromised account. OS-level isolation is where that belongs, and it is not in scope.

mod rand;
mod scope;
mod store;
mod tier;
mod token;
mod verb;

use std::io;
use std::path::PathBuf;

pub use scope::{covers_any, Workspace, WILDCARD};
pub use store::{default_file, is_under, TokenStore};
pub use tier::Tier;
pub use token::{Credential, Issued, Token, PREFIX};
pub use verb::Verb;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {}: {reason}", path.display())]
    Write { path: PathBuf, reason: String },
    #[error("cannot parse {}: {reason}", path.display())]
    Parse { path: PathBuf, reason: String },
    #[error("cannot audit a change to {}: {reason}", path.display())]
    Audit { path: PathBuf, reason: String },
    #[error(
        "{} changed since it was read - another process wrote it, so this change was not made",
        path.display()
    )]
    ConcurrentEdit { path: PathBuf },
    #[error("{label} is not a usable token label: {why}")]
    BadLabel { label: String, why: String },
    #[error("{0} is a name the audit trail already means; a token cannot be called that")]
    ReservedLabel(String),
    #[error("token {0} is already used by another credential")]
    DuplicateId(String),
    #[error("the label {0} is already used by another credential")]
    LabelTaken(String),
    #[error("no credential with id {0}")]
    UnknownId(String),
    #[error("credential {0} is already revoked")]
    AlreadyRevoked(String),
    #[error("a token needs at least one workspace; {0} has none")]
    NoWorkspaces(String),
    #[error(
        "only an admin token may hold {WILDCARD:?} as its workspace; a token of tier {0} cannot"
    )]
    WildcardNeedsAdmin(Tier),
    #[error("workspace {} is not an absolute path, so it could never contain a file", path.display())]
    RelativeWorkspace { path: PathBuf },
    #[error(
        "workspace {} has a . or .. component: the path it opens is not the path it names, \
         so it can scope nothing",
        path.display()
    )]
    DottedWorkspace { path: PathBuf },
    #[error("unknown tier {0:?}; it is one of browse, edit, admin")]
    UnknownTier(String),
    #[error("a workspace must not be empty")]
    EmptyWorkspace,
    #[error("the credential store has no entropy to issue a secret from")]
    EntropyUnavailable,
    #[error("the credential store could not read entropy: {reason}")]
    EntropyUnavailableWith { reason: String },
    #[error("the row with id {0} is missing its salt or its digest")]
    IncompleteRow(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two halves of the plan's sentence "three tiers are whitelists, and none may leave its
    /// workspaces" have to be checkable together, which is only true if the types let a caller
    /// express "granted this verb, in this place" in one step.
    #[test]
    fn a_grant_is_answered_by_the_credential_itself() {
        let issued = Token::issue(
            "co-op",
            Tier::Edit,
            vec![Workspace::parse("D:/notes", Tier::Edit).expect("absolute")],
            0,
        )
        .expect("issued");
        let token = issued.token;
        assert!(token.allows(Verb::FsWrite));
        assert!(token.covers(
            &PathBuf::from("D:/notes/a.txt"),
            &[PathBuf::from("D:/notes")]
        ));
        assert!(
            !token.covers(
                &PathBuf::from("D:/other/a.txt"),
                &[PathBuf::from("D:/notes")]
            ),
            "a verb the tier allows is still bounded by the workspace"
        );
    }

    #[test]
    fn the_wildcard_is_named_the_same_everywhere() {
        assert_eq!(WILDCARD, "*");
        assert!(is_under(
            &PathBuf::from("C:/app"),
            &PathBuf::from("C:/app/x/y")
        ));
        assert!(!is_under(
            &PathBuf::from("C:/app"),
            &PathBuf::from("C:/application")
        ));
    }
}
