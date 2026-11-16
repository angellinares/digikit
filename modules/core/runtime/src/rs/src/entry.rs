//! The module entry point and the service imports of docs/plans/sdk.md section 5.
//!
//! A module defines `dk_module`; the packer writes `init`, `fini`, `tick` and `menu` into the `.dkm` header, and the runtime calls them with a `dk_module_ctx`: a
//! pointer to the module's record in `DKRT_CTXS`. `tick` runs once per pass of the UI task for every loaded module, in `dkrt_tick`.
//!
//! The service imports (`[[import]]` with `service` in sdk/sdk.toml) that name a handle live here: the runtime hands out the handle of a service in `init`
//! (`dk_settings_open`, `dk_files_open`, `dk_sample_pool_open`) and each call takes the handle. The handle is the client number the existing code
//! (`settings.rs`, `files.rs`, `tracks.rs`) keeps, as a pointer. The imports that need no handle point at the functions of `imports.rs`.

use crate::{dlog, files, settings, state::REC_MAX, store, store_writer, MAX_MODULES};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

/// "DKCX": the first word of a live context.
pub const CTX_MAGIC: u32 = 0x444b_4358;

/// What a `dk_module_ctx *` points to: the record of the module that `init`, `fini`, `tick` and `menu` belong to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ctx {
    pub magic: u32,
    pub rec: u32,
}

const NO_CTX: Ctx = Ctx { magic: 0, rec: 0 };

/// The context of each module record; `magic` is `CTX_MAGIC` for a module of the new entry while it is loaded.
#[no_mangle]
pub static mut DKRT_CTXS: [Ctx; REC_MAX] = [NO_CTX; REC_MAX];
/// The address of each module record's `tick` (0: none); one aligned store publishes or clears it, as for the slots.
#[no_mangle]
pub static mut DKRT_TICKS: [u32; REC_MAX] = [0; REC_MAX];

/// Make record `rec` a module of the new entry: its context is live from now on.
pub unsafe fn bind(rec: usize) -> *const Ctx {
    let c = &mut (*addr_of_mut!(DKRT_CTXS))[rec];
    c.rec = rec as u32;
    c.magic = CTX_MAGIC;
    c as *const Ctx
}

/// Record `rec` goes away: no tick, no context.
pub unsafe fn unbind(rec: usize) {
    write_volatile(addr_of_mut!(DKRT_TICKS[rec]), 0);
    (*addr_of_mut!(DKRT_CTXS))[rec] = NO_CTX;
}

/// Start calling `tick` (a module address) of record `rec` once per UI pass; called when `init` has succeeded.
pub unsafe fn publish_tick(rec: usize, tick: u32) {
    write_volatile(addr_of_mut!(DKRT_TICKS[rec]), tick);
}

/// Stop the tick of record `rec` (its context stays for `fini`).
pub unsafe fn stop_tick(rec: usize) {
    write_volatile(addr_of_mut!(DKRT_TICKS[rec]), 0);
}

/// Does record `rec` have a tick?
pub fn has_tick(rec: usize) -> bool {
    rec < REC_MAX && unsafe { read_volatile(addr_of!(DKRT_TICKS[rec])) != 0 }
}

/// Is record `rec` a loaded module (it has a live context)?
pub unsafe fn is_entry(rec: usize) -> bool {
    rec < REC_MAX && read_volatile(addr_of!(DKRT_CTXS[rec].magic)) == CTX_MAGIC
}

/// The context of record `rec` as a pointer (0 when the record is not loaded), for `dkrt_module`.
pub unsafe fn ctx_addr(rec: usize) -> u32 {
    if is_entry(rec) {
        addr_of!(DKRT_CTXS[rec]) as usize as u32
    } else {
        0
    }
}

/// The record a `dk_module_ctx *` belongs to; None for a pointer that is not one of `DKRT_CTXS`' live entries.
unsafe fn ctx_rec(ctx: *mut u8) -> Option<usize> {
    let base = addr_of!(DKRT_CTXS) as usize;
    let at = ctx as usize;
    let size = core::mem::size_of::<Ctx>();
    if at < base || at >= base + REC_MAX * size || (at - base) % size != 0 {
        return None;
    }
    let rec = (at - base) / size;
    if is_entry(rec) {
        Some(rec)
    } else {
        None
    }
}

