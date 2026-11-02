//! core/runtime: the digikit runtime (dkrt) v0. Code, data and the module arena live in `cf.expansion`.
//!
//! What it does (docs/plans/digikit-runtime.md, docs/design/digikit-runtime.md): reads `.dkm` module files from the card
//! module store with the firmware's own block read, checks them, copies and relocates them into a private arena, flushes
//! the caches, calls `init`, binds the module's slots. A hook in the UI task's event loop (`dkrt_tick`) polls a mailbox
//! for host commands (the selection, the restart) and calls slot 0.
//!
//! Rules: task context only (never from an interrupt); no `core::fmt`; every `extern "C"` function takes and returns
//! integers or pointers. Built with rustc_codegen_gcc for -mcpu=54418 (finding 26).
#![cfg_attr(not(test), no_std)]
#![allow(static_mut_refs)]

#[cfg(not(test))]
use core::panic::PanicInfo;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

mod arena;
mod assign;
pub mod boot;
pub mod card;
pub mod crash;
pub mod dlog;
mod dsp;
mod gen_abi_check;
pub mod fxpage;
mod fxsend;
mod loader;
pub mod pageclick;
pub mod registry;
pub mod scan;
pub mod settings;
mod sample;
mod sample_source;
mod tracks;
mod files;
mod sha256;
pub mod store;
mod table;
pub mod upload;
mod watch;

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    crate::crash::panic()
}

/// Arena bytes. A sub-region of `cf.expansion` that is part of this object's bss: patchkit places it with the object, so no
/// static object of any module can overlap it, and the reset-path loader zeroes it.
pub const ARENA_BYTES: usize = dkrt_sdk::device::LIMIT_ARENA_BYTES as usize;
/// Staging buffer for a `.dkm` file (and the store directory): files are at most this size in v0.
pub const STAGE_BYTES: usize = dkrt_sdk::device::LIMIT_STAGE_BYTES as usize;
pub const MAX_MODULES: usize = dkrt_sdk::device::LIMIT_MODULES as usize;
pub const LOG_ENTRIES: usize = 16;
pub const STATE_MAGIC: u32 = 0x444b_5254; // "DKRT"

#[repr(C, align(16))]
pub struct Arena(pub [u8; ARENA_BYTES]);
#[repr(C, align(16))]
pub struct Stage(pub [u8; STAGE_BYTES]);

#[no_mangle]
pub static mut DKRT_ARENA: Arena = Arena([0; ARENA_BYTES]);
#[no_mangle]
pub static mut DKRT_STAGE: Stage = Stage([0; STAGE_BYTES]);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Module {
    pub used: u32,
    pub name: [u8; 32],
    pub version: u32,
    pub base: u32,
    pub size: u32,
    pub fini: u32,
    pub slot_mask: u32,
    pub slot_fn: [u32; SLOTS],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LogEntry {
    pub msg: [u8; 24],
    pub value: u32,
}

/// All runtime state, one object a host can peek (`DKRT_STATE` in plan.json symbols).
#[repr(C)]
pub struct State {
    pub magic: u32,
    pub runtime: u32,
    pub ticks: u32,
    pub last_result: i32,
    pub loads: u32,
    pub slot_calls: u32,
    /// Head of the arena free list (address of the first free block, 0 = none).
    pub free_head: u32,
    pub arena_lo: u32,
    pub arena_hi: u32,
    pub log_next: u32,
    /// Function pointers bound to slots (0 = empty); one aligned 32-bit store publishes or clears one.
    pub slots: [u32; SLOTS],
    /// Tasks currently inside a slot function.
    pub entered: [u32; SLOTS],
    pub modules: [Module; MAX_MODULES],
    pub log: [LogEntry; LOG_ENTRIES],
}

#[no_mangle]
pub static mut DKRT_STATE: State = unsafe { core::mem::zeroed() };
/// The store path each module record was loaded from (kept apart from `State`, whose layout the proof tools peek).
#[no_mangle]
pub static mut DKRT_PATHS: [[u8; 64]; MAX_MODULES] = [[0; 64]; MAX_MODULES];
/// The address of each module record's `dkm_menu` (0: none), apart from `State` for the same reason.
#[no_mangle]
pub static mut DKRT_MENUS: [u32; MAX_MODULES] = [0; MAX_MODULES];
/// The contract versions each module record declared: api, machine, fx, service as (major, minor) bytes (`MODULES` reports them).
#[no_mangle]
pub static mut DKRT_CONTRACTS: [[u8; 8]; MAX_MODULES] = [[0; 8]; MAX_MODULES];
#[no_mangle]
pub static mut DKRT_MAILBOX: Mailbox = Mailbox {
    cmd: 0,
    seq: 0,
    result: 0,
    arg: [0; 96],
};

extern "C" {
    /// Per-device constants, defined by `src/cf/<dev>/device.s` from the device file: the digikit id (`[digikit] id`) and the name the
    /// INFO reply gives (`[digikit] name`: 3 characters and a NUL, as a big-endian word).
    static dkrt_device_id: u32;
    static dkrt_device_name: u32;
    /// The image id patchkit stamps into the image (`src/cf/<dev>/device.s`; `packages/patchkit/src/imageid.rs`): 8 bytes, zero in an object.
    static dkrt_image_id: [u8; 8];
    /// `cpushl` over a byte range (both caches), `src/cf/tramp.s`.
    fn dkrt_flush(start: u32, len: u32);
    /// `src/cf/tramp.s`: mask interrupts (returns the old status register), restore it, and the soft reset.
    fn dkrt_irq_mask() -> u32;
    fn dkrt_irq_restore(sr: u32);
    fn dkrt_soft_reset() -> !;
    // Helpers from compiler_builtins, linked into this object.
    fn memcpy(d: *mut u8, s: *const u8, n: usize) -> *mut u8;
    fn memset(d: *mut u8, c: i32, n: usize) -> *mut u8;
    fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32;
    fn __muldi3();
    fn __udivdi3();
    fn __umoddi3();
}

pub fn st() -> &'static mut State {
    unsafe { &mut *addr_of_mut!(DKRT_STATE) }
}

pub fn flush(start: u32, len: u32) {
    unsafe { dkrt_flush(start, len) }
}

