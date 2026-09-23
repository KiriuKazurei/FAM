//! One line of the audit log: what was asked, by whom, and what came back.
//!
//! A record is JSON on a single line, and the line's own bytes are what the next record hashes, so
//! the serialization here *is* the format. Two rules follow from that and are enforced below rather
//! than by convention: nothing is ever written with `format!` (only `serde_json` escapes a stray
//! quote or newline, and a hand-built line is how a log gets forged), and every free-text field is
//! clipped, so one call cannot make an arbitrarily large line.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::actor::Actor;

/// Ceiling on a target path in a record. A verbatim Windows path can reach 32 K characters; a
/// clipped one still identifies the subtree, and the marker says it was clipped.
const MAX_TARGET_CHARS: usize = 512;
/// Ceiling on the request summary and on the result text, for the same reason.
const MAX_TEXT_CHARS: usize = 240;

/// Whether a call is the question, the answer, or the heartbeat.
///
/// The distinction is the volume control, and it is worth three classes rather than two because the
/// three kinds of call cost the log very different amounts. Opening a folder, starting a search and
/// changing a tag are things a person decided to do; they are rare and each one is a fact. Fetching a
/// window of rows is how much of an answer actually reached the screen, which is the answer to "what
/// did it look at" and is therefore switchable. Asking whether anything has changed yet - the poll
/// and the header read a view does on every beat - is neither: at four beats a second per open
/// column, that stream is several megabytes an hour of the word "no", and what a beat reports when
/// something *did* change arrives as a fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Ask,
    Read,
    Poll,
}

/// The calls an audit line can stand for. Names are on the wire, so they are part of the format:
/// dotted, lower-case, and stable across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Stats,
    Backends,
    TagAdd,
    TagList,
    TagRemove,
    TagFind,
    DirOpen,
    DirState,
    DirPoll,
    DirFetch,
    DirSort,
    DirFilter,
    DirClose,
    SearchBegin,
    SearchPage,
    SearchCancel,
    SearchClose,
    /// Written by the log itself, once per session, before any call is recorded.
    SessionOpen,
    /// The seal: written when the session closes, and the reason a truncated log is noticeable.
    SessionClose,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Backends => "backends",
            Self::TagAdd => "tag.add",
            Self::TagList => "tag.list",
            Self::TagRemove => "tag.remove",
            Self::TagFind => "tag.find",
            Self::DirOpen => "dir.open",
            Self::DirState => "dir.state",
            Self::DirPoll => "dir.poll",
            Self::DirFetch => "dir.fetch",
            Self::DirSort => "dir.sort",
            Self::DirFilter => "dir.filter",
            Self::DirClose => "dir.close",
            Self::SearchBegin => "search.begin",
            Self::SearchPage => "search.page",
            Self::SearchCancel => "search.cancel",
            Self::SearchClose => "search.close",
            Self::SessionOpen => "session.open",
            Self::SessionClose => "session.close",
        }
    }

    pub fn weight(self) -> Weight {
        match self {
            Self::DirFetch | Self::SearchPage => Weight::Read,
            Self::DirState | Self::DirPoll => Weight::Poll,
            _ => Weight::Ask,
        }
    }
}

/// A record on its way to the log: everything the writer does not supply itself. The fields are
/// readable inside the crate because the writer turns exactly one of these into a `Record`; nobody
/// outside builds a `Record` by hand.
#[derive(Debug, Clone)]
pub struct Draft {
    op: Op,
    pub(crate) target: Option<String>,
    pub(crate) args: Option<String>,
    pub(crate) result: String,
}

impl Draft {
    pub fn new(op: Op) -> Self {
        Self {
            op,
            target: None,
            args: None,
            result: "ok".into(),
        }
    }

    /// Stored as a [`fam_core::path_key`], the same identity the index and the tag store use: the
    /// audit trail has to be joinable against "which folder is this", and two spellings of one path
    /// would be two answers. The fold is ASCII-only, so it cannot merge two distinct paths.
    pub fn target(mut self, path: &Path) -> Self {
        self.target = Some(clip(&fam_core::path_key(path), MAX_TARGET_CHARS));
        self
    }

    pub fn args(mut self, summary: impl std::fmt::Display) -> Self {
        let text = summary.to_string();
        if !text.is_empty() {
            self.args = Some(clip(&text, MAX_TEXT_CHARS));
        }
        self
    }

    pub fn result(mut self, outcome: impl std::fmt::Display) -> Self {
        self.result = clip(&outcome.to_string(), MAX_TEXT_CHARS);
        self
    }

    pub fn op(&self) -> Op {
        self.op
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Position within its session, from 1. Gaps are as loud as a broken link.
    pub seq: u64,
    /// UTC, to the millisecond. Local time would make the day boundary move with the calendar, and
    /// an audit trail that cannot say when two records happened relative to each other is worthless.
    pub ts: String,
    pub actor: Actor,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub args: Option<String>,
    pub result: String,
    pub session: u64,
    /// Hash of the previous line of this session, or [`crate::chain::GENESIS`].
    pub prev: String,
    /// Records this log could not write since the last line that did land.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub dropped: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dropped_because: Option<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl Record {
    /// The bytes this record occupies in the file, without the newline: what the next record hashes.
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        // `to_string` rather than a hand-built line: serde_json escapes control characters and
        // quotes, so a path or a label cannot end a line early and start a plausible-looking one
        // after it.
        serde_json::to_string(self)
    }
}

/// A prefix that says it is a prefix. Counted in characters, not bytes, so a multi-byte path is not
/// cut in the middle of a code unit.
pub fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let head: String = text.chars().take(keep).collect();
    format!("{head}~")
}

