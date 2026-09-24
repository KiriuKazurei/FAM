//! The link that makes an edit to the audit log show up as a broken chain.
//!
//! The hash itself is [`fam_core::sha256`]; what belongs to the audit log is the *convention* - what
//! gets hashed, and what the first link is.

use fam_core::sha256_hex;

/// The value the first record of a session chains to. Not a hash, and not 32 bytes of zero: a
/// reader should be able to see that nothing preceded it.
pub const GENESIS: &str = "0";

/// What a record's `prev` field must hold: the hash of the line before it, over exactly the bytes
/// written.
///
/// Chaining over the serialized line - rather than over selected fields - is what makes the check
/// independent of this crate's own reader: any line the log did not write fails to hash to the
/// `prev` of the line after it.
///
/// What that catches: a line edited, a line deleted, a line removed from the tail (the session then
/// has no seal), and a session whose seal no longer matches the lines under it. What it does not
/// catch: a line moved to another day's file, which leaves every link and every count intact because
/// nothing here ties a `ts` to the file it sits in - and a full rewrite, since whoever can write the
/// file can recompute the chain, at which point the log is a self-consistent lie and only a copy held
/// somewhere else would say so. Both gaps are listed in the note for this increment; the second is
/// the reason this says tamper-*evident* and never tamper-*proof*.
pub fn link_of(line: &[u8]) -> String {
    sha256_hex(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The convention is the whole of this module, so what is pinned here is the convention: the
    /// digest is hex, lower-case, 64 characters, and taken over the bytes as given - not over a
    /// trimmed or re-encoded version of them.
    #[test]
    fn a_link_is_the_digest_of_exactly_the_bytes_given() {
        assert_eq!(link_of(b"abc"), sha256_hex(b"abc"));
        assert_eq!(link_of(b"abc").len(), 64);
        assert_ne!(
            link_of(
                b"abc
"
            ),
            link_of(b"abc"),
            "the newline is not part of a line"
        );
        assert_ne!(link_of(b"abc "), link_of(b"abc"), "nor is a trailing space");
        assert_eq!(GENESIS, "0");
    }
}
