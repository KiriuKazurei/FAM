use fam_core::{DirId, Item, PathKey};

/// One enumerated directory: a UTF-16 string arena plus fixed-size records that address into it.
///
/// The arena and the records are allocated per directory, so evicting a directory is a single drop
/// with no fragmentation and no per-row allocation header.
#[derive(Debug)]
pub struct DirTable {
    pub id: DirId,
    pub parent: Option<DirId>,
    pub key: PathKey,
    names: Vec<u16>,
    items: Vec<Item>,
}

impl DirTable {
    pub fn new(id: DirId, key: PathKey, parent: Option<DirId>) -> Self {
        Self {
            id,
            parent,
            key,
            names: Vec::new(),
            items: Vec::new(),
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

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item(&self, index: u32) -> Option<&Item> {
        self.items.get(index as usize)
    }

    pub fn name_of(&self, item: &Item) -> &[u16] {
        let start = item.name_off as usize;
        &self.names[start..start + item.name_units as usize]
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
}
