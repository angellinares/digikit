//! Per-module stereo sample pool of 16 entries (API 1.5, docs/design/sample-api-1.5.md). The state machine is
//! `dkrt_sdk::sample_tracks` (device-neutral, host-tested); this file binds it to the runtime:
//! the Api entries `sample_pool_add`, `_clear`, `_status`, `_info` and (1.6) `_path`, the optional `dkpool_*` backing symbols (plug-in point `dkrt_pool.h`, version 1)
//! and the events adapters call (`dkrt_pool_done`, `dkrt_pool_lost`, `dkrt_project_changed`).
//! No firmware address and no `cf.*` anchor is used here. Without the backing adapter every
//! `sample_pool_add` returns E_CONTRACT.
//!
//! The track table of a module travels in its .dkm DSP section (version 5, record kind 2: the byte offsets of the 16 `dk_strack`
//! records and the 16 control sets in the module's DSP data). `dsp::reserve` calls `reserve` with their L2 byte addresses and
//! `ready` when the DSP image is in; before that no DSP write is made.
//!
//! The DSP link is write-only, so close, drain and publish are queued writes, one in flight at a time (`pump`), with the same link
//! discipline as the 1.3 bank job (`sample.rs`): a start needs an idle loader and no bank, fx page or fx send transfer, and the bank
//! tick waits while a table write is in flight (`busy`).
//!   close:   control CLOSED = 1, then record `ready` = 0. The DSP holds a track only between ACQUIRE and RELEASE inside one `render`
//!            call, so a render that was inside when CLOSED landed ends within that block. The second write lands in a later block than
//!            the first (one transfer at a time, each acknowledged), so its acknowledgement drains the track: no COUNT read-back
//!            exists, and the 1.3 bank never closes, so it never drains.
//!   publish: record words 1..7 (token, frames, rate, channels, fmt, base_l, base_r), control CLOSED = 0, then `ready` = 1 last.
//! A write the link refuses is retried; after `TRIES` failures the op stays failed (a close then never drains and the generic drain
//! guard of `sample_tracks` ends the request; a publish stays unpublished, `ready` = 0, the track plays silence).
//! A sticky DSP FAULT (poisoned COUNT, CLOSED = 1) is never cleared by the runtime: that track stays silent until the module reloads.
//!
//! TODO (not wired yet): persistence of the selections to the digikit store (they live in memory per project now); a request mailbox
//! so API calls from the MIDI task cannot interleave with a tick step of the same track (an idle track is safe today).
use crate::{dsp, irq_mask, irq_restore, sample};
use core::hint::black_box;
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::files::{HASH_ELEKTRON32, SAMPLES};
use dkrt_sdk::sample_tracks::{
    rooted, store_identity, store_inspect, unrooted, Core, Env, FileId, Handle, Inspect, Load, Strack, ID_HASH_VALID, ROOT_DN2, ROOT_DT2, TRACKS,
};
use dkrt_sdk::{E_CONTRACT, E_DSP_XFER, E_NOT_FOUND, E_READ, E_SEL_PATH};

pub const DKPOOL_VERSION: u32 = 1;
const CLIENTS: usize = 2;
const ENTRIES: usize = 16;
const PROJECTS: usize = 4;

extern "C" {
    // Optional symbols of the sample-pool adapter (`[object] optional` in module.toml): 0 when it is not in the image.
    fn dkpool_version() -> u32;
    fn dkpool_load(req: u32, id: *const FileId, path: *const u8, out: *mut Handle) -> i32;
    fn dkpool_release(h: *const Handle);
    fn dkpool_check(h: *const Handle) -> i32;
    fn dkpool_free_frames() -> u32;
    fn dkpool_tick();
    // Optional growth of plug-in point version 1 (dkrt_pool.h): the runtime tests each for 0.
    fn dkpool_cancel(req: u32);
    // Per-device tables of the runtime's assembly objects (DT2 1.17 anchors; zeros on DN2). Both are declared in sample_source.rs and files.rs too.
    static dkrt_sample_fs: [u32; 4];
    static dkrt_files_fs: [u32; 3];
}

