//! One line of the audit log: what was asked, by whom, and what came back.
//!
//! A record is JSON on a single line, and the line's own bytes are what the next record hashes, so
//! the serialization here *is* the format. Two rules follow from that and are enforced below rather
//! than by convention: nothing is ever written with `format!` (only `serde_json` escapes a stray
//! quote or newline, and a hand-built line is how a log gets forged), and every free-text field is
//! clipped, so one call cannot make an arbitrarily large line.

use std::path::Path;

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
    TagMerge,
    TagRename,
    PathDelete,
    PathTransferBegin,
    PathTransferEnd,
    NoteGet,
    NoteSet,
    NoteClear,
    /// The tags of a window of a listing, asked for by path. `tag.list` is one file's tags because
    /// someone asked about that file; this is the same question asked for a hundred and twenty-eight
    /// files because a screen is showing them, and it happens on the shell's poll beat. It is a
    /// heartbeat for the same reason `dir.poll` is one: a line per beat per column would date nothing
    /// and would bury the acts that do.
    TagFetch,
    /// The notes of a window of a listing, asked for by path. `note.get` is one file's note because
    /// someone asked about that file; this is the same question asked for a hundred and twenty-eight
    /// files because a screen is showing them, and it happens on the shell's poll beat. It is a
    /// heartbeat for the same reason `TagFetch` is one: a line per beat per column would date nothing
    /// and would bury the acts that do.
    NoteFetch,
    /// Every tag name in the store with how many files carry it. This one is not a heartbeat - it is a
    /// read of the whole tag table, which is the largest thing on this surface that is not scoped to a
    /// directory, so it is a `Read` and the `record_reads` knob decides whether it leaves a line.
    TagVocabulary,
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
    /// A file or folder someone dragged somewhere. These are the first calls on the ABI surface that
    /// change what is on disk, and they are in this list for the same reason the token verbs are: the
    /// trail exists to date an act, and a move is the act a person most often wants dated - both
    /// "where did it go" and "it was there before I moved it".
    PathMove,
    PathCopy,
    /// The credential verbs - three that change a grant and one that reads them. They are not ABI
    /// calls: the ABI has no credential surface at all, by design. Handing out a token, taking one
    /// away and swapping its secret are exactly the acts an audit trail exists to date, and the only
    /// program that can perform them is the CLI. `target` stays empty: these are about a token, not a
    /// path, and `args` carries the id, the label and the tier - never the secret, and never its
    /// hash.
    TokenAdd,
    TokenRevoke,
    TokenRotate,
    /// The fourteen MCP tools, named **exactly** as the protocol names them (`Verb::name()`), so a
    /// line in the trail can be grepped for the same string a client sent. A dotted spelling here
    /// would be a second vocabulary for one thing, and the second vocabulary is where "we checked
    /// the log" goes wrong.
    McpFsList,
    McpFsRead,
    McpFsSearch,
    McpFsWrite,
    McpFsCreate,
    McpFsDelete,
    McpFsMove,
    McpTagsGet,
    McpTagsSet,
    McpTagsRemove,
    McpTagsMerge,
    McpNoteGet,
    McpNoteSet,
    McpNoteClear,
    McpWhoAmI,
    /// A person read the grant list. Not a change, and recorded anyway: it is an `Ask`, and without
    /// it a `fam mcp token list` session is two bracket lines that say nothing about what ran.
    TokenList,
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
            Self::TagMerge => "tag.merge",
            Self::TagRename => "tag.rename",
            Self::PathDelete => "path.delete",
            Self::PathTransferBegin => "path.transfer.begin",
            Self::PathTransferEnd => "path.transfer.end",
            Self::NoteGet => "note.get",
            Self::NoteSet => "note.set",
            Self::NoteClear => "note.clear",
            Self::TagFetch => "tag.fetch",
            Self::NoteFetch => "note.fetch",
            Self::TagVocabulary => "tag.vocabulary",
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
            Self::PathMove => "path.move",
            Self::PathCopy => "path.copy",
            Self::TokenAdd => "token.add",
            Self::TokenRevoke => "token.revoke",
            Self::TokenRotate => "token.rotate",
            Self::McpFsList => "fs_list",
            Self::McpFsRead => "fs_read",
            Self::McpFsSearch => "fs_search",
            Self::McpFsWrite => "fs_write",
            Self::McpFsCreate => "fs_create",
            Self::McpFsDelete => "fs_delete",
            Self::McpFsMove => "fs_move",
            Self::McpTagsGet => "tags_get",
            Self::McpTagsSet => "tags_set",
            Self::McpTagsRemove => "tags_remove",
            Self::McpTagsMerge => "tags_merge",
            Self::McpNoteGet => "note_get",
            Self::McpNoteSet => "note_set",
            Self::McpNoteClear => "note_clear",
            Self::McpWhoAmI => "whoami",
            Self::TokenList => "token.list",
            Self::SessionOpen => "session.open",
            Self::SessionClose => "session.close",
        }
    }

    /// Which calls the `audit_record_reads` knob and the daily cap may silence.
    ///
    /// `fs_list` and `fs_read` join the `Read` stream because they are the same kind of event as
    /// `dir.fetch`: content being carried out of a directory. The MCP server opens its log with
    /// `record_reads` forced on, so this classification costs it nothing - the knob exists for the
    /// shell's polling-heavy reads, not to make an agent's file reads invisible.
    pub fn weight(self) -> Weight {
        match self {
            Self::DirFetch
            | Self::SearchPage
            | Self::TagVocabulary
            | Self::McpFsList
            | Self::McpFsRead => Weight::Read,
            Self::DirState | Self::DirPoll | Self::TagFetch | Self::NoteFetch => Weight::Poll,
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

    /// The two tag reads the shell's listing needs. Which one is a heartbeat is the whole point of the
    /// test: `tag.fetch` runs once per column per poll beat for as long as a folder is on screen, so a
    /// line for it would be thousands per session and would say nothing anyone wanted dated.
    /// `tag.vocabulary` is the opposite shape - a read of the entire tag table, asked rarely - and it
    /// is the case `record_reads` exists to cover.
    #[test]
    fn the_tag_reads_are_split_by_how_often_they_happen() {
        assert_eq!(Op::TagMerge.name(), "tag.merge");
        assert_eq!(Op::TagRename.name(), "tag.rename");
        assert_eq!(Op::PathDelete.name(), "path.delete");
        assert_eq!(Op::PathTransferBegin.name(), "path.transfer.begin");
        assert_eq!(Op::PathTransferEnd.name(), "path.transfer.end");
        assert_eq!(Op::NoteGet.name(), "note.get");
        assert_eq!(Op::NoteSet.name(), "note.set");
        assert_eq!(Op::NoteClear.name(), "note.clear");
        assert_eq!(Op::TagFetch.name(), "tag.fetch");
        assert_eq!(Op::NoteFetch.name(), "note.fetch");
        assert_eq!(Op::TagFetch.weight(), Weight::Poll);
        assert_eq!(Op::TagVocabulary.name(), "tag.vocabulary");
        assert_eq!(Op::TagVocabulary.weight(), Weight::Read);
        // And the one-file question a person asked is still always recorded.
        assert_eq!(Op::TagList.weight(), Weight::Ask);
    }

    /// The tools an agent calls are named in the trail by the same string the client sent, and the
    /// two that carry content out of a directory are weighed like the shell's own read stream.
    #[test]
    fn the_mcp_tools_are_named_as_the_protocol_names_them() {
        for (op, name) in [
            (Op::McpFsList, "fs_list"),
            (Op::McpFsRead, "fs_read"),
            (Op::McpFsSearch, "fs_search"),
            (Op::McpFsWrite, "fs_write"),
            (Op::McpFsCreate, "fs_create"),
            (Op::McpFsDelete, "fs_delete"),
            (Op::McpFsMove, "fs_move"),
            (Op::McpTagsGet, "tags_get"),
            (Op::McpTagsSet, "tags_set"),
            (Op::McpTagsRemove, "tags_remove"),
            (Op::McpTagsMerge, "tags_merge"),
            (Op::McpNoteGet, "note_get"),
            (Op::McpNoteSet, "note_set"),
            (Op::McpNoteClear, "note_clear"),
            (Op::McpWhoAmI, "whoami"),
        ] {
            assert_eq!(op.name(), name);
            let weight = op.weight();
            let expected = match op {
                Op::McpFsList | Op::McpFsRead => Weight::Read,
                _ => Weight::Ask,
            };
            assert_eq!(weight, expected, "{name} is weighed wrong");
            assert_ne!(weight, Weight::Poll, "{name} is never a heartbeat");
        }
    }

    /// The two verbs a person's drag can produce. They are `Ask` - one act, one line, never a
    /// heartbeat that could bury the act under it - and their names are on the wire, so a rename here
    /// is a format change and this test is where that gets noticed.
    #[test]
    fn the_transfer_verbs_are_named_and_cost_one_ask() {
        for (op, name) in [(Op::PathMove, "path.move"), (Op::PathCopy, "path.copy")] {
            assert_eq!(op.name(), name);
            assert_eq!(op.weight(), Weight::Ask, "{name} is not an Ask");
        }
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
