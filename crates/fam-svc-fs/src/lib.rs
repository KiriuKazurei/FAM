//! Moving and copying a path, with no policy attached to it.
//!
//! This is the one implementation of "put this file over there". Two surfaces call it: the agent's
//! MCP tools, which ask a credential whether the two ends are reachable first, and the human's shell,
//! which needs no asking because it already runs as that person. What must not happen is those two
//! surfaces disagreeing about what a refused operation looks like - so the rules below live here, and
//! a caller that wants to *add* a rule (a scope, a confirmation, an audit line) wraps this rather than
//! copying it.
//!
//! The rules, all of them refusals rather than guesses:
//!
//!   * **never overwrite.** A target that exists is an error, not a prompt. The alternative is a
//!     drag that silently replaces a file, and the person who did it will find out later.
//!   * **the source must exist** and the destination's parent must exist. A drag whose target folder
//!     vanished between the paint and the drop is refused rather than being allowed to create it.
//!   * **a directory is moved, never copied.** A recursive copy is a loop with a delete at the end of
//!     it, and the delete is the one irreversible call in this file. Cross-volume directory moves are
//!     therefore refused by name; same-volume ones are a single `rename` and carry no such risk.
//!   * **a move that the volume refuses becomes a copy plus a delete** - but only for a file, and only
//!     when the operating system said *why* (different device). Any other failure is reported as the
//!     error it is, because "we could not tell" is not a licence to start deleting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Move the source, or leave it where it is and put a copy at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move,
    Copy,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Copy => "copy",
        }
    }
}

/// How the bytes actually got there, which is worth reporting because the two paths differ in what
/// they cost and in what they leave behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// One `rename`: the same volume, and the only way a directory can move.
    Renamed,
    /// Copied, then the source removed. Only reachable for a file, and only after the volume said no.
    CopiedThenRemoved,
    /// Copied, source kept. That is `Action::Copy` succeeding.
    Copied,
}

impl By {
    pub fn name(self) -> &'static str {
        match self {
            Self::Renamed => "renamed",
            Self::CopiedThenRemoved => "copied-then-removed",
            Self::Copied => "copied",
        }
    }
}

/// What happened, in the words a caller can put in front of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transferred {
    pub from: PathBuf,
    pub to: PathBuf,
    pub action: Action,
    pub by: By,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{} does not exist", path.display())]
    Missing { path: PathBuf },
    #[error("{} already exists; a transfer never overwrites", path.display())]
    Exists { path: PathBuf },
    #[error("the source and the destination are the same path")]
    SamePath,
    #[error("the folder to move into, {}, does not exist", path.display())]
    NoParent { path: PathBuf },
    #[error("a directory cannot be copied, only moved: {}", path.display())]
    CopyUnsupported { path: PathBuf },
    /// The two paths are on different volumes and the source is a directory. `rename` cannot do it
    /// and a recursive copy followed by a recursive delete is not implemented here on purpose.
    #[error("moving the directory {} across volumes is not supported; copy it with another tool",
            path.display())]
    CrossVolumeDirectory { path: PathBuf },
    #[error("cannot {action} {} to {}: {reason}", from.display(), to.display())]
    Io {
        action: &'static str,
        from: PathBuf,
        to: PathBuf,
        reason: String,
    },
    #[error("cannot open {}: {source}", path.display())]
    CantOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "{} is a directory that still has entries in it; fs_delete removes files and          empty directories only",
        path.display()
    )]
    DirectoryNotEmpty { path: PathBuf },
    #[error("cannot check whether {} is empty: {source}", path.display())]
    CantCheckEmpty {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot remove {}: {source}", path.display())]
    CantRemove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the transfer to {} was cancelled; the partial copy was removed",
        path.display()
    )]
    Cancelled { path: PathBuf },
}

/// What a removal took off the disk, in the words a caller reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub path: PathBuf,
    /// `"file"` or `"dir"` - the shape the MCP answer has always reported.
    pub kind: &'static str,
}

