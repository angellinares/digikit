//! The run-time registry: machines and parameters that a loaded `.dkm` adds to the rows `core/dynreg` reserved at patch
//! time (docs/design/digikit-registry.md). Everything here runs in the UI task, from the boot-time load (`loader::complete`) and its undo (`remove`).
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
use crate::{cstr_len, rd32};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

const REG_MAGIC: u32 = 0x444b_5247; // "DKRG"
const INFO_MAGIC: u32 = 0x4459_4e52; // "DYNR"
const KIND_PARAM: u32 = 1;
const KIND_MACHINE: u32 = 2;
const KIND_PAGE: u32 = 3;
const KIND_EFFECT: u32 = crate::fxpage::KIND_EFFECT;
const LEN_PARAM: u32 = 0x50;
const LEN_MACHINE: u32 = 0x6c;
const LEN_PAGE: u32 = 0x60;
const LEN_EFFECT: u32 = crate::fxpage::LEN_EFFECT;
const KNOB_PARAM: u32 = 0x8000_0000;
const MAX_RECORDS: u32 = dkneeds::MAX_RECORDS;
const MAX_MACH: usize = limits::MACHINE_ROWS;
const MAX_PARAM: usize = limits::PARAM_ROWS;
const MAX_PAGE: usize = limits::PAGE_ROWS;
const KNOBS: usize = limits::PAGE_KNOBS;
const MACH_TEXT: usize = 24;
const REP_SIZE: usize = 48;

extern "C" {
    /// Data symbol of core/dynreg; resolves to 0 when the module is not in the image (`[object] optional`).
    fn dynreg_info();
}

/// Words of `dynreg_info` (byte offsets / 4). Words 48, 49 and 61 to 64 were the machine-view hook and the in-use functions of hot
/// reload (archived, docs/plans/digikit-hot-reload.md); the table keeps them as zeros so the other offsets stay.
mod w {
    pub const MAGIC: usize = 0;
    pub const DEVICE: usize = 2;
    pub const M_ROWS: usize = 3;
    pub const M_ROW_SIZE: usize = 4;
    pub const M_FIRST: usize = 5;
    pub const M_DYN0: usize = 7;
    pub const M_DYN_N: usize = 8;
    pub const NAMES: usize = 9;
    pub const PERMIT: usize = 10;
    pub const PERTYPE: usize = 11;
    pub const O_LONG: usize = 12;
    pub const O_SHORT: usize = 13;
    pub const O_F0: usize = 14;
    pub const O_CLONE: usize = 15;
    pub const O_POS: usize = 16;
    pub const O_OWN: usize = 17;
    pub const O_NAME: usize = 18;
    pub const O_SNAME: usize = 19;
    pub const P_TABLE: usize = 20;
    pub const P_ROWS: usize = 21;
    pub const P_ROW_SIZE: usize = 22;
    pub const P_FIRST: usize = 23;
    pub const P_DYN0: usize = 25;
    pub const P_DYN_N: usize = 26;
    pub const PF_SIZE: usize = 27;
    pub const P_TEMPLATE_OFF: usize = 28;
    pub const EMPTY: usize = 29;
    pub const SPEC: usize = 30; // 15 words
    pub const KNOBS: usize = 45;
    pub const SLOT_BASE: usize = 46;
    pub const RESERVE_TEMPLATE: usize = 47;
    pub const PG_ROWS: usize = 50; // 0: the device keeps its knobs in the machine row
    pub const PG_ROW_SIZE: usize = 51;
    pub const PG_FIRST: usize = 52;
    pub const PG_DYN0: usize = 53;
    pub const PG_DYN_N: usize = 54;
    pub const PGO_NAME: usize = 55;
    pub const PGO_TITLE: usize = 56;
    pub const PGO_C: usize = 57;
    pub const PGO_MACH: usize = 58;
    pub const PGO_S: usize = 59;
    pub const PG_FREE: usize = 60;
}