pub fn device() -> u32 {
    unsafe { read_volatile(addr_of!(dkrt_device_id)) }
}

/// The effects the image takes at run time: CAPACITY's limit for `FX_SLOTS`. The free effect rows of `core/dynfx` (0 on an image without it, so a device
/// that cannot load effects yet reports 0).
pub fn fx_slots() -> u32 {
    unsafe { dsp::fx_free_rows() }
}

/// The device's short name as the INFO reply gives it: 3 characters and a NUL.
pub fn device_name() -> u32 {
    unsafe { read_volatile(addr_of!(dkrt_device_name)) }
}

/// The 8-byte id of this image, as patchkit stamped it (zeros when the object was not patched in by it).
pub fn image_id() -> [u8; 8] {
    let p = addr_of!(dkrt_image_id) as *const u8;
    let mut o = [0u8; 8];
    let mut i = 0;
    while i < 8 {
        o[i] = unsafe { read_volatile(p.add(i)) };
        i += 1;
    }
    o
}

/// The word a firmware code literal points to, when the literal is a DDR address (a misplaced anchor reads nothing).
unsafe fn word_at_literal(lit: *const u32) -> Option<u32> {
    let l = core::hint::black_box(lit as usize) as u32;
    if l == 0 {
        return None;
    }
    let a = read_volatile(l as *const u32);
    if !(dkrt_sdk::device::DDR_CACHED_BASE..dkrt_sdk::device::DDR_CACHED_BASE + dkrt_sdk::device::DDR_BYTES).contains(&a) || a & 3 != 0 {
        return None;
    }
    Some(read_volatile(a as *const u32))
}

/// The firmware's DSP block counter (1500 per second, from boot; wraps), `None` when the anchor does not point at a DDR word.
/// `BLOCK_COUNTER_LIT` is the 4-byte address literal in the firmware's code, whose value is the address of the word.
#[cfg(not(test))]
pub unsafe fn fw_blocks() -> Option<u32> {
    word_at_literal(addr_of!(dkfw::link::BLOCK_ISR_COUNTER_LIT) as *const u32)
}

/// Bytes the firmware has handed to the USB stack for MIDI and not yet sent (0 when the host reads), `None` as for `fw_blocks`.
pub unsafe fn tx_in_flight() -> Option<u32> {
    word_at_literal(addr_of!(dkfw::midi::TX_INFLIGHT_LIT) as *const u32)
}

/// Is an OS upgrade stream being taken (finding 75)? The receive state object's byte at +0 is 0 when idle and 1 to 3 from the header on. False when the
/// anchor does not point at a DDR object, so the caller then acts as before.
pub unsafe fn os_upgrade_active() -> bool {
    matches!(word_at_literal(addr_of!(dkfw::midi::OS_UPGRADE_STATE_LIT) as *const u32), Some(w) if w >> 24 != 0)
}

pub unsafe fn block_read(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    dkfw::storage::block_read(sector, bytes, buf)
}

/// The firmware's block write, ungated. Only `block_write` and `card::claim` call it.
pub unsafe fn raw_block_write(sector: u32, bytes: u32, buf: *const u8) -> i32 {
    dkfw::storage::block_write(sector, bytes, buf)
}

/// The one function that writes the card. Write `bytes` (a multiple of 512) from `buf` (16-byte aligned) to `sector`: refused with `E_RANGE`
/// outside the store range of the device file, and with `E_CARD_RO` when the card's area is not ours (`card::admit`; finding 46). Every
/// runtime card write goes through here: the selection record and the boot counter (boot.rs), uploads (upload.rs), and the claim of a blank
/// range (card.rs, which calls `raw_block_write` for its own headers after the same checks).
pub unsafe fn block_write(sector: u32, bytes: u32, buf: *const u8) -> i32 {
    let g = card::admit(sector, bytes);
    if g != OK {
        return g;
    }
    card::info().writes = card::info().writes.wrapping_add(1);
    raw_block_write(sector, bytes, buf)
}

#[cfg(not(test))]
pub fn irq_mask() -> u32 {
    unsafe { dkrt_irq_mask() }
}

#[cfg(not(test))]
pub fn irq_restore(sr: u32) {
    unsafe { dkrt_irq_restore(sr) }
}

/// Send one complete MIDI message (F0 .. F7 for SysEx) to `port` (2 = USB). The firmware takes the MIDI TX mutex itself and spins
/// when the 4 KiB TX ring is full.
pub unsafe fn midi_send(len: u32, msg: *const u8, port: u32) {
    dkfw::midi::send(len, msg, port)
}

pub unsafe fn rd32(p: *const u8) -> u32 {
    ((*p as u32) << 24)
        | ((*p.add(1) as u32) << 16)
        | ((*p.add(2) as u32) << 8)
        | (*p.add(3) as u32)
}

pub unsafe fn wr32(p: *mut u8, v: u32) {
    *p = (v >> 24) as u8;
    *p.add(1) = (v >> 16) as u8;
    *p.add(2) = (v >> 8) as u8;
    *p.add(3) = v as u8;
}

/// Compare two NUL-terminated strings.
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

/// Length of a NUL-terminated string, at most `max`.
pub unsafe fn cstr_len(a: *const u8, max: usize) -> usize {
    let mut i = 0;
    while i < max && *a.add(i) != 0 {
        i += 1;
    }
    i
}

// ---- imports -------------------------------------------------------------------------------------------------------

#[repr(C)]
pub struct Import {
    pub name: [u8; 28],
    pub addr: *const (),
}
pub struct Imports(pub [Import; 14]);
unsafe impl Sync for Imports {}

const fn nm(s: &[u8]) -> [u8; 28] {
    let mut o = [0u8; 28];
    let mut i = 0;
    while i < s.len() {
        o[i] = s[i];
        i += 1;
    }
    o
}

/// The names of `IMPORTS`, in its order; checked against the `[[import]]` entries of `sdk/sdk.toml` without `requires` below.
const NAMES: [&[u8]; 14] = [
    b"fw_block_read",
    b"fw_alloc",
    b"fw_free",
    b"memcpy",
    b"memset",
    b"memcmp",
    b"__muldi3",
    b"__udivdi3",
    b"__umoddi3",
    b"dkrt_blocks",
    b"dkrt_log_sink_set",
    b"dkrt_log_ev",
    b"dkrt_peek_range",
    b"dkrt_track_type",
];

