//! One credential: what it is called, what it may do, and the digest of the secret that proves it.
//!
//! The secret itself is never stored. What is stored is a salted digest of it, and the salt is
//! per-token rather than shared - one random value per row - so that two tokens issued at the same
//! moment cannot be told apart by their digests, and so a leaked file cannot be attacked as a whole.
//! A token secret is 128 bits of the operating system's randomness, which is why plain SHA-256 is
//! enough here: the thing being protected is not guessable, so the salt is for correlation, not for
//! slowing down a dictionary attack. (If a lower-entropy credential is ever added to this file, this
//! paragraph is the one that has to change first, to something like PBKDF2.)

use std::path::PathBuf;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::scope::Workspace;
use crate::tier::Tier;

/// The prefix of a credential string, so a token can be recognised in a config file by eye.
pub const PREFIX: &str = "fam";

/// How many random bytes a secret holds. As hex that is 64 characters, and 128 bits of guessing left.
pub(crate) const SECRET_BYTES: usize = 32;
/// Bytes of salt per token.
pub(crate) const SALT_BYTES: usize = 16;
/// Hex characters of token id. Short enough to type, long enough not to collide by accident.
pub(crate) const ID_HEX_CHARS: usize = 8;

/// The domain-separation prefix. Changing it invalidates every stored digest, which is the point: a
/// digest from a different scheme must not verify by coincidence.
const DIGEST_SCOPE: &[u8] = b"fam-mcp-token-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub id: String,
    pub label: String,
    pub tier: Tier,
    pub workspaces: Vec<Workspace>,
    /// Hex. Random per token, kept so the digest cannot be compared across rows.
    pub salt: String,
    /// Hex digest of the secret. Never printed.
    pub secret_hash: String,
    /// UTC, to the millisecond.
    pub created_at: String,
    /// Set once revoked. The row stays: the label has to keep resolving in the audit trail, and a
    /// retired id must not be handed out again.
    pub revoked_at: Option<String>,
}

/// A credential and the plaintext that goes with it. The plaintext exists only here, only once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    pub token: Token,
    pub secret: String,
}

impl Issued {
    /// The string a person pastes into their MCP client.
    pub fn credential(&self) -> String {
        format!("{PREFIX}_{}_{}", self.token.id, self.secret)
    }
}

/// The two halves of a presented credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub id: String,
    pub secret: String,
}

impl Credential {
    /// Reads `fam_<id>_<secret>`.
    ///
    /// Every failure says the same thing whatever it was, because the caller on the other side of an
    /// MCP connection is not entitled to learn whether an id exists, was revoked, or was typed
    /// wrongly - and a credential check that answers differently per case is an enumeration service.
    pub fn parse(text: &str) -> Option<Credential> {
        let rest = text.trim().strip_prefix(&format!("{PREFIX}_"))?;
        // Split at the first underscore: an id is hex, so it never contains one, which means the
        // split cannot be confused by anything on the right - and `split_once` cannot panic on a
        // string that is shorter than the id, which a fixed-width slice could.
        let (id, secret) = rest.split_once('_')?;
        if id.len() != ID_HEX_CHARS
            || !id.chars().all(|c| c.is_ascii_hexdigit())
            || secret.len() < SECRET_BYTES
            || !secret.chars().all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Credential {
            id: id.to_ascii_lowercase(),
            secret: secret.to_ascii_lowercase(),
        })
    }
}

impl Token {
    /// A brand-new credential. The caller supplies the policy; everything secret or identifying is
    /// minted here.
    pub fn issue(
        label: &str,
        tier: Tier,
        workspaces: Vec<Workspace>,
        now_ms: u64,
    ) -> Result<Issued, crate::Error> {
        let label = ValidatedLabel::check(label)?.0;
        if workspaces.is_empty() {
            return Err(crate::Error::NoWorkspaces(label));
        }
        if workspaces
            .iter()
            .any(|workspace| matches!(workspace, Workspace::All))
            && !tier.may_scope_everything()
        {
            return Err(crate::Error::WildcardNeedsAdmin(tier));
        }
        let secret = crate::rand::hex_bytes(SECRET_BYTES)?;
        let salt = crate::rand::hex_bytes(SALT_BYTES)?;
        let id = crate::rand::hex_bytes(ID_HEX_CHARS / 2)?;
        let token = Self {
            secret_hash: digest(&salt, &id, &secret),
            salt,
            id,
            label,
            tier,
            workspaces,
            created_at: fam_core::format_utc(now_ms),
            revoked_at: None,
        };
        Ok(Issued { token, secret })
    }

