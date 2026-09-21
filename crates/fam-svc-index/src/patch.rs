//! Folding one directory's worth of watcher rows into a cached table.
//!
//! This is the half of the event pipeline that decides *what may be applied in place*. The other
//! half - grouping rows by directory and merging two observations of one path - is
//! `fam_sched::Batch::by_directory`, and the line between them is deliberate: the scheduler knows
//! what was reported, this module knows what a cached row can afford to become.
//!
//! # What a row-level patch may and may not do
//!
//! A change event carries a name and an action, nothing else
//! (`docs/notes/event-carries-no-target-type.md`). A disappearance is the one thing that can be
//! answered from the row already held: no I/O, no guessing, apply it.
//!
//! An appearance or a modification cannot be answered at all. A new row's size, time and
//! file-or-folder status are not in the event, and going to get them costs a path lookup - ~190us
//! measured on the development machine, against ~231us to re-read a whole 188-entry directory
//! (`docs/notes/watch-sweep-costs-a-path-lookup.md`). One query per new row is therefore
//! break-even at about one row and loses badly on a bulk paste, so a patch carrying such rows marks
//! the table for a re-read instead - while still placing whatever rows it *could* place. Three files
//! deleted and one created in a window leaves the three gone on screen immediately, and the new one
//! arrives with the read.

use std::collections::HashMap;

use fam_core::name_units_match;

use crate::dir_table::DirTable;

/// What one directory's worth of events said, as far as a cached table can use it.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    /// Names reported gone. These are placed in place.
    pub removed: Vec<Vec<u16>>,
    /// Names reported created or changed, counted not held: no event supplies a row for them, so
    /// all they establish is that the cached rows are no longer the whole truth.
    pub unread: usize,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.unread == 0
    }
}

/// What one [`Patch`] did to one table.
pub struct Patched {
    /// The table with its rows placed. A patch that placed nothing still returns a copy, so
    /// [`Patched::placed`] - not a comparison - is the caller's cue to install it or drop it.
    pub table: DirTable,
    /// Record indices this patch turned from live to gone.
    pub removed: Vec<u32>,
    /// Removed names that matched no live row: already gone, or never in this snapshot. Two
    /// consecutive deletions of one file is a normal sequence, so this counts and is never an error.
    pub missed: usize,
    /// Rows still owed a filesystem read, carried through from [`Patch::unread`].
    pub unread: usize,
}

impl Patched {
    /// Whether the table is worth installing, or the rows on offer are exactly as they were. A patch
    /// that only named arrivals counts as placed, because the freshness a caller must show changed.
    pub fn placed(&self) -> bool {
        !self.removed.is_empty() || self.unread > 0
    }
}