const fn names_match(ours: &[&[u8]], sdk: &[&str]) -> bool {
    if ours.len() != sdk.len() {
        return false;
    }
    let mut i = 0;
    while i < ours.len() {
        let (a, b) = (ours[i], sdk[i].as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut j = 0;
        while j < a.len() {
            if a[j] != b[j] {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

const _: () = assert!(names_match(&NAMES, &dkrt_sdk::api::BUILTIN_NAMES), "IMPORTS names differ from [[import]] of sdk/sdk.toml");
const _: dkrt_sdk::api::DkrtBlocksFn = dkrt_blocks;
const _: dkrt_sdk::api::DkrtLogSinkSetFn = dlog::dkrt_log_sink_set;
const _: dkrt_sdk::api::DkrtLogEvFn = dlog::dkrt_log_ev;
const _: dkrt_sdk::api::DkrtPeekRangeFn = dkrt_peek_range;
const _: dkrt_sdk::api::DkrtTrackTypeFn = dkrt_track_type;

/// The names a module may import. Firmware functions come from patch-time anchors (`dkfw`); the helpers are this
/// object's own copies, so a module need not carry them.
pub static IMPORTS: Imports = Imports([
    Import {
        name: nm(NAMES[0]),
        addr: dkfw::storage::block_read as *const (),
    },
    Import {
        name: nm(NAMES[1]),
        addr: dkfw::mem::new as *const (),
    },
    Import {
        name: nm(NAMES[2]),
        addr: dkfw::mem::free as *const (),
    },
    Import {
        name: nm(NAMES[3]),
        addr: memcpy as *const (),
    },
    Import {
        name: nm(NAMES[4]),
        addr: memset as *const (),
    },
    Import {
        name: nm(NAMES[5]),
        addr: memcmp as *const (),
    },
    Import {
        name: nm(NAMES[6]),
        addr: __muldi3 as *const (),
    },
    Import {
        name: nm(NAMES[7]),
        addr: __udivdi3 as *const (),
    },
    Import {
        name: nm(NAMES[8]),
        addr: __umoddi3 as *const (),
    },
    // the 1500 Hz block clock (`dkrt_blocks`), for a module that wants to measure time
    Import {
        name: nm(NAMES[9]),
        addr: dkrt_blocks as *const (),
    },
    Import {
        name: nm(NAMES[10]),
        addr: dlog::dkrt_log_sink_set as *const (),
    },
    Import {
        name: nm(NAMES[11]),
        addr: dlog::dkrt_log_ev as *const (),
    },
    Import {
        name: nm(NAMES[12]),
        addr: dkrt_peek_range as *const (),
    },
    Import {
        name: nm(NAMES[13]),
        addr: dkrt_track_type as *const (),
    },
]);

extern "C" {
    /// Per-device data from the device file (`[digikit] peek`), defined by `src/cf/<dev>/device.s`: the number of ranges and `lo, hi` pairs.
    static dkrt_peek_n: u32;
    static dkrt_peek_ranges: [u32; 8];
}

/// Import `dkrt_peek_range`: how many RAM ranges the device file allows PEEK to read; for `i` below that, range `i` as `lo, hi` into `out`.
unsafe extern "C" fn dkrt_peek_range(i: u32, out: *mut u32) -> u32 {
    let n = read_volatile(addr_of!(dkrt_peek_n)).min(4);
    if i < n {
        let t = addr_of!(dkrt_peek_ranges) as *const u32;
        *out = read_volatile(t.add(2 * i as usize));
        *out.add(1) = read_volatile(t.add(2 * i as usize + 1));
    }
    n
}

/// Import `dkrt_track_type`: the machine type of a track of the active kit, from the firmware's own accessors (read-only); -1 past track 15 or when the
/// firmware has no type for it, -2 when there is no project yet.
unsafe extern "C" fn dkrt_track_type(track: u32) -> i32 {
    // `Project *f(void)`, `Kit *f(Project *)` (the active kit), `int f(Kit *, int track)` (-1: no such track)
    let p = dkfw::project::singleton();
    if p == 0 {
        return -2;
    }
    if track >= 16 {
        return -1;
    }
    dkfw::project::track_type(dkfw::project::active_kit(p), track as i32) as i32
}

// ---- imports of other modules --------------------------------------------------------------------------------------------

// The imports another module provides (`[[import]] requires` of sdk/sdk.toml: core/input, core/ui). The symbols are in this object's `optional`
// list (module.toml): patchkit links one to 0 when its module is not in the image.
dkrt_sdk::optional_imports!();

/// Address of the import `name` (NUL-terminated), 0 when the table has none. An import of another module (`optional`) gives that module's
/// function; when the module is not in the image it gives the stub of the import if sdk/sdk.toml declares `absent`, else 0.
pub unsafe fn import_addr(name: *const u8) -> u32 {
    let mut i = 0;
    while i < IMPORTS.0.len() {
        if cstr_eq(IMPORTS.0[i].name.as_ptr(), name) {
            return IMPORTS.0[i].addr as usize as u32;
        }
        i += 1;
    }
    let t = optional::table();
    i = 0;
    while i < t.len() {
        if cstr_eq_str(name, t[i].0) {
            return (if t[i].1 != 0 { t[i].1 } else { t[i].2 }) as u32;
        }
        i += 1;
    }
    0
}

/// Whether the NUL-terminated `a` equals `b`.
unsafe fn cstr_eq_str(a: *const u8, b: &str) -> bool {
    let b = b.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if *a.add(i) != b[i] {
            return false;
        }
        i += 1;
    }
    *a.add(b.len()) == 0
}

// ---- the API table ---------------------------------------------------------------------------------------------------

unsafe extern "C" fn api_import(name: *const u8) -> u32 {
    import_addr(name)
}
unsafe extern "C" fn api_alloc(size: u32) -> *mut u8 {
    arena::alloc(size)
}
unsafe extern "C" fn api_free(p: *mut u8) {
    arena::free(p);
}
unsafe extern "C" fn api_store_read(path: *const u8, dst: *mut u8, cap: u32) -> i32 {
    store::read_file(path, dst, cap)
}
unsafe extern "C" fn api_log(msg: *const u8, value: u32) {
    log(msg, value);
}
unsafe extern "C" fn api_log_lv(level: u32, msg: *const u8, value: u32) {
    log_at(level, msg, value);
}
unsafe extern "C" fn api_register_cmds(first: u32, count: u32, handler: Option<dkrt_sdk::api::ServiceCmd>) -> i32 {
    upload::register_cmds(first, count, handler)
}

unsafe extern "C" fn api_sample_bank(id: u32) -> i32 { sample::bank(id) }
unsafe extern "C" fn api_sample_load(handle: u32, path: *const u8) -> i32 { sample::load(handle, path) }
unsafe extern "C" fn api_sample_status(handle: u32) -> i32 { sample::status(handle) }
unsafe extern "C" fn api_sample_tracks(id: u32) -> i32 { tracks::tracks(id) }
unsafe extern "C" fn api_sample_pool_add(handle: u32, path: *const u8) -> i32 { tracks::pool_add(handle, path) }
unsafe extern "C" fn api_sample_pool_clear(handle: u32, track: u32) -> i32 { tracks::pool_clear(handle, track) }
unsafe extern "C" fn api_sample_pool_status(handle: u32, track: u32) -> i32 { tracks::status(handle, track) }
unsafe extern "C" fn api_sample_pool_info(handle: u32, track: u32, out: *mut u8) -> i32 { tracks::info(handle, track, out) }
unsafe extern "C" fn api_sample_pool_path(handle: u32, track: u32, buf: *mut u8, cap: u32) -> i32 { tracks::path(handle, track, buf, cap) }
unsafe extern "C" fn api_files_client() -> i32 { files::client() }
unsafe extern "C" fn api_files_list(client: u32, root: u32, path: *const u8, notify: Option<dkrt_sdk::api::FileEvent>, context: *mut u8) -> i32 { files::list(client, root, path, notify, context) }
unsafe extern "C" fn api_files_poll(client: u32, ticket: u32, out: *mut u8, cap: u32) -> i32 { files::poll(client, ticket, out, cap) }
unsafe extern "C" fn api_files_cancel(client: u32, ticket: u32) -> i32 { files::cancel(client, ticket) }


/// The table is built at first use: pointers to the helpers are not constants.
#[no_mangle]
pub static mut dkrt_api: Api = Api {
    version: API_VERSION,
    size: core::mem::size_of::<Api>() as u32,
    device: 0,
    runtime: RUNTIME_VERSION,
    import: api_import,
    alloc: api_alloc,
    free: api_free,
    store_read: api_store_read,
    log: api_log,
    memcpy: 0,
    memset: 0,
    memcmp: 0,
    muldi3: 0,
    udivdi3: 0,
    umoddi3: 0,
    log_lv: api_log_lv,
    register_cmds: api_register_cmds,
    sample_bank: api_sample_bank,
    sample_load: api_sample_load,
    sample_status: api_sample_status,
    files_client: api_files_client,
    files_list: api_files_list,
    files_poll: api_files_poll,
    files_cancel: api_files_cancel,
    sample_tracks: api_sample_tracks,
    sample_pool_add: api_sample_pool_add,
    sample_pool_clear: api_sample_pool_clear,
    sample_pool_status: api_sample_pool_status,
    sample_pool_info: api_sample_pool_info,
    sample_pool_path: api_sample_pool_path,
};

pub unsafe fn log(msg: *const u8, value: u32) {
    log_at(logk::L_INFO as u32, msg, value);
}

/// `log` at `level` (0 is an error event; above 4 is 4).
pub unsafe fn log_at(level: u32, msg: *const u8, value: u32) {
    let level = if level > logk::L_TRACE as u32 { logk::L_TRACE } else { level as u16 };
    let s = st();
    let e = &mut s.log[(s.log_next as usize) % LOG_ENTRIES];
    let mut i = 0;
    while i < 23 && *msg.add(i) != 0 {
        e.msg[i] = *msg.add(i);
        i += 1;
    }
    while i < 24 {
        e.msg[i] = 0;
        i += 1;
    }
    e.value = value;
    s.log_next = s.log_next.wrapping_add(1);
    dlog::ev(
        logk::MOD_LOG,
        (level << logk::LEVEL_SHIFT) | if level == logk::L_ERROR { logk::F_ERROR } else { 0 },
        [dlog::word_of(e.msg.as_ptr(), 0, 24), dlog::word_of(e.msg.as_ptr(), 4, 24), dlog::word_of(e.msg.as_ptr(), 8, 24), dlog::word_of(e.msg.as_ptr(), 12, 24), value],
    );
}

/// First-use setup: arena free list, API table fields, magic.
pub unsafe fn init_once() {
    let s = st();
    if s.magic == STATE_MAGIC {
        return;
    }
    arena::init();
    let a = &mut *addr_of_mut!(dkrt_api);
    a.device = device();
    a.memcpy = import_addr(b"memcpy\0".as_ptr()) as usize;
    a.memset = import_addr(b"memset\0".as_ptr()) as usize;
    a.memcmp = import_addr(b"memcmp\0".as_ptr()) as usize;
    a.muldi3 = import_addr(b"__muldi3\0".as_ptr()) as usize;
    a.udivdi3 = import_addr(b"__udivdi3\0".as_ptr()) as usize;
    a.umoddi3 = import_addr(b"__umoddi3\0".as_ptr()) as usize;
    s.runtime = RUNTIME_VERSION;
    s.magic = STATE_MAGIC;
}

// ---- the public C ABI ----------------------------------------------------------------------------------------------------

/// Load the module file `path` (NUL-terminated, relative to the store root, for example "modules/dyn-demo.dkm").
/// Returns 0 or a negative `E_*` code; on an error nothing was left behind. Task context only; not reentrant. Only the boot hook
/// loads: the module set is fixed per session, so every other caller gets `E_BOOT_ONLY` (hot reload and `dkrt_unload` are archived,
/// tag `digikit-hot-reload-v1`, docs/plans/digikit-hot-reload.md).
#[no_mangle]
pub unsafe extern "C" fn dkrt_load(path: *const u8) -> i32 {
    init_once();
    let r = loader::load(path);
    st().last_result = r;
    r
}

// ---- health (finding 41) ------------------------------------------------------------------------------------------------

extern "C" {
    // Symbols of core/health; each resolves to 0 when the module is not in the image (`[object] optional`).
    fn health_read();
    fn health_heap();
}

/// What core/health fills (`HealthRaw` of modules/core/adapters/health/src/cf/health.c): 20 words.
#[repr(C)]
struct HealthRaw {
    magic: u32,
    version: u32,
    blocks: u32,
    windows: u32,
    idle_total: u32,
    idle_win: u32,
    idle_ref: u32,
    dsp_avg: u32,
    dsp_max: u32,
    dsp_n: u32,
    dsp_last: u32,
    flags: u32,
    heap_total: u32,
    heap_free: u32,
    heap_largest: u32,
    heap_allocs: u32,
    heap_free_blocks: u32,
    heap_walked: u32,
    pad: [u32; 2],
}
const RAW_MAGIC: u32 = 0x484c_5448;
const RAW_WINDOW: u32 = 1;
const RAW_HEAP: u32 = 2;
const RAW_CUT: u32 = 4;
const RAW_BAD: u32 = 8;

/// The last result of the mailbox command `CMD_HEALTH` (the proof reads it).
#[no_mangle]
pub static mut DKRT_HEALTH: DkrtHealth = unsafe { core::mem::zeroed() };
/// A function the UI-loop tick calls once per pass (0: none); set by `dkrt_poll_set`. The DIGIKIT menu's HEALTH page uses it to
/// refresh itself.
#[no_mangle]
pub static mut DKRT_POLL: usize = 0;

/// DSP blocks counted since boot (1500 per second; wraps): the firmware's own counter (anchor `cf.link.site.block_isr.counter_lit`), so a clock for
/// a UI that refreshes at a bounded rate and for the stable period of safe mode. 0 when the anchor does not point at a counter and the
/// image has no `core/health` (whose own count is the fallback). Any task; reads one word.
#[no_mangle]
pub unsafe extern "C" fn dkrt_blocks() -> u32 {
    if let Some(b) = fw_blocks() {
        return b;
    }
    if health_fn(health_read) == 0 {
        return 0;
    }
    let mut r: HealthRaw = core::mem::zeroed();
    let read: unsafe extern "C" fn(*mut HealthRaw) = core::mem::transmute(health_fn(health_read));
    read(&mut r);
    if r.magic == RAW_MAGIC {
        r.blocks
    } else {
        0
    }
}

/// Register (or, with 0, remove) a function that `dkrt_tick` calls at the end of every UI-loop pass, in the UI task. One caller only (the
/// DIGIKIT menu's HEALTH page); `dkrt_poll_add` has room for more.
#[no_mangle]
pub unsafe extern "C" fn dkrt_poll_set(f: usize) {
    write_volatile(addr_of_mut!(DKRT_POLL), f);
}

/// Functions of `dkrt_poll_add`, called after the one of `dkrt_poll_set`.
const POLLS: usize = 4;
static mut POLL_TABLE: [usize; POLLS] = [0; POLLS];

/// Add a function that `dkrt_tick` calls once per UI-loop pass, in the UI task (`core/ui` keeps its `dk_ui_tick` timers with one). Adding one
/// that is in the table is OK; `E_NOMEM` when it is full. Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_poll_add(f: usize) -> i32 {
    if f == 0 {
        return E_CONTRACT;
    }
    let t = &mut *addr_of_mut!(POLL_TABLE);
    let mut free = POLLS;
    let mut i = 0;
    while i < POLLS {
        if t[i] == f {
            return OK;
        }
        if t[i] == 0 && free == POLLS {
            free = i;
        }
        i += 1;
    }
    if free == POLLS {
        return E_NOMEM;
    }
    t[free] = f;
    OK
}

/// Remove a function added with `dkrt_poll_add` (no effect when it is not there). Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_poll_remove(f: usize) {
    let t = &mut *addr_of_mut!(POLL_TABLE);
    let mut i = 0;
    while i < POLLS {
        if f != 0 && t[i] == f {
            t[i] = 0;
        }
        i += 1;
    }
}

fn health_fn(f: unsafe extern "C" fn()) -> usize {
    core::hint::black_box(f as usize)
}

/// Is `core/health` in the image?
pub fn health_present() -> bool {
    health_fn(health_read) != 0
}

/// Fill `out` (a `DkrtHealth`): the measurements of core/health (CPU idle counter, the DSP's block cycle count, the heap) with the
/// runtime's arena and DSP area. Heap figures come from walking the firmware's free lists (bounded, under its mutex): ask about twice
/// a second at most. Without core/health the flag `HEALTH_MODULE` is 0 and only the arena and DSP area are filled. Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_health(out: *mut DkrtHealth) {
    init_once();
    dkrt_health_light(&mut *out, true);
    store::figures(&mut *out);
}

/// The module files of the store whose path starts with `prefix` (NUL-terminated), sorted by path, for the DIGIKIT menu: at most `max` entries into
/// `out` (`DkrtStoreEntry`); the count, or a negative code (`E_NOT_FOUND`: no store on this card). Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_store_list(prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
    init_once();
    store::list(prefix, out, max)
}

