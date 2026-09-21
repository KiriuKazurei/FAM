use serde::Serialize;

use crate::pathkey::PathKey;

pub type DirId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ItemFlags(pub u8);

impl ItemFlags {
    pub const DIRECTORY: ItemFlags = ItemFlags(0b001);
    pub const REPARSE: ItemFlags = ItemFlags(0b010);
    pub const EXCLUDED: ItemFlags = ItemFlags(0b100);
    /// A row a filesystem event removed. The record stays put so every other row keeps its index -
    /// which is what lets a view update its own ordering from the same event - and only the next
    /// full read of the directory reclaims the space.
    ///
    /// A wire record never carries this bit: views hand out live rows only.
    pub const TOMBSTONE: ItemFlags = ItemFlags(0b1000);

    pub const fn contains(self, other: ItemFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: ItemFlags) -> Self {
        ItemFlags(self.0 | other.0)
    }

    pub const fn without(self, other: ItemFlags) -> Self {
        ItemFlags(self.0 & !other.0)
    }

    pub const fn is_directory(self) -> bool {
        self.contains(Self::DIRECTORY)
    }

    pub const fn is_reparse(self) -> bool {
        self.contains(Self::REPARSE)
    }

    pub const fn is_tombstone(self) -> bool {
        self.contains(Self::TOMBSTONE)
    }
}

/// One directory entry.
///
/// This is simultaneously the arena record and the wire record, so there is exactly one definition
/// of a row and no `RowOut`-style twin to drift. The name itself lives in the owning directory's
/// string arena and is addressed by `name_off`/`name_units`, which is what keeps a 100k-row
/// directory near 32 bytes per row instead of a `PathBuf` each.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Item {
    pub name_off: u32,
    pub name_units: u16,
    /// [`crate::Kind`] as its declaration index.
    pub kind: u8,
    pub flags: u8,
    pub size: u64,
    /// `i64::MIN` means unknown.
    pub mtime_ms: i64,
    /// Raw `dwFileAttributes` on Windows; 0 elsewhere.
    pub attrs: u32,
    pub dir: DirId,
}

// The layout must stay byte-identical across the MSVC (CLI/tests) and GNU (DLL) toolchains, so it
// is asserted rather than assumed.
const _: () = assert!(core::mem::size_of::<Item>() == 32);
const _: () = assert!(core::mem::align_of::<Item>() == 8);

impl Item {
    pub fn flags(&self) -> ItemFlags {
        ItemFlags(self.flags)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRequest {
    pub dir: PathKey,
    pub include_directories: bool,
}

impl ListRequest {
    pub fn new(dir: PathKey) -> Self {
        Self {
            dir,
            include_directories: true,
        }
    }

    pub fn files_only(mut self) -> Self {
        self.include_directories = false;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_record_is_thirty_two_bytes() {
        assert_eq!(core::mem::size_of::<Item>(), 32);
    }

    #[test]
    fn flags_compose() {
        let both = ItemFlags::DIRECTORY.union(ItemFlags::REPARSE);
        assert!(both.is_directory());
        assert!(both.is_reparse());
        assert!(!both.contains(ItemFlags::EXCLUDED));
        assert!(!ItemFlags::DIRECTORY.contains(ItemFlags::REPARSE));

        let gone = both.union(ItemFlags::TOMBSTONE);
        assert!(gone.is_tombstone());
        assert_eq!(
            gone.without(ItemFlags::TOMBSTONE),
            both,
            "the mark comes off alone, so a re-read of the same record keeps what it was"
        );
        assert!(!both.is_tombstone());
    }
}
