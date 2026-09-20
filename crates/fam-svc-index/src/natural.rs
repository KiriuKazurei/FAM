//! Name comparison that agrees with Explorer.
//!
//! The list is sorted with `StrCmpLogicalW` rather than an ordinal compare, because that is what
//! the shell does and a file manager that orders `file10` before `file2` looks broken no matter how
//! defensible the collation is. It lives here rather than in `fam-core` because it is a platform
//! call and `fam-core` carries none.

use std::cmp::Ordering;

#[cfg(windows)]
mod implementation {
    use std::cmp::Ordering;

    use windows_sys::Win32::UI::Shell::StrCmpLogicalW;

    /// NTFS caps one name at 255 UTF-16 code units, so 256 plus the terminator covers every real
    /// input from the stack. A sort of 100k rows is 1.7M comparisons, and the two heap buffers per
    /// comparison that this replaces would dominate it.
    const STACK_UNITS: usize = 257;

    /// Compares two names as the shell does. Case-insensitive, digit runs read as numbers.
    pub fn compare(left: &[u16], right: &[u16]) -> Ordering {
        if left.len() < STACK_UNITS && right.len() < STACK_UNITS {
            let mut first = [0u16; STACK_UNITS];
            let mut second = [0u16; STACK_UNITS];
            first[..left.len()].copy_from_slice(left);
            second[..right.len()].copy_from_slice(right);
            call(&first[..left.len() + 1], &second[..right.len() + 1])
        } else {
            let first: Vec<u16> = left.iter().copied().chain([0]).collect();
            let second: Vec<u16> = right.iter().copied().chain([0]).collect();
            call(&first, &second)
        }
    }

    fn call(left: &[u16], right: &[u16]) -> Ordering {
        // SAFETY: both arguments are NUL-terminated (the slices were built to end in a zero unit)
        // and both buffers outlive the call, which does not retain either pointer.
        unsafe { StrCmpLogicalW(left.as_ptr(), right.as_ptr()) }.cmp(&0)
    }
}

#[cfg(not(windows))]
mod implementation {
    use std::cmp::Ordering;

    /// No shell comparator to agree with; ordinal keeps the order stable and total.
    pub fn compare(left: &[u16], right: &[u16]) -> Ordering {
        left.cmp(right)
    }
}

use implementation::compare;

/// The crate-wide name comparison. `pub(crate)` on purpose: the ABI hands the shell sorted rows,
/// never a collation callback.
pub(crate) fn compare_units(left: &[u16], right: &[u16]) -> Ordering {
    compare(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Everything below pins the Windows comparator, which is the one that ships. The portable
    /// branch gets its own test so a reader can see what it guarantees rather than infer it.
    #[cfg(windows)]
    #[test]
    fn digit_runs_read_as_numbers() {
        assert_eq!(
            compare_units(&units("file2"), &units("file10")),
            Ordering::Less
        );
        assert_eq!(
            compare_units(&units("a1b2.txt"), &units("a1b10.txt")),
            Ordering::Less
        );
        assert_eq!(
            compare_units(&units("Track 3.mp3"), &units("Track 30.mp3")),
            Ordering::Less
        );
    }

    #[cfg(windows)]
    #[test]
    fn case_differences_tie_so_the_caller_has_to_break_them() {
        assert_eq!(
            compare_units(&units("A.TXT"), &units("a.txt")),
            Ordering::Equal
        );
        assert_ne!(
            units("A.TXT").cmp(&units("a.txt")),
            Ordering::Equal,
            "the ordinal tie-break `Collection` applies still separates them"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_portable_comparator_is_ordinal() {
        assert_eq!(
            compare_units(&units("file10"), &units("file2")),
            Ordering::Less,
            "without the shell comparator, digits compare as characters"
        );
        assert_eq!(
            compare_units(&units("A.TXT"), &units("a.txt")),
            Ordering::Less
        );
    }

    #[test]
    fn comparison_is_antisymmetric_over_a_mixed_set() {
        let names = [
            "file1",
            "file10",
            "file2",
            "报告.pdf",
            "REPORT2.PDF",
            "a.b.c",
            "0",
            "00",
            "1",
            "报告2",
        ];
        for left in names {
            for right in names {
                let forward = compare_units(&units(left), &units(right));
                let backward = compare_units(&units(right), &units(left));
                assert_eq!(forward, backward.reverse(), "{left} vs {right}");
            }
        }
    }

    #[test]
    fn names_past_the_stack_buffer_still_compare() {
        let long = "x".repeat(300);
        let longer = format!("{}y", "x".repeat(300));
        assert_eq!(
            compare_units(&units(&long), &units(&longer)),
            Ordering::Less
        );
        assert_eq!(
            compare_units(&units(&longer), &units(&long)),
            Ordering::Greater
        );
    }
}
