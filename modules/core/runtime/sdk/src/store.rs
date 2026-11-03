//! The module and sample store, version 3 (docs/design/digikit-store.md): the format, the readers and the one writer (a copy-on-commit
//! table), shared by the runtime and by tests on the host. `#![no_std]` and ColdFire rules: no `match` on integers, no `core::fmt`, no
//! unchecked slice indexing in a loop that a card error can lengthen.
//!
//! The store is a run of `Layout::sectors` card sectors from `Layout::first` (the device file's `[card.store]`; a device with no room has none):
//!
//! ```text
//! sector 0               header copy A
//! ...                    tables and file data, anywhere in [1, header_b): free space is reused
//! header_b = sectors - 35 header copy B
//! sectors - 34 .. - 3    the settings record, two copies of `settings::SECTORS` sectors (the runtime's; not a store structure)
//! sectors - 2, - 1       the selection record, two copies (the runtime's; not a store structure)
//! ```
//!
//! A header names the table; a table is `count` entries of 256 bytes sorted by (first sector, path), written whole, in free space, on every
//! commit, and the headers switch to it (B first, then A, `seq + 1`). Nothing in use is ever overwritten, so a power cut leaves the old store
//! or the new one. The table is streamed through small buffers: no table is held whole in RAM, so a store holds as many entries as the card has
//! room for.
use crate::proto::{E_FULL, E_WRITE};
use crate::{E_BAD_STORE, E_NOT_FOUND, E_READ, OK};

pub const MAGIC: u32 = 0x444b_5354; // "DKST"
pub const VERSION: u32 = 3;
/// Bytes of a table entry, and of its path field (NUL-terminated inside: a path has at most 159 bytes).
pub const ENTRY: usize = 256;
pub const PATH_BYTES: usize = 160;
pub const PATH_MAX: usize = PATH_BYTES - 1;
const O_FIRST: usize = 160;
const O_SIZE: usize = 164;
const O_SHA: usize = 168;
const O_KIND: usize = 200;
pub const KIND_FILE: u8 = 0;
/// A folder that has no file in it (a folder with files is implied by their paths). No data, size 0.
pub const KIND_DIR: u8 = 1;
/// Sectors of the selection record (two copies of one sector).
pub const SEL_SECTORS: u32 = 2;
/// Sectors of the settings record (`sdk/sdk.toml [settings]`: two copies of `settings::SECTORS`).
pub const SET_SECTORS: u32 = crate::gen_sdk::settings::COPIES * crate::gen_sdk::settings::SECTORS;
/// Sectors after the last one files may use: header copy B, the settings record and the two sectors of the selection record.
pub const TAIL: u32 = 1 + SET_SECTORS + SEL_SECTORS;
/// Sectors one card read or write moves at most (the firmware's block read takes 64 KiB).
pub const MAX_IO_SECTORS: u32 = 128;

/// Where the store is: the first card sector and the number of sectors (`devices/*.toml [card.store]`, per device).
#[derive(Clone, Copy)]
pub struct Layout {
    pub first: u32,
    pub sectors: u32,
}

impl Layout {
    /// Sector of header copy B, relative to the store.
    pub const fn header_b(&self) -> u32 {
        self.sectors - TAIL
    }
    /// Header A, tables and files live in `[0, limit)`.
    pub const fn limit(&self) -> u32 {
        self.sectors - TAIL
    }
    /// Absolute sectors of the two selection record copies.
    pub const fn sel_a(&self) -> u32 {
        self.first + self.sectors - 2
    }
    pub const fn sel_b(&self) -> u32 {
        self.first + self.sectors - 1
    }
    /// Absolute first sectors of the two settings record copies (`settings::SECTORS` sectors each), before the selection record.
    pub const fn set_a(&self) -> u32 {
        self.first + self.sectors - SEL_SECTORS - SET_SECTORS
    }
    pub const fn set_b(&self) -> u32 {
        self.set_a() + crate::gen_sdk::settings::SECTORS
    }
    /// May a card write of `bytes` (a multiple of 512, not 0) starting at absolute `sector` happen? Only inside the store area: the runtime's one
    /// card-write function refuses anything else (finding 46).
    pub const fn write_allowed(&self, sector: u32, bytes: u32) -> bool {
        if bytes == 0 || bytes & 511 != 0 {
            return false;
        }
        let n = bytes / 512;
        sector >= self.first && n <= self.sectors && sector - self.first <= self.sectors - n
    }
}

/// The firmware's eMMC block read and write: `(absolute sector, byte count, buffer)`; 0 = ok. Buffer 16-byte aligned, the count a multiple of 512,
/// at most 0x10000 per call.
pub type BlockIo = unsafe extern "C" fn(sector: u32, bytes: u32, buf: *mut u8) -> i32;

