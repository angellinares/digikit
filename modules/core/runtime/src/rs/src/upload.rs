//! Module upload over USB SysEx (docs/design/digikit-upload.md): the device side.
//!
//! `dkrt_midi_rx` is called from the firmware's MIDI task (hook `cf.midi.site.rx`, `src/cf/tramp.s`) with every SysEx message that
//! came in on USB cable 0, after the firmware gathered it and before its router. A message that starts `F0 7D 64 6B` is ours: it is
//! handled here (one request, one reply, in order) and the router never sees it; any other message returns 0 and takes the stock route.
//!
//! A reply is sent only when the firmware's TX ring has room (`tx_room`), so a host that does not read its input cannot hang the MIDI task.
//!
//! Store writes go through the firmware's block write, only to store sectors: file data and tables in `[1, limit)`, the two header copies at sector
//! 0 and `header_b`. Never the selection record (the two sectors after `header_b`). The table is streamed through two 8 KiB buffers and rewritten whole,
//! in free space, on every commit (`dkrt_sdk::store`, docs/design/digikit-store.md). Rules: no `match` on integers (a dense one becomes a jump
//! table, which is not ColdFire code), no `core::fmt`, no slice indexing (a bounds panic hangs the task).

use crate::hash32::{self, Hash32};
use crate::{block_read, block_write, boot, card, dlog, midi_send, rd32, tx_in_flight, watch, wr32};
use core::ptr::{addr_of, addr_of_mut};
use dkrt_sdk::store::{self, Edit, Entry, Hdr, Io, Layout, Table, KIND_DIR, KIND_FILE};
use dkrt_sdk::proto::*;
use dkrt_sdk::{logk, DkrtCap, CARD_TOO_SMALL, CAP_MAX, E_NOT_FOUND, OK, RUNTIME_VERSION};

// The protocol numbers (commands, the `E_*` statuses, `VERSION`, the INFO capability bits) are `dkrt_sdk::proto`, written by `patchkit gen`
// from `[protocol]` of `sdk/sdk.toml`. E_INUSE: the card's store area is not ours and not blank, the runtime writes nothing there
// (finding 46). E_PEEK: the range is not inside a range the device file lists, or the length is 0 or over 512.

/// Largest DATA chunk (INFO says so) and the unpacked body we accept: offset (4) + chunk.
pub const MAX_CHUNK: u32 = dkrt_sdk::proto::MAX_CHUNK as u32;
/// The unpacked request body starts here in the buffer, so that a DATA chunk (after its 4-byte offset) is 16-byte aligned.
const BODY_AT: usize = 12;
const BODY_MAX: usize = 4 + MAX_CHUNK as usize;
/// The buffer: 32 KiB of body room, then a scratch region (table read-back, header sector).
const SCRATCH_AT: usize = 16 + MAX_CHUNK as usize + 16;
const SCRATCH_BYTES: usize = 4096;
const BUF_BYTES: usize = SCRATCH_AT + SCRATCH_BYTES;

/// The buffer: static (bss of this object, so patchkit reserves it with the code), 16-byte aligned. A static address lets the write-watch
/// of the proofs leave out the card reads that land here by DMA, as it does for `DKRT_STAGE`.
#[repr(C, align(16))]
pub struct UploadBuf([u8; BUF_BYTES]);
#[no_mangle]
pub static mut DKRT_UPLOAD_BUF: UploadBuf = UploadBuf([0; BUF_BYTES]);
/// The table is streamed through two buffers of this size, inside the body region (a command that is not DATA has a small body, copied out first).
const TBL_CAP: usize = 8192;
const IN_AT: usize = 1024;
const OUT_AT: usize = IN_AT + TBL_CAP;
/// Wire bytes of a reply are at most 1 KiB: the unpacked body at most this.
const REPLY_BODY_MAX: usize = 880;
const REQ_WIRE_MAX: u32 = dkrt_sdk::proto::MAX_REQUEST_WIRE as u32;
const CLEAR_MAGIC: u32 = 0x434c_5221;

#[repr(C)]
struct Up {
    open: bool,
    have_last: bool,
    path: [u8; 160],
    size: u32,
    sha: [u8; 32],
    /// The hash of the bytes received so far (DATA feeds it, END compares its slot with `sha`: a large file is not read back from the card).
    run: Hash32,
    /// Next expected byte offset.
    next: u32,
    last_off: u32,
    last_len: u32,
    /// First sector of the file's extent, relative to the store (a gap of the store at BEGIN; 0 for a file with no data).
    start: u32,
}

static mut UP: Up = Up {
    open: false,
    have_last: false,
    path: [0; 160],
    size: 0,
    sha: [0; 32],
    run: Hash32::new(),
    next: 0,
    last_off: 0,
    last_len: 0,
    start: 0,
};
/// A request body that is not DATA or SELECT, copied out of the buffer (a store path is up to 159 bytes: BEGIN's body is at most 196).
static mut REQ: [u8; 256] = [0; 256];
/// Reply under construction: the unpacked body, then the wire message.
static mut RBODY: [u8; REPLY_BODY_MAX] = [0; REPLY_BODY_MAX];
static mut RWIRE: [u8; 1100] = [0; 1100];
/// Test knob (the kept power-cut scenarios poke it): when not 0, the upload path performs that many card writes since boot and then
/// refuses every further one (`E_WRITE`), so a snapshot of the card is the state a power cut after the Nth write leaves.
/// `DKRT_UPLOAD_WRITES` counts the writes done.
#[no_mangle]
pub static mut DKRT_UPLOAD_CUT: u32 = 0;
#[no_mangle]
pub static mut DKRT_UPLOAD_WRITES: u32 = 0;
/// Test knob: when not 0, the Nth card write since boot (counted in `DKRT_UPLOAD_WRITES`) is torn when it is one sector: only its first
/// 256 bytes reach the card (a header then fails its CRC), and every later write is refused as with `DKRT_UPLOAD_CUT`.
#[no_mangle]
pub static mut DKRT_UPLOAD_TEAR: crate::boot::Knob = crate::boot::Knob(0);
#[repr(C, align(16))]
struct Sec([u8; 512]);
static mut TEAR_BUF: Sec = Sec([0; 512]);
/// Test knob: when not 0, the store has this many sectors (from sector 0) for header A, tables and files: the rest up to header B is kept out of every
/// allocation, so a store that is 22 GiB can be filled in a test. Not saved; 0 at every boot.
#[no_mangle]
pub static mut DKRT_STORE_CAP: u32 = 0;
/// Replies dropped because the TX ring had no room (the host retries).
#[no_mangle]
pub static mut DKRT_UPLOAD_DROPPED: u32 = 0;
/// Requests handled and the last status (the proofs peek these).
#[no_mangle]
pub static mut DKRT_UPLOAD_COUNT: u32 = 0;
#[no_mangle]
pub static mut DKRT_UPLOAD_LAST: i32 = 0;

fn up() -> &'static mut Up {
    unsafe { &mut *addr_of_mut!(UP) }
}

// ---- 8-in-7 --------------------------------------------------------------------------------------------------------------

