//! Host-test fakes of the runtime interfaces, shared by the crate `dkrt` and the service crates (`modules/core/services/*`).
//!
//! `FakeCore` implements `CoreApi` (interface C) and records the calls a service makes. Its link is the real queue (`dkrt-link`) over a model of
//! `core/dsp-link`. It lives in a crate of its own, below the services and the runtime, so a service crate tests against it without naming the runtime.
extern crate std;
use core::cell::{Cell, RefCell};
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::dsp::DspState;
use dkrt_abi::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
use dkrt_abi::registry::RegState;
use dkrt_link::fake::{link, FakeLink};
use dkrt_link::Link;
use dkrt_sdk::api::{JobDoneFn, JobStepFn};
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
    pub link: RefCell<&'static mut Link<FakeLink>>,
    pub name: String,
    pub fx_table: Vec<u32>,
    pub device: u32,
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
            link: RefCell::new(Box::leak(Box::new(link()))),
            name: String::from("fake\0"),
            fx_table: std::vec![0; 8],
            device: 0,
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
    unsafe fn write_param_row(
        &self,
        _inf: *const u32,
        _c: u32,
        _t: u32,
        _slot: u32,
        _min: u32,
        _max: u32,
        _def: u32,
        _n: u32,
        _s: u32,
    ) {
    }
    unsafe fn write_fx_param_row(
        &self,
        code: u32,
        _t: u32,
        mirror: u32,
        _min: u32,
        _max: u32,
        def: u32,
        _n: u32,
        _s: u32,
    ) {
        self.rec(Call::FxRow { code, mirror, def })
    }
    unsafe fn write_send_param_row(
        &self,
        code: u32,
        _c: u32,
        _o: u32,
        mirror: u32,
        _n: u32,
        _s: u32,
    ) {
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
    fn device_id(&self) -> u32 {
        self.device
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
    unsafe fn job_post(
        &self,
        owner: u8,
        _s: Option<JobStepFn>,
        _d: Option<JobDoneFn>,
        _c: *mut u8,
    ) -> i32 {
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
