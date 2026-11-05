//! The DSP side of run-time modules (docs/design/digikit-dkm.md "DSP section", docs/findings/32-runtime-dsp-machines.md).
//!
//! A `.dkm` may carry SHARC+ code and data for the DSP. The runtime places it in a range of the DSP area (the `_dl_area` of
//! `core/dsp-link`, first fit, freed when the DSP part of a boot-time load fails), applies the relocations on the ColdFire, sends the bytes with `dl_send` and
//! publishes the module's voice rows. The transfer takes many audio blocks, so a load is a state machine that the UI-loop hook
//! advances (`step`); `dkrt_load` returns `LOADING` and the machine is listed only when the DSP code is acknowledged
//! and the voice rows are in.
//!
//! Voice rows (the voices registry of `core/dsp-voice`, scanned by the DSP for every voice it renders) are 24 words
//! (`ROW_WORDS`) and written one word per transfer through window 1 of `core/dsp-link`: the eight knob ranges (words 2 to 9: the
//! runtime guard clamps the machine's parameter words to them), the pc, the note_on pc (16), the state base (17), the state stride
//! (18) and the clear bytes (19), the guard's disabled-type word cleared, and the type
//! last (the row is still free until then: its type is `0xffffffff`, which no machine type equals), so the render scan never sees a
//! half-written row. A row is freed the other way round (type to `0xffffffff` first). The generation (20) and log level (21) words
//! are not written yet (0).
//!
//! Effect rows (DSP section version 4, `core/dynfx`): the rows of the effect chain of `core/fx` (24 words each, window 2 of `core/dsp-link`). An effect
//! record claims a free row (pc `0xffffffff`) when the range is reserved and publishes it after the code is acknowledged, one word per transfer: the
//! eight parameter words (the record's defaults), desc, state, `enabled` (1) and the pc last, so the chain never sees a half-written row. A row is freed
//! the other way round (the pc to `0xffffffff` first, then the adapter's `was`, `dis` and fault counters cleared). The effect's state is reserved in the
//! module's range behind the image, bss and voice states (`state_bytes` of its `dk_fx_desc`).
//!
//! Nothing here runs unless the file has a DSP section; the link to `core/dsp-link`, `core/dyndsp` and `core/dynfx` is optional.
use crate::{arena, cstr_eq, cstr_len, rd32};
use core::ptr::{addr_of_mut, read_volatile};
use dkrt_sdk::*;

const INFO_MAGIC: u32 = 0x4444_5350; // "DDSP"
const STATE_MAGIC: u32 = 0x444b_4453; // "DKDS"
/// Free voice rows core/dyndsp reserves: one per run-time machine type (a module's voice takes a row).
pub const MAX_ROWS: usize = DYN_TYPES as usize;
const MAX_VOICES: usize = MAX_ROWS;
const MAX_RELOCS: u32 = dkneeds::MAX_RELOCS;
const MAX_IMPORTS: u32 = dkneeds::MAX_IMPORTS;
const GRANULE: u32 = dkneeds::GRANULE;
/// Words of a voice row of `core/dsp-voice`: 0 type, 1 pc, 2 to 9 the range of knob 0 to 7 (min | max << 16), 10 the type the guard disabled, then its counters,
/// 16 note_on pc, 17 state base, 18 state stride, 19 clear bytes, 20 generation, 21 log level, 22 parameter latch mask, 23 spare.
const ROW_WORDS: u32 = 24;
/// Bytes of a voice entry in a DSP section: version 1 16 (type, entry), version 2 48 (type, entry, 8 ranges, 2 reserved),
/// version 3 64 (type, render, note_on, state_bytes, clear_bytes, 8 ranges, voices, latch mask, reserved).
const VOICE_V1: u32 = 16;
const VOICE_V2: u32 = 48;
const VOICE_V3: u32 = 64;
/// Most voice states a module may reserve.
const MAX_STATE_VOICES: u32 = dkneeds::MAX_STATE_VOICES;
/// Steps to publish a row: 8 ranges, the pc, note_on, state base, stride, clear, the latch mask, the cleared disabled word, the type.
const ROW_STEPS: u32 = 16;
/// Effect rows: the most a chain takes (`DK_FX_MAX`), the bytes of an effect record in a DSP section of version 4, the words of a row of `core/fx`, the pc of
/// a free row and the steps to publish a row: 8 parameter words, desc, state, mix, enabled, the 4 amount words and the route (finding 62), the pc.
pub const MAX_FX: usize = dkneeds::MAX_FX as usize;
const FX_REC: u32 = 64;
const FX_ROW_WORDS: u32 = 24;
const FX_FREE_PC: u32 = 0xffff_ffff;
const FX_STEPS: u32 = 18;
/// The words of an effect row (`core/fx` module.toml): pc, desc, state, enabled, p0 (to p7), and the adapter's was, dis, fault window start, count and total.
const FXW_PC: u32 = 0;
const FXW_DESC: u32 = 1;
const FXW_STATE: u32 = 2;
const FXW_ENABLED: u32 = 3;
const FXW_P0: u32 = 4;
const FXW_WAS: u32 = 12;
/// The row's dry/wet blend (a float; 1.0 is the wet signal, 0 the dry one), owned by the adapter and set by the MIX knob of the effect's page.
pub const FXW_MIX: u32 = 13;
/// 1.0f: the mix of a row without a page.
pub const MIX_ONE: u32 = 0x3f80_0000;
const FXW_DIS: u32 = 14;
const FXW_WSTART: u32 = 15;
const FXW_WCOUNT: u32 = 16;
/// The send route (finding 62): 0 master, 1 send bus; then the 16 amounts as 4 words, track t in word `FXW_AMT0 + t / 4`, byte `t % 4` (little endian).
pub const FXW_ROUTE: u32 = 18;
pub const FXW_AMT0: u32 = 19;
const FX_STEP_ROUTE: u32 = 12;
/// `dk_fx_desc` (<dk/fx.h>): magic "DKFX", contract, and its size in bytes.
const FX_DESC_MAGIC: u32 = 0x444b_4658;
const FX_DESC_BYTES: u32 = 28;
const FX_MAX_PARAMS: u32 = 8;
/// The windows of `core/dsp-link`.
const WIN_VOICES: u32 = 1;
const WIN_FX: u32 = 2;
const FX_INFO_MAGIC: u32 = 0x4446_5849; // "DFXI"
const L2_BYTE_BASE: u32 = 0x2000_0000;
const L2_SW_BASE: u32 = 0x00B8_0000;
/// The run-time machine types (`dynamic_first ..= last` of modules/core/adapters/machines/types.toml, through `patchkit gen`).
const TYPE_FIRST: u32 = device::DYN_TYPE_FIRST;
const TYPE_LAST: u32 = device::DYN_TYPE_LAST;
const DYN_TYPES: u32 = device::DYN_TYPES;
// the row count of the runtime's tables (`[limits] machine_rows`) is the count of run-time machine types that the firmware tables reserve
const _: () = assert!(device::LIMIT_MACHINE_ROWS == device::DYN_TYPES);
/// A free voice row's type: no machine type equals it.
const FREE_TYPE: u32 = 0xffff_ffff;
/// UI-loop passes a phase may last before the load is given up.
const GUARD_TICKS: u32 = 6000;
/// Polls a blocking one-word write may spin.
const SPIN: u32 = 4_000_000;