static mut CORE: Core<CLIENTS, ENTRIES, PROJECTS> = Core::new();
/// Module record of the owner of each client slot (`usize::MAX` = free), its local id, the L2 byte addresses of its 16 records and 16
/// control sets, and whether its DSP image is in (table writes may start).
static mut OWNER: [usize; CLIENTS] = [usize::MAX; CLIENTS];
static mut LOCAL: [u32; CLIENTS] = [0; CLIENTS];
static mut RECORDS: [u32; CLIENTS] = [0; CLIENTS];
static mut CONTROL: [u32; CLIENTS] = [0; CLIENTS];
static mut LIVE: [bool; CLIENTS] = [false; CLIENTS];
/// The client slot of a module that is taken down (`usize::MAX`: none): nothing reads its tracks any more, so close drains at once and
/// writes are dropped. Other clients are stepped normally meanwhile.
static mut FORCE: usize = usize::MAX;
/// 0 not checked, 1 usable, 2 absent or wrong version.
static mut POOL: u8 = 0;

unsafe fn core_() -> &'static mut Core<CLIENTS, ENTRIES, PROJECTS> {
    &mut *addr_of_mut!(CORE)
}

/// Is a backing adapter of the right version in the image?
unsafe fn pool_ok() -> bool {
    if POOL == 0 {
        let present = black_box(dkpool_version as *const () as usize) != 0
            && black_box(dkpool_load as *const () as usize) != 0
            && black_box(dkpool_release as *const () as usize) != 0
            && black_box(dkpool_check as *const () as usize) != 0
            && black_box(dkpool_free_frames as *const () as usize) != 0;
        POOL = if present && dkpool_version() == DKPOOL_VERSION { 1 } else { 2 };
    }
    POOL == 1
}

// ---- table writes over the DSP link -------------------------------------------------------------------------------------------

const OP_NONE: u8 = 0;
const OP_CLOSE: u8 = 1;
const OP_PUBLISH: u8 = 2;
const OP_FAILED: u8 = 3;
/// Refusals of one write before the op is given up.
const TRIES: u8 = 8;
const NO_FLIGHT: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Op {
    kind: u8,
    step: u8,
    tries: u8,
    /// The record to publish (`Strack` words in order).
    rec: [u32; 8],
}
const OP0: Op = Op { kind: OP_NONE, step: 0, tries: 0, rec: [0; 8] };
static mut OPS: [[Op; TRACKS]; CLIENTS] = [[OP0; TRACKS]; CLIENTS];
/// The bytes of the write in flight (little-endian DSP words; the link reads them in place) and its track: client * TRACKS + track.
static mut STAGE: [u8; 28] = [0; 28];
static mut FLIGHT: usize = NO_FLIGHT;

/// A table write is in flight: the bank job and the fx word writers wait (one transfer on the link at a time).
pub fn busy() -> bool {
    unsafe { FLIGHT != NO_FLIGHT }
}

/// Little-endian bytes of `words` into `STAGE`; the byte count.
unsafe fn stage(words: &[u32]) -> u32 {
    let mut i = 0;
    while i < words.len() {
        let b = words[i].to_le_bytes();
        let st = &mut *addr_of_mut!(STAGE);
        st[i * 4] = b[0];
        st[i * 4 + 1] = b[1];
        st[i * 4 + 2] = b[2];
        st[i * 4 + 3] = b[3];
        i += 1;
    }
    (words.len() * 4) as u32
}

/// Has the op written everything?
fn complete(op: &Op) -> bool {
    (op.kind == OP_CLOSE && op.step >= 2) || (op.kind == OP_PUBLISH && op.step >= 3)
}

/// The next write of an op: (destination L2 byte address, byte count), the bytes in `STAGE`. None when the op is complete.
/// If chains, not a `match`: dense arms become a PC-relative jump table, which is not ColdFire code.
unsafe fn next_write(c: usize, k: usize, op: &Op) -> Option<(u32, u32)> {
    let rec = RECORDS[c] + 32 * k as u32;
    let ctl = CONTROL[c] + 16 * k as u32;
    if op.kind == OP_CLOSE {
        if op.step == 0 {
            return Some((ctl, stage(&[1])));
        }
        if op.step == 1 {
            return Some((rec, stage(&[0])));
        }
    } else if op.kind == OP_PUBLISH {
        if op.step == 0 {
            return Some((rec + 4, stage(&op.rec[1..8])));
        }
        if op.step == 1 {
            return Some((ctl, stage(&[0])));
        }
        if op.step == 2 {
            return Some((rec, stage(&[op.rec[0]])));
        }
    }
    None
}