/// The header fields of the `.dkm` at store sector `first` (`DkrtScan.first`) into `out`, for the INFO page of the DIGIKIT menu: 0, `E_FORMAT` when
/// the file is not a `.dkm`, or the read error. The runtime reads the sector into its own DMA-safe buffer. Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_module_header(first: u32, out: *mut DkrtModuleHeader) -> i32 {
    init_once();
    scan::module_header(first, out)
}

/// `dkrt_health` without the store figures (which can read the card) and, when `walk_heap` is false, without the heap walk: what the log's
/// snapshot uses. The store figures are left 0 and `HEALTH_STORE` clear.
pub unsafe fn dkrt_health_light(o: &mut DkrtHealth, walk_heap: bool) {
    let s = st();
    let (afree, alargest) = arena::stats();
    o.magic = HEALTH_MAGIC;
    o.version = 3;
    o.flags = 0;
    o.arena_total = s.arena_hi - s.arena_lo;
    o.arena_free = afree;
    o.arena_largest = alargest;
    o.dsp_area_total = dsp::area_bytes();
    let (dfree, dlargest) = dsp::area_stats();
    o.dsp_area_free = dfree;
    o.dsp_area_largest = dlargest;
    o.dsp_period = DSP_BLOCK_CYCLES;
    o.store_total = store::layout().limit() / 2;
    if health_fn(health_read) == 0 {
        return;
    }
    let mut r: HealthRaw = core::mem::zeroed();
    let read: unsafe extern "C" fn(*mut HealthRaw) = core::mem::transmute(health_fn(health_read));
    read(&mut r);
    if r.magic != RAW_MAGIC {
        return;
    }
    let mut fl = HEALTH_MODULE;
    o.blocks = r.blocks;
    o.windows = r.windows;
    o.idle_win = r.idle_win;
    o.idle_ref = r.idle_ref;
    // load = 100 % - the window's idle iterations as a share of the best window; valid from the second window, when the reference
    // is a window with the loop running
    if r.flags & RAW_WINDOW != 0 && r.windows >= 3 && r.idle_ref >= 1000 {
        let share = r.idle_win / (r.idle_ref / 1000);
        o.cpu_x10 = 1000u32.saturating_sub(share.min(1000));
        fl |= HEALTH_CPU;
    }
    if r.flags & RAW_WINDOW != 0 && r.dsp_n != 0 {
        o.dsp_avg = r.dsp_avg;
        o.dsp_max = r.dsp_max;
        o.dsp_avg_x10 = (r.dsp_avg.min(DSP_BLOCK_CYCLES * 4) * 1000) / DSP_BLOCK_CYCLES;
        o.dsp_max_x10 = (r.dsp_max.min(DSP_BLOCK_CYCLES * 4) * 1000) / DSP_BLOCK_CYCLES;
        fl |= HEALTH_DSP;
    }
    if walk_heap && health_fn(health_heap) != 0 {
        let heap: unsafe extern "C" fn(*mut HealthRaw) = core::mem::transmute(health_fn(health_heap));
        heap(&mut r);
        if r.flags & RAW_HEAP != 0 {
            o.heap_total = r.heap_total;
            o.heap_free = r.heap_free;
            o.heap_largest = r.heap_largest;
            o.heap_allocs = r.heap_allocs;
            o.heap_free_blocks = r.heap_free_blocks;
            fl |= HEALTH_HEAP;
            if r.flags & RAW_CUT != 0 {
                fl |= HEALTH_HEAP_CUT;
            }
            if r.flags & RAW_BAD != 0 {
                fl |= HEALTH_HEAP_BAD;
            }
        }
    }
    o.flags = fl;
}