pub const PH_IDLE: u32 = 0;
pub const PH_SEND: u32 = 1;
pub const PH_ROWS: u32 = 2;
const PH_QUARANTINE: u32 = 3;
// Cleanup could not establish row invalidation/CF drain. Keep every allocation
// and transfer source untouched until reboot; unlike PH_QUARANTINE, never poll.
const PH_RETAINED: u32 = 4;

pub unsafe fn retained() -> bool { ds().phase == PH_RETAINED }

// words of `dyndsp_info` (byte offset / 4)
const I_MAGIC: usize = 0;
const I_DEVICE: usize = 2;
const I_AREA: usize = 3;
const I_AREA_BYTES: usize = 4;
const I_VROWS: usize = 5;
const I_VFIRST: usize = 6;
const I_VFREE: usize = 7;
const I_NIMPORT: usize = 9;
const I_IMPORTS: usize = 10;

// words of `dynfx_info` (byte offset / 4)
const F_MAGIC: usize = 0;
const F_DEVICE: usize = 2;
const F_ROWS: usize = 3;
const F_FIRST: usize = 4;
const F_FREE: usize = 5;
const F_WORDS: usize = 7;

extern "C" {
    // Symbols of core/dsp-link and core/dyndsp; each resolves to 0 when the module is not in the image (`[object] optional`).
    fn dl_send();
    fn dl_send_win();
    fn dl_poll();
    fn dyndsp_info();
    // core/dynfx: the effect chain's table; 0 when the module is not in the image.
    fn dynfx_info();
}

#[repr(C)]
pub struct DlStatus {
    pub state: u32,
    pub err: u32,
    pub nchunks: u32,
    pub acked: u32,
    pub sent: u32,
    pub resent: u32,
    pub nerr: u32,
}
const DL_DONE: u32 = 2;
const DL_ERROR: u32 = 3;

/// The CF-side half of a load whose DSP part is under way: everything is in place except publishing (the machine row, `init`,
/// the slots).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Pending {
    pub rec: u32,
    /// Copy of the `register` records (arena block), 0 if none.
    pub reg: u32,
    pub reg_size: u32,
    /// Absolute address of `dkm_init`, 0 if none.
    pub init: u32,
    pub mask: u32,
    pub fns: [u32; SLOTS],
    /// 1: the CF side went live at boot (machine rows, `init`, slots); only the DSP part is left, and a failure takes the module down.
    pub early: u32,
    /// The selection index of a module loaded by the boot hook (`u32::MAX`: none), for the boot report.
    pub boot_idx: u32,
    /// The image to send: byte address in the area, bytes, the arena block holding the relocated copy (set by `launch`).
    pub send_base: u32,
    pub send_len: u32,
    pub send_buf: u32,
}

/// The DSP loader's state, one object a host can peek (`DKRT_DSP` in plan.json symbols).
#[repr(C)]
pub struct DspState {
    pub magic: u32,
    pub phase: u32,
    pub ticks: u32,
    pub step: u32,
    /// A transfer has been started in this step.
    pub started: u32,
    /// The buffer of one-word transfers (`dl_send_win` reads it in place).
    pub word: u32,
    /// Byte address, bytes and arena block of the image `reserve` has prepared and `launch` has not taken yet.
    pub send_base: u32,
    pub send_len: u32,
    pub send_buf: u32,
    pub transfers: u32,
    pub row_writes: u32,
    pub failures: u32,
    pub last_dl_err: u32,
    /// Per module record: byte offset into the area and length of its range (len 0: none), and the rows it owns (bit mask).
    pub mod_off: [u32; crate::MAX_MODULES],
    pub mod_len: [u32; crate::MAX_MODULES],
    pub mod_rows: [u32; crate::MAX_MODULES],
    /// Per voice row: the type claimed for it (`FREE_TYPE` when free), its owner (module record + 1), whether its type word was
    /// written to the DSP (so it needs clearing), and the pc to publish.
    pub row_type: [u32; MAX_ROWS],
    pub row_pc: [u32; MAX_ROWS],
    /// Per voice row: the declared ranges of knobs 0 to 7 (min | max << 16, 0: undeclared).
    pub row_rng: [[u32; 8]; MAX_ROWS],
    pub row_owner: [u8; MAX_ROWS],
    pub row_pub: [u8; MAX_ROWS],
    /// The load being sent, and the loads behind it (the boot hook publishes the CF side of every module at once).
    pub pending: Pending,
    pub waiting: [Pending; crate::MAX_MODULES],
    pub nwait: u32,
    /// Per voice row: the note_on pc (0: none), the byte address of voice 0's state, the bytes between states, the bytes cleared at note-on.
    pub row_note_on: [u32; MAX_ROWS],
    pub row_state_base: [u32; MAX_ROWS],
    pub row_stride: [u32; MAX_ROWS],
    pub row_clear: [u32; MAX_ROWS],
    pub row_latched: [u32; MAX_ROWS],
    /// Per effect row (the free rows of `core/dynfx`, in order): the owner (module record + 1, 0: free), whether its pc word was written to the DSP (so it needs
    /// clearing), the pc, the byte addresses of the desc and the state, and the words p0 to p7 to publish.
    pub fx_owner: [u8; MAX_FX],
    pub fx_pub: [u8; MAX_FX],
    pub fx_pc: [u32; MAX_FX],
    pub fx_desc: [u32; MAX_FX],
    pub fx_state: [u32; MAX_FX],
    pub fx_par: [[u32; 8]; MAX_FX],
    /// Per effect row: the MIX word to publish (`MIX_ONE` unless the effect's page says otherwise).
    pub fx_mix: [u32; MAX_FX],
    /// Per effect row: the route and the four amount words to publish (before the pc: a send effect never runs on the master).
    pub fx_route: [u32; MAX_FX],
    pub fx_amt: [[u32; 4]; MAX_FX],
    /// Per effect row: the flags word of its record (`FXF_*`: the routes the effect may run on and its default route; 0 = master only).
    pub fx_flags: [u32; MAX_FX],
    /// Per module record: the effect rows it owns (bit mask).
    pub mod_fx: [u32; crate::MAX_MODULES],
}

#[no_mangle]
pub static mut DKRT_DSP: DspState = unsafe { core::mem::zeroed() };

pub fn ds() -> &'static mut DspState {
    unsafe { &mut *addr_of_mut!(DKRT_DSP) }
}

fn fn_addr(f: unsafe extern "C" fn()) -> usize {
    core::hint::black_box(f as usize)
}

/// The info table of `core/dyndsp`, or null.
unsafe fn info() -> *const u32 {
    #[cfg(test)] { return crate::cleanup_tests::voice_info(); }
    #[cfg(not(test))] {
        let a = fn_addr(dyndsp_info);
        if a == 0 {
            return core::ptr::null();
        }
        let p = a as *const u32;
        if read_volatile(p.add(I_MAGIC)) != INFO_MAGIC {
            return core::ptr::null();
        }
        p
    }
}

unsafe fn iw(k: usize) -> u32 {
    read_volatile(info().add(k))
}

