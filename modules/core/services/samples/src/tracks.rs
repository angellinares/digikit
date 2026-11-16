//! Per-module stereo sample pool of 16 entries (docs/design/sample-api-1.5.md). The state machine is
//! `dkrt_sdk::sample_tracks` (device-neutral, host-tested); this file binds it to the runtime:
//! the imports `dk_sample_pool_add`, `_set`, `_clear`, `_status`, `_info` and `_path`, the optional `dkpool_*` backing symbols (plug-in point `dkrt_pool.h`, version 1)
//! and the events adapters call (`dkrt_pool_done`, `dkrt_pool_lost`, `dkrt_project_changed`).
//! No firmware address and no `cf.*` anchor is used here. Without the backing adapter every
//! `sample_pool_add` returns E_CONTRACT.
//!
//! The track table of a module travels in its .dkm DSP section (version 5, record kind 2: the byte offsets of the 16 `dk_sample_pool_entry`
//! records and the 16 control sets in the module's DSP data). `dsp::reserve` calls `reserve` with their L2 byte addresses and
//! `ready` when the DSP image is in; before that no DSP write is made.
//!
//! The DSP link is write-only, so close, drain and publish are queued writes, one in flight at a time (`pump`): each write is a request of the owner `Tracks` on the
//! core's link queue (its bytes are copied there), and a start needs an idle loader.
//!   close:   control CLOSED = 1, then record `ready` = 0. The DSP holds a track only between ACQUIRE and RELEASE inside one `render`
//!            call, so a render that was inside when CLOSED landed ends within that block. The second write lands in a later block than
//!            the first (one transfer at a time, each acknowledged), so its acknowledgement drains the track: no COUNT read-back
//!            exists.
//!   publish: record words 1..7 (token, frames, rate, channels, fmt, base_l, base_r), control CLOSED = 0, then `ready` = 1 last.
//! A write the link refuses is retried; after `TRIES` failures the op stays failed (a close then never drains and the generic drain
//! guard of `sample_tracks` ends the request; a publish stays unpublished, `ready` = 0, the track plays silence).
//! A sticky DSP FAULT (poisoned COUNT, CLOSED = 1) is never cleared by the runtime: that track stays silent until the module reloads.
//!
//! TODO (not wired yet): persistence of the selections to the digikit store (they live in memory per project now); a request mailbox
//! so API calls from the MIDI task cannot interleave with a tick step of the same track (an idle track is safe today).
use core::hint::black_box;
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_abi::core_api::{SampleCore, LOG_INFO};
use dkrt_abi::dsp::PH_IDLE;
use dkrt_abi::link::{LinkResult, Owner, Stream, Ticket};
use dkrt_abi::platform::SampleHost;
use dkrt_abi::storage::{EntryKind, Storage};
use dkrt_sdk::files::SAMPLES;
use dkrt_sdk::sample_tracks::{
    rooted, store_inspect, unrooted, Core, Env, FileId, Handle, Inspect, Load, Note, Strack,
    ID_HASH_VALID, ROOT_DN2, ROOT_DT2, TRACKS,
};
use dkrt_sdk::{logk, E_CONTRACT, E_DSP_XFER, E_NOT_FOUND, E_SEL_PATH};

pub(crate) const CLIENTS: usize = 2;
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
    fn dkpool_peaks(h: *const Handle, out: *mut u8, capacity: u32) -> i32;
    fn dkpool_window_step(
        h: *const Handle,
        path: *const u8,
        first: u32,
        frames: u32,
        columns: u32,
        out: *mut i16,
        cursor: *mut u32,
    ) -> i32;
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

pub(crate) unsafe fn core_() -> &'static mut Core<CLIENTS, ENTRIES, PROJECTS> {
    &mut *addr_of_mut!(CORE)
}

