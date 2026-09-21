use fam_core::{name_units_match, DirId, Item, ItemFlags, PathKey};

/// One enumerated directory: a UTF-16 string arena plus fixed-size records that address into it.
///
/// The arena and the records are allocated per directory, so evicting a directory is a single drop
/// with no fragmentation and no per-row allocation header.
///
/// Records are addressed by index, and **an index is stable for the life of the table**: a row that
/// leaves is stamped [`ItemFlags::TOMBSTONE`] rather than removed, because views keep their own
/// ordering as a list of these indices and would otherwise have to be rebuilt for one disappearance.
#[derive(Debug, Clone)]
pub struct DirTable {
    pub id: DirId,
    pub parent: Option<DirId>,
    pub key: PathKey,
    names: Vec<u16>,
    items: Vec<Item>,
    /// Records stamped as tombstones, and the arena units they still hold. Both only ever feed the
    /// "is this table worth re-reading" decision, which is why neither is a `live_rows` substitute.
    dead: u32,
    dead_units: u32,
}

impl DirTable {
    pub fn new(id: DirId, key: PathKey, parent: Option<DirId>) -> Self {
        Self {
            id,
            parent,
            key,
            names: Vec::new(),
            items: Vec::new(),
            dead: 0,
            dead_units: 0,
        }
    }

    /// Appends an entry and stamps its arena offsets and parent id. Returns the row index.
    pub fn push(&mut self, name: &[u16], mut item: Item) -> u32 {
        let index = self.items.len() as u32;
        item.name_off = self.names.len() as u32;
        item.name_units = name.len() as u16;
        item.dir = self.id;
        self.names.extend_from_slice(name);
        self.items.push(item);
        index
    }

    /// Records, tombstoned or not. Use [`DirTable::live_rows`] for "how many rows are there".
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Rows a view may display. O(dead), which is the only reason the count is not a field.
    pub fn live_rows(&self) -> usize {
        self.items.len() - self.dead as usize
    }

    pub fn is_empty(&self) -> bool {
        self.live_rows() == 0
    }

    /// The record at `index`, or `None` if it is out of range *or* has been tombstoned. Reads that
    /// go through this cannot display a row the filesystem no longer has.
    pub fn item(&self, index: u32) -> Option<&Item> {
        let item = self.items.get(index as usize)?;
        (!item.flags().is_tombstone()).then_some(item)
    }

    pub fn name_of(&self, item: &Item) -> &[u16] {
        let start = item.name_off as usize;
        &self.names[start..start + item.name_units as usize]
    }

    /// Stamps a row as gone and keeps its space accounted. `false` means the row was already gone
    /// or never existed, which a racing pair of events can produce.
    pub fn remove(&mut self, index: u32) -> bool {
        let Some(item) = self.items.get_mut(index as usize) else {
            return false;
        };
        if item.flags().is_tombstone() {
            return false;
        }
        self.dead += 1;
        self.dead_units += u32::from(item.name_units);
        item.flags = item.flags().union(ItemFlags::TOMBSTONE).0;
        true
    }

    /// Bytes held by rows nobody can see any more.
    pub fn wasted_bytes(&self) -> usize {
        self.dead_units as usize * 2 + self.dead as usize * core::mem::size_of::<Item>()
    }

    /// The live row whose name is `name`, if the directory has one.
    ///
    /// A scan, not a lookup: an index by name would cost about as much per row as the record itself
    /// to save work that happens once per event window. Patches over a batch of names use
    /// [`DirTable::patched`](crate::DirTable::patched), which walks the records once for all of them.
    pub fn find(&self, name: &[u16]) -> Option<u32> {
        self.live_indices()
            .find(|&index| name_units_match(self.name_at(index), name))
    }