/// Unpack `n` wire bytes into `dst`; returns the unpacked length. Group: a collector byte, then up to 7 bytes; byte j (1..7) takes
/// bit 7 from collector bit (7 - j).
unsafe fn unpack(src: *const u8, n: usize, dst: *mut u8) -> usize {
    let mut i = 0;
    let mut o = 0;
    // whole groups (a collector and seven bytes) with the shifts as constants: about a third of the instructions of the loop below,
    // which every byte of a DATA chunk went through
    while i + 8 <= n {
        let c = *src.add(i) as u32;
        let s = src.add(i + 1);
        let d = dst.add(o);
        *d = (*s & 0x7f) | (((c << 1) & 0x80) as u8);
        *d.add(1) = (*s.add(1) & 0x7f) | (((c << 2) & 0x80) as u8);
        *d.add(2) = (*s.add(2) & 0x7f) | (((c << 3) & 0x80) as u8);
        *d.add(3) = (*s.add(3) & 0x7f) | (((c << 4) & 0x80) as u8);
        *d.add(4) = (*s.add(4) & 0x7f) | (((c << 5) & 0x80) as u8);
        *d.add(5) = (*s.add(5) & 0x7f) | (((c << 6) & 0x80) as u8);
        *d.add(6) = (*s.add(6) & 0x7f) | (((c << 7) & 0x80) as u8);
        i += 8;
        o += 7;
    }
    // the last, short group
    while i < n {
        let c = *src.add(i);
        i += 1;
        let mut j = 1;
        while j <= 7 && i < n {
            let b = *src.add(i) & 0x7f;
            *dst.add(o) = b | (((c >> (7 - j)) & 1) << 7);
            o += 1;
            i += 1;
            j += 1;
        }
    }
    o
}

/// Unpacked length of `n` wire bytes.
fn unpacked_len(n: usize) -> usize {
    n - (n + 7) / 8
}

/// Pack `n` bytes into `dst`; returns the wire length.
unsafe fn pack(src: *const u8, n: usize, dst: *mut u8) -> usize {
    let mut i = 0;
    let mut o = 0;
    while i < n {
        let c_at = o;
        o += 1;
        let mut c = 0u8;
        let mut j = 1;
        while j <= 7 && i < n {
            let b = *src.add(i);
            c |= (b >> 7) << (7 - j);
            *dst.add(o) = b & 0x7f;
            o += 1;
            i += 1;
            j += 1;
        }
        *dst.add(c_at) = c;
    }
    o
}

// ---- replies -------------------------------------------------------------------------------------------------------------

/// The most bytes the firmware may hold unsent in its USB MIDI TX ring (4 KiB) when a reply is sent. The TX function spins, with the MIDI
/// task in it, while the bytes queued plus this word exceed 0xffb, and the USB stack has 31 transfer buffers that a send takes one of (and
/// waits for when none is free): our smallest reply is a 16-byte transfer, so 256 bytes cap the replies waiting at 16 and leave the
/// stock traffic (clock, notes: 4 bytes a transfer) its share. Above this the reply is dropped.
const TX_BUSY_MAX: u32 = 256;
/// The ring's size less its own margin (the function's `0xffb`).
const TX_RING: u32 = 0xff0;

/// Is there room to send a reply of `wire` bytes without the firmware's TX function waiting? It packs 3 bytes of a SysEx message into
/// each 4-byte event, and waits only when the bytes in flight plus what it queued reach the ring size (finding 43).
unsafe fn tx_room(wire: usize) -> bool {
    let need = ((wire as u32 + 2) / 3 + 1) * 4;
    match tx_in_flight() {
        Some(f) => f <= TX_BUSY_MAX && f + need <= TX_RING,
        None => false, // the anchor does not point at a counter: no reply rather than a possible hang
    }
}

/// Send the reply for `cmd`/`seq`: status, then the `extra` bytes already in `RBODY[1..1+extra]`.
unsafe fn reply(cmd: u8, seq: u32, status: i32, extra: usize) -> i32 {
    let rb = addr_of_mut!(RBODY) as *mut u8;
    *rb = status as u8;
    let w = addr_of_mut!(RWIRE) as *mut u8;
    *w = 0xf0;
    *w.add(1) = 0x7d;
    *w.add(2) = 0x64;
    *w.add(3) = 0x6b;
    *w.add(4) = VERSION;
    *w.add(5) = (cmd | 0x40) & 0x7f;
    *w.add(6) = (seq & 0x7f) as u8;
    let n = pack(rb, 1 + extra, w.add(7));
    *w.add(7 + n) = 0xf7;
    DKRT_UPLOAD_LAST = status;
    if logged(cmd, status) {
        dlog::ev(logk::UPLOAD, if status == 0 { dlog::INFO } else { logk::F_ERROR }, [cmd as u32, status as u32, DKRT_UPLOAD_WRITES, 0, 0]);
    }
    if !tx_room(8 + n) {
        DKRT_UPLOAD_DROPPED = DKRT_UPLOAD_DROPPED.wrapping_add(1);
        return 1; // the host reads nothing, or slowly: its retry asks again
    }
    midi_send((8 + n) as u32, w, 2);
    DKRT_UPLOAD_COUNT = DKRT_UPLOAD_COUNT.wrapping_add(1);
    1
}

unsafe fn put32(at: usize, v: u32) {
    wr32((addr_of_mut!(RBODY) as *mut u8).add(at), v);
}

// ---- the store ----------------------------------------------------------------------------------------------------------

fn lay() -> Layout {
    crate::store::layout()
}

unsafe extern "C" fn rd(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    block_read(sector, bytes, buf)
}

/// The card writes of the store code: the one gated write, counted and cut by the test knobs. A header write marks the figures stale.
unsafe extern "C" fn wr(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    let l = lay();
    let header = sector == l.first || sector == l.first + l.header_b();
    let ok = card_write(sector, bytes, buf);
    if header {
        crate::store::figures_stale(); // the HEALTH page's used and free figures come from the headers
    }
    if ok {
        0
    } else {
        -1
    }
}

fn io() -> Io {
    Io { rd, wr }
}

/// A status of `dkrt_sdk::store` as the protocol's: its reads say the load codes.
fn pe(e: i32) -> i32 {
    if e == dkrt_sdk::E_READ {
        E_READ
    } else if e == dkrt_sdk::E_BAD_STORE || e == E_NOT_FOUND {
        E_NOSTORE
    } else {
        e
    }
}

/// Read the current store: the headers (the table is checked once per header: `crate::store::open_in`), through the buffer. A card with nothing in
/// the area is an empty store (`seq` 0). `Err(E_NOSTORE)`: a header is there but no copy is usable, or the card is too small, or the area is somebody's.
unsafe fn state(buf: *mut u8) -> Result<Table, i32> {
    if card::state() == CARD_TOO_SMALL {
        return Err(E_NOSTORE);
    }
    match crate::store::open_in(buf.add(SCRATCH_AT), buf.add(IN_AT), TBL_CAP) {
        Ok(t) => Ok(t),
        Err(e) => {
            if e == E_NOT_FOUND {
                // "No store" is an empty area (or one with only a selection record, which also makes the card ours), or a store of another version.
                // Bytes in a header sector that are not a store header are somebody's data: a write would overwrite them, so refuse (finding 46).
                if !header_sectors_free(buf.add(SCRATCH_AT)) {
                    return Err(E_NOSTORE);
                }
                Ok(Table { h: Hdr::empty(&lay()), copy: 0, agree: true })
            } else {
                Err(E_NOSTORE)
            }
        }
    }
}