/// Is a backing adapter of the right version in the image? (The platform's sample-pool provider.)
unsafe fn pool_ok<C: SampleCore>(core: &C) -> bool {
    core.host().pool_present()
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
const OP0: Op = Op {
    kind: OP_NONE,
    step: 0,
    tries: 0,
    rec: [0; 8],
};
static mut OPS: [[Op; TRACKS]; CLIENTS] = [[OP0; TRACKS]; CLIENTS];
/// The write in flight: the id of its request and its track, client * TRACKS + track.
static mut FLIGHT: usize = NO_FLIGHT;
static mut FLIGHT_ID: u16 = 0;

/// Has the op written everything?
fn complete(op: &Op) -> bool {
    (op.kind == OP_CLOSE && op.step >= 2) || (op.kind == OP_PUBLISH && op.step >= 3)
}

/// The next write of an op: (destination L2 byte address, the words, their count). None when the op is complete.
/// If chains, not a `match`: dense arms become a PC-relative jump table, which is not ColdFire code.
unsafe fn next_write(c: usize, k: usize, op: &Op) -> Option<(u32, [u32; 7], usize)> {
    let rec = RECORDS[c] + 32 * k as u32;
    let ctl = CONTROL[c] + 16 * k as u32;
    let mut w = [0u32; 7];
    if op.kind == OP_CLOSE {
        if op.step == 0 {
            w[0] = 1;
            return Some((ctl, w, 1));
        }
        if op.step == 1 {
            return Some((rec, w, 1));
        }
    } else if op.kind == OP_PUBLISH {
        if op.step == 0 {
            let mut i = 0;
            while i < 7 {
                w[i] = op.rec[1 + i];
                i += 1;
            }
            return Some((rec + 4, w, 7));
        }
        if op.step == 1 {
            return Some((ctl, w, 1));
        }
        if op.step == 2 {
            w[0] = op.rec[0];
            return Some((rec, w, 1));
        }
    }
    None
}

/// Finish the write in flight, then start the next one. Called twice per UI-loop pass.
unsafe fn pump<C: SampleCore>(api: &C) {
    if FLIGHT != NO_FLIGHT {
        let done = api.link_result(Ticket {
            owner: Owner::Tracks,
            id: FLIGHT_ID,
        });
        if done == LinkResult::Pending {
            return;
        }
        let (c, k) = (FLIGHT / TRACKS, FLIGHT % TRACKS);
        FLIGHT = NO_FLIGHT;
        let op = &mut (*addr_of_mut!(OPS))[c][k];
        if done == LinkResult::Done {
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
    // a table write waits for the loader: the rows of a module that loads are not in the DSP yet
    let d = api.dsp();
    if d.phase != PH_IDLE || d.nwait != 0 {
        return;
    }
    c = 0;
    while c < CLIENTS {
        let mut k = 0;
        while k < TRACKS {
            let op = (*addr_of!(OPS))[c][k];
            if (op.kind == OP_CLOSE || op.kind == OP_PUBLISH) && LIVE[c] {
                if let Some((dst, words, n)) = next_write(c, k, &op) {
                    if let Some(stream) = Stream::owned(0, dst, &words[..n]) {
                        if let Some(t) = api.link_submit(Owner::Tracks, &stream) {
                            FLIGHT = c * TRACKS + k;
                            FLIGHT_ID = t.id;
                        }
                    }
                    return;
                }
            }
            k += 1;
        }
        c += 1;
    }
}

// ---- file header and identity ---------------------------------------------------------------------------------------------

/// The device's sample root (`select` paths are root-relative; the runtime keeps the rooted form).
fn root<C: SampleCore>(core: &C) -> &'static [u8] {
    root_of(core.host())
}
fn root_of<H: SampleHost>(h: &H) -> &'static [u8] {
    if h.files_in_store() {
        ROOT_DN2
    } else {
        ROOT_DT2
    }
}

/// The identity of the sample file `path` (device-rooted, not NUL-terminated) from `Storage::stat`: its size and, when the storage gives one, the
/// file hash of its table entry (the listing row's). No mtime exists on either device (`mtime` 0): without a hash the identity is size and path. When
/// `head` is given, the first 64 bytes of the file. The storage routes (store, firmware file system) are the platform's.
unsafe fn probe_with<H: SampleHost>(
    h: &H,
    path: &[u8],
    head: Option<&mut [u8; 64]>,
) -> Result<FileId, i32> {
    if path.is_empty() || path.len() >= dkrt_sdk::sample_tracks::PATH {
        return Err(E_SEL_PATH);
    }
    let rel = unrooted(root_of(h), path);
    if rel.is_empty() {
        return Err(E_SEL_PATH);
    }
    let st = match head {
        Some(head) => h.store().stat_head(SAMPLES, rel, head)?.0,
        None => h.store().stat(SAMPLES, rel)?,
    };
    if st.kind != EntryKind::File {
        return Err(E_SEL_PATH);
    }
    let mut id = FileId {
        root: SAMPLES,
        size: st.size,
        mtime: 0,
        flags: 0,
        hash_kind: 0,
        hash: [0; 32],
    };
    if st.hash_valid {
        id.flags = ID_HASH_VALID;
        id.hash_kind = st.hash_kind as u32;
        id.hash = st.hash;
    }
    Ok(id)
}

unsafe fn probe<C: SampleCore>(
    core: &C,
    path: &[u8],
    head: Option<&mut [u8; 64]>,
) -> Result<FileId, i32> {
    probe_with(core.host(), path, head)
}

/// The environment the track state machine runs in: the pool adapter's `dkpool_*` calls, the table writes, and the core for the platform facts and the log.
struct Device<'a, C: SampleCore>(&'a C);
impl<C: SampleCore> Env for Device<'_, C> {
    fn inspect(&mut self, path: &[u8]) -> Result<Inspect, i32> {
        let mut head = [0u8; 64];
        let id = unsafe { probe(self.0, path, Some(&mut head))? };
        // Size and format only: no PCM is read here (it can be megabytes), the pool adapter reads it.
        // This branch is a format choice (the store identity and header check, or the native header alone), not a storage route: `probe` reads both the same way.
        if self.0.host().files_in_store() {
            return store_inspect(id.size, &id.hash, &head);
        }
        let (frames, channels) = dkrt_sdk::sample::native_header(&head, id.size)?;
        Ok(Inspect {
            id,
            frames,
            rate: dkrt_sdk::sample::RATE,
            channels,
        })
    }
    fn stat(&mut self, path: &[u8]) -> Result<FileId, i32> {
        unsafe { probe(self.0, path, None) }
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
    fn note(&mut self, n: Note) {
        unsafe {
            match n {
                Note::Loaded {
                    client,
                    entry,
                    frames,
                    base_l,
                    token,
                } => self.0.log(
                    logk::POOL_LOAD,
                    LOG_INFO,
                    [client << 8 | entry, 0, frames, base_l, token],
                ),
                Note::Invalid {
                    client,
                    entry,
                    reason,
                    token,
                    count,
                } => self.0.log(
                    logk::POOL_INVALID,
                    LOG_INFO,
                    [client << 8 | entry, reason, token, count, 0],
                ),
            }
        }
    }
    fn publish(&mut self, client: usize, track: usize, rec: &Strack) {
        unsafe {
            self.0.log(
                logk::POOL_PUBLISH,
                LOG_INFO,
                [
                    (client as u32) << 8 | track as u32,
                    rec.token,
                    rec.frames,
                    rec.base_l,
                    rec.base_r,
                ],
            );
            if FORCE == client {
                return;
            }
            let op = &mut (*addr_of_mut!(OPS))[client][track];
            *op = OP0;
            op.kind = OP_PUBLISH;
            op.rec = [
                rec.ready,
                rec.token,
                rec.frames,
                rec.rate,
                rec.channels,
                rec.fmt,
                rec.base_l,
                rec.base_r,
            ];
        }
    }
}