    /// Does this secret unlock this token? Constant-time, and `false` once revoked.
    pub fn verifies(&self, secret: &str) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        constant_time_eq(
            self.secret_hash.as_bytes(),
            digest(&self.salt, &self.id, secret).as_bytes(),
        )
    }

    /// Swaps the secret under the same identity: a new salt, a fresh digest, and nothing else
    /// moved. The id, the label, the tier, the workspaces and `created_at` all stay, because the
    /// audit trail quotes the first two and the grant is the last three - rotation is a new key on
    /// one credential, not a new credential.
    pub(crate) fn reseal(&mut self, salt: &str, secret: &str) {
        self.salt = salt.to_owned();
        self.secret_hash = digest(salt, &self.id, secret);
    }

    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn allows(&self, verb: crate::verb::Verb) -> bool {
        self.is_active() && self.tier.allows(verb)
    }

    pub fn covers(&self, path: &std::path::Path, roots: &[PathBuf]) -> bool {
        crate::scope::covers_any(&self.workspaces, path, roots)
    }

    /// The label the audit trail will carry for this token, as an [`fam_svc_audit::Actor`].
    pub fn actor(&self) -> fam_svc_audit::Actor {
        fam_svc_audit::Actor::agent(&self.label)
    }

    /// Reads a row out of the file. Kept separate from [`Token::issue`] because a row that arrived
    /// from disk has already been through serde, and the two extra checks below are exactly the ones
    /// a hand-edit could break.
    pub(crate) fn accept(self) -> Result<Self, crate::Error> {
        ValidatedLabel::check(&self.label)?;
        if self.salt.is_empty() || self.secret_hash.is_empty() || self.id.is_empty() {
            return Err(crate::Error::IncompleteRow(self.id.clone()));
        }
        if self
            .workspaces
            .iter()
            .any(|workspace| matches!(workspace, Workspace::All))
            && !self.tier.may_scope_everything()
        {
            // The rule is enforced at issue time, so a row that breaks it arrived by hand-edit -
            // and an `edit` token with a wildcard is a privilege the label does not describe.
            return Err(crate::Error::WildcardNeedsAdmin(self.tier));
        }
        Ok(self)
    }
}

/// The label is the name the audit trail will blame, so the rules are about not being able to write
/// someone else's name.
struct ValidatedLabel(String);

impl ValidatedLabel {
    const MAX_CHARS: usize = 48;

    fn check(label: &str) -> Result<Self, crate::Error> {
        let trimmed = label.trim();
        if trimmed.is_empty() || trimmed.chars().count() > Self::MAX_CHARS {
            return Err(crate::Error::BadLabel {
                label: label.to_owned(),
                why: format!("between 1 and {} characters", Self::MAX_CHARS),
            });
        }
        // The character set is the whole of the injection defence: a label with a quote or a newline
        // in it would still be escaped by the serialiser, but a label containing `:` or starting with
        // `?` would survive a round trip through the audit reader as something else.
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(crate::Error::BadLabel {
                label: label.to_owned(),
                why: "letters, digits, dot, dash and underscore only".into(),
            });
        }
        // The three actor names the audit format already means, and the value a blank agent label
        // falls back to. Naming a token any of these would let a machine read as a person.
        const RESERVED: &[&str] = &["user", "system", "unnamed"];
        if RESERVED.contains(&trimmed.to_ascii_lowercase().as_str()) {
            return Err(crate::Error::ReservedLabel(trimmed.to_owned()));
        }
        Ok(Self(trimmed.to_owned()))
    }
}

fn digest(salt: &str, id: &str, secret: &str) -> String {
    let mut data = DIGEST_SCOPE.to_vec();
    data.extend_from_slice(salt.as_bytes());
    data.extend_from_slice(id.as_bytes());
    data.extend_from_slice(secret.as_bytes());
    fam_core::hex_lower(&fam_core::sha256(&data))
}

