//! The v2 wire format: one header, fixed-size records, then the names they point into.
//!
//! v1 sent every row as JSON, and the shell paid a `CString` scan, a DOM parse and five `QString`
//! allocations per row to read it. v2 hands the core a buffer the *caller* owns and fills it:
//!
//! ```text
//! offset 0                            FamRowHeader, 40 bytes
//! offset 40                           row_count * record_size bytes of records
//! offset 40 + row_count * record_size name blob: UTF-16 code units, two bytes each
//! ```
//!
//! A record *is* [`Item`] - the same 32 bytes the cache's arena holds - so there is one definition
//! of a row and no `RowOut`-shaped second thing to drift away from it. `name_off`/`name_units` keep
//! their storage meaning of "units into the arena", where on the wire the arena is the blob at the
//! end: a name starts at `40 + row_count * record_size + name_off * 2`.
//!
//! `record_size` is the version mechanism. Fields are appended to `Item` and never reordered, so a
//! shell built against an older header reads the prefix it knows and steps `record_size` per row
//! over whatever the core added since.

use fam_core::Item;

/// 'FRAM' - the first four bytes of any buffer this module wrote.
pub const MAGIC: u32 = 0x4D41_5246;

/// What [`fam_abi_version`](crate::fam_abi_version) reports, and what every header carries.
pub const ABI: u16 = 2;

/// This buffer holds fewer rows than were asked for; the rest are one call away.
pub const FLAG_TRUNCATED: u8 = 0b1;
/// Events were discarded since the last poll, so what is on screen may be missing rows.
pub const FLAG_LOSS: u8 = 0b10;
/// A change arrived that cannot be expressed as positions: rebuild the view from a fresh fetch.
pub const FLAG_RESYNC: u8 = 0b100;
/// The scope was not fully covered, so this result set is a prefix of what a complete search
/// would find. Only a search carries it - a directory page is always the whole directory.
pub const FLAG_INCOMPLETE: u8 = 0b1000;

/// Row availability, as `u8` so the field is the same width under both toolchains.
pub mod state {
    /// Rows are cached and current.
    pub const READY: u8 = 0;
    /// A read is running and there are no previous rows to show yet.
    pub const LOADING: u8 = 1;
    /// Rows are cached but no longer guaranteed to match the filesystem.
    pub const STALE: u8 = 2;
    /// The last read failed, nothing survived it, and the backoff has not elapsed.
    pub const FAILED: u8 = 3;
    /// Nothing cached and nothing in flight.
    pub const EMPTY: u8 = 4;
}

/// One row position for a view to cut, as [`fam_dir_poll`](crate::fam_dir_poll) reports it.
///
/// `u32`, because `usize` has no agreed width across a C ABI. Positions come back in view order and
/// must be applied in that order, each against the rows the earlier cuts left behind - which is what
/// lets a model coalesce a run into a single `beginRemoveRows`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamCut {
    pub position: u32,
    _reserved: u32,
}

/// The header of every v2 buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FamRowHeader {
    pub magic: u32,
    pub abi: u16,
    /// Size of one record in the buffer, so a caller can skip fields it does not know.
    pub record_size: u16,
    /// Records actually written into this buffer.
    pub row_count: u32,
    /// One of [`state`].
    pub state: u8,
    /// `FLAG_` bits.
    pub flags: u8,
    _pad: u16,
    /// Rows the whole view holds, whether or not this buffer carried them.
    pub total_rows: u32,
    _reserved: u32,
    /// Which version of the directory these rows came from. The same generation means the same rows.
    pub generation: u64,
    /// The delta sequence this buffer reflects; advanced by every poll that reported something.
    pub seq: u64,
}

const _: () = assert!(core::mem::size_of::<FamRowHeader>() == 40);
const _: () = assert!(core::mem::align_of::<FamRowHeader>() == 8);
const _: () = assert!(core::mem::size_of::<Item>() == 32);
const _: () = assert!(core::mem::size_of::<FamCut>() == 8);

/// Bytes of one header.
pub const HEADER_BYTES: usize = core::mem::size_of::<FamRowHeader>();
/// Bytes of one row record.
pub const RECORD_BYTES: usize = core::mem::size_of::<Item>();
/// What `record_size` carries for a buffer of rows...
pub const RECORDS: u16 = core::mem::size_of::<Item>() as u16;
/// ...and for a buffer of cuts. A caller tells them apart by which call filled the buffer, and reads
/// `record_size` from the header either way to know how far apart the records are.
pub const CUTS: u16 = core::mem::size_of::<FamCut>() as u16;