/// Fill `out` (a `DkrtStatus`).
#[no_mangle]
pub unsafe extern "C" fn dkrt_status(out: *mut DkrtStatus) {
    init_once();
    let s = st();
    let (free, largest) = arena::stats();
    let mut n = 0;
    let mut i = 0;
    while i < MAX_MODULES {
        if s.modules[i].used != 0 {
            n += 1;
        }
        i += 1;
    }
    let o = &mut *out;
    o.magic = s.magic;
    o.runtime = s.runtime;
    o.device = device();
    o.arena_base = s.arena_lo;
    o.arena_size = s.arena_hi - s.arena_lo;
    o.arena_free = free;
    o.arena_largest = largest;
    o.modules = n;
    o.ticks = s.ticks;
    o.last_result = s.last_result;
    o.loads = s.loads;
    o.slot_calls = s.slot_calls;
    o.state = dsp::state();
    o.dsp_size = dsp::area_bytes();
    let (dfree, dlargest) = dsp::area_stats();
    o.dsp_free = dfree;
    o.dsp_largest = dlargest;
}

/// `dkrt_capacity(out, max)`: the entries of the CAPACITY command (one `DkrtCap` per resource, kinds in sdk/sdk.toml `[capacity.kinds]`), at most
/// `max` of them; returns how many it wrote. The SysEx command and the DIGIKIT menu's MEMORY page read the same figures.
#[no_mangle]
pub unsafe extern "C" fn dkrt_capacity(out: *mut DkrtCap, max: u32) -> u32 {
    init_once();
    use dkrt_sdk::proto::*;
    let s = st();
    let (af, al) = arena::stats();
    let (df, dl) = dsp::area_stats();
    let area = dsp::area_bytes();
    let arena = s.arena_hi - s.arena_lo;
    let mut mods = 0;
    let mut i = 0;
    while i < MAX_MODULES {
        if s.modules[i].used != 0 {
            mods += 1;
        }
        i += 1;
    }
    let (vrows, vused) = dsp::rows_counts();
    let [mach, param, page] = registry::counts();
    let stage = STAGE_BYTES as u32;
    // the store: its area in KiB (0 when the card is too small for it), and what the headers say is used (a card with no header: nothing)
    let st_total = if card::state() == CARD_TOO_SMALL { 0 } else { store::layout().limit() / 2 };
    let st_used = match store::seen() {
        Some((used, _)) => used,
        None => 0,
    };
    let (set_limit, set_used) = settings::capacity();
    let entries: [(u8, u32, u32, u32); 14] = [
        (CAPK_MODULES, MAX_MODULES as u32, mods, 0),
        (CAPK_SELECTION, dkrt_sdk::SEL_MAX as u32, boot::selected(), 0),
        (CAPK_ARENA, arena, arena - af, al),
        (CAPK_STAGE, stage, 0, stage),
        (CAPK_DSP_AREA, area, area - df, dl),
        (CAPK_MACHINE_ROWS, mach.0, mach.1, 0),
        (CAPK_VOICE_ROWS, vrows, vused, 0),
        (CAPK_PARAM_ROWS, param.0, param.1, 0),
        (CAPK_PAGE_ROWS, page.0, page.1, 0),
        (CAPK_FX_SLOTS, fx_slots(), dsp::fx_used(), 0),
        (CAPK_MAP_ENTRIES, dkrt_sdk::sel::MAP_N as u32, assign::map_used(), 0),
        (CAPK_STORE, st_total, st_used, 0),
        (CAPK_SETTINGS, set_limit, set_used, 0),
        (CAPK_SEND_SLOTS, fxsend::slots(), fxsend::used(), 0),
    ];
    let mut k = 0;
    while k < entries.len() && (k as u32) < max {
        let (kind, limit, used, largest) = entries[k];
        *out.add(k) = DkrtCap { kind: kind as u32, limit, used, largest };
        k += 1;
    }
    k as u32
}