/// Finish the write in flight, then start the next one. Called twice per UI-loop pass.
unsafe fn pump() {
    if FLIGHT != NO_FLIGHT {
        let done = dsp::sample_poll();
        if done.is_none() {
            return;
        }
        let (c, k) = (FLIGHT / TRACKS, FLIGHT % TRACKS);
        FLIGHT = NO_FLIGHT;
        let op = &mut (*addr_of_mut!(OPS))[c][k];
        if done == Some(true) {
            op.step += 1;
            op.tries = 0;
        } else {
            op.tries += 1;
        }
    }
    let mut c = 0;
    while c < CLIENTS {
        let mut k = 0;
        while k < TRACKS {
            let op = &mut (*addr_of_mut!(OPS))[c][k];
            if op.kind == OP_CLOSE || op.kind == OP_PUBLISH {
                if op.tries >= TRIES {
                    op.kind = OP_FAILED;
                } else if complete(op) {
                    op.kind = OP_NONE;
                }
            }
            k += 1;
        }
        c += 1;
    }
    if !dsp::sample_idle() || crate::fxpage::busy() || crate::fxsend::busy() || sample::busy() {
        return;
    }
    c = 0;
    while c < CLIENTS {
        let mut k = 0;
        while k < TRACKS {
            let op = (*addr_of!(OPS))[c][k];
            if (op.kind == OP_CLOSE || op.kind == OP_PUBLISH) && LIVE[c] {
                if let Some((dst, len)) = next_write(c, k, &op) {
                    let r = dsp::sample_send(dst, addr_of!(STAGE) as *const u8, len);
                    if r == 0 {
                        FLIGHT = c * TRACKS + k;
                    } else if r == -2 {
                        (*addr_of_mut!(OPS))[c][k].tries += 1;
                    }
                    return;
                }
            }
            k += 1;
        }
        c += 1;
    }
}

// ---- native file header and identity (DT2) --------------------------------------------------------------------------------

type FsOpen = unsafe extern "C" fn(*const u8, *const u8, *mut u32) -> i32;
type FsRead = unsafe extern "C" fn(*mut u8, u32, *mut u32) -> i32;
type FsClose = unsafe extern "C" fn(*mut u32) -> i32;
type FsSize = unsafe extern "C" fn(u32) -> u32;
type FsMeta = unsafe extern "C" fn(u32, *mut u32) -> i32;

/// The device's sample root (`select` paths are root-relative; the runtime keeps the rooted form).
fn root() -> &'static [u8] {
    if crate::device() == 2 {
        ROOT_DN2
    } else {
        ROOT_DT2
    }
}

/// DN2: a `samples/...` file of the digikit store. Identity is its size and the sha256 of its table entry (the listing row's), and `head` is
/// the first 64 bytes of the file. `path` is not NUL-terminated.
unsafe fn probe_store(path: &[u8], head: Option<&mut [u8; 64]>) -> Result<FileId, i32> {
    if path.is_empty() || path.len() >= dkrt_sdk::sample_tracks::PATH {
        return Err(E_SEL_PATH);
    }
    let mut z = [0u8; dkrt_sdk::sample_tracks::PATH + 1];
    z[..path.len()].copy_from_slice(path);
    let f = crate::store::find(z.as_ptr())?;
    if let Some(h) = head {
        crate::store::read_head(&f, h)?;
    }
    Ok(store_identity(f.size, &f.sha))
}