/// Milliseconds since the Unix epoch, or `None` for a clock before it.
pub fn epoch_ms(at: SystemTime) -> Option<u64> {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// The calendar date in UTC, from milliseconds since the epoch.
///
/// Days-from-civil after Howard Hinnant's algorithm: no month table, no leap-year special case to
/// get wrong, and correct for the whole range a real clock will produce.
pub fn utc_date(at_ms: u64) -> (i64, u32, u32) {
    civil_from_days((at_ms / 86_400_000) as i64)
}

/// `2026-09-23T04:11:07.415Z`
pub fn format_utc(at_ms: u64) -> String {
    let (year, month, day) = utc_date(at_ms);
    let seconds_of_day = (at_ms % 86_400_000) / 1000;
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );
    let millis = at_ms % 1000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    // The year this counts from starts in March, so January and February belong to the year above.
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_are_named_and_weighed() {
        assert_eq!(Op::DirOpen.name(), "dir.open");
        // The answer, switchable.
        assert_eq!(Op::DirFetch.weight(), Weight::Read);
        assert_eq!(Op::SearchPage.weight(), Weight::Read);
        // The heartbeat, never a line.
        assert_eq!(Op::DirState.weight(), Weight::Poll);
        assert_eq!(Op::DirPoll.weight(), Weight::Poll);
        // The question is always recorded.
        assert_eq!(Op::SearchBegin.weight(), Weight::Ask);
        assert_eq!(Op::TagAdd.weight(), Weight::Ask);
        assert_eq!(Op::DirClose.weight(), Weight::Ask);
        assert_eq!(Op::SessionClose.weight(), Weight::Ask);
    }

    #[test]
    fn a_target_is_the_folded_identity_and_clips_gracefully() {
        let draft = Draft::new(Op::DirOpen).target(Path::new("D:\\Datas\\Notes.TXT"));
        let record = completed(&draft, 1);
        assert_eq!(record.target.as_deref(), Some("D:/DATAS/NOTES.TXT"));
        let deep = format!("d:/{}", "x".repeat(5000));
        let long = Draft::new(Op::DirOpen).target(Path::new(&deep));
        let record = completed(&long, 2);
        let target = record.target.expect("set");
        assert_eq!(target.chars().count(), MAX_TARGET_CHARS);
        assert!(target.ends_with('~'));
    }

    #[test]
    fn an_empty_summary_is_left_out_rather_than_written_as_empty() {
        let record = completed(&Draft::new(Op::Stats).args(""), 1);
        assert!(record.args.is_none());
        let record = completed(&Draft::new(Op::DirFetch).args("24+80"), 2);
        assert_eq!(record.args.as_deref(), Some("24+80"));
    }

    /// The forgery check: a caller that hands over a path or a query containing a quote and a
    /// newline gets it escaped, so the record is still exactly one line and the next record's `prev`
    /// still covers the whole of it.
    #[test]
    fn a_quoted_newline_cannot_start_a_second_line() {
        let draft = Draft::new(Op::SearchBegin)
            .args("query=\"x\\n{\\\"op\\\":\\\"clean\\\"}\"")
            .result("error \"not allowed\"");
        let record = completed(&draft, 1);
        let line = record.to_line().expect("serialises");
        assert_eq!(line.lines().count(), 1, "line was {line:?}");
        let back: Record = serde_json::from_str(&line).expect("parses back");
        assert_eq!(back, record);
    }

    /// Field order is part of the format because the bytes are hashed: a reordering silently breaks
    /// every chain written by the previous version.
    #[test]
    fn the_line_shape_is_stable() {
        let record = completed(&Draft::new(Op::DirOpen).target(Path::new("d:/x")), 1);
        let line = record.to_line().expect("serialises");
        assert!(line.starts_with(
            "{\"seq\":1,\"ts\":\"1970-01-01T00:00:00.000Z\",\"actor\":\"user\",\"op\":\"dir.open\""
        ));
        assert!(line.ends_with("\"prev\":\"0\"}"));
        assert!(!line.contains("dropped"));
    }

    #[test]
    fn a_day_is_a_day_in_utc() {
        assert_eq!(utc_date(0), (1970, 1, 1));
        assert_eq!(utc_date(86_399_999), (1970, 1, 1));
        assert_eq!(utc_date(86_400_000), (1970, 1, 2));
        // A leap day, and the century rule that does not leap.
        assert_eq!(utc_date(951_782_400_000), (2000, 2, 29));
        assert_eq!(utc_date(1_577_836_800_000), (2020, 1, 1));
        // A winter evening, from `date -u -d @1766528059`.
        assert_eq!(utc_date(1_766_528_059_000), (2025, 12, 23));
    }

    #[test]
    fn a_timestamp_reads_as_a_clock() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00.000Z");
        // Both from `date -u -d @<seconds>` on this machine.
        assert_eq!(format_utc(1_727_050_267_415), "2024-09-23T00:11:07.415Z");
        assert_eq!(format_utc(1_766_528_059_000), "2025-12-23T22:14:19.000Z");
    }

    #[test]
    fn clipping_counts_characters() {
        assert_eq!(clip("héllo", 5), "héllo");
        assert_eq!(clip("héllo", 3), "hé~");
        assert_eq!(clip("", 3), "");
    }

    fn completed(draft: &Draft, seq: u64) -> Record {
        Record {
            seq,
            ts: "1970-01-01T00:00:00.000Z".into(),
            actor: Actor::User,
            op: draft.op().name().to_owned(),
            target: draft.target.clone(),
            args: draft.args.clone(),
            result: draft.result.clone(),
            session: 1,
            prev: crate::chain::GENESIS.into(),
            dropped: 0,
            dropped_because: None,
        }
    }
}
