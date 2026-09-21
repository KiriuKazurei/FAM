//! Turning an Everything result list into hits, a page at a time.
//!
//! Everything answers a query with one process-global result list, so a search is drained in pages
//! and each page is a bounded request (`SetOffset` + `SetMax`). The contract between the client and
//! this module is one sentence:
//!
//! > `fetch(offset, want)` returns at most `want` rows, and returns fewer only when the list is
//! > exhausted.
//!
//! That is what makes the loop testable on a machine with no Everything installed - which is this
//! one. Everything below `fetch` is ordinary code over a `Vec<RawRow>`: the offset arithmetic, the
//! limit accounting, the cancellation checks, the kind filter, and the re-check that turns the
//! query layer's deliberate over-approximation back into an exact answer.

use fam_core::{
    classify_units, path_buf_from_units, Cancellation, PathKey, SearchError, SearchHit,
    SearchOutcome, SearchRequest,
};

/// Rows one request asks for. Small enough that a search nobody cancels still fits in one bounded
/// IPC round trip, large enough that a big result set is not a request per row.
pub const PAGE: u32 = 1024;

/// The separator the service itself writes between a result's path and its name.
const SEPARATOR: u16 = std::path::MAIN_SEPARATOR as u16;

/// 100-nanosecond intervals between the `FILETIME` epoch (1601-01-01) and the Unix epoch.
const FILETIME_TO_UNIX_100NS: u64 = 116_444_736_000_000_000;

/// One row of the result list, as undecorated as the client can hand it over: the two strings the
/// service keeps separate, and the two attributes it may or may not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    pub name: Vec<u16>,
    /// The directory the name sits in, with no trailing separator. Empty for a volume row.
    pub path: Vec<u16>,
    pub folder: bool,
    pub size: u64,
    /// `FILETIME` as 100-nanosecond intervals since 1601-01-01, or `None` when the service had no
    /// date to give. A hit reports an unknown time as `i64::MIN`, the same convention enumeration
    /// uses, so the two sources cannot be told apart by a caller looking for a missing value.
    pub modified_100ns: Option<u64>,
}

impl RawRow {
    /// A row for tests and for callers that have a name and nothing else.
    pub fn file(name: &str, path: &str) -> Self {
        Self {
            name: name.encode_utf16().collect(),
            path: path.encode_utf16().collect(),
            folder: false,
            size: 0,
            modified_100ns: None,
        }
    }
}

/// One row of a result list as a hit, or `None` when the request does not want it.
///
/// The query layer quotes everything it cannot express and so may return rows the request would not
/// match; this is where they are dropped. It is also where the extension bucket is decided, which is
/// deliberately *not* pushed down as an `ext:` list (see [`crate::query`]).
pub fn to_hit(row: &RawRow, request: &SearchRequest) -> Option<SearchHit> {
    let kind = classify_units(&row.name, row.folder);
    if request.kind.is_some_and(|wanted| wanted != kind) {
        return None;
    }
    if !request.matches_name(&String::from_utf16_lossy(&row.name).to_lowercase()) {
        return None;
    }
    let mut full = row.path.clone();
    if !full.is_empty() {
        full.push(SEPARATOR);
    }
    full.extend_from_slice(&row.name);
    Some(SearchHit {
        key: PathKey::of(&path_buf_from_units(&full)),
        path: full,
        kind,
        size: row.size,
        mtime_ms: row.modified_100ns.map_or(i64::MIN, epoch_ms),
    })
}