/// Open `path` (not NUL-terminated), take the identity of its listing row (size and the stored hash by entry id, as `files.rs` lists it)
/// and, when `head` is given, read the 64 header bytes. DT2 only: the store route of DN2 is `probe_store`.
unsafe fn probe(path: &[u8], head: Option<&mut [u8; 64]>) -> Result<FileId, i32> {
    if crate::device() == 2 {
        return probe_store(path, head);
    }
    if crate::device() != 1 {
        return Err(E_CONTRACT);
    }
    if path.is_empty() || path.len() >= dkrt_sdk::sample_tracks::PATH {
        return Err(E_SEL_PATH);
    }
    let mut z = [0u8; dkrt_sdk::sample_tracks::PATH + 1];
    z[..path.len()].copy_from_slice(path);
    let table = addr_of!(dkrt_sample_fs) as *const u32;
    let (open_addr, read_addr, close_addr, size_addr) =
        (read_volatile(table), read_volatile(table.add(1)), read_volatile(table.add(2)), read_volatile(table.add(3)));
    let meta_addr = read_volatile((addr_of!(dkrt_files_fs) as *const u32).add(2));
    if open_addr == 0 || read_addr == 0 || close_addr == 0 || size_addr == 0 {
        return Err(E_NOT_FOUND);
    }
    let open: FsOpen = core::mem::transmute(open_addr as usize);
    let read: FsRead = core::mem::transmute(read_addr as usize);
    let close: FsClose = core::mem::transmute(close_addr as usize);
    let size_of: FsSize = core::mem::transmute(size_addr as usize);
    let mut handle = [0u32; 4];
    if open(z.as_ptr(), b"r\0".as_ptr(), handle.as_mut_ptr()) != 0 {
        return Err(E_NOT_FOUND);
    }
    let result = (|| {
        let size = size_of(handle[0]);
        if size == u32::MAX {
            return Err(E_READ);
        }
        if let Some(h) = head {
            if size < 64 || read(h.as_mut_ptr(), 64, handle.as_mut_ptr()) != 64 {
                return Err(E_READ);
            }
        }
        let mut id = FileId { root: SAMPLES, size, mtime: 0, flags: 0, hash_kind: 0, hash: [0; 32] };
        // The stored hash of the entry (words: id, odd hash, physical size, node). No mtime exists on DT2: without a hash the identity is size and path.
        if meta_addr != 0 {
            let meta: FsMeta = core::mem::transmute(meta_addr as usize);
            let mut values = [0u32; 4];
            if meta(handle[0], values.as_mut_ptr()) == 0 && values[2] == size {
                id.flags = ID_HASH_VALID;
                id.hash_kind = HASH_ELEKTRON32;
                id.hash[..4].copy_from_slice(&values[1].to_be_bytes());
            }
        }
        Ok(id)
    })();
    if close(handle.as_mut_ptr()) != 0 {
        return Err(E_READ);
    }
    result
}