/// Does each header sector (A, B) read all 0x00 or all 0xFF, or hold a store header of another version? A sector that cannot be read is not free.
unsafe fn header_sectors_free(scratch: *mut u8) -> bool {
    let l = lay();
    let mut k = 0;
    while k < 2 {
        let rel = if k == 0 { 0 } else { l.header_b() };
        if block_read(l.first + rel, 512, scratch) != 0 || !(card::blank(scratch) || dkrt_sdk::store::old_header(scratch)) {
            return false;
        }
        k += 1;
    }
    true
}

/// The only place the upload path writes the card.
unsafe fn card_write(sector: u32, bytes: u32, buf: *const u8) -> bool {
    let cut = core::ptr::read_volatile(addr_of!(DKRT_UPLOAD_CUT));
    if cut != 0 && DKRT_UPLOAD_WRITES >= cut {
        return false;
    }
    DKRT_UPLOAD_WRITES = DKRT_UPLOAD_WRITES.wrapping_add(1);
    let tear = core::ptr::read_volatile(addr_of!(DKRT_UPLOAD_TEAR.0));
    if tear != 0 && DKRT_UPLOAD_WRITES == tear && bytes == 512 {
        let t = addr_of_mut!(TEAR_BUF) as *mut u8;
        copy(t, buf, 256);
        zero(t.add(256), 256);
        block_write(sector, 512, t);
        core::ptr::write_volatile(addr_of_mut!(DKRT_UPLOAD_CUT), DKRT_UPLOAD_WRITES);
        return false;
    }
    block_write(sector, bytes, buf) == 0
}

fn sectors_of(bytes: u32) -> u32 {
    store::sectors_of(bytes)
}

/// Sectors a store may use now: up to header B, or the test knob.
fn limit_now() -> u32 {
    let l = lay();
    let cap = unsafe { core::ptr::read_volatile(addr_of!(DKRT_STORE_CAP)) };
    if cap != 0 && cap < l.limit() {
        cap
    } else {
        l.limit()
    }
}

/// The extents an allocation keeps out of: the sectors the test knob takes away, and `pend` (a file being uploaded). At most two.
fn extras(buf: &mut [(u32, u32); 2], pend: Option<(u32, u32)>) -> &[(u32, u32)] {
    let l = lay();
    let mut n = 0;
    let lim = limit_now();
    if lim < l.limit() {
        buf[n] = (lim, l.limit() - lim);
        n += 1;
    }
    if let Some(p) = pend {
        buf[n] = p;
        n += 1;
    }
    &buf[..n]
}

/// Bytes of free space in all (the room for the table's next entry kept back), saturated to 32 bits for INFO.
fn free_bytes(h: &Hdr) -> u32 {
    let room = store::table_sectors(h.count + 1);
    let b = limit_now().saturating_sub(h.used).saturating_sub(room) as u64 * 512;
    if b > u32::MAX as u64 {
        u32::MAX
    } else {
        b as u32
    }
}

unsafe fn copy(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
}

unsafe fn zero(dst: *mut u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = 0;
        i += 1;
    }
}

/// Make the two header copies say the same thing before any space is reused (`store::sync_headers`). `false`: that write failed.
unsafe fn sync_headers(buf: *mut u8, t: &Table) -> bool {
    store::sync_headers(&io(), &lay(), t, buf.add(SCRATCH_AT))
}

/// Commit an edit of the table `cur`; the runtime's store cache learns the new table. A status.
unsafe fn do_commit(buf: *mut u8, cur: &Hdr, edit: Edit, pend: Option<(u32, u32)>) -> i32 {
    let mut ex = [(0, 0); 2];
    let extra = extras(&mut ex, pend);
    match store::commit(&io(), &lay(), cur, buf.add(IN_AT), buf.add(OUT_AT), TBL_CAP, buf.add(SCRATCH_AT), edit, extra) {
        Ok(c) => {
            crate::store::note_commit(&c.h);
            OK
        }
        Err(e) => pe(e),
    }
}

// ---- commands -------------------------------------------------------------------------------------------------------------

unsafe fn cmd_info(seq: u32, buf: *mut u8) -> i32 {
    let st = state(buf);
    let free = match &st {
        Ok(s) => free_bytes(&s.h),
        Err(_) => 0,
    };
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = 0;
    *rb.add(1) = VERSION; // protocol 1
    *rb.add(2) = (RUNTIME_VERSION >> 8) as u8;
    *rb.add(3) = RUNTIME_VERSION as u8;
    wr32(rb.add(4), crate::device_name()); // the device's short name from its device file
    put32(1 + 8, free);
    put32(1 + 12, MAX_CHUNK);
    let flags = (boot::info().safe & 1)
        | if up().open { 2 } else { 0 }
        | if card::writable() { 0 } else { 4 }
        | if dlog::sink_present() { 8 } else { 0 }
        | if crate::crash::seen().found != 0 { 16 } else { 0 };
    *rb.add(16) = flags as u8;
    // the log range (bytes 17 to 24) stays zero: the log is the debug module's, whose LOG command has the range
    wr32(rb.add(17), 0);
    wr32(rb.add(21), 0);
    // appended after the log range: the whole packed runtime version (bytes 2-3 keep its low 16 bits) and the capability bits
    wr32(rb.add(25), RUNTIME_VERSION);
    let log_caps = if dlog::sink_present() { CAP_LOG | CAP_PEEK | CAP_LOGCFG } else { 0 };
    wr32(rb.add(29), log_caps | CAP_SELECT | CAP_RESTART | CAP_MODULES | CAP_CAPACITY | CAP_READ | CAP_VERIFY | CAP_SERVICE_CMDS | CAP_FOLDERS | CAP_SETTINGS | CAP_FILEHASH);
    // appended after the capability bits: the id of the flashed image (patchkit stamps it; zeros when there is none)
    let id = crate::image_id();
    let mut i = 0;
    while i < 8 {
        *rb.add(33 + i) = id[i];
        i += 1;
    }
    reply(CMD_INFO, seq, OK, 41)
}

