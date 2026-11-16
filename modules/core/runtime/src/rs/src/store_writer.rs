//! The store's write steps (docs/plans/usb-fast.md 3.1): begin, write, close, abort and delete of a store file, the one write session, the store
//! lock and the card write with the power-cut knobs. Two front ends use it: the SysEx handlers of `transport.rs` and `store_cmds.rs` (the MIDI task, through the upload
//! buffer) and the SDK `file_*` calls (the I/O task, through a static buffer of their own).
//!
//! Store writes go through the firmware's block write, only to store sectors: file data and tables in `[1, limit)`, the two header copies at sector
//! 0 and `header_b`. Never the selection record. The table is streamed through two 8 KiB buffers and rewritten whole, in free space, on every
//! commit (`dkrt_sdk::store`, docs/design/digikit-store.md).
//!
//! One write session in all: a SysEx upload or an SDK file. A SysEx BEGIN replaces an open SysEx session (as ever); anything that would take the
//! place of an SDK session gets `E_BUSY`. One store lock (`try_lock`, taken without blocking under the irq mask) is held for the length of every
//! store operation of the MIDI task and of every SDK call, so a commit never overlaps a LIST, a VERIFY or another write.
//!
//! Rules: no `match` on integers (a dense one becomes a jump table, which is not ColdFire code), no `core::fmt`, no slice indexing.

use crate::hash32::{self, Hash32};
use crate::{block_read, block_write, card, PLATFORM};
use dkrt_abi::platform::{Platform, StagedWriter};
#[cfg(not(test))]
use dkrt_abi::platform::Tasking;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::proto::*;
use dkrt_sdk::store::lease::{self, Leases};
use dkrt_sdk::store::{self, Edit, Entry, Hdr, Io, Layout, Table, KIND_DIR, KIND_FILE};
use dkrt_sdk::{CARD_TOO_SMALL, E_BUSY, E_CONTEXT, E_CONTRACT, E_EXISTS, E_NOT_FOUND, OK};

/// The table is streamed through two buffers of this size.
pub const TBL_CAP: usize = 8192;
/// Offsets in the SysEx upload buffer (`transport::DKRT_UPLOAD_BUF`): the two table buffers, inside the body region (a command that is not DATA
/// has a small body, copied out first), and the scratch region after the 32 KiB of body room (table read-back, header sector).
pub const IN_AT: usize = 1024;
pub const OUT_AT: usize = IN_AT + TBL_CAP;
pub const SCRATCH_AT: usize = 16 + MAX_CHUNK + 16;

/// Largest chunk of one write (the protocol's DATA chunk).
const MAX_CHUNK: usize = dkrt_sdk::proto::MAX_CHUNK as usize;

/// Largest piece of one SDK write: `store::MAX_WRITE_SECTORS` sectors, the largest card command of core/sdhc-large (the firmware's block write
/// splits a piece into 64 KiB commands itself when the module is not in the image). The SDK path writes from the caller's buffer, so this needs no
/// scratch (the SysEx path stays at `MAX_CHUNK`, the size of its body region).
const WRITE_PIECE: usize = store::MAX_WRITE_SECTORS as usize * 512;

/// The three buffers of a store operation: the table's input and output streams (`TBL_CAP` bytes each) and a scratch (at least 512 bytes);
/// all 16-byte aligned and apart.
#[derive(Clone, Copy)]
pub struct Bufs {
    pub tin: *mut u8,
    pub tout: *mut u8,
    pub scr: *mut u8,
    /// The close's card read-back buffer (16-byte aligned, a multiple of 512 bytes): it overlaps the table buffers, which are free until the commit.
    pub rb: *mut u8,
    pub rb_cap: usize,
}

impl Bufs {
    /// The regions of the SysEx upload buffer `buf`.
    pub unsafe fn sysex(buf: *mut u8) -> Bufs {
        Bufs { tin: buf.add(IN_AT), tout: buf.add(OUT_AT), scr: buf.add(SCRATCH_AT), rb: buf.add(16), rb_cap: MAX_CHUNK }
    }
}

/// The SDK path's own buffers (the MIDI task uses the upload buffer at the same time). Static, so that the write-watch of the proofs leaves out the
/// card reads that land here by DMA (tests/regression/runtime/write-safety.ts).
const SDK_SCRATCH: usize = 1024;
#[repr(C, align(16))]
pub struct SdkBuf {
    tin: [u8; TBL_CAP],
    tout: [u8; TBL_CAP],
    scr: [u8; SDK_SCRATCH],
}
#[no_mangle]
pub static mut DKRT_SDK_BUF: SdkBuf = SdkBuf { tin: [0; TBL_CAP], tout: [0; TBL_CAP], scr: [0; SDK_SCRATCH] };

pub(crate) unsafe fn sdk_bufs() -> Bufs {
    let b = addr_of_mut!(DKRT_SDK_BUF);
    let tin = addr_of_mut!((*b).tin) as *mut u8;
    // `tin` and `tout` are adjacent (repr(C), 16-byte aligned): one 16 KiB read-back buffer
    Bufs { tin, tout: addr_of_mut!((*b).tout) as *mut u8, scr: addr_of_mut!((*b).scr) as *mut u8, rb: tin, rb_cap: 2 * TBL_CAP }
}

// ---- the lock -----------------------------------------------------------------------------------------------------------------

static mut STORE_LOCK: bool = false;

/// Take the store lock without blocking. False: another task is inside a store operation.
pub fn try_lock() -> bool {
    let sr = crate::irq_mask();
    let free = unsafe { !read_volatile(addr_of!(STORE_LOCK)) };
    if free {
        unsafe { write_volatile(addr_of_mut!(STORE_LOCK), true) };
    }
    crate::irq_restore(sr);
    free
}

/// How long a reader in the UI or boot task waits for the lock before it gives up (1 ms steps).
const WAIT_STEPS: u32 = 3000;

#[cfg(not(test))]
unsafe fn pause() {
    PLATFORM.sleep_us(1000);
}
#[cfg(test)]
unsafe fn pause() {}

/// Take the store lock for a reader in a task that may sleep (the UI or boot task, the I/O task); in the MIDI task a single try: `try_lock`, and while it is held (an SDK write or commit on the I/O task, a SysEx command
/// of the MIDI task) sleep 1 ms with the firmware's `sleep_us`, up to `WAIT_STEPS` times. False after that: the caller fails with `E_BUSY`
/// (the failure is logged); a reader never reads the table without the lock. Call only where `sleep_us` may block (a task, not an interrupt),
/// and do not wait on anything else while holding the lock.
pub unsafe fn acquire() -> bool {
    let mut n = 0;
    // the MIDI task must never sleep (it is the firmware's MIDI input and the host's replies): one try, then the caller says E_BUSY
    let waits = !on_midi_task();
    loop {
        if try_lock() {
            return true;
        }
        if !waits || n >= WAIT_STEPS {
            crate::dlog::ev(dkrt_sdk::logk::UPLOAD, dkrt_sdk::logk::F_ERROR, [0xb5, E_BUSY as u32, 0, 0, 0]);
            return false;
        }
        pause();
        n += 1;
    }
}

/// The MIDI task's TCB, noted when `dkrt_midi_rx` first runs (0 before). `acquire` does not sleep in that task, whatever code (a service command
/// handler, a store read called by one) is running in it.
static mut MIDI_TASK: u32 = 0;

/// `dkrt_midi_rx` calls it on entry.
pub unsafe fn note_midi_task() {
    if read_volatile(addr_of!(MIDI_TASK)) == 0 {
        write_volatile(addr_of_mut!(MIDI_TASK), crate::io::current_task());
    }
}

unsafe fn on_midi_task() -> bool {
    let m = read_volatile(addr_of!(MIDI_TASK));
    m != 0 && crate::io::current_task() == m
}

/// Is the store lock held now (an SDK call or a SysEx command is inside the store)?
pub fn is_locked() -> bool {
    unsafe { read_volatile(addr_of!(STORE_LOCK)) }
}

pub fn unlock() {
    let sr = crate::irq_mask();
    unsafe { write_volatile(addr_of_mut!(STORE_LOCK), false) };
    crate::irq_restore(sr);
}

// ---- the session --------------------------------------------------------------------------------------------------------------

const NONE: u8 = 0;
pub const SYSEX: u8 = 1;
pub const SDK: u8 = 2;

#[repr(C)]
struct Up {
    /// `NONE`, `SYSEX` or `SDK`: who holds the one write session.
    kind: u8,
    have_last: bool,
    /// The SDK session's owner (source index of the module whose I/O step created it) and handle.
    owner: u8,
    have_want: bool,
    handle: i32,
    /// The I/O job that opened the SDK session (`io::step_job`): the session ends with it.
    job: u32,
    /// The block clock (1500 a second) at the last BEGIN or write: an idle session expires (`tick`).
    last: u32,
    path: [u8; 160],
    size: u32,
    /// The expected hash slot (BEGIN's, or `hash4` of `file_create`); valid when `have_want`.
    sha: [u8; 32],
    /// The hash of the bytes received so far (not fed when `skip_rx`).
    run: Hash32,
    /// Lever A (`speed::HASH_ONCE`, decided at BEGIN): the received bytes are not hashed; the close's card read-back is the only hash and is compared with
    /// the expected one (`have_want`).
    skip_rx: bool,
    /// The session writes the +Drive through `core/plusdrive-write` (DT2 samples root): no store lock, no table, the adapter holds the file.
    pd: bool,
    /// Next expected byte offset.
    next: u32,
    last_off: u32,
    last_len: u32,
    /// First sector of the file's extent, relative to the store (a gap of the store at BEGIN; 0 for a file with no data).
    start: u32,
    /// What BEGIN found for the commit (`Hint`).
    hint: Hint,
    /// The file is closed as a pending file (`DK_FILE_DEFER`), not named in the table at once.
    defer: bool,
}

/// The table's place that BEGIN found for the commit of the session. It is used only when the commit sees the same header (every commit changes
/// `seq`, so any other writer in between makes it differ) and the same kept-out extents (a lease, the capacity knob or the file's extent that
/// changed): then the table, the extents and the size of the table are those BEGIN looked at, so the gap BEGIN found is what a new search gives.
#[derive(Clone, Copy)]
struct Hint {
    valid: bool,
    h: Hdr,
    extra: [(u32, u32); EXTRA_MAX],
    n: usize,
    at: u32,
}

