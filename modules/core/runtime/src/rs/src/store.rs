//! The runtime's use of the module and sample store: the format, the reader and the writer are `dkrt_sdk::store` (docs/design/digikit-store.md); this
//! file adds the area of this device, the staging buffer, the file's hash check, `Api::store_read` and the list the DIGIKIT menu shows. Reads go
//! through the firmware's block read (16-byte aligned buffers, multiples of 512 bytes, at most 0x10000 per call). The table is never held whole: it is
//! scanned through the staging buffer.

use crate::hash32;
use crate::{DKRT_STAGE, STAGE_BYTES};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::store::{self, Hdr, Io, Layout, Table, KIND_FILE};
use dkrt_sdk::*;

extern "C" {
    /// The store area of this device (`devices/*.toml [card.store]`, `src/cf/<dev>/device.s`): the first card sector and the number of sectors.
    static dkrt_store_first: u32;
    static dkrt_store_sectors: u32;
}

/// Where the store is on this device.
pub fn layout() -> Layout {
    unsafe { Layout { first: read_volatile(addr_of!(dkrt_store_first)), sectors: read_volatile(addr_of!(dkrt_store_sectors)) } }
}

/// 512 bytes, 16-byte aligned: the header sectors (UI task or the mailbox; the upload has its own buffer).
#[repr(C, align(16))]
pub struct PeekBuf([u8; 512]);
#[no_mangle]
pub static mut DKRT_STORE_PEEK: PeekBuf = PeekBuf([0; 512]);
/// The fingerprint reads of a stable list.
#[no_mangle]
pub static mut DKRT_STORE_FP: PeekBuf = PeekBuf([0; 512]);

pub unsafe fn stage() -> *mut u8 {
    addr_of_mut!(DKRT_STAGE) as *mut u8
}

unsafe fn scratch() -> *mut u8 {
    addr_of_mut!(DKRT_STORE_PEEK) as *mut u8
}

unsafe extern "C" fn block_read(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    crate::block_read(sector, bytes, buf)
}

unsafe extern "C" fn no_write(_sector: u32, _bytes: u32, _buf: *mut u8) -> i32 {
    -1
}

/// The card for reads (it cannot write).
pub fn reader() -> Io {
    Io { rd: block_read, wr: no_write }
}

/// Where a file is and what its table entry says its file hash slot is.
pub struct File {
    pub first: u32,
    pub size: u32,
    pub sha: [u8; 32],
}

/// The table the reads last checked: (seq, table CRC, table first sector). A commit of the runtime's own upload sets it; a read of a header that
/// differs checks the table's CRC again (one pass over the table), so a lookup costs the headers and one scan.
static mut CHECKED: Option<(u32, u32, u32)> = None;

/// The upload committed `h`: its table is the one just written and read back.
pub fn note_commit(h: &Hdr) {
    unsafe { write_volatile(addr_of_mut!(CHECKED), Some((h.seq, h.table_crc, h.table_first))) }
}

/// Open the store for a read through the buffers given (`scratch`: 512 bytes, `buf`: `cap` bytes, both 16-byte aligned, apart): the headers, and
/// the table's CRC when the headers are not the ones last checked. `Err(E_NOT_FOUND)` also when the card is smaller than the area (there is no
/// store).
pub unsafe fn open_in(scratch: *mut u8, buf: *mut u8, cap: usize) -> Result<Table, i32> {
    if crate::card::state() == CARD_TOO_SMALL {
        return Err(E_NOT_FOUND);
    }
    let (io, lay) = (reader(), layout());
    let t = store::open(&io, &lay, scratch, buf, cap, false)?;
    let key = (t.h.seq, t.h.table_crc, t.h.table_first);
    if read_volatile(addr_of!(CHECKED)) == Some(key) {
        return Ok(t);
    }
    let t = store::open(&io, &lay, scratch, buf, cap, true)?;
    write_volatile(addr_of_mut!(CHECKED), Some((t.h.seq, t.h.table_crc, t.h.table_first)));
    Ok(t)
}

/// `open_in` with the runtime's own buffers (the staging buffer and the peek sector).
pub unsafe fn open() -> Result<Table, i32> {
    open_in(scratch(), stage(), STAGE_BYTES)
}

/// Look `path` up. Leaves garbage in the staging buffer (the table is scanned through it); the result is by value.
pub unsafe fn find(path: *const u8) -> Result<File, i32> {
    if !crate::store_writer::acquire() {
        return Err(E_BUSY);
    }
    let r = find_unlocked(path);
    crate::store_writer::unlock();
    r
}

