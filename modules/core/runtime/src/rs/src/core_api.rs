//! Interface C of docs/ARCHITECTURE.md, the core's side: `RealCore` (what a service calls through `CoreApi`), `FakeCore` (host tests) and the few core functions
//! a service may name directly. The trait and the pure types are in the crate `dkrt-abi`. The services reach the core through this module only
//! (`check:layers` enforces it).
//!
//! `RealCore` is a zero-sized type that delegates to the core functions, so a call through it costs what the direct call cost. A function that a host test must
//! drive takes a generic `C: CoreApi` (no `dyn`: the runtime has no allocator and no vtables); everything else calls the `REAL_CORE` constant.
use crate::{dlog, registry, settings, worker};
use core::ptr::addr_of;
use dkrt_abi::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
use dkrt_abi::dsp::DspState;
use dkrt_abi::registry::RegState;
use dkrt_sdk::api::{JobDoneFn, JobStepFn};

pub use dkrt_abi::core_api::{CoreApi, SampleCore, JOB_E_CANCELLED, LOG_INFO};

// The platform facts a service may read (the device profile, the interrupt mask, the sample-pool provider): the only platform names a service has.
pub use crate::platform::PLATFORM;
pub use dkrt_abi::platform::{Platform, SamplePool};
pub use dkrt_abi::storage::Storage;

// The C-API glue of the services' imports (`dk_sample_pool_*`): the handle a module holds and the record behind a context.
pub(crate) use crate::entry::{client, give, own_rec};

/// What the services' host tests use of the core's job queue.
#[cfg(test)]
pub(crate) mod testing {
    pub(crate) use crate::test_lock::LOCK as TEST_LOCK;
    pub(crate) use crate::worker::{cancel, test_pump, test_reset};
}

/// The core's own functions, called through `CoreApi`.
#[derive(Clone, Copy)]
pub struct RealCore;

/// The value a function that needs no host test passes to a `CoreApi` call.
pub const REAL_CORE: RealCore = RealCore;

impl SampleCore for RealCore {
    type Host = crate::platform::RealPlatform;
    fn host(&self) -> &crate::platform::RealPlatform {
        &PLATFORM
    }
}

impl CoreApi for RealCore {
    unsafe fn dynreg(&self) -> *const u32 {
        registry::info()
    }
    fn reg_state(&self) -> &'static mut RegState {
        registry::rs()
    }
    fn param_first(&self) -> u32 {
        registry::param_first()
    }
    fn param_owners(&self) -> &[u8] {
        unsafe { &(*addr_of!(registry::DKRT_REG)).param_owner }
    }
    unsafe fn write_param_row(&self, inf: *const u32, code: u32, template: u32, slot: u32, min: u32, max: u32, def: u32, name: u32, short: u32) {
        crate::params::write_param_row(inf, code, template, slot, min, max, def, name, short)
    }
    unsafe fn write_fx_param_row(&self, code: u32, template: u32, mirror: u32, min: u32, max: u32, def: u32, name: u32, short: u32) {
        crate::params::write_fx_param_row(code, template, mirror, min, max, def, name, short)
    }
    unsafe fn write_send_param_row(&self, code: u32, class: u32, owner_of: u32, mirror: u32, name: u32, short: u32) {
        crate::params::write_send_param_row(code, class, owner_of, mirror, name, short)
    }
    unsafe fn reset_param_row(&self, code: u32) {
        crate::params::reset_param_row(code)
    }
    unsafe fn make_rep(&self, rep: *mut u8, src: *const u8, upper: bool) -> u32 {
        crate::params::make_rep(rep, src, upper)
    }
    unsafe fn type_resolve(&self, key: u32) -> u32 {
        crate::assign::resolve(key)
    }
    fn type_map_used(&self) -> u32 {
        crate::assign::map_used()
    }
    unsafe fn guard_faults(&self, ty: u32) -> u32 {
        crate::watch::guard_faults(ty)
    }
    unsafe fn guard_disabled(&self, ty: u32) -> bool {
        crate::watch::guard_disabled(ty)
    }
    fn device_id(&self) -> u32 {
        PLATFORM.device_id()
    }
    unsafe fn module_name(&self, owner: u8) -> *const u8 {
        crate::st().modules[owner as usize - 1].name.as_ptr()
    }
    unsafe fn settings_get(&self, key: &[u8], out: &mut [u8]) -> i32 {
        settings::get(key, out)
    }
    unsafe fn settings_put(&self, key: &[u8], value: &[u8]) -> i32 {
        settings::put(key, value)
    }
    unsafe fn log(&self, kind: u16, flags: u16, args: [u32; 5]) {
        dlog::ev(kind, flags, args)
    }
    fn current_source(&self) -> u8 {
        dlog::current_source()
    }
    unsafe fn job_post(&self, owner: u8, step: Option<JobStepFn>, done: Option<JobDoneFn>, ctx: *mut u8) -> i32 {
        worker::post(owner, step, done, ctx)
    }
    unsafe fn job_cancel(&self, owner: u8, id: u32) -> i32 {
        worker::cancel(owner, id)
    }
    fn native_available(&self, kind: u32) -> bool {
        crate::native::available(kind)
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
        crate::native::register(kind, owner, page, flags, title, draw, event, bindings, words, word_count, arg, with_arg)
    }
    unsafe fn native_remove(&self, kind: u32) {
        crate::native::remove(kind)
    }
    unsafe fn native_turn(&self, view: *const u8, event: *const u8) -> u32 {
        crate::native::turn(view, event)
    }
    fn dsp(&self) -> &'static mut DspState {
        crate::dsp::ds()
    }
    unsafe fn dsp_info(&self) -> *const u32 {
        crate::dsp::info()
    }
    unsafe fn fx_info(&self) -> *const u32 {
        crate::dsp::finfo()
    }
    unsafe fn info(&self, name_hash: u32) -> *const u32 {
        PLATFORM.info(name_hash)
    }
    fn link_set(&self, slot: Slot, value: u32) {
        crate::link::lk().set(slot, value)
    }
    fn link_confirmed(&self, slot: Slot) -> Option<u32> {
        crate::link::lk().confirmed(slot)
    }
    fn link_errors(&self) -> u32 {
        crate::link::lk().errors()
    }
    fn link_forget(&self, win: u32, first: u32, count: u32) {
        crate::link::lk().forget(win, first, count)
    }
    unsafe fn link_submit(&self, owner: Owner, stream: &Stream) -> Option<Ticket> {
        crate::link::lk().submit(owner, stream)
    }
    fn link_result(&self, t: Ticket) -> LinkResult {
        crate::link::lk().result(t)
    }
    unsafe fn link_drain(&self, owner: Owner) -> bool {
        crate::link::lk().drain(owner)
    }
    unsafe fn link_run(&self, owner: Owner, seq: &Sequence) -> bool {
        crate::link::lk().run(owner, seq)
    }
}

#[cfg(test)]
pub use crate::fake_platform::FakePlatform;
#[cfg(test)]
pub use dkrt_test_support::{Call, FakeCore};

/// The shared lock of the tests that drive the runtime statics, for tests inside services.
#[cfg(test)]
pub(crate) use crate::test_lock::LOCK as TEST_LOCK;