const NO_HINT: Hint = Hint {
    valid: false,
    h: Hdr { count: 0, table_sectors: 0, table_first: 0, table_crc: 0, seq: 0, used: 0, area: 0 },
    extra: [(0, 0); EXTRA_MAX],
    n: 0,
    at: 0,
};

static mut UP: Up = Up {
    kind: NONE,
    have_last: false,
    owner: 0,
    have_want: false,
    handle: 0,
    job: 0,
    last: 0,
    path: [0; 160],
    size: 0,
    sha: [0; 32],
    run: Hash32::new(),
    skip_rx: false,
    pd: false,
    next: 0,
    last_off: 0,
    last_len: 0,
    start: 0,
    hint: NO_HINT,
    defer: false,
};
static mut NEXT_HANDLE: i32 = 0;

fn up() -> &'static mut Up {
    unsafe { &mut *addr_of_mut!(UP) }
}

/// Is a write session open (SysEx or SDK)?
pub fn is_open() -> bool {
    up().kind != NONE
}

/// Is a SysEx upload open?
pub fn sysex_open() -> bool {
    up().kind == SYSEX
}

/// A SysEx command that changes the table takes the place of an open SysEx session (it is aborted). `false`: an SDK session is open and stays.
pub fn preempt() -> bool {
    let u = up();
    if u.kind == SDK {
        return false;
    }
    u.kind = NONE;
    true
}

/// The I/O job `id` has ended (finished, failed, cancelled or dropped): a session it left open is aborted, so that no write stays blocked.
pub fn end_job(id: u32) {
    let u = up();
    if u.kind == SDK && id != 0 && u.job == id {
        drop_sdk(u);
    }
    let p = pf();
    if id != 0 && p.n != 0 && p.job == id {
        p.flush = true; // the job that closed the pending files is over: `pend_tick` commits them
    }
}

/// Test knob: blocks (1500 a second) of idleness after which a write session is aborted; 0: the default, 20 s (the stock firmware's writer timeout).
#[no_mangle]
pub static mut DKRT_SESSION_IDLE: u32 = 0;
const IDLE_BLOCKS: u32 = 20 * 1500;

#[cfg(not(test))]
unsafe fn now() -> u32 {
    crate::fw_blocks().unwrap_or(0)
}
#[cfg(test)]
pub static mut TEST_NOW: u32 = 0;
#[cfg(test)]
unsafe fn now() -> u32 {
    TEST_NOW
}

/// UI task, once per pass: a write session (SysEx or SDK) with no BEGIN or write for 20 s is dropped (its extent was never in the table) and logged.
/// Skipped while the store lock is held (a write is in progress) and when the block clock is not there.
pub unsafe fn tick() {
    pd_tick();
    pend_tick();
    let u = up();
    if u.kind == NONE {
        return;
    }
    #[cfg(not(test))]
    if crate::fw_blocks().is_none() {
        return;
    }
    if u.pd && pd_busy() {
        return; // an adapter call is running in the I/O task
    }
    let knob = read_volatile(addr_of!(DKRT_SESSION_IDLE));
    let limit = if knob != 0 { knob } else { IDLE_BLOCKS };
    if now().wrapping_sub(u.last) < limit || !try_lock() {
        return;
    }
    if u.kind != NONE && now().wrapping_sub(u.last) >= limit {
        crate::dlog::ev(dkrt_sdk::logk::UPLOAD, dkrt_sdk::logk::F_ERROR, [0xb6, u.kind as u32, u.owner as u32, u.next, u.size]);
        drop_sdk(u);
    }
    unlock();
}

/// The SysEx ABORT: drops a SysEx session; an SDK session is not its to drop.
pub fn abort_sysex() {
    let u = up();
    if u.kind == SYSEX {
        u.kind = NONE;
    }
}

/// The module whose source index is `owner` is going away: drop its SDK session.
pub fn retire(owner: u8) {
    let u = up();
    if u.kind == SDK && u.owner == owner {
        drop_sdk(u);
    }
    let p = pf();
    let mut i = 0;
    while i < p.n {
        if p.owner[i] == owner {
            p.flush = true; // the module goes: its pending files are verified data and are committed, not dropped
        }
        i += 1;
    }
    lease_drop_owner(owner);
}

// ---- read leases --------------------------------------------------------------------------------------------------------------

/// The leases (`dkrt_sdk::store::lease`): extents of store files that a reader still reads. A lease is taken under the store lock (`lease_take`), so
/// the extent is the committed one and no commit runs at the same time. The table changes with the interrupts masked: a release comes from the
/// reader's own task and never waits for the lock. Reads of the table (the refusals, the allocator's extents) hold the store lock.
static mut LEASES: Leases = Leases::new();

fn leases() -> &'static mut Leases {
    unsafe { &mut *addr_of_mut!(LEASES) }
}

/// Lease the data extent (`first`, `size` bytes) of a store file for `owner` (0: an adapter). The caller holds the store lock. The id (> 0), or
/// `E_NOMEM` when the table (`lease::MAX`) is full: the caller fails its load; it must not wait with the lock held.
pub fn lease_take(owner: u8, first: u32, size: u32) -> i32 {
    let sr = crate::irq_mask();
    let id = leases().take(owner, first, sectors_of(size));
    crate::irq_restore(sr);
    id
}

/// Release lease `id` (any task, no lock): 0, or `E_NOT_FOUND`.
pub fn lease_end(id: i32) -> i32 {
    let sr = crate::irq_mask();
    let r = leases().end(id);
    crate::irq_restore(sr);
    r
}

fn lease_drop_owner(owner: u8) {
    let sr = crate::irq_mask();
    leases().drop_owner(owner);
    crate::irq_restore(sr);
}

/// Is any lease live? (CLEAR refuses then.)
pub fn leased_any() -> bool {
    leases().any()
}

/// Does a lease hold the data of the file entry `e`? Replacing or deleting it is refused then (`E_FILE_IN_USE`).
fn leased(e: &Entry) -> bool {
    leases().guards(e.first, store::entry_sectors(e))
}

// ---- test knobs ---------------------------------------------------------------------------------------------------------------

/// Test knob (the kept power-cut scenarios poke it): when not 0, the write path performs that many card writes since boot and then
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
/// Test knob: when not 0, the Nth card write since boot (counted in `DKRT_UPLOAD_WRITES`) succeeds, then the first byte of its first sector on the card is
/// flipped: a card that holds other bytes than the host sent (the close's read-back must refuse it).
#[no_mangle]
pub static mut DKRT_UPLOAD_FLIP: crate::boot::Knob = crate::boot::Knob(0);
#[repr(C, align(16))]
struct Sec([u8; 512]);
static mut TEAR_BUF: Sec = Sec([0; 512]);
/// Test knob: when not 0, the store has this many sectors (from sector 0) for header A, tables and files: the rest up to header B is kept out of every
/// allocation, so a store that is 22 GiB can be filled in a test. Not saved; 0 at every boot.
#[no_mangle]
pub static mut DKRT_STORE_CAP: u32 = 0;

// ---- the store ----------------------------------------------------------------------------------------------------------------