/// `dkrt_diag(out)`: what the debug log, the card check and the DSP watch found (a `DkrtDiag`).
#[no_mangle]
pub unsafe extern "C" fn dkrt_diag(out: *mut DkrtDiag) {
    init_once();
    let o = &mut *out;
    let c = card::info();
    let (dsp_ok, hb) = watch::heartbeat();
    let k = crash::seen();
    *o = DkrtDiag {
        magic: DIAG_MAGIC,
        version: 1,
        card_state: c.state,
        card_first_nonblank: c.first_nonblank,
        card_hash: c.hash,
        card_refused: c.refused,
        card_read_errors: c.read_errors,
        debug_on: dlog::sink_present() as u32,
        log_oldest: 0,
        log_newest: 0,
        dsp_diag_ok: dsp_ok as u32,
        dsp_stopped: watch::dsp_stopped() as u32,
        dsp_heartbeat: hb,
        crash_found: k.found,
        crash_pc: k.pc,
        crash_vec: k.vec,
        ram_kept: k.ram_kept,
        dsp_crumb: watch::stop_crumb(),
        fault_rows: 0,
        fault_text: [[0; 24]; FAULT_ROWS],
    };
    // the last fault, kept in the settings store (`dk/fault`): CPU crash or DSP stop, the module, the place
    let mut v = [0u8; dkrt_sdk::fault::BYTES];
    if settings::get(dkrt_sdk::fault::KEY, &mut v) == dkrt_sdk::fault::BYTES as i32 {
        if let Some(f) = dkrt_sdk::fault::Fault::decode(&v) {
            let mut rows = [dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new()];
            let n = dkrt_sdk::fault::rows(&f, &mut rows);
            let mut k = 0;
            while k < n && k < FAULT_ROWS {
                o.fault_text[k] = rows[k].b;
                k += 1;
            }
            o.fault_rows = k as u32;
        }
    }
}