/// Removes one path, with the same refusals-everywhere rules the transfers carry:
///
///   * **a directory that still has entries is refused, by name.** The obvious fix for a caller that
///     wanted it gone is the recursive call, and the recursive delete is the one irreversible call
///     there is - this crate refuses it rather than making it quiet.
///   * **a symlink or junction is removed as the link, never as what it points at.**
///   * an entry that fails to read counts as *not* empty: an error is not evidence of emptiness.
///
/// This is the one implementation of "take this file away", shared by the agent's `fs_delete` and
/// the human's undo; a caller that wants a scope or an audit line around it wraps it.
pub fn remove_path(path: &Path) -> Result<Removed, Error> {
    let meta = fs::symlink_metadata(path).map_err(|source| Error::CantOpen {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.is_dir() {
        let is_empty = match fs::read_dir(path) {
            Ok(mut entries) => entries.next().is_none(),
            Err(source) => {
                return Err(Error::CantCheckEmpty {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        if !is_empty {
            return Err(Error::DirectoryNotEmpty {
                path: path.to_path_buf(),
            });
        }
        return fs::remove_dir(path)
            .map(|_| Removed {
                path: path.to_path_buf(),
                kind: "dir",
            })
            .map_err(|source| Error::CantRemove {
                path: path.to_path_buf(),
                source,
            });
    }
    fs::remove_file(path)
        .map(|_| Removed {
            path: path.to_path_buf(),
            kind: "file",
        })
        .map_err(|source| Error::CantRemove {
            path: path.to_path_buf(),
            source,
        })
}

/// Move or copy `from` onto `to`, where `to` is a full destination path (folder plus name), not a
/// folder to drop into. Composing the name is the caller's job, because only the caller knows whether
/// the person meant "into this folder" or "rename it to this".
/// How far one transfer has got. `bytes` counts destination bytes written; `total` is the source's
/// length as the filesystem reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferProgress {
    pub bytes: u64,
    pub total: u64,
}

/// One chunk, because `fs::copy` is a single call and a single call has no middle: no progress to
/// show, nothing to cancel. The chunk is also the granularity of the cancel check, so a copy is
/// never more than one chunk past the moment the person said stop.
pub const CHUNK: u64 = 1024 * 1024;

/// Asks whether the transfer may go on. Called once before the first chunk (so a cancel that
/// arrives before any byte moves is honoured before any byte moves) and once after every chunk;
/// returning `false` stops the copy and removes the partial destination.
pub type Progress<'a> = &'a mut dyn FnMut(TransferProgress) -> bool;

pub fn transfer(from: &Path, to: &Path, action: Action) -> Result<Transferred, Error> {
    transfer_with(from, to, action, &mut |_| true)
}

/// [`transfer`] with a person on the other end of the progress callback - the shape the shell's
/// asynchronous transfers run, where the callback reads a cancel flag and reports a beat.
pub fn transfer_with(
    from: &Path,
    to: &Path,
    action: Action,
    progress: Progress,
) -> Result<Transferred, Error> {
    let meta = match fs::symlink_metadata(from) {
        Ok(meta) => meta,
        Err(_) => {
            return Err(Error::Missing {
                path: from.to_path_buf(),
            })
        }
    };
    if from == to {
        return Err(Error::SamePath);
    }
    if to.symlink_metadata().is_ok() {
        return Err(Error::Exists {
            path: to.to_path_buf(),
        });
    }
    let parent = to
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty() && dir.exists())
        .ok_or_else(|| Error::NoParent {
            path: to.parent().unwrap_or(to).to_path_buf(),
        })?;

    let done = |by| Transferred {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        action,
        by,
    };

    if action == Action::Copy {
        if meta.is_dir() {
            return Err(Error::CopyUnsupported {
                path: from.to_path_buf(),
            });
        }
        copy_chunked(from, to, progress)
            .map_err(|error| error.into_io(action, from, to, parent))?;
        return Ok(done(By::Copied));
    }

    match fs::rename(from, to) {
        Ok(()) => return Ok(done(By::Renamed)),
        Err(source) => {
            // Only the one reason the operating system can give us turns a move into a copy. Anything
            // else - access denied, name too long, in use - is reported as what it said.
            if !is_cross_device(&source) {
                return Err(io_error(action, from, to, parent, source));
            }
            if meta.is_dir() {
                return Err(Error::CrossVolumeDirectory {
                    path: from.to_path_buf(),
                });
            }
        }
    }

    copy_chunked(from, to, progress).map_err(|error| error.into_io(action, from, to, parent))?;
    // Reaching here means the bytes are at the destination. If the delete then fails, the answer is
    // not "failed" - it is "copied, and the source is still there", which is a state a person can act
    // on. Reporting it as a failure would invite a retry that hits `Exists` and looks like nothing
    // happened.
    if fs::remove_file(from).is_err() {
        return Ok(done(By::Copied));
    }
    Ok(done(By::CopiedThenRemoved))
}

/// One cancelled or failed copy may leave a partial destination behind; this is the sweep that
/// makes "cancelled" mean "nothing arrived".
fn discard_partial(to: &Path) {
    let _ = fs::remove_file(to);
}

/// The file copy, one chunk at a time, with the person's hand on the callback. Carries the same
/// bytes and the same permissions `fs::copy` would carry, and adds the two things a single call
/// could not: a middle to report, and a stop that cleans up after itself.
fn copy_chunked(from: &Path, to: &Path, progress: Progress) -> Result<(), CopyError> {
    use std::io::{Read, Write};

    let total = fs::metadata(from).map(|meta| meta.len()).unwrap_or(0);
    if !progress(TransferProgress { bytes: 0, total }) {
        return Err(CopyError::Cancelled {
            path: to.to_path_buf(),
        });
    }
    let mut copy = || -> Result<(), CopyStop> {
        let mut source = fs::File::open(from).map_err(CopyStop::Io)?;
        let mut destination = fs::File::create(to).map_err(CopyStop::Io)?;
        let mut buffer = vec![0u8; CHUNK as usize];
        let mut copied = 0u64;
        loop {
            let read = source.read(&mut buffer).map_err(CopyStop::Io)?;
            if read == 0 {
                break;
            }
            destination
                .write_all(&buffer[..read])
                .map_err(CopyStop::Io)?;
            copied += read as u64;
            if !progress(TransferProgress {
                bytes: copied,
                total,
            }) {
                return Err(CopyStop::Cancelled);
            }
        }
        let permissions = source.metadata().map_err(CopyStop::Io)?.permissions();
        destination
            .set_permissions(permissions)
            .map_err(CopyStop::Io)?;
        destination.flush().map_err(CopyStop::Io)?;
        Ok(())
    };
    match copy() {
        Ok(()) => Ok(()),
        Err(CopyStop::Cancelled) => {
            discard_partial(to);
            Err(CopyError::Cancelled {
                path: to.to_path_buf(),
            })
        }
        Err(CopyStop::Io(source)) => Err(CopyError::Io { source }),
    }
}

/// A copy's two ways of stopping, kept distinguishable: a read or write failure must not be
/// mistaken for a stop, and a stop must not be reported as one.
enum CopyStop {
    Cancelled,
    Io(io::Error),
}

/// A copy's failure, before the caller folds it into the transfer error vocabulary.
enum CopyError {
    Cancelled { path: PathBuf },
    Io { source: io::Error },
}

impl CopyError {
    fn into_io(self, action: Action, from: &Path, to: &Path, parent: &Path) -> Error {
        match self {
            CopyError::Cancelled { path } => Error::Cancelled { path },
            CopyError::Io { source } => io_error(action, from, to, parent, source),
        }
    }
}

/// The OS codes that mean "these two paths are not on one volume": `ERROR_NOT_SAME_DEVICE` on
/// Windows, `EXDEV` on everything else.
fn is_cross_device(source: &io::Error) -> bool {
    const CROSS_DEVICE: i32 = if cfg!(windows) { 17 } else { 18 };
    matches!(source.raw_os_error(), Some(code) if code == CROSS_DEVICE)
}

fn io_error(action: Action, from: &Path, to: &Path, _parent: &Path, source: io::Error) -> Error {
    Error::Io {
        action: action.name(),
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        reason: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn a_move_renames_and_the_source_is_gone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = file(temp.path(), "before.txt", "the content\n");
        let to = temp.path().join("after.txt");

        let done = transfer(&from, &to, Action::Move).expect("the move goes through");
        assert_eq!(done.by, By::Renamed, "one volume, so a rename");
        assert!(!from.exists(), "the source is gone");
        assert_eq!(fs::read_to_string(&to).expect("read"), "the content\n");
    }

    /// A directory moves in one call because `rename` takes the subtree with it. This is the case
    /// worth pinning: it is also the only way a directory can move at all here.
    #[test]
    fn a_directory_moves_with_everything_inside_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("folder");
        fs::create_dir_all(from.join("deeper")).expect("tree");
        file(&from, "one.txt", "1\n");
        file(&from.join("deeper"), "two.txt", "2\n");
        let to = temp.path().join("moved");

        transfer(&from, &to, Action::Move).expect("the directory moves");
        assert!(!from.exists());
        assert_eq!(
            fs::read_to_string(to.join("deeper/two.txt")).expect("read"),
            "2\n"
        );
    }

    #[test]
    fn a_copy_leaves_both_and_says_so() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = file(temp.path(), "keep.txt", "both\n");
        let to = temp.path().join("also.txt");

        let done = transfer(&from, &to, Action::Copy).expect("the copy goes through");
        assert_eq!(done.by, By::Copied);
        assert!(from.exists(), "a copy does not take the source");
        assert_eq!(fs::read_to_string(&to).expect("read"), "both\n");
    }

    /// The rule a misremembered drag would break: an existing target is a refusal, and the refusal
    /// leaves both files exactly as they were.
    #[test]
    fn an_existing_target_is_refused_and_nothing_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = file(temp.path(), "source.txt", "wanted\n");
        let to = file(temp.path(), "target.txt", "already here\n");

        let error = transfer(&from, &to, Action::Move).expect_err("overwriting is refused");
        assert!(matches!(error, Error::Exists { .. }), "{error}");
        assert_eq!(fs::read_to_string(&from).expect("read"), "wanted\n");
        assert_eq!(fs::read_to_string(&to).expect("read"), "already here\n");

        let error =
            transfer(&from, &to, Action::Copy).expect_err("a copy does not overwrite either");
        assert!(matches!(error, Error::Exists { .. }), "{error}");
    }

    #[test]
    fn a_missing_source_and_a_vanished_target_folder_are_both_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gone = temp.path().join("not-here.txt");
        let error = transfer(&gone, &temp.path().join("x.txt"), Action::Move)
            .expect_err("nothing is there to move");
        assert!(matches!(error, Error::Missing { .. }), "{error}");

        let from = file(temp.path(), "file.txt", "x\n");
        let error = transfer(
            &from,
            &temp.path().join("closed-folder/file.txt"),
            Action::Move,
        )
        .expect_err("the folder it was going into is gone");
        assert!(matches!(error, Error::NoParent { .. }), "{error}");
        assert!(from.exists(), "and the source is untouched: {error}");
    }

    #[test]
    fn copying_a_directory_is_refused_rather_than_walked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("tree");
        fs::create_dir_all(&from).expect("dir");
        file(&from, "leaf.txt", "1\n");

        let error = transfer(&from, &temp.path().join("tree-copy"), Action::Copy)
            .expect_err("a recursive copy is not implemented");
        assert!(matches!(error, Error::CopyUnsupported { .. }), "{error}");
        assert!(from.join("leaf.txt").exists(), "the tree is untouched");
    }

    /// The same path under two names is `SamePath`, not a rename onto itself. The shell guards this
    /// too - dropping a file back in the folder it came from should not even make the call - but the
    /// primitive answers for itself.
    #[test]
    fn a_path_does_not_move_onto_itself() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = file(temp.path(), "here.txt", "x\n");
        let error = transfer(&from, &from, Action::Move).expect_err("that is not a move");
        assert!(matches!(error, Error::SamePath), "{error}");
        assert!(from.exists());
    }

    /// A symlink is moved as the link, not as whatever it points at. `symlink_metadata` is what keeps
    /// that true: following the link here would move a file the person never selected.
    #[test]
    #[cfg(unix)]
    fn a_symlink_moves_as_the_link_and_not_as_its_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = file(temp.path(), "target.txt", "content\n");
        let link = temp.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        transfer(&link, &temp.path().join("link-moved.txt"), Action::Move).expect("the link moves");
        assert!(target.exists(), "what it pointed at is still there");
        assert_eq!(
            fs::read_link(temp.path().join("link-moved.txt")).expect("still a link"),
            target
        );
    }

    /// The cross-volume branches cannot be driven on one disk, so this checks the classifier instead
    /// - and says plainly which paths are therefore unverified.
    #[test]
    fn only_the_different_device_code_turns_a_move_into_a_copy() {
        assert!(!is_cross_device(&io::Error::from_raw_os_error(5)));
        let cross = if cfg!(windows) { 17 } else { 18 };
        assert!(is_cross_device(&io::Error::from_raw_os_error(cross)));
    }
}