    /// Indices of records still visible. Tombstoned rows keep their index — see the type doc — so
    /// these stay valid for as long as the table does.
    pub fn live_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.flags().is_tombstone())
            .map(|(index, _)| index as u32)
    }

    /// The name of the record at `index`, live or gone. Out of range reads back as no name, which
    /// matches nothing: an empty name is not a name a directory can hold.
    pub fn name_at(&self, index: u32) -> &[u16] {
        self.items
            .get(index as usize)
            .map_or(&[], |item| self.name_of(item))
    }

    /// What this table costs the cache, used by the byte budget once eviction lands.
    pub fn approx_bytes(&self) -> usize {
        self.names.len() * 2 + self.items.len() * core::mem::size_of::<Item>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fam_core::{ItemFlags, Kind};

    fn item(kind: Kind) -> Item {
        Item {
            name_off: 0,
            name_units: 0,
            kind: kind.index(),
            flags: ItemFlags::default().0,
            size: 7,
            mtime_ms: 1,
            attrs: 0,
            dir: 0,
        }
    }

    #[test]
    fn records_address_the_arena_and_carry_the_parent() {
        let key = PathKey::of(std::path::Path::new("D:/docs"));
        let mut table = DirTable::new(4, key, Some(2));

        let name: Vec<u16> = "报告.pdf".encode_utf16().collect();
        let index = table.push(&name, item(Kind::Document));

        assert_eq!(index, 0);
        let stored = table.item(0).unwrap();
        assert_eq!(stored.dir, 4);
        assert_eq!(stored.size, 7);
        assert_eq!(
            String::from_utf16(table.name_of(stored)).unwrap(),
            "报告.pdf"
        );
        assert_eq!(
            table.approx_bytes(),
            name.len() * 2 + core::mem::size_of::<Item>()
        );
    }

    #[test]
    fn lone_surrogates_survive_the_arena() {
        let mut table = DirTable::new(1, PathKey::of(std::path::Path::new("D:/x")), None);
        let name: Vec<u16> = vec![0xD800, b'a' as u16];
        table.push(&name, item(Kind::Other));
        assert_eq!(table.name_of(table.item(0).unwrap()), &name[..]);
    }

    #[test]
    fn a_tombstone_holds_its_place_and_its_bytes() {
        let mut table = DirTable::new(1, PathKey::of(std::path::Path::new("D:/x")), None);
        for name in ["one.txt", "two.txt", "three.txt"] {
            table.push(
                &name.encode_utf16().collect::<Vec<u16>>(),
                item(Kind::Other),
            );
        }
        let live = table.approx_bytes();

        assert!(table.remove(1));
        assert!(!table.remove(1), "removing it again changes nothing");

        assert_eq!(table.len(), 3, "the record stayed where it was");
        assert_eq!(table.live_rows(), 2);
        assert_eq!(
            table.item(1),
            None,
            "and it is not addressable as a row any more"
        );
        assert_eq!(
            table.name_at(1),
            "two.txt".encode_utf16().collect::<Vec<u16>>(),
            "still readable, because a tombstone is a mark on the record, not an erasure"
        );
        assert_eq!(
            table.wasted_bytes(),
            "two.txt".len() * 2 + core::mem::size_of::<Item>()
        );
        assert_eq!(
            table.approx_bytes(),
            live,
            "the table's cost does not shrink"
        );
        assert_eq!(
            table.find(&"two.txt".encode_utf16().collect::<Vec<u16>>()),
            None,
            "and a lookup of the gone name finds nothing"
        );
    }

    #[test]
    fn a_name_differing_only_in_ascii_case_is_the_same_name() {
        let mut table = DirTable::new(1, PathKey::of(std::path::Path::new("D:/x")), None);
        table.push(
            &"Readme.MD".encode_utf16().collect::<Vec<u16>>(),
            item(Kind::Document),
        );

        assert_eq!(
            table.find(&"readme.md".encode_utf16().collect::<Vec<u16>>()),
            Some(0)
        );
        assert_eq!(
            table.find(&"readme.mk".encode_utf16().collect::<Vec<u16>>()),
            None
        );
    }
}
