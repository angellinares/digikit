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
pub type ServiceTable = dkrt_abi::service::ServiceTable;

/// The rows whose declare check failed (`check_declarations`): bit `i` for row `i`.
static mut FAILED: u32 = 0;

/// The services in the order the core calls them: the in-crate ones (`[[service]]` of sdk/sdk.toml with `rust =`: samples, fx), then the rows of
/// loaded service files (`svc_rows.rs`), with the mask of the in-crate ones that failed their declare check.
pub fn services() -> ServiceTable {
    #[cfg(test)]
    crate::svc_rows::test_machines();
    crate::svc_rows::table(ServiceTable { failed: unsafe { core::ptr::read_volatile(core::ptr::addr_of!(FAILED)) }, ..crate::gen_services::TABLE })
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
    /// The effect list of DIGIKIT > FX ROUTES: the fx service's row method; without the service file (no fx) there is no effect.
    pub unsafe fn fx_entry(index: u32, out: &mut dkrt_sdk::DkrtFx) -> i32 {
        super::services().fx_entry(index, out)
    }
    /// Host tests only: the page set of the machines service, and its row as a loaded service file has it (`svc_rows::test_machines`).
    #[cfg(test)]
    pub(crate) use dkrt_svc_machines::nativepage::PageSet;
    #[cfg(test)]
    pub(crate) fn machines_row() -> &'static dkrt_abi::service::ServiceRow {
        static ROW: dkrt_abi::service::ServiceRow =
            dkrt_abi::service::ServiceRow::of::<dkrt_svc_machines::records::Machines, crate::core_api::RealCore>("machines", &dkrt_sdk::records::KINDS_MACHINES);
        &ROW
    }
    /// The machine type numbers of the machine rows module `owner` filled (`module_cmds`: the module list); returns how many.
    pub fn machine_types_of(owner: u8, out: &mut [u8; 8]) -> usize {
        unsafe { super::services().machine_types_of(owner, out) }
    }
    /// The shapes of the lfo service, for the runtime's own tests of the record path.
    #[cfg(test)]
    pub(crate) use dkrt_svc_lfo::records::{owner_of, shape, used, Lfo, MAX_SHAPES, SAMPLES};

    // ---- the sample pool (interface A: the imports of sdk.toml over the samples service) ----------------------------------------
    // The handle a module holds is its client number (`entry`); each call resolves it inside the service. The service is generic over the core: these wrappers
    // pass the real one.
    use crate::core_api::{client, give, own_rec, REAL_CORE};
    use dkrt_sdk::E_CONTRACT;

    pub unsafe extern "C" fn dk_sample_pool_open(ctx: *mut u8, out: *mut *mut u8) -> i32 {
        if out.is_null() || own_rec(ctx).is_none() {
            return E_CONTRACT;
        }
        give(out, dkrt_svc_samples::tracks::open_first(&REAL_CORE))
    }

    pub unsafe extern "C" fn dk_sample_pool_add(p: *mut u8, path: *const u8) -> i32 {
        dkrt_svc_samples::tracks::pool_add(&REAL_CORE, client(p), path)
    }

    pub unsafe extern "C" fn dk_sample_pool_clear(p: *mut u8, index: u32) -> i32 {
        dkrt_svc_samples::tracks::pool_clear(&REAL_CORE, client(p), index)
    }

    pub unsafe extern "C" fn dk_sample_pool_status(p: *mut u8, index: u32) -> i32 {
        dkrt_svc_samples::tracks::status(client(p), index)
    }

    pub unsafe extern "C" fn dk_sample_pool_info(p: *mut u8, index: u32, out: *mut u8) -> i32 {
        dkrt_svc_samples::tracks::info(client(p), index, out)
    }

    pub unsafe extern "C" fn dk_sample_pool_path(p: *mut u8, index: u32, buf: *mut u8, cap: u32) -> i32 {
        dkrt_svc_samples::tracks::path(&REAL_CORE, client(p), index, buf, cap)
    }

    pub unsafe extern "C" fn dk_sample_pool_set(p: *mut u8, index: u32, path: *const u8) -> i32 {
        dkrt_svc_samples::tracks::pool_set(&REAL_CORE, client(p), index, path)
    }

    pub unsafe extern "C" fn dk_sample_pool_peaks(p: *mut u8, index: u32, token: u32, out: *mut u8, capacity: u32) -> i32 {
        dkrt_svc_samples::tracks::peaks(client(p), index, token, out, capacity)
    }

    pub unsafe extern "C" fn dk_sample_pool_peaks_window(
        p: *mut u8,
        index: u32,
        token: u32,
        first: u32,
        frames: u32,
        columns: u32,
        out: *mut u8,
        capacity: u32,
    ) -> i32 {
        dkrt_svc_samples::peakwin::window(&REAL_CORE, client(p), index, token, first, frames, columns, out, capacity)
    }

    // ---- the events the adapters call ------------------------------------------------------------------------------------------
    /// Adapter to runtime: load `req` finished (`result` 0, E_POOL, E_READ or E_FORMAT).
    #[no_mangle]
    pub unsafe extern "C" fn dkrt_pool_done(req: u32, result: i32) {
        dkrt_svc_samples::tracks::pool_done(req, result)
    }
    /// Adapter to runtime: the backing was lost, all handles are invalid.
    #[no_mangle]
    pub unsafe extern "C" fn dkrt_pool_lost() {
        dkrt_svc_samples::tracks::pool_lost(&REAL_CORE)
    }
    /// Project adapter to runtime: the current project slot changed.
    #[no_mangle]
    pub unsafe extern "C" fn dkrt_project_changed(slot: u32) {
        dkrt_svc_samples::tracks::project_changed(slot)
    }
}

/// The length of a record of `kind`; `E_FORMAT`-worthy kinds are `None`. For the builder lowering (`collect`).
pub fn len(kind: u32) -> Option<u32> {
    services().len(kind)
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod lfo_tests;
#[cfg(test)]
pub(crate) mod fx_fixture;
#[cfg(test)]
pub(crate) mod dks_decls;