/// The `index`th loaded module (in load-slot order) into `out`; returns 0, or `E_NOT_FOUND` past the end.
#[no_mangle]
pub unsafe extern "C" fn dkrt_module(index: u32, out: *mut DkrtModule) -> i32 {
    init_once();
    let s = st();
    let mut seen = 0;
    let mut i = 0;
    while i < MAX_MODULES {
        let m = &s.modules[i];
        if m.used != 0 {
            if seen == index {
                let o = &mut *out;
                o.name = m.name;
                o.version = m.version;
                o.base = m.base;
                o.size = m.size;
                o.slots = m.slot_mask;
                o.path = (*addr_of!(DKRT_PATHS))[i];
                o.state = if m.used == 2 { MOD_LOADING } else { MOD_LOADED };
                o.dsp_size = dsp::ds().mod_len[i];
                o.menu = (*addr_of!(DKRT_MENUS))[i];
                return OK;
            }
            seen += 1;
        }
        i += 1;
    }
    E_NOT_FOUND
}

/// The `index`th loaded effect that has a page (row order) into `out`: its tab, module id, allowed routes, default route, the route it runs on and the route the settings
/// give it (DIGIKIT > FX ROUTES). 0, or `E_NOT_FOUND` past the end. UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_fx(index: u32, out: *mut DkrtFx) -> i32 {
    init_once();
    fxsend::fx_entry(index, &mut *out)
}

// ---- boot-time loading (finding 37) ---------------------------------------------------------------------------------------

/// `dkrt_selection_get(out)`: the saved selection (what the next boot loads). Always 0.
#[no_mangle]
pub unsafe extern "C" fn dkrt_selection_get(out: *mut DkrtSelection) -> i32 {
    init_once();
    boot::selection_get(out)
}

/// `dkrt_selection_set(sel)`: save the selection (two copies, sequence number, CRC); no effect until the next boot. 0 or `E_SEL_*`.
#[no_mangle]
pub unsafe extern "C" fn dkrt_selection_set(sel: *const DkrtSelection) -> i32 {
    init_once();
    boot::selection_set(sel)
}

/// `dkrt_boot_info(out)`: safe mode and why, the boot counter, each selected module's result at this boot.
#[no_mangle]
pub unsafe extern "C" fn dkrt_boot_info(out: *mut DkrtBootInfo) {
    init_once();
    boot::boot_info(out)
}

/// What `dkrt_restart` does to keep the working state (finding 40): bit 0 serializes the live project into the persistent arena
/// (`dkfw::project::serialize`), bit 1 writes the arena to the next boot record (`dkfw::storage::boot_record_write`, the firmware's own writer for
/// the OS-update path). The default is 0 (no save; the menu asks for a project save first): finding 40 has no edit that the save keeps,
/// and the serialize may race the firmware's BgWorker. 3 runs both steps, for the experiments of finding 40 (a host poke).
#[no_mangle]
pub static mut DKRT_SAVE_MODE: Aligned4 = Aligned4(0);

/// A word that is 4-aligned (the m68k ABI aligns `u32` to 2; the host pokes this one with aligned stores).
#[repr(C, align(4))]
pub struct Aligned4(pub u32);

/// Save the working state the way the firmware does before it flashes an update: the project into the arena, the arena into
/// the boot record. Runs in the UI task with interrupts enabled (the writer takes the eMMC mutex itself).
unsafe fn save_working_state() {
    let mode = read_volatile(addr_of!(DKRT_SAVE_MODE.0));
    if mode & 1 != 0 {
        dkfw::project::serialize();
    }
    if mode & 2 != 0 {
        dkfw::storage::boot_record_write();
    }
}

