//! dkrt-svc-rt: what a service file (`.dks`) links to talk to the runtime (docs/plans/runtime-and-services.md section 13.2).
//!
//! - `ImportCore`: a zero-size `CoreApi` over the `dkc_*` imports of `dkrt_abi::core_imports`. A `Service<C: CoreApi>` runs over it unchanged; the static rows
//!   of the runtime run the same code over `RealCore`, the host tests over `FakeCore`.
//! - `service!`: the module record `dk_module` (its `init` declares the service's record kinds, its `ServiceRow` and, with `setup =` and `hooks =`, its hook sites:
//!   `("cf.page.hook.page_desc", hooks::fxp_acc)` for every device, `(name, entry, dt2)` or `(name, entry, dn2)` for one; see `modules/README.md` "Services"), the data word `dk_svc_abi` the packer
//!   copies to header word 0x3c, and the panic handler (the runtime's panic: crash capture, safe mode on the next boot).
//!
//! `#![no_std]`. The boundary is `extern "C"` with `repr(C)` types only (`improper_ctypes` is denied).
#![no_std]
#![deny(improper_ctypes, improper_ctypes_definitions)]

pub use dkrt_abi;
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::dsp::DspState;
use dkrt_abi::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
use dkrt_abi::registry::RegState;
use dkrt_abi::service::ServiceRow;
use dkrt_sdk::api::{JobDoneFn, JobStepFn};

/// The imports (`dkrt_abi::core_imports`): the names the runtime's `SVC_IMPORTS` resolves when the file is loaded.
pub mod sys {
    macro_rules! declare {
        ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
            extern "C" {
                $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)*
            }
        };
    }
    dkrt_abi::core_imports!(declare);
}

extern "C" {
    /// An import of every module (`dk_service_declare_kind` of sdk.toml): one record kind this service owns, with its length and handler entry.
    fn dk_service_declare_kind(ctx: *mut u8, kind: u32, len: u32, handler: *const u8) -> i32;
}
const _: dkrt_sdk::api::DkServiceDeclareKindFn = dk_service_declare_kind;
extern "C" {
    /// An import of every module (`dk_service_declare_hook` of sdk.toml): one hook site this service takes (the `name_hash` of the hook's name) and the function that
    /// runs there. The runtime compares it with the `HOOKS` of the header table at the commit and installs the site after `init`.
    fn dk_service_declare_hook(ctx: *mut u8, site: u32, entry: *const u8) -> i32;
    /// An import of every module (`dk_service_declare_slot` of sdk.toml): one pointer slot of an image adapter this service fills (the `name_hash` of the slot's name) and the
    /// function whose address the installer writes in it. The runtime compares it with the `SLOTS` of the header table.
    fn dk_service_declare_slot(ctx: *mut u8, slot: u32, flags: u32, entry: *const u8) -> i32;
    /// An import of every module (`dk_service_declare_export` of sdk.toml): one function this service offers to other modules (the `name_hash` of its name) and its entry.
    /// The runtime compares it with the `EXPORTS` of the header table.
    fn dk_service_declare_export(ctx: *mut u8, name_hash: u32, entry: *const u8) -> i32;
}
const _: dkrt_sdk::api::DkServiceDeclareHookFn = dk_service_declare_hook;
const _: dkrt_sdk::api::DkServiceDeclareSlotFn = dk_service_declare_slot;
const _: dkrt_sdk::api::DkServiceDeclareExportFn = dk_service_declare_export;

/// `name_hash` of a name (FNV-1a 32, as `dkneeds::keys::name_hash`): the identity of a hook site in `HOOKS`.
pub const fn name_hash(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
}

/// The `[digikit] id` of a device in the form `service!` takes for the device filter of a hook (`dt2`, `dn2`; no filter: 0, every device).
#[doc(hidden)]
pub const DEVICE_DT2: u32 = dkrt_abi::profiles::DT2.id;
#[doc(hidden)]
pub const DEVICE_DN2: u32 = dkrt_abi::profiles::DN2.id;

use sys::*;

/// `CoreApi` over the `dkc_*` imports.
#[derive(Clone, Copy)]
pub struct ImportCore;

