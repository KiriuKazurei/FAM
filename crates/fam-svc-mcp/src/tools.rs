//! The tool table: what the protocol advertises, and nothing else.
//!
//! Fourteen entries, one per [`Verb`], and the names are the verb names - so the string a client
//! sends, the string written in the audit trail, and the whitelist entry that decides whether it is
//! allowed are one string rather than three that have to be kept in step. The cross-check that they
//! are is in `tests/mcp.rs`, the one place that sees both crates.
//!
//! The descriptions are written for a reader who cannot see the filesystem: each states its limit
//! instead of leaving it to be discovered by a failed call, and each says what the tool will not do.
//! `fs_delete` refusing a non-empty directory belongs in the description, because an agent that reads
//! it there does not have to try it to find out.
//!
//! Schemas are JSON *text*. The table is a `const`, and a `json!` literal allocates at runtime, so
//! either the table is built lazily or the schema is written as it will be sent - and a lazily built
//! table is where a malformed schema hides until a client asks for the list.
//! `every_schema_is_json` parses all fourteen at test time instead.

use fam_svc_auth::Verb;
use serde_json::Value;

/// One advertised tool.
pub struct ToolSpec {
    pub verb: Verb,
    pub name: &'static str,
    pub description: &'static str,
    pub schema: &'static str,
}

impl ToolSpec {
    /// The shape MCP's `tools/list` expects.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.schema_json(),
        })
    }

    /// The parsed schema. Panics only on a bug in this file, which `every_schema_is_json` catches.
    pub fn schema_json(&self) -> Value {
        serde_json::from_str(self.schema).expect("a tool schema in tools.rs is not valid JSON")
    }
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        verb: Verb::FsList,
        name: "fs_list",
        description: "List one directory: name, kind, size and modification time per row, folders \
                      first. Reads through the same lazy index the shell uses, so the answer carries \
                      a generation and says whether it is stale.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Rows to return. Default 200."},
    "offset": {"type": "integer", "minimum": 0, "description": "Rows to skip, for paging a long listing."},
    "sort": {"type": "string", "enum": ["name", "size", "kind", "time"], "description": "Order of the rows. Default name."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsRead,
        name: "fs_read",
        description: "Read a text file whole. Files over 1 MiB, and anything that is not valid \
                      UTF-8, are refused rather than truncated - the size comes back so the caller \
                      can decide what to do next.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsSearch,
        name: "fs_search",
        description: "Search file names. Routed between the installed index and a recursive walk the \
                      same way the shell routes it, and the answer says which backend answered and \
                      whether the answer is complete - a partial result is reported as partial. A \
                      word prefixed `tag:` names a label instead of part of a name; inside the value \
                      `,` is AND (all must be carried), `||` is OR (one of them suffices, the comma \
                      binds tighter), and a leading `!` excludes the name - all half-width ASCII; the \
                      full-width `，｜｜！` are ordinary tag-name characters. Tags are matched exactly, \
                      so the case must be the one that was set.",
        schema: r#"{
  "type": "object",
  "properties": {
    "query": {"type": "string", "description": "Words that must all appear in the name; `tag:name` asks for files carrying that label instead."},
    "within": {"type": "string", "description": "Restrict to this subtree. It must be inside a granted workspace; it narrows the roots, it never widens them."},
    "kind": {"type": "string", "enum": ["directory", "image", "video", "audio", "document", "archive", "code", "executable", "other"]},
    "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Hits to return. Default 200."}
  },
  "required": ["query"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsWrite,
        name: "fs_write",
        description: "Write a text file, replacing it if it exists. The parent directory must \
                      already exist. Content over 1 MiB is refused.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "content": {"type": "string", "description": "The whole new contents."}
  },
  "required": ["path", "content"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsCreate,
        name: "fs_create",
        description: "Create an empty file or a directory. Missing parents are created for a \
                      directory, not for a file.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "kind": {"type": "string", "enum": ["file", "dir"], "description": "Default file."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsDelete,
        name: "fs_delete",
        description: "Delete one file, or one directory that is empty. A non-empty directory is \
                      refused: removing a tree because a model miscounted its own argument is the \
                      one mistake here that cannot be undone.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::FsMove,
        name: "fs_move",
        description: "Rename or move one entry. Both ends are checked against the workspace, and an \
                      existing target is refused rather than overwritten.",
        schema: r#"{
  "type": "object",
  "properties": {
    "from": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "to": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["from", "to"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::TagsGet,
        name: "tags_get",
        description: "The tags on a path, with each one's source and priority.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::TagsSet,
        name: "tags_set",
        description: "Add tags to a path. Idempotent: a tag already there is refreshed, not \
                      duplicated.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "tags": {"type": "array", "items": {"type": "string"}, "description": "At least one."},
    "source": {"type": "string", "enum": ["manual", "auto"], "description": "Default manual."}
  },
  "required": ["path", "tags"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::TagsRemove,
        name: "tags_remove",
        description: "Remove tags from a path. An empty list removes every tag on it.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "tags": {"type": "array", "items": {"type": "string"}}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::TagsMerge,
        name: "tags_merge",
        description: "Rename a tag **across the whole library**: every file carrying `from` ends up                       carrying `into` instead, and where a file already has `into` from the same                       source the two rows collapse into one. Names no path, because the library is                       shared; the answer reports counts, never paths. An unknown `from` is an error,                       so a typo merging onto a real name cannot read as success.",
        schema: r#"{
  "type": "object",
  "properties": {
    "from": {"type": "string", "description": "The tag name to move every row off. Must exist."},
    "into": {"type": "string", "description": "The name the rows take. Validated like any tag; equal to `from` is refused."}
  },
  "required": ["from", "into"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::NoteGet,
        name: "note_get",
        description: "The note on a path, or nothing. A note is free text stored beside the path; it \
                      is not a tag, and it never appears in a tag list or a search result.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::NoteSet,
        name: "note_set",
        description: "Write the note on a path, replacing the previous one. Empty text is refused - \
                      use note_clear to remove a note.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."},
    "body": {"type": "string", "description": "Up to 8 KiB."}
  },
  "required": ["path", "body"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::NoteClear,
        name: "note_clear",
        description: "Remove the note on a path. Says whether there was one to remove.",
        schema: r#"{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Absolute path inside one of this credential's workspaces."}
  },
  "required": ["path"],
  "additionalProperties": false
}"#,
    },
    ToolSpec {
        verb: Verb::WhoAmI,
        name: "whoami",
        description: "Which credential is speaking: its label, its tier, the verbs it may call, the \
                      workspaces it was granted, and the roots this program manages. Claims no path.",
        schema: r#"{
  "type": "object",
  "properties": {},
  "additionalProperties": false
}"#,
    },
];

/// The tool for a verb, which is how the dispatcher gets a name back to its handler.
pub fn spec(verb: Verb) -> &'static ToolSpec {
    TOOLS
        .iter()
        .find(|tool| tool.verb == verb)
        .unwrap_or_else(|| panic!("{} has no tool description", verb.name()))
}

pub fn find(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.name == name)
}
