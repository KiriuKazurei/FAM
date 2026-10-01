use std::io;
use std::path::PathBuf;

use crate::cancel::Cancellation;
use crate::kind::Kind;
use crate::pathkey::PathKey;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchRequest {
    /// Lower-cased; every term must appear in the candidate name.
    pub terms: Vec<String>,
    /// Tag names the file must carry, **all of them**. A request naming any of these is answered
    /// by the store and not by a name index - see [`SearchProvider::handles_tags`].
    pub tags: Vec<String>,
    /// OR groups from `||`: the file must carry at least one name of each group. The comma binds
    /// tighter, so `tag:a,b||c` means "(a AND b) OR c".
    pub tag_groups: Vec<Vec<String>>,
    /// Excluded tag names from `!` - files carrying any of them are out of the answer.
    pub tags_not: Vec<String>,
    /// Directory subtree to confine the search to, spelled as the caller spelled it.
    ///
    /// Not a [`PathKey`], even though a key is the natural type for a directory: a provider that has
    /// to restart enumeration uses this as its root, and a root taken from a folded key reports the
    /// user's own folders back to them in lower case. Providers fold it themselves for the identity
    /// comparisons.
    pub scope: Option<PathBuf>,
    pub kind: Option<Kind>,
    pub limit: usize,
}

impl SearchRequest {
    /// What a caller types in front of a word to say "this is a tag, not part of a name".
    ///
    /// Typed rather than guessed: a box that treated every word as a possible tag would answer a name
    /// search with the store's shorter list, and the two answers are different questions - "files whose
    /// name contains this" against "files somebody labelled with this". The shell's filter bar needs no
    /// prefix, because clicking a chip *is* the statement.
    pub const TAG_PREFIX: &'static str = "tag:";
    /// The exclusion spelling, in front of the whole token (`!tag:a`) or a name (`tag:!a`).
    pub const TAG_NOT_PREFIX: &'static str = "!";
    /// Half-width only: the full-width `｜` is an ordinary character of a Chinese tag name.
    pub const TAG_OR: &'static str = "||";
    /// Half-width only: the full-width `，` is an ordinary character of a Chinese tag name.
    pub const TAG_AND: &'static str = ",";

    /// One tag name out of a comma/`||` group. A leading `!` marks the name excluded; an empty
    /// name (a doubled separator, a bare `!`) is dropped - the separators between nothing mean
    /// nothing, and the store rejects empty names on the way in anyway.
    fn push_tag_name(
        raw: &str,
        force_excluded: bool,
        included: &mut Vec<String>,
        excluded: &mut Vec<String>,
    ) {
        if raw.is_empty() {
            return;
        }
        match raw.strip_prefix(Self::TAG_NOT_PREFIX) {
            Some(name) if !name.is_empty() => excluded.push(name.to_string()),
            Some(_) => {}
            None => included.push(raw.to_string()),
        }
        if force_excluded {
            // `!tag:a` marks every name in the token excluded; move them across.
            excluded.extend(std::mem::take(included));
        }
    }

    pub fn new(text: &str) -> Self {
        let mut terms: Vec<String> = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        let mut tag_groups: Vec<Vec<String>> = Vec::new();
        let mut tags_not: Vec<String> = Vec::new();
        for token in text.split_whitespace() {
            // `!tag:name` excludes the name, same grammar as `tag:!name` inside the value.
            let (excluded, tag_token) = match token.strip_prefix(Self::TAG_NOT_PREFIX) {
                Some(rest) => (true, rest),
                None => (false, token),
            };
            let Some(value) = tag_token.strip_prefix(Self::TAG_PREFIX) else {
                terms.push(token.to_lowercase());
                continue;
            };
            // Verbatim, not lower-cased: the store matches a tag exactly (`tag IN (...)` over a
            // BINARY column), so folding the case here would turn `tag:PDF` into a search that
            // found nothing and read as an empty library rather than as a spelling that never
            // matched. The separators are half-width ASCII only - the full-width `，｜｜！` are
            // ordinary tag-name characters, which is exactly what a Chinese tag library needs.
            // A tag containing a half-width space cannot be typed this way; that is the bar's job.
            let groups: Vec<&str> = value.split(Self::TAG_OR).collect();
            if groups.len() == 1 {
                // No `||`: the comma members are one AND list.
                for name in value.split(Self::TAG_AND) {
                    Self::push_tag_name(name, excluded, &mut tags, &mut tags_not);
                }
                continue;
            }
            for group in groups {
                let mut members: Vec<String> = Vec::new();
                for name in group.split(Self::TAG_AND) {
                    Self::push_tag_name(name, excluded, &mut members, &mut tags_not);
                }
                if !members.is_empty() {
                    tag_groups.push(members);
                }
            }
        }
        Self {
            terms,
            tags,
            tag_groups,
            tags_not,
            scope: None,
            kind: None,
            limit: 0,
        }
    }