/// `find` for a caller that holds the store lock.
unsafe fn find_unlocked(path: *const u8) -> Result<File, i32> {
    let t = open()?;
    let (io, lay) = (reader(), layout());
    match store::find(&io, &lay, &t.h, stage(), STAGE_BYTES, path)? {
        Some((_, e)) if e.kind == KIND_FILE => Ok(File { first: e.first, size: e.size, sha: e.sha }),
        _ => Err(E_NOT_FOUND),
    }
}

/// The first 64 bytes of the file `f` (a native sample header), through the peek sector. Task context, like every store read.
pub unsafe fn read_head(f: &File, head: &mut [u8; 64]) -> Result<(), i32> {
    if f.size == 0 {
        return Err(E_FORMAT);
    }
    let r = read_sector(f.first, scratch());
    if r != OK {
        return Err(r);
    }
    let mut i = 0;
    while i < 64 {
        head[i] = *scratch().add(i);
        i += 1;
    }
    Ok(())
}

/// Read the file `f` into the staging buffer and check the table's file hash. The file is then at `stage()`.
pub unsafe fn load_to_stage(f: &File) -> i32 {
    let r = capacity::stage_fit(f.size, STAGE_BYTES as u32);
    if r != OK {
        return r;
    }
    let sectors = (f.size + 511) / 512;
    let buf = stage();
    // `first` is relative to the start of the store
    if !crate::store_writer::acquire() {
        return E_BUSY;
    }
    let r = store::read_sectors(&reader(), &layout(), f.first, sectors, buf, STAGE_BYTES);
    crate::store_writer::unlock();
    if r != OK {
        return r;
    }
    if hash32::slot_of(buf, f.size) != f.sha {
        return E_SHA;
    }
    OK
}