struct Device;
impl Env for Device {
    fn inspect(&mut self, path: &[u8]) -> Result<Inspect, i32> {
        let mut head = [0u8; 64];
        let id = unsafe { probe(path, Some(&mut head))? };
        // Size and format only: no PCM is read here (it can be megabytes), the pool adapter reads it.
        if crate::device() == 2 {
            return store_inspect(id.size, &id.hash, &head);
        }
        let (frames, channels) = dkrt_sdk::sample::native_header(&head, id.size)?;
        Ok(Inspect { id, frames, rate: dkrt_sdk::sample::RATE, channels })
    }
    fn stat(&mut self, path: &[u8]) -> Result<FileId, i32> {
        unsafe { probe(path, None) }
    }
    fn load(&mut self, req: u32, id: &FileId, path: &[u8], out: &mut Handle) -> Load {
        // The adapter gets a NUL-terminated copy: the runtime's path buffers are not.
        let mut z = [0u8; dkrt_sdk::sample_tracks::PATH + 1];
        z[..path.len()].copy_from_slice(path);
        match unsafe { dkpool_load(req, id, z.as_ptr(), out) } {
            0 => Load::Done,
            1 => Load::Queued,
            e => Load::Err(e),
        }
    }
    fn release(&mut self, h: &Handle) {
        unsafe { dkpool_release(h) }
    }
    fn cancel(&mut self, req: u32) {
        unsafe {
            if black_box(dkpool_cancel as *const () as usize) != 0 {
                dkpool_cancel(req);
            }
        }
    }
    fn check(&mut self, h: &Handle) -> i32 {
        unsafe { dkpool_check(h) }
    }
    fn free_frames(&mut self) -> u32 {
        unsafe { dkpool_free_frames() }
    }
    fn close(&mut self, client: usize, track: usize) {
        unsafe {
            let op = &mut (*addr_of_mut!(OPS))[client][track];
            // A publish still queued is dropped: the close writes CLOSED and `ready` = 0 anyway. Before the DSP image is in there is
            // nothing to close.
            *op = OP0;
            if LIVE[client] && FORCE != client {
                op.kind = OP_CLOSE;
            }
        }
    }
    fn drained(&mut self, client: usize, track: usize) -> bool {
        unsafe { FORCE == client || (*addr_of!(OPS))[client][track].kind == OP_NONE }
    }
    fn published(&mut self, client: usize, track: usize) -> i32 {
        unsafe {
            let kind = (*addr_of!(OPS))[client][track].kind;
            if kind == OP_FAILED {
                return E_DSP_XFER;
            }
            if kind == OP_NONE {
                1
            } else {
                0
            }
        }
    }
    fn publish(&mut self, client: usize, track: usize, rec: &Strack) {
        unsafe {
            if FORCE == client {
                return;
            }
            let op = &mut (*addr_of_mut!(OPS))[client][track];
            *op = OP0;
            op.kind = OP_PUBLISH;
            op.rec = [rec.ready, rec.token, rec.frames, rec.rate, rec.channels, rec.fmt, rec.base_l, rec.base_r];
        }
    }
}

/// The client slot of the calling module, if it has a track table.
unsafe fn client(handle: u32) -> Result<usize, i32> {
    let h = handle as usize;
    if h == 0 || h > CLIENTS || !core_().is_open(h - 1) {
        return Err(E_NOT_FOUND);
    }
    Ok(h - 1)
}

/// Is a client slot free (checked by `dsp::reserve` before it takes anything)?
pub unsafe fn can_reserve() -> bool {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == usize::MAX {
            return true;
        }
        i += 1;
    }
    false
}

/// A module's track table was found in its DSP section: `records` and `control` are the L2 byte addresses of its 16 records and 16
/// control sets. Nothing is written to the DSP before `ready`.
pub unsafe fn reserve(rec: usize, local_id: u32, records: u32, control: u32) -> bool {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == usize::MAX && core_().open(i) {
            OWNER[i] = rec;
            LOCAL[i] = local_id;
            RECORDS[i] = records;
            CONTROL[i] = control;
            LIVE[i] = false;
            (*addr_of_mut!(OPS))[i] = [OP0; TRACKS];
            return true;
        }
        i += 1;
    }
    false
}

/// The module's DSP image is in, so its table words exist: queued writes may go out.
pub unsafe fn ready(rec: usize) {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec {
            LIVE[i] = true;
        }
        i += 1;
    }
}

/// A module's record went away (its machine rows are already cleared, so no voice runs): unbind its tracks without closing them in
/// the DSP, release the pool references and free the client slot. A write already in flight ends by itself (its bytes are static, and
/// the link is serialized, so a later load of the range lands after it).
pub unsafe fn remove(rec: usize) {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec {
            FORCE = i;
            let mut n = 0;
            while n < 4 && !core_().teardown(i, &mut Device) {
                n += 1;
            }
            FORCE = usize::MAX;
            (*addr_of_mut!(OPS))[i] = [OP0; TRACKS];
            LIVE[i] = false;
            OWNER[i] = usize::MAX;
        }
        i += 1;
    }
}

/// Does module record `rec` hold a track table (the bank API refuses it then)?
pub unsafe fn has(rec: usize) -> bool {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec {
            return true;
        }
        i += 1;
    }
    false
}

pub unsafe fn tracks(local_id: u32) -> i32 {
    let owner = crate::dlog::current_source();
    if owner == 0 {
        return E_NOT_FOUND;
    }
    let rec = owner as usize - 1;
    if sample::has_bank(rec) {
        return E_CONTRACT;
    }
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec && LOCAL[i] == local_id {
            return i as i32 + 1;
        }
        i += 1;
    }
    E_NOT_FOUND
}