/// The card: how the store reads and writes. The runtime's `wr` is its one gated card-write function.
#[derive(Clone, Copy)]
pub struct Io {
    pub rd: BlockIo,
    pub wr: BlockIo,
}

pub unsafe fn be32(p: *const u8) -> u32 {
    ((*p as u32) << 24) | ((*p.add(1) as u32) << 16) | ((*p.add(2) as u32) << 8) | (*p.add(3) as u32)
}

pub unsafe fn put32(p: *mut u8, v: u32) {
    *p = (v >> 24) as u8;
    *p.add(1) = (v >> 16) as u8;
    *p.add(2) = (v >> 8) as u8;
    *p.add(3) = v as u8;
}

pub unsafe fn cstr_eq(a: *const u8, b: *const u8) -> bool {
    let mut i = 0;
    loop {
        let x = *a.add(i);
        if x != *b.add(i) {
            return false;
        }
        if x == 0 {
            return true;
        }
        i += 1;
    }
}

/// Byte order of two NUL-terminated paths (at most `PATH_BYTES`): negative, 0, positive.
pub unsafe fn cstr_cmp(a: *const u8, b: *const u8) -> i32 {
    let mut i = 0;
    while i < PATH_BYTES {
        let x = *a.add(i);
        let y = *b.add(i);
        if x != y {
            return x as i32 - y as i32;
        }
        if x == 0 {
            return 0;
        }
        i += 1;
    }
    0
}

/// CRC-32 (IEEE, reflected) before the final inversion; `crc32_of` is the finished value (zlib's).
pub fn crc32(mut crc: u32, p: *const u8, n: usize) -> u32 {
    let mut i = 0;
    while i < n {
        crc ^= unsafe { *p.add(i) } as u32;
        let mut k = 0;
        while k < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            k += 1;
        }
        i += 1;
    }
    crc
}

pub fn crc32_of(p: *const u8, n: usize) -> u32 {
    !crc32(0xffff_ffff, p, n)
}

pub const fn sectors_of(bytes: u32) -> u32 {
    bytes / 512 + ((bytes & 511 != 0) as u32)
}

/// Sectors of a table of `count` entries.
pub const fn table_sectors(count: u32) -> u32 {
    (count + 1) / 2
}

// ---- reads --------------------------------------------------------------------------------------------------------------

/// Read `count` sectors starting `rel` sectors into the store; `cap` is the size of `buf` in bytes.
pub unsafe fn read_sectors(io: &Io, lay: &Layout, rel: u32, count: u32, buf: *mut u8, cap: usize) -> i32 {
    if rel >= lay.sectors || count > lay.sectors - rel || count as usize * 512 > cap || count > MAX_IO_SECTORS {
        return E_BAD_STORE;
    }
    if (io.rd)(lay.first + rel, count * 512, buf) != 0 {
        return E_READ;
    }
    OK
}

/// The store sector where `count` sectors of a file start, `sector` sectors into it (`first` and `size` are the file's table entry). The
/// range must lie inside the file rounded up to whole sectors, and `count` is 1 to `MAX_IO_SECTORS`: `E_RANGE` otherwise. A file's sectors
/// are contiguous.
pub fn file_span(first: u32, size: u32, sector: u32, count: u32) -> Result<u32, i32> {
    let sectors = (size as u64 + 511) / 512;
    if count == 0 || count > MAX_IO_SECTORS || sector as u64 + count as u64 > sectors {
        return Err(crate::E_RANGE);
    }
    first.checked_add(sector).ok_or(crate::E_RANGE)
}

/// One header copy as the sector holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hdr {
    pub count: u32,
    pub table_sectors: u32,
    /// Where the table starts, relative to the store (1 when there is no table).
    pub table_first: u32,
    pub table_crc: u32,
    pub seq: u32,
    /// Sectors in use: header A, the table and every file.
    pub used: u32,
    /// The store's size in sectors when it was written: a store made for another area is not this one.
    pub area: u32,
}

impl Hdr {
    /// The header of a store with nothing in it (`seq` 0: nothing was ever committed).
    pub const fn empty(lay: &Layout) -> Hdr {
        Hdr { count: 0, table_sectors: 0, table_first: 1, table_crc: 0, seq: 0, used: 1, area: lay.sectors }
    }
}

/// Decode a header sector: the magic, version 3 and the CRC over bytes 0..508. `None` when any fails.
pub unsafe fn parse_header(p: *const u8) -> Option<Hdr> {
    if be32(p) != MAGIC || be32(p.add(4)) != VERSION || be32(p.add(508)) != crc32_of(p, 508) {
        return None;
    }
    Some(Hdr {
        count: be32(p.add(8)),
        table_sectors: be32(p.add(12)),
        table_first: be32(p.add(16)),
        table_crc: be32(p.add(20)),
        seq: be32(p.add(24)),
        used: be32(p.add(28)),
        area: be32(p.add(32)),
    })
}

