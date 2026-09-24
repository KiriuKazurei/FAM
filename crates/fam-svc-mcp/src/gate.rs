//! The gate: four questions every path-bearing tool call has to pass, in one place.
//!
//! The order is the one the plan sets, and it is not arbitrary - cheapest and least-informative
//! first, so a refusal tells a caller as little as possible while costing the server as little as
//! possible:
//!
//! 1. **tier** - does this token's whitelist contain this verb at all?
//! 2. **scope** - is this path inside one of the workspaces this token was granted?
//! 3. **exclusion** - is this path the program's own state (the audit trail, the credential file)?
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

use fam_core::{PathKey, Prune};
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
}

impl Gate {
    pub fn new(roots: &[PathBuf], prune: Prune) -> Self {
        Self {
            roots: roots.to_vec(),
            prune,
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
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
        if self.prune.excludes(path) {
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
                // This root does not work, but another granted workspace might: a token with two
                // workspaces must not be refused by whichever one the path happened to match first.
                Err(at) => {
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
    fn first_escape(&self, root: &Path, path: &Path) -> Result<(), PathBuf> {
        let mut cursor = PathBuf::from(root);
        let mut link: Option<PathBuf> = None;
        for component in below(root, path) {
            cursor.push(component.as_os_str());
            if is_link(&cursor) {
                link = Some(cursor);
                break;
            }
        }
        // The walk pushes every component below the root *including the last one*, so the leaf is
        // already covered: a symlinked file costs the same stat the loop just spent. A separate leaf
        // check here would be a second metadata call per request, and one metadata call on this
        // machine measures 120-130 µs across runs - the whole gate is paid for in those.
        let Some(link) = link else {
            return Ok(());
        };

        let real_root = match canonical(root) {
            Some(resolved) => resolved,
            // A granted root that does not exist cannot be escaped from, and a token granted a folder
            // that has not been created yet is the ordinary case - the grant is for next week's
            // directory.
            None => return Err(link),
        };
        let root_key = PathKey::of(&real_root);
        // A dangling link has no target, and a target that cannot be resolved cannot be *proven* to
        // be inside. Refuse it: the alternative is to let an agent walk through a link whose
        // destination this process cannot name.
        match canonical(&link) {
            Some(resolved) if root_key.is_within(&PathKey::of(&resolved)) => Ok(()),
            _ => Err(link),
        }
    }
}

/// Whether this name is a reparse point - a symlink, a junction, or an alias.
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
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

/// The components of `path` below `root`. `Path::strip_prefix` is used rather than a component count,
/// because the two spellings of one directory differ in separators and case and the count would then
/// skip the wrong number of parts.
fn below<'a>(root: &Path, path: &'a Path) -> Vec<Component<'a>> {
    match path.strip_prefix(root) {
        Ok(rest) => rest.components().collect(),
        Err(_) => path.components().collect(),
    }
}

fn canonical(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}