impl CoreApi for ImportCore {
    unsafe fn dynreg(&self) -> *const u32 {
        dkc_dynreg()
    }
    fn reg_state(&self) -> &'static mut RegState {
        unsafe { &mut *dkc_reg_state() }
    }
    fn param_first(&self) -> u32 {
        unsafe { dkc_param_first() }
    }
    fn param_owners(&self) -> &[u8] {
        unsafe {
            let mut n = 0u32;
            let p = dkc_param_owners(&mut n);
            if p.is_null() {
                &[]
            } else {
                core::slice::from_raw_parts(p, n as usize)
            }
        }
    }
    unsafe fn write_param_row(
        &self,
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
        dkc_write_param_row(inf, code, template, slot, min, max, def, name, short)
    }
    unsafe fn write_fx_param_row(
        &self,
        code: u32,
        template: u32,
        mirror: u32,
        min: u32,
        max: u32,
        def: u32,
        name: u32,
        short: u32,
    ) {
        dkc_write_fx_param_row(code, template, mirror, min, max, def, name, short)
    }
    unsafe fn write_send_param_row(
        &self,
        code: u32,
        class: u32,
        owner_of: u32,
        mirror: u32,
        name: u32,
        short: u32,
    ) {
        dkc_write_send_param_row(code, class, owner_of, mirror, name, short)
    }
    unsafe fn reset_param_row(&self, code: u32) {
        dkc_reset_param_row(code)
    }
    unsafe fn make_rep(&self, rep: *mut u8, src: *const u8, upper: bool) -> u32 {
        dkc_make_rep(rep, src, upper)
    }
    unsafe fn type_resolve(&self, key: u32) -> u32 {
        dkc_type_resolve(key)
    }
    fn type_map_used(&self) -> u32 {
        unsafe { dkc_type_map_used() }
    }
    unsafe fn guard_faults(&self, ty: u32) -> u32 {
        dkc_guard_faults(ty)
    }
    unsafe fn guard_disabled(&self, ty: u32) -> bool {
        dkc_guard_disabled(ty)
    }
    fn device_id(&self) -> u32 {
        unsafe { dkc_device_id() }
    }
    unsafe fn module_name(&self, owner: u8) -> *const u8 {
        dkc_module_name(owner)
    }
    unsafe fn settings_get(&self, key: &[u8], out: &mut [u8]) -> i32 {
        dkc_settings_get(
            key.as_ptr(),
            key.len() as u32,
            out.as_mut_ptr(),
            out.len() as u32,
        )
    }
    unsafe fn settings_put(&self, key: &[u8], value: &[u8]) -> i32 {
        dkc_settings_put(
            key.as_ptr(),
            key.len() as u32,
            value.as_ptr(),
            value.len() as u32,
        )
    }
    unsafe fn log(&self, kind: u16, flags: u16, args: [u32; 5]) {
        dkc_log(kind, flags, args.as_ptr())
    }
    fn current_source(&self) -> u8 {
        unsafe { dkc_current_source() }
    }
    unsafe fn job_post(
        &self,
        owner: u8,
        step: Option<JobStepFn>,
        done: Option<JobDoneFn>,
        ctx: *mut u8,
    ) -> i32 {
        dkc_job_post(owner, step, done, ctx)
    }
    unsafe fn job_cancel(&self, owner: u8, id: u32) -> i32 {
        dkc_job_cancel(owner, id)
    }
    fn native_available(&self, kind: u32) -> bool {
        unsafe { dkc_native_available(kind) }
    }
    unsafe fn native_register(
        &self,
        kind: u32,
        owner: u32,
        page: u32,
        flags: u32,
        title: *const u8,
        draw: u32,
        event: u32,
        bindings: *const u32,
        words: *const u32,
        word_count: u32,
        arg: u32,
        with_arg: u32,
    ) -> i32 {
        dkc_native_register(
            kind, owner, page, flags, title, draw, event, bindings, words, word_count, arg,
            with_arg,
        )
    }
    unsafe fn native_remove(&self, kind: u32) {
        dkc_native_remove(kind)
    }
    unsafe fn native_turn(&self, view: *const u8, event: *const u8) -> u32 {
        dkc_native_turn(view, event)
    }
    fn dsp(&self) -> &'static mut DspState {
        unsafe { &mut *dkc_dsp() }
    }
    unsafe fn dsp_info(&self) -> *const u32 {
        dkc_dsp_info()
    }
    unsafe fn fx_info(&self) -> *const u32 {
        dkc_fx_info()
    }
    unsafe fn info(&self, name_hash: u32) -> *const u32 {
        dkc_info(name_hash)
    }
    fn link_set(&self, slot: Slot, value: u32) {
        unsafe { dkc_link_set(&slot, value) }
    }
    fn link_confirmed(&self, slot: Slot) -> Option<u32> {
        let mut v = 0u32;
        if unsafe { dkc_link_confirmed(&slot, &mut v) } {
            Some(v)
        } else {
            None
        }
    }
    fn link_errors(&self) -> u32 {
        unsafe { dkc_link_errors() }
    }
    fn link_forget(&self, win: u32, first: u32, count: u32) {
        unsafe { dkc_link_forget(win, first, count) }
    }
    unsafe fn link_submit(&self, owner: Owner, stream: &Stream) -> Option<Ticket> {
        let mut t = core::mem::MaybeUninit::<Ticket>::uninit();
        if dkc_link_submit(owner, stream, t.as_mut_ptr()) {
            Some(t.assume_init())
        } else {
            None
        }
    }
    fn link_result(&self, t: Ticket) -> LinkResult {
        unsafe { dkc_link_result(&t) }
    }
    unsafe fn link_drain(&self, owner: Owner) -> bool {
        dkc_link_drain(owner)
    }
    unsafe fn link_run(&self, owner: Owner, seq: &Sequence) -> bool {
        dkc_link_run(owner, seq)
    }
}

