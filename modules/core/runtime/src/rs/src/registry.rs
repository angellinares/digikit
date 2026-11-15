//! The run-time registry: machines and parameters that a loaded `.dkm` adds to the rows `core/dynreg` reserved at patch
//! time (docs/design/digikit-registry.md). Everything here runs in the UI task. The loader calls it (`apply` from the completion step of the boot-time load, `remove` from the undo); it never calls the loader.
//!
//! The runtime does not link to `core/machines` or `core/params`. `core/dynreg` exports one table, `dynreg_info`, with the
//! addresses of the rows and tables and the row layouts; the link is optional, so an image with the runtime and without
//! `core/dynreg` loads modules that carry no `register` records and refuses the ones that do (`E_NO_REGISTRY`).
//!
//! Digitone II keeps a machine's knobs in page rows (`pages`), not in the machine row: a PAGE record fills one reserved page
//! row; the machine record's `own_dsp` is written to the row (the frame builder then tells the DSP the machine's own type).
//!
//! Rules: all checks run before the first write (`check`); rows are written in an order that a reader never sees a
//! half-filled machine (the name pointer, which marks a row as used, is the last store, one aligned 32-bit word); the
//! strings the rows point to live in this module's own state, never in the arena, so unload cannot leave a dangling pointer.
use crate::handlers::{check_step, services, ApplyCtx, CheckCtx, ServiceTable};
use crate::rd32;
use core::ptr::{addr_of, addr_of_mut};
#[cfg(not(test))]
use core::ptr::read_volatile;
use dkrt_sdk::*;

const REG_MAGIC: u32 = 0x444b_5247; // "DKRG"
const INFO_MAGIC: u32 = 0x4459_4e52; // "DYNR"
const MAX_RECORDS: u32 = dkneeds::MAX_RECORDS;
const MAX_MACH: usize = limits::MACHINE_ROWS;
const MAX_PARAM: usize = limits::PARAM_ROWS;
const MAX_PAGE: usize = limits::PAGE_ROWS;
const KNOBS: usize = limits::PAGE_KNOBS;

pub(crate) use dkrt_abi::mem::{iw, rd, wr};
pub(crate) use dkrt_abi::registry::{each, w, ApplyArgs, RegState, MACH_TEXT};

extern "C" {
    /// Data symbol of core/dynreg; resolves to 0 when the module is not in the image (`[object] optional`).
    fn dynreg_info();
}

#[no_mangle]
pub static mut DKRT_REG: RegState = unsafe { core::mem::zeroed() };

pub(crate) fn rs() -> &'static mut RegState {
    unsafe { &mut *addr_of_mut!(DKRT_REG) }
}

pub(crate) unsafe fn info() -> *const u32 {
    #[cfg(test)]
    {
        return crate::handlers::tests::dynreg_table();
    }
    #[cfg(not(test))]
    info_table()
}

#[cfg(not(test))]
#[inline(always)]
unsafe fn info_table() -> *const u32 {
    let a = core::hint::black_box(dynreg_info as usize);
    if a == 0 {
        return core::ptr::null();
    }
    let p = a as *const u32;
    if read_volatile(p.add(w::MAGIC)) != INFO_MAGIC {
        return core::ptr::null();
    }
    p
}

/// The device facts of the builder lowering (`collect::lower`) from `dynreg_info` version 3; None without a registry or with an older table.
pub unsafe fn profile() -> Option<crate::collect::Profile> {
    let inf = info();
    if inf.is_null() || iw(inf, w::VERSION) < 3 {
        return None;
    }
    let mut p = crate::collect::Profile {
        device: iw(inf, w::DEVICE),
        pages: iw(inf, w::PG_ROWS) != 0,
        slot_base: iw(inf, w::SLOT_BASE),
        clone_of: iw(inf, w::P_CLONE),
        level: iw(inf, w::P_LEVEL),
        own_dsp: iw(inf, w::P_OWN) != 0,
        tune_code: iw(inf, w::P_TUNE),
        tune_slot: iw(inf, w::P_TUNE_SLOT),
        level_code: iw(inf, w::P_LEVEL_CODE),
        level_knob: iw(inf, w::P_LEVEL_KNOB),
        nslots: iw(inf, w::P_NSLOTS).min(8),
        slots: [0; 8],
        tpl: [0; 6],
    };
    let mut i = 0;
    while i < 8 {
        p.slots[i] = iw(inf, w::P_SLOTS + i);
        i += 1;
    }
    i = 0;
    while i < 6 {
        p.tpl[i] = iw(inf, w::P_TPL + i);
        i += 1;
    }
    Some(p)
}