#[repr(C, align(4))]
struct Strings {
    mach_name: [[u8; MACH_TEXT]; MAX_MACH],
    mach_short: [[u8; MACH_TEXT]; MAX_MACH],
    rep_long: [[u8; REP_SIZE]; MAX_MACH],
    rep_short: [[u8; REP_SIZE]; MAX_MACH],
    param_name: [[u8; 32]; MAX_PARAM],
    param_short: [[u8; 16]; MAX_PARAM],
    page_name: [[u8; MACH_TEXT]; MAX_PAGE],
    page_title: [[u8; MACH_TEXT]; MAX_PAGE],
    rep_pname: [[u8; REP_SIZE]; MAX_PAGE],
    rep_ptitle: [[u8; REP_SIZE]; MAX_PAGE],
}

/// The registry's state: who owns each reserved row (module record index + 1, 0 = free) and the strings the rows use.
#[repr(C, align(4))]
pub struct RegState {
    pub magic: u32,
    pub registers: u32,
    pub removes: u32,
    pub mach_owner: [u8; MAX_MACH],
    pub param_owner: [u8; MAX_PARAM],
    pub page_owner: [u8; MAX_PAGE],
    /// An empty std::string representation (length 0, capacity 0, reference count -1, NUL) for a free machine row.
    empty_rep: [u32; 4],
    s: Strings,
}

#[no_mangle]
pub static mut DKRT_REG: RegState = unsafe { core::mem::zeroed() };

fn rs() -> &'static mut RegState {
    unsafe { &mut *addr_of_mut!(DKRT_REG) }
}

