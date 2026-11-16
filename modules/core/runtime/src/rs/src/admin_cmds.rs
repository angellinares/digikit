//! The upload commands about the runtime itself: INFO, CAPACITY, SETTINGS, FAULT and RESTART (with the pending restart that `restart_poll` runs).

use crate::store_writer::{self as sw};
use crate::store_writer::{copy, free_bytes};
use crate::platform::{rd32, tx_in_flight, wr32};
use crate::{boot, card, dlog};
use core::ptr::addr_of_mut;
use dkrt_sdk::proto::*;
use dkrt_sdk::{logk, DkrtCap, CAP_MAX, OK, RUNTIME_VERSION};
use crate::transport::*;
use crate::transport::MAX_CHUNK;
use crate::store_cmds::state;

pub(crate) unsafe fn cmd_info(seq: u32, buf: *mut u8) -> i32 {
    let st = state(buf);
    let free = match &st {
        Ok(s) => free_bytes(&s.h),
        Err(_) => 0,
    };
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = 0;
    *rb.add(1) = VERSION; // protocol 1
    *rb.add(2) = (RUNTIME_VERSION >> 8) as u8;
    *rb.add(3) = RUNTIME_VERSION as u8;
    wr32(rb.add(4), crate::device_name()); // the device's short name from its device file
    put32(1 + 8, free);
    put32(1 + 12, MAX_CHUNK);
    let flags = (boot::info().safe & 1)
        | if sw::is_open() { 2 } else { 0 }
        | if card::writable() { 0 } else { 4 }
        | if dlog::sink_present() { 8 } else { 0 }
        | if crate::fault::seen().found != 0 { 16 } else { 0 };
    *rb.add(16) = flags as u8;
    // the log range (bytes 17 to 24) stays zero: the log is the debug module's, whose LOG command has the range
    wr32(rb.add(17), 0);
    wr32(rb.add(21), 0);
    // appended after the log range: the whole packed runtime version (bytes 2-3 keep its low 16 bits) and the capability bits
    wr32(rb.add(25), RUNTIME_VERSION);
    let log_caps = if dlog::sink_present() { CAP_LOG | CAP_PEEK | CAP_LOGCFG } else { 0 };
    wr32(rb.add(29), log_caps | CAP_SELECT | CAP_RESTART | CAP_MODULES | CAP_CAPACITY | CAP_READ | CAP_VERIFY | CAP_SERVICE_CMDS | CAP_FOLDERS | CAP_SETTINGS | CAP_FILEHASH | CAP_RENAME | CAP_READBACK | CAP_LOADS | CAP_SVC_ABI);
    // appended after the capability bits: the id of the flashed image (patchkit stamps it; zeros when there is none)
    let id = crate::image_id();
    let mut i = 0;
    while i < 8 {
        *rb.add(33 + i) = id[i];
        i += 1;
    }
    // appended after the image id: the ABI fingerprint of interfaces B and C (`dkrt_abi::SVC_ABI`, CAP_SVC_ABI), the word a service file carries at header 0x3c
    wr32(rb.add(41), dkrt_abi::service::SVC_ABI);
    reply(CMD_INFO, seq, OK, 45)
}

/// CAPACITY: per resource of the runtime, its limit, what is used and the largest free piece. Body: none. Reply: `u8 version` (1), `u8 n`, then
/// `n` entries of `u8 kind` (`CAPK_*`, sdk/sdk.toml `[capacity.kinds]`), `u32 limit`, `u32 used`, `u32 largest` (0 where a largest piece has
/// no meaning). A host skips a kind it does not know; new kinds are appended.
pub(crate) unsafe fn cmd_capacity(seq: u32) -> i32 {
    let mut entries = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let n = crate::dkrt_capacity(entries.as_mut_ptr(), CAP_MAX as u32) as usize;
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = 1;
    *rb.add(1) = n as u8;
    let mut at = 2usize;
    let mut k = 0;
    while k < n {
        let e = &entries[k];
        *rb.add(at) = e.kind as u8;
        wr32(rb.add(at + 1), e.limit);
        wr32(rb.add(at + 5), e.used);
        wr32(rb.add(at + 9), e.largest);
        at += 13;
        k += 1;
    }
    reply(CMD_CAPACITY, seq, OK, at)
}

