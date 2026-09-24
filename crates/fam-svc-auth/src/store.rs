//! The credential file and everything that changes it.
//!
//! Location is `AppPaths::config_dir()/mcp-tokens.toml` - a file a person edits or removes with the
//! same two commands, never over the ABI and never over MCP. `fam-svc-auth` is not a dependency of
//! `fam-ffi`, so there is no C entry point that can mint, widen or revoke a grant: that stays with
//! the human who owns the account.
//!
//! Two things are true of every mutating call here, and both are structural rather than polite:
//!
//! * The store owns its [`AuditLog`], so a change to a grant cannot happen without a line about it.
//!   The line names the human who made it (the actor is `user`) and describes the token it changed -
//!   id, label, tier, workspaces - and never a secret, never a digest.
//! * A save re-reads the file first and refuses if it changed since this store read it. Losing a
//!   revoke to a concurrent `add` would be a silent widening of access, which is the one mistake a
//!   credential file is not allowed to make.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fam_core::{epoch_ms, format_utc, Config, PathKey};
use fam_svc_audit::{Actor, AuditLog, Draft, Op, Options as AuditOptions};
use serde::{Deserialize, Serialize};

use crate::scope::Workspace;
use crate::tier::Tier;
use crate::token::{Credential, Issued, Token, ID_HEX_CHARS, SALT_BYTES, SECRET_BYTES};

/// What the file holds: the rows, under a key.
///
/// The key is fixed and unknown keys are refused, which is the opposite of forward-compatible on
/// purpose. A table read under a misspelled name (`[[tokens]]`) comes back as an empty store, and an
/// empty store that the person then adds to writes a file with one row - every grant they had is
/// gone, and the trail only says `token.add`. A top-level field is added by changing this struct, so
/// the tolerance would buy nothing that a code change does not already cover.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    token: Vec<Token>,
}

#[derive(Debug)]
pub struct TokenStore {
    path: PathBuf,
    tokens: Vec<Token>,
    /// Digest of the bytes this store read, so a save can notice it is about to overwrite somebody
    /// else's change. `None` when the file did not exist yet.
    loaded_digest: Option<String>,
    audit: AuditLog,
}

impl TokenStore {
    /// Opens the credential file and the audit trail it writes into.
    ///
    /// The trail is opened first on purpose: a store that cannot record its own changes is not one a
    /// person should be issuing grants through, and that is the same "no log, no program" rule the
    /// core applies to browsing.
    pub fn open(config: &Config) -> Result<Self, crate::Error> {
        let audit = AuditLog::open(AuditOptions {
            directory: config.audit_dir.clone(),
            actor: Actor::User,
            record_reads: true,
            daily_limit_bytes: config.audit_daily_limit_bytes(),
            clock: fam_svc_audit::system_clock(),
        })
        .map_err(|source| crate::Error::Audit {
            path: config.audit_dir.clone(),
            reason: source.to_string(),
        })?;
        Self::at(config.mcp_token_file.clone(), audit)
    }

