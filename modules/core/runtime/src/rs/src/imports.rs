//! The import table of the modules (`IMPORTS`, one list of names and addresses checked against `sdk/sdk.toml`), the imports of other
//! modules (`optional`), the lookups `import_addr` and `import_needs`, and the imports that are plain functions.

use crate::platform::cstr_eq;
use crate::state::log_at;
use crate::{arena, aux, collect, dlog, entry, hash32, intro, io, worker};
use crate::debug_selected_params;
use crate::health::dkrt_blocks;
use crate::report::{debug_peek, debug_poll, debug_span_clock, debug_speed_flags, debug_track_type};

extern "C" {
    // Helpers from compiler_builtins, linked into this object.
    fn memcpy(d: *mut u8, s: *const u8, n: usize) -> *mut u8;
    fn memset(d: *mut u8, c: i32, n: usize) -> *mut u8;
    fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32;
    fn __muldi3();
    fn __udivdi3();
    fn __umoddi3();
}

// ---- imports -------------------------------------------------------------------------------------------------------

#[repr(C)]
pub struct Import {
    pub name: [u8; 32],
    pub addr: *const (),
}
pub struct Imports(pub [Import; dkrt_sdk::api::BUILTIN_NAMES.len()]);
unsafe impl Sync for Imports {}

const fn nm(s: &[u8]) -> [u8; 32] {
    let mut o = [0u8; 32];
    let mut i = 0;
    while i < s.len() {
        o[i] = s[i];
        i += 1;
    }
    o
}

