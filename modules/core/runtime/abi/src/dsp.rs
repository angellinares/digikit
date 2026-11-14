//! The DSP side's shared types and constants: the words of the info tables, the row table state (`DspState`), the loader's pending load, the checked
//! section (`Job`, `DspSec`). The core owns the object `DKRT_DSP` (a host tool reads its layout); a service reaches it through `CoreApi::dsp`.
use crate::MAX_MODULES;
use dkneeds;
use dkrt_sdk::*;

pub const INFO_MAGIC: u32 = 0x4444_5350; // "DDSP"
pub const STATE_MAGIC: u32 = 0x444b_4453; // "DKDS"
/// Free voice rows core/dyndsp reserves: one per run-time machine type (a module's voice takes a row).
pub const MAX_ROWS: usize = DYN_TYPES as usize;
pub const MAX_VOICES: usize = MAX_ROWS;
pub const MAX_RELOCS: u32 = dkneeds::MAX_RELOCS;
pub const MAX_IMPORTS: u32 = dkneeds::MAX_IMPORTS;
pub const GRANULE: u32 = dkneeds::GRANULE;
/// Words of a voice row of `core/dsp-voice`: 0 type, 1 pc, 2 to 9 the range of knob 0 to 7 (min | max << 16), 10 the type the guard disabled, then its counters,
/// 16 note_on pc, 17 state base, 18 state stride, 19 clear bytes, 20 generation, 21 log level, 22 parameter latch mask, 23 the tracks whose state the adapter has zeroed since the load (bit t; the runtime writes 0).
pub const ROW_WORDS: u32 = 24;
/// Bytes of a voice entry in a DSP section: version 1 16 (type, entry), version 2 48 (type, entry, 8 ranges, 2 reserved),
/// version 3 64 (type, render, note_on, state_bytes, clear_bytes, 8 ranges, voices, latch mask, reserved).
pub const VOICE_V1: u32 = 16;
pub const VOICE_V2: u32 = 48;
pub const VOICE_V3: u32 = 64;
/// Most voice states a module may reserve.
pub const MAX_STATE_VOICES: u32 = dkneeds::MAX_STATE_VOICES;
/// Steps to publish a row: 8 ranges, the pc, note_on, state base, stride, clear, the latch mask, the zeroed-tracks mask, the cleared disabled word, the type.
pub const ROW_STEPS: u32 = 17;
/// Effect rows: the most a chain takes (`DK_FX_MAX`), the bytes of an effect record in a DSP section of version 4, the words of a row of `core/fx`, the pc of
/// a free row and the steps to publish a row: 8 parameter words, desc, state, mix, enabled, the 4 amount words and the route (finding 62), the pc.
pub const MAX_FX: usize = dkneeds::MAX_FX as usize;
pub const FX_REC: u32 = 64;
pub const FX_ROW_WORDS: u32 = 24;
pub const FX_FREE_PC: u32 = 0xffff_ffff;
pub const FX_STEPS: u32 = 18;
/// The words of an effect row (`core/fx` module.toml): pc, desc, state, enabled, p0 (to p7), and the adapter's was, dis, fault window start, count and total.
pub const FXW_PC: u32 = 0;
pub const FXW_DESC: u32 = 1;
pub const FXW_STATE: u32 = 2;
pub const FXW_ENABLED: u32 = 3;
pub const FXW_P0: u32 = 4;
pub const FXW_WAS: u32 = 12;
/// The row's dry/wet blend (a float; 1.0 is the wet signal, 0 the dry one), owned by the adapter and set by the MIX knob of the effect's page.
pub const FXW_MIX: u32 = 13;
/// 1.0f: the mix of a row without a page.
pub const MIX_ONE: u32 = 0x3f80_0000;
pub const FXW_DIS: u32 = 14;
pub const FXW_WSTART: u32 = 15;
pub const FXW_WCOUNT: u32 = 16;
/// The send route (finding 62): 0 master, 1 send bus; then the 16 amounts as 4 words, track t in word `FXW_AMT0 + t / 4`, byte `t % 4` (little endian).
pub const FXW_ROUTE: u32 = 18;
pub const FXW_AMT0: u32 = 19;
pub const FX_STEP_ROUTE: u32 = 12;
/// `dk_fx_desc` (<dk/fx/fx.h>): magic "DKFX", contract, and its size in bytes.
pub const FX_DESC_MAGIC: u32 = 0x444b_4658;
pub const FX_DESC_BYTES: u32 = 24; // DK_FX_DESC_BYTES in sdk/include/dk/gen/fx_consts.h
pub const FX_MAX_PARAMS: u32 = 8;
/// The windows of `core/dsp-link`.
pub const WIN_VOICES: u32 = 1;
pub const WIN_FX: u32 = 2;
pub const FX_INFO_MAGIC: u32 = 0x4446_5849; // "DFXI"
pub const L2_BYTE_BASE: u32 = 0x2000_0000;
pub const L2_SW_BASE: u32 = 0x00B8_0000;
/// The run-time machine types (`dynamic_first ..= last` of modules/core/adapters/machines/types.toml, through `patchkit gen`).
pub const TYPE_FIRST: u32 = device::DYN_TYPE_FIRST;
pub const TYPE_LAST: u32 = device::DYN_TYPE_LAST;
pub const DYN_TYPES: u32 = device::DYN_TYPES;
// the row count of the runtime's tables (`[limits] machine_rows`) is the count of run-time machine types that the firmware tables reserve
const _: () = assert!(device::LIMIT_MACHINE_ROWS == device::DYN_TYPES);
/// A free voice row's type: no machine type equals it.
pub const FREE_TYPE: u32 = 0xffff_ffff;
/// UI-loop passes a phase may last before the load is given up.
pub const GUARD_TICKS: u32 = 6000;
/// Polls a blocking one-word write may spin.
pub const SPIN: u32 = 4_000_000;