/// The client slot of the calling module, if it has a track table.
pub(crate) unsafe fn client(handle: u32) -> Result<usize, i32> {
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
pub unsafe fn remove<C: SampleCore>(core: &C, rec: usize) {
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec {
            FORCE = i;
            let mut n = 0;
            while n < 4 && !core_().teardown(i, &mut Device(core)) {
                n += 1;
            }
            FORCE = usize::MAX;
            (*addr_of_mut!(OPS))[i] = [OP0; TRACKS];
            crate::peakwin::reset(core, i);
            LIVE[i] = false;
            OWNER[i] = usize::MAX;
        }
        i += 1;
    }
}

/// Does module record `rec` hold a track table?
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

pub unsafe fn tracks<C: SampleCore>(core: &C, local_id: u32) -> i32 {
    let owner = core.current_source();
    if owner == 0 {
        return E_NOT_FOUND;
    }
    let rec = owner as usize - 1;
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec && LOCAL[i] == local_id {
            return i as i32 + 1;
        }
        i += 1;
    }
    E_NOT_FOUND
}

/// `dk_sample_pool_open`: the handle of the module's pool table with the lowest local id (the one table a module declares), as `tracks` gives it for
/// that id; E_NOT_FOUND without a table.
pub unsafe fn open_first<C: SampleCore>(core: &C) -> i32 {
    let owner = core.current_source();
    if owner == 0 {
        return E_NOT_FOUND;
    }
    let rec = owner as usize - 1;
    let mut best = usize::MAX;
    let mut i = 0;
    while i < CLIENTS {
        if OWNER[i] == rec && (best == usize::MAX || LOCAL[i] < LOCAL[best]) {
            best = i;
        }
        i += 1;
    }
    if best == usize::MAX {
        return E_NOT_FOUND;
    }
    tracks(core, LOCAL[best])
}