/// `sample_pool_add`: the pool index of the file (a new entry, or the one that already holds the path), or a negative error.
pub unsafe fn pool_add(handle: u32, path: *const u8) -> i32 {
    if path.is_null() {
        return E_SEL_PATH;
    }
    if !pool_ok() {
        return E_CONTRACT;
    }
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut copy = [0u8; dkrt_sdk::sample_tracks::PATH];
    let mut n = 0;
    while n < copy.len() {
        let v = *path.add(n);
        if v == 0 {
            break;
        }
        copy[n] = v;
        n += 1;
    }
    if n == copy.len() {
        return E_SEL_PATH;
    }
    // The path is root-relative (a listing row); the runtime keeps and checks the device-rooted form. A DT2 path that starts with '/' stays.
    let mut full = [0u8; dkrt_sdk::sample_tracks::PATH];
    let n = match rooted(root(), &copy[..n], &mut full) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let sr = irq_mask();
    let r = core_().pool_add(c, &full[..n]);
    irq_restore(sr);
    r
}

/// `sample_pool_clear`: the entry becomes UNUSED (a hole); the other indexes stay.
pub unsafe fn pool_clear(handle: u32, track: u32) -> i32 {
    match client(handle) {
        Ok(c) => {
            let sr = irq_mask();
            let r = core_().unbind(c, track as usize);
            irq_restore(sr);
            r
        }
        Err(e) => e,
    }
}

pub unsafe fn status(handle: u32, track: u32) -> i32 {
    match client(handle) {
        Ok(c) => core_().status(c, track as usize),
        Err(e) => e,
    }
}

/// `sample_pool_path`: the entry's current selection, root-relative, NUL-terminated, into `buf`; the length without the NUL.
pub unsafe fn path(handle: u32, track: u32, buf: *mut u8, cap: u32) -> i32 {
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if buf.is_null() {
        return E_SEL_PATH;
    }
    match core_().path(c, track as usize) {
        Ok(stored) => {
            let p = unrooted(root(), stored);
            if p.len() + 1 > cap as usize {
                return E_SEL_PATH;
            }
            core::ptr::copy_nonoverlapping(p.as_ptr(), buf, p.len());
            *buf.add(p.len()) = 0;
            p.len() as i32
        }
        Err(e) => e,
    }
}

/// Copies a 16-byte `dk_sample_info` (big-endian words, as the module sees them) when the track is READY.
pub unsafe fn info(handle: u32, track: u32, out: *mut u8) -> i32 {
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if out.is_null() {
        return E_NOT_FOUND;
    }
    match core_().info(c, track as usize) {
        Ok(i) => {
            let w = [i.frames, i.rate, i.channels, i.token];
            let mut k = 0;
            while k < 4 {
                core::ptr::copy_nonoverlapping(w[k].to_be_bytes().as_ptr(), out.add(k * 4), 4);
                k += 1;
            }
            0
        }
        Err(e) => e,
    }
}

/// One step of every track, in the UI loop. Without a backing adapter nothing runs.
pub unsafe fn tick() {
    if POOL == 2 || !pool_ok() {
        return;
    }
    // Optional on its own: the adapter delivers completions and finds a stock reset here, before the runtime steps the tracks.
    if black_box(dkpool_tick as *const () as usize) != 0 {
        dkpool_tick();
    }
    pump();
    core_().tick(&mut Device);
    pump();
}

/// Adapter to runtime: load `req` finished (`result` 0, E_POOL, E_READ or E_FORMAT).
#[no_mangle]
pub unsafe extern "C" fn dkrt_pool_done(req: u32, result: i32) {
    core_().done(req, result);
}
/// Adapter to runtime: the backing was lost, all handles are invalid.
#[no_mangle]
pub unsafe extern "C" fn dkrt_pool_lost() {
    core_().lost(&mut Device);
}
/// Project adapter to runtime: the current project slot changed.
#[no_mangle]
pub unsafe extern "C" fn dkrt_project_changed(slot: u32) {
    core_().project_changed(slot);
}
