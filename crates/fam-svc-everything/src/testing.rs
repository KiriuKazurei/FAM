//! A stand-in for the Everything service, for the tests this machine cannot otherwise run.
//!
//! Everything is an outside process, and the development machine does not have one. That leaves the
//! interesting questions - does the router actually prefer the index when it is up? does a result
//! list longer than a page drain correctly? does a service that dies halfway fall back to
//! enumeration? - unanswerable, and answering them with a provider written from scratch would test a
//! shape that does not exist.
//!
//! So the fake is the *other* half of the real thing. Everything below [`drain`](crate::search::drain) is
//! untouched: the fake supplies rows the way the service's result list does, and the paging loop,
//! the offset arithmetic, the ceiling, the cancellation checks, the kind bucket and the re-check all
//! run as they do against a bound DLL.
//!
//! Deliberately, the fake **ignores the compiled query**. A real Everything answers from the whole
//! volume and knows nothing about the caller's roots or exclusions, so a fake that filtered its own
//! corpus to fit the request would hide exactly the hole the router's own scope check has to close.
//! Rows that do not match the request are dropped by
//! [`to_hit`](crate::search::to_hit) instead - which is what happens to the query layer's
//! over-approximation, so the two are tested by the same path.
//!
//! Public because the crate that routes between providers uses it, and `#[cfg(test)]` would not
//! reach that far. Production code has no reason to construct one.

use std::path::PathBuf;

use fam_core::{
    Cancellation, PathKey, SearchError, SearchHit, SearchOutcome, SearchProvider, SearchRequest,
};

use crate::query::compile;
use crate::search::{drain, RawRow};
use crate::volumes::{self, Volumes};

/// The separator the service writes between a result's directory and its name.
const SEPARATOR: u16 = b'\\' as u16;

/// A result list the fake hands out, plus the two facts that decide whether it gets asked.
pub struct FakeEverything {
    rows: Vec<RawRow>,
    /// Volume roots this index covers, for `covers`.
    indexed: Volumes,
    /// Roots the caller configured, for `covers` of an unscoped request.
    roots: Vec<PathKey>,
    /// Refusing from this offset onwards, as the service going away mid-search would.
    breaks_at: Option<u32>,
}

impl FakeEverything {
    /// An up-and-running index over `paths`, on every volume those paths name.
    ///
    /// Spell a directory with a trailing `/` to make the row a folder. Volume roots are taken from
    /// the paths themselves, so an index of `D:/…` rows answers as an index of the `D:` volume and
    /// not as one of `E:`.
    pub fn indexing<S: AsRef<str>>(paths: &[S]) -> Self {
        let rows = paths.iter().map(|path| row(path.as_ref())).collect();
        let indexed = Volumes::new(
            paths
                .iter()
                .filter_map(|path| drive_of(path.as_ref()))
                .collect::<Vec<_>>(),
        );
        Self {
            rows,
            indexed,
            roots: Vec::new(),
            breaks_at: None,
        }
    }

    /// The roots an unscoped request stands for, i.e. what the caller configured.
    pub fn rooted_at(mut self, roots: &[PathBuf]) -> Self {
        self.roots = roots.iter().map(|root| PathKey::of(root)).collect();
        self
    }

    /// Volume roots the index covers, overriding the ones read off `paths`.
    ///
    /// Without this there is no way to say "an index of `D:` that was asked about `E:`", because the
    /// corpus alone always covers itself.
    pub fn indexed_on(mut self, drives: &[&str]) -> Self {
        self.indexed = Volumes::new(drives.iter().map(|drive| PathBuf::from(*drive)));
        self
    }

    /// The service stops answering partway through: rows before `offset` arrive, later ones error.
    pub fn breaks_at(mut self, offset: u32) -> Self {
        self.breaks_at = Some(offset);
        self
    }

    /// Rows from `offset`, at most `want` of them - and fewer only when the list has run out, which
    /// is the whole contract [`drain`](crate::search::drain) depends on.
    fn whole_list(&self, offset: u32, want: u32) -> Vec<RawRow> {
        let start = offset as usize;
        let end = start.saturating_add(want as usize).min(self.rows.len());
        self.rows[start.min(self.rows.len())..end].to_vec()
    }
}