/// MODULES: the loaded modules and what they hold. Body: `u8 start` (a module record index). Reply: `u32 arena_free, arena_largest, dsp_free,
/// dsp_largest`, `u8 total` (loaded modules), `u8 n` (records in this reply), then per record `u8 index`, `name[32]`, `u32 version`, 8 contract bytes
/// (api, machine, fx, service as major, minor), `u32 cf_base, cf_size, dsp_off, dsp_len, rows` (voice rows as a mask), `u8 n_types` and that many machine
/// type bytes (at most 8: the machine rows of the module, then its voice rows' types), `u32 faults` (the guard's count for those types), `u8 flags` (bit 0:
/// the guard switched one of them off). A reply holds the records from `start` on that fit; the host asks again from the last index + 1 until it has `total`.
unsafe fn cmd_modules(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen < 1 {
        return reply(CMD_MODULES, seq, E_MSG, 0);
    }
    let start = *body as usize;
    let s = crate::st();
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    let (af, al) = crate::arena::stats();
    let (df, dl) = crate::dsp::area_stats();
    wr32(rb, af);
    wr32(rb.add(4), al);
    wr32(rb.add(8), df);
    wr32(rb.add(12), dl);
    let mut total = 0u8;
    let mut i = 0;
    while i < crate::MAX_MODULES {
        if s.modules[i].used != 0 {
            total += 1;
        }
        i += 1;
    }
    *rb.add(16) = total;
    let mut at = 18usize;
    let mut n = 0u8;
    let d = crate::dsp::ds();
    i = start;
    while i < crate::MAX_MODULES {
        let m = &s.modules[i];
        if m.used == 0 {
            i += 1;
            continue;
        }
        // the machine types: rows of the registry, then voice rows (pinned numbers of a DSP-only module)
        let mut types = [0u8; 8];
        let mut nt = crate::registry::types_of(i as u8 + 1, &mut types);
        let mut r = 0;
        while r < crate::dsp::MAX_ROWS {
            if d.row_owner[r] == i as u8 + 1 && d.row_type[r] < 256 && nt < 8 {
                let t = d.row_type[r] as u8;
                let mut dup = false;
                let mut j = 0;
                while j < nt {
                    dup |= types[j] == t;
                    j += 1;
                }
                if !dup {
                    types[nt] = t;
                    nt += 1;
                }
            }
            r += 1;
        }
        if at + 71 + nt > REPLY_BODY_MAX - 1 {
            break;
        }
        *rb.add(at) = i as u8;
        copy(rb.add(at + 1), m.name.as_ptr(), 32);
        wr32(rb.add(at + 33), m.version);
        copy(rb.add(at + 37), (*addr_of!(crate::DKRT_CONTRACTS))[i].as_ptr(), 8);
        wr32(rb.add(at + 45), m.base);
        wr32(rb.add(at + 49), m.size);
        wr32(rb.add(at + 53), d.mod_off[i]);
        wr32(rb.add(at + 57), d.mod_len[i]);
        wr32(rb.add(at + 61), d.mod_rows[i]);
        *rb.add(at + 65) = nt as u8;
        let mut faults = 0u32;
        let mut off = false;
        let mut j = 0;
        while j < nt {
            *rb.add(at + 66 + j) = types[j];
            faults = faults.wrapping_add(watch::guard_faults(types[j] as u32));
            off |= watch::guard_disabled(types[j] as u32);
            j += 1;
        }
        wr32(rb.add(at + 66 + nt), faults);
        *rb.add(at + 70 + nt) = off as u8;
        at += 71 + nt;
        n += 1;
        i += 1;
    }
    *rb.add(17) = n;
    reply(CMD_MODULES, seq, OK, at)
}

/// CAPACITY: per resource of the runtime, its limit, what is used and the largest free piece. Body: none. Reply: `u8 version` (1), `u8 n`, then
/// `n` entries of `u8 kind` (`CAPK_*`, sdk/sdk.toml `[capacity.kinds]`), `u32 limit`, `u32 used`, `u32 largest` (0 where a largest piece has
/// no meaning). A host skips a kind it does not know; new kinds are appended.
unsafe fn cmd_capacity(seq: u32) -> i32 {
    let mut entries = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let n = crate::dkrt_capacity(entries.as_mut_ptr(), CAP_MAX as u32) as usize;
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = 1;
    *rb.add(1) = n as u8;
    let mut at = 2usize;
    let mut k = 0;
    while k < n {
        let e = &entries[k];
        *rb.add(at) = e.kind as u8;
        wr32(rb.add(at + 1), e.limit);
        wr32(rb.add(at + 5), e.used);
        wr32(rb.add(at + 9), e.largest);
        at += 13;
        k += 1;
    }
    reply(CMD_CAPACITY, seq, OK, at)
}

/// The store paths a request carries: 1 to 159 bytes, NUL-terminated inside the body.
unsafe fn path_ok(p: *const u8, max: usize) -> Option<usize> {
    store::path_ok(p, max)
}

/// LIST: body `u32 first`, then optionally `prefix NUL` (1 to 159 bytes, e.g. `samples/`). Without a prefix: `first` is an entry index, `count` all
/// entries. With one: only the entries whose path starts with it (files, and folders with nothing in them), `first` indexes those and `count` is how many
/// there are. Reply: `u32 count`, `u8 n`, then per entry `path NUL, u32 size, hash[32]`, as many as fit, in table order (by first sector: a host sorts
/// for display). A folder with no file in it has size 0 and a zero hash slot.
unsafe fn cmd_list(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    if blen < 4 || (blen > 4 && (blen < 6 || blen > 4 + 160 || *body.add(blen - 1) != 0)) {
        return reply(CMD_LIST, seq, E_MSG, 0);
    }
    let first = rd32(body);
    let prefix = if blen > 4 { body.add(4) } else { core::ptr::null() };
    let mut plen = 0usize;
    if blen > 4 {
        plen = blen - 5;
        let mut i = 0;
        while i < plen {
            if *prefix.add(i) == 0 {
                return reply(CMD_LIST, seq, E_MSG, 0);
            }
            i += 1;
        }
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_LIST, seq, e, 0),
    };
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    let mut at = 5usize;
    let mut n = 0u32;
    let mut seen = 0u32; // entries matching so far (prefix lists)
    let mut full = false;
    let from = if plen == 0 { first } else { 0 };
    let r = store::scan_from(&io(), &lay(), &t.h, from, buf.add(IN_AT), TBL_CAP, |_, e| {
        if plen != 0 {
            let mut i = 0;
            while i < plen {
                if *e.add(i) != *prefix.add(i) {
                    return true;
                }
                i += 1;
            }
            seen += 1;
            if seen <= first || full {
                return true;
            }
        }
        let pl = store::path_len(e);
        let need = pl + 1 + 4 + 32;
        if at + need > REPLY_BODY_MAX - 1 {
            full = true;
            return plen != 0; // a prefix list goes on to count the rest
        }
        let en = store::entry_of(e);
        copy(rb.add(at), e, pl);
        *rb.add(at + pl) = 0;
        wr32(rb.add(at + pl + 1), en.size);
        copy(rb.add(at + pl + 5), en.sha.as_ptr(), 32);
        at += need;
        n += 1;
        true
    });
    if r != OK {
        return reply(CMD_LIST, seq, pe(r), 0);
    }
    wr32(rb, if plen == 0 { t.h.count } else { seen });
    *rb.add(4) = n as u8;
    reply(CMD_LIST, seq, OK, at)
}

