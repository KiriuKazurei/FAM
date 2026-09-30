//! The gate: four questions every path-bearing tool call has to pass, in one place.
//!
//! The order is the one the plan sets, and it is not arbitrary - cheapest and least-informative
//! first, so a refusal tells a caller as little as possible while costing the server as little as
//! possible:
//!
//! 1. **tier** - does this token's whitelist contain this verb at all?
//! 2. **scope** - is this path inside one of the workspaces this token was granted?
//! 3. **exclusion** - is this path the program's own state (its audit trail, its credential file,
//!    its tag database)?
//! 4. **reparse** - does the path *actually* stay inside, once the filesystem resolves it?
//!
//! Question 3 is here because the answer to 2 alone was not enough. `fam_open_dir` lists a directory
//! without consulting `Prune` at all, and once this layer grew an `fs_read`, "the credential file is
//! not searchable" and "the credential file cannot be read" stopped being the same sentence. The
//! review of increment 11 recorded that as the item to close at exactly this moment, so it closes
//! here: an admin token holding `*` still cannot read `mcp-tokens.toml`.
//!
//! Question 4 is here because containment on a spelling is containment on a spelling. A junction
//! under `D:/granted` that points at `C:/Windows` passes 2 - the path does start with the granted
//! directory - and then leaves it. So each component below the root is checked for being a reparse
//! point, and the whole path is resolved and re-checked: the "必要时解析真实路径再判一次" the plan
//! asks for.
//!
//! What this does not do is resolve a path that does not exist yet. `fs_create` of a new file inside
//! a directory that turns out to be a link out is caught by the component walk, not by a final
//! `canonicalize` - which cannot succeed on a path nothing owns yet.

use std::fs;
use std::path::{Component, Path, PathBuf};

use fam_core::{Config, PathKey, Prune};
use fam_svc_auth::{Token, Verb, Workspace};

/// Why a call was refused. The text goes to the agent, so it says what is true about the request and
/// nothing more about the credential than its owner already knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The verb is not on this tier's whitelist. Says nothing about the path, which the caller has
    /// not earned the right to hear about yet.
    TierLacks {
        verb: &'static str,
        tier: &'static str,
    },
    Outside {
        path: PathBuf,
    },
    /// The path is the program's own state. Kept distinct from `Outside` because the fix is
    /// different - moving the workspace does not help - and a caller that cannot tell the two apart
    /// will retry forever.
    Excluded {
        path: PathBuf,
    },
    /// A link or junction at or below the granted root leads out of it. Names the component that
    /// crossed, because that is the one a person can go and look at.
    Reparse {
        at: PathBuf,
    },
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::TierLacks { verb, tier } => format!(
                "a {tier} token cannot call {verb}; its tier allows a different set of tools"
            ),
            Self::Outside { path } => format!(
                "{} is not inside any workspace this credential was granted",
                path.display()
            ),
            Self::Excluded { path } => format!(
                "{} is this program's own state and is not reachable through the tools",
                path.display()
            ),
            Self::Reparse { at } => format!(
                "{} is a link or junction that leaves the granted workspace",
                at.display()
            ),
        }
    }
}

pub struct Gate {
    /// The roots the program manages, which is what a `*` workspace expands to.
    roots: Vec<PathBuf>,
    prune: Prune,
    /// This program's own files, named exactly rather than by folder. `Prune` can only exclude a
    /// directory and everything under it, which is the right shape for the audit trail and the
    /// credential folder and the wrong shape for the tag database: a database file sitting *above*
    /// a search root would exclude that whole root and answer "no results" for a folder that is
    /// full. So the database joins the gate's own list instead of the walk's.
    own: Vec<PathKey>,
}

impl Gate {
    pub fn new(roots: &[PathBuf], prune: Prune, own: &[PathBuf]) -> Self {
        Self {
            roots: roots.to_vec(),
            prune,
            own: own.iter().map(|path| PathKey::of(path.as_path())).collect(),
        }
    }

