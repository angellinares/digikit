//! The second import table, `SVC_IMPORTS`: what a service file (`.dks`, role 1) imports besides `IMPORTS`. It is interface C as C-shaped functions
//! (`dkrt_abi::core_imports`: one `dkc_*` per `CoreApi` method) and the two imports only a service file makes (`dk_service_declare_row`, `dkc_panic`). The
//! list is one macro in `dkrt-abi`; `dkrt-svc-rt` declares the same names and implements `CoreApi` over them (`ImportCore`).
//!
//! `shims` has one function of the declared type for each name; a const check per entry fails the build when a type differs from the list. A shim calls the
//! `RealCore` method (or the runtime function) and converts the C shape back. The loader looks the table up for role 1 only, after `IMPORTS`.
//! In host tests the shims are exported under their names (`no_mangle`), so a test can link `ImportCore` to them.
use crate::core_api::{CoreApi, REAL_CORE};
use crate::imports::{nm, Import};
use crate::platform::cstr_eq;

/// The shims, one per entry of `dkrt_abi::core_imports!`.
pub mod shims {
    use super::*;
    use dkrt_abi::dsp::DspState;
    use dkrt_abi::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
    use dkrt_abi::registry::RegState;
    use dkrt_abi::service::ServiceRow;
    use dkrt_sdk::api::{JobDoneFn, JobStepFn};

    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_dynreg() -> *const u32 {
        REAL_CORE.dynreg()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_reg_state() -> *mut RegState {
        REAL_CORE.reg_state()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_param_first() -> u32 {
        REAL_CORE.param_first()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_param_owners(len: *mut u32) -> *const u8 {
        let o = REAL_CORE.param_owners();
        *len = o.len() as u32;
        o.as_ptr()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_write_param_row(inf: *const u32, code: u32, template: u32, slot: u32, min: u32, max: u32, def: u32, name: u32, short: u32) {
        REAL_CORE.write_param_row(inf, code, template, slot, min, max, def, name, short)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_write_fx_param_row(code: u32, template: u32, mirror: u32, min: u32, max: u32, def: u32, name: u32, short: u32) {
        REAL_CORE.write_fx_param_row(code, template, mirror, min, max, def, name, short)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_write_send_param_row(code: u32, class: u32, owner_of: u32, mirror: u32, name: u32, short: u32) {
        REAL_CORE.write_send_param_row(code, class, owner_of, mirror, name, short)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_reset_param_row(code: u32) {
        REAL_CORE.reset_param_row(code)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_make_rep(rep: *mut u8, src: *const u8, upper: bool) -> u32 {
        REAL_CORE.make_rep(rep, src, upper)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_type_resolve(key: u32) -> u32 {
        REAL_CORE.type_resolve(key)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_type_map_used() -> u32 {
        REAL_CORE.type_map_used()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_guard_faults(ty: u32) -> u32 {
        REAL_CORE.guard_faults(ty)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_guard_disabled(ty: u32) -> bool {
        REAL_CORE.guard_disabled(ty)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_device_id() -> u32 {
        REAL_CORE.device_id()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_module_name(owner: u8) -> *const u8 {
        REAL_CORE.module_name(owner)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_settings_get(key: *const u8, key_len: u32, out: *mut u8, out_len: u32) -> i32 {
        REAL_CORE.settings_get(core::slice::from_raw_parts(key, key_len as usize), core::slice::from_raw_parts_mut(out, out_len as usize))
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_settings_put(key: *const u8, key_len: u32, value: *const u8, value_len: u32) -> i32 {
        REAL_CORE.settings_put(core::slice::from_raw_parts(key, key_len as usize), core::slice::from_raw_parts(value, value_len as usize))
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_log(kind: u16, flags: u16, args: *const u32) {
        let a = core::slice::from_raw_parts(args, 5);
        REAL_CORE.log(kind, flags, [a[0], a[1], a[2], a[3], a[4]])
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_current_source() -> u8 {
        REAL_CORE.current_source()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_job_post(owner: u8, step: Option<JobStepFn>, done: Option<JobDoneFn>, ctx: *mut u8) -> i32 {
        REAL_CORE.job_post(owner, step, done, ctx)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_job_cancel(owner: u8, id: u32) -> i32 {
        REAL_CORE.job_cancel(owner, id)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_native_available(kind: u32) -> bool {
        REAL_CORE.native_available(kind)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_native_register(
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
        REAL_CORE.native_register(kind, owner, page, flags, title, draw, event, bindings, words, word_count, arg, with_arg)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_native_remove(kind: u32) {
        REAL_CORE.native_remove(kind)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_native_turn(view: *const u8, event: *const u8) -> u32 {
        REAL_CORE.native_turn(view, event)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_dsp() -> *mut DspState {
        REAL_CORE.dsp()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_dsp_info() -> *const u32 {
        REAL_CORE.dsp_info()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_fx_info() -> *const u32 {
        REAL_CORE.fx_info()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_set(slot: *const Slot, value: u32) {
        REAL_CORE.link_set(*slot, value)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_confirmed(slot: *const Slot, out: *mut u32) -> bool {
        match REAL_CORE.link_confirmed(*slot) {
            Some(v) => {
                *out = v;
                true
            }
            None => false,
        }
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_errors() -> u32 {
        REAL_CORE.link_errors()
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_forget(win: u32, first: u32, count: u32) {
        REAL_CORE.link_forget(win, first, count)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_submit(owner: Owner, stream: *const Stream, out: *mut Ticket) -> bool {
        match REAL_CORE.link_submit(owner, &*stream) {
            Some(t) => {
                *out = t;
                true
            }
            None => false,
        }
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_result(ticket: *const Ticket) -> LinkResult {
        REAL_CORE.link_result(*ticket)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_drain(owner: Owner) -> bool {
        REAL_CORE.link_drain(owner)
    }
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_link_run(owner: Owner, seq: *const Sequence) -> bool {
        REAL_CORE.link_run(owner, &*seq)
    }

    /// The row a service file declares in `init` (the declarations of the collection).
    pub use crate::collect::dk_service_declare_row;

    /// A service panicked: the runtime's panic (crash capture, safe mode on the next boot).
    #[cfg_attr(test, no_mangle)]
    pub unsafe extern "C" fn dkc_panic() -> ! {
        crate::fault::panic()
    }
}

/// The table: `SVC_COUNT` entries, in the order of the list.
pub struct SvcImports(pub [Import; SVC_COUNT]);
unsafe impl Sync for SvcImports {}

macro_rules! svc_table {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        /// The number of imports a service file may add to `IMPORTS`.
        pub const SVC_COUNT: usize = [$(stringify!($name)),*].len();
        // each shim has the type the list declares
        $(const _: unsafe extern "C" fn($($ty),*) $(-> $ret)? = shims::$name;)*
        /// The imports of a service file (role 1) after `IMPORTS`: names and addresses from one list, so they cannot drift apart.
        pub static SVC_IMPORTS: SvcImports = SvcImports([$(Import { name: nm(stringify!($name).as_bytes()), addr: shims::$name as *const () }),*]);
    };
}
dkrt_abi::core_imports!(svc_table);

/// Address of the service import `name` (NUL-terminated), 0 when the table has none.
pub unsafe fn svc_import_addr(name: *const u8) -> u32 {
    let mut i = 0;
    while i < SVC_IMPORTS.0.len() {
        if cstr_eq(SVC_IMPORTS.0[i].name.as_ptr(), name) {
            return SVC_IMPORTS.0[i].addr as usize as u32;
        }
        i += 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_every_name_once_and_none_of_the_module_imports() {
        let mut names: std::vec::Vec<&[u8]> = SVC_IMPORTS.0.iter().map(|i| &i.name[..i.name.iter().position(|&b| b == 0).unwrap()]).collect();
        assert_eq!(names.len(), SVC_COUNT);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SVC_COUNT, "a name twice");
        for n in dkrt_sdk::api::BUILTIN_NAMES.iter() {
            assert!(names.binary_search(&n.as_bytes()).is_err(), "{n} is in both tables");
        }
        assert!(names.contains(&&b"dk_service_declare_row"[..]) && names.contains(&&b"dkc_panic"[..]));
        // one `dkc_*` per CoreApi method, and the two of a service file
        assert!(SVC_COUNT >= 38, "{SVC_COUNT}");
    }

    #[test]
    fn a_name_resolves_to_its_shim_and_an_unknown_name_to_zero() {
        unsafe {
            assert_eq!(svc_import_addr(b"dkc_device_id\0".as_ptr()), shims::dkc_device_id as usize as u32);
            assert_eq!(svc_import_addr(b"dkc_nothing\0".as_ptr()), 0);
            assert_eq!(svc_import_addr(b"dk_alloc\0".as_ptr()), 0, "the module imports are the other table's");
        }
    }
}
