//! The image table `machines_info` of `core/machines` (its `[[info]]`), as the machines service file (`machines.dks`) uses it.
//!
//! The adapter in the image knows the firmware (addresses, the registry rows, where each hook continues in the stock code) and the service file does not: the file reads
//! one table through `CoreApi::info` and copies it into its own bss (`bind_table`, from `setup`), where its hooks (`hooks-dt2.s`, `hooks-dn2.s`) read it as `MF_TAB`.
//! The firmware calls of the click flow (`TableFw`, `fw.rs`) go through the same table. One layout for both devices; a word a device does not use is 0.
//!
//! The words are big endian in the image (ColdFire is big endian, so a read of the word is the value).
use core::ptr::{read_volatile, write_volatile};
use dkrt_abi::core_api::CoreApi;
use dkrt_sdk::{E_NO_REGISTRY, OK};
use dkrt_svc_rt::name_hash;

/// `MACH`.
pub const MAGIC: u32 = 0x4d41_4348;
pub const VERSION: u32 = 1;
/// Words of the table.
pub const WORDS: usize = 39;

/// Word indexes of the table (byte offset = 4 * index; `hooks-dt2.s` and `hooks-dn2.s` name the byte offsets `T_*`, and a test compares them). The hooks that fall through to the
/// stock instructions of their site use the tail `patchkit bind` makes (`dk_tail_<hash>`), so the table has no word for them; it has the exits of the hooks that cannot (a
/// branch among the displaced instructions, an exit elsewhere in the routine) as addresses.
pub mod w {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 1;
    /// 1 Digitakt II, 2 Digitone II (`[digikit] id` of the device file).
    pub const DEVICE: usize = 2;
    /// The firmware functions of the click flow: `cf.input.fn.key_down`, `cf.param.fn.set_value`, `cf.page.fn.view_track`.
    pub const KEY_DOWN: usize = 3;
    pub const SET_VALUE: usize = 4;
    pub const VIEW_TRACK: usize = 5;
    /// The registry: `machines.first`, `.count`, `.entries`, `.rows`.
    pub const FIRST: usize = 6;
    pub const COUNT: usize = 7;
    pub const ENTRIES: usize = 8;
    pub const ROWS: usize = 9;
    /// `mf_ensure` of the image (builds the registry's run-time tables once).
    pub const ENSURE: usize = 10;
    /// The adapter's native page functions (`mf_native_*`).
    pub const N_PAGE_COUNT: usize = 11;
    pub const N_DESCRIPTOR: usize = 12;
    pub const N_CODE: usize = 13;
    pub const N_DRAW: usize = 14;
    pub const N_OVERLAY: usize = 15;
    /// Digitakt II: `cf.machine.fn.current_track_type` and the table `mf_slotmap`.
    pub const SPT: usize = 16;
    pub const SLOTMAP: usize = 17;
    /// Digitone II: the page rows `pages.rows` and `pages.count`.
    pub const PGROWS: usize = 18;
    pub const PGN: usize = 19;
    /// Digitakt II clone.5: where the masked test continues when equal and when not.
    pub const R_C5_NE: usize = 20;
    pub const R_C5_EQ: usize = 21;
    /// Digitakt II parameter code: its second exit (d0 = the field).
    pub const X_TYPEARG: usize = 22;
    /// Digitakt II SRC page: the stock path and the knob-page branch.
    pub const R_SP_STOCK: usize = 23;
    pub const R_SP_ALT: usize = 24;
    /// Digitone II group helper: behind the site and the target of its branch.
    pub const R_GROUP_NEXT: usize = 25;
    pub const R_GROUP_TAKEN: usize = 26;
    /// Digitone II: behind the frame type site, the type reader load and the owner test.
    pub const R_FT: usize = 27;
    pub const R_RD: usize = 28;
    pub const R_OW: usize = 29;
    /// Digitone II: the end of each of the nine calls of the type reader in identity code (9 words).
    pub const RC0: usize = 30;
}

/// The byte offsets the assembler files name (`.equ T_NAME, offset`), for the test that compares them with `w`.
pub const ASM_OFFSETS: &[(&str, usize)] = &[
    ("T_FIRST", w::FIRST),
    ("T_COUNT", w::COUNT),
    ("T_ROWS", w::ROWS),
    ("T_ENSURE", w::ENSURE),
    ("T_N_PAGE_COUNT", w::N_PAGE_COUNT),
    ("T_N_DESCRIPTOR", w::N_DESCRIPTOR),
    ("T_N_CODE", w::N_CODE),
    ("T_N_DRAW", w::N_DRAW),
    ("T_N_OVERLAY", w::N_OVERLAY),
    ("T_SPT", w::SPT),
    ("T_SLOTMAP", w::SLOTMAP),
    ("T_PGROWS", w::PGROWS),
    ("T_PGN", w::PGN),
    ("T_R_C5_NE", w::R_C5_NE),
    ("T_R_C5_EQ", w::R_C5_EQ),
    ("T_X_TYPEARG", w::X_TYPEARG),
    ("T_R_SP_STOCK", w::R_SP_STOCK),
    ("T_R_SP_ALT", w::R_SP_ALT),
    ("T_R_GROUP_NEXT", w::R_GROUP_NEXT),
    ("T_R_GROUP_TAKEN", w::R_GROUP_TAKEN),
    ("T_R_FT", w::R_FT),
    ("T_R_RD", w::R_RD),
    ("T_R_OW", w::R_OW),
    ("T_RC0", w::RC0),
];

/// The image table `machines_info`, copied by `setup`; the hooks read it (`T_*` of the assembler files).
#[no_mangle]
pub static mut MF_TAB: [u32; WORDS] = [0; WORDS];

/// The service file's `setup` (the first step of `init`): bind the image table into `MF_TAB`, or the file is refused (`E_NO_REGISTRY`: no usable `core/machines` in the image).
pub unsafe fn setup<C: CoreApi>(core: &C) -> i32 {
    bind_table(core, core::ptr::addr_of_mut!(MF_TAB) as *mut u32)
}

/// The table of the image, or null: the image has no `core/machines`.
unsafe fn image<C: CoreApi>(core: &C) -> *const u32 {
    core.info(name_hash("machines_info"))
}

/// Is `src` a table this service can use (magic, version, the device the core runs on, and the words the hooks cannot do without)?
unsafe fn usable<C: CoreApi>(core: &C, src: *const u32) -> bool {
    !src.is_null()
        && read_volatile(src.add(w::MAGIC)) == MAGIC
        && read_volatile(src.add(w::VERSION)) == VERSION
        && read_volatile(src.add(w::DEVICE)) == core.device_id()
        && read_volatile(src.add(w::ENSURE)) != 0
        && read_volatile(src.add(w::ROWS)) != 0
}

/// Copy the image table to `dst` (`WORDS` words), the magic last: a table is usable only when every word is in. `E_NO_REGISTRY` when the image has no usable table.
///
/// # Safety
/// `dst` points at `WORDS` words.
pub unsafe fn bind_table<C: CoreApi>(core: &C, dst: *mut u32) -> i32 {
    let src = image(core);
    if !usable(core, src) {
        return E_NO_REGISTRY;
    }
    let mut i = 1;
    while i < WORDS {
        write_volatile(dst.add(i), read_volatile(src.add(i)));
        i += 1;
    }
    write_volatile(dst, MAGIC);
    OK
}