/// Drains a result list into `sink`. `fetch` is the only part that touches the DLL.
///
/// `complete` means "the whole scope was covered", and three things deny it: the caller's signal
/// stopped us, the caller's ceiling was reached, or the sink stopped taking rows. All three are the
/// same statement the enumeration backend makes for the same reasons.
///
/// Cancellation is noticed **between** pages, not within one: a page the service already sent is
/// delivered in full rather than dropped halfway, which is the same granularity enumeration has when
/// it polls between directories. And a short page ends the list - that is the contract stated at the
/// top of this module, so a `fetch` that comes back short early reads as an exhausted list, not as a
/// bug.
pub fn drain(
    request: &SearchRequest,
    cancel: &Cancellation,
    sink: &mut dyn FnMut(SearchHit) -> bool,
    mut fetch: impl FnMut(u32, u32) -> Result<Vec<RawRow>, SearchError>,
) -> Result<SearchOutcome, SearchError> {
    let mut outcome = SearchOutcome::default();
    let ceiling = request.limit.min(u32::MAX as usize) as u32;
    let mut offset = 0u32;
    loop {
        if cancel.is_stopped() {
            return Ok(outcome);
        }
        // Ask for no more than the caller still wants, so a bounded search cannot make the service
        // assemble a page nobody will read.
        let want = match ceiling {
            0 => PAGE,
            limit => PAGE.min(limit.saturating_sub(outcome.matched.min(u32::MAX as u64) as u32)),
        };
        if want == 0 {
            return Ok(outcome);
        }
        let rows = fetch(offset, want)?;
        let got = rows.len() as u32;
        for row in rows {
            outcome.visited += 1;
            let Some(hit) = to_hit(&row, request) else {
                continue;
            };
            outcome.matched += 1;
            if !sink(hit) {
                return Ok(outcome);
            }
        }
        offset += got;
        if got < want {
            // The list ran out. Nothing was left unexamined, whatever the reason for asking.
            outcome.complete = true;
            return Ok(outcome);
        }
        if ceiling > 0 && outcome.matched >= ceiling as u64 {
            return Ok(outcome);
        }
    }
}