/// MKDIR: make the folder `path NUL` (a folder entry with nothing in it). Ok when it already exists (a folder entry, or implied by files inside it);
/// `E_PATH` when the path is a file or lies inside one. A folder entry is dropped when a file is stored inside the folder. An upload in progress is aborted.
unsafe fn cmd_mkdir(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    up().open = false;
    if !card::writable() {
        return reply(CMD_MKDIR, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    match path_ok(body, blen) {
        Some(n) if n + 1 == blen => {}
        _ => return reply(CMD_MKDIR, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_MKDIR, seq, e, 0),
    };
    let mut exists = false;
    let mut conflict = false;
    let r = store::scan(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, |_, e| {
        let rel = store::relation(body, e);
        if rel == 3 || (rel == 1 && store::entry_of(e).kind == KIND_FILE) {
            conflict = true;
        } else if rel == 1 || rel == 2 {
            exists = true;
        }
        !conflict
    });
    if r != OK {
        return reply(CMD_MKDIR, seq, pe(r), 0);
    }
    if conflict {
        return reply(CMD_MKDIR, seq, E_PATH, 0);
    }
    if exists {
        return reply(CMD_MKDIR, seq, OK, 0);
    }
    if !sync_headers(buf, &t) {
        return reply(CMD_MKDIR, seq, E_WRITE, 0);
    }
    let mut ent = [0u8; store::ENTRY];
    let e = Entry { first: 0, size: 0, kind: KIND_DIR, sha: [0; 32] };
    store::encode_entry(body, &e, ent.as_mut_ptr());
    let r = do_commit(buf, &t.h, Edit::Insert(ent.as_ptr()), None);
    reply(CMD_MKDIR, seq, r, 0)
}

unsafe fn cmd_begin(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    up().open = false;
    if !card::writable() {
        return reply(CMD_BEGIN, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    let pl = match path_ok(body, blen) {
        Some(n) => n,
        None => return reply(CMD_BEGIN, seq, if blen < 1 + 4 + 32 { E_MSG } else { E_PATH }, 0),
    };
    if blen != pl + 1 + 4 + 32 {
        return reply(CMD_BEGIN, seq, E_MSG, 0);
    }
    let size = rd32(body.add(pl + 1));
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_BEGIN, seq, e, 0),
    };
    if !sync_headers(buf, &t) {
        return reply(CMD_BEGIN, seq, E_WRITE, 0);
    }
    let (io, lay) = (io(), lay());
    // the path may replace a file, but not a folder, and may not lie inside a file or over a folder that has entries
    let mut conflict = false;
    let r = store::scan(&io, &lay, &t.h, buf.add(IN_AT), TBL_CAP, |_, e| {
        let rel = store::relation(body, e);
        if rel >= 2 || (rel == 1 && store::entry_of(e).kind == KIND_DIR) {
            conflict = true;
        }
        !conflict
    });
    if r != OK {
        return reply(CMD_BEGIN, seq, pe(r), 0);
    }
    if conflict {
        return reply(CMD_BEGIN, seq, E_PATH, 0);
    }
    // the file's extent: the first gap that holds it, and room beside it for the table of one more entry
    let need = sectors_of(size);
    let mut ex0 = [(0, 0); 2];
    let start = if need == 0 {
        0
    } else {
        match store::first_fit(&io, &lay, &t.h, buf.add(IN_AT), TBL_CAP, need, extras(&mut ex0, None)) {
            Ok(Some(s)) => s,
            Ok(None) => {
                put32(1, free_bytes(&t.h));
                return reply(CMD_BEGIN, seq, E_FULL, 4);
            }
            Err(e) => return reply(CMD_BEGIN, seq, pe(e), 0),
        }
    };
    let pend = if need == 0 { None } else { Some((start, need)) };
    let mut ex1 = [(0, 0); 2];
    match store::first_fit(&io, &lay, &t.h, buf.add(IN_AT), TBL_CAP, store::table_sectors(t.h.count + 1), extras(&mut ex1, pend)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            put32(1, free_bytes(&t.h));
            return reply(CMD_BEGIN, seq, E_FULL, 4);
        }
        Err(e) => return reply(CMD_BEGIN, seq, pe(e), 0),
    }
    let u = up();
    zero(u.path.as_mut_ptr(), 160);
    copy(u.path.as_mut_ptr(), body, pl);
    u.size = size;
    copy(u.sha.as_mut_ptr(), body.add(pl + 5), 32);
    u.run = Hash32::new();
    u.next = 0;
    u.have_last = false;
    u.last_off = 0;
    u.last_len = 0;
    u.start = start;
    u.open = true;
    reply(CMD_BEGIN, seq, OK, 0)
}

unsafe fn cmd_data(seq: u32, buf: *mut u8, blen: usize) -> i32 {
    let u = up();
    if !u.open {
        return reply(CMD_DATA, seq, E_STATE, 0);
    }
    if blen < 5 {
        return reply(CMD_DATA, seq, E_MSG, 0);
    }
    let body = buf.add(BODY_AT);
    let off = rd32(body);
    let len = (blen - 4) as u32;
    if len > MAX_CHUNK {
        return reply(CMD_DATA, seq, E_LENGTH, 0);
    }
    if u.have_last && off == u.last_off && len == u.last_len {
        return reply(CMD_DATA, seq, OK, 0); // a retry of the previous chunk
    }
    if off != u.next {
        return reply(CMD_DATA, seq, E_OFFSET, 0);
    }
    let end = off as u64 + len as u64;
    if end > u.size as u64 || (len & 511 != 0 && end != u.size as u64) {
        return reply(CMD_DATA, seq, E_LENGTH, 0);
    }
    let data = buf.add(BODY_AT + 4);
    let sectors = sectors_of(len);
    zero(data.add(len as usize), (sectors * 512 - len) as usize);
    if store::write_sectors(&io(), &lay(), u.start + off / 512, sectors, data) != OK {
        return reply(CMD_DATA, seq, E_WRITE, 0);
    }
    u.have_last = true;
    u.last_off = off;
    u.last_len = len;
    u.next = end as u32;
    u.run.update(data, len);
    reply(CMD_DATA, seq, OK, 0)
}

/// The hash slot of `size` bytes of the card from store sector `first`, read in chunks through `buf`.
unsafe fn sha_of(buf: *mut u8, first: u32, size: u32, out: *mut u8) -> bool {
    let l = lay();
    let mut h = Hash32::new();
    let mut left = size;
    let mut sector = first;
    while left > 0 {
        let n = if left > MAX_CHUNK { MAX_CHUNK } else { left };
        let secs = sectors_of(n);
        if block_read(l.first + sector, secs * 512, buf.add(16)) != 0 {
            return false;
        }
        h.update(buf.add(16), n);
        left -= n;
        sector += secs;
    }
    core::ptr::copy_nonoverlapping(hash32::slot(h.finish()).as_ptr(), out, 32);
    true
}

unsafe fn cmd_end(seq: u32, buf: *mut u8) -> i32 {
    let u = up();
    if !u.open {
        return reply(CMD_END, seq, E_STATE, 0);
    }
    // END closes the upload whatever the result
    u.open = false;
    if u.next != u.size {
        return reply(CMD_END, seq, E_LENGTH, 0);
    }
    // the hash of the bytes received (DATA fed it as they came: END does not read a large file back from the card; VERIFY does)
    let got = hash32::slot(core::mem::replace(&mut u.run, Hash32::new()).finish());
    let mut i = 0;
    while i < 32 {
        if got[i] != u.sha[i] {
            return reply(CMD_END, seq, E_HASH, 0);
        }
        i += 1;
    }
    // the new table: the current one with the entry replaced or inserted
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_END, seq, e, 0),
    };
    let mut ent = [0u8; store::ENTRY];
    let e = Entry { first: u.start, size: u.size, kind: KIND_FILE, sha: u.sha };
    store::encode_entry(u.path.as_ptr(), &e, ent.as_mut_ptr());
    let pend = if u.size == 0 { None } else { Some((u.start, sectors_of(u.size))) };
    let r = do_commit(buf, &t.h, Edit::Insert(ent.as_ptr()), pend);
    reply(CMD_END, seq, r, 0)
}