/// The table of `core/dynfx`, or null (also when it is for another device or has another row size than the runtime knows).
unsafe fn finfo() -> *const u32 {
    #[cfg(test)] { return crate::cleanup_tests::fx_info(); }
    #[cfg(not(test))] {
        let a = fn_addr(dynfx_info);
        if a == 0 || info().is_null() {
            return core::ptr::null();
        }
        let p = a as *const u32;
        if read_volatile(p.add(F_MAGIC)) != FX_INFO_MAGIC
            || read_volatile(p.add(F_DEVICE)) != iw(I_DEVICE)
            || read_volatile(p.add(F_WORDS)) != FX_ROW_WORDS
        {
            return core::ptr::null();
        }
        p
    }
}

unsafe fn fiw(k: usize) -> u32 {
    read_volatile(finfo().add(k))
}

/// Effect rows the image reserves for run-time effects (0 without `core/dynfx`).
pub unsafe fn fx_free_rows() -> u32 {
    if finfo().is_null() {
        0
    } else {
        fiw(F_FREE).min(MAX_FX as u32)
    }
}

/// Effect rows in use (claimed by a module that is loaded or loading), for CAPACITY.
pub unsafe fn fx_used() -> u32 {
    let d = ds();
    let mut used = 0;
    let mut k = 0;
    while k < MAX_FX {
        if d.fx_owner[k] != 0 {
            used += 1;
        }
        k += 1;
    }
    used
}

/// The module record + 1 that owns effect row `k` (0: free) and the row's index in the chain's table (the registry `fx`), for the effect pages.
pub unsafe fn fx_owner_of(k: usize) -> u8 {
    if k < MAX_FX {
        ds().fx_owner[k]
    } else {
        0
    }
}

/// The effect row (0 ..) that module record `owner - 1` claimed for its `ordinal`-th effect record (rows are claimed in record order, lowest first).
pub unsafe fn fx_row_of(owner: u8, ordinal: u32) -> Option<usize> {
    let d = ds();
    let mut seen = 0;
    let mut k = 0;
    while k < MAX_FX {
        if d.fx_owner[k] == owner {
            if seen == ordinal {
                return Some(k);
            }
            seen += 1;
        }
        k += 1;
    }
    None
}

/// The flags word of the record that effect row `k` was claimed for (`FXF_SEND`, `FXF_NOMASTER`, `FXF_DEFAULT_SEND`; 0 for a free row).
pub unsafe fn fx_flags_of(k: usize) -> u32 {
    if k < MAX_FX {
        ds().fx_flags[k]
    } else {
        0
    }
}

/// Set the MIX word an effect row publishes (before its rows go out) and the defaults of its parameter words are the record's.
pub unsafe fn fx_set_mix(k: usize, bits: u32) {
    if k < MAX_FX {
        ds().fx_mix[k] = bits;
    }
}

/// Set the route and the four amount words an effect row publishes (before its rows go out; `fxsend::apply` does it from the settings).
pub unsafe fn fx_set_route(k: usize, route: u32, amt: [u32; 4]) {
    if k < MAX_FX {
        let d = ds();
        d.fx_route[k] = route;
        d.fx_amt[k] = amt;
    }
}

/// An effect row whose words are in the DSP: claimed, published and no load in progress, so a one-word write can go out.
pub unsafe fn fx_live(k: usize) -> bool {
    let d = ds();
    k < MAX_FX && d.fx_owner[k] != 0 && d.fx_pub[k] != 0 && d.phase == PH_IDLE && d.nwait == 0 && !crate::sample::busy() && !crate::tracks::busy()
}

/// The effect row's words in the chain: word `w` of row `k` (a one-word write through window 2).
pub unsafe fn fx_word_start(k: usize, w: u32, value: u32) -> bool {
    if finfo().is_null() {
        return false;
    }
    start_word(WIN_FX, FX_ROW_WORDS * (fiw(F_FIRST) + k as u32) + w, value)
}

/// `Some(true)` done, `Some(false)` failed, `None` still running (a write started by `fx_word_start`).
pub unsafe fn fx_word_poll() -> Option<bool> {
    poll_done()
}

unsafe fn have_link() -> bool {
    fn_addr(dl_send) != 0 && fn_addr(dl_send_win) != 0 && fn_addr(dl_poll) != 0 && !info().is_null()
}

unsafe fn call_send(dst: u32, buf: *const u8, len: u32) -> i32 {
    if retained() { return -2; }
    #[cfg(test)] { return crate::cleanup_tests::send_image(dst, buf, len); }
    #[cfg(not(test))] {
        let f: unsafe extern "C" fn(u32, *const u8, u32) -> i32 = core::mem::transmute(fn_addr(dl_send));
        f(dst, buf, len)
    }
}

unsafe fn call_send_win(win: u32, dst: u32, buf: *const u32, len: u32) -> i32 {
    #[cfg(test)] { return crate::cleanup_tests::send_word(win, dst, buf, len); }
    #[cfg(not(test))] {
        let f: unsafe extern "C" fn(u32, u32, *const u32, u32) -> i32 =
            core::mem::transmute(fn_addr(dl_send_win));
        f(win, dst, buf, len)
    }
}

unsafe fn call_poll(s: *mut DlStatus) -> i32 {
    #[cfg(test)] {
        let (state, err) = crate::cleanup_tests::poll_state();
        (*s).state = state;
        (*s).err = err;
        return 0;
    }
    #[cfg(not(test))] {
        let f: unsafe extern "C" fn(*mut DlStatus) -> i32 = core::mem::transmute(fn_addr(dl_poll));
        f(s)
    }
}

pub unsafe fn init_once() {
    let d = ds();
    if d.magic == STATE_MAGIC {
        return;
    }
    let mut i = 0;
    while i < MAX_ROWS {
        d.row_type[i] = FREE_TYPE;
        i += 1;
    }
    d.magic = STATE_MAGIC;
}

// ---- the area ----------------------------------------------------------------------------------------------------------

/// Bytes of the area (0: the image has no DSP loader).
pub unsafe fn area_bytes() -> u32 {
    if info().is_null() {
        0
    } else {
        iw(I_AREA_BYTES)
    }
}

/// Free bytes in the area and the largest free range, for `dkrt_status`.
pub unsafe fn area_stats() -> (u32, u32) {
    let total = area_bytes();
    if total == 0 {
        return (0, 0);
    }
    let d = ds();
    let mut used = 0;
    let mut i = 0;
    while i < crate::MAX_MODULES {
        used += d.mod_len[i];
        i += 1;
    }
    let mut largest = 0;
    let mut cursor = 0u32;
    loop {
        let mut next = total;
        i = 0;
        while i < crate::MAX_MODULES {
            if d.mod_len[i] != 0 && d.mod_off[i] >= cursor && d.mod_off[i] < next {
                next = d.mod_off[i];
            }
            i += 1;
        }
        if next - cursor > largest {
            largest = next - cursor;
        }
        if next >= total {
            break;
        }
        let mut len = 0;
        i = 0;
        while i < crate::MAX_MODULES {
            if d.mod_len[i] != 0 && d.mod_off[i] == next {
                len = d.mod_len[i];
            }
            i += 1;
        }
        cursor = next + len;
    }
    (total - used, largest)
}

/// (voice rows the area's table reserves, rows in use); (0, 0) without a DSP loader (for CAPACITY).
pub unsafe fn rows_counts() -> (u32, u32) {
    if info().is_null() {
        return (0, 0);
    }
    let used = capacity::used_rows(&ds().row_owner);
    (iw(I_VFREE).min(MAX_ROWS as u32), used)
}