/// Write the 512 bytes of a header copy to `out`.
pub unsafe fn encode_header(h: &Hdr, out: *mut u8) {
    let mut i = 0;
    while i < 512 {
        *out.add(i) = 0;
        i += 1;
    }
    put32(out, MAGIC);
    put32(out.add(4), VERSION);
    put32(out.add(8), h.count);
    put32(out.add(12), h.table_sectors);
    put32(out.add(16), h.table_first);
    put32(out.add(20), h.table_crc);
    put32(out.add(24), h.seq);
    put32(out.add(28), h.used);
    put32(out.add(32), h.area);
    put32(out.add(508), crc32_of(out, 508));
}

/// Is a decoded header consistent with the layout and with itself?
fn plausible(lay: &Layout, h: &Hdr) -> bool {
    h.area == lay.sectors
        && h.count <= (lay.limit() as u64 * 2) as u32
        && h.table_sectors == table_sectors(h.count)
        && h.table_first >= 1
        && (h.table_sectors == 0 || (h.table_first as u64 + h.table_sectors as u64) <= lay.limit() as u64)
        && h.used >= 1 + h.table_sectors
        && h.used <= lay.limit()
}

/// What the headers say: the usable one with the higher `seq` (A on a tie) and whether the other copy says the same.
#[derive(Clone, Copy)]
pub struct Table {
    pub h: Hdr,
    /// 0 = copy A, 1 = copy B.
    pub copy: u32,
    /// The other copy is a usable header that says the same (the upload's BEGIN repairs the store when it does not).
    pub agree: bool,
}

unsafe fn header_at(io: &Io, lay: &Layout, rel: u32, scratch: *mut u8) -> Option<Hdr> {
    if read_sectors(io, lay, rel, 1, scratch, 512) != OK {
        return None;
    }
    parse_header(scratch).filter(|h| plausible(lay, h))
}

/// Read both headers (two sectors into `scratch`, 512 bytes, 16-byte aligned). `Err(E_NOT_FOUND)`: no store (neither sector has the magic);
/// `Err(E_BAD_STORE)`: a header is there but neither copy is usable.
pub unsafe fn open_headers(io: &Io, lay: &Layout, scratch: *mut u8) -> Result<(Option<Hdr>, Option<Hdr>), i32> {
    let r = read_sectors(io, lay, 0, 1, scratch, 512);
    if r != OK {
        return Err(r);
    }
    let a_magic = be32(scratch) == MAGIC;
    let a = parse_header(scratch).filter(|h| plausible(lay, h));
    let b = header_at(io, lay, lay.header_b(), scratch);
    if a.is_none() && b.is_none() {
        return Err(if a_magic { E_BAD_STORE } else { E_NOT_FOUND });
    }
    Ok((a, b))
}

/// The copy to use first: `true` = B (usable, with a higher `seq`, or A is not usable).
pub fn prefer_b(a: Option<&Hdr>, b: Option<&Hdr>) -> bool {
    match (a, b) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => b.seq > a.seq,
    }
}

/// CRC of a table, read in pieces through `buf` (`cap` bytes).
pub unsafe fn table_crc(io: &Io, lay: &Layout, first: u32, sectors: u32, buf: *mut u8, cap: usize) -> Result<u32, i32> {
    let per = chunk_sectors(cap);
    let mut c = 0xffff_ffffu32;
    let mut done = 0;
    while done < sectors {
        let n = if sectors - done > per { per } else { sectors - done };
        let r = read_sectors(io, lay, first + done, n, buf, cap);
        if r != OK {
            return Err(r);
        }
        c = crc32(c, buf, n as usize * 512);
        done += n;
    }
    Ok(!c)
}

fn chunk_sectors(cap: usize) -> u32 {
    let n = (cap / 512) as u32;
    if n > MAX_IO_SECTORS {
        MAX_IO_SECTORS
    } else if n == 0 {
        1
    } else {
        n
    }
}

/// Does the table of `h`, or a file in it, reach into the tail (`header_b ..` the end of the area)? A store written before the settings record (tail of 3 sectors)
/// may: its files could lie where the settings copies and header B are now. Such a store is not used (`open` skips it): the runtime would write over a file.
pub unsafe fn reaches_tail(io: &Io, lay: &Layout, h: &Hdr, buf: *mut u8, cap: usize) -> Result<bool, i32> {
    let end = lay.header_b();
    if h.table_sectors != 0 && h.table_first.saturating_add(h.table_sectors) > end {
        return Ok(true);
    }
    let mut hit = false;
    let r = scan(io, lay, h, buf, cap, |_, e| {
        let ent = entry_of(e);
        let len = entry_sectors(&ent);
        if len != 0 && ent.first.saturating_add(len) > end {
            hit = true;
            return false;
        }
        true
    });
    if r != OK {
        return Err(r);
    }
    Ok(hit)
}