/// Free reserved machine, parameter and page rows, for the commit of a builder module.
pub unsafe fn free_rows() -> Option<crate::collect::Free> {
    let inf = info();
    if inf.is_null() {
        return None;
    }
    let s = rs();
    Some(crate::collect::Free {
        machine_rows: capacity::free_rows(&s.mach_owner, iw(inf, w::M_DYN_N)),
        param_rows: capacity::free_rows(&s.param_owner, iw(inf, w::P_DYN_N)),
        page_rows: capacity::free_rows(&s.page_owner, iw(inf, w::PG_DYN_N)),
    })
}

static mut ARGS: Option<ApplyArgs> = None;

pub unsafe fn set_args(a: Option<ApplyArgs>) {
    *addr_of_mut!(ARGS) = a;
}

/// The code of the first run-time parameter row (a row `k` of the registry has code `param_first() + k`); `u32::MAX` without a registry.
pub fn param_first() -> u32 {
    unsafe {
        let inf = info();
        if inf.is_null() {
            u32::MAX
        } else {
            iw(inf, w::P_DYN0)
        }
    }
}


/// Validate the `register` records of a module file. `reg` points into the file (the staging buffer). Nothing is written.
pub unsafe fn check(reg: *const u8, size: u32, image_size: u32) -> i32 {
    check_with(services(), reg, size, image_size)
}

/// The walker of `check`: the layout of the section against the lengths the services declare, then each service's own records.
pub(crate) unsafe fn check_with(l: ServiceTable, reg: *const u8, size: u32, image_size: u32) -> i32 {
    if size == 0 {
        return OK;
    }
    let inf = info();
    if inf.is_null() || !l.fits(inf) {
        return E_NO_REGISTRY;
    }
    if size < 16 || rd32(reg) != REG_MAGIC || rd32(reg.add(4)) != 1 {
        return E_FORMAT;
    }
    if rd32(reg.add(8)) != iw(inf, w::DEVICE) {
        return E_REG_DEVICE;
    }
    let n = rd32(reg.add(12));
    if n > MAX_RECORDS {
        return E_FORMAT;
    }
    // the layout: every record has a kind some service declared, and that kind's length
    let mut at: u32 = 16;
    let mut i = 0;
    while i < n {
        if at > size || size - at < 8 {
            return E_FORMAT;
        }
        let kind = rd32(reg.add(at as usize));
        let len = rd32(reg.add(at as usize + 4));
        let want = match l.len(kind) {
            Some(want) => want,
            None => return E_FORMAT,
        };
        if len != want || size - at < len {
            return E_FORMAT;
        }
        at += len;
        i += 1;
    }
    if at != size {
        return E_FORMAT;
    }
    // the steps of the old single body, each over every service's records in file order
    let c = CheckCtx { reg, image_size, inf };
    let mut step = 0;
    while step < check_step::COUNT {
        let r = l.check(step, &c);
        if r != OK {
            return r;
        }
        step += 1;
    }
    OK
}

/// Fill the reserved rows from the (checked) records. `owner` = module record index + 1.
pub unsafe fn apply(owner: u8, reg: *const u8, size: u32, base: u32) -> i32 {
    apply_with(services(), owner, reg, size, base)
}

/// The walker of `apply`: each service writes its records, in list order; the first failure stops the walk.
pub(crate) unsafe fn apply_with(l: ServiceTable, owner: u8, reg: *const u8, size: u32, base: u32) -> i32 {
    if size == 0 {
        return OK;
    }
    let inf = info();
    if inf.is_null() {
        return E_NO_REGISTRY;
    }
    let c = ApplyCtx { owner, reg, base, inf, args: (*addr_of!(ARGS)).as_ref() };
    let mut rc = l.apply(&c);
    if rc == OK {
        // the records that register callbacks and pages, in file order; the first failure stops the walk
        each(reg, |kind, b| {
            if rc == OK {
                rc = l.apply_record(kind, b, &c);
            }
        });
    }
    *addr_of_mut!(ARGS) = None;
    if rc != OK {
        return rc;
    }
    let s = rs();
    s.registers = s.registers.wrapping_add(1);
    OK
}

/// Give back every row `owner` filled: each service gives back its own, the last service of the list first.
pub unsafe fn remove(owner: u8) {
    remove_with(services(), owner)
}

pub(crate) unsafe fn remove_with(l: ServiceTable, owner: u8) {
    l.remove(owner);
    if !info().is_null() {
        let s = rs();
        s.removes = s.removes.wrapping_add(1);
    }
}