/// First fit: the lowest offset whose byte address is `align`-aligned and whose `len` bytes touch no other range.
unsafe fn alloc_range(len: u32, align: u32) -> Option<u32> {
    let d = ds();
    capacity::first_fit(iw(I_AREA), iw(I_AREA_BYTES), &d.mod_off, &d.mod_len, len, align)
}

// ---- checking a section --------------------------------------------------------------------------------------------------

/// What `check` found in a DSP section; pointers are into the staged file.
pub struct Job {
    pub image: *const u8,
    pub image_size: u32,
    pub bss: u32,
    pub align: u32,
    pub relocs: *const u8,
    pub nreloc: u32,
    pub imports: *const u8,
    pub nimport: u32,
    pub voices: *const u8,
    pub nvoice: u32,
    /// Bytes of a voice entry (`VOICE_V1`, `VOICE_V2` or `VOICE_V3`).
    pub vstride: u32,
    /// The effect records (version 4), `FX_REC` bytes each.
    pub fxs: *const u8,
    pub nfx: u32,
    pub bank: *const u8,
    import_vals: [u32; MAX_IMPORTS as usize],
}

impl Job {
    pub const fn empty() -> Job {
        Job {
            image: core::ptr::null(),
            image_size: 0,
            bss: 0,
            align: 0,
            relocs: core::ptr::null(),
            nreloc: 0,
            imports: core::ptr::null(),
            nimport: 0,
            voices: core::ptr::null(),
            nvoice: 0,
            vstride: VOICE_V2,
            fxs: core::ptr::null(),
            nfx: 0,
            bank: core::ptr::null(),
            import_vals: [0; MAX_IMPORTS as usize],
        }
    }
}

unsafe fn in_sec(off: u32, len: u32, size: u32) -> bool {
    off <= size && len <= size - off
}

/// Bytes of the range a module needs, before the granule: image, bss, then (version 3) the state blocks of its voices, every block
/// of `voices * state_bytes` rounded to 8 bytes, then (version 4) the state of each effect rounded to 8. The caller has checked the counts, so nothing overflows.
unsafe fn extent(image_size: u32, bss: u32, voices: *const u8, nvoice: u32, vstride: u32, fxs: *const u8, nfx: u32) -> u32 {
    let mut total = (image_size + bss + 7) & !7;
    if vstride == VOICE_V3 {
        let mut i = 0;
        while i < nvoice {
            let e = voices.add((vstride * i) as usize);
            total += rd32(e.add(52)) * ((rd32(e.add(12)) + 7) & !7);
            i += 1;
        }
    }
    // then the state of each effect (version 4)
    let mut i = 0;
    while i < nfx {
        total += (rd32(fxs.add((FX_REC * i) as usize + 8)) + 7) & !7;
        i += 1;
    }
    total
}

/// The little-endian word at `p` (the DSP image's own byte order).
unsafe fn rd32le(p: *const u8) -> u32 {
    (*p as u32) | ((*p.add(1) as u32) << 8) | ((*p.add(2) as u32) << 16) | ((*p.add(3) as u32) << 24)
}

/// An exported DSP symbol of `dyndsp_info`; 0 when there is none of that name.
unsafe fn dsp_symbol(name: *const u8) -> u32 {
    let n = iw(I_NIMPORT);
    let mut i = 0;
    while i < n {
        let e = (info().add(I_IMPORTS) as *const u8).add(32 * i as usize);
        if cstr_eq(e, name) {
            return read_volatile(e.add(28) as *const u32);
        }
        i += 1;
    }
    0
}