/// Open the store: the headers, and (when `verify`) the table's CRC read through `buf` (`cap` bytes) and that nothing reaches into the tail; a copy whose table does not check is
/// skipped for the other one. `scratch` is 512 bytes, 16-byte aligned, and must not overlap `buf`.
pub unsafe fn open(io: &Io, lay: &Layout, scratch: *mut u8, buf: *mut u8, cap: usize, verify: bool) -> Result<Table, i32> {
    let (a, b) = open_headers(io, lay, scratch)?;
    let first_b = prefer_b(a.as_ref(), b.as_ref());
    let mut n = 0;
    while n < 2 {
        let use_b = (n == 0) == first_b;
        let pick = if use_b { b } else { a };
        n += 1;
        let h = match pick {
            Some(h) => h,
            None => continue,
        };
        if verify && h.table_sectors != 0 {
            match table_crc(io, lay, h.table_first, h.table_sectors, buf, cap) {
                Ok(c) if c == h.table_crc => {}
                _ => continue,
            }
        } else if verify && h.table_crc != 0 {
            continue;
        }
        // an older store whose table or files reach into the tail (settings record, header B) is skipped for the other copy, or refused
        if verify && !matches!(reaches_tail(io, lay, &h, buf, cap), Ok(false)) {
            continue;
        }
        let other = if use_b { a } else { b };
        let agree = match other {
            Some(o) => o == h,
            None => false,
        };
        return Ok(Table { h, copy: use_b as u32, agree });
    }
    Err(E_BAD_STORE)
}

/// What the two header sectors say right now, as four words (the header CRC and the sequence number, of A and of B; 0 for a sector that is not a
/// header). Every commit of a store changes at least one of them (the sequence number is under the CRC). `None`: a read failed. `scratch` is 512
/// bytes, 16-byte aligned.
pub unsafe fn fingerprint(io: &Io, lay: &Layout, scratch: *mut u8) -> Option<[u32; 4]> {
    let mut out = [0u32; 4];
    let mut k = 0;
    while k < 2 {
        let rel = if k == 0 { 0 } else { lay.header_b() };
        if read_sectors(io, lay, rel, 1, scratch, 512) != OK {
            return None;
        }
        if be32(scratch) == MAGIC {
            out[2 * k] = be32(scratch.add(508)) ^ be32(scratch.add(20)).rotate_left(7) ^ be32(scratch.add(24)).rotate_left(13);
            out[2 * k + 1] = be32(scratch.add(4)) ^ be32(scratch.add(28)).rotate_left(3);
        }
        k += 1;
    }
    Some(out)
}

/// How many times `stable` reads again before it gives the last answer.
pub const STABLE_TRIES: u32 = 4;

/// Run a read of the store (`f`) that a commit running at the same time cannot tear: the headers' fingerprint is taken before and after, and a
/// changed one means a commit ran in between, so `f` runs again. The upload (the MIDI task) writes a table to free space, then header B, then
/// header A; a reader in another task that read the headers before and the table after could see a table that a later commit freed and reused.
/// No lock: neither task ever waits for the other, and a reader that keeps losing the race (more than `STABLE_TRIES` commits in a row, which one
/// host cannot do) returns what it read last. `scratch` as for `fingerprint`; `f` must not use it.
pub unsafe fn stable<T, F: FnMut() -> Result<T, i32>>(io: &Io, lay: &Layout, scratch: *mut u8, mut f: F) -> Result<T, i32> {
    let mut tries = 0;
    loop {
        let before = fingerprint(io, lay, scratch);
        let r = f();
        if tries >= STABLE_TRIES || before.is_none() || fingerprint(io, lay, scratch) == before {
            return r;
        }
        tries += 1;
    }
}

/// Sectors a new file may take in total: the room for the table of `count + 1` entries is kept back. Not contiguous: `largest_gap` is the most one file
/// can take.
pub const fn free_sectors(lay: &Layout, h: &Hdr) -> u32 {
    let room = table_sectors(h.count + 1);
    lay.limit().saturating_sub(h.used).saturating_sub(room)
}

// ---- entries ---------------------------------------------------------------------------------------------------------------

/// A table entry as a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Relative to the store; 0 for an entry with no data (a folder, an empty file).
    pub first: u32,
    pub size: u32,
    pub kind: u8,
    pub sha: [u8; 32],
}

pub unsafe fn entry_of(e: *const u8) -> Entry {
    let mut sha = [0u8; 32];
    let mut k = 0;
    while k < 32 {
        sha[k] = *e.add(O_SHA + k);
        k += 1;
    }
    Entry { first: be32(e.add(O_FIRST)), size: be32(e.add(O_SIZE)), kind: *e.add(O_KIND), sha }
}

/// Sectors the data of an entry takes.
pub fn entry_sectors(e: &Entry) -> u32 {
    if e.kind == KIND_FILE {
        sectors_of(e.size)
    } else {
        0
    }
}