/// The device-rooted copy of a module's root-relative path (a listing row) in `full`, and its length; or a negative error.
unsafe fn rooted_path<C: SampleCore>(
    core: &C,
    path: *const u8,
    full: &mut [u8; dkrt_sdk::sample_tracks::PATH],
) -> Result<usize, i32> {
    if path.is_null() {
        return Err(E_SEL_PATH);
    }
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
        return Err(E_SEL_PATH);
    }
    // The path is root-relative (a listing row); the runtime keeps and checks the device-rooted form. A DT2 path that starts with '/' stays.
    rooted(root(core), &copy[..n], full)
}

/// `sample_pool_add`: the pool index of the file (a new entry, or the one that already holds the path), or a negative error.
pub unsafe fn pool_add<C: SampleCore>(core: &C, handle: u32, path: *const u8) -> i32 {
    if path.is_null() {
        return E_SEL_PATH;
    }
    if !pool_ok(core) {
        return E_CONTRACT;
    }
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut full = [0u8; dkrt_sdk::sample_tracks::PATH];
    let n = match rooted_path(core, path, &mut full) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let sr = core.host().mask_irq();
    let r = core_().pool_add(c, &full[..n]);
    core.host().restore_irq(sr);
    r
}

/// `sample_pool_set`: load the file into entry `index` (replacing what was there); 0 or a negative error.
pub unsafe fn pool_set<C: SampleCore>(core: &C, handle: u32, index: u32, path: *const u8) -> i32 {
    if path.is_null() {
        return E_SEL_PATH;
    }
    if !pool_ok(core) {
        return E_CONTRACT;
    }
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut full = [0u8; dkrt_sdk::sample_tracks::PATH];
    let n = match rooted_path(core, path, &mut full) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let sr = core.host().mask_irq();
    let r = core_().pool_set(c, index as usize, &full[..n]);
    core.host().restore_irq(sr);
    r
}

