//! Interface C of docs/ARCHITECTURE.md: what a service (`src/services/*` of the runtime crate) calls in the core. A service reaches parameter rows, the
//! registry's and the DSP loader's state, the module table, settings, the store, the log, the job queue and the DSP link only through `CoreApi`, or through the
//! pure types of this crate.
//!
//! `RealCore` (runtime crate) is a zero-sized type that delegates to the core functions, so a call through it costs what the direct call cost. A function
//! that a host test must drive takes a generic `C: CoreApi` (no `dyn`: the runtime has no allocator and no vtables); everything else calls the `REAL_CORE`
//! constant. `FakeCore` (host tests of the runtime crate) records the calls in order.
//!
//! The DSP link belongs to the core (`link.rs` of the runtime crate): a service sets live values with `link_set` and sends owned streams with
//! `link_submit` (types in `crate::link`). It never asks whether the link is busy.
//!
//! Platform facts: the samples service reads the few it needs through `SampleCore::host` (`platform::SampleHost`); `SampleCore` extends `CoreApi` and is not part
//! of interface C of a `.dks` (no import carries a platform handle). The device profile comes with the device id.
use crate::dsp::DspState;
use crate::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
use crate::platform::SampleHost;
use crate::registry::RegState;
use dkrt_sdk::api::{JobDoneFn, JobStepFn};

/// The `done` status of a job that was cancelled (`sdk/include/dk/platform/job.h`).
pub const JOB_E_CANCELLED: i32 = -80;

/// The flags of an info-level log record.
pub const LOG_INFO: u16 = dkrt_sdk::logk::L_INFO << dkrt_sdk::logk::LEVEL_SHIFT;

/// `CoreApi` with the platform facts the samples service reads. Only the in-crate samples service takes this bound; a service file has no such core.
pub trait SampleCore: CoreApi {
    /// What this core's platform is: the real one in the runtime, a small fake in a host test.
    type Host: SampleHost;
    /// The platform facts the samples service may read (`SampleHost`).
    fn host(&self) -> &Self::Host;
}