/// Write an entry (256 bytes) to `out`.
pub unsafe fn encode_entry(path: *const u8, e: &Entry, out: *mut u8) {
    let mut i = 0;
    while i < ENTRY {
        *out.add(i) = 0;
        i += 1;
    }
    i = 0;
    while i < PATH_MAX && *path.add(i) != 0 {
        *out.add(i) = *path.add(i);
        i += 1;
    }
    put32(out.add(O_FIRST), e.first);
    put32(out.add(O_SIZE), e.size);
    let mut k = 0;
    while k < 32 {
        *out.add(O_SHA + k) = e.sha[k];
        k += 1;
    }
    *out.add(O_KIND) = e.kind;
}

/// Length of the path of an entry (NUL-terminated inside the 160 bytes).
pub unsafe fn path_len(e: *const u8) -> usize {
    let mut n = 0;
    while n < PATH_MAX && *e.add(n) != 0 {
        n += 1;
    }
    n
}

/// Call `f(index, entry)` for the entries of the table of `h`, in table order, reading through `buf` (`cap` bytes, 16-byte aligned); stops
/// when `f` returns false. The entry pointer is valid only inside the call.
pub unsafe fn scan<F: FnMut(u32, *const u8) -> bool>(io: &Io, lay: &Layout, h: &Hdr, buf: *mut u8, cap: usize, f: F) -> i32 {
    scan_from(io, lay, h, 0, buf, cap, f)
}

/// `scan` from entry `from` on (the read starts at its sector).
pub unsafe fn scan_from<F: FnMut(u32, *const u8) -> bool>(io: &Io, lay: &Layout, h: &Hdr, from: u32, buf: *mut u8, cap: usize, mut f: F) -> i32 {
    let per = chunk_sectors(cap);
    let mut sector = from / 2;
    let mut idx = from & !1;
    while sector < h.table_sectors {
        let n = if h.table_sectors - sector > per { per } else { h.table_sectors - sector };
        let r = read_sectors(io, lay, h.table_first + sector, n, buf, cap);
        if r != OK {
            return r;
        }
        let mut e = 0u32;
        while e < n * 2 && idx < h.count {
            if idx >= from && !f(idx, buf.add(e as usize * ENTRY)) {
                return OK;
            }
            idx += 1;
            e += 1;
        }
        sector += n;
    }
    OK
}

/// The entry with exactly this path (NUL-terminated).
pub unsafe fn find(io: &Io, lay: &Layout, h: &Hdr, buf: *mut u8, cap: usize, path: *const u8) -> Result<Option<(u32, Entry)>, i32> {
    let mut hit: Option<(u32, Entry)> = None;
    let r = scan(io, lay, h, buf, cap, |i, e| {
        if cstr_eq(e, path) {
            hit = Some((i, entry_of(e)));
            return false;
        }
        true
    });
    if r != OK {
        return Err(r);
    }
    Ok(hit)
}

/// Copy one complete file into caller-owned storage, with fail-closed generation
/// checks around lookup and read. Unlike `stable`, exhaustion never returns an
/// unverified result. `hash` must compute SHA-256 of the supplied retained bytes.
///
/// # Safety
/// `path` is NUL terminated. All buffers are private to this operation, disjoint,
/// and 16-byte aligned: `fp` is 512 bytes; `scratch` has `cap >= 512` bytes;
/// `dst` has `dst_cap` bytes, including sector padding. A failed call does not
/// publish any bytes. The caller must not use `dst` after an error.
#[allow(clippy::too_many_arguments)]
pub unsafe fn snapshot<F: FnMut(&[u8]) -> [u8; 32]>(
    io: &Io,
    lay: &Layout,
    path: *const u8,
    fp: *mut u8,
    scratch: *mut u8,
    cap: usize,
    dst: *mut u8,
    dst_cap: usize,
    max_bytes: u32,
    mut hash: F,
) -> Result<Entry, i32> {
    let mut tries = 0;
    loop {
        let before = fingerprint(io, lay, fp).ok_or(E_READ)?;
        let result = (|| {
            let table = open(io, lay, fp, scratch, cap, true)?;
            let (_, entry) = find(io, lay, &table.h, scratch, cap, path)?.ok_or(E_NOT_FOUND)?;
            if entry.kind != KIND_FILE {
                return Err(E_NOT_FOUND);
            }
            if entry.size == 0 || entry.size > max_bytes || entry.size > u32::MAX - 511 {
                return Err(crate::E_TOO_BIG);
            }
            let sectors = sectors_of(entry.size);
            if sectors as usize > dst_cap / 512 || entry.first == 0 || entry.first >= lay.limit() || sectors > lay.limit() - entry.first {
                return Err(crate::E_TOO_BIG);
            }
            // Bounded reads even when the caller permits a file larger than one
            // firmware block read. Nothing retains a table extent after return.
            let mut done = 0;
            while done < sectors {
                let count = core::cmp::min(sectors - done, MAX_IO_SECTORS);
                let r = read_sectors(io, lay, entry.first + done, count, dst.add(done as usize * 512), dst_cap - done as usize * 512);
                if r != OK {
                    return Err(r);
                }
                done += count;
            }
            if hash(core::slice::from_raw_parts(dst, entry.size as usize)) != entry.sha {
                return Err(crate::E_SHA);
            }
            Ok(entry)
        })();
        let after = fingerprint(io, lay, fp).ok_or(E_READ)?;
        if before == after {
            return result;
        }
        if tries >= STABLE_TRIES {
            return Err(crate::E_BUSY);
        }
        tries += 1;
    }
}