/// Is `ctx` the live context of a loaded module (the builder calls take it)?
pub unsafe fn is_ctx(ctx: *mut u8) -> bool {
    ctx_rec(ctx).is_some()
}

/// The record of the module that is running when it passes its own context (an `open` call), else None.
pub(crate) unsafe fn own_rec(ctx: *mut u8) -> Option<usize> {
    let rec = ctx_rec(ctx)?;
    if dlog::current_source() as usize == rec + 1 {
        Some(rec)
    } else {
        None
    }
}

/// Call every loaded module's `tick` once, in the UI task, with the module as the source of what it logs.
pub unsafe fn tick_all() {
    let mut rec = 0;
    while rec < MAX_MODULES {
        let t = read_volatile(addr_of!(DKRT_TICKS[rec]));
        if t != 0 {
            let call: unsafe extern "C" fn(*const Ctx) = core::mem::transmute(t as usize);
            let prev = dlog::enter_module(rec, dlog::PHASE_TICK);
            let t0 = dlog::span_t0();
            call(addr_of!(DKRT_CTXS[rec]));
            dlog::leave_module(prev);
            // The emit runs after `leave_module`: the sink's own work and faults are not the module's.
            if let Some(t) = t0 {
                dlog::span_end(rec as u32, t);
            }
            let s = crate::st();
            s.tick_calls = s.tick_calls.wrapping_add(1);
        }
        rec += 1;
    }
}

/// A handle is the client number the existing code keeps, as a pointer (never null: a client is 1 or more).
pub(crate) fn handle(client: i32) -> *mut u8 {
    client as usize as *mut u8
}

pub(crate) fn client(h: *mut u8) -> u32 {
    h as usize as u32
}

/// Hand out `client` (positive) in `*out`: 0; a negative result is the error, `*out` untouched.
pub(crate) unsafe fn give(out: *mut *mut u8, client: i32) -> i32 {
    if client <= 0 {
        return if client == 0 { E_NOT_FOUND } else { client };
    }
    *out = handle(client);
    OK
}

// ---- the context -------------------------------------------------------------------------------------------------------------

pub unsafe extern "C" fn dk_module_ctx_device(ctx: *mut u8) -> u32 {
    if ctx_rec(ctx).is_some() {
        crate::device()
    } else {
        0
    }
}

pub unsafe extern "C" fn dk_module_ctx_runtime_version(ctx: *mut u8) -> u32 {
    if ctx_rec(ctx).is_some() {
        RUNTIME_VERSION
    } else {
        0
    }
}

// ---- settings ---------------------------------------------------------------------------------------------------------------

pub unsafe extern "C" fn dk_settings_open(ctx: *mut u8, out: *mut *mut u8) -> i32 {
    if out.is_null() || own_rec(ctx).is_none() {
        return E_CONTRACT;
    }
    give(out, settings::module_client())
}

pub unsafe extern "C" fn dk_settings_get(s: *mut u8, key: *const u8, out: *mut u8, cap: u32) -> i32 {
    settings::module_get(client(s), key, out, cap)
}

pub unsafe extern "C" fn dk_settings_put(s: *mut u8, key: *const u8, value: *const u8, len: u32) -> i32 {
    settings::module_put(client(s), key, value, len)
}

pub unsafe extern "C" fn dk_settings_remove(s: *mut u8, key: *const u8) -> i32 {
    settings::module_remove(client(s), key)
}

// ---- files (the read-only listing) -----------------------------------------------------------------------------------------

pub unsafe extern "C" fn dk_files_open(ctx: *mut u8, out: *mut *mut u8) -> i32 {
    if out.is_null() || own_rec(ctx).is_none() {
        return E_CONTRACT;
    }
    give(out, files::client())
}

pub unsafe extern "C" fn dk_files_list(f: *mut u8, root: u32, path: *const u8, notify: Option<api::FilesEventFn>, arg: *mut u8) -> i32 {
    files::list(client(f), root, path, notify, arg)
}