const fn names_match(ours: &[&[u8]], sdk: &[&str]) -> bool {
    if ours.len() != sdk.len() {
        return false;
    }
    let mut i = 0;
    while i < ours.len() {
        let (a, b) = (ours[i], sdk[i].as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut j = 0;
        while j < a.len() {
            if a[j] != b[j] {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// `NAMES` (the names of `IMPORTS`, in its order) and `IMPORTS` from one list, so a name and its address cannot drift apart; the names are checked against
/// the `[[import]]` entries of `sdk/sdk.toml` without `requires`.
macro_rules! import_table {
    ($(($name:literal, $addr:expr)),* $(,)?) => {
        const NAMES: [&[u8]; dkrt_sdk::api::BUILTIN_NAMES.len()] = [$($name.as_bytes()),*];
        /// The names a module may import. Firmware functions come from patch-time anchors (`dkfw`); the helpers are this object's own copies, so a module
        /// need not carry them. The `dk_*` services are the runtime's own functions (`entry.rs` for the ones that take a handle).
        pub static IMPORTS: Imports = Imports([$(Import { name: nm($name.as_bytes()), addr: $addr as *const () }),*]);
    };
}

import_table! {
    ("memcpy", memcpy),
    ("memset", memset),
    ("memcmp", memcmp),
    ("__muldi3", __muldi3),
    ("__udivdi3", __udivdi3),
    ("__umoddi3", __umoddi3),
    // the service imports (docs/plans/sdk.md section 5.1)
    ("dk_alloc", imp_alloc),
    ("dk_free", imp_free),
    ("dk_log", imp_log),
    ("dk_log_event", dlog::dk_log_event),
    ("dk_sink_set", dlog::dk_sink_set),
    ("dk_span_sink_set", dlog::dk_span_sink_set),
    ("dk_time_blocks", dkrt_blocks),
    ("dk_fw_alloc", crate::platform::fw_alloc),
    ("dk_fw_free", crate::platform::fw_free),
    ("dk_fw_block_read", crate::platform::fw_block_read),
    ("dk_files_read", entry::dk_files_read),
    ("dk_host_register", imp_host_register),
    ("dk_module_ctx_device", entry::dk_module_ctx_device),
    ("dk_module_ctx_runtime_version", entry::dk_module_ctx_runtime_version),
    ("dk_settings_open", entry::dk_settings_open),
    ("dk_settings_get", entry::dk_settings_get),
    ("dk_settings_put", entry::dk_settings_put),
    ("dk_settings_remove", entry::dk_settings_remove),
    ("dk_files_open", entry::dk_files_open),
    ("dk_files_list", entry::dk_files_list),
    ("dk_files_poll", entry::dk_files_poll),
    ("dk_files_cancel", entry::dk_files_cancel),
    ("dk_files_create", entry::dk_files_create),
    ("dk_files_write", entry::dk_files_write),
    ("dk_files_close", entry::dk_files_close),
    ("dk_files_abort", entry::dk_files_abort),
    ("dk_files_delete", entry::dk_files_delete),
    ("dk_files_commit", entry::dk_files_commit),
    ("dk_files_pending", entry::dk_files_pending),
    ("dk_io_post", imp_io_post),
    ("dk_io_cancel", imp_io_cancel),
    ("dk_io_wake", imp_io_wake),
    ("dk_helper_run", imp_helper_run),
    ("dk_helper_wait", imp_helper_wait),
    ("dk_job_post", imp_job_post),
    ("dk_job_cancel", imp_job_cancel),
    ("dk_hash32_init", imp_hash32_init),
    ("dk_hash32_update", imp_hash32_update),
    ("dk_hash32_finish", imp_hash32_finish),
    ("dk_intro_take", imp_intro_take),
    ("dk_module_ctx_add_machine", collect::dk_module_ctx_add_machine),
    ("dk_module_ctx_add_fx", collect::dk_module_ctx_add_fx),
    ("dk_record_add", collect::dk_record_add),
    ("dk_service_declare_kind", collect::dk_service_declare_kind),
    ("dk_service_declare_slot", collect::dk_service_declare_slot),
    ("dk_service_declare_hook", collect::dk_service_declare_hook),
    ("dk_service_declare_export", collect::dk_service_declare_export),
    ("dk_service_declare_resource", collect::dk_service_declare_resource),
    ("dk_machine_add_page", collect::dk_machine_add_page),
    ("dk_fx_add_page", collect::dk_fx_add_page),
    ("dk_page_add_param", collect::dk_page_add_param),
    ("dk_page_add_fx_param", collect::dk_page_add_fx_param),
    ("dk_param_range", collect::dk_param_range),
    ("dk_param_glyph", collect::dk_param_glyph),
    ("dk_param_latched", collect::dk_param_latched),
    ("dk_param_on_click", collect::dk_param_on_click),
    ("dk_page_canvas", collect::dk_page_canvas),
    ("dk_page_bind", collect::dk_page_bind),
    ("dk_page_bind_local", collect::dk_page_bind_local),
    ("dk_fx_routes", collect::dk_fx_routes),
    ("dk_fx_mix", collect::dk_fx_mix),
    ("dk_sample_pool_open", crate::handlers::exports::dk_sample_pool_open),
    ("dk_sample_pool_add", crate::handlers::exports::dk_sample_pool_add),
    ("dk_sample_pool_clear", crate::handlers::exports::dk_sample_pool_clear),
    ("dk_sample_pool_status", crate::handlers::exports::dk_sample_pool_status),
    ("dk_sample_pool_info", crate::handlers::exports::dk_sample_pool_info),
    ("dk_sample_pool_path", crate::handlers::exports::dk_sample_pool_path),
    ("dk_sample_pool_set", crate::handlers::exports::dk_sample_pool_set),
    ("dk_sample_pool_peaks", crate::handlers::exports::dk_sample_pool_peaks),
    ("dk_sample_pool_peaks_window", crate::handlers::exports::dk_sample_pool_peaks_window),
    ("dk_debug_peek", debug_peek),
    ("dk_debug_span_clock", debug_span_clock),
    ("dk_debug_poll", debug_poll),
    ("dk_debug_track_type", debug_track_type),
    ("dk_debug_selected_params", debug_selected_params),
    ("dk_debug_log_sink_set", dlog::dk_debug_log_sink_set),
    ("dk_debug_speed_flags", debug_speed_flags),
}

const _: () = assert!(names_match(&NAMES, &dkrt_sdk::api::BUILTIN_NAMES), "IMPORTS names differ from [[import]] of sdk/sdk.toml");
// the service imports have the types sdk/sdk.toml gives them
const _: dkrt_sdk::api::DkAllocFn = imp_alloc;
const _: dkrt_sdk::api::DkFreeFn = imp_free;
const _: dkrt_sdk::api::DkLogFn = imp_log;
const _: dkrt_sdk::api::DkLogEventFn = dlog::dk_log_event;
const _: dkrt_sdk::api::DkSinkSetFn = dlog::dk_sink_set;
const _: dkrt_sdk::api::DkSpanSinkSetFn = dlog::dk_span_sink_set;
const _: dkrt_sdk::api::DkTimeBlocksFn = dkrt_blocks;
const _: dkrt_sdk::api::DkFwAllocFn = crate::platform::fw_alloc;
const _: dkrt_sdk::api::DkFwFreeFn = crate::platform::fw_free;
const _: dkrt_sdk::api::DkFwBlockReadFn = crate::platform::fw_block_read;
const _: dkrt_sdk::api::DkFilesReadFn = entry::dk_files_read;
const _: dkrt_sdk::api::DkHostRegisterFn = imp_host_register;
const _: dkrt_sdk::api::DkModuleCtxDeviceFn = entry::dk_module_ctx_device;
const _: dkrt_sdk::api::DkModuleCtxRuntimeVersionFn = entry::dk_module_ctx_runtime_version;
const _: dkrt_sdk::api::DkSettingsOpenFn = entry::dk_settings_open;
const _: dkrt_sdk::api::DkSettingsGetFn = entry::dk_settings_get;
const _: dkrt_sdk::api::DkSettingsPutFn = entry::dk_settings_put;
const _: dkrt_sdk::api::DkSettingsRemoveFn = entry::dk_settings_remove;
const _: dkrt_sdk::api::DkFilesOpenFn = entry::dk_files_open;
const _: dkrt_sdk::api::DkFilesListFn = entry::dk_files_list;
const _: dkrt_sdk::api::DkFilesPollFn = entry::dk_files_poll;
const _: dkrt_sdk::api::DkFilesCancelFn = entry::dk_files_cancel;
const _: dkrt_sdk::api::DkFilesCreateFn = entry::dk_files_create;
const _: dkrt_sdk::api::DkFilesWriteFn = entry::dk_files_write;
const _: dkrt_sdk::api::DkFilesCloseFn = entry::dk_files_close;
const _: dkrt_sdk::api::DkFilesAbortFn = entry::dk_files_abort;
const _: dkrt_sdk::api::DkFilesDeleteFn = entry::dk_files_delete;
const _: dkrt_sdk::api::DkFilesCommitFn = entry::dk_files_commit;
const _: dkrt_sdk::api::DkFilesPendingFn = entry::dk_files_pending;
const _: dkrt_sdk::api::DkIoPostFn = imp_io_post;
const _: dkrt_sdk::api::DkIoCancelFn = imp_io_cancel;
const _: dkrt_sdk::api::DkIoWakeFn = imp_io_wake;
const _: dkrt_sdk::api::DkHelperRunFn = imp_helper_run;
const _: dkrt_sdk::api::DkHelperWaitFn = imp_helper_wait;
const _: dkrt_sdk::api::DkJobPostFn = imp_job_post;
const _: dkrt_sdk::api::DkJobCancelFn = imp_job_cancel;
const _: dkrt_sdk::api::DkHash32InitFn = imp_hash32_init;
const _: dkrt_sdk::api::DkHash32UpdateFn = imp_hash32_update;
const _: dkrt_sdk::api::DkHash32FinishFn = imp_hash32_finish;
const _: dkrt_sdk::api::DkIntroTakeFn = imp_intro_take;
const _: dkrt_sdk::api::DkModuleCtxAddMachineFn = collect::dk_module_ctx_add_machine;
const _: dkrt_sdk::api::DkModuleCtxAddFxFn = collect::dk_module_ctx_add_fx;
const _: dkrt_sdk::api::DkMachineAddPageFn = collect::dk_machine_add_page;
const _: dkrt_sdk::api::DkFxAddPageFn = collect::dk_fx_add_page;
const _: dkrt_sdk::api::DkPageAddParamFn = collect::dk_page_add_param;
const _: dkrt_sdk::api::DkPageAddFxParamFn = collect::dk_page_add_fx_param;
const _: dkrt_sdk::api::DkParamRangeFn = collect::dk_param_range;
const _: dkrt_sdk::api::DkParamGlyphFn = collect::dk_param_glyph;
const _: dkrt_sdk::api::DkParamLatchedFn = collect::dk_param_latched;
const _: dkrt_sdk::api::DkParamOnClickFn = collect::dk_param_on_click;
const _: dkrt_sdk::api::DkPageCanvasFn = collect::dk_page_canvas;
const _: dkrt_sdk::api::DkPageBindFn = collect::dk_page_bind;
const _: dkrt_sdk::api::DkPageBindLocalFn = collect::dk_page_bind_local;
const _: dkrt_sdk::api::DkFxRoutesFn = collect::dk_fx_routes;
const _: dkrt_sdk::api::DkRecordAddFn = collect::dk_record_add;
const _: dkrt_sdk::api::DkServiceDeclareKindFn = collect::dk_service_declare_kind;
const _: dkrt_sdk::api::DkServiceDeclareSlotFn = collect::dk_service_declare_slot;
const _: dkrt_sdk::api::DkServiceDeclareHookFn = collect::dk_service_declare_hook;
const _: dkrt_sdk::api::DkServiceDeclareExportFn = collect::dk_service_declare_export;
const _: dkrt_sdk::api::DkServiceDeclareResourceFn = collect::dk_service_declare_resource;
const _: dkrt_sdk::api::DkFxMixFn = collect::dk_fx_mix;
const _: dkrt_sdk::api::DkSamplePoolOpenFn = crate::handlers::exports::dk_sample_pool_open;
const _: dkrt_sdk::api::DkSamplePoolAddFn = crate::handlers::exports::dk_sample_pool_add;
const _: dkrt_sdk::api::DkSamplePoolClearFn = crate::handlers::exports::dk_sample_pool_clear;
const _: dkrt_sdk::api::DkSamplePoolStatusFn = crate::handlers::exports::dk_sample_pool_status;
const _: dkrt_sdk::api::DkSamplePoolInfoFn = crate::handlers::exports::dk_sample_pool_info;
const _: dkrt_sdk::api::DkSamplePoolPathFn = crate::handlers::exports::dk_sample_pool_path;
const _: dkrt_sdk::api::DkSamplePoolSetFn = crate::handlers::exports::dk_sample_pool_set;
const _: dkrt_sdk::api::DkSamplePoolPeaksFn = crate::handlers::exports::dk_sample_pool_peaks;
const _: dkrt_sdk::api::DkSamplePoolPeaksWindowFn = crate::handlers::exports::dk_sample_pool_peaks_window;
const _: dkrt_sdk::api::DkDebugPeekFn = debug_peek;
const _: dkrt_sdk::api::DkDebugSpanClockFn = debug_span_clock;
const _: dkrt_sdk::api::DkDebugPollFn = debug_poll;
const _: dkrt_sdk::api::DkDebugTrackTypeFn = debug_track_type;
const _: dkrt_sdk::api::DkDebugSelectedParamsFn = debug_selected_params;
const _: dkrt_sdk::api::DkDebugLogSinkSetFn = dlog::dk_debug_log_sink_set;
const _: dkrt_sdk::api::DkDebugSpeedFlagsFn = debug_speed_flags;



// ---- imports of other modules --------------------------------------------------------------------------------------------

// The imports another module provides (`[[import]] requires` of sdk/sdk.toml: core/input, core/ui). The symbols are in this object's `optional`
// list (module.toml): patchkit links one to 0 when its module is not in the image.
dkrt_sdk::optional_imports!();

/// Address of the import `name` (NUL-terminated), 0 when the table has none. An import of another module (`optional`) gives that module's
/// function; when the module is not in the image it gives the stub of the import if sdk/sdk.toml declares `absent`, else 0.
pub unsafe fn import_addr(name: *const u8) -> u32 {
    let mut i = 0;
    while i < IMPORTS.0.len() {
        if cstr_eq(IMPORTS.0[i].name.as_ptr(), name) {
            return IMPORTS.0[i].addr as usize as u32;
        }
        i += 1;
    }
    let t = optional::table();
    i = 0;
    while i < t.len() {
        if cstr_eq_str(name, t[i].0) {
            return (if t[i].1 != 0 { t[i].1 } else { t[i].2 }) as u32;
        }
        i += 1;
    }
    0
}

/// The module whose absence leaves the import `name` (NUL-terminated) without an address, without the `core/` prefix ("usb-bulk" for an import of core/usb-bulk
/// that has no stub); None when the table has no such import, or it has an address or a stub. Every import of another module has a stub today.
pub unsafe fn import_needs(name: *const u8) -> Option<&'static str> {
    let t = optional::table();
    let mut i = 0;
    while i < t.len() {
        if cstr_eq_str(name, t[i].0) {
            if t[i].1 != 0 || t[i].2 != 0 {
                return None;
            }
            let r = t[i].3;
            return Some(match r.rfind('/') {
                Some(at) => &r[at + 1..],
                None => r,
            });
        }
        i += 1;
    }
    None
}

/// Whether the NUL-terminated `a` equals `b`.
unsafe fn cstr_eq_str(a: *const u8, b: &str) -> bool {
    let b = b.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if *a.add(i) != b[i] {
            return false;
        }
        i += 1;
    }
    *a.add(b.len()) == 0
}



// ---- the imports that are plain functions ----------------------------------------------------------------------------------

unsafe extern "C" fn imp_alloc(size: u32) -> *mut u8 {
    arena::alloc(size)
}
unsafe extern "C" fn imp_free(p: *mut u8) {
    arena::free(p);
}
unsafe extern "C" fn imp_log(level: u32, msg: *const u8, value: u32) {
    log_at(level, msg, value);
}
unsafe extern "C" fn imp_host_register(first: u32, count: u32, handler: Option<dkrt_sdk::HostCmdFn>) -> i32 {
    crate::module_cmds::host_register(first, count, handler)
}

unsafe extern "C" fn imp_job_post(step: Option<dkrt_sdk::api::JobStepFn>, done: Option<dkrt_sdk::api::JobDoneFn>, arg: *mut u8) -> i32 { worker::post(dlog::current_source(), step, done, arg) }
unsafe extern "C" fn imp_job_cancel(id: u32) -> i32 { worker::cancel(dlog::current_source(), id) }
unsafe extern "C" fn imp_intro_take(frame: Option<dkrt_sdk::api::IntroFrameFn>, done: Option<dkrt_sdk::api::IntroDoneFn>, arg: *mut u8, ticks: u32) -> i32 { intro::take(frame, done, arg, ticks) }
unsafe extern "C" fn imp_io_post(step: Option<dkrt_sdk::api::IoStepFn>, done: Option<dkrt_sdk::api::IoDoneFn>, arg: *mut u8) -> i32 { io::post(dlog::current_source(), step, done, arg) }
unsafe extern "C" fn imp_io_cancel(id: i32) -> i32 { io::cancel(dlog::current_source(), id) }
unsafe extern "C" fn imp_io_wake(id: i32) { io::wake(id) }
unsafe extern "C" fn imp_helper_run(work: Option<dkrt_sdk::api::HelperFn>, arg: *mut u8) -> i32 { aux::module_run(core::mem::transmute::<Option<dkrt_sdk::api::HelperFn>, usize>(work), arg) }
unsafe extern "C" fn imp_helper_wait() -> i32 { aux::module_wait() }
unsafe extern "C" fn imp_hash32_init(state: *mut u8) { core::ptr::write(state as *mut hash32::Hash32, hash32::Hash32::new()) }
unsafe extern "C" fn imp_hash32_update(state: *mut u8, p: *const u8, n: u32) { (*(state as *mut hash32::Hash32)).update(p, n) }
unsafe extern "C" fn imp_hash32_finish(state: *mut u8) -> u32 { core::ptr::read(state as *const hash32::Hash32).finish() | 1 }
