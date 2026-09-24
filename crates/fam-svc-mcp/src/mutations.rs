//! The tools that change something: writes, moves, tags and notes.
//!
//! These are the reason the gate exists. A read that escapes a workspace reveals names; a write that
//! escapes it deletes someone's holiday photographs, and an agent that did it by guessing a path is
//! not going to undo it. So each of these calls the same [`Server::admit`] the reads do - on **both**
//! ends where there are two - and each one refuses the case where the damage is unbounded:
//!
//! * `fs_delete` will not remove a directory that still has something in it. Not "warns", refuses.
//!   A recursive delete behind a model that miscounted its own argument is the single worst thing
//!   this surface could do, and the caller can always delete the contents first - deliberately, one
//!   call at a time, each of which is in the trail.
//! * `fs_move` will not overwrite an existing target.
//! * `fs_write` will not invent a parent directory, and neither will `fs_create` of a **file**: a
//!   typo in the last component of a path must not quietly relocate the file somewhere else.
//!   `fs_create` of a *directory* does create the chain, because making directories is the whole
//!   job of that call and every part of the chain was already inside the grant.
//!
//! What is *not* here: any way to widen, shorten or end a credential. Those are the four
//! `fam mcp token` commands, run by a person, and this crate never calls `add`, `revoke` or
//! `rotate`.

use std::fs;
use std::path::{Path, PathBuf};

use fam_svc_auth::Verb;
use fam_svc_store::NewTag;
use serde_json::json;

use crate::server::{Server, ToolAnswer, MAX_FILE_BYTES};