/// `FILETIME` to milliseconds since the Unix epoch.
fn epoch_ms(timestamp: u64) -> i64 {
    (timestamp as i64 - FILETIME_TO_UNIX_100NS as i64) / 10_000
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use super::*;

    fn request(text: &str) -> SearchRequest {
        SearchRequest::new(text)
    }

    /// Pages arrive until one comes back short, and each request starts where the last stopped.
    #[test]
    fn a_list_is_drained_page_by_page_and_reported_complete() {
        let asked = RefCell::new(Vec::new());
        let mut pages = vec![
            (0..PAGE)
                .map(|_| RawRow::file("a.txt", "C:/x"))
                .collect::<Vec<_>>(),
            (0..PAGE)
                .map(|_| RawRow::file("a.txt", "C:/x"))
                .collect::<Vec<_>>(),
            vec![RawRow::file("a.txt", "C:/x")],
        ];
        let outcome = drain(
            &request("a"),
            &Cancellation::new(),
            &mut |_| true,
            |offset, want| {
                asked.borrow_mut().push((offset, want));
                Ok(pages.remove(0))
            },
        )
        .expect("the fake never fails");

        assert_eq!(
            *asked.borrow(),
            vec![(0, PAGE), (PAGE, PAGE), (2 * PAGE, PAGE)],
            "the third request starts where the second ended"
        );
        assert_eq!(outcome.visited, 2 * PAGE as u64 + 1);
        assert_eq!(outcome.matched, outcome.visited);
        assert!(outcome.complete, "the list was walked to its end");
    }

    #[test]
    fn a_stopped_signal_keeps_the_rows_already_handed_over_and_says_it_is_partial() {
        let cancel = Cancellation::new();
        let mut seen = 0u64;
        let mut asked = 0usize;
        let outcome = drain(
            &request("a"),
            &cancel,
            &mut |_| {
                seen += 1;
                if seen == 3 {
                    cancel.cancel();
                }
                true
            },
            |_, want| {
                asked += 1;
                Ok((0..want).map(|_| RawRow::file("a.txt", "C:/x")).collect())
            },
        )
        .unwrap();

        assert_eq!(
            seen,
            u64::from(PAGE),
            "a page already in hand is delivered in full before the signal is noticed"
        );
        assert_eq!(asked, 1, "and the next page is never asked for");
        assert!(
            !outcome.complete,
            "the rest of the list was never looked at"
        );
    }

    /// A sink that stops taking rows is the caller's decision, not a failure - but it is a bounded
    /// answer, and the same rule the enumeration backend follows applies.
    #[test]
    fn a_sink_that_refuses_ends_the_drain_without_losing_what_it_took() {
        let mut taken = 0;
        let outcome = drain(
            &request("a"),
            &Cancellation::new(),
            &mut |_| {
                taken += 1;
                taken < 5
            },
            |_, _| Ok((0..100).map(|_| RawRow::file("a.txt", "C:/x")).collect()),
        )
        .unwrap();

        assert_eq!(taken, 5);
        assert_eq!(outcome.matched, 5);
        assert!(!outcome.complete);
    }

    #[test]
    fn a_ceiling_is_final_but_never_claims_the_scope() {
        let asked = RefCell::new(Vec::new());
        let outcome = drain(
            &request("a").limit(3),
            &Cancellation::new(),
            &mut |_| true,
            |_, want| {
                asked.borrow_mut().push(want);
                Ok((0..want).map(|_| RawRow::file("a.txt", "C:/x")).collect())
            },
        )
        .unwrap();

        assert_eq!(outcome.matched, 3);
        assert!(
            !outcome.complete,
            "three is what was asked for, not all there is"
        );
        assert_eq!(
            *asked.borrow(),
            vec![3],
            "and no page was ever asked for whole"
        );
    }

    #[test]
    fn rows_the_query_cannot_confirm_are_visited_not_matched() {
        // The compiled query is allowed to be a superset; this is where the excess is dropped.
        let outcome = drain(
            &request("needle"),
            &Cancellation::new(),
            &mut |_| panic!("nothing should reach a sink that asked for `needle`"),
            |_, _| {
                Ok(vec![
                    RawRow::file("haystack.txt", "C:/x"),
                    RawRow::file("other.txt", "C:/x"),
                ])
            },
        )
        .unwrap();

        assert_eq!(outcome.visited, 2, "they were looked at");
        assert_eq!(outcome.matched, 0, "and none of them matched");
        assert!(
            outcome.complete,
            "looking at all of them is still covering the list"
        );
    }

    #[test]
    fn the_kind_bucket_is_decided_here_not_in_the_query() {
        let mut want_image = request("report");
        want_image.kind = Some(fam_core::Kind::Image);
        let outcome = drain(&want_image, &Cancellation::new(), &mut |_| true, |_, _| {
            Ok(vec![
                RawRow::file("report.png", "C:/x"),
                RawRow::file("report.txt", "C:/x"),
            ])
        })
        .unwrap();
        assert_eq!(
            outcome.matched, 1,
            "the .txt is the query's over-approximation"
        );
    }

    #[test]
    fn a_hit_carries_the_joined_path_verbatim_and_a_folded_identity() {
        let row = RawRow {
            name: "Report-2026.PDF".encode_utf16().collect(),
            path: "C:\\Users\\Kirig\\Docs".encode_utf16().collect(),
            folder: false,
            size: 7,
            modified_100ns: None,
        };
        let hit = to_hit(&row, &request("report")).expect("the term is in the name");

        assert_eq!(
            path_buf_from_units(&hit.path),
            Path::new(r"C:\Users\Kirig\Docs\Report-2026.PDF")
        );
        assert_eq!(
            hit.key,
            PathKey::of(Path::new(r"c:\users\kirig\docs\report-2026.pdf"))
        );
        assert_eq!(hit.kind, fam_core::Kind::Document);
        assert_eq!(hit.size, 7);
        assert_eq!(
            hit.mtime_ms,
            i64::MIN,
            "a date the service did not have is not zero"
        );
    }

    #[test]
    fn a_volume_row_has_no_directory_to_join() {
        let row = RawRow {
            name: "Data (D:)".encode_utf16().collect(),
            path: Vec::new(),
            folder: true,
            size: 0,
            modified_100ns: None,
        };
        let hit = to_hit(&row, &request("data")).expect("a volume is a folder like any other");
        assert_eq!(String::from_utf16_lossy(&hit.path), "Data (D:)");
        assert_eq!(hit.kind, fam_core::Kind::Directory);
    }

    #[test]
    fn filetimes_cross_the_epoch_boundary_the_way_windows_means() {
        // 2026-01-01T00:00:00Z is 1767225600000 ms after the Unix epoch.
        assert_eq!(epoch_ms(134_116_992_000_000_000), 1_767_225_600_000);
        // The zero a caller may see from a row with no date is 1601, i.e. deeply negative - which is
        // why `modified_100ns` is an Option and not a 0 the UI would print as a year 1601 file.
        assert_eq!(epoch_ms(0), -11_644_473_600_000);
    }

    #[test]
    fn a_service_error_ends_the_search_with_the_error() {
        let error = drain(
            &request("a"),
            &Cancellation::new(),
            &mut |_| true,
            |_, _| Err(SearchError::Empty),
        )
        .expect_err("the fake said no");
        assert!(matches!(error, SearchError::Empty));
    }
}