/// Validate the DSP section `[off, off + size)` of the staged file `f` for this runtime and image. Writes only `job`.
pub unsafe fn check(f: *const u8, off: u32, size: u32, file_size: u32, job: &mut Job) -> i32 {
    init_once();
    if !in_sec(off, size, file_size) || size < 0x40 {
        return E_DSP_FORMAT;
    }
    let s = f.add(off as usize);
    let version = rd32(s.add(4));
    if rd32(s) != STATE_MAGIC || (version != 1 && version != 2 && version != 3 && version != 4 && version != 5) {
        return E_DSP_FORMAT;
    }
    if !have_link() {
        return E_NO_DSP;
    }
    if rd32(s.add(8)) != iw(I_DEVICE) {
        return E_DSP_DEVICE;
    }
    let image_size = rd32(s.add(0x0c));
    let bss = rd32(s.add(0x10));
    let align = rd32(s.add(0x14));
    let (image_off, reloc_off, nreloc) = (rd32(s.add(0x18)), rd32(s.add(0x1c)), rd32(s.add(0x20)));
    let (import_off, nimport) = (rd32(s.add(0x24)), rd32(s.add(0x28)));
    let (voice_off, nvoice) = (rd32(s.add(0x2c)), rd32(s.add(0x30)));
    // version 4: the effect records (0x34 offset, 0x38 count)
    let (fx_off, nfx) = if version >= 4 { (rd32(s.add(0x34)), rd32(s.add(0x38))) } else { (0, 0) };
    let vstride = match version {
        1 => VOICE_V1,
        2 => VOICE_V2,
        _ => VOICE_V3,
    };
    if image_size > iw(I_AREA_BYTES)
        || bss > iw(I_AREA_BYTES)
        || image_size == 0
        || image_size & 3 != 0
        || bss & 3 != 0
        || !(8..=GRANULE).contains(&align)
        || align & (align - 1) != 0
        || nreloc > MAX_RELOCS
        || nimport > MAX_IMPORTS
        || nvoice as usize > MAX_VOICES
        || nfx as usize > MAX_FX
        || !in_sec(fx_off, nfx * FX_REC, size)
        || !in_sec(image_off, image_size, size)
        || !in_sec(reloc_off, nreloc * 8, size)
        || !in_sec(import_off, nimport * 32, size)
        || !in_sec(voice_off, nvoice * vstride, size)
    {
        return E_DSP_FORMAT;
    }
    let bank_off = if version == 5 { rd32(s.add(0x3c)) } else { 0 };
    if version == 5 {
        if bank_off < 64 || bank_off & 3 != 0 || !in_sec(bank_off, 32, size) {
            return E_DSP_FORMAT;
        }
        // The record must not alias another section table or the initialized image.
        for (o, n) in [
            (image_off, image_size),
            (reloc_off, nreloc * 8),
            (import_off, nimport * 32),
            (voice_off, nvoice * vstride),
            (fx_off, nfx * FX_REC),
        ] {
            if n != 0 && bank_off < o + n && o < bank_off + 32 {
                return E_DSP_FORMAT;
            }
        }
        let regions = [
            (image_off, image_size),
            (reloc_off, nreloc * 8),
            (import_off, nimport * 32),
            (voice_off, nvoice * vstride),
            (fx_off, nfx * FX_REC),
        ];
        for (i, &(o, n)) in regions.iter().enumerate() {
            if n != 0 && o < 64 {
                return E_DSP_FORMAT;
            }
            for &(p, m) in &regions[..i] {
                if n != 0 && m != 0 && o < p + m && p < o + n {
                    return E_DSP_FORMAT;
                }
            }
        }
        let b = s.add(bank_off as usize);
        let record = core::slice::from_raw_parts(b, 32);
        let image = core::slice::from_raw_parts(s.add(image_off as usize), image_size as usize);
        // The record is a bank (kind 1) with its descriptor, or a per-track table (kind 2) with its two blocks.
        if rd32le(b) == 2 {
            if !dkrt_sdk::sample_job::valid_tracks(record, image) {
                return E_DSP_FORMAT;
            }
            // Relocations cannot turn the initially empty table into a published one.
            let (records, control) = (rd32le(b.add(8)), rd32le(b.add(12)));
            for k in 0..nreloc {
                let at = rd32(s.add((reloc_off + k * 8) as usize));
                if (at < records + dkrt_sdk::sample_job::TRACK_RECORDS_BYTES && records < at.saturating_add(6))
                    || (at < control + dkrt_sdk::sample_job::TRACK_CONTROL_BYTES && control < at.saturating_add(6))
                {
                    return E_DSP_FORMAT;
                }
            }
        } else {
            if !dkrt_sdk::sample_job::valid_bank(record, image, bss) {
                return E_DSP_FORMAT;
            }
            let desc = rd32le(b.add(16));
            // Relocations cannot turn the initially empty descriptor into a published one.
            for k in 0..nreloc {
                let at = rd32(s.add((reloc_off + k * 8) as usize));
                if at < desc + 16 && desc < at.saturating_add(6) {
                    return E_DSP_FORMAT;
                }
            }
        }
    }
    // version 3 voices: the state counts must be sane before the range is computed from them
    if vstride == VOICE_V3 {
        let mut i = 0;
        while i < nvoice {
            let e = s.add((voice_off + vstride * i) as usize);
            let (note_on, state, clear, count) = (rd32(e.add(8)), rd32(e.add(12)), rd32(e.add(16)), rd32(e.add(52)));
            if note_on >= image_size || note_on & 1 != 0 || clear > state || state > iw(I_AREA_BYTES) || count > MAX_STATE_VOICES {
                return E_DSP_FORMAT;
            }
            let latched = rd32(e.add(56));
            if latched & !0xff != 0 || (latched != 0 && note_on == 0) || rd32(e.add(60)) != 0 {
                return E_DSP_FORMAT;
            }
            i += 1;
        }
    }
    // effect records: entries and descriptor inside the image, the descriptor says what the record says (the adapter zeroes `state_bytes` of the
    // descriptor at the state address: it must not be more than the range holds)
    let mut i = 0;
    while i < nfx {
        let e = s.add((fx_off + FX_REC * i) as usize);
        let (process, desc, state, np) = (rd32(e), rd32(e.add(4)), rd32(e.add(8)), rd32(e.add(12)));
        if process >= image_size
            || process & 1 != 0
            || desc & 3 != 0
            || !in_sec(desc, FX_DESC_BYTES, image_size)
            || state & 3 != 0
            || state > iw(I_AREA_BYTES)
            || np > FX_MAX_PARAMS
        {
            return E_DSP_FORMAT;
        }
        // the routes: send allowed (bit 0), master not allowed (bit 1), default send (bit 2); the default must be an allowed route (0 is a record before the routes)
        let fl = rd32(e.add(16));
        if fl & !7 != 0 || (fl & 6 != 0 && fl & 1 == 0) || (fl & 2 != 0 && fl & 4 == 0) {
            return E_DSP_FORMAT;
        }
        let dd = s.add((image_off + desc) as usize);
        if rd32le(dd) != FX_DESC_MAGIC || rd32le(dd.add(4)) != 1 || rd32le(dd.add(8)) != np || rd32le(dd.add(12)) != state {
            return E_DSP_FORMAT;
        }
        i += 1;
    }
    // the range must fit the area at all (a later load may still find it taken: reserve())
    let need = extent(image_size, bss, s.add(voice_off as usize), nvoice, vstride, s.add(fx_off as usize), nfx);
    if need > iw(I_AREA_BYTES) || (need + GRANULE - 1) & !(GRANULE - 1) > iw(I_AREA_BYTES) {
        return E_DSP_NOSPACE;
    }
    let mut i = 0;
    while i < nimport {
        let p = s.add(import_off as usize + 32 * i as usize);
        if cstr_len(p, 32) == 32 {
            return E_DSP_FORMAT;
        }
        let v = dsp_symbol(p);
        if v == 0 {
            return E_DSP_IMPORT;
        }
        job.import_vals[i as usize] = v;
        i += 1;
    }
    i = 0;
    while i < nreloc {
        let e = s.add(reloc_off as usize + 8 * i as usize);
        let o = rd32(e);
        let w = rd32(e.add(4));
        let (kind, class, imp) = (w >> 28, (w >> 24) & 15, w & 0xffff);
        if o > image_size - 6 || kind > 1 || class > 2 || (class == 2 && imp >= nimport) || (kind == 0 && class == 1) {
            return E_DSP_FORMAT;
        }
        i += 1;
    }
    // voices: types in the run-time range, entries inside the image, rows available
    let d = ds();
    let r = capacity::dsp_rows(nvoice, capacity::free_rows(&d.row_owner, iw(I_VFREE)));
    if r != OK {
        return r;
    }
    // effect rows: the image must have the chain's table, and a free row for each record
    if nfx > 0 {
        if finfo().is_null() {
            return E_NO_DSP;
        }
        let r = capacity::dsp_rows(nfx, capacity::free_rows(&d.fx_owner, fx_free_rows()));
        if r != OK {
            return r;
        }
    }
    i = 0;
    while i < nvoice {
        let e = s.add(voice_off as usize + vstride as usize * i as usize);
        // the voice names its machine by key (or by a pinned number): `assign` has the type
        let t = crate::assign::resolve(rd32(e));
        let entry = rd32(e.add(4));
        if !(TYPE_FIRST..=TYPE_LAST).contains(&t) || entry >= image_size || entry & 1 != 0 {
            return E_DSP_TYPE;
        }
        let mut k = 0;
        while k < MAX_ROWS {
            if d.row_owner[k] != 0 && d.row_type[k] == t {
                return E_DSP_TYPE; // used by a module that is loaded or loading
            }
            k += 1;
        }
        let mut j = 0;
        while j < i {
            if crate::assign::resolve(rd32(s.add(voice_off as usize + vstride as usize * j as usize))) == t {
                return E_DSP_TYPE; // twice in this file
            }
            j += 1;
        }
        i += 1;
    }
    job.image = s.add(image_off as usize);
    job.image_size = image_size;
    job.bss = bss;
    job.align = align;
    job.relocs = s.add(reloc_off as usize);
    job.nreloc = nreloc;
    job.imports = s.add(import_off as usize);
    job.nimport = nimport;
    job.voices = s.add(voice_off as usize);
    job.nvoice = nvoice;
    job.vstride = vstride;
    job.fxs = s.add(fx_off as usize);
    job.nfx = nfx;
    job.bank = if version == 5 { s.add(bank_off as usize) } else { core::ptr::null() };
    OK
}

// ---- reserving: range, relocated image, voice rows ------------------------------------------------------------------------

unsafe fn le16(p: *const u8) -> u32 {
    (*p as u32) | ((*p.add(1) as u32) << 8)
}

unsafe fn put_le16(p: *mut u8, v: u32) {
    *p = v as u8;
    *p.add(1) = (v >> 8) as u8;
}