/// `Api::store_read`: copy a store file to `dst` (at most `cap` bytes); returns its size or a negative code.
pub unsafe fn read_file(path: *const u8, dst: *mut u8, cap: u32) -> i32 {
    let f = match find(path) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let r = capacity::read_fit(f.size, cap);
    if r != OK {
        return r;
    }
    let r = load_to_stage(&f);
    if r != OK {
        return r;
    }
    let src = stage();
    let mut i = 0;
    while i < f.size as usize {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
    f.size as i32
}

/// The store figures as the headers last said them, and whether the headers may have changed since. Only an upload writes a header sector
/// (the card is not swapped while the device runs), so the HEALTH page, which asks twice a second, reads the card once and again after an
/// upload wrote a header (`upload.rs` sets `STORE_DIRTY` when it writes one): a page that read the card on every refresh would cost the UI task
/// card reads that show up in the CPU load it displays.
static mut STORE_DIRTY: bool = true;
static mut STORE_SEEN: Option<(u32, u32)> = None; // (used KiB, free KiB) of a usable header; None: no store

/// An upload wrote (or tried to write) a header sector: the figures are stale. Called from the MIDI task.
pub fn figures_stale() {
    unsafe { core::ptr::write_volatile(addr_of_mut!(STORE_DIRTY), true) }
}

/// Fill the store figures of a `DkrtHealth` from the headers alone (two sectors, no table, and only when an upload changed them), in KiB: the sectors
/// a store may use in all, the sectors used, and what a new file may still take. Sets `HEALTH_STORE` when a usable header was read; a card with no
/// store leaves them 0.
pub unsafe fn figures(o: &mut DkrtHealth) {
    o.store_total = layout().limit() / 2;
    o.store_used = 0;
    o.store_free = 0;
    if let Some((used, free)) = seen() {
        o.store_used = used;
        o.store_free = free;
        o.flags |= HEALTH_STORE;
    }
}

/// (used KiB, free KiB) of the store from the headers (read again only after an upload wrote one), or `None`: no store (a card smaller than the area,
/// or no usable header).
pub unsafe fn seen() -> Option<(u32, u32)> {
    let lay = layout();
    if crate::card::state() == CARD_TOO_SMALL {
        return None;
    }
    if core::ptr::read_volatile(addr_of!(STORE_DIRTY)) {
        // an SDK write or a SysEx command is inside the store: keep the last figures, the flag stays set for the next refresh
        if !crate::store_writer::try_lock() {
            return STORE_SEEN;
        }
        // clear first: a header written while we read leaves the flag set for the next refresh
        core::ptr::write_volatile(addr_of_mut!(STORE_DIRTY), false);
        STORE_SEEN = match store::open_headers(&reader(), &lay, scratch()) {
            Ok((a, b)) => {
                let pick = if store::prefer_b(a.as_ref(), b.as_ref()) { b } else { a };
                pick.map(|h| (h.used / 2, store::free_sectors(&lay, &h) / 2))
            }
            Err(_) => None,
        };
        crate::store_writer::unlock();
    }
    STORE_SEEN
}

/// `dkrt_store_list(prefix, out, max)`: the files whose path starts with `prefix` (NUL-terminated, e.g. `modules/`), in path order, at most `max`
/// into `out`; returns how many, or a negative code (`E_NOT_FOUND`: no store). A path that does not fit 63 bytes, a folder and an empty file are
/// left out. A commit of an upload running at the same time makes the list start again (it is one header's table).
pub unsafe fn list(prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
    if !crate::store_writer::acquire() {
        return E_BUSY;
    }
    let r = list_unlocked(prefix, out, max);
    crate::store_writer::unlock();
    r
}

/// `list` for a caller that holds the store lock.
unsafe fn list_unlocked(prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
    let (io, lay) = (reader(), layout());
    let fp = addr_of_mut!(DKRT_STORE_FP) as *mut u8;
    let mut plen = 0;
    while *prefix.add(plen) != 0 {
        plen += 1;
    }
    let r = store::stable(&io, &lay, fp, || {
        let t = open()?;
        let mut n = 0u32;
        let r = store::scan(&io, &lay, &t.h, stage(), STAGE_BYTES, |_, e| {
            let en = store::entry_of(e);
            let len = store::path_len(e);
            if en.kind != KIND_FILE || en.size == 0 || len >= 64 || len < plen {
                return true;
            }
            let mut i = 0;
            while i < plen {
                if *e.add(i) != *prefix.add(i) {
                    return true;
                }
                i += 1;
            }
            if n >= max {
                return false;
            }
            let o = &mut *out.add(n as usize);
            let mut k = 0;
            while k < 64 {
                o.path[k] = if k < len { *e.add(k) } else { 0 };
                k += 1;
            }
            o.first = en.first;
            o.size = en.size;
            n += 1;
            true
        });
        if r != OK {
            return Err(r);
        }
        Ok(n)
    });
    let n = match r {
        Ok(n) => n,
        Err(e) => return e,
    };
    // path order (insertion sort: the table is in position order)
    let mut i = 1;
    while i < n as usize {
        let mut j = i;
        while j > 0 && store::cstr_cmp((*out.add(j - 1)).path.as_ptr(), (*out.add(j)).path.as_ptr()) > 0 {
            core::ptr::swap(out.add(j - 1), out.add(j));
            j -= 1;
        }
        i += 1;
    }
    n as i32
}

/// `dkrt_store_read_sector(rel, out)`: one sector of the store (relative to its start) into `out`, 512 bytes, 16-byte aligned. 0 or a negative code.
pub unsafe fn read_sector(rel: u32, out: *mut u8) -> i32 {
    if crate::card::state() == CARD_TOO_SMALL {
        return E_NOT_FOUND;
    }
    if !crate::store_writer::acquire() {
        return E_BUSY;
    }
    let r = store::read_sectors(&reader(), &layout(), rel, 1, out, 512);
    crate::store_writer::unlock();
    r
}

/// `dkrt_store_stat(path, out)`: where a store file is, for an adapter that streams it. `E_NOT_FOUND` for no store, no such file and a folder.
/// Leaves garbage in the staging buffer (the table is scanned through it), so only call it at the start of a load.
#[no_mangle]
pub unsafe extern "C" fn dkrt_store_stat(path: *const u8, out: *mut DkrtStoreFile) -> i32 {
    if path.is_null() || out.is_null() {
        return E_CONTRACT;
    }
    crate::init_once();
    match find(path) {
        Ok(f) => {
            let o = &mut *out;
            o.first = f.first;
            o.size = f.size;
            o.sha = f.sha;
            OK
        }
        Err(e) => e,
    }
}

/// `dkrt_store_read(f, sector, count, dst)`: `count` sectors of the file `f` from its sector `sector` into `dst` (16-byte aligned, 512 bytes a
/// sector). No sha check and no staging buffer. Run in the task that owns the card reads (the UI loop).
#[no_mangle]
pub unsafe extern "C" fn dkrt_store_read(f: *const DkrtStoreFile, sector: u32, count: u32, dst: *mut u8) -> i32 {
    if f.is_null() || dst.is_null() || (dst as usize) & 15 != 0 {
        return E_CONTRACT;
    }
    crate::init_once();
    if crate::card::state() == CARD_TOO_SMALL {
        return E_NOT_FOUND;
    }
    let f = &*f;
    let rel = match store::file_span(f.first, f.size, sector, count) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if !crate::store_writer::acquire() {
        return E_BUSY;
    }
    let r = store::read_sectors(&reader(), &layout(), rel, count, dst, count as usize * 512);
    crate::store_writer::unlock();
    r
}
