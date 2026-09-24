//! The scope rules: subtrees the user excluded, and directory names that mean "stop here".
//!
//! This used to live inside the walk policy, which was fine while enumeration was the only thing that
//! answered a search. It is not any more: an outside index - Everything's, say - knows nothing about a
//! user's exclusions, and a search routed through it would otherwise answer with files the same user
//! had deliberately kept out of every other listing. So the rules are a value of their own: the walker
//! uses them to prune while descending, and the router enforces them on whatever any backend hands
//! back.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::pathkey::{path_key, PathKey};

/// Which paths a search may report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prune {
    /// [`path_key`] forms; anything at or below one of them is out.
    prefixes: Vec<String>,
    /// Upper-cased directory names, matched at any depth.
    names: Vec<String>,
}

impl Prune {
    /// The rules in their normalised form: prefixes as path keys, names already upper-cased.
    pub fn new(prefixes: Vec<String>, names: Vec<String>) -> Self {
        Self { prefixes, names }
    }

    pub fn from_config(config: &Config) -> Self {
        let mut prefixes = config.excluded_dir_keys();
        // The audit log lives in the data directory, which on Windows is under the user's home -
        // and home is the default search root. Left in, the walk would enumerate the log, a search
        // would report it, and the tool would be reading its own trail. It is not a rule the user
        // configured, so it goes on the list here rather than in `excluded_dirs`, where
        // `fam config show` would present it as something they asked for.
        prefixes.push(path_key(&config.audit_dir));
        // The credential file lives beside the configuration, and a search that can name
        // `mcp-tokens.toml` is a search that can hand its contents to whoever asked. Excluding the
        // directory rather than the single file is deliberate: the walk enforces prefixes on the
        // directory it is about to enter, which is where it can refuse cheaply and completely - and
        // a lone file path in this list would be matched by nothing.
        if let Some(directory) = config.mcp_token_file.parent() {
            prefixes.push(path_key(directory));
        }
        Self {
            prefixes,
            names: config.excluded_name_keys(),
        }
    }