/// A hash over the name as the filesystem would compare it: ASCII folded, everything else verbatim.
///
/// Only a bucket key - every candidate is still checked with [`name_units_match`] before a row is
/// removed, so a collision costs a comparison and cannot cost a wrong row.
fn folded_hash(name: &[u16]) -> u64 {
    const ASCII_A: u16 = b'a' as u16;
    const ASCII_Z: u16 = b'z' as u16;
    const DELTA: u16 = b'a' as u16 - b'A' as u16;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in name {
        let folded = if (ASCII_A..=ASCII_Z).contains(unit) {
            unit - DELTA
        } else {
            *unit
        };
        hash = (hash ^ u64::from(folded)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl DirTable {
    /// Returns a copy with `patch` folded in.
    ///
    /// Copying rather than mutating in place is what lets a [`Snapshot`](crate::cache::Snapshot)
    /// handed to another thread stay usable while this patch is computed: a table someone is reading
    /// from is never changed underneath them, and installing the result is one pointer swap. Row
    /// indices stay valid across the copy because a gone row is tombstoned where it stands
    /// ([`ItemFlags::TOMBSTONE`](fam_core::ItemFlags)), which is what lets a view apply the same
    /// `removed` list to its own ordering instead of rebuilding it.
    ///
    /// Names are resolved by one walk of the records for the whole batch. Comparing every name
    /// against every record instead costs rows x names per window - 20k rows and a bulk delete is a
    /// hundred million comparisons, on the thread that also delivers batches to the UI.
    pub fn patched(&self, patch: &Patch) -> Patched {
        let mut table = self.clone();
        let mut candidates: HashMap<u64, Vec<u32>> = patch
            .removed
            .iter()
            .map(|name| (folded_hash(name), Vec::new()))
            .collect();
        for index in self.live_indices() {
            let name = self.name_at(index);
            if let Some(bucket) = candidates.get_mut(&folded_hash(name)) {
                bucket.push(index);
            }
        }

        let mut removed = Vec::with_capacity(patch.removed.len());
        let mut missed = 0usize;
        for name in &patch.removed {
            let hit = candidates
                .get_mut(&folded_hash(name))
                .and_then(|bucket| {
                    let position = bucket
                        .iter()
                        .position(|&index| name_units_match(self.name_at(index), name))?;
                    Some(bucket.swap_remove(position))
                })
                .filter(|&index| table.remove(index));
            match hit {
                Some(index) => removed.push(index),
                None => missed += 1,
            }
        }
        Patched {
            table,
            removed,
            missed,
            unread: patch.unread,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use fam_core::{Item, ItemFlags, Kind, PathKey};

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    fn table_with(names: &[&str]) -> DirTable {
        let mut table = DirTable::new(3, PathKey::of(std::path::Path::new("D:/docs")), None);
        for name in names {
            table.push(
                &units(name),
                Item {
                    name_off: 0,
                    name_units: 0,
                    kind: Kind::Other.index(),
                    flags: ItemFlags::default().0,
                    size: 4,
                    mtime_ms: 0,
                    attrs: 0,
                    dir: 0,
                },
            );
        }
        table
    }

    fn removals(names: &[&str]) -> Patch {
        Patch {
            removed: names.iter().map(|name| units(name)).collect(),
            unread: 0,
        }
    }

    fn names_of(table: &DirTable) -> Vec<String> {
        table
            .live_indices()
            .map(|index| String::from_utf16_lossy(table.name_at(index)))
            .collect()
    }

    #[test]
    fn a_removed_name_leaves_the_other_records_where_they_were() {
        let table = table_with(&["a.txt", "b.txt", "c.txt"]);

        let patched = table.patched(&removals(&["b.txt"]));

        assert_eq!(
            patched.removed,
            vec![1],
            "the middle record is the one that went"
        );
        assert_eq!(patched.missed, 0);
        assert!(patched.placed());
        assert_eq!(names_of(&patched.table), ["a.txt", "c.txt"]);
        // The indices a view holds still address the same rows.
        assert_eq!(patched.table.name_at(0), units("a.txt"));
        assert_eq!(patched.table.name_at(2), units("c.txt"));
        assert!(patched.table.item(1).is_none(), "a tombstone is not a row");
        assert_eq!(
            patched.table.len(),
            3,
            "the record is still taking its place"
        );
        assert_eq!(patched.table.live_rows(), 2);
    }

    #[test]
    fn the_original_table_is_untouched_by_a_patch() {
        let table = table_with(&["a.txt", "b.txt"]);
        // What the cache hands out is an `Arc` of the table; a reader holding one must keep seeing
        // the rows it was given while somebody else patches.
        let shared = Arc::new(table);

        let patched = shared.patched(&removals(&["a.txt"]));

        assert_eq!(
            names_of(&shared),
            ["a.txt", "b.txt"],
            "the copy patched, not the original"
        );
        assert_eq!(names_of(&patched.table), ["b.txt"]);
        assert_eq!(
            Arc::strong_count(&shared),
            1,
            "a patch borrows, it does not take"
        );
    }

    #[test]
    fn a_name_asked_twice_deletes_once_and_reports_the_second_as_gone() {
        let table = table_with(&["gone.txt", "kept.txt"]);

        let patched = table.patched(&removals(&["gone.txt", "gone.txt"]));

        assert_eq!(patched.removed.len(), 1);
        assert_eq!(
            patched.missed, 1,
            "the second event explained itself, it changed nothing"
        );
        assert_eq!(names_of(&patched.table), ["kept.txt"]);
    }

    #[test]
    fn an_event_naming_a_different_ascii_case_still_finds_the_row() {
        // The table holds what enumeration reported; the notification reports what the creating
        // program asked for. NTFS keeps one file, so one row must go.
        let table = table_with(&["Report.PDF"]);

        let patched = table.patched(&removals(&["report.pdf"]));

        assert_eq!(patched.removed, vec![0]);
        assert!(patched.table.is_empty());
    }

    #[test]
    fn a_name_that_never_existed_is_counted_not_guessed_at() {
        let table = table_with(&["a.txt"]);

        let patched = table.patched(&removals(&["absent.txt", "a.txt"]));

        assert_eq!(patched.missed, 1);
        assert_eq!(patched.removed, vec![0]);
        assert_eq!(patched.table.wasted_bytes(), 32 + 2 * "a.txt".len());
    }

    #[test]
    fn a_bulk_delete_resolves_every_name_in_one_walk() {
        let names: Vec<String> = (0..500).map(|index| format!("f{index:04}.bin")).collect();
        let table = table_with(&names.iter().map(String::as_str).collect::<Vec<&str>>());
        let wanted: Vec<&str> = names.iter().step_by(2).map(String::as_str).collect();

        let patched = table.patched(&removals(&wanted));

        assert_eq!(patched.removed.len(), 250, "every asked name placed");
        assert_eq!(patched.missed, 0);
        assert_eq!(patched.table.live_rows(), 250);
    }

    #[test]
    fn a_created_row_asks_for_a_read_without_inventing_a_row() {
        let table = table_with(&["a.txt"]);

        let patched = table.patched(&Patch {
            removed: Vec::new(),
            unread: 1,
        });

        assert!(
            patched.removed.is_empty(),
            "nothing may be placed from an event that carried no attributes"
        );
        assert_eq!(patched.unread, 1, "the caller still owes a read");
        assert!(
            patched.placed(),
            "`placed` is the cue to install, not the row count"
        );
        assert_eq!(
            names_of(&patched.table),
            ["a.txt"],
            "the table is as it was"
        );
    }

    #[test]
    fn an_empty_patch_places_nothing() {
        let table = table_with(&["a.txt"]);

        let patched = table.patched(&Patch::default());

        assert!(!patched.placed());
        assert!(patched.removed.is_empty());
        assert_eq!(patched.missed, 0);
    }
}