/// Reserve everything a module's DSP part needs: a range in the area, an arena block with the relocated image, the voice rows.
/// Nothing is sent yet. On an error nothing is left reserved.
pub unsafe fn reserve(rec: usize, job: &Job) -> i32 {
    if retained() { return E_BUSY; }
    // a track table needs a free client slot: refuse before anything is reserved
    if !job.bank.is_null() && rd32le(job.bank) == 2 && !crate::tracks::can_reserve() {
        return E_TABLE_FULL;
    }
    let d = ds();
    let len = (extent(job.image_size, job.bss, job.voices, job.nvoice, job.vstride, job.fxs, job.nfx) + GRANULE - 1) & !(GRANULE - 1);
    let off = match alloc_range(len, job.align) {
        Some(o) => o,
        None => return E_DSP_NOSPACE,
    };
    let buf = arena::alloc(job.image_size);
    if buf.is_null() {
        return E_NOMEM;
    }
    let base = iw(I_AREA) + off;
    // copy, then relocate: abs24 / abs32 fields of 48-bit instructions, shifted by where the object sits in L2
    let mut i = 0;
    while i < job.image_size as usize {
        *buf.add(i) = *job.image.add(i);
        i += 1;
    }
    let pc_shift = (base - L2_BYTE_BASE) / 2;
    let byte_shift = base - L2_BYTE_BASE;
    i = 0;
    while i < job.nreloc as usize {
        let e = job.relocs.add(8 * i);
        let o = rd32(e) as usize;
        let w = rd32(e.add(4));
        let (kind, class, imp) = (w >> 28, (w >> 24) & 15, (w & 0xffff) as usize);
        let add = match class {
            0 => pc_shift,
            1 => byte_shift,
            _ => job.import_vals[imp],
        };
        let p = buf.add(o);
        if kind == 0 {
            let v = (((*p.add(2)) as u32) << 16 | le16(p.add(4))).wrapping_add(add);
            if v >= 1 << 24 {
                arena::free(buf);
                return E_DSP_FORMAT;
            }
            *p.add(2) = (v >> 16) as u8;
            put_le16(p.add(4), v & 0xffff);
        } else {
            let v = (le16(p.add(2)) << 16 | le16(p.add(4))).wrapping_add(add);
            put_le16(p.add(2), v >> 16);
            put_le16(p.add(4), v & 0xffff);
        }
        i += 1;
    }
    // claim the voice rows (lowest free ones first)
    let mut claimed = 0u32;
    let mut v = 0;
    let mut row = 0;
    // version 3: the state blocks follow image and bss (8-aligned), one block of `voices * stride` per voice record, in record order
    let mut state_at = (job.image_size + job.bss + 7) & !7;
    while v < job.nvoice as usize {
        while row < MAX_ROWS && (d.row_owner[row] != 0 || (row as u32) >= iw(I_VFREE)) {
            row += 1;
        }
        let e = job.voices.add(job.vstride as usize * v);
        d.row_type[row] = crate::assign::resolve(rd32(e));
        d.row_pc[row] = L2_SW_BASE + (base + rd32(e.add(4)) - L2_BYTE_BASE) / 2;
        let mut k = 0;
        while k < 8 {
            d.row_rng[row][k] = match job.vstride {
                VOICE_V2 => rd32(e.add(8 + 4 * k)),
                VOICE_V3 => rd32(e.add(20 + 4 * k)),
                _ => 0,
            };
            k += 1;
        }
        if job.vstride == VOICE_V3 {
            let note_on = rd32(e.add(8));
            let stride = (rd32(e.add(12)) + 7) & !7;
            d.row_note_on[row] = if note_on == 0 { 0 } else { L2_SW_BASE + (base + note_on - L2_BYTE_BASE) / 2 };
            // the byte address of a data symbol at image offset `state_at`: nominal L2_BYTE_BASE + state_at, plus the byte shift
            d.row_state_base[row] = L2_BYTE_BASE + state_at + byte_shift;
            d.row_stride[row] = stride;
            d.row_clear[row] = rd32(e.add(16));
            d.row_latched[row] = rd32(e.add(56));
            state_at += rd32(e.add(52)) * stride;
        } else {
            d.row_note_on[row] = 0;
            d.row_state_base[row] = 0;
            d.row_stride[row] = 0;
            d.row_clear[row] = 0;
            d.row_latched[row] = 0;
        }
        d.row_owner[row] = rec as u8 + 1;
        d.row_pub[row] = 0;
        claimed |= 1 << row;
        row += 1;
        v += 1;
    }
    // claim the effect rows (lowest free ones first); the states follow the voice states, in record order
    let mut fclaimed = 0u32;
    let mut f = 0;
    let mut frow = 0;
    while f < job.nfx as usize {
        while frow < MAX_FX && (d.fx_owner[frow] != 0 || (frow as u32) >= fx_free_rows()) {
            frow += 1;
        }
        let e = job.fxs.add(FX_REC as usize * f);
        d.fx_pc[frow] = L2_SW_BASE + (base + rd32(e) - L2_BYTE_BASE) / 2;
        d.fx_desc[frow] = base + rd32(e.add(4));
        d.fx_state[frow] = base + state_at;
        state_at += (rd32(e.add(8)) + 7) & !7;
        let mut k = 0;
        while k < 8 {
            d.fx_par[frow][k] = rd32(e.add(20 + 4 * k));
            k += 1;
        }
        d.fx_mix[frow] = MIX_ONE;
        d.fx_route[frow] = 0;
        d.fx_amt[frow] = [0; 4];
        d.fx_flags[frow] = rd32(e.add(16));
        d.fx_owner[frow] = rec as u8 + 1;
        d.fx_pub[frow] = 0;
        fclaimed |= 1 << frow;
        frow += 1;
        f += 1;
    }
    if !job.bank.is_null() {
        if rd32le(job.bank) == 2 {
            // a client slot was checked free at the start of this function
            crate::tracks::reserve(rec, rd32le(job.bank.add(4)), base + rd32le(job.bank.add(8)), base + rd32le(job.bank.add(12)));
        } else {
            crate::sample::reserve(
                rec,
                rd32le(job.bank.add(4)),
                base + rd32le(job.bank.add(8)),
                rd32le(job.bank.add(12)),
                base + rd32le(job.bank.add(16)),
            );
        }
    }
    d.mod_fx[rec] = fclaimed;
    d.mod_off[rec] = off;
    d.mod_len[rec] = len;
    d.mod_rows[rec] = claimed;
    d.send_base = base;
    d.send_len = job.image_size;
    d.send_buf = buf as u32;
    OK
}

/// Give back what `reserve` took for module record `rec` (the rows must have been cleared first).
unsafe fn unreserve(rec: usize) {
    crate::sample::remove(rec);
    crate::tracks::remove(rec);
    let d = ds();
    let mut k = 0;
    while k < MAX_ROWS {
        if d.row_owner[k] == rec as u8 + 1 {
            d.row_owner[k] = 0;
            d.row_type[k] = FREE_TYPE;
            d.row_pub[k] = 0;
            d.row_pc[k] = 0;
            d.row_rng[k] = [0; 8];
            d.row_note_on[k] = 0;
            d.row_state_base[k] = 0;
            d.row_stride[k] = 0;
            d.row_clear[k] = 0;
            d.row_latched[k] = 0;
        }
        k += 1;
    }
    k = 0;
    while k < MAX_FX {
        if d.fx_owner[k] == rec as u8 + 1 {
            d.fx_owner[k] = 0;
            d.fx_pub[k] = 0;
            d.fx_pc[k] = 0;
            d.fx_desc[k] = 0;
            d.fx_state[k] = 0;
            d.fx_par[k] = [0; 8];
            d.fx_mix[k] = MIX_ONE;
            d.fx_route[k] = 0;
            d.fx_amt[k] = [0; 4];
            d.fx_flags[k] = 0;
        }
        k += 1;
    }
    d.mod_off[rec] = 0;
    d.mod_len[rec] = 0;
    d.mod_rows[rec] = 0;
    d.mod_fx[rec] = 0;
}