/// DELETE: a path of the store (a file or a folder entry). The space is free again when the commit is done. An upload in progress is aborted.
unsafe fn cmd_delete(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    up().open = false;
    if !card::writable() {
        return reply(CMD_DELETE, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    match path_ok(body, blen) {
        Some(n) if n + 1 == blen => {}
        _ => return reply(CMD_DELETE, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_DELETE, seq, e, 0),
    };
    if !sync_headers(buf, &t) {
        return reply(CMD_DELETE, seq, E_WRITE, 0);
    }
    match store::find(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, body) {
        Ok(Some(_)) => {}
        Ok(None) => return reply(CMD_DELETE, seq, E_NOFILE, 0),
        Err(e) => return reply(CMD_DELETE, seq, pe(e), 0),
    }
    let r = do_commit(buf, &t.h, Edit::Remove(body), None);
    reply(CMD_DELETE, seq, r, 0)
}

/// Most bytes one READ returns: the reply body is a status, the u32 file size and the bytes, in at most `REPLY_BODY_MAX`.
const READ_MAX: u32 = dkrt_sdk::proto::READ_MAX as u32;

/// The file `path` (NUL-terminated) of the store, or the status to reply.
unsafe fn file_of(buf: *mut u8, path: *const u8) -> Result<Entry, i32> {
    let t = state(buf)?;
    match store::find(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, path) {
        Ok(Some((_, e))) if e.kind == KIND_FILE => Ok(e),
        Ok(_) => Err(E_NOFILE),
        Err(e) => Err(pe(e)),
    }
}

/// READ: path, NUL, u32 offset, u32 length. Reply: the file size (u32), then `min(length, 768, size - offset)` bytes.
/// E_PATH / E_MSG: bad request; E_NOFILE: not in the store; E_LENGTH: length 0; E_OFFSET: offset at or past the end of the file.
unsafe fn cmd_read(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    let pl = match path_ok(body, blen) {
        Some(n) if n + 1 + 8 == blen => n,
        _ => return reply(CMD_READ, seq, if blen < 10 { E_MSG } else { E_PATH }, 0),
    };
    let off = rd32(body.add(pl + 1));
    let mut len = rd32(body.add(pl + 5));
    if len == 0 {
        return reply(CMD_READ, seq, E_LENGTH, 0);
    }
    let f = match file_of(buf, body) {
        Ok(f) => f,
        Err(e) => return reply(CMD_READ, seq, e, 0),
    };
    if off >= f.size {
        return reply(CMD_READ, seq, E_OFFSET, 0);
    }
    if len > READ_MAX {
        len = READ_MAX;
    }
    if len > f.size - off {
        len = f.size - off;
    }
    // the sectors that hold the range, into the scratch region (3 sectors at most)
    let scratch = buf.add(SCRATCH_AT);
    let skip = off % 512;
    let secs = (skip + len + 511) / 512;
    if block_read(lay().first + f.first + off / 512, secs * 512, scratch) != 0 {
        return reply(CMD_READ, seq, E_READ, 0);
    }
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    wr32(rb, f.size);
    copy(rb.add(4), scratch.add(skip as usize), len as usize);
    reply(CMD_READ, seq, OK, 4 + len as usize)
}

/// SETTINGS (read only): the settings record as the runtime holds it (docs/design/digikit-store.md, "The settings record"). Body: `u32 offset`, `u16 length`
/// (1 to 768). Reply: `u8 flags` (bit 0: the record has changes that the card does not hold yet), `u8 copy` (the card's current copy: 0 A, 1 B, 2 none),
/// `u32 seq` (the card's), `u32 used` (the record's bytes), then `min(length, used - offset)` bytes of the record from `offset`. A host reads in pieces
/// and decodes the whole (the format is `dkrt_sdk::settings`). E_SET_NOKEY: no record on the card and none made; E_OFFSET: offset past the record.
unsafe fn cmd_settings(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen != 6 {
        return reply(CMD_SETTINGS, seq, E_MSG, 0);
    }
    let off = rd32(body) as usize;
    let mut len = (((*body.add(4) as u32) << 8) | *body.add(5) as u32) as usize;
    if len == 0 {
        return reply(CMD_SETTINGS, seq, E_LENGTH, 0);
    }
    if crate::settings::is_off() {
        return reply(CMD_SETTINGS, seq, E_SET_OFF, 0);
    }
    let (flags, cur, rseq, rec) = match crate::settings::snapshot() {
        Some(r) => r,
        None => return reply(CMD_SETTINGS, seq, E_SET_NOKEY, 0),
    };
    if off >= rec.len() {
        return reply(CMD_SETTINGS, seq, E_OFFSET, 0);
    }
    if len > READ_MAX as usize {
        len = READ_MAX as usize;
    }
    if len > rec.len() - off {
        len = rec.len() - off;
    }
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = flags;
    *rb.add(1) = cur as u8;
    wr32(rb.add(2), rseq);
    wr32(rb.add(6), rec.len() as u32);
    copy(rb.add(10), rec.as_ptr().add(off), len);
    reply(CMD_SETTINGS, seq, OK, 10 + len)
}

/// VERIFY: `path NUL`, optionally then `u32 offset, u32 length`. Reads the file (or that range of it) back from the card, in chunks, and replies its
/// hash slot (32 bytes): what a host compares with its own hash to know that the bytes on the card are the bytes it sent. The cost is about 7 ColdFire
/// instructions a byte, and a large file is still checked in ranges (the offset is a multiple of 512). Errors: E_NOFILE, E_READ, E_OFFSET (not a multiple of
/// 512, or at or past the end), E_LENGTH (0, or past the end of the file), E_MSG, E_PATH.
unsafe fn cmd_verify(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    let pl = match path_ok(body, blen) {
        Some(n) if n + 1 == blen || n + 1 + 8 == blen => n,
        _ => return reply(CMD_VERIFY, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    };
    let f = match file_of(buf, body) {
        Ok(f) => f,
        Err(e) => return reply(CMD_VERIFY, seq, e, 0),
    };
    let (mut off, mut len) = (0u32, f.size);
    if blen > pl + 1 {
        off = rd32(body.add(pl + 1));
        len = rd32(body.add(pl + 5));
        if off & 511 != 0 || off >= f.size {
            return reply(CMD_VERIFY, seq, E_OFFSET, 0);
        }
        if len == 0 || len as u64 > (f.size - off) as u64 {
            return reply(CMD_VERIFY, seq, E_LENGTH, 0);
        }
    }
    if !sha_of(buf, f.first + off / 512, len, (addr_of_mut!(RBODY) as *mut u8).add(1)) {
        return reply(CMD_VERIFY, seq, E_READ, 0);
    }
    reply(CMD_VERIFY, seq, OK, 32)
}

unsafe fn cmd_clear(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    up().open = false;
    if !card::writable() {
        return reply(CMD_CLEAR, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    if blen != 4 || rd32(body) != CLEAR_MAGIC {
        return reply(CMD_CLEAR, seq, E_MSG, 0);
    }
    // CLEAR works on a damaged store too: the new sequence number is above every copy that is readable
    let seq_now = match state(buf) {
        Ok(t) => t.h.seq,
        Err(_) => highest_seq(buf),
    };
    let cur = Hdr { seq: seq_now, ..Hdr::empty(&lay()) };
    let r = do_commit(buf, &cur, Edit::Clear, None);
    reply(CMD_CLEAR, seq, r, 0)
}

/// The higher `seq` of the two header copies that parse (0 when none).
unsafe fn highest_seq(buf: *mut u8) -> u32 {
    let l = lay();
    let mut best = 0;
    let mut k = 0;
    while k < 2 {
        let rel = if k == 0 { 0 } else { l.header_b() };
        if block_read(l.first + rel, 512, buf.add(SCRATCH_AT)) == 0 {
            if let Some(h) = store::parse_header(buf.add(SCRATCH_AT)) {
                if h.seq > best {
                    best = h.seq;
                }
            }
        }
        k += 1;
    }
    best
}

/// FAULT (dev): body `u8 what`; 1 makes the runtime panic with a failed bounds check, to test the fault path. It does not reply: the device resets.
/// Other values: `E_PROTO`.
unsafe fn cmd_fault(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen != 1 {
        return reply(CMD_FAULT, seq, E_MSG, 0);
    }
    if *body != 1 {
        return reply(CMD_FAULT, seq, E_PROTO, 0);
    }
    let a = [1u8, 2, 3, 4];
    let i = core::hint::black_box(a.len() + 3);
    reply(CMD_FAULT, seq, OK, a[i] as usize)
}

/// SELECT (runtime 4): save the selection record, the same `boot::selection_set` the menu's APPLY & RESTART calls (so the card range check and
/// the "blank or ours" rule apply). Body: `u8 n`, then `n` store paths, each NUL terminated. Status: 0, `E_MSG` (malformed), `E_INUSE` (card
/// not writable), or the selection code (`E_SEL_COUNT`, `E_SEL_PATH`, `E_SEL_WRITE`, `E_CARD_RO`). Takes effect at the next boot.
unsafe fn cmd_select(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen < 1 {
        return reply(CMD_SELECT, seq, E_MSG, 0);
    }
    if !card::writable() {
        return reply(CMD_SELECT, seq, E_INUSE, 0);
    }
    let n = *body as usize;
    if n > dkrt_sdk::SEL_MAX {
        return reply(CMD_SELECT, seq, dkrt_sdk::E_SEL_COUNT, 0);
    }
    // static, not on the MIDI task's stack (the record is about 450 bytes)
    let sel = &mut *addr_of_mut!(SELBUF);
    *sel = core::mem::zeroed();
    let mut at = 1usize;
    let mut k = 0usize;
    while k < n {
        let mut len = 0usize;
        while at + len < blen && *body.add(at + len) != 0 {
            len += 1;
        }
        if at + len >= blen {
            return reply(CMD_SELECT, seq, E_MSG, 0); // no NUL
        }
        if len >= dkrt_sdk::SEL_PATH {
            return reply(CMD_SELECT, seq, dkrt_sdk::E_SEL_PATH, 0);
        }
        let mut i = 0;
        while i < len {
            sel.paths[k][i] = *body.add(at + i);
            i += 1;
        }
        at += len + 1;
        k += 1;
    }
    if at != blen {
        return reply(CMD_SELECT, seq, E_MSG, 0);
    }
    sel.count = n as u32;
    let r = boot::selection_set(sel);
    reply(CMD_SELECT, seq, r, 0)
}

static mut SELBUF: dkrt_sdk::DkrtSelection = dkrt_sdk::DkrtSelection { count: 0, flags: 0, paths: [[0; dkrt_sdk::SEL_PATH]; dkrt_sdk::SEL_MAX] };

/// Ticks to wait for the reply to leave the TX ring, at most, before the restart goes anyway.
const RESTART_TICKS_MAX: u32 = 4000;
/// 0: no restart pending; else 1 + the UI passes waited so far.
static mut RESTART_WAIT: u32 = 0;

/// RESTART (runtime 4): the reply goes out first; the soft reset (`dkrt_restart`, the one APPLY & RESTART calls) runs from the UI task's next
/// passes (`restart_poll`), once the TX ring has drained. Status: 0, or `E_STATE` while an upload is open. No body.
unsafe fn cmd_restart(seq: u32) -> i32 {
    if up().open {
        return reply(CMD_RESTART, seq, E_STATE, 0);
    }
    RESTART_WAIT = 1;
    reply(CMD_RESTART, seq, OK, 0)
}

/// Called once per UI pass (`dkrt_tick`). Runs a pending RESTART after its reply has been sent. `dkrt_restart` returns only when it refuses
/// (a card transfer in flight): the request is then dropped and logged as an error; the host sees the runtime still running and asks again.
pub unsafe fn restart_poll() {
    if RESTART_WAIT == 0 {
        return;
    }
    RESTART_WAIT += 1;
    let drained = matches!(tx_in_flight(), Some(0));
    if (drained && RESTART_WAIT > 3) || RESTART_WAIT > RESTART_TICKS_MAX {
        RESTART_WAIT = 0;
        let r = crate::dkrt_restart();
        dlog::ev(logk::UPLOAD, logk::F_ERROR, [CMD_RESTART as u32, r as u32, 0, 0, 0]);
    }
}

unsafe fn buffer() -> *mut u8 {
    addr_of_mut!(DKRT_UPLOAD_BUF) as *mut u8
}

/// The hook: `msg` is one gathered SysEx message, F0 to F7 inclusive. Returns 1 when it was ours (the router must not see it).
#[no_mangle]
pub unsafe extern "C" fn dkrt_midi_rx(msg: *const u8, len: u32, _source: u32) -> i32 {
    if len < 5 || *msg != 0xf0 || *msg.add(1) != 0x7d || *msg.add(2) != 0x64 || *msg.add(3) != 0x6b {
        return 0;
    }
    crate::init_once();
    if len < 8 || *msg.add(len as usize - 1) != 0xf7 {
        return reply(0, 0, E_MSG, 0);
    }
    let ver = *msg.add(4);
    let cmd = *msg.add(5);
    let seq = *msg.add(6) as u32;
    if ver != VERSION {
        return reply(cmd, seq, E_PROTO, 0);
    }
    let wire = len - 8;
    if wire > REQ_WIRE_MAX || unpacked_len(wire as usize) > BODY_MAX {
        return reply(cmd, seq, E_MSG, 0);
    }
    let buf = buffer();
    let blen = unpack(msg.add(7), wire as usize, buf.add(BODY_AT));
    let ent = find(cmd);
    let big = ent.is_some_and(|e| e.flags & BIG != 0);
    // Every command but DATA and SELECT has a small body. Copy it out: the commands stream the table through the buffer.
    // (DATA and SELECT read theirs in place: SELECT's is up to 8 paths of 56 bytes and it never touches the table; DATA's is a chunk.)
    let req = &mut *addr_of_mut!(REQ);
    if !big {
        if blen > req.len() {
            return reply(cmd, seq, E_MSG, 0);
        }
        copy(req.as_mut_ptr(), buf.add(BODY_AT), blen);
    }
    let body = if big { buf.add(BODY_AT) as *const u8 } else { req.as_ptr() };
    if let Some(e) = ent {
        return (e.h)(seq, buf, body, blen);
    }
    // a command a module registered
    match service(cmd) {
        Some(h) => {
            let out = (addr_of_mut!(RBODY) as *mut u8).add(1);
            // what the handler logs carries its module's source (owner 255: registered by no module)
            let owner = service_of(cmd).map_or(255, |s| s.owner);
            let prev = if owner != 255 && owner > 0 { dlog::enter_module(owner as usize - 1) } else { dlog::current_source() };
            let r = h(cmd as u32, seq, body, blen as u32, out);
            if owner != 255 && owner > 0 {
                dlog::leave_module(prev);
            }
            if r < 0 {
                reply(cmd, seq, r, 0)
            } else if r as usize > REPLY_BODY_MAX - 1 {
                reply(cmd, seq, E_LENGTH, 0)
            } else {
                reply(cmd, seq, OK, r as usize)
            }
        }
        None => reply(cmd, seq, E_PROTO, 0),
    }
}

// ---- the command table ---------------------------------------------------------------------------------------------------

/// A built-in command: its handler takes the request number, the buffer (table scratch), the body and its length, and sends the reply.
type Handler = unsafe fn(u32, *mut u8, *const u8, usize) -> i32;

/// The reply of this command is not logged as an UPLOAD event (it is read-only and frequent).
const NOLOG: u8 = 1;
/// The body is read in place (`buf + BODY_AT`, up to a chunk), not copied into the small request buffer.
const BIG: u8 = 2;

struct Cmd {
    cmd: u8,
    flags: u8,
    h: Handler,
}

unsafe fn h_info(seq: u32, buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_info(seq, buf)
}
unsafe fn h_list(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_list(seq, buf, body, blen)
}
unsafe fn h_modules(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_modules(seq, body, blen)
}
unsafe fn h_capacity(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_capacity(seq)
}
unsafe fn h_begin(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_begin(seq, buf, body, blen)
}
unsafe fn h_data(seq: u32, buf: *mut u8, _body: *const u8, blen: usize) -> i32 {
    cmd_data(seq, buf, blen)
}
unsafe fn h_end(seq: u32, buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_end(seq, buf)
}
unsafe fn h_mkdir(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_mkdir(seq, buf, body, blen)
}
unsafe fn h_delete(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_delete(seq, buf, body, blen)
}
unsafe fn h_read(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_read(seq, buf, body, blen)
}
unsafe fn h_settings(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_settings(seq, body, blen)
}
unsafe fn h_verify(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_verify(seq, buf, body, blen)
}
unsafe fn h_clear(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_clear(seq, buf, body, blen)
}
unsafe fn h_abort(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    up().open = false;
    reply(CMD_ABORT, seq, OK, 0)
}
unsafe fn h_fault(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_fault(seq, body, blen)
}
unsafe fn h_select(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_select(seq, body, blen)
}
unsafe fn h_restart(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_restart(seq)
}

/// The built-in commands, looked up one after the other (a jump table is not ColdFire code).
static CMDS: [Cmd; 17] = [
    Cmd { cmd: CMD_INFO, flags: NOLOG, h: h_info },
    Cmd { cmd: CMD_LIST, flags: 0, h: h_list },
    Cmd { cmd: CMD_MODULES, flags: NOLOG, h: h_modules },
    Cmd { cmd: CMD_CAPACITY, flags: NOLOG, h: h_capacity },
    Cmd { cmd: CMD_BEGIN, flags: 0, h: h_begin },
    Cmd { cmd: CMD_DATA, flags: BIG, h: h_data },
    Cmd { cmd: CMD_END, flags: 0, h: h_end },
    Cmd { cmd: CMD_DELETE, flags: 0, h: h_delete },
    Cmd { cmd: CMD_MKDIR, flags: 0, h: h_mkdir },
    Cmd { cmd: CMD_READ, flags: NOLOG, h: h_read },
    Cmd { cmd: CMD_VERIFY, flags: NOLOG, h: h_verify },
    Cmd { cmd: CMD_SETTINGS, flags: NOLOG, h: h_settings },
    Cmd { cmd: CMD_CLEAR, flags: 0, h: h_clear },
    Cmd { cmd: CMD_ABORT, flags: 0, h: h_abort },
    Cmd { cmd: CMD_FAULT, flags: 0, h: h_fault },
    Cmd { cmd: CMD_SELECT, flags: BIG, h: h_select },
    Cmd { cmd: CMD_RESTART, flags: 0, h: h_restart },
];

fn find(cmd: u8) -> Option<&'static Cmd> {
    let mut i = 0;
    while i < CMDS.len() {
        if CMDS[i].cmd == cmd {
            return Some(&CMDS[i]);
        }
        i += 1;
    }
    None
}

/// Is the reply to `cmd` with `status` logged as an UPLOAD event? Not a read-only built-in (flag), and a service command only when it failed
/// (a key press must not fill the ring).
fn logged(cmd: u8, status: i32) -> bool {
    match find(cmd) {
        Some(e) => e.flags & NOLOG == 0,
        None => status != 0 || service(cmd).is_none(),
    }
}

// ---- commands that modules register (Api::register_cmds) ------------------------------------------------------------------

/// Registered ranges at most (a module takes one per command group: digikit/remote 2, digikit/probe 1).
const SVC_MAX: usize = 8;

#[derive(Clone, Copy)]
struct Svc {
    first: u8,
    last: u8,
    /// `dlog::current_source()` of the registering module (record index + 1); 0: the slot is free.
    owner: u8,
    h: Option<dkrt_sdk::ServiceCmd>,
}

static mut SVC: [Svc; SVC_MAX] = [Svc { first: 0, last: 0, owner: 0, h: None }; SVC_MAX];

fn svc() -> &'static mut [Svc; SVC_MAX] {
    unsafe { &mut *addr_of_mut!(SVC) }
}

/// The handler of a registered command.
fn service(cmd: u8) -> Option<dkrt_sdk::ServiceCmd> {
    service_of(cmd).and_then(|s| s.h)
}

fn service_of(cmd: u8) -> Option<&'static Svc> {
    let t = svc();
    let mut i = 0;
    while i < SVC_MAX {
        if t[i].owner != 0 && cmd >= t[i].first && cmd <= t[i].last {
            return Some(&t[i]);
        }
        i += 1;
    }
    None
}

/// `Api::register_cmds`: `E_CMD_RANGE` when the range is empty, outside every reserved range, over a registered one, or there is no free
/// slot; else 0.
pub unsafe fn register_cmds(first: u32, count: u32, handler: Option<dkrt_sdk::ServiceCmd>) -> i32 {
    if handler.is_none() || count == 0 || first == 0 || first > 0xff || count > 0xff || first + count - 1 > 0xff {
        return E_CMD_RANGE as i32;
    }
    let (lo, hi) = (first as u8, (first + count - 1) as u8);
    let mut reserved = false;
    let mut i = 0;
    while i < RANGES.len() {
        if lo >= RANGES[i].0 && hi <= RANGES[i].1 {
            reserved = true;
        }
        i += 1;
    }
    if !reserved {
        return E_CMD_RANGE as i32;
    }
    let t = svc();
    let mut free = SVC_MAX;
    i = 0;
    while i < SVC_MAX {
        if t[i].owner == 0 {
            free = i;
        } else if lo <= t[i].last && t[i].first <= hi {
            return E_CMD_RANGE as i32;
        }
        i += 1;
    }
    if free == SVC_MAX {
        return E_CMD_RANGE as i32;
    }
    // a registration outside a module's init (owner 0) is kept under owner 255: nothing releases it
    let owner = dlog::current_source();
    t[free] = Svc { first: lo, last: hi, owner: if owner == 0 { 255 } else { owner }, h: handler };
    OK
}

/// The module whose source index is `owner` was taken down: release its ranges.
pub unsafe fn release_cmds(owner: u8) {
    let t = svc();
    let mut i = 0;
    while i < SVC_MAX {
        if t[i].owner == owner {
            t[i] = Svc { first: 0, last: 0, owner: 0, h: None };
        }
        i += 1;
    }
}

#[allow(dead_code)]
fn _keep(_: *const u8) {
    let _ = addr_of!(UP);
}