pub trait CoreApi {
    // ---- the registry's rows ---------------------------------------------------------------------------------------------
    /// The words of `dynreg_info` (`registry::w`); null without a registry.
    unsafe fn dynreg(&self) -> *const u32;
    /// The ownership and string state of the reserved machine, parameter and page rows.
    fn reg_state(&self) -> &'static mut RegState;
    /// The code of the first run-time parameter row; `u32::MAX` without a registry.
    fn param_first(&self) -> u32;
    /// The owner (module record + 1, 0 free) of each run-time parameter row.
    fn param_owners(&self) -> &[u8];
    /// Write one parameter table row from its lowering spec (`inf` is `dynreg()`).
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
    );
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
    );
    unsafe fn write_send_param_row(
        &self,
        code: u32,
        class: u32,
        owner_of: u32,
        mirror: u32,
        name: u32,
        short: u32,
    );
    unsafe fn reset_param_row(&self, code: u32);
    /// A firmware string representation of `src` at `rep`; the address of its characters.
    unsafe fn make_rep(&self, rep: *mut u8, src: *const u8, upper: bool) -> u32;
    /// The machine type number for machine key `key` of the module being loaded (sticky assignment); 0 when there is none.
    unsafe fn type_resolve(&self, key: u32) -> u32;
    /// Entries of the sticky type map in use (for CAPACITY).
    fn type_map_used(&self) -> u32;
    /// What the DSP guard counted for machine type `ty`: faults and whether it disabled the type.
    unsafe fn guard_faults(&self, ty: u32) -> u32;
    unsafe fn guard_disabled(&self, ty: u32) -> bool;

    // ---- the device ------------------------------------------------------------------------------------------------------
    /// The device id of the profile the runtime was built for (`Platform::device_id`), to compare with what an adapter table records.
    fn device_id(&self) -> u32;

    // ---- the module table ------------------------------------------------------------------------------------------------
    /// The NUL-terminated id of module record `owner - 1` (`owner` is 1 or more).
    unsafe fn module_name(&self, owner: u8) -> *const u8;

    // ---- settings --------------------------------------------------------------------------------------------------------
    unsafe fn settings_get(&self, key: &[u8], out: &mut [u8]) -> i32;
    unsafe fn settings_put(&self, key: &[u8], value: &[u8]) -> i32;

    // ---- log and source --------------------------------------------------------------------------------------------------
    unsafe fn log(&self, kind: u16, flags: u16, args: [u32; 5]);
    /// The module whose code runs now (record index + 1; 0 outside module code).
    fn current_source(&self) -> u8;

    // ---- jobs ------------------------------------------------------------------------------------------------------------
    unsafe fn job_post(
        &self,
        owner: u8,
        step: Option<JobStepFn>,
        done: Option<JobDoneFn>,
        ctx: *mut u8,
    ) -> i32;
    unsafe fn job_cancel(&self, owner: u8, id: u32) -> i32;

    // ---- the native page adapter (core/machines) -----------------------------------------------------------------------------
    /// Whether the firmware has the native page calls of record kind `kind` (`KIND_NATIVE_PAGE` or `KIND_NATIVE_PAGE_V2`).
    fn native_available(&self, kind: u32) -> bool;
    /// Register a native page for machine type `owner`: `title` (a NUL-terminated text), the absolute `draw` and `event` callbacks, eight `[kind, value, position]`
    /// bindings, the `word_count` parameter codes of its snapshot and the callbacks' `arg` (`with_arg`: 1 when there is one). A status (`OK` or `E_*`).
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
    ) -> i32;
    /// Remove the native pages of machine type `kind` (before its callbacks and strings are released).
    unsafe fn native_remove(&self, kind: u32);
    /// The native handler of an encoder turn on `view`: nonzero when it consumed the turn.
    unsafe fn native_turn(&self, view: *const u8, event: *const u8) -> u32;

    // ---- the DSP loader's state and tables -------------------------------------------------------------------------------
    /// The DSP loader's state and row table.
    fn dsp(&self) -> &'static mut DspState;
    /// The words of `dyndsp_info` (voice rows, the area); null without it.
    unsafe fn dsp_info(&self) -> *const u32;
    /// The words of `dynfx_info` (effect rows); null without it.
    unsafe fn fx_info(&self) -> *const u32;
    /// The words of the entry `name_hash` of the image's one table `dkrt_info_table` (`dkneeds::keys::name_hash` of the name that an adapter's `[[info]]` gives): the data
    /// table of an image adapter, such as `fxpages_info`. Null when the image has no such entry or the core does not serve it. The caller checks the words (magic, device).
    /// The core forwards to `Platform::info`; it names no service and no table.
    unsafe fn info(&self, name_hash: u32) -> *const u32;

    // ---- the DSP link ----------------------------------------------------------------------------------------------------
    /// Set the live value of `slot`. The core sends it (the newest value only) and sends it again when a write failed.
    fn link_set(&self, slot: Slot, value: u32);
    /// The value of `slot` that the DSP has taken (`None`: none yet, or the DSP restarted since).
    fn link_confirmed(&self, slot: Slot) -> Option<u32>;
    /// Live writes that failed so far (each is sent again).
    fn link_errors(&self) -> u32;
    /// Drop the live values of the `count` words from `first` of window `win` (their row is gone): nothing more is sent for them.
    fn link_forget(&self, win: u32, first: u32, count: u32);
    /// Queue a request of `owner`. `None` when the queue is full or the request is not valid (an empty or odd-sized transfer, a reserved window).
    unsafe fn link_submit(&self, owner: Owner, stream: &Stream) -> Option<Ticket>;
    /// The result of a request, for its owner only. A terminal result is given once.
    fn link_result(&self, t: Ticket) -> LinkResult;
    /// Cancel the queued requests of `owner` and wait for its transfer in flight. `false` when that did not end in time.
    unsafe fn link_drain(&self, owner: Owner) -> bool;
    /// Send `seq` for `owner` and wait for the result (error paths and unload clears). `true` when every step was taken.
    unsafe fn link_run(&self, owner: Owner, seq: &Sequence) -> bool;
}