/// SETTINGS (read only): the settings record as the runtime holds it (docs/design/digikit-store.md, "The settings record"). Body: `u32 offset`, `u16 length`
/// (1 to 768). Reply: `u8 flags` (bit 0: the record has changes that the card does not hold yet), `u8 copy` (the card's current copy: 0 A, 1 B, 2 none),
/// `u32 seq` (the card's), `u32 used` (the record's bytes), then `min(length, used - offset)` bytes of the record from `offset`. A host reads in pieces
/// and decodes the whole (the format is `dkrt_sdk::settings`). E_SET_NOKEY: no record on the card and none made; E_OFFSET: offset past the record.
pub(crate) unsafe fn cmd_settings(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen != 6 {
        return reply(CMD_SETTINGS, seq, E_MSG, 0);
    }
    let off = rd32(body) as usize;
    let mut len = (((*body.add(4) as u32) << 8) | *body.add(5) as u32) as usize;
    if len == 0 {
        return reply(CMD_SETTINGS, seq, E_LENGTH, 0);
    }
    if crate::settings::is_off() {
        return reply(CMD_SETTINGS, seq, E_SET_OFF, 0);
    }
    let (flags, cur, rseq, rec) = match crate::settings::snapshot() {
        Some(r) => r,
        None => return reply(CMD_SETTINGS, seq, E_SET_NOKEY, 0),
    };
    if off >= rec.len() {
        return reply(CMD_SETTINGS, seq, E_OFFSET, 0);
    }
    if len > READ_MAX as usize {
        len = READ_MAX as usize;
    }
    if len > rec.len() - off {
        len = rec.len() - off;
    }
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    *rb = flags;
    *rb.add(1) = cur as u8;
    wr32(rb.add(2), rseq);
    wr32(rb.add(6), rec.len() as u32);
    copy(rb.add(10), rec.as_ptr().add(off), len);
    reply(CMD_SETTINGS, seq, OK, 10 + len)
}

/// FAULT (dev): body `u8 what`; 1 makes the runtime panic with a failed bounds check, to test the fault path. It does not reply: the device resets.
/// Other values: `E_PROTO`.
pub(crate) unsafe fn cmd_fault(seq: u32, body: *const u8, blen: usize) -> i32 {
    if blen != 1 {
        return reply(CMD_FAULT, seq, E_MSG, 0);
    }
    if *body != 1 {
        return reply(CMD_FAULT, seq, E_PROTO, 0);
    }
    let a = [1u8, 2, 3, 4];
    let i = core::hint::black_box(a.len() + 3);
    reply(CMD_FAULT, seq, OK, a[i] as usize)
}

/// Ticks to wait for the reply to leave the TX ring, at most, before the restart goes anyway.
pub(crate) const RESTART_TICKS_MAX: u32 = 4000;
/// 0: no restart pending; else 1 + the UI passes waited so far.
pub(crate) static mut RESTART_WAIT: u32 = 0;

/// RESTART (runtime 4): the reply goes out first; the soft reset (`dkrt_restart`, the one APPLY & RESTART calls) runs from the UI task's next
/// passes (`restart_poll`), once the TX ring has drained. Status: 0, or `E_STATE` while an upload is open. No body.
pub(crate) unsafe fn cmd_restart(seq: u32) -> i32 {
    // an upload is open (an SDK commit keeps its session open until it is done), or a store operation is in flight
    if sw::is_open() || sw::is_locked() {
        return reply(CMD_RESTART, seq, E_STATE, 0);
    }
    RESTART_WAIT = 1;
    reply(CMD_RESTART, seq, OK, 0)
}

/// Called once per UI pass (`dkrt_tick`). Runs a pending RESTART after its reply has been sent. `dkrt_restart` returns only when it refuses
/// (a card transfer in flight): the request is then dropped and logged as an error; the host sees the runtime still running and asks again.
pub unsafe fn restart_poll() {
    if RESTART_WAIT == 0 {
        return;
    }
    if sw::is_locked() {
        return; // a store write is in flight: the reset waits (the wait counter does not run)
    }
    RESTART_WAIT += 1;
    let drained = matches!(tx_in_flight(), Some(0));
    if (drained && RESTART_WAIT > 3) || RESTART_WAIT > RESTART_TICKS_MAX {
        RESTART_WAIT = 0;
        let r = crate::dkrt_restart();
        dlog::ev(logk::UPLOAD, logk::F_ERROR, [CMD_RESTART as u32, r as u32, 0, 0, 0]);
    }
}

pub(crate) unsafe fn h_info(seq: u32, buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_info(seq, buf)
}
pub(crate) unsafe fn h_capacity(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_capacity(seq)
}
pub(crate) unsafe fn h_settings(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_settings(seq, body, blen)
}
pub(crate) unsafe fn h_fault(seq: u32, _buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_fault(seq, body, blen)
}
pub(crate) unsafe fn h_restart(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_restart(seq)
}

