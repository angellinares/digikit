//! Interface C as the imports of a service file (docs/plans/runtime-and-services.md section 13.2): one `extern "C" fn dkc_*` per `CoreApi` method, in C shape, and
//! the two imports only a service file makes (`dk_service_declare_row`, `dkc_panic`). The list is one macro; two crates expand it:
//!
//! - `dkrt-svc-rt` declares the names in an `extern "C"` block and implements `CoreApi` over them (`ImportCore`);
//! - the runtime checks one function of its own for each entry against the declared type and builds its second import table `SVC_IMPORTS` from the names.
//!
//! The shape rules: a `&'static mut T` is a `*mut T`, a slice is a pointer and a length, an `Option` result is a `bool` and an out pointer, a struct that is
//! passed or returned by reference is a `*const` or `*mut` of its `repr(C)` type, an array of words is a pointer. A result that is a `bool` stays a `bool`
//! (`extern "C"` and Rust on both sides).
//!
//! This file is one of the boundary files `patchkit gen` hashes into `SVC_ABI`: any change here changes the fingerprint.
pub use dkrt_sdk::api::{JobDoneFn, JobStepFn};

/// Invoke `$m! { fn name(arg: Ty, ...) -> Ret; ... }` with the imports of a service file: the `dkc_*` of `CoreApi`, then `dk_service_declare_row` and `dkc_panic`.
#[macro_export]
macro_rules! core_imports {
    ($m:ident) => {
        $m! {
            // ---- the registry's rows
            fn dkc_dynreg() -> *const u32;
            fn dkc_reg_state() -> *mut $crate::registry::RegState;
            fn dkc_param_first() -> u32;
            // The owner of each run-time parameter row: `*len` is set to the count.
            fn dkc_param_owners(len: *mut u32) -> *const u8;
            fn dkc_write_param_row(inf: *const u32, code: u32, template: u32, slot: u32, min: u32, max: u32, def: u32, name: u32, short: u32);
            fn dkc_write_fx_param_row(code: u32, template: u32, mirror: u32, min: u32, max: u32, def: u32, name: u32, short: u32);
            fn dkc_write_send_param_row(code: u32, class: u32, owner_of: u32, mirror: u32, name: u32, short: u32);
            fn dkc_reset_param_row(code: u32);
            fn dkc_make_rep(rep: *mut u8, src: *const u8, upper: bool) -> u32;
            fn dkc_type_resolve(key: u32) -> u32;
            fn dkc_type_map_used() -> u32;
            fn dkc_guard_faults(ty: u32) -> u32;
            fn dkc_guard_disabled(ty: u32) -> bool;
            // ---- the device
            fn dkc_device_id() -> u32;
            // ---- the module table
            fn dkc_module_name(owner: u8) -> *const u8;
            // ---- settings
            fn dkc_settings_get(key: *const u8, key_len: u32, out: *mut u8, out_len: u32) -> i32;
            fn dkc_settings_put(key: *const u8, key_len: u32, value: *const u8, value_len: u32) -> i32;
            // ---- log and source: `args` points at five words
            fn dkc_log(kind: u16, flags: u16, args: *const u32);
            fn dkc_current_source() -> u8;
            // ---- jobs
            fn dkc_job_post(owner: u8, step: Option<$crate::core_imports::JobStepFn>, done: Option<$crate::core_imports::JobDoneFn>, ctx: *mut u8) -> i32;
            fn dkc_job_cancel(owner: u8, id: u32) -> i32;
            // ---- the native page adapter: `bindings` points at 24 words (eight triples)
            fn dkc_native_available(kind: u32) -> bool;
            fn dkc_native_register(
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
            ) -> i32;
            fn dkc_native_remove(kind: u32);
            fn dkc_native_turn(view: *const u8, event: *const u8) -> u32;
            // ---- the DSP loader's state and tables
            fn dkc_dsp() -> *mut $crate::dsp::DspState;
            fn dkc_dsp_info() -> *const u32;
            fn dkc_fx_info() -> *const u32;
            // An entry of the image's info table by the `name_hash` of its name (`CoreApi::info`): null when the image has none.
            fn dkc_info(name_hash: u32) -> *const u32;
            // ---- the DSP link
            fn dkc_link_set(slot: *const $crate::link::Slot, value: u32);
            // `true`: `*out` is the confirmed value.
            fn dkc_link_confirmed(slot: *const $crate::link::Slot, out: *mut u32) -> bool;
            fn dkc_link_errors() -> u32;
            fn dkc_link_forget(win: u32, first: u32, count: u32);
            // `true`: `*out` is the ticket.
            fn dkc_link_submit(owner: $crate::link::Owner, stream: *const $crate::link::Stream, out: *mut $crate::link::Ticket) -> bool;
            fn dkc_link_result(ticket: *const $crate::link::Ticket) -> $crate::link::LinkResult;
            fn dkc_link_drain(owner: $crate::link::Owner) -> bool;
            fn dkc_link_run(owner: $crate::link::Owner, seq: *const $crate::link::Sequence) -> bool;
            // ---- only a service file: its row, and a panic
            // Called in `init`: the `ServiceRow` this file provides (`ctx` is the module context). A status.
            fn dk_service_declare_row(ctx: *mut u8, row: *const $crate::service::ServiceRow) -> i32;
            // A service panicked: the runtime's panic (crash capture, safe mode on the next boot). Never returns.
            fn dkc_panic() -> !;
        }
    };
}
