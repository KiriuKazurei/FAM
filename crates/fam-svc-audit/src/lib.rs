//! The audit trail: one JSON line per request the core answers, chained so that an edit shows up.
//!
//! This exists because the tool is about to be driven by something other than a person at the
//! keyboard. An agent with a file browser can read the whole filesystem in an afternoon, and "what
//! did it look at" is only answerable if every lookup left a line behind. So the shape is the one an
//! audit needs: append-only, written by the layer that answers the request rather than by the caller
//! that might forget, and self-describing about its own gaps - a record that could not be written
//! raises a count the next record carries, so a full disk leaves a log that says so rather than a
//! shorter log that does not.
//!
//! Three properties worth stating before reading the code, because each one is a trade:
//!
//! * **The chain is per session, not per file.** Two processes appending one day's file would each
//!   hash the same previous line and fork the chain, which `verify` could not tell from tampering.
//!   A session that seals itself - "I wrote N lines" - is what makes a removed file or a cut tail
//!   visible instead.
//! * **Writes are synchronous.** A queue in front of a hash chain has to drop when it fills, and a
//!   dropped record is a hole in the thing the chain is for. The cost is one `WriteFile` per audited
//!   call on the calling thread.
//! * **Nothing here is a security boundary.** Whoever can write the log can recompute every link in
//!   it; what they cannot easily do is delete or edit a line and leave the file looking untouched.
//!   The guarantee is tamper-*evident*, not tamper-*proof*, and it only bites once somebody runs
//!   [`inspect::verify`].

use std::io;
use std::path::{Path, PathBuf};

use fam_core::PathKey;

mod actor;
mod chain;
mod inspect;
mod record;
mod writer;

pub use actor::Actor;
pub use chain::{link_of, GENESIS};
pub use inspect::{files, summary, tail, verify, FileSummary, Finding, Problem, Report, Summary};
pub use record::{clip, Draft, Op, Record, Weight};
pub use writer::{
    default_directory, file_name_for, system_clock, AuditLog, Clock, Health, Options, FILE_PREFIX,
    FILE_SUFFIX,
};

/// The one directory name the audit log ever uses, so `fam-core` can point the exclusion rules at it
/// without the two of them disagreeing.
pub const DIRECTORY_NAME: &str = "audit";

/// The root that sits at or below the audit directory, if there is one.
///
/// An audit directory *inside* a search root is normal - on Windows the data directory lives under
/// the user's home, which is the default root - and [`fam_core::Prune`] keeps the log out of what
/// gets enumerated, watched and searched. A search root *inside* the audit directory is a different
/// thing: honouring it would mean writing the log into the tree the user asked to index, and
/// excluding that tree would silently delete a root they named. Neither is a decision this program
/// should make on its own, so it refuses to start and says which root is in the way.
pub fn conflicting_root(directory: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let audit = PathKey::of(directory);
    roots
        .iter()
        .find(|root| audit.is_within(&PathKey::of(root)))
        .cloned()
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot use {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("no audit log in {} yet", path.display())]
    Missing { path: PathBuf },
    #[error("{}:{line} is not an audit record: {reason}", path.display())]
    Line {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("the system clock is outside the range an audit record can carry")]
    Clock,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_underneath_the_audit_directory_conflicts() {
        let audit = Path::new("C:/Users/Kirig/AppData/Roaming/FileAutoManage/audit");
        let roots = vec![PathBuf::from(
            "C:/Users/Kirig/AppData/Roaming/FileAutoManage/audit/nested",
        )];
        assert_eq!(conflicting_root(audit, &roots), Some(roots[0].clone()));
        assert_eq!(
            conflicting_root(audit, &[]),
            None,
            "no roots configured, nothing to conflict with"
        );
    }

    /// The default Windows layout, and the reason "refuse whenever the log is inside a root" is not
    /// the rule: the data directory is under the home directory, and the home directory is the
    /// default root. That case is the exclusion's job - the reverse one, a root placed inside the
    /// log, is what refuses to start, and `a_root_underneath_the_audit_directory_conflicts` has it.
    #[test]
    fn a_log_inside_a_root_is_the_ordinary_case() {
        let audit = Path::new("C:/Users/Kirig/AppData/Roaming/FileAutoManage/audit");
        assert_eq!(
            conflicting_root(audit, &[PathBuf::from("C:/Users/Kirig")]),
            None,
            "the data directory is under the default home root, and that is the exclusion's job"
        );
        assert_eq!(
            conflicting_root(
                Path::new("D:/fam-audit"),
                &[PathBuf::from("C:/Users/Kirig")]
            ),
            None,
            "a log on a volume of its own is in no one's root"
        );
    }

    #[test]
    fn the_comparison_is_the_folded_one() {
        let audit = Path::new("d:\\FAM-AUDIT");
        let roots = vec![PathBuf::from("D:/fam-audit/sub")];
        assert_eq!(conflicting_root(audit, &roots), Some(roots[0].clone()));
    }
}