    /// The same, with a trail the caller already holds. Tests use this to keep both files inside one
    /// temporary directory.
    pub fn at(path: PathBuf, audit: AuditLog) -> Result<Self, crate::Error> {
        let (tokens, loaded_digest) =
            match fs::read(&path) {
                Ok(bytes) => {
                    let digest = fam_core::hex_lower(&fam_core::sha256(&bytes));
                    let document: Document = toml::from_str(&String::from_utf8_lossy(&bytes))
                        .map_err(|source| crate::Error::Parse {
                            path: path.clone(),
                            reason: source.to_string(),
                        })?;
                    (document.token, Some(digest))
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
                Err(source) => {
                    return Err(crate::Error::Io {
                        path: path.clone(),
                        source,
                    })
                }
            };
        let store = Self {
            path,
            tokens,
            loaded_digest,
            audit,
        };
        // Ids and labels each have to resolve uniquely: the trail quotes a label and a verification
        // quotes an id. Checked on read, so a hand-edited duplicate is refused rather than silently
        // first-wins.
        store.check_identifiers()?;
        Ok(store)
    }

    fn check_identifiers(&self) -> Result<(), crate::Error> {
        let mut ids: Vec<&str> = Vec::with_capacity(self.tokens.len());
        let mut labels: Vec<String> = Vec::with_capacity(self.tokens.len());
        for token in &self.tokens {
            if ids.contains(&token.id.as_str()) {
                return Err(crate::Error::DuplicateId(token.id.clone()));
            }
            // Case-insensitive, the same rule `add` applies: two rows whose labels differ only by
            // ASCII case would let one be revoked while the other keeps the same grant, and the
            // trail quotes labels to a person who will read them as the same name.
            if labels
                .iter()
                .any(|taken| taken.eq_ignore_ascii_case(&token.label))
            {
                return Err(crate::Error::LabelTaken(token.label.clone()));
            }
            ids.push(token.id.as_str());
            labels.push(token.label.clone());
        }
        Ok(())
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The trail this store writes into, so a caller can record the attempts it refused before any
    /// change was made. One attempt - refused or completed - is one line, which is the rule the ABI
    /// shell follows and the reason the store does not own the whole story.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    pub fn find(&self, id: &str) -> Option<&Token> {
        self.tokens.iter().find(|token| token.id == id)
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.tokens.iter().position(|token| token.id == id)
    }

    /// Which credential string unlocks a live token, if any.
    ///
    /// One answer for every no: unknown id, wrong secret, revoked, malformed. Nothing here says which
    /// of the four it was, because asking that question repeatedly is only worth it if the answers
    /// differ - and then the file has become a list of live token ids.
    pub fn verify(&self, credential: &str) -> Option<&Token> {
        let parsed = Credential::parse(credential)?;
        self.tokens
            .iter()
            .find(|token| token.id == parsed.id && token.verifies(&parsed.secret))
    }

    /// Grants a new token. The returned plaintext exists nowhere else.
    pub fn add(
        &mut self,
        label: &str,
        tier: Tier,
        workspaces: &[String],
        now: SystemTime,
    ) -> Result<Issued, crate::Error> {
        let workspaces = workspaces
            .iter()
            .map(|text| Workspace::parse(text, tier))
            .collect::<Result<Vec<_>, _>>()?;
        let issued = Token::issue(label, tier, workspaces, epoch_ms(now).unwrap_or_default())?;
        if self.index_of(&issued.token.id).is_some() {
            return Err(crate::Error::DuplicateId(issued.token.id));
        }
        if self
            .tokens
            .iter()
            .any(|token| token.label.eq_ignore_ascii_case(&issued.token.label))
        {
            return Err(crate::Error::LabelTaken(issued.token.label));
        }
        let mut rows = self.tokens.clone();
        rows.push(issued.token.clone());
        self.save_as(&rows)?;
        // Memory follows the disk, never the other way round: a save that failed has to leave this
        // store answering `verify` with exactly the rows the file holds, or one process gets to act on
        // a grant nobody else can see and nothing on disk says so.
        self.tokens = rows;
        self.record(Op::TokenAdd, &issued.token);
        Ok(issued)
    }

    /// Marks a token revoked, and keeps the row.
    ///
    /// Deleting the row instead would be tempting only until the trail is considered: the log keeps
    /// naming `agent:<label>`, and a label that no longer resolves to a row turns those lines into an
    /// unsolvable claim. The row is also what stops an id being handed out again.
    pub fn revoke(&mut self, id: &str, now: SystemTime) -> Result<Token, crate::Error> {
        let index = self
            .index_of(id)
            .ok_or_else(|| crate::Error::UnknownId(id.to_owned()))?;
        if self.tokens[index].revoked_at.is_some() {
            return Err(crate::Error::AlreadyRevoked(self.tokens[index].id.clone()));
        }
        let mut rows = self.tokens.clone();
        rows[index].revoked_at = Some(format_utc(epoch_ms(now).unwrap_or_default()));
        let snapshot = rows[index].clone();
        self.save_as(&rows)?;
        self.tokens = rows;
        self.record(Op::TokenRevoke, &snapshot);
        Ok(snapshot)
    }

    /// Replaces a token's secret, keeping its identity and its policy.
    ///
    /// A revoked token cannot be rotated. Otherwise "revoked" would mean "disabled until someone
    /// rotates it", which is not what the person who ran `revoke` meant: the credential comes back
    /// with its old id, its old label and therefore its old history.
    pub fn rotate(&mut self, id: &str) -> Result<Issued, crate::Error> {
        let index = self
            .index_of(id)
            .ok_or_else(|| crate::Error::UnknownId(id.to_owned()))?;
        let token = self.tokens[index].clone();
        if token.revoked_at.is_some() {
            return Err(crate::Error::AlreadyRevoked(token.id));
        }
        // A new salt, not the old one: two rotations must not leave a pair of rows whose digests can
        // be compared to each other, whatever else the file goes on to say.
        let secret = crate::rand::hex_bytes(SECRET_BYTES)?;
        let salt = crate::rand::hex_bytes(SALT_BYTES)?;
        let mut refreshed = token;
        refreshed.reseal(&salt, &secret);
        let mut rows = self.tokens.clone();
        rows[index] = refreshed.clone();
        self.save_as(&rows)?;
        self.tokens = rows;
        self.record(Op::TokenRotate, &refreshed);
        Ok(Issued {
            token: refreshed,
            secret,
        })
    }

    /// Writes `rows` as the file, and only then adopts them.
    ///
    /// Three rules live here, and they are all about the direction a failure can go:
    ///
    /// * The trail has to be writing **before** anything changes. A grant that cannot be recorded is
    ///   not a grant, and this is the last moment where taking it back costs nothing.
    /// * A file that changed since this store read it is refused, not merged. Losing somebody else's
    ///   revoke would be a silent widening of access.
    /// * After a successful rename the baseline moves to what was just written, so a second change in
    ///   the same process is compared against the file it actually made, not the one it found.
    fn save_as(&mut self, rows: &[Token]) -> Result<(), crate::Error> {
        let health = self.audit.health();
        if !health.writing {
            return Err(crate::Error::Audit {
                path: self.path.clone(),
                reason: format!(
                    "the trail is not writing ({})",
                    health
                        .last_error
                        .unwrap_or_else(|| "no reason given".into())
                ),
            });
        }
        match &self.loaded_digest {
            Some(previous) => {
                let current = match fs::read(&self.path) {
                    Ok(bytes) => fam_core::hex_lower(&fam_core::sha256(&bytes)),
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                        return Err(crate::Error::ConcurrentEdit {
                            path: self.path.clone(),
                        })
                    }
                    Err(source) => {
                        return Err(crate::Error::Io {
                            path: self.path.clone(),
                            source,
                        })
                    }
                };
                if &current != previous {
                    return Err(crate::Error::ConcurrentEdit {
                        path: self.path.clone(),
                    });
                }
            }
            // No baseline to compare against, which is the window a *first* grant falls into: two
            // processes both see no file, and the later rename publishes a table the first one never
            // read. So the file has to still be absent.
            None => match fs::metadata(&self.path) {
                Ok(_) => {
                    return Err(crate::Error::ConcurrentEdit {
                        path: self.path.clone(),
                    })
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(crate::Error::Io {
                        path: self.path.clone(),
                        source,
                    })
                }
            },
        }
        let text = toml::to_string_pretty(&Document {
            token: rows.to_vec(),
        })
        .map_err(|source| crate::Error::Write {
            path: self.path.clone(),
            reason: source.to_string(),
        })?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| crate::Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // The process id is in the temporary name because two savers sharing one name can publish
        // each other's half-written bytes: the rename is atomic, the write into it is not.
        let temp = self
            .path
            .with_extension(format!("toml.tmp.{}", std::process::id()));
        write_then_rename(&temp, &self.path, text.as_bytes())?;
        self.loaded_digest = Some(fam_core::hex_lower(&fam_core::sha256(text.as_bytes())));
        Ok(())
    }