pub fn lay() -> Layout {
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

pub fn io() -> Io {
    Io { rd, wr }
}

/// A status of `dkrt_sdk::store` as the protocol's: its reads say the load codes.
pub fn pe(e: i32) -> i32 {
    if e == dkrt_sdk::E_READ {
        E_READ
    } else if e == dkrt_sdk::E_BAD_STORE || e == E_NOT_FOUND {
        E_NOSTORE
    } else {
        e
    }
}

/// Read the current store: the headers (the table is checked once per header: `crate::store::open_in`), through `b`. A card with nothing in
/// the area is an empty store (`seq` 0). `Err(E_NOSTORE)`: a header is there but no copy is usable, or the card is too small, or the area is somebody's.
pub unsafe fn state(b: &Bufs) -> Result<Table, i32> {
    if card::state() == CARD_TOO_SMALL {
        return Err(E_NOSTORE);
    }
    match crate::store::open_in(b.scr, b.tin, TBL_CAP) {
        Ok(t) => Ok(t),
        Err(e) => {
            if e == E_NOT_FOUND {
                // "No store" is an empty area (or one with only a selection record, which also makes the card ours), or a store of another version.
                // Bytes in a header sector that are not a store header are somebody's data: a write would overwrite them, so refuse (finding 46).
                if !header_sectors_free(b.scr) {
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

/// The only place the write path writes the card.
unsafe fn card_write(sector: u32, bytes: u32, buf: *const u8) -> bool {
    let cut = read_volatile(addr_of!(DKRT_UPLOAD_CUT));
    if cut != 0 && DKRT_UPLOAD_WRITES >= cut {
        return false;
    }
    DKRT_UPLOAD_WRITES = DKRT_UPLOAD_WRITES.wrapping_add(1);
    let tear = read_volatile(addr_of!(DKRT_UPLOAD_TEAR.0));
    if tear != 0 && DKRT_UPLOAD_WRITES == tear && bytes == 512 {
        let t = addr_of_mut!(TEAR_BUF) as *mut u8;
        copy(t, buf, 256);
        zero(t.add(256), 256);
        block_write(sector, 512, t);
        write_volatile(addr_of_mut!(DKRT_UPLOAD_CUT), DKRT_UPLOAD_WRITES);
        return false;
    }
    let ok = block_write(sector, bytes, buf) == 0;
    let flip = read_volatile(addr_of!(DKRT_UPLOAD_FLIP.0));
    if ok && flip != 0 && DKRT_UPLOAD_WRITES == flip {
        let t = addr_of_mut!(TEAR_BUF) as *mut u8;
        copy(t, buf, 512);
        *t ^= 0xff;
        block_write(sector, 512, t);
    }
    ok
}

pub fn sectors_of(bytes: u32) -> u32 {
    store::sectors_of(bytes)
}

/// Sectors a store may use now: up to header B, or the test knob.
fn limit_now() -> u32 {
    let l = lay();
    let cap = unsafe { read_volatile(addr_of!(DKRT_STORE_CAP)) };
    if cap != 0 && cap < l.limit() {
        cap
    } else {
        l.limit()
    }
}

/// Room for the extents an allocation keeps out of: the capacity knob, a file being written and the leases.
pub const EXTRA_MAX: usize = 2 + PENDING_MAX + lease::MAX;

/// The extents an allocation keeps out of: the sectors the test knob takes away, `pend` (a file being written) and the leased extents. A lease
/// guards its file against replace and delete, so its extent is free space only if the table lost the file by a path the writer does not
/// know (a CLEAR is refused too); keeping it out of the allocator as well makes the guarantee hold without that assumption. The caller holds
/// the store lock.
pub fn extras(buf: &mut [(u32, u32); EXTRA_MAX], pend: Option<(u32, u32)>) -> &[(u32, u32)] {
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
    n = pend_extents(buf, n);
    n = leases().extents(buf, n);
    &buf[..n]
}

/// Bytes of free space in all (the room for the table's next entry kept back), saturated to 32 bits for INFO.
pub fn free_bytes(h: &Hdr) -> u32 {
    let pf = pf();
    let room = store::table_sectors(h.count + pf.n as u32 + 1);
    let b = limit_now().saturating_sub(h.used).saturating_sub(room).saturating_sub(pf.sectors()) as u64 * 512;
    if b > u32::MAX as u64 {
        u32::MAX
    } else {
        b as u32
    }
}

pub unsafe fn copy(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
}

pub unsafe fn zero(dst: *mut u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = 0;
        i += 1;
    }
}

/// Make the two header copies say the same thing before any space is reused (`store::sync_headers`). `false`: that write failed.
pub unsafe fn sync_headers(b: &Bufs, t: &Table) -> bool {
    store::sync_headers(&io(), &lay(), t, b.scr)
}

/// Commit an edit of the table `cur`; the runtime's store cache learns the new table. A status.
pub unsafe fn do_commit(b: &Bufs, cur: &Hdr, edit: Edit, pend: Option<(u32, u32)>) -> i32 {
    do_commit_hinted(b, cur, edit, pend, None)
}

/// `do_commit`; `hint`: what the session's BEGIN found, used when it still holds (`Hint`).
unsafe fn do_commit_hinted(b: &Bufs, cur: &Hdr, edit: Edit, pend: Option<(u32, u32)>, hint: Option<&Hint>) -> i32 {
    let mut ex = [(0, 0); EXTRA_MAX];
    let extra = extras(&mut ex, pend);
    let at = match hint {
        Some(k) if k.valid && k.h == *cur && k.n == extra.len() && same_extents(&k.extra[..k.n], extra) => Some(k.at),
        _ => None,
    };
    match store::commit_at(&io(), &lay(), cur, b.tin, b.tout, TBL_CAP, b.scr, edit, extra, at) {
        Ok(c) => {
            crate::store::note_commit(&c.h);
            OK
        }
        Err(e) => pe(e),
    }
}

fn same_extents(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// ---- pending files (DK_FILE_DEFER) ---------------------------------------------------------------------------------------------

/// At most this many pending files, and this many bytes of them (docs/plans/usb-fast-batch.md B): a deferred create that would pass either commits all first.
pub const PENDING_MAX: usize = store::MANY_MAX;
const PENDING_BYTES_MAX: u32 = 16 * 1024 * 1024;
/// 1 s of block clock: the owner's last file call this long ago, and the pending files are committed.
const PENDING_IDLE: u32 = 1500;
/// After a failed automatic commit, wait this long before the next try.
const PENDING_RETRY: u32 = 1500;

/// Files that are written, read back and hash-checked, and named in no table yet. Their extents are kept out of every allocation (`extras`).
/// Only the card holds them: a power cut loses them (the space is free after boot, the old table is valid).
struct Pending {
    n: usize,
    owner: [u8; PENDING_MAX],
    /// The 256-byte table entries.
    ent: [[u8; store::ENTRY]; PENDING_MAX],
    /// The I/O job of the latest pending file: its end triggers the commit.
    job: u32,
    /// The block clock at the last file call of any module.
    last: u32,
    /// Commit now (the job ended or the module goes), not after the idle time.
    flush: bool,
    /// The automatic commit job is posted.
    posted: bool,
    /// No automatic commit before this block time (after an error).
    retry_at: u32,
    retrying: bool,
}

impl Pending {
    fn sectors(&self) -> u32 {
        let mut t = 0;
        let mut i = 0;
        while i < self.n {
            t += store::entry_sectors(unsafe { &store::entry_of(self.ent[i].as_ptr()) });
            i += 1;
        }
        t
    }
    fn bytes(&self) -> u32 {
        let mut t = 0u32;
        let mut i = 0;
        while i < self.n {
            t = t.saturating_add(unsafe { store::entry_of(self.ent[i].as_ptr()) }.size);
            i += 1;
        }
        t
    }
    fn remove_at(&mut self, i: usize) {
        let mut k = i;
        while k + 1 < self.n {
            self.owner[k] = self.owner[k + 1];
            self.ent[k] = self.ent[k + 1];
            k += 1;
        }
        self.n -= 1;
    }
}

static mut PEND: Pending = Pending {
    n: 0,
    owner: [0; PENDING_MAX],
    ent: [[0; store::ENTRY]; PENDING_MAX],
    job: 0,
    last: 0,
    flush: false,
    posted: false,
    retry_at: 0,
    retrying: false,
};

fn pf() -> &'static mut Pending {
    unsafe { &mut *addr_of_mut!(PEND) }
}

/// Add the data extents of the pending files to `buf` from `n`: their data is on the card and nothing names it yet. The new length.
fn pend_extents(buf: &mut [(u32, u32); EXTRA_MAX], mut n: usize) -> usize {
    let p = pf();
    let mut i = 0;
    while i < p.n {
        let e = unsafe { store::entry_of(p.ent[i].as_ptr()) };
        let secs = store::entry_sectors(&e);
        if secs != 0 {
            buf[n] = (e.first, secs);
            n += 1;
        }
        i += 1;
    }
    n
}

/// Pending files of `owner`.
fn pend_count(owner: u8) -> i32 {
    let p = pf();
    let mut n = 0;
    let mut i = 0;
    while i < p.n {
        if p.owner[i] == owner {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Is there a pending file with this path (NUL-terminated)?
unsafe fn pend_has_path(path: *const u8) -> bool {
    let p = pf();
    let mut i = 0;
    while i < p.n {
        if store::relation(path, p.ent[i].as_ptr()) == 1 {
            return true;
        }
        i += 1;
    }
    false
}

/// Drop the pending file with this path (NUL-terminated). True when there was one.
unsafe fn pend_remove_path(path: *const u8) -> bool {
    let p = pf();
    let mut i = 0;
    while i < p.n {
        if store::relation(path, p.ent[i].as_ptr()) == 1 {
            p.remove_at(i);
            return true;
        }
        i += 1;
    }
    false
}

/// Drop every pending file (the SysEx CLEAR empties the store).
pub fn pend_clear() {
    let p = pf();
    p.n = 0;
    p.flush = false;
}

/// A file was verified: it replaces a pending file of its path and joins the others. It never commits and never fails for a limit: the create that
/// opened the file made room (`make_room`).
unsafe fn pend_add(owner: u8, job: u32, ent: &[u8; store::ENTRY]) -> i32 {
    pend_remove_path(ent.as_ptr());
    let p = pf();
    if p.n >= PENDING_MAX {
        return E_FULL;
    }
    p.ent[p.n] = *ent;
    p.owner[p.n] = owner;
    p.n += 1;
    p.job = job;
    p.last = now();
    OK
}

/// Must the pending files (`n` of them, `bytes` of data) be committed before a deferred file of `size` bytes is created?
fn room_needed(n: usize, bytes: u32, size: u32) -> bool {
    n != 0 && (n >= PENDING_MAX || bytes as u64 + size as u64 > PENDING_BYTES_MAX as u64)
}

/// A deferred create of a file of `size` bytes: when the pending files are at the limit (16 files) or would pass 16 MiB with this one, commit them first.
/// An error (or leftovers that stay pending, E_BUSY) is the create's result, before any data is sent. The caller holds the store lock.
unsafe fn make_room(b: &Bufs, size: u32) -> i32 {
    let p = pf();
    if !room_needed(p.n, p.bytes(), size) {
        return OK;
    }
    let r = commit_pending(b, None);
    if r != OK {
        return r;
    }
    if pf().n >= PENDING_MAX {
        return E_BUSY; // entries whose replaced file is leased stay pending: try again
    }
    OK
}

/// Name the pending files of `owner` (all when `None`) in the table in one commit. Nothing is dropped on an error. The caller holds the store lock.
pub unsafe fn commit_pending(b: &Bufs, owner: Option<u8>) -> i32 {
    let p = pf();
    let mut ptrs = [core::ptr::null::<u8>(); PENDING_MAX];
    let mut which = [0usize; PENDING_MAX];
    let mut n = 0;
    let mut i = 0;
    while i < p.n {
        if owner.map_or(true, |o| o == p.owner[i]) {
            ptrs[n] = p.ent[i].as_ptr();
            which[n] = i;
            n += 1;
        }
        i += 1;
    }
    if n == 0 {
        return OK;
    }
    if !card::writable() {
        return if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE };
    }
    let t = match state(b) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if !sync_headers(b, &t) {
        return E_WRITE;
    }
    // a reader took a lease on a file that a pending one replaces: the commit would free its extent, so that entry stays pending (the others commit)
    let mut kept = 0;
    if leased_any() {
        let mut k = 0;
        let mut m = 0;
        while k < n {
            let leased_target = match store::find(&io(), &lay(), &t.h, b.tin, TBL_CAP, ptrs[k]) {
                Ok(Some((_, old))) => leased(&old),
                Ok(None) => false,
                Err(e) => return pe(e),
            };
            if leased_target {
                kept += 1;
            } else {
                ptrs[m] = ptrs[k];
                which[m] = which[k];
                m += 1;
            }
            k += 1;
        }
        n = m;
        if n == 0 {
            return E_FILE_IN_USE;
        }
    }
    // an open session's extent stays out of the table's place too (`extras` already holds the pending files)
    let u = up();
    let open = if u.kind != NONE && !u.pd && u.size != 0 { Some((u.start, sectors_of(u.size))) } else { None };
    let r = do_commit(b, &t.h, Edit::InsertMany(ptrs.as_ptr(), n), open);
    if r == OK {
        // drop what was named, last first so the indexes hold
        let mut k = n;
        while k > 0 {
            k -= 1;
            p.remove_at(which[k]);
        }
        if p.n == 0 {
            p.flush = false;
        }
        if kept != 0 {
            // the leased ones are tried again by the automatic commit, after a pause
            p.retrying = true;
            p.retry_at = now().wrapping_add(PENDING_RETRY);
        }
    }
    r
}

/// Are the pending files due for the automatic commit at block time `t`: not waiting after an error, and their job or module gone or the owner idle?
fn pend_due(p: &Pending, t: u32) -> bool {
    if p.n == 0 || p.posted {
        return false;
    }
    if p.retrying && (t.wrapping_sub(p.retry_at) as i32) < 0 {
        return false;
    }
    p.flush || t.wrapping_sub(p.last) >= PENDING_IDLE
}

/// UI task, once per pass: when pending files are due (the idle time passed, or their job or module is gone), post the commit as an I/O job. The
/// commit runs in the I/O task, never here.
unsafe fn pend_tick() {
    let p = pf();
    if p.n == 0 || p.posted {
        return;
    }
    #[cfg(not(test))]
    if crate::fw_blocks().is_none() {
        return;
    }
    if !pend_due(p, now()) {
        return;
    }
    p.posted = true;
    if crate::io::post(0, Some(pend_step), None, core::ptr::null_mut()) <= 0 {
        p.posted = false; // no job entry free: the next pass tries again
    }
}

/// The automatic commit, a job of the runtime in the I/O task.
unsafe extern "C" fn pend_step(_: *mut u8) -> i32 {
    if !acquire() {
        pf().posted = false;
        return 0; // the next pass posts it again
    }
    let r = commit_pending(&sdk_bufs(), None);
    unlock();
    let p = pf();
    p.posted = false;
    if r == OK {
        if p.n == 0 {
            p.retrying = false; // else entries with a leased target stay: `commit_pending` set the pause
        }
    } else {
        crate::dlog::ev(dkrt_sdk::logk::UPLOAD, dkrt_sdk::logk::F_ERROR, [0xb8, r as u32, p.n as u32, 0, 0]);
        p.retrying = true;
        p.retry_at = now().wrapping_add(PENDING_RETRY);
    }
    0
}

/// SysEx commands that change the table (MKDIR, RENAME) first name the pending files, so that their checks see them. `E_BUSY`-free: the caller holds the lock.
pub unsafe fn flush_pending(b: &Bufs) -> i32 {
    commit_pending(b, None)
}

// ---- begin, write, close, abort, delete ---------------------------------------------------------------------------------------

/// Open the write session of `kind` for the file `path` (`pl` bytes, NUL-terminated, already checked by `store::path_ok`) of `size` bytes.
/// `want`: the expected hash slot (END or `close` checks the received bytes against it). `no_overwrite`: an existing file is `E_EXISTS`.
/// The caller took the store lock and, for SysEx, called `preempt`; an open session is `E_BUSY`. On `E_FULL`, `free` is the free bytes.
pub unsafe fn begin(
    b: &Bufs,
    kind: u8,
    owner: u8,
    path: *const u8,
    pl: usize,
    size: u32,
    want: Option<[u8; 32]>,
    no_overwrite: bool,
    free: &mut u32,
) -> i32 {
    if up().kind != NONE {
        return E_BUSY;
    }
    if !card::writable() {
        return if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE };
    }
    let t = match state(b) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if !sync_headers(b, &t) {
        return E_WRITE;
    }
    let (io, lay) = (io(), lay());
    up().hint.valid = false;
    // the path may replace a file (unless `no_overwrite`), but not a folder, and may not lie inside a file or over a folder that has entries
    let mut conflict = false;
    let mut exists = false;
    let mut held = false;
    // one pass over the table: the path checks, the file's extent (the first gap that holds it) and whether the table of one more entry still has
    // room beside it (`store::begin_fit`)
    let need = sectors_of(size);
    let mut ex0 = [(0, 0); EXTRA_MAX];
    let fit = match store::begin_fit(&io, &lay, &t.h, b.tin, TBL_CAP, extras(&mut ex0, None), need, store::table_sectors(t.h.count + pf().n as u32 + 1), |e| {
        let rel = store::relation(path, e);
        if rel >= 2 || (rel == 1 && store::entry_of(e).kind == KIND_DIR) {
            conflict = true;
        } else if rel == 1 {
            exists = true;
            held = leased(&store::entry_of(e));
        }
        !conflict
    }) {
        Ok(f) => f,
        Err(r) => return pe(r),
    };
    // the pending files (named in no table yet) count like the stored ones
    let mut i = 0;
    while i < pf().n {
        let rel = store::relation(path, pf().ent[i].as_ptr());
        if rel >= 2 {
            conflict = true;
        } else if rel == 1 {
            exists = true;
        }
        i += 1;
    }
    if conflict {
        return E_PATH;
    }
    if exists && no_overwrite {
        return E_EXISTS;
    }
    // a reader still reads the file this one would replace (the commit would free its extent)
    if held {
        return E_FILE_IN_USE;
    }
    let start = match fit.data {
        Some(s) => s,
        None => {
            *free = free_bytes(&t.h);
            return E_FULL;
        }
    };
    let table_at = match fit.table {
        Some(a) => a,
        None => {
            *free = free_bytes(&t.h);
            return E_FULL;
        }
    };
    let pend = if need == 0 { None } else { Some((start, need)) };
    // the commit looks for the table's gap again only when the store or the kept-out extents changed (`Hint`)
    let mut ex1 = [(0, 0); EXTRA_MAX];
    let kept = extras(&mut ex1, pend);
    let hint = &mut up().hint;
    hint.h = t.h;
    hint.n = kept.len();
    hint.extra = [(0, 0); EXTRA_MAX];
    let mut i = 0;
    while i < kept.len() {
        hint.extra[i] = kept[i];
        i += 1;
    }
    hint.at = table_at;
    hint.valid = true;
    let u = up();
    zero(u.path.as_mut_ptr(), 160);
    copy(u.path.as_mut_ptr(), path, pl);
    u.size = size;
    u.have_want = want.is_some();
    if let Some(w) = want {
        u.sha = w;
    }
    u.run = Hash32::new();
    u.skip_rx = want.is_some() && crate::speed::on(crate::speed::HASH_ONCE);
    u.pd = false;
    u.defer = false;
    u.next = 0;
    u.have_last = false;
    u.last_off = 0;
    u.last_len = 0;
    u.start = start;
    u.owner = owner;
    NEXT_HANDLE = NEXT_HANDLE.wrapping_add(1) & 0x7fff;
    u.handle = NEXT_HANDLE + 1;
    u.job = if kind == SDK { crate::io::step_job() } else { 0 };
    u.last = now();
    u.kind = kind;
    OK
}

/// The handle of the open session (what an SDK `file_create` returns).
pub fn handle() -> i32 {
    up().handle
}

/// Is the open session the one of `kind` that the caller owns?
fn mine(kind: u8, owner: u8, handle: i32) -> bool {
    let u = up();
    u.kind == kind && (kind == SYSEX || (u.owner == owner && u.handle == handle))
}

/// Write `len` bytes at `data` to the open session, `off` being the next byte. `padded`: the buffer has room up to the next sector boundary
/// (SysEx: the body region), so a short last chunk is padded in place; else its tail sector goes through the scratch. A retry of the
/// previous SysEx chunk is ok and writes nothing. Statuses: `E_STATE`, `E_OFFSET`, `E_LENGTH`, `E_WRITE`.
pub unsafe fn write(b: &Bufs, kind: u8, owner: u8, handle: i32, off: u32, data: *mut u8, len: u32, padded: bool) -> i32 {
    // the card writes below may run as large commands (core/sdhc-large): `len` plus the padding of a last sector is the buffer's extent
    large_range(data as u32, (data as u32).wrapping_add(len).wrapping_add(512));
    let r = write_inner(b, kind, owner, handle, off, data, len, padded);
    large_range(0, 0);
    r
}

extern "C" {
    // Optional symbols of the adapter (`dkrt_sdhc.h`; `optional` in module.toml): 0 when core/sdhc-large is not in the image.
    fn sdhc_large_range(lo: u32, hi: u32);
    fn sdhc_large_ctl(key: u32, val: u32) -> u32;
}

/// Tell core/sdhc-large which buffer the next card transfers use (`lo` = `hi` = 0: none). A no-op when the module is not in the image (the symbol is 0).
#[inline(never)]
unsafe fn large_range(lo: u32, hi: u32) {
    let f: unsafe extern "C" fn(u32, u32) = sdhc_large_range;
    if core::hint::black_box(f as usize) != 0 {
        f(lo, hi);
    }
}

unsafe fn write_inner(b: &Bufs, kind: u8, owner: u8, handle: i32, off: u32, data: *mut u8, len: u32, padded: bool) -> i32 {
    if !mine(kind, owner, handle) {
        return E_STATE;
    }
    let u = up();
    if kind == SYSEX && u.have_last && off == u.last_off && len == u.last_len {
        return OK; // a retry of the previous chunk
    }
    if off != u.next {
        return E_OFFSET;
    }
    let end = off as u64 + len as u64;
    if end > u.size as u64 || (len & 511 != 0 && end != u.size as u64) {
        return E_LENGTH;
    }
    let (io, lay) = (io(), lay());
    let at = u.start + off / 512;
    if padded {
        let sectors = sectors_of(len);
        zero(data.add(len as usize), (sectors * 512 - len) as usize);
        if store::write_sectors(&io, &lay, at, sectors, data) != OK {
            return E_WRITE;
        }
    } else {
        let whole = len / 512;
        if whole > 0 && store::write_sectors(&io, &lay, at, whole, data) != OK {
            return E_WRITE;
        }
        if len & 511 != 0 {
            copy(b.scr, data.add((whole * 512) as usize), (len & 511) as usize);
            zero(b.scr.add((len & 511) as usize), (512 - (len & 511)) as usize);
            if store::write_sectors(&io, &lay, at + whole, 1, b.scr) != OK {
                return E_WRITE;
            }
        }
    }
    u.last = now();
    u.have_last = true;
    u.last_off = off;
    u.last_len = len;
    u.next = end as u32;
    if !u.skip_rx {
        u.run.update(data, len);
    }
    OK
}

/// End the session: the bytes received must be all of `size` (`E_LENGTH`) and hash to the expected slot when there is one (`E_HASH`), then the
/// table is committed. The session is closed whatever the result. `out`: when not null, the 4 bytes of the received data's hash (`hash | 1`,
/// big-endian: the device's file hash) are written there before any check. `E_STATE`: no such session.
pub unsafe fn close(b: &Bufs, kind: u8, owner: u8, handle: i32, out: *mut u8) -> i32 {
    if !mine(kind, owner, handle) {
        return E_STATE;
    }
    // the session stays open (`is_open`, which refuses RESTART) until the commit is done
    let r = close_inner(b, out);
    up().kind = NONE;
    r
}

unsafe fn close_inner(b: &Bufs, out: *mut u8) -> i32 {
    let u = up();
    if u.next != u.size {
        return E_LENGTH;
    }
    let got;
    if u.skip_rx {
        // lever A: nothing was hashed on the way in. The card holds what was received: read it back, hash it once, and that hash is the file's
        // (compared with the expected one before anything commits: it covers the USB transfer and the card)
        let r = if u.size == 0 { Ok(hash32::slot(Hash32::new().finish())) } else { readback_file(b, u.start, u.size) };
        got = match r {
            Ok(g) => g,
            Err(e) => return e,
        };
        if !out.is_null() {
            copy(out, got.as_ptr(), 4);
        }
        if got != u.sha {
            return E_HASH;
        }
    } else {
        // the hash of the bytes received (`write` fed it as they came)
        got = hash32::slot(core::mem::replace(&mut u.run, Hash32::new()).finish());
        if !out.is_null() {
            copy(out, got.as_ptr(), 4);
        }
        if u.have_want && got != u.sha {
            return E_HASH;
        }
        // the card holds what was received: read the file back and hash it (the end-to-end check; capability READBACK of INFO)
        if u.size != 0 {
            match readback_file(b, u.start, u.size) {
                Ok(g) => {
                    if g != got {
                        return E_HASH;
                    }
                }
                Err(e) => return e,
            }
        }
    }
    if u.defer {
        // a deferred file is verified: it becomes a pending file and is named in the table at the next commit of pending files
        let mut ent = [0u8; store::ENTRY];
        let e = Entry { first: u.start, size: u.size, kind: KIND_FILE, sha: got };
        store::encode_entry(u.path.as_ptr(), &e, ent.as_mut_ptr());
        u.hint.valid = false;
        return pend_add(u.owner, u.job, &ent);
    }
    // the new table: the current one with the entry replaced or inserted
    let t = match state(b) {
        Ok(t) => t,
        Err(e) => return e,
    };
    // a reader took a lease on the file this commit replaces after BEGIN: the commit would free its extent, so the new bytes are not committed
    if leased_any() {
        match store::find(&io(), &lay(), &t.h, b.tin, TBL_CAP, u.path.as_ptr()) {
            Ok(Some((_, old))) => {
                if leased(&old) {
                    return E_FILE_IN_USE;
                }
            }
            Ok(None) => {}
            Err(e) => return pe(e),
        }
    }
    let mut ent = [0u8; store::ENTRY];
    let e = Entry { first: u.start, size: u.size, kind: KIND_FILE, sha: got };
    store::encode_entry(u.path.as_ptr(), &e, ent.as_mut_ptr());
    let pend = if u.size == 0 { None } else { Some((u.start, sectors_of(u.size))) };
    let hint = u.hint;
    u.hint.valid = false;
    let r = do_commit_hinted(b, &t.h, Edit::Insert(ent.as_ptr()), pend, Some(&hint));
    if r == OK {
        pend_remove_path(u.path.as_ptr()); // the file replaced a pending one of the same path
    }
    r
}

/// The read-back piece: the bytes of one card read of the close. The firmware's block read takes 64 KiB at most (`store::MAX_IO_SECTORS` sectors); with
/// core/sdhc-large on, a read into a registered buffer takes up to 511 KiB, and the close reads 256 KiB pieces.
const RB_DEFAULT: usize = store::MAX_IO_SECTORS as usize * 512;
const RB_LARGE: usize = 256 * 1024;

/// Whether the large-command adapter (core/sdhc-large) is present and its switch is on (`sdhc_large_ctl` key 0), so a block read of a registered buffer
/// can exceed `store::MAX_IO_SECTORS` sectors. An absent adapter (the symbol is 0): false. Without it the piece stays at `RB_DEFAULT`.
#[cfg(not(test))]
fn large_reads_allowed() -> bool {
    let f: unsafe extern "C" fn(u32, u32) -> u32 = sdhc_large_ctl;
    // val 2 changes nothing: it reads the switch
    core::hint::black_box(f as usize) != 0 && unsafe { f(0, 2) } == 1
}
#[cfg(test)]
fn large_reads_allowed() -> bool {
    false
}

/// The piece the close reads: `RB_LARGE` when `large_reads_allowed`, else `RB_DEFAULT`.
fn readback_piece() -> usize {
    if large_reads_allowed() { RB_LARGE } else { RB_DEFAULT }
}

/// `n` bytes, 16-byte aligned, from the firmware's allocator (null when it has none free or gives an unaligned block: the close then reads in the
/// caller's smaller pieces). Free it with `rb_free`.
#[cfg(not(test))]
unsafe fn rb_alloc(n: usize) -> *mut u8 {
    let p = PLATFORM.alloc(n as u32);
    if !p.is_null() && p as usize & 15 != 0 {
        PLATFORM.free(p);
        return core::ptr::null_mut();
    }
    p
}
#[cfg(not(test))]
unsafe fn rb_free(p: *mut u8) {
    PLATFORM.free(p)
}
#[cfg(test)]
unsafe fn rb_alloc(_: usize) -> *mut u8 {
    core::ptr::null_mut()
}
#[cfg(test)]
unsafe fn rb_free(_: *mut u8) {}

/// The card's DMA writes behind the data cache: make the lines of `[p, p + n)` clean first (`before`: a push of a dirty line after the DMA would write old
/// bytes over it) and drop them after each read. Nothing writes these buffers but the DMA, so the lines stay clean between the two.
#[cfg(not(test))]
fn cache_sync(p: *const u8, n: usize) {
    crate::flush(p as u32, n as u32);
}
#[cfg(test)]
fn cache_sync(_: *const u8, _: usize) {}

/// Read the file at store sector `start`, `size` bytes, back from the card and return its hash slot. A file of more than one small read gets a buffer of
/// its own from the firmware's allocator, the read-back piece (64 KiB, 256 KiB with core/sdhc-large on), twice that when lever C (`speed::OVERLAP`) is on and the helper
/// task exists; otherwise the caller's buffer.
unsafe fn readback_file(b: &Bufs, start: u32, size: u32) -> Result<[u8; 32], i32> {
    let l = lay();
    let piece = readback_piece();
    let two = crate::speed::on(crate::speed::OVERLAP) && size as usize > piece;
    let want = if size as usize > b.rb_cap { if two { 2 * piece } else { piece } } else { 0 };
    let big = if want != 0 { rb_alloc(want) } else { core::ptr::null_mut() };
    let r = if big.is_null() {
        readback_hash(|s, n, p| block_read(l.first + s, n, p), start, size, b.rb, b.rb_cap, false)
    } else {
        // the card reads into `big` may run as large commands (core/sdhc-large); one range at a time: `write` clears its own before this runs
        let ranged = piece > RB_DEFAULT;
        if ranged {
            large_range(big as u32, (big as u32).wrapping_add(want as u32));
        }
        let r = readback_hash(|s, n, p| block_read(l.first + s, n, p), start, size, big, piece, two);
        if ranged {
            large_range(0, 0);
        }
        r
    };
    if !big.is_null() {
        rb_free(big);
    }
    r
}

/// The hash a read-back piece is fed to, on the helper task (`aux`) while the next piece is read.
struct HashJob {
    h: *mut Hash32,
    buf: *const u8,
    n: u32,
}

extern "C" fn hash_job(ctx: *mut u8) {
    unsafe {
        let j = &*(ctx as *const HashJob);
        (*j.h).update(j.buf, j.n);
    }
}

/// Read `size` bytes of the file at store sector `start` back through `rd(sector, bytes, buf)` and return their hash slot, in pieces of at most `cap`
/// bytes (a multiple of 512; `buf` holds one piece, or two when `two`) of at most `RB_LARGE`. `E_READ` when a read fails.
/// With `two`, the hash of piece n runs on the helper task while piece n + 1 is read into the other half of `buf`; the call returns only when the helper is idle.
unsafe fn readback_hash(mut rd: impl FnMut(u32, u32, *mut u8) -> i32, start: u32, size: u32, buf: *mut u8, cap: usize, two: bool) -> Result<[u8; 32], i32> {
    let piece = cap & !511;
    let mut h = Hash32::new();
    let hp: *mut Hash32 = &mut h;
    let mut job = HashJob { h: hp, buf: core::ptr::null(), n: 0 };
    let (mut left, mut sector, mut i) = (size, start, 0usize);
    let mut pending = false;
    let mut err = OK;
    cache_sync(buf, if two { 2 * piece } else { piece });
    while left > 0 {
        let n = core::cmp::min(left, piece as u32);
        let secs = sectors_of(n);
        let at = if two { buf.add((i & 1) * piece) } else { buf };
        if rd(sector, secs * 512, at) != 0 {
            err = E_READ;
            break;
        }
        cache_sync(at, (secs * 512) as usize);
        if pending {
            crate::aux::wait();
            pending = false;
        }
        if two {
            job.buf = at;
            job.n = n;
            if crate::aux::run(hash_job, &mut job as *mut HashJob as *mut u8) == OK {
                pending = true;
            } else {
                (*hp).update(at, n);
            }
        } else {
            (*hp).update(at, n);
        }
        left -= n;
        sector += secs;
        i += 1;
    }
    if pending {
        crate::aux::wait();
    }
    if err != OK {
        return Err(err);
    }
    Ok(hash32::slot(core::ptr::read(hp).finish()))
}

/// `readback_hash` of one buffer, compared with `want`: `E_READ` when a read fails, `E_HASH` when the card's bytes differ.
#[cfg(test)]
unsafe fn readback(rd: impl FnMut(u32, u32, *mut u8) -> i32, start: u32, size: u32, buf: *mut u8, cap: usize, want: &[u8; 32]) -> i32 {
    match readback_hash(rd, start, size, buf, cap, false) {
        Ok(g) => if g == *want { OK } else { E_HASH },
        Err(e) => e,
    }
}

/// Drop the session of `kind` (the partial file was never in the table). `E_STATE`: no such session.
pub fn abort(kind: u8, owner: u8, handle: i32) -> i32 {
    if !mine(kind, owner, handle) {
        return E_STATE;
    }
    up().kind = NONE;
    OK
}

/// Remove `path` (NUL-terminated, already checked) from the table: a file or a folder entry. No session may be open (the caller called
/// `preempt` for SysEx; an open session is `E_BUSY` here). The space is free again when the commit is done.
/// `drop_sysex`: the caller is the SysEx DELETE, which takes the place of an open SysEx session. The session is dropped only when the command goes
/// on to commit: a refusal (`E_FILE_IN_USE`, `E_NOFILE`, ...) leaves it open. An SDK session is always `E_BUSY`.
pub unsafe fn delete(b: &Bufs, path: *const u8, drop_sysex: bool) -> i32 {
    let k = up().kind;
    if k == SDK || (k == SYSEX && !drop_sysex) {
        return E_BUSY;
    }
    if !card::writable() {
        return if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE };
    }
    let t = match state(b) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if !sync_headers(b, &t) {
        return E_WRITE;
    }
    // a pending file of this path is dropped (it was never named) only after every check passed; a stored one is removed by a commit
    let was_pending = pend_has_path(path);
    match store::find(&io(), &lay(), &t.h, b.tin, TBL_CAP, path) {
        Ok(Some((_, e))) => {
            if leased(&e) {
                return E_FILE_IN_USE;
            }
        }
        Ok(None) => {
            if was_pending {
                pend_remove_path(path);
                return OK;
            }
            return E_NOFILE;
        }
        Err(e) => return pe(e),
    }
    if k == SYSEX {
        up().kind = NONE;
    }
    let r = do_commit(b, &t.h, Edit::Remove(path), None);
    if r == OK {
        pend_remove_path(path);
    }
    r
}

// ---- the SDK front end (`file_*`, api 1.10) -----------------------------------------------------------------------------------

/// `DK_FILES_*` of `<dk/platform/files.h>`.
pub(crate) const ROOT_SAMPLES: u32 = 1;
pub(crate) const ROOT_STORE: u32 = 2;
/// `DK_FILE_REPLACE` of `<dk/platform/file.h>`.
pub const FILE_REPLACE: u32 = 1;
/// `DK_FILE_DEFER`: the file is verified at close and named in the table at the next commit of pending files.
pub const FILE_DEFER: u32 = 2;
/// A store path with its root prefix: at most `store::PATH_MAX` bytes and the NUL.
const FULL_MAX: usize = 168;

/// The SDK calls run in an I/O step only: the running task is the I/O task and a step is running. The owner is the module whose job it is.
unsafe fn sdk_owner() -> Result<u8, i32> {
    match crate::io::step_owner() {
        Some(o) => {
            pf().last = now(); // the owner's last file call: the idle time of the pending files counts from here
            Ok(o)
        }
        None => Err(E_CONTEXT),
    }
}

/// An SDK result: E_BUSY tells the I/O task that the step met a busy store, so that it sleeps a moment before it runs a step that returns DK_IO_MORE.
fn busy(r: i32) -> i32 {
    if r == E_BUSY {
        crate::io::note_busy();
    }
    r
}

/// Where the root's files live: the prefix that the store path takes, and whether the +Drive adapter serves it. The device's capabilities decide:
/// the samples are files of the store (`samples/`), or the firmware's folder filesystem behind the staged writer (`E_CONTRACT` without it).
pub(crate) fn root_route<P: Platform>(p: &P, root: u32) -> Result<(&'static [u8], bool), i32> {
    if root == ROOT_STORE {
        Ok((b"", false))
    } else if root == ROOT_SAMPLES && p.samples_in_store() {
        Ok((b"samples/", false))
    } else if root == ROOT_SAMPLES && StagedWriter::present(p) {
        Ok((b"", true))
    } else {
        Err(E_CONTRACT) // no such root, or no adapter in the image
    }
}

/// `root`/`path` as one path in `full` (the store's: `store::path_ok`; the +Drive's: the same rules without a root prefix). `pd` is set for the
/// DT2 samples root, which the +Drive adapter serves. `E_CONTRACT`: no such root; `E_PATH`: no NUL in 160 bytes, or the path breaks the rules, or
/// (+Drive) it starts in the adapter's staging folder. Returns the length.
unsafe fn sdk_path(root: u32, path: *const u8, full: &mut [u8; FULL_MAX], pd: &mut bool) -> Result<usize, i32> {
    let (prefix, via_pd) = root_route(&PLATFORM, root)?;
    *pd = via_pd;
    if path.is_null() {
        return Err(E_PATH);
    }
    let mut n = 0;
    while n < 160 && *path.add(n) != 0 {
        n += 1;
    }
    if n >= 160 {
        return Err(E_PATH);
    }
    zero(full.as_mut_ptr(), FULL_MAX);
    copy(full.as_mut_ptr(), prefix.as_ptr(), prefix.len());
    copy(full.as_mut_ptr().add(prefix.len()), path, n);
    match store::path_ok(full.as_ptr(), FULL_MAX) {
        Some(l) => {
            if *pd && pd_staging(full.as_ptr(), l) {
                return Err(E_PATH);
            }
            Ok(l)
        }
        None => Err(E_PATH),
    }
}

/// `file_create`: a write session for a new file; the handle (> 0) or a status. `hash4`: null, or the expected `hash | 1`, big-endian.
/// `flags`: `FILE_REPLACE` or 0.
pub unsafe fn sdk_create(root: u32, path: *const u8, size: u32, hash4: *const u8, flags: u32) -> i32 {
    let owner = match sdk_owner() {
        Ok(o) => o,
        Err(e) => return e,
    };
    if flags & !(FILE_REPLACE | FILE_DEFER) != 0 {
        return E_CONTRACT;
    }
    let mut full = [0u8; FULL_MAX];
    let mut pd = false;
    let pl = match sdk_path(root, path, &mut full, &mut pd) {
        Ok(l) => l,
        Err(e) => return e,
    };
    if pd {
        return pd_create(owner, full.as_ptr(), size, hash4, flags & FILE_REPLACE); // the +Drive names its files at close
    }
    let want = if hash4.is_null() {
        None
    } else {
        let mut w = [0u8; 32];
        copy(w.as_mut_ptr(), hash4, 4);
        Some(w)
    };
    if !acquire() {
        return busy(E_BUSY);
    }
    let mut free = 0;
    if flags & FILE_DEFER != 0 {
        let m = make_room(&sdk_bufs(), size);
        if m != OK {
            unlock();
            return busy(m);
        }
    }
    let r = begin(&sdk_bufs(), SDK, owner, full.as_ptr(), pl, size, want, flags & FILE_REPLACE == 0, &mut free);
    unlock();
    if r != OK {
        return busy(r);
    }
    up().defer = flags & FILE_DEFER != 0;
    handle()
}

/// `dk_files_commit`: name the calling module's pending files in one commit. The DT2 samples root: finish the adapter's table rebuild.
pub unsafe fn sdk_commit() -> i32 {
    let owner = match sdk_owner() {
        Ok(o) => o,
        Err(e) => return e,
    };
    if !acquire() {
        return busy(E_BUSY);
    }
    let r = commit_pending(&sdk_bufs(), Some(owner));
    unlock();
    if r != OK {
        return busy(r);
    }
    if !PLATFORM.samples_in_store() && pd_present() {
        if up().pd || !pd_enter() {
            return busy(E_BUSY); // a +Drive file is open, or the adapter is in a call
        }
        let r = pd_commit();
        pd_leave();
        return r;
    }
    OK
}

/// `dk_files_pending`: the calling module's pending files.
pub unsafe fn sdk_pending() -> i32 {
    match sdk_owner() {
        Ok(o) => pend_count(o),
        Err(e) => e,
    }
}

/// `file_write`: `len` bytes of `buf` (16-byte aligned) at `offset`, the next byte, in pieces of `WRITE_PIECE`. The store lock is taken once for the
/// whole call: `E_BUSY` only when it could not be taken, before anything was written. A status, 0 when all went in.
pub unsafe fn sdk_write(h: i32, offset: u32, buf: *const u8, len: u32) -> i32 {
    let owner = match sdk_owner() {
        Ok(o) => o,
        Err(e) => return e,
    };
    if buf.is_null() || (buf as usize) & 15 != 0 {
        return E_CONTRACT;
    }
    if up().pd {
        return pd_write(owner, h, offset, buf, len);
    }
    if !acquire() {
        return busy(E_BUSY);
    }
    let mut done = 0u32;
    let mut r = OK;
    while done < len && r == OK {
        let n = if len - done > WRITE_PIECE as u32 { WRITE_PIECE as u32 } else { len - done };
        r = write(&sdk_bufs(), SDK, owner, h, offset.wrapping_add(done), buf.add(done as usize) as *mut u8, n, false);
        done += n;
    }
    unlock();
    r
}

/// `file_close`: end and commit; `out` (4 bytes, may be null) gets the device's file hash.
pub unsafe fn sdk_close(h: i32, out: *mut u8) -> i32 {
    let owner = match sdk_owner() {
        Ok(o) => o,
        Err(e) => return e,
    };
    if up().pd {
        return pd_close(owner, h, out);
    }
    if !acquire() {
        return busy(E_BUSY);
    }
    let r = close(&sdk_bufs(), SDK, owner, h, out);
    unlock();
    r
}

pub unsafe fn sdk_abort(h: i32) -> i32 {
    let owner = match sdk_owner() {
        Ok(o) => o,
        Err(e) => return e,
    };
    if up().pd {
        return pd_abort(owner, h);
    }
    if !acquire() {
        return busy(E_BUSY);
    }
    let r = abort(SDK, owner, h);
    unlock();
    r
}

pub unsafe fn sdk_delete(root: u32, path: *const u8) -> i32 {
    if let Err(e) = sdk_owner() {
        return e;
    }
    let mut full = [0u8; FULL_MAX];
    let mut pd = false;
    if let Err(e) = sdk_path(root, path, &mut full, &mut pd) {
        return e;
    }
    if pd {
        return pd_delete(full.as_ptr());
    }
    if !acquire() {
        return busy(E_BUSY);
    }
    let r = delete(&sdk_bufs(), full.as_ptr(), false);
    unlock();
    busy(r)
}


// ---- the +Drive route (DT2 samples root, `core/plusdrive-write`) --------------------------------------------------------------

extern "C" {
    // Optional symbols of the adapter (`dkrt_pdw.h`; `[object] optional` in module.toml): 0 when it is not in the image.
    fn dkpw_version() -> u32;
    fn dkpw_begin(path: *const u8, size: u32, hash4: *const u8, flags: u32) -> i32;
    fn dkpw_write(buf: *const u8, len: u32) -> i32;
    fn dkpw_close(hash_out: *mut u8) -> i32;
    fn dkpw_abort();
    fn dkpw_delete(path: *const u8) -> i32;
    fn dkpw_recover(out: *mut u32) -> i32;
    fn dkpw_commit() -> i32;
    fn dkpw_staging() -> *const u8;
}

/// An adapter call is running (the I/O task, or the UI task in `pd_tick`): the adapter has one set of state and runs one call at a time.
static mut PD_BUSY: bool = false;
/// The adapter holds a staged file that the runtime no longer tracks (a session dropped by idle expiry, job end or unload): `pd_tick` aborts it.
static mut PD_DIRTY: bool = false;
/// The adapter may hold a staged file (`dkpw_begin` succeeded and no close or abort since).
static mut PD_ACTIVE: bool = false;
/// Test knob: when not 0, the adapter stops dead at that step of the next +Drive write (`dkrt_pdw.h`), as a power cut would; 0 at every boot.
#[no_mangle]
pub static mut DKRT_PD_CUT: u32 = 0;
/// The boot recovery of the staging folder ran.
static mut PD_RECOVERED: bool = false;
/// The staging folder may exist: the adapter keeps it for a whole upload session. `pd_tick` removes it (the adapter's recovery call removes an
/// empty folder) once no write session has run for `PD_STAGE_IDLE` blocks.
static mut PD_STAGE: bool = false;
/// Block clock at the last start or end of a +Drive write session.
static mut PD_STAGE_AT: u32 = 0;
/// 2 s (1500 blocks a second): longer than the gap between the files of one upload, short enough that the folder is gone soon after it.
const PD_STAGE_IDLE: u32 = 2 * 1500;

fn pd_present() -> bool {
    StagedWriter::present(&PLATFORM)
}

/// Take the adapter for one call while no write session is open (the UI pass). The session test and the claim of the adapter are one step under
/// the irq mask, so a `pd_create` that claims the session meanwhile finds the adapter busy and gives up.
fn pd_enter_idle() -> bool {
    let sr = crate::irq_mask();
    let ok = unsafe { !read_volatile(addr_of!(PD_BUSY)) && up().kind == NONE };
    if ok {
        unsafe { write_volatile(addr_of_mut!(PD_BUSY), true) };
    }
    crate::irq_restore(sr);
    ok
}

/// Take the adapter for one call. False: another call is running.
fn pd_enter() -> bool {
    let sr = crate::irq_mask();
    let free = unsafe { !read_volatile(addr_of!(PD_BUSY)) };
    if free {
        unsafe { write_volatile(addr_of_mut!(PD_BUSY), true) };
    }
    crate::irq_restore(sr);
    free
}

fn pd_leave() {
    unsafe { write_volatile(addr_of_mut!(PD_BUSY), false) };
}

fn pd_busy() -> bool {
    unsafe { read_volatile(addr_of!(PD_BUSY)) }
}

/// Drop an SDK session that nobody ends any more; a +Drive session leaves a staged file that `pd_tick` removes.
fn drop_sdk(u: &mut Up) {
    if u.pd {
        unsafe { write_volatile(addr_of_mut!(PD_DIRTY), true) };
    }
    u.kind = NONE;
}

/// Is `full` (`l` bytes) the adapter's staging folder or inside it?
unsafe fn pd_staging(full: *const u8, l: usize) -> bool {
    let st = dkpw_staging();
    let mut n = 0;
    while *st.add(n) != 0 {
        n += 1;
    }
    // `staging` is the folder name with its leading '/': the path has none, so compare from the second byte
    let name = st.add(1);
    let m = n - 1;
    if l < m {
        return false;
    }
    let mut i = 0;
    while i < m {
        if *full.add(i) != *name.add(i) {
            return false;
        }
        i += 1;
    }
    l == m || *full.add(m) == b'/'
}

/// UI task, once per pass: remove the staged file of a dropped session, and run the staging recovery once after the filesystem is up.
unsafe fn pd_tick() {
    if PLATFORM.samples_in_store() || !pd_present() {
        return;
    }
    if read_volatile(addr_of!(PD_DIRTY)) && pd_enter_idle() {
        dkpw_abort();
        write_volatile(addr_of_mut!(PD_DIRTY), false);
        write_volatile(addr_of_mut!(PD_ACTIVE), false);
        write_volatile(addr_of_mut!(PD_STAGE_AT), now());
        pd_leave();
    }
    // The upload session is over: remove the empty staging folder. `pd_enter_idle` holds the adapter and checks that no write session is open.
    let idle = read_volatile(addr_of!(PD_STAGE)) && now().wrapping_sub(read_volatile(addr_of!(PD_STAGE_AT))) >= PD_STAGE_IDLE;
    if (!read_volatile(addr_of!(PD_RECOVERED)) || idle) && pd_enter_idle() {
        let mut out = [0u32; 4];
        let r = dkpw_recover(out.as_mut_ptr());
        pd_leave();
        if r >= 0 {
            write_volatile(addr_of_mut!(PD_STAGE), false);
            write_volatile(addr_of_mut!(PD_RECOVERED), true);
            if out[0] != 0 || out[1] != 0 || out[2] != 0 || out[3] != 0 {
                // renamed into place, deleted, our leftovers that stayed, entries that are not ours (left alone)
                crate::dlog::ev(dkrt_sdk::logk::UPLOAD, 0, [0xb7, out[0], out[1], out[2], out[3]]);
            }
        }
    }
}

/// The adapter's deferred table rebuild (`dkpw_commit`); the caller holds the adapter (`pd_enter`).
unsafe fn pd_commit() -> i32 {
    dkpw_commit()
}

/// Claim the one write session for the +Drive. `E_BUSY`: a session is open or the store is in use.
unsafe fn pd_create(owner: u8, path: *const u8, size: u32, hash4: *const u8, flags: u32) -> i32 {
    if !pd_present() {
        return E_CONTRACT;
    }
    if !read_volatile(addr_of!(PD_RECOVERED)) {
        return busy(E_BUSY); // the boot recovery of the staging folder has not run yet
    }
    if !acquire() {
        return busy(E_BUSY);
    }
    let u = up();
    if u.kind != NONE {
        unlock();
        return busy(E_BUSY);
    }
    // the claim is made before the adapter call (which can take long): a SysEx BEGIN meanwhile finds an SDK session and says E_BUSY
    u.pd = true;
    u.size = size;
    u.next = 0;
    u.owner = owner;
    NEXT_HANDLE = NEXT_HANDLE.wrapping_add(1) & 0x7fff;
    u.handle = NEXT_HANDLE + 1;
    u.job = crate::io::step_job();
    u.last = now();
    u.kind = SDK;
    unlock();
    if !pd_enter() {
        u.kind = NONE;
        u.pd = false;
        return busy(E_BUSY);
    }
    if read_volatile(addr_of!(PD_ACTIVE)) {
        dkpw_abort(); // a staged file that no session owns
        write_volatile(addr_of_mut!(PD_ACTIVE), false);
    }
    let r = dkpw_begin(path, size, hash4, flags | (read_volatile(addr_of!(DKRT_PD_CUT)) << 8));
    if r == OK {
        write_volatile(addr_of_mut!(PD_ACTIVE), true);
    }
    // the adapter may have created the folder, also when the BEGIN failed
    write_volatile(addr_of_mut!(PD_STAGE), true);
    write_volatile(addr_of_mut!(PD_STAGE_AT), now());
    pd_leave();
    if r != OK {
        u.kind = NONE;
        u.pd = false;
        return busy(r);
    }
    u.last = now();
    u.handle
}

unsafe fn pd_write(owner: u8, h: i32, offset: u32, buf: *const u8, len: u32) -> i32 {
    if !mine(SDK, owner, h) {
        return E_STATE;
    }
    let u = up();
    if offset != u.next {
        return E_OFFSET;
    }
    let end = offset as u64 + len as u64;
    if end > u.size as u64 || (len & 511 != 0 && end != u.size as u64) {
        return E_LENGTH;
    }
    if len == 0 {
        return OK;
    }
    if !pd_enter() {
        return busy(E_BUSY);
    }
    let r = dkpw_write(buf, len);
    pd_leave();
    if r == OK {
        u.last = now();
        u.next = end as u32;
    }
    r
}

unsafe fn pd_close(owner: u8, h: i32, out: *mut u8) -> i32 {
    if !mine(SDK, owner, h) {
        return E_STATE;
    }
    if !pd_enter() {
        return busy(E_BUSY);
    }
    let u = up();
    let r = if u.next != u.size {
        dkpw_abort();
        E_LENGTH
    } else {
        dkpw_close(out)
    };
    write_volatile(addr_of_mut!(PD_ACTIVE), false);
    write_volatile(addr_of_mut!(PD_STAGE_AT), now());
    pd_leave();
    u.kind = NONE;
    u.pd = false;
    if r != OK {
        // a failed close can leave a journal and a staged file (a rename that failed after the destination was deleted): the UI pass runs the
        // recovery again before the next file is accepted
        write_volatile(addr_of_mut!(PD_RECOVERED), false);
    }
    r
}

unsafe fn pd_abort(owner: u8, h: i32) -> i32 {
    if !mine(SDK, owner, h) {
        return E_STATE;
    }
    if !pd_enter() {
        return busy(E_BUSY);
    }
    dkpw_abort();
    write_volatile(addr_of_mut!(PD_ACTIVE), false);
    write_volatile(addr_of_mut!(PD_STAGE_AT), now());
    pd_leave();
    let u = up();
    u.kind = NONE;
    u.pd = false;
    OK
}

unsafe fn pd_delete(path: *const u8) -> i32 {
    if !pd_present() {
        return E_CONTRACT;
    }
    if is_open() || !read_volatile(addr_of!(PD_RECOVERED)) || !pd_enter() {
        return busy(E_BUSY);
    }
    let r = dkpw_delete(path);
    pd_leave();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    unsafe fn open_session(kind: u8, last: u32) {
        let u = up();
        u.kind = kind;
        u.owner = 3;
        u.last = last;
        TEST_NOW = last;
        write_volatile(addr_of_mut!(DKRT_SESSION_IDLE), 0);
        unlock();
    }

    #[test]
    fn an_idle_session_expires_after_20_seconds_of_block_clock_and_not_before() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            open_session(SYSEX, 1000);
            TEST_NOW = 1000 + 20 * 1500 - 1;
            tick();
            assert!(is_open(), "19.999 s: still open");
            TEST_NOW = 1000 + 20 * 1500;
            tick();
            assert!(!is_open(), "20 s: aborted");
            open_session(SDK, 5);
            TEST_NOW = 5 + 20 * 1500;
            tick();
            assert!(!is_open(), "an SDK session expires too");
        }
    }

    #[test]
    fn a_session_is_not_dropped_while_the_store_lock_is_held() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            open_session(SDK, 0);
            TEST_NOW = 100_000;
            assert!(try_lock());
            tick();
            assert!(is_open(), "a write is in progress: leave the session alone");
            unlock();
            tick();
            assert!(!is_open());
        }
    }

    #[test]
    fn the_idle_knob_overrides_the_default() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            open_session(SYSEX, 0);
            write_volatile(addr_of_mut!(DKRT_SESSION_IDLE), 10);
            TEST_NOW = 10;
            tick();
            assert!(!is_open());
            write_volatile(addr_of_mut!(DKRT_SESSION_IDLE), 0);
        }
    }

    /// A fake card: sectors of `data` from sector 100; `fail_at` makes a read of that sector fail.
    fn fake(data: &'static [u8], fail_at: Option<u32>) -> impl FnMut(u32, u32, *mut u8) -> i32 {
        move |sector, bytes, p| unsafe {
            if fail_at.map_or(false, |f| sector <= f && f < sector + bytes / 512) {
                return 1;
            }
            let off = (sector - 100) as usize * 512;
            for i in 0..bytes as usize {
                *p.add(i) = *data.get(off + i).unwrap_or(&0);
            }
            0
        }
    }

    #[test]
    fn the_readback_accepts_the_card_bytes_and_refuses_a_flipped_bit_or_a_failed_read() {
        static GOOD: [u8; 100_000] = {
            let mut a = [0u8; 100_000];
            let mut i = 0;
            while i < a.len() {
                a[i] = (i * 7 + 3) as u8;
                i += 1;
            }
            a
        };
        static BAD: [u8; 100_000] = {
            let mut a = GOOD;
            a[99_999] ^= 1; // the last byte, in the partial last sector
            a
        };
        static BAD_MID: [u8; 100_000] = {
            let mut a = GOOD;
            a[40_000] ^= 0x80; // in the second piece of a 32 KiB read-back
            a
        };
        #[repr(C, align(16))]
        struct Buf([u8; 32768]);
        let mut buf = Buf([0; 32768]);
        let want = hash32::slot(unsafe {
            let mut h = Hash32::new();
            h.update(GOOD.as_ptr(), GOOD.len() as u32);
            h.finish()
        });
        unsafe {
            let p = buf.0.as_mut_ptr();
            for cap in [32768usize, 16384, 512] {
                assert_eq!(readback(fake(&GOOD, None), 100, GOOD.len() as u32, p, cap, &want), OK, "cap {cap}");
            }
            assert_eq!(readback(fake(&BAD, None), 100, GOOD.len() as u32, p, 32768, &want), E_HASH);
            assert_eq!(readback(fake(&BAD_MID, None), 100, GOOD.len() as u32, p, 16384, &want), E_HASH);
            assert_eq!(readback(fake(&GOOD, Some(150)), 100, GOOD.len() as u32, p, 32768, &want), E_READ);
            // a size in whole sectors, and one byte
            let w1 = hash32::slot({
                let mut h = Hash32::new();
                h.update(GOOD.as_ptr(), 1);
                h.finish()
            });
            assert_eq!(readback(fake(&GOOD, None), 100, 1, p, 32768, &w1), OK);
            assert_eq!(readback(fake(&GOOD, None), 100, 1, p, 32768, &want), E_HASH);
            // two buffers (the hash of one piece overlaps the read of the next): the same slot, the same refusals, a bad read still returns
            assert_eq!(readback_hash(fake(&GOOD, None), 100, GOOD.len() as u32, p, 16384, true), Ok(want));
            assert_eq!(readback_hash(fake(&GOOD, None), 100, GOOD.len() as u32, p, 512, true), Ok(want));
            assert_ne!(readback_hash(fake(&BAD_MID, None), 100, GOOD.len() as u32, p, 16384, true), Ok(want));
            assert_eq!(readback_hash(fake(&GOOD, Some(150)), 100, GOOD.len() as u32, p, 16384, true), Err(E_READ));
        }
    }

    fn file_entry(path: &str, first: u32, size: u32) -> [u8; store::ENTRY] {
        let mut ent = [0u8; store::ENTRY];
        let mut p = [0u8; 160];
        p[..path.len()].copy_from_slice(path.as_bytes());
        let e = Entry { first, size, kind: KIND_FILE, sha: [first as u8; 32] };
        unsafe { store::encode_entry(p.as_ptr(), &e, ent.as_mut_ptr()) };
        ent
    }

    fn put(path: &str, first: u32, size: u32, owner: u8) {
        let p = pf();
        p.ent[p.n] = file_entry(path, first, size);
        p.owner[p.n] = owner;
        p.n += 1;
    }

    fn cpath(s: &str) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[..s.len()].copy_from_slice(s.as_bytes());
        b
    }

    #[test]
    fn pending_files_keep_their_extents_out_and_count_for_their_owner() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            put("samples/a", 500, 1024, 3);
            put("samples/b", 600, 512, 4);
            put("samples/c", 700, 0, 3); // no data: no extent
            assert_eq!((pend_count(3), pend_count(4), pend_count(9)), (2, 1, 0));
            assert_eq!(pf().sectors(), 3);
            assert_eq!(pf().bytes(), 1536);
            let mut ex = [(0, 0); EXTRA_MAX];
            ex[0] = (900, 4);
            let n = pend_extents(&mut ex, 1);
            let got = &ex[..n];
            assert_eq!(got, &[(900, 4), (500, 2), (600, 1)], "the file without data has no extent");
            // a path is found by `pend_remove_path` and its slot closes
            let p = cpath("samples/b");
            assert!(pend_remove_path(p.as_ptr()));
            assert!(!pend_remove_path(p.as_ptr()));
            assert_eq!((pf().n, pend_count(4)), (2, 0));
            pend_clear();
            assert_eq!(pf().n, 0);
        }
    }

    #[test]
    fn a_pending_replace_takes_the_slot_of_its_path() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            TEST_NOW = 10;
            assert_eq!(pend_add(3, 7, &file_entry("samples/a", 500, 512)), OK);
            assert_eq!(pend_add(3, 7, &file_entry("samples/b", 600, 512)), OK);
            assert_eq!(pend_add(3, 8, &file_entry("samples/a", 800, 1024)), OK);
            assert_eq!(pf().n, 2);
            let a = store::entry_of(pf().ent[1].as_ptr());
            assert_eq!((a.first, a.size), (800, 1024), "the new file of the path is the pending one");
            assert_eq!((pf().job, pf().last), (8, 10));
            pend_clear();
        }
    }

    #[test]
    fn pending_files_are_due_when_the_owner_is_idle_for_a_second_or_its_job_ends_and_never_dropped_by_the_session_idle() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            put("samples/a", 500, 512, 3);
            let p = pf();
            p.last = 100;
            p.job = 7;
            p.posted = false;
            p.retrying = false;
            assert!(!pend_due(p, 100 + PENDING_IDLE - 1), "999 ms after the last call");
            assert!(pend_due(p, 100 + PENDING_IDLE), "1 s");
            // the job ends: due at once
            end_job(9);
            assert!(!pend_due(pf(), 101), "another job");
            end_job(7);
            assert!(pend_due(pf(), 101));
            // an error waits before the next try; a posted job is not posted twice
            pf().retrying = true;
            pf().retry_at = 5000;
            assert!(!pend_due(pf(), 4999) && pend_due(pf(), 5000));
            pf().posted = true;
            assert!(!pend_due(pf(), 9999));
            pf().posted = false;
            // the 20 s idle drop of a write session leaves the pending files alone
            open_session(SDK, 0);
            TEST_NOW = 100_000;
            tick();
            assert!(!is_open());
            assert_eq!(pf().n, 1);
            pend_clear();
            pf().retrying = false;
        }
    }

    #[test]
    fn a_module_that_goes_gets_its_pending_files_flushed() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            put("samples/a", 500, 512, 3);
            pf().last = 0;
            retire(4);
            assert!(!pf().flush, "another module's");
            retire(3);
            assert!(pf().flush);
            pend_clear();
            assert!(!pf().flush);
        }
    }

    #[test]
    fn the_sdk_accepts_the_defer_flag_and_nothing_else() {
        assert_eq!(FILE_DEFER & FILE_REPLACE, 0);
        assert_eq!((FILE_REPLACE | FILE_DEFER) & !(FILE_REPLACE | FILE_DEFER), 0);
        assert_eq!(PENDING_MAX, 16);
    }

    #[test]
    fn a_deferred_create_commits_first_only_at_the_limits() {
        assert!(!room_needed(0, 0, 100 * 1024 * 1024), "nothing pending: nothing to commit, whatever the size");
        assert!(!room_needed(15, 1000, 1000));
        assert!(room_needed(16, 0, 1), "16 files pending");
        assert!(!room_needed(3, PENDING_BYTES_MAX - 10, 10), "exactly 16 MiB is allowed");
        assert!(room_needed(3, PENDING_BYTES_MAX - 10, 11), "this file would pass 16 MiB");
    }

    #[test]
    fn free_bytes_leaves_room_for_the_pending_entries_and_their_data() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            crate::store::TEST_LAYOUT.with(|c| c.set(Some(Layout { first: 100, sectors: 100_000 })));
            let h = Hdr { count: 0, table_sectors: 0, table_first: 0, table_crc: 0, seq: 1, used: 1, area: 100_000 };
            let none = free_bytes(&h);
            put("samples/a", 500, 1024, 3); // 2 sectors of data, one more table entry
            put("samples/b", 600, 512, 3);
            let two = free_bytes(&h);
            let room = |n: u32| store::table_sectors(n);
            let drop = (3 + room(3) - room(1)) as u64 * 512; // data sectors and the table's growth
            assert_eq!(none as u64 - two as u64, drop);
            pend_clear();
            crate::store::TEST_LAYOUT.with(|c| c.set(None));
        }
    }

    #[test]
    fn a_pending_path_is_found_without_being_removed() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            pend_clear();
            put("samples/a", 500, 512, 3);
            let p = cpath("samples/a");
            assert!(pend_has_path(p.as_ptr()));
            assert_eq!(pf().n, 1, "looking does not drop it (delete drops only after its checks)");
            assert!(pend_remove_path(p.as_ptr()));
            assert!(!pend_has_path(p.as_ptr()));
            pend_clear();
        }
    }

    #[test]
    fn a_stale_or_foreign_handle_is_refused() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            open_session(SDK, 0);
            up().handle = 7;
            assert_eq!(abort(SDK, 4, 7), E_STATE, "another module's handle");
            assert_eq!(abort(SDK, 3, 8), E_STATE, "a stale handle");
            assert!(is_open());
            assert_eq!(abort(SDK, 3, 7), OK);
            assert_eq!(abort(SDK, 3, 7), E_STATE, "a closed handle");
        }
    }
}