    /// Nothing is excluded, which is also "never ask the allocating predicates below".
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty() && self.names.is_empty()
    }

    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Whether a single directory component's name is one to stop at.
    ///
    /// Split out because a descending walk knows the component it is about to enter and does not need
    /// the whole path taken apart again.
    pub fn excludes_name(&self, name: &OsStr) -> bool {
        if self.names.is_empty() {
            return false;
        }
        let name = name.to_string_lossy().to_uppercase();
        self.names.contains(&name)
    }

    /// Whether a path key is at or below an excluded prefix.
    pub fn excludes_prefix(&self, key: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| crate::is_within(prefix, key))
    }

    /// True when `path` may not be reported: under an excluded prefix, or with an excluded directory
    /// name among its **ancestor** components.
    ///
    /// The last component is excluded from the name check because a path being reported ends at a
    /// file, and `excluded_names` is a list of directory names: a file that happens to be called
    /// `.git` is not a repository directory, and the walk - which asks this about directories it is
    /// about to enter, via [`Prune::excludes_name`] - would not skip it either.
    pub fn excludes(&self, path: &Path) -> bool {
        if self.is_empty() {
            return false;
        }
        self.excludes_prefix(&path_key(path))
            || path.parent().is_some_and(|parent| {
                parent
                    .components()
                    .any(|c| self.excludes_name(c.as_os_str()))
            })
    }

    /// The same question about a path already reduced to a key.
    ///
    /// This one allocates - the key is turned back into a path to take its components apart - because
    /// the name check is defined over components and a hit's key has no record of where they were.
    /// Search hands over at most a page of rows at a time, so the cost is bounded by the limit the
    /// caller set, not by anything on disk.
    pub fn excludes_key(&self, key: &PathKey) -> bool {
        self.excludes(&PathBuf::from(key.to_lossy_string()))
    }

    /// Whether `key` is excluded **as part of a search rooted at `boundary`**.
    ///
    /// The two rule lists answer differently once someone names the subtree they want, because they
    /// claim different things. An excluded *prefix* is an absolute subtree - "never show me
    /// `D:/secret`" - and a search rooted inside it is still inside it, so that rule keeps seeing the
    /// whole path. An excluded *name* is a rule about descending ("stop at `.git`"), which the walk
    /// enforces only on directories it enters and never on the root it was given. Naming
    /// `D:/proj/node_modules` as where to search is not a descent into it, so the name rule sees only
    /// the components below the boundary.
    ///
    /// Getting this wrong in either direction is invisible to the user: the search returns nothing and
    /// says it looked.
    pub fn excludes_below(&self, boundary: &PathKey, key: &PathKey) -> bool {
        if self.is_empty() {
            return false;
        }
        if !boundary.is_within(key) {
            // None of this row lies below the boundary the caller named, so there is no reading of
            // the rules narrower than the plain one, and answering "clear" here would be a claim
            // about a path this method has no business vouching for.
            return self.excludes_key(key);
        }
        if self.excludes_prefix(&key.to_lossy_string()) {
            return true;
        }
        if self.names.is_empty() {
            return false;
        }
        // Both keys are already folded, so the boundary really is a prefix of the key and the
        // remainder is exactly what lies below it.
        let full = key.to_path_buf();
        let below = full.strip_prefix(boundary.to_path_buf()).unwrap_or(&full);
        // `parent()` drops the last component, which is the file's own name: the list is of directory
        // names, for the reason given on [`Prune::excludes`].
        below.parent().is_some_and(|parent| {
            parent
                .components()
                .any(|component| self.excludes_name(component.as_os_str()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(root: &Path) -> Prune {
        Prune::new(
            vec![path_key(&root.join("excluded"))],
            vec!["NODE_MODULES".into(), ".GIT".into()],
        )
    }

    #[test]
    fn a_prefix_takes_its_whole_subtree_and_not_its_neighbour() {
        let root = Path::new("D:/proj");
        let rules = rules(root);
        assert!(rules.excludes(Path::new("D:/proj/excluded/deep/file.bin")));
        assert!(!rules.excludes(Path::new("D:/proj/excluded_sibling/file.bin")));
        assert!(!rules.excludes(Path::new("D:/proj/src/main.rs")));
    }

    #[test]
    fn an_excluded_name_counts_at_any_depth_and_ignores_case() {
        let rules = rules(Path::new("D:/proj"));
        assert!(rules.excludes(Path::new("D:/other/a/b/node_modules/x.js")));
        assert!(rules.excludes(Path::new("D:/other/.git/HEAD")));
        assert!(!rules.excludes(Path::new("D:/other/nodemodules/x.js")));
    }

    /// What the name list is *not*: a subtree to keep out. A file named after one of these is still a
    /// file, and the walk - which asks about the directory it is entering - would not skip it either.
    #[test]
    fn an_excluded_directory_name_does_not_exclude_a_whole_volume() {
        let rules = rules(Path::new("D:/proj"));
        assert!(rules.excludes(Path::new("D:/.git/config")));
        assert!(rules.excludes(Path::new("C:/Users/Kirig/node_modules/left-pad/index.js")));
    }

    #[test]
    fn a_file_called_by_an_excluded_name_is_still_a_file() {
        // `excluded_names` is a list of *directory* names; a file named `.git` is not a repository.
        let rules = rules(Path::new("D:/proj"));
        assert!(!rules.excludes(Path::new("D:/notes/.git")));
    }

    #[test]
    fn the_key_form_answers_the_same_question() {
        let rules = rules(Path::new("D:/proj"));
        assert!(rules.excludes_key(&PathKey::of(Path::new("D:/proj/excluded/x.bin"))));
        assert!(!rules.excludes_key(&PathKey::of(Path::new("D:/proj/src/main.rs"))));
    }

    #[test]
    fn no_rules_short_circuit_everything() {
        let none = Prune::default();
        assert!(none.is_empty());
        assert!(!none.excludes(Path::new("D:/anything/node_modules/x")));
        assert!(!none.excludes_name(OsStr::new("NODE_MODULES")));
    }

    #[test]
    fn a_name_predicate_does_not_need_the_whole_path() {
        let rules = rules(Path::new("D:/proj"));
        assert!(rules.excludes_name(OsStr::new("node_modules")));
        assert!(!rules.excludes_name(OsStr::new("src")));
    }

    /// The rule a scoped search depends on: the directory someone named is not one they descended
    /// into, so its own name cannot disqualify the search. Getting this wrong returns zero rows from
    /// `fam search react --in D:\proj\node_modules` and claims the folder was searched.
    #[test]
    fn a_named_boundary_is_not_excluded_by_its_own_name() {
        let rules = rules(Path::new("D:/proj"));
        let boundary = PathKey::of(Path::new("D:/proj/node_modules"));
        assert!(
            !rules.excludes_below(
                &boundary,
                &PathKey::of(Path::new("D:/proj/node_modules/react/index.js"))
            ),
            "the only excluded name on this path is the boundary itself"
        );
        assert!(
            rules.excludes_below(
                &boundary,
                &PathKey::of(Path::new("D:/proj/node_modules/react/.git/hooks/x"))
            ),
            "one *below* the boundary is a descent, and descents are still pruned"
        );
    }

    /// The other half: an excluded prefix is an absolute place, and naming your way into it does not
    /// un-exclude it.
    #[test]
    fn an_excluded_prefix_holds_inside_a_named_boundary() {
        let rules = rules(Path::new("D:/proj"));
        let boundary = PathKey::of(Path::new("D:/proj/excluded/deep"));
        assert!(rules.excludes_below(
            &boundary,
            &PathKey::of(Path::new("D:/proj/excluded/deep/x.bin"))
        ));
    }

    #[test]
    fn a_path_outside_the_boundary_is_answered_for_rather_than_silently_passed() {
        let rules = rules(Path::new("D:/proj"));
        // Not a case the router creates - it checks the boundary first - but a rule that quietly
        // said "fine" here would be a rule nobody could trust.
        assert!(rules.excludes_below(
            &PathKey::of(Path::new("D:/proj/src")),
            &PathKey::of(Path::new("D:/other/a/node_modules/x.js"))
        ));
    }

    /// The audit log and the credential file are in the exclusion list without anyone asking for
    /// them, because on Windows both live under the user's home directory - which is the default
    /// search root. Left in, the walk would enumerate them, a search would report them, and the tool
    /// would be reading its own trail and handing out its own keys.
    #[test]
    fn the_programs_own_directory_is_never_reported() {
        let home = Path::new("C:/Users/Kirig");
        let own = home.join("AppData/Roaming/FileAutoManage");
        let config = Config {
            search_roots: vec![home.to_path_buf()],
            audit_dir: own.join("audit"),
            mcp_token_file: own.join("mcp-tokens.toml"),
            database_path: own.join("tags.db"),
            ..Config::default()
        };
        let rules = Prune::from_config(&config);
        assert!(rules.excludes(&own.join("audit/audit-2026-09-23.jsonl")));
        assert!(rules.excludes(&own.join("audit")));
        assert!(rules.excludes(&own.join("mcp-tokens.toml")));
        // The credential file's directory is the data directory on Windows, so the tag database
        // inside it is excluded by the same rule. That is not an accident being tolerated: a listing
        // that can name `tags.db` is a listing that can hand over every tag assignment the user has
        // ever made, and increment 10's note lists it as the one remaining half of that question.
        assert!(rules.excludes(&own.join("tags.db")));
        // What the rule is not: a claim about everything the user owns.
        assert!(!rules.excludes(&home.join("Documents/notes.md")));
        assert!(!rules.excludes(&own.with_file_name("FileAutoManageBackup/notes.md")));
    }

    /// A configured `excluded_dirs` entry and the automatic ones are the same kind of rule, so the
    /// user's list survives the injection - and each automatic prefix is its own entry, so removing
    /// one cannot take the others with it.
    #[test]
    fn the_configured_exclusions_and_the_automatic_ones_are_all_honoured() {
        let config = Config {
            excluded_dirs: vec![PathBuf::from("D:/secret")],
            audit_dir: PathBuf::from("D:/audit"),
            mcp_token_file: PathBuf::from("D:/state/mcp-tokens.toml"),
            ..Config::default()
        };
        let rules = Prune::from_config(&config);
        assert_eq!(rules.prefixes().len(), 3, "one configured, two automatic");
        assert!(rules.excludes(Path::new("D:/secret/key.pem")));
        assert!(rules.excludes(Path::new("D:/audit/audit-2026-09-23.jsonl")));
        assert!(rules.excludes(Path::new("D:/state/mcp-tokens.toml")));
        assert!(!rules.excludes(Path::new("D:/work/notes.md")));
    }
}