/// Bounded file reads with explicit digest completion.
pub mod stream;

/// Internal bounded metadata enumeration; no public C SDK contract.
pub mod catalog;

/// Pure fixed-memory immediate-child projection of a complete catalogue scan.
pub mod directory;

// ---- free space -------------------------------------------------------------------------------------------------------------

const FIXED_MAX: usize = 6;

/// The extents that are not files: header A, the table, the tail and what the caller says is reserved (a file being uploaded), sorted by start.
struct Fixed {
    v: [(u32, u32); FIXED_MAX],
    n: usize,
}

impl Fixed {
    fn new() -> Fixed {
        Fixed { v: [(0, 0); FIXED_MAX], n: 0 }
    }
    fn push(&mut self, start: u32, len: u32) {
        if len == 0 || self.n >= FIXED_MAX {
            return;
        }
        let mut i = self.n;
        while i > 0 && self.v[i - 1].0 > start {
            self.v[i] = self.v[i - 1];
            i -= 1;
        }
        self.v[i] = (start, len);
        self.n += 1;
    }
}

/// Call `f(start, len)` for every gap between used extents, in order of position, until it returns false. The used extents: header A, the table
/// of `h`, the entries' data, the tail (header B and the selection record), and `extra`.
pub unsafe fn gaps<F: FnMut(u32, u32) -> bool>(
    io: &Io,
    lay: &Layout,
    h: &Hdr,
    buf: *mut u8,
    cap: usize,
    extra: &[(u32, u32)],
    mut f: F,
) -> i32 {
    let mut fx = Fixed::new();
    fx.push(0, 1);
    if h.table_sectors != 0 {
        fx.push(h.table_first, h.table_sectors);
    }
    let mut k = 0;
    while k < extra.len() {
        fx.push(extra[k].0, extra[k].1);
        k += 1;
    }
    fx.push(lay.header_b(), lay.sectors - lay.header_b());
    let mut cursor = 0u32;
    let mut fi = 0usize;
    let mut stop = false;
    // one used extent at a time: a gap is what lies between the cursor and the extent's start
    let mut take = |start: u32, len: u32, cursor: &mut u32, stop: &mut bool, f: &mut F| {
        if start > *cursor && !f(*cursor, start - *cursor) {
            *stop = true;
        }
        let end = start.saturating_add(len);
        if end > *cursor {
            *cursor = end;
        }
    };
    let r = scan(io, lay, h, buf, cap, |_, e| {
        let ent = entry_of(e);
        let len = entry_sectors(&ent);
        if len == 0 {
            return true;
        }
        while fi < fx.n && fx.v[fi].0 <= ent.first {
            take(fx.v[fi].0, fx.v[fi].1, &mut cursor, &mut stop, &mut f);
            fi += 1;
        }
        take(ent.first, len, &mut cursor, &mut stop, &mut f);
        !stop
    });
    if r != OK || stop {
        return r;
    }
    while fi < fx.n && !stop {
        take(fx.v[fi].0, fx.v[fi].1, &mut cursor, &mut stop, &mut f);
        fi += 1;
    }
    OK
}

/// The first gap of at least `need` sectors (first fit).
pub unsafe fn first_fit(io: &Io, lay: &Layout, h: &Hdr, buf: *mut u8, cap: usize, need: u32, extra: &[(u32, u32)]) -> Result<Option<u32>, i32> {
    let mut at: Option<u32> = None;
    let r = gaps(io, lay, h, buf, cap, extra, |s, l| {
        if l >= need {
            at = Some(s);
            return false;
        }
        true
    });
    if r != OK {
        return Err(r);
    }
    Ok(at)
}

/// The largest gap in sectors.
pub unsafe fn largest_gap(io: &Io, lay: &Layout, h: &Hdr, buf: *mut u8, cap: usize, extra: &[(u32, u32)]) -> Result<u32, i32> {
    let mut best = 0u32;
    let r = gaps(io, lay, h, buf, cap, extra, |_, l| {
        if l > best {
            best = l;
        }
        true
    });
    if r != OK {
        return Err(r);
    }
    Ok(best)
}

// ---- the writer -------------------------------------------------------------------------------------------------------------