/// The record `dk_module` of a service file: the four entry points of `dk_module_def` (<dk/core/module.h>); a missing one is a zero word.
#[repr(C)]
pub struct ModuleDef {
    pub init: Option<unsafe extern "C" fn(*mut u8) -> i32>,
    pub fini: Option<unsafe extern "C" fn(*mut u8)>,
    pub tick: Option<unsafe extern "C" fn(*mut u8)>,
    pub menu: Option<unsafe extern "C" fn(*mut u8, *mut u8)>,
}

// Read-only data.
unsafe impl Sync for ModuleDef {}

/// The `init` of `service!`: declare each record kind the row owns (its handler entry is `handler`, an address of the file, which the header table records as
/// an image offset) and then the row. The first refusal is the result.
///
/// # Safety
/// `ctx` is the module context `init` was called with; `row` stays valid as long as the file is loaded.
pub unsafe fn init(ctx: *mut u8, row: &'static ServiceRow, handler: *const u8) -> i32 {
    for &(kind, len) in row.kinds() {
        let r = dk_service_declare_kind(ctx, kind, len, handler);
        if r != 0 {
            return r;
        }
    }
    dk_service_declare_row(ctx, row)
}

/// The declarations of `service!` after `init` (kinds and row): one `dk_service_declare_hook` for each `(site hash, entry, device)` that is for `device` (0: every
/// device), one `dk_service_declare_slot` for each `(slot hash, entry, device)` of `fills` likewise, then one `dk_service_declare_export` for each `(name hash,
/// entry)`. The first refusal is the result. `device` is the `[digikit] id` of the running device.
///
/// # Safety
/// As `init`; each entry is the address of a function of the file.
pub unsafe fn declare(
    ctx: *mut u8,
    row: &'static ServiceRow,
    handler: *const u8,
    device: fn() -> u32,
    hooks: &[(u32, *const u8, u32)],
    fills: &[(u32, *const u8, u32)],
    exports: &[(u32, *const u8)],
) -> i32 {
    let r = init(ctx, row, handler);
    if r != 0 {
        return r;
    }
    // the device is asked for only when an entry has a filter to compare with
    let device = if hooks.is_empty() && fills.is_empty() {
        0
    } else {
        device()
    };
    for &(site, entry, only) in hooks {
        if only != 0 && only != device {
            continue;
        }
        let r = dk_service_declare_hook(ctx, site, entry);
        if r != 0 {
            return r;
        }
    }
    for &(slot, entry, only) in fills {
        if only != 0 && only != device {
            continue;
        }
        let r = dk_service_declare_slot(ctx, slot, 0, entry);
        if r != 0 {
            return r;
        }
    }
    for &(name, entry) in exports {
        let r = dk_service_declare_export(ctx, name, entry);
        if r != 0 {
            return r;
        }
    }
    0
}

/// The `[digikit] id` of the running device (`dkc_device_id`): what `service!` passes to `declare`.
#[doc(hidden)]
pub fn running_device() -> u32 {
    ImportCore.device_id()
}

/// The device filter of one hook entry of `service!`: none (every device), `dt2` or `dn2`.
#[doc(hidden)]
#[macro_export]
macro_rules! hook_device {
    () => {
        0
    };
    (dt2) => {
        $crate::DEVICE_DT2
    };
    (dn2) => {
        $crate::DEVICE_DN2
    };
}