impl SearchProvider for FakeEverything {
    fn id(&self) -> &'static str {
        "everything"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn covers(&self, request: &SearchRequest) -> bool {
        volumes::covers(&self.indexed, &self.roots, request)
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        if request.is_empty() {
            return Err(SearchError::Empty);
        }
        // The same refusal the real provider makes: a request with no quotable term would, if sent,
        // bring back the whole index, and a fake that accepted it would hide the bug.
        compile(request).ok_or(SearchError::Empty)?;
        let breaks_at = self.breaks_at;
        drain(request, cancel, sink, |offset, want| match breaks_at {
            Some(stop) if offset >= stop => Err(SearchError::Unavailable {
                provider: "everything",
                reason: "the service went away mid-search".into(),
            }),
            _ => Ok(self.whole_list(offset, want)),
        })
    }
}

/// One result row, from the path a test spelled it as.
///
/// The size is the length of that spelling, which is all a test needs it to be: sizes and dates are
/// the service's business, and [`to_hit`](crate::search::to_hit) has its own tests for passing them through.
fn row(spec: &str) -> RawRow {
    let folder = spec.ends_with('/');
    let full = spec.trim_end_matches('/').replace('/', "\\");
    let units: Vec<u16> = full.encode_utf16().collect();
    let split = units.iter().rposition(|unit| *unit == SEPARATOR);
    let (path, name) = match split {
        Some(at) => (units[..at].to_vec(), units[at + 1..].to_vec()),
        None => (Vec::new(), units),
    };
    RawRow {
        name,
        path,
        folder,
        size: spec.len() as u64,
        modified_100ns: None,
    }
}

/// `D:/docs/a.txt` as `D:\`, the form [`Volumes`] compares roots in.
fn drive_of(path: &str) -> Option<PathBuf> {
    let drive = path.get(..2)?;
    drive
        .ends_with(':')
        .then(|| PathBuf::from(format!("{drive}\\")))
}

#[cfg(test)]
mod tests {
    use fam_core::{path_buf_from_units, Kind};

    use crate::search::PAGE;

    use super::*;

    fn hits(fake: &FakeEverything, text: &str) -> Vec<(PathBuf, Kind)> {
        let mut found = Vec::new();
        fake.search(
            &SearchRequest::new(text),
            &Cancellation::new(),
            &mut |hit| {
                found.push((path_buf_from_units(&hit.path), hit.kind));
                true
            },
        )
        .expect("the fake answers");
        found
    }

    /// The rows have to be the ones the service would send, or everything built on the fake proves
    /// nothing: a directory and a name arrive separately and are joined by a backslash, and the
    /// casing the user gave is kept.
    #[test]
    fn a_spelled_path_becomes_a_row_the_service_would_have_sent() {
        let fake = FakeEverything::indexing(&["D:/Docs/keep-one.pdf", "D:/Docs/keep-two/"]);
        assert_eq!(
            hits(&fake, "keep"),
            vec![
                (PathBuf::from(r"D:\Docs\keep-one.pdf"), Kind::Document),
                (PathBuf::from(r"D:\Docs\keep-two"), Kind::Directory),
            ],
            "a trailing slash is what makes a row a folder"
        );
    }

    /// The paging loop ends a list on a **short** page. A fake that always answered a full page would
    /// spin forever, so this is the fake's own contract being checked rather than assumed.
    #[test]
    fn the_list_ends_when_it_runs_out_rather_than_when_a_page_is_full() {
        let fake = FakeEverything::indexing(
            &(0..=PAGE)
                .map(|index| format!("D:/index/paged-{index}.txt"))
                .collect::<Vec<_>>(),
        );
        let outcome = fake
            .search(
                &SearchRequest::new("paged"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap();

        assert_eq!(outcome.matched, u64::from(PAGE) + 1);
        assert!(
            outcome.complete,
            "one full page then one row of one is an exhausted list"
        );
    }

    #[test]
    fn breaking_refuses_from_the_offset_it_names_onwards() {
        let corpus: Vec<String> = (0..=PAGE)
            .map(|index| format!("D:/index/paged-{index}.txt"))
            .collect();
        let fake = FakeEverything::indexing(&corpus).breaks_at(PAGE);
        let error = fake
            .search(
                &SearchRequest::new("paged"),
                &Cancellation::new(),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(
            matches!(&error, SearchError::Unavailable { provider, .. } if *provider == "everything"),
            "{error}"
        );
    }

    /// The refusal the real provider makes for the same reason: an unquotable request would otherwise
    /// bring back the whole index.
    #[test]
    fn a_request_with_nothing_to_ask_for_is_refused() {
        let fake = FakeEverything::indexing(&["D:/index/paged.txt"]);
        assert!(matches!(
            fake.search(
                &SearchRequest::new("   "),
                &Cancellation::new(),
                &mut |_| true,
            ),
            Err(SearchError::Empty)
        ));
    }
}