/// `sample_pool_clear`: the entry becomes UNUSED (a hole); the other indexes stay.
pub unsafe fn pool_clear<C: SampleCore>(core: &C, handle: u32, track: u32) -> i32 {
    match client(handle) {
        Ok(c) => {
            let sr = core.host().mask_irq();
            let r = core_().unbind(c, track as usize);
            core.host().restore_irq(sr);
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
pub unsafe fn path<C: SampleCore>(
    core: &C,
    handle: u32,
    track: u32,
    buf: *mut u8,
    cap: u32,
) -> i32 {
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if buf.is_null() {
        return E_SEL_PATH;
    }
    match core_().path(c, track as usize) {
        Ok(stored) => {
            let p = unrooted(root(core), stored);
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
pub unsafe fn tick<C: SampleCore>(core: &C) {
    if !pool_ok(core) {
        return;
    }
    // Optional on its own: the adapter delivers completions and finds a stock reset here, before the runtime steps the tracks.
    if black_box(dkpool_tick as *const () as usize) != 0 {
        dkpool_tick();
    }
    pump(core);
    core_().tick(&mut Device(core));
    pump(core);
}

/// Adapter to runtime: load `req` finished (`result` 0, E_POOL, E_READ or E_FORMAT). The runtime's `dkrt_pool_done` calls it.
pub unsafe fn pool_done(req: u32, result: i32) {
    core_().done(req, result);
}
/// Adapter to runtime: the backing was lost, all handles are invalid. The runtime's `dkrt_pool_lost` calls it.
pub unsafe fn pool_lost<C: SampleCore>(core: &C) {
    static mut LOST: u32 = 0;
    let n = read_volatile(addr_of!(LOST)).wrapping_add(1);
    *addr_of_mut!(LOST) = n;
    core.log(
        logk::POOL_LOST,
        logk::L_WARN << logk::LEVEL_SHIFT,
        [n, 0, 0, 0, 0],
    );
    core_().lost(&mut Device(core));
}
/// Project adapter to runtime: the current project slot changed. The runtime's `dkrt_project_changed` calls it.
pub unsafe fn project_changed(slot: u32) {
    core_().project_changed(slot);
}

/// Is the optional `dkpool_window_step` hook in the image?
pub(crate) unsafe fn window_hook() -> bool {
    black_box(dkpool_window_step as *const () as usize) != 0
}

/// One step of a peak window (worker task); see `dkpool_window_step` in dkrt_pool.h.
pub(crate) unsafe fn window_step(
    h: *const Handle,
    path: *const u8,
    first: u32,
    frames: u32,
    columns: u32,
    out: *mut i16,
    cursor: *mut u32,
) -> i32 {
    dkpool_window_step(h, path, first, frames, columns, out, cursor)
}

/// Bounded overview copy; no file or PCM access in the UI task.
pub unsafe fn peaks(handle: u32, track: u32, token: u32, out: *mut u8, capacity: u32) -> i32 {
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if out.is_null()
        || (out as usize & 1) != 0
        || capacity < 2048
        || black_box(dkpool_peaks as *const () as usize) == 0
    {
        return E_CONTRACT;
    }
    match core_().peak_handle(c, track as usize, token) {
        Ok(h) => dkpool_peaks(&h, out, capacity),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TEST_LOCK as LOCK;
    use dkrt_abi::core_api::CoreApi;
    use dkrt_test_support::{FakeCore, FakeHost};

    /// Client 0, track 1 has an op `kind`; its records and control sets are at fixed L2 addresses.
    unsafe fn seed(kind: u8, rec: [u32; 8]) {
        *addr_of_mut!(OPS) = [[OP0; TRACKS]; CLIENTS];
        FLIGHT = NO_FLIGHT;
        LIVE = [false; CLIENTS];
        LIVE[0] = true;
        RECORDS[0] = 0x1000;
        CONTROL[0] = 0x2000;
        (*addr_of_mut!(OPS))[0][1] = Op {
            kind,
            step: 0,
            tries: 0,
            rec,
        };
    }

    /// The writer's pass and the link's pass, `n` times (the model's time passes with each call).
    unsafe fn passes(fake: &FakeCore, n: usize) {
        for _ in 0..n {
            pump(fake);
            fake.link.borrow_mut().pump();
        }
    }

    #[test]
    fn a_publish_op_submits_its_three_writes_in_order() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            seed(OP_PUBLISH, [1, 2, 3, 4, 5, 6, 7, 8]);
            let fake = FakeCore::new();
            passes(&fake, 60);
            let words = |w: &[u32]| {
                w.iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<std::vec::Vec<u8>>()
            };
            let wire: std::vec::Vec<(u32, u32, std::vec::Vec<u8>)> = fake
                .link
                .borrow()
                .t
                .wire
                .iter()
                .map(|w| (w.win, w.dst, w.bytes.clone()))
                .collect();
            assert_eq!(
                wire,
                [
                    (0, 0x1000 + 32 + 4, words(&[2, 3, 4, 5, 6, 7, 8])),
                    (0, 0x2000 + 16, words(&[0])),
                    (0, 0x1000 + 32, words(&[1])),
                ]
            );
            assert_eq!((*addr_of!(OPS))[0][1].kind, OP_NONE);
        }
    }

    #[test]
    fn a_load_in_progress_holds_the_table_writes() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            seed(OP_CLOSE, [0; 8]);
            let fake = FakeCore::new();
            fake.dsp().phase = dkrt_abi::dsp::PH_SEND;
            passes(&fake, 20);
            assert!(fake.link.borrow().t.wire.is_empty());
            fake.dsp().phase = PH_IDLE;
            passes(&fake, 40);
            assert_eq!(fake.link.borrow().t.wire.len(), 2);
        }
    }

    #[test]
    fn a_write_that_fails_is_tried_again_and_a_stuck_op_ends_failed() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            seed(OP_CLOSE, [0; 8]);
            let fake = FakeCore::new();
            fake.link.borrow_mut().t.script.push_back(Some(92));
            passes(&fake, 80);
            // the first write failed once and was sent again; both writes of the close then went out
            assert_eq!(fake.link.borrow().t.wire.len(), 3);
            assert_eq!((*addr_of!(OPS))[0][1].kind, OP_NONE);
            seed(OP_CLOSE, [0; 8]);
            fake.link
                .borrow_mut()
                .t
                .script
                .extend(std::iter::repeat(Some(92)).take(TRIES as usize));
            passes(&fake, 400);
            assert_eq!((*addr_of!(OPS))[0][1].kind, OP_FAILED);
        }
    }

    const H: [u8; 32] = [7; 32];

    fn sample_files(f: &FakeHost) {
        f.storage.add_file(
            SAMPLES,
            b"KICKS/K1",
            &[3u8; 100],
            dkrt_abi::storage::HASH_ELEKTRON32,
            H,
        );
        f.storage.add_file(
            SAMPLES,
            b"KICKS/K2",
            &[4u8; 100],
            dkrt_abi::storage::HASH_NONE,
            [0; 32],
        );
        f.storage.add_dir(SAMPLES, b"KICKS/SUB");
    }

    #[test]
    fn a_store_route_device_gives_the_hash_of_the_table_entry() {
        let f = FakeHost::new();
        f.in_store.set(true);
        sample_files(&f);
        let mut head = [0u8; 64];
        unsafe {
            let id = probe_with(&f, b"samples/KICKS/K1", Some(&mut head)).unwrap();
            assert_eq!(
                id,
                FileId {
                    root: SAMPLES,
                    size: 100,
                    mtime: 0,
                    flags: ID_HASH_VALID,
                    hash_kind: 2,
                    hash: H
                }
            );
            assert_eq!(&head[..4], [3, 3, 3, 3]);
            assert_eq!(
                probe_with(&f, b"samples/KICKS/none", None),
                Err(E_NOT_FOUND)
            );
            assert_eq!(probe_with(&f, b"samples/", None), Err(E_SEL_PATH));
            assert_eq!(
                probe_with(&f, b"samples/KICKS/SUB", None),
                Err(E_NOT_FOUND),
                "a folder is no sample (it does not open as a file)"
            );
        }
    }

    #[test]
    fn a_firmware_fs_device_has_a_hash_only_when_the_storage_gives_one() {
        let f = FakeHost::new();
        sample_files(&f);
        unsafe {
            let hashed = probe_with(&f, b"/KICKS/K1", None).unwrap();
            assert_eq!(
                hashed,
                FileId {
                    root: SAMPLES,
                    size: 100,
                    mtime: 0,
                    flags: ID_HASH_VALID,
                    hash_kind: 2,
                    hash: H
                }
            );
            let plain = probe_with(&f, b"/KICKS/K2", None).unwrap();
            assert_eq!(
                plain,
                FileId {
                    root: SAMPLES,
                    size: 100,
                    mtime: 0,
                    flags: 0,
                    hash_kind: 0,
                    hash: [0; 32]
                }
            );
            assert!(!plain.same(&FileId {
                hash_kind: 2,
                flags: ID_HASH_VALID,
                hash: H,
                ..plain
            }));
            assert_eq!(probe_with(&f, b"/", None), Err(E_SEL_PATH));
        }
    }
}