/// The service file's glue: `service!(Lfo, "lfo", KINDS_LFO)` for `impl<C: CoreApi> Service<C> for Lfo`, the name of its `[[service]]` and the record kinds
/// it owns (`&'static [(u32, u32)]`, as `dkrt_sdk::records::KINDS_*`). It emits `dk_module`, `dk_svc_abi` and (on the ColdFire) the panic handler.
///
/// After the kinds: `setup = f`, `hooks = [...]`, `fills = [...]` and `exports = [("dk_param_get", fw::dk_param_get), ...]` (name, function: declared with the hash of
/// the name, as the `[[export]]` entries of `module.toml`), each optional list in this order. Every form also emits `dk_svc_declare(ctx, device)`: the declarations
/// of `init` without `setup` (the host test of the assembly runs it and compares the result with the header table).
///
/// The symbols are `no_mangle` on the ColdFire only: a host test links the service crates of all three files into one binary.
#[macro_export]
macro_rules! service {
    ($svc:ty, $name:literal, $kinds:expr) => {
        $crate::service!(@emit $svc, $name, $kinds, setup = None, hooks = [], fills = [], exports = []);
    };
    ($svc:ty, $name:literal, $kinds:expr, setup = $setup:expr, hooks = [$($hook:tt)*]) => {
        $crate::service!(@emit $svc, $name, $kinds, setup = Some($setup), hooks = [$($hook)*], fills = [], exports = []);
    };
    ($svc:ty, $name:literal, $kinds:expr, setup = $setup:expr, hooks = [$($hook:tt)*], fills = [$($fill:tt)*]) => {
        $crate::service!(@emit $svc, $name, $kinds, setup = Some($setup), hooks = [$($hook)*], fills = [$($fill)*], exports = []);
    };
    ($svc:ty, $name:literal, $kinds:expr, setup = $setup:expr, hooks = [$($hook:tt)*], fills = [$($fill:tt)*], exports = [$($export:tt)*]) => {
        $crate::service!(@emit $svc, $name, $kinds, setup = Some($setup), hooks = [$($hook)*], fills = [$($fill)*], exports = [$($export)*]);
    };
    (@emit $svc:ty, $name:literal, $kinds:expr, setup = $setup:expr,
     hooks = [$(($site:literal, $entry:path $(, $dev:ident)?)),* $(,)?],
     fills = [$(($slot:literal, $fill:path $(, $fdev:ident)?)),* $(,)?],
     exports = [$(($ename:literal, $efn:path)),* $(,)?]) => {
        static DK_SVC_ROW: $crate::dkrt_abi::service::ServiceRow =
            $crate::dkrt_abi::service::ServiceRow::of::<$svc, $crate::ImportCore>($name, $kinds);

        /// The declarations of `init` (without `setup`); `device` gives the `[digikit] id` of the device the entries with a filter are compared with.
        ///
        /// # Safety
        /// `ctx` is the module context of a collection; the entries are addresses of the file.
        pub unsafe fn dk_svc_declare(ctx: *mut u8, device: fn() -> u32) -> i32 {
            $crate::declare(ctx, &DK_SVC_ROW, dk_svc_init as usize as *const u8, device, &$crate::service!(@hooks $(($site, $entry $(, $dev)?)),*), &$crate::service!(@hooks $(($slot, $fill $(, $fdev)?)),*), &$crate::service!(@exports $(($ename, $efn)),*))
        }

        unsafe extern "C" fn dk_svc_init(ctx: *mut u8) -> i32 {
            let setup: Option<unsafe fn(&$crate::ImportCore) -> i32> = $setup;
            let r = match setup {
                Some(f) => f(&$crate::ImportCore),
                None => 0,
            };
            if r != 0 {
                return r;
            }
            dk_svc_declare(ctx, $crate::running_device)
        }

        #[cfg_attr(target_arch = "m68k", no_mangle)]
        pub static dk_module: $crate::ModuleDef = $crate::ModuleDef {
            init: Some(dk_svc_init),
            fini: None,
            tick: None,
            menu: None,
        };

        /// The ABI fingerprint the packer copies to header word 0x3c (`dkrt_abi::service::SVC_ABI`).
        #[cfg_attr(target_arch = "m68k", no_mangle)]
        pub static dk_svc_abi: u32 = $crate::dkrt_abi::service::SVC_ABI;

        #[cfg(target_arch = "m68k")]
        #[panic_handler]
        fn dk_svc_panic(_: &core::panic::PanicInfo) -> ! {
            unsafe { $crate::sys::dkc_panic() }
        }
    };
    (@hooks $(($site:literal, $entry:path $(, $dev:ident)?)),*) => {
        [$((
            $crate::name_hash($site),
            $entry as usize as *const u8,
            $crate::hook_device!($($dev)?),
        )),*]
    };
    (@exports $(($ename:literal, $efn:path)),*) => {
        [$(($crate::name_hash($ename), $efn as usize as *const u8)),*]
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_record_is_four_words_and_a_missing_entry_is_zero() {
        // the packer reads `dk_module` as four words (init, fini, tick, menu) and takes a zero word for no entry
        assert_eq!(
            core::mem::size_of::<ModuleDef>(),
            4 * core::mem::size_of::<usize>()
        );
        let none = ModuleDef {
            init: None,
            fini: None,
            tick: None,
            menu: None,
        };
        let words: [usize; 4] = unsafe { core::mem::transmute(none) };
        assert_eq!(words, [0; 4]);
    }
}