/// Equal length, equal content - without letting the first differing byte end the comparison.
///
/// Three lines of `fold` over the XOR of the bytes: the digest comparison is over fixed-length hex,
/// so a byte-at-a-time early return is the only shape a timing measurement could read.
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// A row as it sits in the TOML file: `revoked_at` is an optional string and everything else is
/// required, and the derived impls are what let a hand-edited file be rejected field by field.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenRow {
    pub id: String,
    pub label: String,
    pub tier: Tier,
    pub workspaces: Vec<Workspace>,
    pub salt: String,
    pub secret_hash: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revoked_at: Option<String>,
}

impl From<&Token> for TokenRow {
    fn from(token: &Token) -> Self {
        Self {
            id: token.id.clone(),
            label: token.label.clone(),
            tier: token.tier,
            workspaces: token.workspaces.clone(),
            salt: token.salt.clone(),
            secret_hash: token.secret_hash.clone(),
            created_at: token.created_at.clone(),
            revoked_at: token.revoked_at.clone(),
        }
    }
}

impl From<TokenRow> for (Token, u64) {
    /// The `u64` is a placeholder the caller replaces; see `TokenStore::load`, which reads the
    /// timestamps as text and does not need to turn them back into a clock.
    fn from(row: TokenRow) -> Self {
        (
            Token {
                id: row.id,
                label: row.label,
                tier: row.tier,
                workspaces: row.workspaces,
                salt: row.salt,
                secret_hash: row.secret_hash,
                created_at: row.created_at,
                revoked_at: row.revoked_at,
            },
            0,
        )
    }
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let row = TokenRow::deserialize(deserializer)?;
        let (token, _) = row.into();
        token.accept().map_err(D::Error::custom)
    }
}

impl Serialize for Token {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TokenRow::from(self).serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verb::Verb;

    fn issue(label: &str, tier: Tier) -> Issued {
        Token::issue(
            label,
            tier,
            vec![Workspace::parse("D:/notes", tier).expect("absolute")],
            1_758_600_000_000,
        )
        .expect("issued")
    }

    #[test]
    fn a_secret_verifies_once_and_no_other_secret_does() {
        let issued = issue("notes-bot", Tier::Browse);
        assert!(issued.token.verifies(&issued.secret));
        assert!(!issued.token.verifies(""));
        assert!(!issued.token.verifies(&issued.secret.replace('0', "1")));
        assert_eq!(issued.secret.len(), SECRET_BYTES * 2);
        assert_ne!(issued.token.secret_hash, issued.secret);
        assert!(!issued
            .token
            .secret_hash
            .contains(&issued.secret[..8.min(issued.secret.len())]));
    }

    /// The same secret issued twice must produce different digests, which is the whole job of the
    /// per-token salt.
    #[test]
    fn two_tokens_do_not_share_a_digest_shape() {
        let first = issue("a-bot", Tier::Browse);
        let second = issue("b-bot", Tier::Browse);
        assert_ne!(first.token.salt, second.token.salt);
        assert_ne!(first.token.secret_hash, second.token.secret_hash);
        assert!(!first.token.verifies(&second.secret));
    }

    #[test]
    fn revoking_stops_verification_and_keeps_the_row() {
        let issued = issue("old-bot", Tier::Edit);
        let mut token = issued.token;
        assert!(token.verifies(&issued.secret));
        token.revoked_at = Some("2026-09-24T00:00:00.000Z".into());
        assert!(!token.is_active());
        assert!(
            !token.verifies(&issued.secret),
            "a revoked secret must stop working, not merely look revoked"
        );
        assert!(!token.allows(Verb::FsWrite), "and with it every verb");
    }

    #[test]
    fn a_credential_splits_without_trusting_the_secret_for_boundaries() {
        let issued = issue("split-bot", Tier::Browse);
        let text = issued.credential();
        let parsed = Credential::parse(&text).expect("round trips");
        assert_eq!(parsed.id, issued.token.id);
        assert_eq!(parsed.secret, issued.secret);
        // A hex secret never contains an underscore, but a hand-written credential is somebody's
        // problem: everything after the first underscore is the secret, so a too-short or non-hex
        // tail is refused and a longer one is not.
        assert!(Credential::parse(&format!("fam_{}_aa_bb", issued.token.id)).is_none());
        let long = Credential::parse(&format!("fam_{}_{}", issued.token.id, "a".repeat(70)))
            .expect("a longer secret is still one secret");
        assert_eq!(long.id, issued.token.id);
        assert_eq!(
            long.secret.len(),
            70,
            "the tail after the first underscore, whole"
        );
    }

