//! The upload commands about modules: MODULES, LOADS and SELECT, and the commands that modules register (`dk_host_register`).

use crate::handlers::services;
use crate::store_writer::copy;
use crate::platform::wr32;
use crate::{boot, card, dlog};
use core::ptr::{addr_of, addr_of_mut};
use dkrt_sdk::proto::*;
use dkrt_sdk::OK;
use crate::transport::*;

/// MODULES: the loaded modules and what they hold. Body: `u8 start` (a module record index). Reply: `u32 arena_free, arena_largest, dsp_free,
/// dsp_largest`, `u8 total` (loaded modules), `u8 n` (records in this reply), then per record `u8 index`, `name[32]`, `u32 version`, 8 bytes (`u32` the runtime
/// version the module needs, then four zero bytes), `u32 cf_base, cf_size, dsp_off, dsp_len, rows` (voice rows as a mask), `u8 n_types` and that many machine
/// type bytes (at most 8: the machine rows of the module, then its voice rows' types), `u32 faults` (the guard's count for those types), `u8 flags` (bit 0:
/// the guard switched one of them off). A reply holds the records from `start` on that fit; the host asks again from the last index + 1 until it has `total`.
pub(crate) unsafe fn cmd_modules(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen < 1 {
        return reply(CMD_MODULES, seq, E_MSG, 0);
    }
    let start = *body as usize;
    let s = crate::st();
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    let (af, al) = crate::arena::stats();
    let (df, dl) = crate::dsp::area_stats();
    wr32(rb, af);
    wr32(rb.add(4), al);
    wr32(rb.add(8), df);
    wr32(rb.add(12), dl);
    let mut total = 0u8;
    let mut i = 0;
    while i < crate::MAX_MODULES {
        if s.modules[i].used != 0 {
            total += 1;
        }
        i += 1;
    }
    *rb.add(16) = total;
    let mut at = 18usize;
    let mut n = 0u8;
    let d = crate::dsp::ds();
    i = start;
    while i < crate::MAX_MODULES {
        let m = &s.modules[i];
        if m.used == 0 {
            i += 1;
            continue;
        }
        // the machine types: rows of the registry, then voice rows (pinned numbers of a DSP-only module)
        let mut types = [0u8; 8];
        let mut nt = crate::handlers::exports::machine_types_of(i as u8 + 1, &mut types);
        let mut r = 0;
        while r < crate::dsp::MAX_ROWS {
            if d.row_owner[r] == i as u8 + 1 && d.row_type[r] < 256 && nt < 8 {
                let t = d.row_type[r] as u8;
                let mut dup = false;
                let mut j = 0;
                while j < nt {
                    dup |= types[j] == t;
                    j += 1;
                }
                if !dup {
                    types[nt] = t;
                    nt += 1;
                }
            }
            r += 1;
        }
        if at + 71 + nt > REPLY_BODY_MAX - 1 {
            break;
        }
        *rb.add(at) = i as u8;
        copy(rb.add(at + 1), m.name.as_ptr(), 32);
        wr32(rb.add(at + 33), m.version);
        // the version of the runtime the module needs, then four reserved zero bytes
        wr32(rb.add(at + 37), (*addr_of!(crate::DKRT_MODULE_RUNTIME))[i]);
        wr32(rb.add(at + 41), 0);
        wr32(rb.add(at + 45), m.base);
        wr32(rb.add(at + 49), m.size);
        wr32(rb.add(at + 53), d.mod_off[i]);
        wr32(rb.add(at + 57), d.mod_len[i]);
        wr32(rb.add(at + 61), d.mod_rows[i]);
        *rb.add(at + 65) = nt as u8;
        let mut faults = 0u32;
        let mut off = false;
        let mut j = 0;
        while j < nt {
            *rb.add(at + 66 + j) = types[j];
            if let Some(g) = services().guard_state(types[j] as u32) {
                faults = faults.wrapping_add(g.faults);
                off |= g.disabled;
            }
            j += 1;
        }
        wr32(rb.add(at + 66 + nt), faults);
        *rb.add(at + 70 + nt) = off as u8;
        at += 71 + nt;
        n += 1;
        i += 1;
    }
    *rb.add(17) = n;
    reply(CMD_MODULES, seq, OK, at)
}

/// LOADS: what this boot's selection did. Body: none. Reply (version 2, `dkrt_abi::state::loads_v2`): `u8 version` (2), `u8 n` (the modules of the selection, at
/// most 8), then per module in selection order `i32 result` (`OK`, a negative `E_*`, or LOADING while its DSP part is going), `u8 state` (`[load.state]`),
/// 3 zero bytes, `u32 detail[2]` (what the plan refused on), `needs[12]` (with E_IMPORT: the module its imports need that is not in the image, without `core/`,
/// NUL padded; else zeros) and `name[28]` (the file name of its path, NUL padded). The same data as the DIGIKIT menu's BOOT page, for a host that has no debug
/// module to read the log from.
pub(crate) unsafe fn cmd_loads(seq: u32) -> i32 {
    let rb = core::slice::from_raw_parts_mut((addr_of_mut!(RBODY) as *mut u8).add(1), dkrt_abi::state::loads_len(dkrt_sdk::SEL_MAX));
    let at = dkrt_abi::state::loads_v2(&boot::loads_view(), rb);
    reply(CMD_LOADS, seq, OK, at)
}