impl Server {
    pub(super) fn fs_write(&mut self, path: &Path, content: &str) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsWrite, path) {
            return ToolAnswer::error(why);
        }
        if content.len() as u64 > MAX_FILE_BYTES {
            return ToolAnswer::failed(json!({
                "error": format!(
                    "the content is {} bytes and fs_write accepts at most {MAX_FILE_BYTES}",
                    content.len()
                ),
                "limit": MAX_FILE_BYTES,
            }));
        }
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => return ToolAnswer::error("path must name a file inside a directory"),
        };
        if !parent.is_dir() {
            return ToolAnswer::error(format!(
                "{} does not exist; fs_write does not create directories",
                parent.display()
            ));
        }
        if path.is_dir() {
            return ToolAnswer::error(format!("{} is a directory", path.display()));
        }
        let existed = path.exists();
        if let Err(source) = fs::write(path, content) {
            return ToolAnswer::error(format!("cannot write {}: {source}", path.display()));
        }
        ToolAnswer::ok(json!({
            "path": path.display().to_string(),
            "written": content.len(),
            "replaced": existed,
        }))
    }

    pub(super) fn fs_create(&mut self, path: &Path, kind: &str) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsCreate, path) {
            return ToolAnswer::error(why);
        }
        if path.exists() {
            return ToolAnswer::error(format!("{} already exists", path.display()));
        }
        match kind {
            "dir" => match fs::create_dir_all(path) {
                Ok(()) => {
                    ToolAnswer::ok(json!({"created": path.display().to_string(), "kind": "dir"}))
                }
                Err(source) => {
                    ToolAnswer::error(format!("cannot create {}: {source}", path.display()))
                }
            },
            "file" => {
                let parent = match path.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent,
                    _ => return ToolAnswer::error("path must name a file inside a directory"),
                };
                if !parent.is_dir() {
                    return ToolAnswer::error(format!(
                        "{} does not exist; fs_create does not make parents for a file",
                        parent.display()
                    ));
                }
                match fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)
                {
                    Ok(_) => ToolAnswer::ok(
                        json!({"created": path.display().to_string(), "kind": "file"}),
                    ),
                    Err(source) => {
                        ToolAnswer::error(format!("cannot create {}: {source}", path.display()))
                    }
                }
            }
            other => ToolAnswer::error(format!("kind must be \"file\" or \"dir\", not {other:?}")),
        }
    }

    pub(super) fn fs_delete(&mut self, path: &Path) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsDelete, path) {
            return ToolAnswer::error(why);
        }
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(source) => {
                return ToolAnswer::error(format!("cannot open {}: {source}", path.display()))
            }
        };
        if meta.is_dir() {
            // Refused here rather than left to the operating system. `remove_dir` on a folder that
            // still has entries fails too, but with a locale-dependent system message handed straight
            // to the agent - and the obvious "fix" for a caller that wants the folder gone is the
            // recursive call, which is the one this tool must never make. An entry that fails to
            // read counts as *not* empty: an error is not evidence of emptiness.
            let is_empty = match fs::read_dir(path) {
                Ok(mut entries) => entries.next().is_none(),
                Err(source) => {
                    return ToolAnswer::error(format!(
                        "cannot check whether {} is empty: {source}",
                        path.display()
                    ))
                }
            };
            if !is_empty {
                return ToolAnswer::error(format!(
                    "{} is a directory that still has entries in it; fs_delete removes files and \
                     empty directories only",
                    path.display()
                ));
            }
            return match fs::remove_dir(path) {
                Ok(()) => {
                    ToolAnswer::ok(json!({"removed": path.display().to_string(), "kind": "dir"}))
                }
                Err(source) => {
                    ToolAnswer::error(format!("cannot remove {}: {source}", path.display()))
                }
            };
        }
        // A symlink or junction is removed as the link itself, never as what it points at: the gate
        // has already refused a link that leaves, and deleting through one would be deleting outside.
        match fs::remove_file(path) {
            Ok(()) => {
                ToolAnswer::ok(json!({"removed": path.display().to_string(), "kind": "file"}))
            }
            Err(source) => ToolAnswer::error(format!("cannot remove {}: {source}", path.display())),
        }
    }

    pub(super) fn fs_move(&mut self, from: &PathBuf, to: &PathBuf) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::FsMove, from) {
            return ToolAnswer::error(format!("the source is not reachable: {why}"));
        }
        if let Err(why) = self.admit(Verb::FsMove, to) {
            return ToolAnswer::error(format!("the destination is not reachable: {why}"));
        }
        if !Path::new(from).exists() {
            return ToolAnswer::error(format!("{} does not exist", from.display()));
        }
        if Path::new(to).exists() {
            return ToolAnswer::error(format!(
                "{} already exists; fs_move never overwrites",
                to.display()
            ));
        }
        match fs::rename(from, to) {
            Ok(()) => ToolAnswer::ok(json!({
                "from": from.display().to_string(),
                "to": to.display().to_string(),
                "moved": true,
            })),
            Err(source) => ToolAnswer::error(format!(
                "cannot move {} to {}: {source}",
                from.display(),
                to.display()
            )),
        }
    }

    pub(super) fn tags_get(&mut self, path: &Path) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::TagsGet, path) {
            return ToolAnswer::error(why);
        }
        match self.tags().tags_of(path, None) {
            Ok(records) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "tags": records
                    .iter()
                    .map(|record| json!({"tag": record.tag, "source": record.source.as_str(),
                                         "priority": record.priority}))
                    .collect::<Vec<_>>(),
            })),
            Err(source) => ToolAnswer::error(format!("cannot read tags: {source}")),
        }
    }

    pub(super) fn tags_set(
        &mut self,
        path: &Path,
        tags: &[String],
        source: fam_svc_store::Source,
    ) -> ToolAnswer {
        if tags.is_empty() {
            return ToolAnswer::error("tags must name at least one tag");
        }
        if let Err(why) = self.admit(Verb::TagsSet, path) {
            return ToolAnswer::error(why);
        }
        let new: Vec<NewTag> = tags
            .iter()
            .map(|tag| NewTag {
                tag: tag.clone(),
                source,
                priority: 0,
            })
            .collect();
        match self.tags().add_tags(path, &new) {
            Ok(outcome) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "inserted": outcome.inserted,
                "refreshed": outcome.refreshed,
            })),
            Err(why) => ToolAnswer::error(format!("cannot set tags: {why}")),
        }
    }

    pub(super) fn tags_remove(&mut self, path: &Path, tags: &[String]) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::TagsRemove, path) {
            return ToolAnswer::error(why);
        }
        // An empty list means "every tag on this path", which is spelled out here rather than left
        // to the store's behaviour on an empty argument: the two are the same only until someone
        // changes the store.
        let names: Vec<String> = if tags.is_empty() {
            match self.tags().tags_of(path, None) {
                Ok(records) => records.into_iter().map(|record| record.tag).collect(),
                Err(source) => return ToolAnswer::error(format!("cannot read tags: {source}")),
            }
        } else {
            tags.to_vec()
        };
        if names.is_empty() {
            return ToolAnswer::ok(json!({"path": path.display().to_string(), "removed": 0}));
        }
        match self.tags().remove_tags(path, &names, None) {
            Ok(removed) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "removed": removed,
            })),
            Err(why) => ToolAnswer::error(format!("cannot remove tags: {why}")),
        }
    }

    pub(super) fn note_get(&mut self, path: &Path) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::NoteGet, path) {
            return ToolAnswer::error(why);
        }
        match self.tags().note(path) {
            Ok(Some(record)) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "body": record.body,
                "updated_at": record.updated_at,
            })),
            Ok(None) => ToolAnswer::ok(json!({"path": path.display().to_string(), "body": null})),
            Err(source) => ToolAnswer::error(format!("cannot read the note: {source}")),
        }
    }

    pub(super) fn note_set(&mut self, path: &Path, body: &str) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::NoteSet, path) {
            return ToolAnswer::error(why);
        }
        match self.tags().set_note(path, body) {
            Ok(()) => ToolAnswer::ok(json!({
                "path": path.display().to_string(),
                "written": body.len(),
            })),
            Err(source) => ToolAnswer::error(format!("cannot write the note: {source}")),
        }
    }

    pub(super) fn note_clear(&mut self, path: &Path) -> ToolAnswer {
        if let Err(why) = self.admit(Verb::NoteClear, path) {
            return ToolAnswer::error(why);
        }
        match self.tags().clear_note(path) {
            Ok(removed) => {
                ToolAnswer::ok(json!({"path": path.display().to_string(), "removed": removed}))
            }
            Err(source) => ToolAnswer::error(format!("cannot clear the note: {source}")),
        }
    }
}