/// The module record (index) that owns machine type `t`.
pub unsafe fn module_of_type(t: u32) -> Option<usize> {
    let d = ds();
    let mut k = 0;
    while k < MAX_ROWS {
        if d.row_owner[k] != 0 && d.row_type[k] == t {
            return Some(d.row_owner[k] as usize - 1);
        }
        k += 1;
    }
    None
}

/// The module record (index) that owns row `r` of the effect table (the registry `fx`), for an effect fault.
pub unsafe fn module_of_fx_table_row(r: u32) -> Option<usize> {
    if finfo().is_null() {
        return None;
    }
    let first = fiw(F_FIRST);
    if r < first || ((r - first) as usize) >= MAX_FX {
        return None;
    }
    let owner = ds().fx_owner[(r - first) as usize];
    if owner == 0 {
        None
    } else {
        Some(owner as usize - 1)
    }
}

/// Undo a `reserve` that was never started (the CF side failed after it).
pub unsafe fn cancel(rec: usize) {
    if retained() { return; }
    let d = ds();
    if d.send_buf != 0 {
        arena::free(d.send_buf as *mut u8);
        d.send_buf = 0;
    }
    unreserve(rec);
}

// ---- one-word writes into the voices registry ---------------------------------------------------------------------------------

/// Start writing `value` into word `word` of window `win` (1 the voice rows, 2 the effect rows); false when the link refused (busy: try again).
unsafe fn start_word(win: u32, word: u32, value: u32) -> bool {
    if retained() { return false; }
    let d = ds();
    // the link sends the bytes of the buffer as little-endian DSP words; this is a big-endian ColdFire long
    d.word = (value << 24) | ((value & 0xff00) << 8) | ((value >> 8) & 0xff00) | (value >> 24); // byte swap by hand: no libgcc here
    call_send_win(win, word, &d.word as *const u32, 4) == 0
}

/// `Some(true)` done, `Some(false)` failed, `None` still running.
unsafe fn poll_done() -> Option<bool> {
    let mut s = DlStatus {
        state: 0,
        err: 0,
        nchunks: 0,
        acked: 0,
        sent: 0,
        resent: 0,
        nerr: 0,
    };
    call_poll(&mut s);
    match s.state {
        DL_DONE => Some(true),
        DL_ERROR => {
            ds().last_dl_err = s.err;
            Some(false)
        }
        _ => None,
    }
}

/// Write one word and wait for it (the UI task spins; a few audio blocks). Used on the error paths.
unsafe fn write_word_blocking(win: u32, word: u32, value: u32) -> bool {
    let mut n = 0;
    while !start_word(win, word, value) {
        n += 1;
        if n > SPIN {
            return false;
        }
        core::hint::black_box(n);
    }
    ds().row_writes += 1;
    n = 0;
    loop {
        match poll_done() {
            Some(ok) => return ok,
            None => {
                n += 1;
                if n > SPIN {
                    return false;
                }
                core::hint::black_box(n);
            }
        }
    }
}

/// Free the rows module record `rec` published: the type word first, so the scan skips the row at once.
unsafe fn clear_rows(rec: usize) -> bool {
    let d = ds();
    let first = iw(I_VFIRST);
    let mut k = 0;
    while k < MAX_ROWS {
        if d.row_owner[k] == rec as u8 + 1 && d.row_pub[k] != 0 {
            if !write_word_blocking(WIN_VOICES, ROW_WORDS * (first + k as u32), FREE_TYPE) { return false; }
            d.row_pub[k] = 0;
        }
        k += 1;
    }
    true
}

/// Free the effect rows module record `rec` published: the pc first, so the chain skips the row at once; then the adapter's words (`was`, `dis`, the fault
/// window and its count) back to 0, so the next effect on the row starts clean.
unsafe fn clear_fx_rows(rec: usize) -> bool {
    let d = ds();
    if finfo().is_null() {
        // Missing info is harmless only when no owned row was published.
        let mut k = 0;
        while k < MAX_FX {
            if d.fx_owner[k] == rec as u8 + 1 && d.fx_pub[k] != 0 { return false; }
            k += 1;
        }
        return true;
    }
    let first = fiw(F_FIRST);
    let mut k = 0;
    while k < MAX_FX {
        if d.fx_owner[k] == rec as u8 + 1 && d.fx_pub[k] != 0 {
            let at = FX_ROW_WORDS * (first + k as u32);
            if !write_word_blocking(WIN_FX, at + FXW_PC, FX_FREE_PC) { return false; }
            // the next effect on the row starts on the master route with no amounts (finding 62)
            if !write_word_blocking(WIN_FX, at + FXW_ROUTE, 0) { return false; }
            let mut w = 0;
            while w < 4 {
                if !write_word_blocking(WIN_FX, at + FXW_AMT0 + w, 0) { return false; }
                w += 1;
            }
            if !write_word_blocking(WIN_FX, at + FXW_WAS, 0) { return false; }
            if !write_word_blocking(WIN_FX, at + FXW_MIX, MIX_ONE) { return false; }
            if !write_word_blocking(WIN_FX, at + FXW_DIS, 0) { return false; }
            if !write_word_blocking(WIN_FX, at + FXW_WSTART, 0) { return false; }
            if !write_word_blocking(WIN_FX, at + FXW_WCOUNT, 0) { return false; }
            d.fx_pub[k] = 0;
        }
        k += 1;
    }
    true
}

/// Invalidate published DSP rows; does not release code/state or transfer sources.
pub unsafe fn quiesce(rec: usize) -> bool {
    if retained() { return false; }
    if ds().mod_len[rec] == 0 { return true; }
    clear_rows(rec) && clear_fx_rows(rec)
}

/// Release only after the caller has confirmed invalidation and CF admission/drain.
pub unsafe fn release_range(rec: usize) {
    if !retained() { unreserve(rec); }
}

pub unsafe fn release(rec: usize) -> bool {
    if !quiesce(rec) { ds().phase = PH_RETAINED; return false; }
    release_range(rec);
    true
}

// ---- the state machine --------------------------------------------------------------------------------------------------------

/// Hand a load over to the state machine: the CF side is complete and described by `p`; `reserve` has run for `p.rec`.
pub unsafe fn launch(mut p: Pending) {
    // A prepared reservation, if any, must remain owned after global retention.
    if retained() { return; }
    let d = ds();
    p.send_base = d.send_base;
    p.send_len = d.send_len;
    p.send_buf = d.send_buf;
    d.send_buf = 0;
    if d.phase == PH_IDLE && d.nwait == 0 {
        start(p);
    } else {
        d.waiting[d.nwait as usize] = p;
        d.nwait += 1;
    }
}

unsafe fn start(p: Pending) {
    let d = ds();
    d.pending = p;
    d.phase = PH_SEND;
    d.ticks = 0;
    d.step = 0;
    d.started = 0;
    d.transfers += 1;
}

