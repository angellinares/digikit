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

pub use dkrt_abi::core_api::{CoreApi, JOB_E_CANCELLED, LOG_INFO};

// The platform facts a service may read (the device profile, the interrupt mask, the sample-pool provider): the only platform names a service has.
pub use crate::platform::PLATFORM;
pub use dkrt_abi::platform::{Platform, SamplePool};
pub use dkrt_abi::storage::Storage;

// The C-API glue of the services' imports (`dk_sample_pool_*`): the handle a module holds and the record behind a context.
pub(crate) use crate::entry::{client, give, own_rec};

/// What the services' host tests use of the core's job queue.
#[cfg(test)]
pub(crate) mod testing {
    pub(crate) use crate::worker::{cancel, test_pump, test_reset, TEST_LOCK};
}

/// The core's own functions, called through `CoreApi`.
#[derive(Clone, Copy)]
pub struct RealCore;

/// The value a function that needs no host test passes to a `CoreApi` call.
pub const REAL_CORE: RealCore = RealCore;

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
    fn dsp(&self) -> &'static mut DspState {
        crate::dsp::ds()
    }
    unsafe fn dsp_info(&self) -> *const u32 {
        crate::dsp::info()
    }
    unsafe fn fx_info(&self) -> *const u32 {
        crate::dsp::finfo()
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
pub use fake::{Call, FakeCore};

#[cfg(test)]
mod fake {
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    /// One call the service made, in order.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Call {
        FxRow { code: u32, mirror: u32, def: u32 },
        SendRow { code: u32, mirror: u32 },
        ResetRow(u32),
        SettingsPut(Vec<u8>, Vec<u8>),
        Log { kind: u16, args: [u32; 5] },
        JobPost { owner: u8 },
        JobCancel { owner: u8, id: u32 },
        Set { slot: Slot, value: u32 },
    }

    /// A core that records what services ask of it. Its link is the real queue over a model of `core/dsp-link` (`link::fake::FakeLink`): the tests read the
    /// wire from `link.borrow().t.wire` and let the model's time pass with `pump`. `fx_table` is the words of `dynfx_info` the fake hands out (first row 0).
    pub struct FakeCore {
        pub calls: RefCell<Vec<Call>>,
        pub first: u32,
        pub owners: Vec<u8>,
        pub source: Cell<u8>,
        pub settings: RefCell<Vec<(Vec<u8>, Vec<u8>)>>,
        pub link: RefCell<&'static mut crate::link::Link<crate::link::fake::FakeLink>>,
        pub name: String,
        pub fx_table: Vec<u32>,
        reg: *mut RegState,
        dsp: *mut DspState,
    }

    impl FakeCore {
        pub fn new() -> Self {
            // zero is every state's valid initial value (the real objects are zeroed statics)
            let reg: &'static mut RegState = Box::leak(Box::new(unsafe { core::mem::zeroed() }));
            let dsp: &'static mut DspState = Box::leak(Box::new(unsafe { core::mem::zeroed() }));
            FakeCore {
                calls: RefCell::new(Vec::new()),
                first: 100,
                owners: Vec::new(),
                source: Cell::new(0),
                settings: RefCell::new(Vec::new()),
                link: RefCell::new(Box::leak(Box::new(crate::link::fake::link()))),
                name: String::from("fake\0"),
                fx_table: std::vec![0; 8],
                reg,
                dsp,
            }
        }
        pub fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }
        fn rec(&self, c: Call) {
            self.calls.borrow_mut().push(c);
        }
    }

    impl CoreApi for FakeCore {
        unsafe fn dynreg(&self) -> *const u32 {
            core::ptr::null()
        }
        fn reg_state(&self) -> &'static mut RegState {
            unsafe { &mut *self.reg }
        }
        fn param_first(&self) -> u32 {
            self.first
        }
        fn param_owners(&self) -> &[u8] {
            &self.owners
        }
        unsafe fn write_param_row(&self, _inf: *const u32, _c: u32, _t: u32, _slot: u32, _min: u32, _max: u32, _def: u32, _n: u32, _s: u32) {}
        unsafe fn write_fx_param_row(&self, code: u32, _t: u32, mirror: u32, _min: u32, _max: u32, def: u32, _n: u32, _s: u32) {
            self.rec(Call::FxRow { code, mirror, def })
        }
        unsafe fn write_send_param_row(&self, code: u32, _c: u32, _o: u32, mirror: u32, _n: u32, _s: u32) {
            self.rec(Call::SendRow { code, mirror })
        }
        unsafe fn reset_param_row(&self, code: u32) {
            self.rec(Call::ResetRow(code))
        }
        unsafe fn make_rep(&self, rep: *mut u8, _src: *const u8, _upper: bool) -> u32 {
            rep as u32
        }
        unsafe fn type_resolve(&self, key: u32) -> u32 {
            key & 0xff
        }
        fn type_map_used(&self) -> u32 {
            0
        }
        unsafe fn guard_faults(&self, _ty: u32) -> u32 {
            0
        }
        unsafe fn guard_disabled(&self, _ty: u32) -> bool {
            false
        }
        unsafe fn module_name(&self, _owner: u8) -> *const u8 {
            self.name.as_ptr()
        }
        unsafe fn settings_get(&self, key: &[u8], out: &mut [u8]) -> i32 {
            for (k, v) in self.settings.borrow().iter() {
                if k == key {
                    out[..v.len()].copy_from_slice(v);
                    return v.len() as i32;
                }
            }
            -1
        }
        unsafe fn settings_put(&self, key: &[u8], value: &[u8]) -> i32 {
            self.rec(Call::SettingsPut(key.to_vec(), value.to_vec()));
            0
        }
        unsafe fn log(&self, kind: u16, _flags: u16, args: [u32; 5]) {
            self.rec(Call::Log { kind, args })
        }
        fn current_source(&self) -> u8 {
            self.source.get()
        }
        unsafe fn job_post(&self, owner: u8, _s: Option<JobStepFn>, _d: Option<JobDoneFn>, _c: *mut u8) -> i32 {
            self.rec(Call::JobPost { owner });
            1
        }
        unsafe fn job_cancel(&self, owner: u8, id: u32) -> i32 {
            self.rec(Call::JobCancel { owner, id });
            0
        }
        fn dsp(&self) -> &'static mut DspState {
            unsafe { &mut *self.dsp }
        }
        unsafe fn dsp_info(&self) -> *const u32 {
            core::ptr::null()
        }
        unsafe fn fx_info(&self) -> *const u32 {
            self.fx_table.as_ptr()
        }
        fn link_set(&self, slot: Slot, value: u32) {
            self.rec(Call::Set { slot, value });
            self.link.borrow_mut().set(slot, value)
        }
        fn link_confirmed(&self, slot: Slot) -> Option<u32> {
            self.link.borrow().confirmed(slot)
        }
        fn link_errors(&self) -> u32 {
            self.link.borrow().errors()
        }
        fn link_forget(&self, win: u32, first: u32, count: u32) {
            self.link.borrow_mut().forget(win, first, count)
        }
        unsafe fn link_submit(&self, owner: Owner, stream: &Stream) -> Option<Ticket> {
            self.link.borrow_mut().submit(owner, stream)
        }
        fn link_result(&self, t: Ticket) -> LinkResult {
            self.link.borrow_mut().result(t)
        }
        unsafe fn link_drain(&self, owner: Owner) -> bool {
            self.link.borrow_mut().drain(owner)
        }
        unsafe fn link_run(&self, owner: Owner, seq: &Sequence) -> bool {
            self.link.borrow_mut().run(owner, seq)
        }
    }
}

/// The shared lock of the tests that drive the runtime statics, for tests inside services.
#[cfg(test)]
pub(crate) use crate::test_lock::LOCK as TEST_LOCK;
