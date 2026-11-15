//! Interface B of docs/ARCHITECTURE.md: what the core calls on a service, and only what the service declared.
//!
//! A service implements `Service`. The core keeps one static table of the in-crate services (`gen_services.rs`, written by `patchkit gen`
//! from `[[service]]` and `[records.kinds]` of sdk/sdk.toml) and walks it through `ServiceTable`: a row is the service's name, its record
//! kinds and one function pointer per method; no `dyn`, no table in RAM. The core never names a service outside the generated table.
//!
//! What a service declares:
//! - record kinds of the `register` section (the row's `kinds`: kind and length from sdk.toml), and their `check`, `apply` and `remove` (the registry walker calls them);
//! - the DSP section parts it owns: `dsp_check_*`, `dsp_admit`, `dsp_claim`, `dsp_release`, `dsp_clear`, `dsp_row_step`
//!   (the DSP loader in `dsp/mod.rs` keeps the area, the row table and the state machine);
//! - capacity counts (`cap`, per capacity kind), the module behind a fault site (`attribute`), the guard's state of a machine type (`guard_state`);
//! - the declarations of a module's DSP rows (`has_voice`, `declare_voice`, `declare_fx`) and `on_loaded`.
pub use dkrt_abi::service::*;

/// The table over the runtime's own core.
pub type ServiceTable = dkrt_abi::service::ServiceTable<crate::core_api::RealCore>;

/// The rows whose declare check failed (`check_declarations`): bit `i` for row `i`.
static mut FAILED: u32 = 0;

/// The in-crate services, in the order the core calls them (`[[service]]` of sdk/sdk.toml: samples, machines, fx), with the mask of the ones that failed
/// their declare check.
pub fn services() -> ServiceTable {
    ServiceTable { failed: unsafe { core::ptr::read_volatile(core::ptr::addr_of!(FAILED)) }, ..crate::gen_services::TABLE }
}

/// Declare-check-commit of the in-crate services (the runtime's init): each service declares its kinds, the runtime compares them with the generated
/// table of the service. A service that differs is FAILED: its kinds are unknown to the registry (a record of the kind is `E_FORMAT`), and the
/// event log says which (`E_DECL_KIND`). Returns the mask.
pub unsafe fn check_declarations() -> u32 {
    let failed = crate::gen_services::TABLE.check_declarations();
    core::ptr::write_volatile(core::ptr::addr_of_mut!(FAILED), failed);
    if failed != 0 {
        // a MOD_LOAD of selection index 0xffffffff (no module): the result and the mask of the failed rows
        crate::dlog::ev(dkrt_sdk::logk::MOD_LOAD, dkrt_sdk::logk::F_ERROR, [u32::MAX, dkrt_sdk::E_DECL_KIND as u32, failed, 0, 0]);
    }
    failed
}

/// The C-ABI entry points of the services that the core links into its own tables (interface A: the imports of `sdk.toml` and the events the adapters
/// call). This is the one place where the core names a service function; the rest of the core calls `Services` or `CoreApi`.
pub mod exports {
    /// The effect list of DIGIKIT > FX ROUTES over the real core (the fx crate is generic over the core).
    pub unsafe fn fx_entry(index: u32, out: &mut dkrt_sdk::DkrtFx) -> i32 {
        dkrt_svc_fx::fxsend::fx_entry(&crate::core_api::REAL_CORE, index, out)
    }
    pub use crate::services::machines::pageclick::{param_get, param_set};
    pub use crate::services::samples::tracks::{dkrt_pool_done, dkrt_pool_lost, dkrt_project_changed};
    pub use crate::services::samples::{
        dk_sample_pool_add, dk_sample_pool_clear, dk_sample_pool_info, dk_sample_pool_open, dk_sample_pool_path, dk_sample_pool_peaks,
        dk_sample_pool_peaks_window, dk_sample_pool_set, dk_sample_pool_status,
    };
    #[cfg(test)]
    pub(crate) use crate::services::machines::nativepage::PageSet;
}

/// The length of a record of `kind`; `E_FORMAT`-worthy kinds are `None`. For the builder lowering (`collect`).
pub fn len(kind: u32) -> Option<u32> {
    services().len(kind)
}

#[cfg(test)]
pub(crate) mod tests;