    pub fn scope(mut self, scope: impl Into<PathBuf>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// A tag the file must carry. Repeatable, and several of them mean all of them - the same
    /// agreement the name terms already keep, so `tag:a tag:b` narrows the way `a b` does.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// `0` means unbounded.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Whether this asks about tags at all - the question a name-only backend has to be asked before
    /// it is allowed to answer. An exclusion-only request (`tag:!a`) asks about tags too: it needs
    /// the library to know which files carry `a`.
    pub fn names_tags(&self) -> bool {
        !self.tags.is_empty() || !self.tag_groups.is_empty() || !self.tags_not.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && !self.names_tags()
    }

    /// `name` must already be lower-cased.
    pub fn matches_name(&self, name: &str) -> bool {
        self.terms.iter().all(|term| name.contains(term))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Folded identity, for a caller that has to match the hit against the directory cache.
    pub key: PathKey,
    /// Verbatim UTF-16 path as the filesystem reported it, separators included.
    ///
    /// This is not a convenience on top of `key`: a `PathKey` folds ASCII case because two spellings
    /// of one name are one file, and a filename label drawn from the fold tells the user their own
    /// folders are spelled in lower case. The base name is the part after the last separator.
    pub path: Vec<u16>,
    pub kind: Kind,
    pub size: u64,
    pub mtime_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOutcome {
    pub matched: u64,
    /// Entries looked at, including ones the query filtered out; never conflate with `matched`.
    pub visited: u64,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("query carries no search terms")]
    Empty,
    #[error("provider {provider} is unavailable: {reason}")]
    Unavailable {
        provider: &'static str,
        reason: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A name-search backend that pushes results instead of returning a collection.
///
/// A contract whose answer *is* a `Vec` makes streaming impossible by construction: no backend could
/// hand a result to the caller before its whole search finished, however soon the first match was
/// known, and a search of `D:\` is not something to make a user wait out. Returning `false` from
/// `sink` stops the provider, and `limit` on the request bounds how much a provider may visit.
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_available(&self) -> bool;

    /// Whether this backend can answer **all** of `request`.
    ///
    /// A backend that reaches only part of a scope says no rather than answering what it can and
    /// calling that complete: a shorter list that looks exhaustive is worse than a slower one that is
    /// not. (Merging several partial answers is a different thing, and is not implemented anywhere
    /// yet - so there is no third state to name.)
    fn covers(&self, request: &SearchRequest) -> bool {
        let _ = request;
        true
    }

    /// Whether this backend can restrict a search to files **tagged** with a name.
    ///
    /// A capability rather than one more `covers()` answer, because `covers` is about which part of the
    /// filesystem a backend reaches and this is about a field it does not have at all: a name index
    /// asked for `tag:合同` would return the files whose *names* contain 合同, which is a different
    /// question wearing the same shape, and it would report that answer as complete. The router does
    /// not ask such a backend at all - not even one the caller insisted on, since insisting on
    /// Everything does not make Everything know what anybody labelled.
    fn handles_tags(&self) -> bool {
        false
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_split_and_lowercased() {
        let request = SearchRequest::new("  Q1  Report ");
        assert_eq!(request.terms, vec!["q1", "report"]);
        assert!(!request.is_empty());
        assert!(SearchRequest::new("   ").is_empty());
    }

    #[test]
    fn a_tag_prefix_pulls_a_word_out_of_the_name_search() {
        let request = SearchRequest::new("pdf tag:合同 report");
        assert_eq!(request.terms, vec!["pdf", "report"]);
        // Verbatim: the store compares tags exactly, so the case the caller typed is the case asked
        // for. Lower-casing here would silently turn `tag:PDF` into a search of an empty library.
        assert_eq!(request.tags, vec!["合同"]);
        assert!(request.names_tags());
    }

    #[test]
    fn a_request_naming_only_a_tag_is_not_empty() {
        // The front ends refuse an empty request before starting a search, so a tag on its own has to
        // count as something asked - it is the bar's whole gesture: click a chip, search this label.
        let request = SearchRequest::new("tag:contract");
        assert!(request.terms.is_empty());
        assert!(!request.is_empty());
        // A bare prefix names the empty tag rather than swallowing the next word.
        assert!(SearchRequest::new("tag:").is_empty());
    }

    #[test]
    fn comma_joins_names_into_one_and_list() {
        let request = SearchRequest::new("tag:alpha,beta");
        assert_eq!(request.tags, vec!["alpha", "beta"]);
        assert!(request.tag_groups.is_empty());
        assert!(request.tags_not.is_empty());
    }

    #[test]
    fn double_bar_makes_an_or_group_and_the_comma_binds_tighter() {
        let request = SearchRequest::new("tag:alpha||beta");
        assert!(request.tags.is_empty());
        /* Two single-name groups: carry alpha or carry beta. */
        assert_eq!(request.tag_groups, vec![vec!["alpha"], vec!["beta"]]);

        let mixed = SearchRequest::new("tag:alpha,beta||gamma");
        assert!(mixed.tags.is_empty());
        /* The comma bound tighter inside the first group: (alpha AND beta) OR gamma. */
        assert_eq!(mixed.tag_groups, vec![vec!["alpha", "beta"], vec!["gamma"]]);
    }

    #[test]
    fn bang_excludes_and_both_spellings_agree() {
        let inner = SearchRequest::new("tag:!beta");
        assert!(inner.tags.is_empty());
        assert_eq!(inner.tags_not, vec!["beta"]);

        let outer = SearchRequest::new("!tag:beta");
        assert_eq!(outer.tags_not, vec!["beta"]);
    }

    #[test]
    fn full_width_separators_are_tag_name_characters_not_syntax() {
        let request = SearchRequest::new("tag:alpha，beta｜｜gamma！x");
        assert_eq!(request.tags, vec!["alpha，beta｜｜gamma！x"]);
        assert!(request.tag_groups.is_empty());
        assert!(request.tags_not.is_empty());
    }

    #[test]
    fn exclusion_only_requests_still_ask_the_library() {
        let request = SearchRequest::new("tag:!beta");
        assert!(request.names_tags());
        assert!(!request.is_empty());
    }

    #[test]
    fn empty_members_from_doubled_separators_are_dropped() {
        let request = SearchRequest::new("tag:alpha,,||,beta");
        /* Each group keeps its non-empty members: (alpha) OR (beta). */
        assert_eq!(request.tag_groups, vec![vec!["alpha"], vec!["beta"]]);
    }

    #[test]
    fn every_term_must_appear() {
        let request = SearchRequest::new("q1 report");
        assert!(request.matches_name("q1-report.pdf"));
        assert!(!request.matches_name("report.pdf"));
    }

    #[test]
    fn builders_compose() {
        let request = SearchRequest::new("a")
            .scope(PathBuf::from("D:/MixedCase"))
            .kind(Kind::Document)
            .limit(50);
        assert_eq!(request.limit, 50);
        assert_eq!(request.kind, Some(Kind::Document));
        assert_eq!(
            request.scope,
            Some(PathBuf::from("D:/MixedCase")),
            "the scope is kept as it was given, not folded"
        );
    }
}