    #[test]
    fn a_malformed_credential_is_not_a_token() {
        for text in [
            "",
            "fam",
            "fam_",
            "fam_00_zzzzzzzz",
            "fam_zzzzzzzz_00",
            "fam_00000000_short",
            "other_00000000_00",
            "fam_00000000_00  ",
        ] {
            assert!(Credential::parse(text).is_none(), "accepted {text:?}");
        }
        // Surrounding whitespace from a config file or a paste is not part of the credential.
        let issued = issue("ws-bot", Tier::Browse);
        let padded = format!("  {}\n", issued.credential());
        assert!(Credential::parse(&padded).is_some());
    }

    #[test]
    fn a_label_cannot_impersonate_a_real_actor() {
        for label in [
            "user", "USER", "system", "unnamed", "agent:me", "a b", "a\nb", "a\"b", "",
        ] {
            let error = Token::issue(
                label,
                Tier::Browse,
                vec![Workspace::parse("D:/notes", Tier::Browse).expect("absolute")],
                0,
            )
            .expect_err("refused");
            assert!(
                matches!(
                    error,
                    crate::Error::BadLabel { .. } | crate::Error::ReservedLabel(_)
                ),
                "label {label:?} was accepted: {error:?}"
            );
        }
        // Every accepted label survives a trip through the audit actor format unchanged, which is
        // what makes "the trail says who" worth having.
        for label in ["notes-bot", "ci_bot", "a.b", "Z9"] {
            let issued = issue(label, Tier::Browse);
            assert_eq!(issued.token.actor().encoded(), format!("agent:{label}"));
            assert_eq!(
                fam_svc_audit::Actor::parse(&issued.token.actor().encoded()).as_ref(),
                Some(&issued.token.actor())
            );
        }
    }

    #[test]
    fn a_label_is_clipped_to_a_bounded_length() {
        let long = "x".repeat(49);
        assert!(Token::issue(
            &long,
            Tier::Browse,
            vec![Workspace::parse("D:/n", Tier::Browse).expect("absolute")],
            0
        )
        .is_err());
        assert_eq!(
            issue(&"y".repeat(48), Tier::Browse)
                .token
                .label
                .chars()
                .count(),
            48
        );
    }

    #[test]
    fn a_token_with_no_workspace_grants_nothing_so_it_is_not_issued() {
        let error = Token::issue("lost-bot", Tier::Admin, Vec::new(), 0)
            .expect_err("a token that can reach no path is a mistake");
        assert!(
            matches!(&error, crate::Error::NoWorkspaces(label) if label == "lost-bot"),
            "the message names the token that was refused: {error}"
        );
    }

    #[test]
    fn a_row_missing_its_digest_is_refused_on_read() {
        let issued = issue("row-bot", Tier::Browse);
        let mut token = issued.token;
        token.secret_hash = String::new();
        assert!(matches!(
            token.accept(),
            Err(crate::Error::IncompleteRow(_))
        ));
    }

    #[test]
    fn the_digest_covers_the_id_so_rows_cannot_be_swapped() {
        // Two salts and one secret: recomputing with the other row's id must fail, which is what
        // stops a file edit that pastes one row's digest onto another row.
        let salt = crate::rand::hex_bytes(SALT_BYTES).expect("entropy");
        let secret = crate::rand::hex_bytes(SECRET_BYTES).expect("entropy");
        let for_a = digest(&salt, "aaaaaaaa", &secret);
        let for_b = digest(&salt, "bbbbbbbb", &secret);
        assert_ne!(for_a, for_b);
        assert_eq!(for_a.len(), 64);
    }

    #[test]
    fn constant_time_comparison_answers_the_same_question() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"abcd", b"abcde"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn the_file_row_does_not_carry_the_secret() {
        let issued = issue("toml-bot", Tier::Browse);
        let text = toml::to_string_pretty(&issued.token).expect("serialises");
        assert!(
            text.contains(&issued.token.secret_hash),
            "what is stored is the digest"
        );
        assert!(
            !text.contains(&issued.secret),
            "the plaintext is not written"
        );
        assert!(
            !text.contains(&issued.secret[..24]),
            "nor is any prefix of it, which is enough to mount a partial guess"
        );
        assert!(text.contains("label = \"toml-bot\""));
        let back: Token = toml::from_str(&text).expect("parses back");
        assert_eq!(back, issued.token);
    }
}