pub const PH_IDLE: u32 = 0;
pub const PH_SEND: u32 = 1;
pub const PH_ROWS: u32 = 2;
pub const PH_QUARANTINE: u32 = 3;
// Cleanup could not establish row invalidation/CF drain. Keep every allocation
// and transfer source untouched until reboot; unlike PH_QUARANTINE, never poll.
pub const PH_RETAINED: u32 = 4;

// words of `dyndsp_info` (byte offset / 4)
pub const I_MAGIC: usize = 0;
pub const I_DEVICE: usize = 2;
pub const I_AREA: usize = 3;
pub const I_AREA_BYTES: usize = 4;
pub const I_VROWS: usize = 5;
pub const I_VFIRST: usize = 6;
pub const I_VFREE: usize = 7;
pub const I_NIMPORT: usize = 9;
pub const I_IMPORTS: usize = 10;

// words of `dynfx_info` (byte offset / 4)
pub const F_MAGIC: usize = 0;
pub const F_DEVICE: usize = 2;
pub const F_ROWS: usize = 3;
pub const F_FIRST: usize = 4;
pub const F_FREE: usize = 5;
pub const F_WORDS: usize = 7;

/// The CF-side half of a load whose DSP part is under way: everything is in place except publishing (the machine row, `init`,
/// the tick).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Pending {
    pub rec: u32,
    /// Copy of the `register` records (arena block), 0 if none.
    pub reg: u32,
    pub reg_size: u32,
    /// Absolute address of `init`, 0 if none.
    pub init: u32,
    /// 1: the CF side went live at boot (machine rows, `init`, tick); only the DSP part is left, and a failure takes the module down.
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
    /// The link request of the loader in flight (the id of its `Ticket`; 0: none).
    pub ticket: u32,
    /// The steps of the row sequence that request carries.
    pub row_steps: u32,
    /// Byte address, bytes and arena block of the image `reserve` has prepared and `launch` has not taken yet.
    pub send_base: u32,
    pub send_len: u32,
    pub send_buf: u32,
    pub transfers: u32,
    pub row_writes: u32,
    pub failures: u32,
    pub last_dl_err: u32,
    /// Per module record: byte offset into the area and length of its range (len 0: none), and the rows it owns (bit mask).
    pub mod_off: [u32; MAX_MODULES],
    pub mod_len: [u32; MAX_MODULES],
    pub mod_rows: [u32; MAX_MODULES],
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
    pub waiting: [Pending; MAX_MODULES],
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
    pub mod_fx: [u32; MAX_MODULES],
}

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
    pub import_vals: [u32; MAX_IMPORTS as usize],
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

/// The tables of a DSP section after the header checks (offsets are into the section `s`), for the services' checks of their entries.
#[derive(Clone, Copy)]
pub struct DspSec {
    pub s: *const u8,
    pub version: u32,
    /// Bytes of the section.
    pub size: u32,
    pub image_size: u32,
    pub image_off: u32,
    pub reloc_off: u32,
    pub nreloc: u32,
    pub import_off: u32,
    pub nimport: u32,
    pub voice_off: u32,
    pub nvoice: u32,
    pub fx_off: u32,
    pub nfx: u32,
    /// Bytes of a voice entry (`VOICE_V1`, `VOICE_V2` or `VOICE_V3`).
    pub vstride: u32,
    /// The track-table record's offset (version 5; 0 otherwise).
    pub bank_off: u32,
}

pub unsafe fn in_sec(off: u32, len: u32, size: u32) -> bool {
    off <= size && len <= size - off
}

/// The little-endian word at `p` (the DSP image's own byte order).
pub unsafe fn rd32le(p: *const u8) -> u32 {
    (*p as u32)
        | ((*p.add(1) as u32) << 8)
        | ((*p.add(2) as u32) << 16)
        | ((*p.add(3) as u32) << 24)
}