/// SELECT (runtime 4): save the selection record, the same `boot::selection_set` the menu's APPLY & RESTART calls (so the card range check and
/// the "blank or ours" rule apply). Body: `u8 n`, then `n` store paths, each NUL terminated. Status: 0, `E_MSG` (malformed), `E_INUSE` (card
/// not writable), or the selection code (`E_SEL_COUNT`, `E_SEL_PATH`, `E_SEL_WRITE`, `E_CARD_RO`). Takes effect at the next boot.
pub(crate) unsafe fn cmd_select(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen < 1 {
        return reply(CMD_SELECT, seq, E_MSG, 0);
    }
    if !card::writable() {
        return reply(CMD_SELECT, seq, E_INUSE, 0);
    }
    let n = *body as usize;
    if n > dkrt_sdk::SEL_MAX {
        return reply(CMD_SELECT, seq, dkrt_sdk::E_SEL_COUNT, 0);
    }
    // static, not on the MIDI task's stack (the record is about 450 bytes)
    let sel = &mut *addr_of_mut!(SELBUF);
    *sel = core::mem::zeroed();
    let mut at = 1usize;
    let mut k = 0usize;
    while k < n {
        let mut len = 0usize;
        while at + len < blen && *body.add(at + len) != 0 {
            len += 1;
        }
        if at + len >= blen {
            return reply(CMD_SELECT, seq, E_MSG, 0); // no NUL
        }
        if len >= dkrt_sdk::SEL_PATH {
            return reply(CMD_SELECT, seq, dkrt_sdk::E_SEL_PATH, 0);
        }
        let mut i = 0;
        while i < len {
            sel.paths[k][i] = *body.add(at + i);
            i += 1;
        }
        at += len + 1;
        k += 1;
    }
    if at != blen {
        return reply(CMD_SELECT, seq, E_MSG, 0);
    }
    sel.count = n as u32;
    let r = boot::selection_set(sel);
    reply(CMD_SELECT, seq, r, 0)
}

pub(crate) static mut SELBUF: dkrt_sdk::DkrtSelection = dkrt_sdk::DkrtSelection { count: 0, flags: 0, paths: [[0; dkrt_sdk::SEL_PATH]; dkrt_sdk::SEL_MAX] };

pub(crate) unsafe fn h_modules(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_modules(seq, body, blen)
}
pub(crate) unsafe fn h_loads(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_loads(seq)
}
pub(crate) unsafe fn h_select(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_select(seq, body, blen)
}
// ---- commands that modules register (dk_host_register) ------------------------------------------------------------------

/// Registered ranges at most (a module takes one per command group: digikit/remote 2, digikit/probe 1).
pub(crate) const SVC_MAX: usize = 8;

#[derive(Clone, Copy)]
pub(crate) struct Svc {
    pub(crate) first: u8,
    pub(crate) last: u8,
    /// `dlog::current_source()` of the registering module (record index + 1); 0: the slot is free.
    pub(crate) owner: u8,
    pub(crate) h: Option<dkrt_sdk::HostCmdFn>,
}

pub(crate) static mut SVC: [Svc; SVC_MAX] = [Svc { first: 0, last: 0, owner: 0, h: None }; SVC_MAX];

pub(crate) fn svc() -> &'static mut [Svc; SVC_MAX] {
    unsafe { &mut *addr_of_mut!(SVC) }
}

/// The handler of a registered command.
pub(crate) fn service(cmd: u8) -> Option<dkrt_sdk::HostCmdFn> {
    service_of(cmd).and_then(|s| s.h)
}

pub(crate) fn service_of(cmd: u8) -> Option<&'static Svc> {
    let t = svc();
    let mut i = 0;
    while i < SVC_MAX {
        if t[i].owner != 0 && cmd >= t[i].first && cmd <= t[i].last {
            return Some(&t[i]);
        }
        i += 1;
    }
    None
}

/// `dk_host_register`: `E_CMD_RANGE` when the range is empty, outside every reserved range, over a registered one, or there is no free
/// slot; else 0.
pub unsafe fn host_register(first: u32, count: u32, handler: Option<dkrt_sdk::HostCmdFn>) -> i32 {
    if handler.is_none() || count == 0 || first == 0 || first > 0xff || count > 0xff || first + count - 1 > 0xff {
        return E_CMD_RANGE as i32;
    }
    let (lo, hi) = (first as u8, (first + count - 1) as u8);
    let mut reserved = false;
    let mut i = 0;
    while i < RANGES.len() {
        if lo >= RANGES[i].0 && hi <= RANGES[i].1 {
            reserved = true;
        }
        i += 1;
    }
    if !reserved {
        return E_CMD_RANGE as i32;
    }
    let t = svc();
    let mut free = SVC_MAX;
    i = 0;
    while i < SVC_MAX {
        if t[i].owner == 0 {
            free = i;
        } else if lo <= t[i].last && t[i].first <= hi {
            return E_CMD_RANGE as i32;
        }
        i += 1;
    }
    if free == SVC_MAX {
        return E_CMD_RANGE as i32;
    }
    // a registration outside a module's init (owner 0) is kept under owner 255: nothing releases it
    let owner = dlog::current_source();
    t[free] = Svc { first: lo, last: hi, owner: if owner == 0 { 255 } else { owner }, h: handler };
    OK
}

/// The module whose source index is `owner` was taken down: release its ranges.
pub unsafe fn release_cmds(owner: u8) {
    let t = svc();
    let mut i = 0;
    while i < SVC_MAX {
        if t[i].owner == owner {
            t[i] = Svc { first: 0, last: 0, owner: 0, h: None };
        }
        i += 1;
    }
}