/// A change of the table.
#[derive(Clone, Copy)]
pub enum Edit {
    /// Add the entry at `*const u8` (256 bytes), or replace the one with its path.
    Insert(*const u8),
    /// Remove the entry with this NUL-terminated path.
    Remove(*const u8),
    /// An empty table.
    Clear,
}

/// What a commit did.
pub struct Committed {
    pub h: Hdr,
    /// The entry the edit replaced or removed.
    pub removed: Option<Entry>,
}

/// Is entry `a` before entry `b` in table order: lower first sector, then path in byte order.
unsafe fn before(a: *const u8, b: *const u8) -> bool {
    let (fa, fb) = (be32(a.add(O_FIRST)), be32(b.add(O_FIRST)));
    if fa != fb {
        return fa < fb;
    }
    cstr_cmp(a, b) < 0
}

/// State of the output stream of a table being written.
struct Out {
    buf: *mut u8,
    cap: usize,
    /// Bytes in `buf`.
    at: usize,
    /// Sectors written so far.
    sectors: u32,
    crc: u32,
    count: u32,
    used_files: u32,
    start: u32,
}

impl Out {
    unsafe fn flush(&mut self, io: &Io, lay: &Layout) -> i32 {
        if self.at == 0 {
            return OK;
        }
        // a final partial sector is padded with zeros
        while self.at % 512 != 0 {
            *self.buf.add(self.at) = 0;
            self.at += 1;
        }
        let n = (self.at / 512) as u32;
        let r = write_sectors(io, lay, self.start + self.sectors, n, self.buf);
        if r != OK {
            return r;
        }
        self.crc = crc32(self.crc, self.buf, self.at);
        self.sectors += n;
        self.at = 0;
        OK
    }

    unsafe fn put(&mut self, io: &Io, lay: &Layout, e: *const u8) -> i32 {
        let mut i = 0;
        while i < ENTRY {
            *self.buf.add(self.at + i) = *e.add(i);
            i += 1;
        }
        self.at += ENTRY;
        self.count += 1;
        let ent = entry_of(e);
        self.used_files += entry_sectors(&ent);
        if self.at + ENTRY > self.cap {
            return self.flush(io, lay);
        }
        OK
    }
}

/// Write `n` sectors at store sector `rel` (data and tables: `[1, limit)`), at most `MAX_IO_SECTORS` at a time.
pub unsafe fn write_sectors(io: &Io, lay: &Layout, rel: u32, n: u32, buf: *const u8) -> i32 {
    if rel < 1 || n == 0 || n > MAX_IO_SECTORS || rel > lay.limit() || n > lay.limit() - rel {
        return E_WRITE;
    }
    if (io.wr)(lay.first + rel, n * 512, buf as *mut u8) != 0 {
        return E_WRITE;
    }
    OK
}

/// Write a header sector to copy A (`b` false) or B.
pub unsafe fn write_header(io: &Io, lay: &Layout, b: bool, buf: *const u8) -> bool {
    let rel = if b { lay.header_b() } else { 0 };
    (io.wr)(lay.first + rel, 512, buf as *mut u8) == 0
}

/// Make the two header copies say the same thing before any space is reused (finding 43, "CLEAR double fault"): a cut between header B and header A
/// leaves A lagging, pointing at the table of the store before. That is harmless while no sector is reused, but the next commit writes a table into
/// free space, which may be exactly the sectors of that old table; if its header B then tears, the lagging A would be the only copy left and would
/// point at overwritten sectors. So the first write of an upload or a commit rewrites the lagging or unusable copy from the winner. `false`: that
/// write failed. A store with nothing committed has nothing to do. `scratch` is 512 bytes, 16-byte aligned.
pub unsafe fn sync_headers(io: &Io, lay: &Layout, t: &Table, scratch: *mut u8) -> bool {
    if t.h.seq == 0 || t.agree {
        return true;
    }
    encode_header(&t.h, scratch);
    write_header(io, lay, t.copy == 0, scratch)
}