/// MCF5441x eSDHC: offset of the present-state register (`PRSSTAT`); bits 0 to 2 are command inhibit, data inhibit and data line active.
const MCF5441X_ESDHC_PRSSTAT: u32 = 0x24;
/// The card's eSDHC controller (`[board] esdhc_base` of the device files) and its present-state register.
const ESDHC_PRSSTAT: u32 = dkrt_sdk::device::ESDHC_BASE + MCF5441X_ESDHC_PRSSTAT;

unsafe fn card_busy() -> bool {
    read_volatile(ESDHC_PRSSTAT as *const u32) & 7 != 0
}

/// `dkrt_restart()`: write pending settings (`settings::flush_if_dirty`), save the working state (`save_working_state`), then the firmware's own soft reset: interrupts masked, console drained,
/// DDR word 0 cleared, `0x80` into `RCM.RCR`. Returns only when it refuses (`E_CARD_BUSY`: a card transfer is in flight; the
/// check is made again with interrupts masked, when no other task can start one).
#[no_mangle]
pub unsafe extern "C" fn dkrt_restart() -> i32 {
    init_once();
    if card_busy() {
        return E_CARD_BUSY;
    }
    // a settings change younger than the write delay would be lost to the reset: write it now (a failure is in the log; the restart goes on)
    settings::flush_if_dirty();
    dlog::ev(logk::MENU, dlog::INFO, [logk::MENU_RESTART, 0, 0, 0, 0]);
    save_working_state();
    let sr = dkrt_irq_mask();
    if card_busy() {
        dkrt_irq_restore(sr);
        return E_CARD_BUSY;
    }
    dkrt_soft_reset()
}

/// Called from the UI task's event loop (`dkrt_loop_hook`, src/cf/tramp.s) once per pass, before it waits for the next
/// event. Registers d0, d1, a0, a1 are saved by the hook. Polls the mailbox, then calls slot 0.
#[no_mangle]
pub unsafe extern "C" fn dkrt_tick() {
    init_once();
    let s = st();
    s.ticks = s.ticks.wrapping_add(1);
    boot::ui_pass();
    upload::restart_poll();
    crash::install();
    dsp::step();
    sample::tick();
    tracks::tick();
    files::tick();
    fxpage::tick();
    pageclick::tick();
    settings::tick();
    watch::tick();
    let mb = &mut *addr_of_mut!(DKRT_MAILBOX);
    let cmd = read_volatile(addr_of!(mb.cmd));
    if cmd != 0 {
        // an if chain, not a `match`: a dense match becomes a PC-relative jump table, which is not ColdFire code
        let r = if cmd == CMD_LOAD {
            dkrt_load(mb.arg.as_ptr())
        } else if cmd == CMD_SELECT {
            mailbox_select(mb.arg.as_ptr(), 2)
        } else if cmd == CMD_SELECT_ONE {
            mailbox_select(mb.arg.as_ptr().add(1), *mb.arg.as_ptr() as u32)
        } else if cmd == CMD_RESTART {
            dkrt_restart()
        } else if cmd == CMD_HEALTH {
            dkrt_health(addr_of_mut!(DKRT_HEALTH));
            OK
        } else if cmd == CMD_CARD_PROBE {
            card::probe(mb.arg.as_ptr())
        } else if cmd >= CMD_SETTINGS_PUT && cmd <= CMD_SETTINGS_FLUSH {
            settings::mailbox(cmd, mb.arg.as_mut_ptr())
        } else {
            E_FORMAT
        };
        write_volatile(addr_of_mut!(mb.result), r);
        write_volatile(addr_of_mut!(mb.seq), mb.seq.wrapping_add(1));
        write_volatile(addr_of_mut!(mb.cmd), 0);
    }
    let f = read_volatile(addr_of!(s.slots[0]));
    if f != 0 {
        write_volatile(addr_of_mut!(s.entered[0]), s.entered[0] + 1);
        let call: unsafe extern "C" fn(*const Api) = core::mem::transmute(f as usize);
        // the module that owns slot 0 is the source of what it logs
        let mut owner = 0;
        while owner < MAX_MODULES && !(s.modules[owner].used != 0 && s.modules[owner].slot_mask & 1 != 0) {
            owner += 1;
        }
        let prev = if owner < MAX_MODULES { dlog::enter_module(owner) } else { 0 };
        call(addr_of!(dkrt_api));
        if owner < MAX_MODULES {
            dlog::leave_module(prev);
        }
        write_volatile(addr_of_mut!(s.entered[0]), s.entered[0] - 1);
        s.slot_calls = s.slot_calls.wrapping_add(1);
    }
    let p = read_volatile(addr_of!(DKRT_POLL));
    if p != 0 {
        let call: unsafe extern "C" fn() = core::mem::transmute(p);
        call();
    }
    let mut i = 0;
    while i < POLLS {
        let f = read_volatile(addr_of!(POLL_TABLE[i]));
        if f != 0 {
            let call: unsafe extern "C" fn() = core::mem::transmute(f);
            call();
        }
        i += 1;
    }
}

/// `CMD_SELECT` / `CMD_SELECT_ONE`: the mailbox argument is a list of NUL-terminated paths ended by an empty one. `copy` 0 or 1
/// writes that copy only; 2 writes both.
unsafe fn mailbox_select(arg: *const u8, copy: u32) -> i32 {
    let mut sel: DkrtSelection = core::mem::zeroed();
    let mut p = 0usize;
    let mut n = 0usize;
    loop {
        let len = cstr_len(arg.add(p), 95 - p.min(95));
        if len == 0 {
            break;
        }
        if n == SEL_MAX {
            return E_SEL_COUNT;
        }
        if len >= SEL_PATH {
            return E_SEL_PATH;
        }
        let mut i = 0;
        while i < len {
            sel.paths[n][i] = *arg.add(p + i);
            i += 1;
        }
        n += 1;
        p += len + 1;
        if p >= 95 {
            break;
        }
    }
    sel.count = n as u32;
    if copy == 2 {
        boot::selection_set(&sel)
    } else {
        boot::selection_set_one(copy, &sel)
    }
}

#[cfg(test)]
mod cleanup_tests;

#[cfg(test)]
pub fn irq_mask() -> u32 { 0 }
#[cfg(test)]
pub fn irq_restore(_: u32) {}

#[cfg(test)]
pub unsafe fn fw_blocks() -> Option<u32> { None }