    /// The files that belong to this program rather than to the data it is showing. The three SQLite
    /// names, because a database is a header plus a write-ahead log plus a shared-memory file and
    /// all three hold rows.
    pub fn program_files(config: &Config) -> Vec<PathBuf> {
        let database = &config.database_path;
        [
            Some(database.to_path_buf()),
            sidecar(database, "-wal"),
            sidecar(database, "-shm"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Whether this path is the program's own state: inside a directory it keeps, or one of the
    /// files it writes for itself.
    fn own_state(&self, path: &Path) -> bool {
        self.prune.excludes(path) || self.own.iter().any(|key| key == &PathKey::of(path))
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The tier question alone, for the verbs that name no path and so never reach `admit`'s
    /// later questions. `whoami` is held by every tier, so this had no caller until a no-path
    /// write arrived - and a write is exactly the call that must not skip the only question
    /// left to ask it.
    pub fn admit_verb(&self, token: &Token, verb: Verb) -> Result<(), Refusal> {
        if !token.allows(verb) {
            return Err(Refusal::TierLacks {
                verb: verb.name(),
                tier: token.tier.name(),
            });
        }
        Ok(())
    }

    /// The four questions, in order. `Ok(())` means the call may go ahead on `path` as spelled.
    pub fn admit(&self, token: &Token, verb: Verb, path: &Path) -> Result<(), Refusal> {
        if !token.allows(verb) {
            return Err(Refusal::TierLacks {
                verb: verb.name(),
                tier: token.tier.name(),
            });
        }
        if !verb.names_a_path() {
            // `whoami` claims no path, so there is nothing left to ask and no reason to run the path
            // questions over an empty argument.
            return Ok(());
        }
        if !token.covers(path, &self.roots) {
            return Err(Refusal::Outside {
                path: path.to_path_buf(),
            });
        }
        if self.own_state(path) {
            return Err(Refusal::Excluded {
                path: path.to_path_buf(),
            });
        }
        self.stays_inside(path, token)
    }

    /// Every component below a granted root, and the path itself, resolved against the filesystem.
    fn stays_inside(&self, path: &Path, token: &Token) -> Result<(), Refusal> {
        let mut escape = None;
        for root in granted(token, &self.roots) {
            let root_key = PathKey::of(&root);
            if !root_key.is_within(&PathKey::of(path)) {
                continue;
            }
            match self.first_escape(&root, path) {
                Ok(()) => return Ok(()),
                // The program's own state is not a property of which workspace was tried: it holds
                // for every root the token has, so this answer goes out unchanged.
                Err(Crossing::Excluded(at)) => {
                    return Err(Refusal::Excluded { path: at });
                }
                Err(Crossing::Link(at)) => {
                    // This root does not work, but another granted workspace might: a token with two
                    // workspaces must not be refused by whichever one the path happened to match first.
                    if escape.is_none() {
                        escape = Some(at);
                    }
                }
            }
        }
        match escape {
            Some(at) => Err(Refusal::Reparse { at }),
            // `covers` agreed the path is in scope but no granted root contains it - which is the
            // same answer as being outside, and gets the same message.
            None => Err(Refusal::Outside {
                path: path.to_path_buf(),
            }),
        }
    }

    /// `Ok(())` when the path stays inside `root` once the filesystem resolves it.
    ///
    /// The resolving only happens if a link is actually on the path. That is not a micro-optimisation
    /// that was added later: the first version called `canonicalize` twice per check unconditionally
    /// and measured 648 µs for one admission - sixty times the cost of writing the audit line that
    /// follows it. `symlink_metadata` per component costs a few microseconds, and a path with no
    /// reparse point on it cannot resolve to anywhere its own spelling does not point.
    fn first_escape(&self, root: &Path, path: &Path) -> Result<(), Crossing> {
        let Some(components) = below(root, path) else {
            // The two spellings do not line up - a granted `\\?\C:\dir` against a requested
            // `C:\dir`, say - even though the folded identity says they name the same folder, so
            // there is no telling which parts of the request are "below the root". Resolve the whole
            // thing and judge the result: this is the case where paying for a `canonicalize`
            // unconditionally is the only safe answer, and it cannot be reached by a caller who
            // spells their path the way the grant reads.
            let resolved = canonical(path).unwrap_or_else(|| path.to_path_buf());
            return self.judge(root, &resolved);
        };
        let mut cursor = PathBuf::from(root);
        let mut hops = 0;
        for component in components {
            cursor.push(component.as_os_str());
            if !is_link(&cursor) {
                if hops > 0 {
                    // Every step *after* a link is a step somewhere the caller did not name. The
                    // two questions the caller's own spelling already answered - "not this program's
                    // state", "still inside the grant" - have to be asked again about where the walk
                    // has actually arrived, or a junction standing in a granted folder is a door.
                    self.judge(root, &cursor)?;
                }
                continue;
            }
            hops += 1;
            if hops > MAX_LINK_HOPS {
                // `a -> b` and `b -> a` would spin here forever.
                return Err(Crossing::Link(cursor));
            }
            let Some(resolved) = canonical(&cursor) else {
                // A dangling link has no target, and a target that cannot be resolved cannot be
                // *proven* to be inside. Refuse it: the alternative is to let an agent walk through
                // a link whose destination this process cannot name.
                return Err(Crossing::Link(cursor));
            };
            cursor = resolved;
            self.judge(root, &cursor)?;
        }
        Ok(())
    }

    /// Where the walk is standing, asked against the granted root: still inside, and still not this
    /// program's own state.
    fn judge(&self, root: &Path, here: &Path) -> Result<(), Crossing> {
        if self.own_state(here) {
            return Err(Crossing::Excluded(here.to_path_buf()));
        }
        let real_root = match canonical(root) {
            Some(resolved) => resolved,
            // A granted root that does not exist cannot be escaped from, and a token granted a folder
            // that has not been created yet is the ordinary case - the grant is for next week's
            // directory.
            None => return Ok(()),
        };
        if PathKey::of(&real_root).is_within(&PathKey::of(here)) {
            Ok(())
        } else {
            Err(Crossing::Link(here.to_path_buf()))
        }
    }
}

/// What the component walk found. Private to this file because the difference only matters to
/// [`Gate::stays_inside`]: an exclusion is final, while a link is a reason to try the token's next
/// granted workspace before refusing.
enum Crossing {
    /// A reparse point at or below the granted root leads out of it.
    Link(PathBuf),
    /// The walk landed inside this program's own state.
    Excluded(PathBuf),
}

/// A guard against a pair of links that point at each other, not a policy about depth. Real chains
/// are one or two links long; `canonicalize` has its own kernel limit and reports failure there.
const MAX_LINK_HOPS: u32 = 32;

/// Whether this name is a reparse point. Deliberately wider than `file_type().is_symlink()`, which
/// answers only for symlink and mount-point tags: the listing this gate protects reports *any*
/// reparse point (`ItemFlags::REPARSE`), so a tag that redirects without being a symlink must not be
/// the one thing the gate assumes cannot exist.
fn is_link(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT, 0x400 - the same bit the enumeration layer turns into
        // `ItemFlags::REPARSE`.
        meta.file_attributes() & 0x400 != 0 || meta.file_type().is_symlink()
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// The directories this token's scope names: the paths it was granted, plus the configured roots when
/// it may hold `*`.
fn granted(token: &Token, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut granted: Vec<PathBuf> = token
        .workspaces
        .iter()
        .filter_map(|workspace| match workspace {
            Workspace::Path(path) => Some(path.clone()),
            Workspace::All => None,
        })
        .collect();
    if token.tier.may_scope_everything() {
        granted.extend(roots.iter().cloned());
    }
    granted
}

/// The components of `path` below `root`, or `None` when the two spellings cannot be lined up.
///
/// `Path::strip_prefix` is used rather than a component count because a count is not stable across
/// spellings the folded identity calls one folder: `\\?\C:\dir` and `C:\dir`, or a grant stored in
/// another case. `None` is not a refusal - the caller resolves the whole path and judges that - but
/// it must never be answered by walking `path`'s components *onto* `root`, which would build a path
/// nobody asked about, find no links on it, and report the check as passed.
fn below<'a>(root: &Path, path: &'a Path) -> Option<Vec<Component<'a>>> {
    path.strip_prefix(root)
        .ok()
        .map(|rest| rest.components().collect())
}

fn canonical(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}

/// `tags.db` beside a write-ahead log is `tags.db-wal` - SQLite's own spelling, which is not an
/// extension and so cannot be reached with `with_extension`.
fn sidecar(path: &Path, suffix: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_os_string();
    let mut name = name.into_string().ok()?;
    name.push_str(suffix);
    Some(path.parent()?.join(name))
}