    /// The line saying what changed, written after the change is on disk.
    ///
    /// The order is the one that cannot lie: a line only ever follows a change that took effect. What
    /// a line cannot do is report itself, so a caller that cares whether the count of lost lines grew
    /// reads [`TokenStore::trail_dropped`] - the CLI does, and says so out loud.
    fn record(&mut self, op: Op, token: &Token) {
        self.audit.record(Draft::new(op).args(describe(token)));
    }

    /// Lines this session wanted to write and could not. Non-zero after a change means the file and
    /// the trail disagree about that change, which is the one thing this pair must never do.
    pub fn trail_dropped(&self) -> u64 {
        self.audit.health().dropped_total
    }
}

/// One credential's policy as a summary line: enough to reconstruct the grant from the trail, and
/// nothing that unlocks anything.
fn describe(token: &Token) -> String {
    let workspaces: Vec<String> = token
        .workspaces
        .iter()
        .map(|workspace| workspace.text().replace(' ', "_"))
        .collect();
    format!(
        "id={} label={} tier={} workspaces={}",
        token.id,
        token.label,
        token.tier,
        workspaces.join(",")
    )
}

/// Where the credential file lives when nothing says otherwise, and the reason it is beside the
/// configuration rather than inside a workspace: `Config::mcp_token_file`'s parent directory is added
/// to the exclusion prefixes, so putting it anywhere a person keeps files would hide those files from
/// every listing.
pub fn default_file(config_dir: &Path) -> PathBuf {
    config_dir.join("mcp-tokens.toml")
}

