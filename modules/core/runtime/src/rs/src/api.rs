//! The public C ABI of the runtime: load, poll functions, module and effect listings, the selection, boot info and restart.

use crate::platform::st;
use crate::PLATFORM;
use dkrt_abi::platform::Platform;
use crate::state::{init_once, DKRT_POLLS, DKRT_POLL_OWNER, MAX_MODULES, POLLS, POLL_OWNER_RUNTIME, DKRT_MENUS, DKRT_PATHS};
use crate::{boot, dlog, dsp, entry, loader, scan, settings};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::*;

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



/// Add a function that `dkrt_tick` calls once per UI-loop pass, in the UI task (`core/ui` keeps its `dk_ui_tick` timers with one). Adding one
/// that is in the table is OK; `E_NOMEM` when it is full. Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_poll_add(f: usize) -> i32 {
    if f == 0 {
        return E_CONTRACT;
    }
    let t = &mut *addr_of_mut!(DKRT_POLLS);
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
    (*addr_of_mut!(DKRT_POLL_OWNER))[free] = poll_owner(f);
    OK
}

/// The module record whose loaded range holds `f`, or `POLL_OWNER_RUNTIME`.
unsafe fn poll_owner(f: usize) -> u8 {
    let s = st();
    let mut rec = 0;
    while rec < MAX_MODULES {
        let m = &s.modules[rec];
        let off = (f as u64).wrapping_sub(m.base as u64);
        if m.used != 0 && off < m.size as u64 {
            return rec as u8;
        }
        rec += 1;
    }
    POLL_OWNER_RUNTIME
}

/// Free the poll slots whose function lies in `[base, base + size)` (a module that is released).
pub(crate) unsafe fn poll_drop_range(base: u32, size: u32) {
    let t = &mut *addr_of_mut!(DKRT_POLLS);
    let mut i = 0;
    while i < POLLS {
        if t[i] != 0 && (t[i] as u64).wrapping_sub(base as u64) < size as u64 {
            t[i] = 0;
            (*addr_of_mut!(DKRT_POLL_OWNER))[i] = POLL_OWNER_RUNTIME;
        }
        i += 1;
    }
}

/// Remove a function added with `dkrt_poll_add` (no effect when it is not there). Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_poll_remove(f: usize) {
    let t = &mut *addr_of_mut!(DKRT_POLLS);
    let mut i = 0;
    while i < POLLS {
        if f != 0 && t[i] == f {
            t[i] = 0;
            (*addr_of_mut!(DKRT_POLL_OWNER))[i] = POLL_OWNER_RUNTIME;
        }
        i += 1;
    }
}



/// The header fields of the `.dkm` at store sector `first` (`DkrtScan.first`) into `out`, for the INFO page of the DIGIKIT menu: 0, `E_FORMAT` when
/// the file is not a `.dkm`, or the read error. The runtime reads the sector into its own DMA-safe buffer. Task context.
#[no_mangle]
pub unsafe extern "C" fn dkrt_module_header(first: u32, out: *mut DkrtModuleHeader) -> i32 {
    init_once();
    scan::module_header(first, out)
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
                o.path = (*addr_of!(DKRT_PATHS))[i];
                o.state = if m.used == 2 { MOD_LOADING } else { MOD_LOADED };
                o.dsp_size = dsp::ds().mod_len[i];
                o.menu = (*addr_of!(DKRT_MENUS))[i];
                o.ctx = entry::ctx_addr(i);
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
    crate::handlers::exports::fx_entry(index, &mut *out)
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
        crate::platform::project_serialize();
    }
    if mode & 2 != 0 {
        crate::platform::boot_record_write();
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
    let sr = PLATFORM.irq_mask();
    if card_busy() {
        PLATFORM.irq_restore(sr);
        return E_CARD_BUSY;
    }
    PLATFORM.soft_reset()
}