pub unsafe extern "C" fn dk_files_poll(f: *mut u8, ticket: u32, out: *mut u8, capacity: u32) -> i32 {
    files::poll(client(f), ticket, out, capacity)
}

pub unsafe extern "C" fn dk_files_cancel(f: *mut u8, ticket: u32) -> i32 {
    files::cancel(client(f), ticket)
}

/// Read a store file into `buf` (init only, through the runtime's staging buffer). The handle names the owner and is not looked up yet.
pub unsafe extern "C" fn dk_files_read(_f: *mut u8, path: *const u8, buf: *mut u8, cap: u32) -> i32 {
    store::read_file(path, buf, cap)
}

// The write calls take the same handle. The store has one write session in all, so the handle only names the owner: it is not looked up yet.

pub unsafe extern "C" fn dk_files_create(_f: *mut u8, root: u32, path: *const u8, size: u32, hash4: *const u8, flags: u32) -> i32 {
    store_writer::sdk_create(root, path, size, hash4, flags)
}

pub unsafe extern "C" fn dk_files_write(_f: *mut u8, h: i32, offset: u32, buf: *const u8, len: u32) -> i32 {
    store_writer::sdk_write(h, offset, buf, len)
}

pub unsafe extern "C" fn dk_files_close(_f: *mut u8, h: i32, hash_out: *mut u8) -> i32 {
    store_writer::sdk_close(h, hash_out)
}

pub unsafe extern "C" fn dk_files_abort(_f: *mut u8, h: i32) -> i32 {
    store_writer::sdk_abort(h)
}

pub unsafe extern "C" fn dk_files_delete(_f: *mut u8, root: u32, path: *const u8) -> i32 {
    store_writer::sdk_delete(root, path)
}

pub unsafe extern "C" fn dk_files_commit(_f: *mut u8) -> i32 {
    store_writer::sdk_commit()
}

pub unsafe extern "C" fn dk_files_pending(_f: *mut u8) -> i32 {
    store_writer::sdk_pending()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_context_is_one_live_entry_of_the_table() {
        unsafe {
            let rec = MAX_MODULES - 1;
            unbind(rec);
            let before = addr_of!(DKRT_CTXS[rec]) as usize as *mut u8;
            assert!(ctx_rec(before).is_none(), "an unbound record is not a context");
            assert_eq!(dk_module_ctx_device(before), 0);
            let c = bind(rec);
            assert_eq!(c as usize, before as usize);
            assert_eq!(ctx_rec(before), Some(rec));
            assert!(is_entry(rec) && ctx_addr(rec) == before as usize as u32);
            assert!(ctx_rec((before as usize + 1) as *mut u8).is_none(), "an unaligned pointer is no context");
            assert!(ctx_rec(core::ptr::null_mut()).is_none());
            assert!(ctx_rec(addr_of!(DKRT_TICKS) as usize as *mut u8).is_none(), "another table is no context");
            unbind(rec);
            assert!(ctx_rec(before).is_none() && ctx_addr(rec) == 0);
        }
    }

    #[test]
    fn a_handle_is_the_client_number() {
        unsafe {
            let mut out: *mut u8 = core::ptr::null_mut();
            assert_eq!(give(&mut out, 3), OK);
            assert_eq!(client(out), 3);
            assert_eq!(give(&mut out, E_NOT_FOUND), E_NOT_FOUND);
            assert_eq!(client(out), 3, "a refusal leaves the handle as it was");
            let mut none: *mut u8 = core::ptr::null_mut();
            assert_eq!(give(&mut none, 0), E_NOT_FOUND);
            assert!(none.is_null());
        }
    }

    #[test]
    fn open_needs_the_running_modules_own_context() {
        unsafe {
            let mut out: *mut u8 = core::ptr::null_mut();
            assert_eq!(dk_settings_open(core::ptr::null_mut(), &mut out), E_CONTRACT);
            assert_eq!(dk_files_open(core::ptr::null_mut(), &mut out), E_CONTRACT);
            assert_eq!(crate::handlers::exports::dk_sample_pool_open(core::ptr::null_mut(), &mut out), E_CONTRACT);
            assert!(out.is_null());
        }
    }
}