/// Whether a path is under a directory, by identity rather than by spelling.
pub fn is_under(directory: &Path, path: &Path) -> bool {
    PathKey::of(directory).is_within(&PathKey::of(path))
}

fn write_then_rename(temp: &Path, target: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
    if let Err(error) = write_temp(temp, bytes) {
        // A half-written temporary is not a thing anyone will ever read, but leaving it behind means
        // the next save finds a file it did not create.
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    fs::rename(temp, target).map_err(|source| crate::Error::Io {
        path: target.to_path_buf(),
        source,
    })
}

fn write_temp(temp: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(temp)
        .map_err(|source| crate::Error::Io {
            path: temp.to_path_buf(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| crate::Error::Io {
            path: temp.to_path_buf(),
            source,
        })
}

/// The fixed widths a credential string is built from, asserted against `token.rs` so the two cannot
/// drift apart in silence.
const _: () = {
    assert!(ID_HEX_CHARS % 2 == 0, "an id of whole bytes");
    assert!(
        SECRET_BYTES >= 16,
        "at least 128 bits of machine randomness"
    );
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verb::Verb;

    struct Scratch {
        root: tempfile::TempDir,
    }

    impl Scratch {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().expect("tempdir"),
            }
        }

        /// The credential file and the audit directory are siblings, children of the temp dir - the
        /// same discipline the `fam-ffi` tests keep for the tree they list, so neither file can turn
        /// up as a row in a listing being asserted on.
        fn file(&self) -> PathBuf {
            self.root.path().join("auth/mcp-tokens.toml")
        }

        fn audit_dir(&self) -> PathBuf {
            self.root.path().join("auth-audit")
        }

        fn audit(&self) -> AuditLog {
            AuditLog::open(AuditOptions {
                directory: self.audit_dir(),
                actor: Actor::User,
                record_reads: true,
                daily_limit_bytes: 64 * 1024 * 1024,
                clock: fam_svc_audit::system_clock(),
            })
            .expect("the audit log opens")
        }

        fn store(&self) -> TokenStore {
            self.try_store().expect("an absent file is an empty store")
        }

        fn try_store(&self) -> Result<TokenStore, crate::Error> {
            TokenStore::at(self.file(), self.audit())
        }

        fn audit_lines(&self) -> Vec<fam_svc_audit::Record> {
            let mut out = Vec::new();
            for path in fam_svc_audit::files(&self.audit_dir()).unwrap_or_default() {
                let bytes = fs::read(&path).expect("readable");
                for line in bytes
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                {
                    out.push(serde_json::from_slice(line).expect("a record"));
                }
            }
            out
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_758_600_000_000)
    }

    #[test]
    fn a_granted_token_verifies_and_a_forged_one_does_not() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let issued = store
            .add("bot", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        assert_eq!(
            store.verify(&issued.credential()).map(|token| &token.id),
            Some(&issued.token.id)
        );
        let forged = issued.credential().replace(&issued.secret[..8], "ffffffff");
        assert!(store.verify(&forged).is_none(), "one wrong byte is enough");
        assert!(store.verify("fam_00000000_deadbeef").is_none());
        assert!(store.verify("").is_none());
        assert_eq!(issued.credential().split('_').count(), 3, "fam, id, secret");
    }

    /// The tier says what, the workspace says where, and both have to agree before anything happens.
    #[test]
    fn a_browse_token_cannot_write_and_an_edit_token_cannot_leave_its_folder() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let browse = store
            .add("reader", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let edit = store
            .add("writer", Tier::Edit, &["D:/notes".into()], now())
            .expect("granted");
        let roots = [PathBuf::from("D:/")];
        let reader = store.verify(&browse.credential()).expect("live");
        let writer = store.verify(&edit.credential()).expect("live");
        assert!(!reader.allows(Verb::FsDelete));
        assert!(reader.allows(Verb::FsList));
        assert!(writer.allows(Verb::FsDelete));
        assert!(writer.covers(Path::new("D:/notes/a.txt"), &roots));
        assert!(!writer.covers(Path::new("D:/other/a.txt"), &roots));
    }

    #[test]
    fn every_change_leaves_a_line_and_no_line_leaks_a_secret() {
        let scratch = Scratch::new();
        let issued;
        {
            let mut store = scratch.store();
            issued = store
                .add("trail", Tier::Edit, &["D:/notes".into()], now())
                .expect("granted");
            store.revoke(&issued.token.id, now()).expect("revoked");
        }
        let lines = scratch.audit_lines();
        let ops: Vec<&str> = lines.iter().map(|record| record.op.as_str()).collect();
        assert_eq!(
            ops,
            vec!["session.open", "token.add", "token.revoke", "session.close"]
        );
        let text = lines
            .iter()
            .map(|record| serde_json::to_string(record).expect("serialisable"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !text.contains(&issued.secret),
            "the plaintext must never reach the trail"
        );
        assert!(
            !text.contains(&issued.token.secret_hash),
            "nor the digest, which is what an offline attack would want"
        );
        let summary = lines[1].args.as_deref().expect("a summary");
        assert!(summary.contains("tier=edit"), "{summary}");
        assert!(summary.contains("label=trail"), "{summary}");
        assert_eq!(lines[1].actor, Actor::User, "a human ran this");
        let report = fam_svc_audit::verify(&scratch.audit_dir()).expect("reads");
        assert!(report.intact(), "{:#?}", report.findings);
    }

    #[test]
    fn a_revoke_stops_the_secret_immediately_and_permanently() {
        let scratch = Scratch::new();
        let issued;
        {
            let mut store = scratch.store();
            issued = store
                .add("kill", Tier::Admin, &["*".into()], now())
                .expect("granted");
            store.revoke(&issued.token.id, now()).expect("revoked");
            assert!(store.verify(&issued.credential()).is_none());
            assert!(store
                .find(&issued.token.id)
                .expect("the row stays")
                .revoked_at
                .is_some());
            assert!(
                matches!(
                    store.rotate(&issued.token.id),
                    Err(crate::Error::AlreadyRevoked(_))
                ),
                "revocation ends a credential's life"
            );
        }
        let reopened = scratch.store();
        assert!(
            reopened.verify(&issued.credential()).is_none(),
            "a second process agrees, because revoked_at is in the file"
        );
    }

    #[test]
    fn rotating_keeps_the_identity_and_changes_the_key() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let issued = store
            .add("spin", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let rotated = store.rotate(&issued.token.id).expect("rotated");
        assert_eq!(rotated.token.id, issued.token.id, "same id");
        assert_eq!(rotated.token.label, issued.token.label, "same label");
        assert_eq!(rotated.token.tier, issued.token.tier, "same tier");
        assert_eq!(
            rotated.token.workspaces, issued.token.workspaces,
            "same ground"
        );
        assert_ne!(rotated.secret, issued.secret, "new secret");
        assert!(store.verify(&rotated.credential()).is_some());
        assert!(store.verify(&issued.credential()).is_none());
    }

    #[test]
    fn a_label_is_taken_once_and_case_does_not_make_it_a_different_one() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let first = store
            .add("same", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        assert!(
            matches!(
                store.add("same", Tier::Browse, &["D:/notes".into()], now()),
                Err(crate::Error::LabelTaken(label)) if label == "same"
            ),
            "the message names the label that was already taken"
        );
        assert!(matches!(
            store.add("SAME", Tier::Browse, &["D:/notes".into()], now()),
            Err(crate::Error::LabelTaken(_))
        ));
        assert!(
            store.find(&first.token.id).is_some(),
            "nothing half-written"
        );
        assert_eq!(
            store.tokens().len(),
            1,
            "and the refused grant is not in memory either"
        );
    }

    /// The lost-update guard: a store that read the file before someone else changed it refuses to
    /// write, rather than quietly deleting their row.
    #[test]
    fn a_stale_store_refuses_to_write_rather_than_clobber() {
        let scratch = Scratch::new();
        let mut writer = scratch.store();
        writer
            .add("first", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let mut stale = scratch.store();
        let mut current = scratch.store();
        current
            .add("second", Tier::Edit, &["D:/notes".into()], now())
            .expect("granted");
        assert!(
            matches!(
                stale.add("third", Tier::Browse, &["D:/notes".into()], now()),
                Err(crate::Error::ConcurrentEdit { .. })
            ),
            "the stale store must refuse rather than drop the other process's row"
        );
        assert_eq!(
            stale.tokens().len(),
            1,
            "a refused save leaves the store holding the rows it read, not those plus the one it \
             tried to add - this assertion used to say 2, which was the bug"
        );
        assert_eq!(current.tokens().len(), 2);
        // The stale store reloads cleanly and can then proceed, which is what a person does after
        // being told: run it again.
        drop(stale);
        let mut fresh = scratch.store();
        assert!(fresh
            .add("third", Tier::Browse, &["D:/notes".into()], now())
            .is_ok());
    }

    #[test]
    fn a_duplicated_row_in_the_file_is_refused_on_load() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        store
            .add("dupe", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let text = fs::read_to_string(store.path()).expect("readable");
        let row = text.rsplit_once("[[token]]").expect("a row").1;
        fs::write(store.path(), format!("[[token]]{row}[[token]]{row}")).expect("writable");
        assert!(
            matches!(scratch.try_store(), Err(crate::Error::DuplicateId(_))),
            "the same id twice would make verify answer for only one of them"
        );
    }

    #[test]
    fn a_wildcard_workspace_needs_the_tier_that_may_hold_it() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        assert!(
            matches!(
                store.add("wide", Tier::Edit, &["*".into()], now()),
                Err(crate::Error::WildcardNeedsAdmin(Tier::Edit))
            ),
            "an edit token cannot buy the wildcard"
        );
        assert_eq!(store.tokens().len(), 0, "and nothing was written");
        assert!(store.add("wide", Tier::Admin, &["*".into()], now()).is_ok());
    }

    #[test]
    fn an_unparsable_file_is_an_error_not_an_empty_store() {
        let scratch = Scratch::new();
        fs::create_dir_all(scratch.file().parent().expect("a parent")).expect("a dir");
        fs::write(scratch.file(), "tier = not a table\n").expect("writable");
        let error = scratch
            .try_store()
            .expect_err("an unparsable file is not an empty store");
        assert!(matches!(error, crate::Error::Parse { .. }), "got {error:?}");
    }

    /// A hand-edit can put a wildcard on an `edit` row, which is the one thing the tier is supposed
    /// to make impossible. The row is refused on load rather than honoured.
    #[test]
    fn a_hand_edited_privilege_escalation_is_refused_on_load() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        store
            .add("sneaky", Tier::Edit, &["D:/notes".into()], now())
            .expect("granted");
        let text = fs::read_to_string(store.path()).expect("readable");
        fs::write(&store.path, text.replace("\"D:/notes\"", "\"*\"")).expect("writable");
        let error = scratch
            .try_store()
            .expect_err("the widened row must not load");
        // The refusal arrives wrapped in the file's own parse error, which is what a person editing
        // the file by hand needs: the row is not loaded, and the reason is the row's own.
        let message = error.to_string();
        assert!(matches!(error, crate::Error::Parse { .. }), "got {message}");
        assert!(
            message.contains("only an admin token may hold"),
            "the reason has to survive the wrap: {message}"
        );
    }

    #[test]
    fn the_temporary_file_never_survives_a_save() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        store
            .add("temp", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let leftovers: Vec<String> = fs::read_dir(scratch.file().parent().expect("a dir"))
            .expect("readable")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("mcp-tokens.toml") && name != "mcp-tokens.toml")
            .collect();
        assert!(
            leftovers.is_empty(),
            "a save leaves one file: {leftovers:?}"
        );
        assert!(scratch.file().exists());
    }

    /// Two changes in one process. The stale-file guard compares against what this store read, so
    /// unless a successful save moves that baseline forward, the second change in any session is
    /// refused - and the refusal looks exactly like somebody else editing the file.
    #[test]
    fn a_second_change_in_the_same_session_is_not_a_conflict() {
        let scratch = Scratch::new();
        let issued;
        {
            let mut first = scratch.store();
            issued = first
                .add("pair", Tier::Edit, &["D:/notes".into()], now())
                .expect("granted");
        }
        // Reopened, so the file existed when this store read it - that is the case with a baseline.
        let mut again = scratch.store();
        again.rotate(&issued.token.id).expect("rotated");
        again
            .revoke(&issued.token.id, now())
            .expect("revoked after rotating");
        let reopened = scratch.store();
        assert_eq!(
            reopened.tokens().len(),
            1,
            "both changes are on file, not just the first"
        );
        assert!(reopened.tokens()[0].revoked_at.is_some());
        assert!(
            reopened.verify(&issued.credential()).is_none(),
            "and the last one won"
        );
    }

    /// The window a first grant falls into: two processes both open an absent file, so neither has a
    /// baseline to compare against. "No baseline" must mean "the file still has to not be there",
    /// otherwise the later rename publishes a table the earlier one never read, and what gets lost is
    /// whoever revoked something.
    #[test]
    fn a_store_that_read_no_file_refuses_to_overwrite_one_that_appeared() {
        let scratch = Scratch::new();
        let mut first = scratch.store();
        let mut second = scratch.store();
        first
            .add("early", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        assert!(
            matches!(
                second.add("late", Tier::Browse, &["D:/notes".into()], now()),
                Err(crate::Error::ConcurrentEdit { .. })
            ),
            "the second process may not write a table it never saw"
        );
        assert_eq!(scratch.store().tokens().len(), 1);
    }

    /// A misspelled key is the mistake a person editing this file actually makes, and the ordinary
    /// serde answer to it - ignore the unknown field - is the direction that loses a revoke.
    #[test]
    fn a_misspelled_key_is_refused_rather_than_read_as_a_default() {
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let issued = store
            .add("typo", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        store.revoke(&issued.token.id, now()).expect("revoked");
        let text = fs::read_to_string(store.path()).expect("readable");
        assert!(text.contains("revoked_at"), "{text}");
        for spelling in ["revokedAt", "revoked", "revoked_at_"] {
            let widened = text.replace("revoked_at", spelling);
            fs::write(store.path(), &widened).expect("writable");
            let error = scratch
                .try_store()
                .expect_err("{spelling} must not read as a live row");
            assert!(
                matches!(error, crate::Error::Parse { .. }),
                "{spelling}: got {error:?}"
            );
        }
        // And the table itself: `[[tokens]]` is one character from `[[token]]`, and read as an absent
        // key it is an empty store - which the next `add` would then save over the real rows.
        fs::write(store.path(), text.replace("[[token]]", "[[tokens]]")).expect("writable");
        let error = scratch
            .try_store()
            .expect_err("a misspelled table is not an empty credential file");
        assert!(matches!(error, crate::Error::Parse { .. }), "got {error:?}");
    }

    /// Not a gate - a number, for the note that prices this layer. Run it with
    /// `cargo test --release -p fam-svc-auth --lib -- --ignored --nocapture`.
    ///
    /// The question worth a number is what a request pays for the credential check, since the check
    /// runs once per MCP call: the digest over ~110 ASCII bytes plus the compare, on rows already in
    /// memory. The file read happens once per process, not once per request.
    #[test]
    #[ignore]
    fn the_price_of_one_authentication() {
        const ROUNDS: u32 = 200_000;
        let scratch = Scratch::new();
        let mut store = scratch.store();
        let issued = store
            .add("priced", Tier::Browse, &["D:/notes".into()], now())
            .expect("granted");
        let good = issued.credential();
        let mut wrong = good.clone();
        let last = wrong.len() - 1;
        wrong.remove(last);
        wrong.push(if good.ends_with('0') { '1' } else { '0' });
        assert_ne!(good, wrong);
        let absent = format!("fam_01234567_{}", &good[13..]);

        let started = std::time::Instant::now();
        let mut hits = 0;
        for _ in 0..ROUNDS {
            hits += store.verify(&good).is_some() as u32;
        }
        let accepted = started.elapsed();
        let started = std::time::Instant::now();
        let mut misses = 0;
        for _ in 0..ROUNDS {
            misses += store.verify(&wrong).is_some() as u32;
        }
        let wrong_secret = started.elapsed();
        let started = std::time::Instant::now();
        for _ in 0..ROUNDS {
            misses += store.verify(&absent).is_some() as u32;
        }
        let unknown_id = started.elapsed();

        assert_eq!(hits, ROUNDS, "every good credential was accepted");
        assert_eq!(misses, 0, "and nothing else was");
        for (what, took) in [
            ("accept     ", accepted),
            ("wrong secret", wrong_secret),
            ("unknown id ", unknown_id),
        ] {
            println!(
                "{what}: {:>6.0} ns per check, {} rounds in {:.2?}",
                took.as_nanos() as f64 / f64::from(ROUNDS),
                ROUNDS,
                took
            );
        }
    }
}