pub fn cuts(positions: &[usize]) -> Vec<FamCut> {
    positions
        .iter()
        .map(|&position| FamCut {
            // A view cannot hold more than `u32::MAX` rows in anything this ABI serves: a directory
            // that large is a 4 GB table and the byte budget would have evicted it long before.
            position: position.min(u32::MAX as usize) as u32,
            _reserved: 0,
        })
        .collect()
}

/// A block of `Item` records, ready to copy into a caller's buffer.
///
/// `Item` is `#[repr(C)]` with no trailing padding, so a slice of them *is* contiguous records -
/// but going through the bytes once here keeps the unsafety in one place instead of spreading it
/// across every entry point that hands rows out.
pub fn record_block(rows: &[Item]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(rows.len() * RECORD_BYTES);
    for row in rows {
        bytes.extend_from_slice(record_bytes(row));
    }
    bytes
}

/// A block of [`FamCut`] records.
pub fn cut_block(positions: &[usize]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(positions.len() * core::mem::size_of::<FamCut>());
    for cut in cuts(positions) {
        bytes.extend_from_slice(unsafe {
            // SAFETY: `FamCut` is `#[repr(C)]`, two `u32` fields, no padding, and the value is a
            // local that outlives the copy.
            std::slice::from_raw_parts(
                &cut as *const FamCut as *const u8,
                core::mem::size_of::<FamCut>(),
            )
        });
    }
    bytes
}

fn record_bytes(row: &Item) -> &[u8] {
    // SAFETY: `Item` is `#[repr(C)]` and `Copy`, its size is asserted above to be the sum of its
    // fields, and the returned slice cannot outlive the borrow of `row`.
    unsafe { std::slice::from_raw_parts(row as *const Item as *const u8, RECORD_BYTES) }
}

/// Lays out `header` + `records` + `names` into `buffer`, returning the bytes written.
///
/// Either the whole payload fits or nothing is sent: a row is its record *and* its name, and half a
/// row on the far side is a corrupt record rather than a partial answer. On overflow the header
/// still goes out, with `row_count` 0 and [`FLAG_TRUNCATED`] set, which tells the caller the buffer
/// was too small rather than that the directory is empty.
pub fn encode(
    records: &[u8],
    record_size: u16,
    names: &[u16],
    mut header: FamRowHeader,
    buffer: &mut [u8],
) -> usize {
    header.magic = MAGIC;
    header.abi = ABI;
    header.record_size = record_size;
    let tail = names.len() * 2;
    if buffer.len() < HEADER_BYTES + records.len() + tail {
        header.row_count = 0;
        header.flags |= FLAG_TRUNCATED;
        write_header(&header, buffer);
        return HEADER_BYTES.min(buffer.len());
    }
    header.row_count = if record_size == 0 {
        0
    } else {
        (records.len() / record_size as usize) as u32
    };
    write_header(&header, buffer);
    buffer[HEADER_BYTES..HEADER_BYTES + records.len()].copy_from_slice(records);
    let blob = HEADER_BYTES + records.len();
    for (index, unit) in names.iter().enumerate() {
        let at = blob + index * 2;
        buffer[at] = (*unit & 0xFF) as u8;
        buffer[at + 1] = (*unit >> 8) as u8;
    }
    blob + tail
}

/// Where a caller finds the name blob, given what the header said.
pub fn blob_offset(row_count: usize, record_size: u16) -> usize {
    HEADER_BYTES + row_count * record_size as usize
}

/// The header of a call that carries no rows: a status answer, or a poll with nothing to report.
pub fn status(state: u8, total_rows: u32, generation: u64, seq: u64, flags: u8) -> FamRowHeader {
    FamRowHeader {
        magic: MAGIC,
        abi: ABI,
        record_size: RECORD_BYTES as u16,
        row_count: 0,
        state,
        flags,
        _pad: 0,
        total_rows,
        _reserved: 0,
        generation,
        seq,
    }
}

fn write_header(header: &FamRowHeader, buffer: &mut [u8]) {
    if buffer.len() < HEADER_BYTES {
        return;
    }
    // SAFETY: `FamRowHeader` is `#[repr(C)]` with no padding that carries a value, and the buffer
    // length was just checked to be at least the struct's size. Reading it back as bytes does not
    // create an alias - this only copies out.
    let bytes = unsafe {
        std::slice::from_raw_parts(header as *const FamRowHeader as *const u8, HEADER_BYTES)
    };
    buffer[..HEADER_BYTES].copy_from_slice(bytes);
}