/// The load in progress is over: start the next waiting one, or go idle.
unsafe fn next_job() {
    if retained() { return; }
    let d = ds();
    if d.nwait > 0 {
        let p = d.waiting[0];
        let mut k = 1;
        while k < d.nwait as usize {
            d.waiting[k - 1] = d.waiting[k];
            k += 1;
        }
        d.nwait -= 1;
        start(p);
    } else {
        d.phase = PH_IDLE;
    }
}

unsafe fn fail(code: i32) {
    let d = ds();
    let rec = d.pending.rec as usize;
    // Teardown invalidates once, after closing/waiting CF slots, before any fini/free.
    let reclaimed = crate::loader::undo_early(rec) == OK;
    crate::boot::note_result(d.pending.boot_idx, code);
    d.failures += 1;
    crate::st().last_result = code;
    if !reclaimed {
        d.phase = PH_RETAINED;
        return;
    }
    if d.pending.send_buf != 0 {
        arena::free(d.pending.send_buf as *mut u8);
        d.pending.send_buf = 0;
    }
    next_job();
}

/// Advance the load in progress by what is ready now. Called once per UI-loop pass.
pub unsafe fn step() {
    let d = ds();
    if d.magic != STATE_MAGIC {
        return;
    }
    if d.phase == PH_IDLE || d.phase == PH_RETAINED {
        return;
    }
    d.ticks = d.ticks.saturating_add(1);
    if d.phase == PH_QUARANTINE {
        if poll_done().is_some() {
            d.started = 0;
            fail(E_DSP_XFER);
        }
        return;
    }
    if d.ticks > GUARD_TICKS {
        if d.started != 0 && poll_done().is_none() {
            d.phase = PH_QUARANTINE;
            return;
        }
        fail(E_DSP_XFER);
        return;
    }
    loop {
        match d.phase {
            PH_SEND => {
                if d.started == 0 {
                    let r = call_send(d.pending.send_base, d.pending.send_buf as *const u8, d.pending.send_len);
                    if r == -2 {
                        fail(E_DSP_XFER);
                        return;
                    }
                    if r == 0 {
                        d.started = 1;
                    }
                    return;
                }
                match poll_done() {
                    None => return,
                    Some(false) => {
                        fail(E_DSP_XFER);
                        return;
                    }
                    Some(true) => {
                        arena::free(d.pending.send_buf as *mut u8);
                        d.pending.send_buf = 0;
                        d.phase = PH_ROWS;
                        d.step = 0;
                        d.started = 0;
                        d.ticks = 0;
                    }
                }
            }
            PH_ROWS => {
                // rows of the module in claim order; `ROW_STEPS` steps each: the 8 ranges, the pc, note_on, state base, stride, clear
                // bytes, the latch mask, the disabled word cleared, the type
                let rec = d.pending.rec as usize;
                let first = iw(I_VFIRST);
                let mut want = d.step;
                let mut row = MAX_ROWS;
                let mut k = 0;
                while k < MAX_ROWS {
                    if d.row_owner[k] == rec as u8 + 1 {
                        if want < ROW_STEPS {
                            row = k;
                            break;
                        }
                        want -= ROW_STEPS;
                    }
                    k += 1;
                }
                // then the effect rows, `FX_STEPS` steps each: the 8 parameter words, desc, state, mix, enabled, the 4 amount words, the route, the pc
                let mut frow = MAX_FX;
                if row == MAX_ROWS {
                    k = 0;
                    while k < MAX_FX {
                        if d.fx_owner[k] == rec as u8 + 1 {
                            if want < FX_STEPS {
                                frow = k;
                                break;
                            }
                            want -= FX_STEPS;
                        }
                        k += 1;
                    }
                }
                if row == MAX_ROWS && frow == MAX_FX {
                    // every row is in: list the machine, run init, publish the slots
                    crate::sample::ready(rec);
                    crate::tracks::ready(rec);
                    crate::boot::note_result(d.pending.boot_idx, OK);
                    crate::st().last_result = OK;
                    next_job();
                    return;
                }
                if d.started == 0 && frow != MAX_FX {
                    let at = FX_ROW_WORDS * (fiw(F_FIRST) + frow as u32);
                    let (word, value) = if want < 8 {
                        (at + FXW_P0 + want, d.fx_par[frow][want as usize])
                    } else if want == 8 {
                        (at + FXW_DESC, d.fx_desc[frow])
                    } else if want == 9 {
                        (at + FXW_STATE, d.fx_state[frow])
                    } else if want == 10 {
                        (at + FXW_MIX, d.fx_mix[frow])
                    } else if want == 11 {
                        (at + FXW_ENABLED, 1)
                    } else if want < FX_STEPS - 2 {
                        (at + FXW_AMT0 + (want - FX_STEP_ROUTE), d.fx_amt[frow][(want - FX_STEP_ROUTE) as usize])
                    } else if want == FX_STEPS - 2 {
                        (at + FXW_ROUTE, d.fx_route[frow])
                    } else {
                        (at + FXW_PC, d.fx_pc[frow])
                    };
                    if start_word(WIN_FX, word, value) {
                        d.started = 1;
                        d.row_writes += 1;
                        if want >= FX_STEP_ROUTE {
                            d.fx_pub[frow] = 1; // from here the amounts, the route or the pc word may be in the DSP: clear them on failure
                        }
                    }
                    return;
                }
                if d.started == 0 {
                    let at = ROW_WORDS * (first + row as u32);
                    let (word, value) = if want < 8 {
                        (at + 2 + want, d.row_rng[row][want as usize])
                    } else if want == 8 {
                        (at + 1, d.row_pc[row])
                    } else if want == 9 {
                        (at + 16, d.row_note_on[row])
                    } else if want == 10 {
                        (at + 17, d.row_state_base[row])
                    } else if want == 11 {
                        (at + 18, d.row_stride[row])
                    } else if want == 12 {
                        (at + 19, d.row_clear[row])
                    } else if want == 13 {
                        (at + 22, d.row_latched[row])
                    } else if want == 14 {
                        (at + 10, 0)
                    } else {
                        (at, d.row_type[row])
                    };
                    if start_word(WIN_VOICES, word, value) {
                        d.started = 1;
                        d.row_writes += 1;
                        if want == ROW_STEPS - 1 {
                            d.row_pub[row] = 1; // from here the type word may be in the DSP: clear it on failure
                        }
                    }
                    return;
                }
                match poll_done() {
                    None => return,
                    Some(false) => {
                        fail(E_DSP_XFER);
                        return;
                    }
                    Some(true) => {
                        d.step += 1;
                        d.started = 0;
                        d.ticks = 0;
                    }
                }
            }
            _ => return,
        }
    }
}

pub unsafe fn state() -> u32 {
    if ds().phase != PH_IDLE {
        STATE_LOADING
    } else {
        STATE_IDLE
    }
}

/// Serialized window-zero transfer for an owned sample job.
pub unsafe fn sample_send(dst: u32, src: *const u8, len: u32) -> i32 {
    call_send(dst, src, len)
}
pub unsafe fn sample_poll() -> Option<bool> {
    poll_done()
}
pub unsafe fn sample_idle() -> bool {
    ds().phase == PH_IDLE && ds().nwait == 0
}

#[cfg(test)]
pub(super) unsafe fn cleanup_test_first_fit(len: u32, align: u32) -> Option<u32> {
    alloc_range(len, align)
}