#[cfg(test)]
mod removal_tests {
    use super::*;

    #[test]
    fn a_file_is_removed_and_a_second_remove_says_it_cannot_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("gone.txt");
        fs::write(&target, "x\n").expect("file");

        let removed = remove_path(&target).expect("the file goes");
        assert_eq!(removed.kind, "file");
        assert!(!target.exists());

        let error = remove_path(&target).expect_err("nothing left to remove");
        assert!(matches!(error, Error::CantOpen { .. }), "{error:?}");
    }

    #[test]
    fn a_directory_with_entries_is_refused_by_name_and_an_empty_one_goes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let full = temp.path().join("full");
        fs::create_dir_all(full.join("inner")).expect("entries");
        fs::write(full.join("inner").join("a.txt"), "x\n").expect("file");

        let error = remove_path(&full).expect_err("a non-empty directory is refused");
        assert!(
            error.to_string().contains("still has entries in it"),
            "the sentence the gate test pins: {error}"
        );
        assert!(full.exists(), "and the directory is still there");

        let empty = temp.path().join("empty");
        fs::create_dir(&empty).expect("empty dir");
        let removed = remove_path(&empty).expect("an empty directory goes");
        assert_eq!(removed.kind, "dir");
        assert!(!empty.exists());
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn a_copy_reports_progress_and_the_sum_of_the_reports_is_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("big.bin");
        let body = vec![7u8; 3 * CHUNK as usize + 17];
        fs::write(&from, &body).expect("source");
        let to = temp.path().join("out.bin");

        let mut reports: Vec<TransferProgress> = Vec::new();
        transfer_with(&from, &to, Action::Copy, &mut |report| {
            reports.push(report);
            true
        })
        .expect("the copy runs");

        assert_eq!(fs::read(&to).expect("read back"), body, "the bytes arrive");
        let first = *reports.first().expect("at least one report");
        assert_eq!((first.bytes, first.total), (0, 3 * CHUNK + 17));
        let last = *reports.last().expect("at least one report");
        assert_eq!(
            (last.bytes, last.total),
            (body.len() as u64, body.len() as u64)
        );
        // Monotone, and every total is the source's length.
        for pair in reports.windows(2) {
            assert!(pair[0].bytes < pair[1].bytes, "{pair:?}");
            assert_eq!(pair[0].total, body.len() as u64);
        }
    }

    #[test]
    fn a_cancel_before_the_first_chunk_writes_nothing_and_leaves_no_partial() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("big.bin");
        fs::write(&from, vec![7u8; 2 * CHUNK as usize]).expect("source");
        let to = temp.path().join("out.bin");

        let error = transfer_with(&from, &to, Action::Copy, &mut |_| false)
            .expect_err("the person said stop");
        assert!(matches!(error, Error::Cancelled { .. }), "{error:?}");
        assert!(!to.exists(), "no partial destination is left behind");
        assert!(from.exists(), "and the source is untouched");
    }

    #[test]
    fn a_cancel_mid_copy_removes_the_partial_and_keeps_the_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("big.bin");
        fs::write(&from, vec![7u8; 4 * CHUNK as usize]).expect("source");
        let to = temp.path().join("out.bin");

        let mut calls = 0u32;
        let error = transfer_with(&from, &to, Action::Copy, &mut |_| {
            calls += 1;
            calls <= 2
        })
        .expect_err("stopped after two chunks");
        assert!(matches!(error, Error::Cancelled { .. }), "{error:?}");
        assert!(!to.exists(), "the partial copy was swept");
        assert!(from.exists());
        // Two chunks made it before the stop: the callback runs before the first write too.
        assert!(calls >= 3, "{calls}");
    }

    #[test]
    fn a_move_reports_no_progress_because_a_rename_has_no_middle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("a.txt");
        fs::write(&from, "x\n").expect("source");
        let to = temp.path().join("b.txt");
        let mut calls = 0u32;
        transfer_with(&from, &to, Action::Move, &mut |_| {
            calls += 1;
            true
        })
        .expect("renamed");
        assert_eq!(calls, 0, "a same-volume move is one rename, not a copy");
        assert!(!from.exists() && to.exists());
    }
}