pub(crate) unsafe fn info() -> *const u32 {
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

unsafe fn iw(i: *const u32, k: usize) -> u32 {
    read_volatile(i.add(k))
}

unsafe fn rd(a: u32) -> u32 {
    read_volatile(a as *const u32)
}

unsafe fn wr(a: u32, v: u32) {
    write_volatile(a as *mut u32, v)
}

/// A NUL-terminated printable-ASCII text of exactly `field` bytes, 1 to `field - 1` characters.
unsafe fn text_ok(p: *const u8, field: usize) -> bool {
    let n = cstr_len(p, field);
    if n == 0 || n == field {
        return false;
    }
    let mut i = 0;
    while i < field {
        let c = *p.add(i);
        if i < n {
            if !(0x20..0x7f).contains(&c) {
                return false;
            }
        } else if c != 0 {
            return false;
        }
        i += 1;
    }
    true
}

unsafe fn copy_text(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
}

/// Build a std::string representation {len, cap, refcount -1, chars, 0} at `rep` (upper-cased when `upper`); returns the
/// chars' address.
pub(crate) unsafe fn make_rep(rep: *mut u8, src: *const u8, upper: bool) -> u32 {
    let n = cstr_len(src, MACH_TEXT) as u32;
    crate::wr32(rep, n);
    crate::wr32(rep.add(4), n);
    crate::wr32(rep.add(8), 0xffff_ffff);
    let mut i = 0;
    while i < n as usize {
        let c = *src.add(i);
        *rep.add(12 + i) = if upper && c.is_ascii_lowercase() { c - 32 } else { c };
        i += 1;
    }
    *rep.add(12 + n as usize) = 0;
    rep as u32 + 12
}

/// Walk the records of a validated section. Calls `f(kind, body)` for each.
unsafe fn each(reg: *const u8, mut f: impl FnMut(u32, *const u8)) {
    let n = rd32(reg.add(12));
    let mut at = 16usize;
    let mut i = 0;
    while i < n {
        let kind = rd32(reg.add(at));
        let len = rd32(reg.add(at + 4)) as usize;
        f(kind, reg.add(at + 8));
        at += len;
        i += 1;
    }
}

/// Validate the `register` records of a module file. `reg` points into the file (the staging buffer). Nothing is written.
pub unsafe fn check(reg: *const u8, size: u32) -> i32 {
    if size == 0 {
        return OK;
    }
    let inf = info();
    if inf.is_null() {
        return E_NO_REGISTRY;
    }
    // the table must fit this registry's state (a table that says more rows than the state has would index past it)
    if iw(inf, w::M_DYN_N) as usize > MAX_MACH
        || iw(inf, w::P_DYN_N) as usize > MAX_PARAM
        || iw(inf, w::PG_DYN_N) as usize > MAX_PAGE
    {
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
    // pass 1: layout, counts
    let mut at: u32 = 16;
    let (mut np, mut nm, mut ng, mut ne) = (0u32, 0u32, 0u32, 0u32);
    let mut i = 0;
    while i < n {
        if at > size || size - at < 8 {
            return E_FORMAT;
        }
        let kind = rd32(reg.add(at as usize));
        let len = rd32(reg.add(at as usize + 4));
        let want = match kind {
            KIND_PARAM => LEN_PARAM,
            KIND_MACHINE => LEN_MACHINE,
            KIND_PAGE => LEN_PAGE,
            KIND_EFFECT => LEN_EFFECT,
            _ => return E_FORMAT,
        };
        if len != want || size - at < len {
            return E_FORMAT;
        }
        if kind == KIND_PARAM {
            np += 1;
        } else if kind == KIND_MACHINE {
            nm += 1;
        } else if kind == KIND_EFFECT {
            ne += 1;
        } else {
            ng += 1;
        }
        at += len;
        i += 1;
    }
    if at != size || np as usize > MAX_PARAM || nm as usize > MAX_MACH || ng as usize > MAX_PAGE || ne as usize > crate::fxpage::MAX_FX {
        return E_FORMAT;
    }
    // effect pages need core/fxpages; each record is checked on its own, and an effect has one page
    let mut fx_seen = 0u32;
    let mut bad_fx = OK;
    each(reg, |kind, b| {
        if kind == KIND_EFFECT && bad_fx == OK {
            let r = crate::fxpage::check_record(b);
            let fx = rd32(b);
            if r == OK && fx < 32 && fx_seen & (1 << fx) != 0 {
                bad_fx = E_FORMAT;
            } else if r != OK {
                bad_fx = r;
            }
            if fx < 32 {
                fx_seen |= 1 << fx;
            }
        }
    });
    if bad_fx != OK {
        return bad_fx;
    }
    // pages need a device that has page rows
    let has_pages = iw(inf, w::PG_ROWS) != 0;
    if ng != 0 && !has_pages {
        return E_FORMAT;
    }
    // pass 2: the records
    let (p_first, knobs, slot_base) = (iw(inf, w::P_FIRST), iw(inf, w::KNOBS), iw(inf, w::SLOT_BASE));
    let (dyn0, dyn_n, m_first) = (iw(inf, w::M_DYN0), iw(inf, w::M_DYN_N), iw(inf, w::M_FIRST));
    let s = rs();
    let mut seen_types = [0u32; MAX_MACH];
    let mut ns = 0usize;
    let mut bad = OK;
    let mut pi = 0u32;
    each(reg, |kind, b| {
        if bad != OK || kind == KIND_EFFECT {
            return; // an effect's page was checked above, it has no machine, parameter or page row of this registry
        }
        if kind == KIND_PARAM {
            let index = rd32(b);
            let (template, slot) = (rd32(b.add(4)), rd32(b.add(8)));
            let (min, max) = (rd32(b.add(12)) as i32, rd32(b.add(16)) as i32);
            if index != pi
                || template >= p_first
                || slot < slot_base
                || slot >= slot_base + knobs
                || min > max
                || !text_ok(b.add(24), 32)
                || !text_ok(b.add(56), 16)
            {
                bad = E_FORMAT;
            }
            pi += 1;
        } else if kind == KIND_PAGE {
            // machine type (checked against the file's machines below), eight knobs, level, name, title
            let mut j = 0;
            while j < KNOBS {
                let kn = rd32(b.add(4 + 4 * j));
                if kn != 0 && (kn & KNOB_PARAM == 0 || (kn & !KNOB_PARAM) >= np) {
                    bad = E_FORMAT;
                    return;
                }
                j += 1;
            }
            if rd32(b.add(36)) > 0xffff
                || !text_ok(b.add(40), MACH_TEXT)
                || !text_ok(b.add(40 + MACH_TEXT), MACH_TEXT)
                || knobs as usize != KNOBS
            {
                bad = E_FORMAT;
            }
        } else {
            // the first word is a machine key; `assign` has the number for it (0: none, which is out of range below)
            let t = crate::assign::resolve(rd32(b));
            let clone = rd32(b.add(4));
            let own = rd32(b.add(12));
            if rd32(b.add(8)) != 255 || own > 1 || (own == 1 && !has_pages) || rd32(b.add(16)) > 0xffff {
                bad = E_FORMAT;
                return;
            }
            let r = capacity::machine_type(t, dyn0, dyn_n, clone, m_first);
            if r != OK {
                bad = r;
                return;
            }
            let mut j = 0;
            while j < ns {
                if seen_types[j] == t {
                    bad = E_FORMAT;
                    return;
                }
                j += 1;
            }
            seen_types[ns] = t;
            ns += 1;
            let k = (t - dyn0) as usize;
            let row = iw(inf, w::M_ROWS) + (t - m_first) * iw(inf, w::M_ROW_SIZE);
            let name = rd(row + iw(inf, w::O_NAME));
            if s.mach_owner[k] != 0 || (name != 0 && *(name as *const u8) != 0) {
                bad = E_TYPE_USED;
                return;
            }
            let mut j = 0;
            while j < knobs as usize {
                let kn = rd32(b.add(20 + 4 * j));
                if kn & KNOB_PARAM != 0 {
                    if (kn & !KNOB_PARAM) >= np {
                        bad = E_FORMAT;
                        return;
                    }
                } else if kn >= p_first {
                    bad = E_FORMAT;
                    return;
                }
                j += 1;
            }
            if !text_ok(b.add(52), MACH_TEXT) || !text_ok(b.add(52 + MACH_TEXT), MACH_TEXT) {
                bad = E_FORMAT;
            }
        }
    });
    if bad != OK {
        return bad;
    }
    // a page belongs to a machine of the same file
    each(reg, |kind, b| {
        if kind != KIND_PAGE || bad != OK {
            return;
        }
        let t = crate::assign::resolve(rd32(b));
        let mut j = 0;
        let mut found = false;
        while j < ns {
            found |= seen_types[j] == t;
            j += 1;
        }
        if !found {
            bad = E_FORMAT;
        }
    });
    if bad != OK {
        return bad;
    }
    // free reserved page and parameter rows
    capacity::registry_rows(
        ng,
        capacity::free_rows(&s.page_owner, iw(inf, w::PG_DYN_N)),
        np,
        capacity::free_rows(&s.param_owner, iw(inf, w::P_DYN_N)),
    )
}

/// Write one 60-byte parameter table row from its lowering spec (the template's words, the record's values).
unsafe fn write_param_row(
    inf: *const u32,
    code: u32,
    template: u32,
    slot: u32,
    min: u32,
    max: u32,
    def: u32,
    name: u32,
    short: u32,
) {
    let table = iw(inf, w::P_TABLE);
    let size = iw(inf, w::PF_SIZE);
    let row = table + code * size;
    let tpl = table + template * size;
    // values by spec code 0x100 ..: min, max, default, name, short, mirror index, kind word, 0x80 + slot, the slot itself
    // (no `match`: a jump table would put data into the code section)
    let t = ((slot + 1) >> 2) & 1;
    let u = 1i32.wrapping_sub((slot >> 2) as i32);
    let kv = (0x10u32 + slot).wrapping_add((t as i32).wrapping_mul(u).wrapping_mul(0xffec) as u32);
    let vals: [u32; 16] = [
        min,
        max,
        def,
        name,
        short,
        25 + slot + (slot >> 2) * 2,
        (kv << 16) | (rd(tpl + 0x18) & 0xffff),
        0x80 + slot,
        slot,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    let mut i = 0;
    while i < 15 {
        let spec = iw(inf, w::SPEC + i);
        let v = if spec <= 0x3c {
            rd(tpl + spec)
        } else {
            vals[(spec as usize - 0x100) & 15]
        };
        wr(row + 4 * i as u32, v);
        i += 1;
    }
    // the registry row (core/params): name, short, template, slot, min, max, default; pf_t2 reads `template`
    let reg_row = iw(inf, w::P_ROWS) + (code - iw(inf, w::P_FIRST)) * iw(inf, w::P_ROW_SIZE);
    wr(reg_row, name);
    wr(reg_row + 4, short);
    wr(reg_row + iw(inf, w::P_TEMPLATE_OFF), template);
    wr(reg_row + iw(inf, w::P_TEMPLATE_OFF) + 4, slot);
    wr(reg_row + iw(inf, w::P_TEMPLATE_OFF) + 8, min);
    wr(reg_row + iw(inf, w::P_TEMPLATE_OFF) + 12, max);
    wr(reg_row + iw(inf, w::P_TEMPLATE_OFF) + 16, def);
}

/// Fill the reserved rows from the (checked) records. `owner` = module record index + 1.
pub unsafe fn apply(owner: u8, reg: *const u8, size: u32) -> i32 {
    if size == 0 {
        return OK;
    }
    let inf = info();
    if inf.is_null() {
        return E_NO_REGISTRY;
    }
    let s = rs();
    let n = rd32(reg.add(12));
    // parameters first: their codes are what the machines' knobs name
    let mut codes = [0u32; MAX_PARAM];
    let mut np = 0usize;
    each(reg, |kind, b| {
        if kind != KIND_PARAM {
            return;
        }
        let mut k = 0;
        while k < MAX_PARAM && s.param_owner[k] != 0 {
            k += 1;
        }
        if k == MAX_PARAM {
            return; // cannot happen after `check`
        }
        s.param_owner[k] = owner;
        let code = iw(inf, w::P_DYN0) + k as u32;
        codes[np] = code;
        np += 1;
        copy_text(s.s.param_name[k].as_mut_ptr(), b.add(24), 32);
        copy_text(s.s.param_short[k].as_mut_ptr(), b.add(56), 16);
        write_param_row(
            inf,
            code,
            rd32(b.add(4)),
            rd32(b.add(8)),
            rd32(b.add(12)),
            rd32(b.add(16)),
            rd32(b.add(20)),
            s.s.param_name[k].as_ptr() as u32,
            s.s.param_short[k].as_ptr() as u32,
        );
    });
    let _ = n;
    // pages (Digitone II): their machine is published below, so they sit unused until then
    let slot_base = iw(inf, w::SLOT_BASE);
    each(reg, |kind, b| {
        if kind != KIND_PAGE {
            return;
        }
        let mut k = 0;
        while k < MAX_PAGE && s.page_owner[k] != 0 {
            k += 1;
        }
        if k == MAX_PAGE {
            return; // cannot happen after `check`
        }
        s.page_owner[k] = owner;
        let row = iw(inf, w::PG_ROWS) + (iw(inf, w::PG_DYN0) + k as u32 - iw(inf, w::PG_FIRST)) * iw(inf, w::PG_ROW_SIZE);
        copy_text(s.s.page_name[k].as_mut_ptr(), b.add(40), MACH_TEXT);
        copy_text(s.s.page_title[k].as_mut_ptr(), b.add(40 + MACH_TEXT), MACH_TEXT);
        let name = make_rep(s.s.rep_pname[k].as_mut_ptr(), s.s.page_name[k].as_ptr(), false);
        let title = make_rep(s.s.rep_ptitle[k].as_mut_ptr(), s.s.page_title[k].as_ptr(), false);
        let (c0, s0) = (row + iw(inf, w::PGO_C), row + iw(inf, w::PGO_S));
        let mut j = 0;
        while j < KNOBS {
            let kn = rd32(b.add(4 + 4 * j));
            let known = kn & KNOB_PARAM != 0;
            wr(c0 + 4 * j as u32, if known { codes[(kn & !KNOB_PARAM) as usize] } else { 0 });
            wr(s0 + 4 * j as u32, if known { slot_base + j as u32 } else { 0 });
            j += 1;
        }
        wr(c0 + 4 * KNOBS as u32, rd32(b.add(36))); // the level code
        wr(s0 + 4 * KNOBS as u32, 0);
        wr(row + iw(inf, w::PGO_NAME), name);
        wr(row + iw(inf, w::PGO_TITLE), title);
        wr(row + iw(inf, w::PGO_MACH), crate::assign::resolve(rd32(b))); // the page belongs to its machine from this store on
    });
    // machines
    let knobs = iw(inf, w::KNOBS) as usize;
    each(reg, |kind, b| {
        if kind != KIND_MACHINE {
            return;
        }
        let t = crate::assign::resolve(rd32(b));
        let clone = rd32(b.add(4));
        let k = (t - iw(inf, w::M_DYN0)) as usize;
        let row = iw(inf, w::M_ROWS) + (t - iw(inf, w::M_FIRST)) * iw(inf, w::M_ROW_SIZE);
        let f0 = row + iw(inf, w::O_F0);
        let mut j = 0;
        while j < knobs {
            let kn = rd32(b.add(20 + 4 * j));
            wr(f0 + 4 * j as u32, if kn & KNOB_PARAM != 0 { codes[(kn & !KNOB_PARAM) as usize] } else { kn });
            j += 1;
        }
        wr(f0 + 4 * knobs as u32, rd32(b.add(16))); // the level, the field after the knobs
        wr(row + iw(inf, w::O_CLONE), clone);
        wr(row + iw(inf, w::O_POS), rd32(b.add(8)));
        wr(row + iw(inf, w::O_OWN), rd32(b.add(12)));
        // strings
        copy_text(s.s.mach_name[k].as_mut_ptr(), b.add(52), MACH_TEXT);
        copy_text(s.s.mach_short[k].as_mut_ptr(), b.add(52 + MACH_TEXT), MACH_TEXT);
        let long = make_rep(s.s.rep_long[k].as_mut_ptr(), s.s.mach_name[k].as_ptr(), true);
        let short = make_rep(s.s.rep_short[k].as_mut_ptr(), s.s.mach_short[k].as_ptr(), true);
        wr(row + iw(inf, w::O_LONG), long);
        wr(row + iw(inf, w::O_SHORT), short);
        wr(row + iw(inf, w::O_SNAME), s.s.mach_short[k].as_ptr() as u32);
        // the per-type tables: the clone's row, our names
        let nrow = iw(inf, w::NAMES) + 12 * t;
        wr(nrow, s.s.mach_name[k].as_ptr() as u32);
        wr(nrow + 4, s.s.mach_short[k].as_ptr() as u32);
        wr(nrow + 8, rd(iw(inf, w::NAMES) + 12 * clone + 8));
        wr(iw(inf, w::PERMIT) + 4 * t, rd(iw(inf, w::PERMIT) + 4 * clone));
        let pt = iw(inf, w::PERTYPE);
        if pt != 0 {
            *((pt + t) as *mut u8) = *((pt + clone) as *const u8);
        }
        // publish: the name pointer marks the row as used (one aligned store)
        s.mach_owner[k] = owner;
        wr(row + iw(inf, w::O_NAME), s.s.mach_name[k].as_ptr() as u32);
    });
    // effect pages (Digitakt II): the effect's row in the chain is claimed, so its page, parameter rows and list entry can be written
    let mut rc = OK;
    each(reg, |kind, b| {
        if kind == KIND_EFFECT && rc == OK {
            rc = crate::fxpage::apply(owner, b);
        }
    });
    if rc != OK {
        return rc;
    }
    s.registers = s.registers.wrapping_add(1);
    OK
}

/// A parameter row for an effect knob (`core/fxpages`): the lowering of `write_param_row` with the kit's effect value index as the mirror index and the template's
/// own kind and key words (the reserved machine rows' `0x10 + slot` ids would collide with stock effect parameters).
pub(crate) unsafe fn write_fx_param_row(code: u32, template: u32, mirror: u32, min: u32, max: u32, def: u32, name: u32, short: u32) {
    let inf = info();
    if inf.is_null() {
        return;
    }
    write_param_row(inf, code, template, 0, min, max, def, name, short);
    let table = iw(inf, w::P_TABLE);
    let size = iw(inf, w::PF_SIZE);
    let (row, tpl) = (table + code * size, table + template * size);
    wr(row + 4, mirror);
    wr(row + 0x18, rd(tpl + 0x18));
    wr(row + 0x1c, rd(tpl + 0x1c));
}

/// A parameter row for a send knob (`core/fxpages`, finding 64): the lowering of `write_fx_param_row` with the value class of `class` (the display, and no p-lock: the
/// value class of the kit's Delay rows) and the owner of the stock send row `owner_of` (so the firmware's own set and get reach the current track's Sound array at `mirror`),
/// and no CC and no NRPN (the row's word 6 is -1: `ParameterSet::vfunc_25` sends nothing).
pub(crate) unsafe fn write_send_param_row(code: u32, class: u32, owner_of: u32, mirror: u32, name: u32, short: u32) {
    let inf = info();
    if inf.is_null() {
        return;
    }
    write_fx_param_row(code, class, mirror, 0, 0x7f00, 0, name, short);
    let table = iw(inf, w::P_TABLE);
    let size = iw(inf, w::PF_SIZE);
    let row = table + code * size;
    wr(row, rd(table + owner_of * size));
    wr(row + 0x18, 0xffff_ffff);
}

/// A parameter row back to the reserved state (an empty gap row, as `remove` leaves the machine parameters).
pub(crate) unsafe fn reset_param_row(code: u32) {
    let inf = info();
    if inf.is_null() {
        return;
    }
    let empty = iw(inf, w::EMPTY);
    write_param_row(inf, code, iw(inf, w::RESERVE_TEMPLATE), iw(inf, w::SLOT_BASE), 0, 0, 0, empty, empty);
}

/// The reserved machine, parameter and page rows as (rows the registry has, rows in use); (0, 0) without a registry (for CAPACITY).
pub unsafe fn counts() -> [(u32, u32); 3] {
    let inf = info();
    if inf.is_null() {
        return [(0, 0); 3];
    }
    let s = rs();
    let used = |o: &[u8]| {
        let mut n = 0;
        let mut k = 0;
        while k < o.len() {
            if o[k] != 0 {
                n += 1;
            }
            k += 1;
        }
        n
    };
    [
        (iw(inf, w::M_DYN_N), used(&s.mach_owner)),
        (iw(inf, w::P_DYN_N), used(&s.param_owner)),
        (iw(inf, w::PG_DYN_N), used(&s.page_owner)),
    ]
}

/// The machine type numbers of the machine rows `owner` filled (at most 8), in type order; returns how many.
pub fn types_of(owner: u8, out: &mut [u8; MAX_MACH]) -> usize {
    let s = rs();
    let first = dkrt_sdk::device::DYN_TYPE_FIRST;
    let mut n = 0;
    let mut k = 0;
    while k < MAX_MACH {
        if s.mach_owner[k] == owner && n < MAX_MACH {
            out[n] = (first + k as u32) as u8;
            n += 1;
        }
        k += 1;
    }
    n
}

/// Give back every row `owner` filled: the machines first (hidden, then reset to a gap row), then the parameters.
pub unsafe fn remove(owner: u8) {
    crate::fxpage::remove(owner);
    let inf = info();
    if inf.is_null() {
        return;
    }
    let s = rs();
    let empty = iw(inf, w::EMPTY);
    let knobs = iw(inf, w::KNOBS) as usize;
    let first = iw(inf, w::M_FIRST);
    let mut k = 0;
    while k < MAX_MACH {
        if s.mach_owner[k] == owner {
            let t = iw(inf, w::M_DYN0) + k as u32;
            let row = iw(inf, w::M_ROWS) + (t - first) * iw(inf, w::M_ROW_SIZE);
            wr(row + iw(inf, w::O_NAME), empty); // hide first
            s.empty_rep = [0, 0, 0xffff_ffff, 0];
            let rep = addr_of!(DKRT_REG.empty_rep) as u32 + 12;
            wr(row + iw(inf, w::O_LONG), rep);
            wr(row + iw(inf, w::O_SHORT), rep);
            wr(row + iw(inf, w::O_SNAME), empty);
            let f0 = row + iw(inf, w::O_F0);
            let mut j = 0;
            while j < knobs {
                wr(f0 + 4 * j as u32, 0);
                j += 1;
            }
            wr(f0 + 4 * knobs as u32, 10);
            wr(row + iw(inf, w::O_CLONE), 0);
            wr(row + iw(inf, w::O_OWN), 0);
            let nrow = iw(inf, w::NAMES) + 12 * t;
            wr(nrow, empty);
            wr(nrow + 4, empty);
            wr(nrow + 8, rd(iw(inf, w::NAMES) + 8));
            wr(iw(inf, w::PERMIT) + 4 * t, rd(iw(inf, w::PERMIT)));
            let pt = iw(inf, w::PERTYPE);
            if pt != 0 {
                *((pt + t) as *mut u8) = *(pt as *const u8);
            }
            s.mach_owner[k] = 0;
        }
        k += 1;
    }
    // pages: unbind from the machine first, then the rest of the row back to a free row
    k = 0;
    while k < MAX_PAGE {
        if s.page_owner[k] == owner {
            let row = iw(inf, w::PG_ROWS)
                + (iw(inf, w::PG_DYN0) + k as u32 - iw(inf, w::PG_FIRST)) * iw(inf, w::PG_ROW_SIZE);
            wr(row + iw(inf, w::PGO_MACH), iw(inf, w::PG_FREE));
            s.empty_rep = [0, 0, 0xffff_ffff, 0];
            let rep = addr_of!(DKRT_REG.empty_rep) as u32 + 12;
            wr(row + iw(inf, w::PGO_NAME), rep);
            wr(row + iw(inf, w::PGO_TITLE), rep);
            let (c0, s0) = (row + iw(inf, w::PGO_C), row + iw(inf, w::PGO_S));
            let mut j = 0;
            while j < KNOBS {
                wr(c0 + 4 * j as u32, 0);
                wr(s0 + 4 * j as u32, 0);
                j += 1;
            }
            wr(c0 + 4 * KNOBS as u32, 10); // the track level code, as in a free row
            wr(s0 + 4 * KNOBS as u32, 0);
            s.page_owner[k] = 0;
        }
        k += 1;
    }
    k = 0;
    while k < MAX_PARAM {
        if s.param_owner[k] == owner {
            write_param_row(
                inf,
                iw(inf, w::P_DYN0) + k as u32,
                iw(inf, w::RESERVE_TEMPLATE),
                iw(inf, w::SLOT_BASE),
                0,
                0,
                0,
                empty,
                empty,
            );
            s.param_owner[k] = 0;
        }
        k += 1;
    }
    s.removes = s.removes.wrapping_add(1);
}