/// Commit an edit of the table `cur` (`Hdr::empty` for a store that has none yet): the new table is streamed from the old one, with the edit applied,
/// into the first gap that holds it (not over `extra`, the extents the caller keeps, such as a file being uploaded), read back and checked; then header
/// B and header A are written with `seq + 1`. `buf_in` and `buf_out` are `cap` bytes each (a multiple of 512, at least 1024), 16-byte aligned;
/// `scratch` is 512 bytes, 16-byte aligned. `E_FULL`: no gap for the table. Nothing in use is overwritten before the headers change.
pub unsafe fn commit(
    io: &Io,
    lay: &Layout,
    cur: &Hdr,
    buf_in: *mut u8,
    buf_out: *mut u8,
    cap: usize,
    scratch: *mut u8,
    edit: Edit,
    extra: &[(u32, u32)],
) -> Result<Committed, i32> {
    let need = match edit {
        Edit::Insert(_) => table_sectors(cur.count + 1),
        Edit::Remove(_) => table_sectors(cur.count),
        Edit::Clear => 0,
    };
    let mut start = 1u32;
    if need != 0 {
        start = match first_fit(io, lay, cur, buf_in, cap, need, extra)? {
            Some(s) => s,
            None => return Err(E_FULL),
        };
    }
    let mut out = Out { buf: buf_out, cap, at: 0, sectors: 0, crc: 0xffff_ffff, count: 0, used_files: 0, start };
    let mut removed: Option<Entry> = None;
    let mut inserted = false;
    let mut err = OK;
    if !matches!(edit, Edit::Clear) {
        let r = scan(io, lay, cur, buf_in, cap, |_, e| {
            match edit {
                Edit::Insert(ne) => {
                    // a folder entry is for a folder with nothing in it: it goes when a file or folder is stored inside it
                    if *e.add(O_KIND) == KIND_DIR && relation(e, ne) == 2 {
                        return true;
                    }
                    if cstr_eq(e, ne) {
                        removed = Some(entry_of(e));
                        return true;
                    }
                    if !inserted && before(ne, e) {
                        err = out.put(io, lay, ne);
                        inserted = true;
                        if err != OK {
                            return false;
                        }
                    }
                }
                Edit::Remove(path) => {
                    if cstr_eq(e, path) {
                        removed = Some(entry_of(e));
                        return true;
                    }
                }
                Edit::Clear => {}
            }
            err = out.put(io, lay, e);
            err == OK
        });
        if r != OK {
            return Err(r);
        }
        if err != OK {
            return Err(err);
        }
    }
    if let Edit::Insert(ne) = edit {
        if !inserted {
            let r = out.put(io, lay, ne);
            if r != OK {
                return Err(r);
            }
        }
    }
    let r = out.flush(io, lay);
    if r != OK {
        return Err(r);
    }
    let ts = out.sectors;
    let crc = !out.crc;
    let crc = if ts == 0 { 0 } else { crc };
    if ts != table_sectors(out.count) || ts > need {
        return Err(E_WRITE);
    }
    // read the table back and check it
    if ts != 0 {
        match table_crc(io, lay, start, ts, buf_in, cap) {
            Ok(c) if c == crc => {}
            Ok(_) => return Err(E_WRITE),
            Err(e) => return Err(e),
        }
    }
    let h = Hdr {
        count: out.count,
        table_sectors: ts,
        table_first: if ts == 0 { 1 } else { start },
        table_crc: crc,
        seq: cur.seq + 1,
        used: 1 + ts + out.used_files,
        area: lay.sectors,
    };
    encode_header(&h, scratch);
    if !write_header(io, lay, true, scratch) {
        return Err(E_WRITE);
    }
    if !write_header(io, lay, false, scratch) {
        return Err(E_WRITE);
    }
    Ok(Committed { h, removed })
}

// ---- paths ------------------------------------------------------------------------------------------------------------------

/// Is `p` (NUL-terminated within `max`) a store path: 1 to `PATH_MAX` bytes, no empty, `.` or `..` part, nothing below 0x20 or 0x7f (the store
/// itself allows any byte but NUL; 7-bit text keeps the paths printable on the hosts)? Returns the length.
pub unsafe fn path_ok(p: *const u8, max: usize) -> Option<usize> {
    let mut n = 0;
    while n < max && *p.add(n) != 0 {
        n += 1;
    }
    if n == 0 || n > PATH_MAX || n >= max {
        return None;
    }
    let mut part = 0usize;
    let mut dots = 0usize;
    let mut i = 0;
    while i <= n {
        let c = if i < n { *p.add(i) } else { b'/' };
        if i < n && (c < 0x20 || c == 0x7f) {
            return None;
        }
        if c == b'/' {
            if part == 0 || (dots == part && part <= 2) {
                return None;
            }
            part = 0;
            dots = 0;
        } else {
            part += 1;
            if c == b'.' {
                dots += 1;
            }
        }
        i += 1;
    }
    Some(n)
}

/// How `path` relates to the entry `e`: 0 no relation, 1 the same path, 2 `e` is a folder or file inside `path` (so `path` is a folder),
/// 3 `path` is inside `e` and `e` is a file (so a parent is a file).
pub unsafe fn relation(path: *const u8, e: *const u8) -> u32 {
    let mut i = 0;
    while *path.add(i) != 0 && *e.add(i) != 0 && *path.add(i) == *e.add(i) {
        i += 1;
    }
    let (pc, ec) = (*path.add(i), *e.add(i));
    if pc == 0 && ec == 0 {
        return 1;
    }
    if pc == 0 && ec == b'/' {
        return 2;
    }
    if ec == 0 && pc == b'/' && *e.add(O_KIND) == KIND_FILE {
        return 3;
    }
    0
}

#[cfg(test)]
mod tests;
