//! Host-test fakes of the runtime interfaces, shared by the crate `dkrt` and the service crates (`modules/core/services/*`).
//!
//! `FakeCore` implements `CoreApi` (interface C) and records the calls a service makes. Its link is the real queue (`dkrt-link`) over a model of
//! `core/dsp-link`. It lives in a crate of its own, below the services and the runtime, so a service crate tests against it without naming the runtime.
extern crate std;
mod pool;
mod storage;
use core::cell::{Cell, RefCell};
use dkrt_abi::core_api::{CoreApi, SampleCore};
use dkrt_abi::dsp::DspState;
use dkrt_abi::fw::Fw;
use dkrt_abi::link::{LinkResult, Owner, Sequence, Slot, Stream, Ticket};
use dkrt_abi::platform::SampleHost;
use dkrt_abi::registry::RegState;
use dkrt_link::fake::{link, FakeLink};
use dkrt_link::Link;
use dkrt_sdk::api::{JobDoneFn, JobStepFn};
use std::boxed::Box;
use std::string::String;
use std::vec::Vec;
pub use storage::FakeStorage;

/// One call the service made, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    ParamRow {
        code: u32,
        min: u32,
        max: u32,
        def: u32,
    },
    FxRow {
        code: u32,
        mirror: u32,
        def: u32,
    },
    SendRow {
        code: u32,
        mirror: u32,
    },
    ResetRow(u32),
    SettingsPut(Vec<u8>, Vec<u8>),
    Log {
        kind: u16,
        args: [u32; 5],
    },
    JobPost {
        owner: u8,
    },
    JobCancel {
        owner: u8,
        id: u32,
    },
    Set {
        slot: Slot,
        value: u32,
    },
    /// A native page registered for machine type `owner`: the callbacks and the snapshot words.
    NativeRegister {
        kind: u32,
        owner: u32,
        page: u32,
        flags: u32,
        draw: u32,
        event: u32,
        words: Vec<u32>,
        arg: u32,
        with_arg: u32,
    },
    NativeRemove(u32),
    NativeTurn,
}

/// The platform facts of a `FakeCore` (`SampleHost`): the sample files in a `FakeStorage`, whether the pool adapter is in the image and whether the
/// samples are files of the store (a DN2) or of the firmware's file system (a DT2). The interrupt mask is a counter: `masked` says how many masks are open.
pub struct FakeHost {
    pub storage: FakeStorage,
    pub pool: Cell<bool>,
    pub in_store: Cell<bool>,
    pub masked: Cell<u32>,
}

impl FakeHost {
    pub fn new() -> Self {
        FakeHost {
            storage: FakeStorage::new(),
            pool: Cell::new(true),
            in_store: Cell::new(false),
            masked: Cell::new(0),
        }
    }
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleHost for FakeHost {
    type Store = FakeStorage;
    fn store(&self) -> &FakeStorage {
        &self.storage
    }
    fn pool_present(&self) -> bool {
        self.pool.get()
    }
    fn files_in_store(&self) -> bool {
        self.in_store.get()
    }
    fn mask_irq(&self) -> u32 {
        self.masked.set(self.masked.get() + 1);
        0x2700
    }
    fn restore_irq(&self, _sr: u32) {
        self.masked.set(self.masked.get() - 1);
    }
}

/// A job a service posted to the fake core's queue (`FakeCore::job_pump`).
struct FakeJob {
    id: u32,
    owner: u8,
    step: Option<JobStepFn>,
    done: Option<JobDoneFn>,
    ctx: *mut u8,
    cancelled: bool,
    /// The status the last step ended with: `done` is called with it at the next pass.
    ended: Option<i32>,
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
    /// The info entries `CoreApi::info` hands out, by name hash: the words of each. Empty: the image has no table.
    pub info_tables: Vec<(u32, Vec<u32>)>,
    pub device: u32,
    /// The words of `dynreg_info` the fake hands out (empty: no registry, `dynreg` is null).
    pub dynreg_table: Vec<u32>,
    /// What `native_available` answers, what `native_register` returns and what `native_turn` returns.
    pub native_available: bool,
    pub native_status: i32,
    pub native_consumes: u32,
    /// The platform facts (`CoreApi::host`).
    pub host: FakeHost,
    jobs: RefCell<Vec<FakeJob>>,
    next_job: Cell<u32>,
    reg: *mut RegState,
    dsp: *mut DspState,
}

impl Default for FakeCore {
    fn default() -> Self {
        Self::new()
    }
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
            info_tables: Vec::new(),
            device: 0,
            dynreg_table: Vec::new(),
            native_available: true,
            native_status: 0,
            native_consumes: 0,
            host: FakeHost::new(),
            jobs: RefCell::new(Vec::new()),
            next_job: Cell::new(0),
            reg,
            dsp,
        }
    }
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
    /// One UI pass and one worker step, as the runtime's job queue runs them: a job whose last step ended, or that was cancelled, gets its `done` (status
    /// or `JOB_E_CANCELLED`) and leaves the queue; then the first job left runs one step (it stays while the step returns 1). A `done` may post a job.
    ///
    /// # Safety
    ///
    /// The pump calls the `step` and `done` functions of the queued jobs with their `ctx`. Each function must be safe to call
    /// with the `ctx` that was posted with it.
    pub unsafe fn job_pump(&self) {
        let ended: Vec<FakeJob> = {
            let mut jobs = self.jobs.borrow_mut();
            let (out, keep): (Vec<FakeJob>, Vec<FakeJob>) = jobs
                .drain(..)
                .partition(|j| j.ended.is_some() || j.cancelled);
            *jobs = keep;
            out
        };
        for j in ended {
            if let Some(done) = j.done {
                done(
                    j.ctx,
                    j.ended.unwrap_or(dkrt_abi::core_api::JOB_E_CANCELLED),
                );
            }
        }
        let first = self.jobs.borrow().first().map(|j| (j.id, j.step, j.ctx));
        if let Some((id, Some(step), ctx)) = first {
            let r = step(ctx);
            if r != 1 {
                if let Some(j) = self.jobs.borrow_mut().iter_mut().find(|j| j.id == id) {
                    j.ended = Some(r);
                }
            }
        }
    }
    /// The jobs still queued.
    pub fn jobs_queued(&self) -> usize {
        self.jobs.borrow().len()
    }
    fn rec(&self, c: Call) {
        self.calls.borrow_mut().push(c);
    }
}

impl SampleCore for FakeCore {
    type Host = FakeHost;
    fn host(&self) -> &FakeHost {
        &self.host
    }
}

impl CoreApi for FakeCore {
    unsafe fn dynreg(&self) -> *const u32 {
        if self.dynreg_table.is_empty() {
            core::ptr::null()
        } else {
            self.dynreg_table.as_ptr()
        }
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
        code: u32,
        _t: u32,
        _slot: u32,
        min: u32,
        max: u32,
        def: u32,
        _n: u32,
        _s: u32,
    ) {
        self.rec(Call::ParamRow {
            code,
            min,
            max,
            def,
        })
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
        step: Option<JobStepFn>,
        done: Option<JobDoneFn>,
        ctx: *mut u8,
    ) -> i32 {
        self.rec(Call::JobPost { owner });
        let id = self.next_job.get() + 1;
        self.next_job.set(id);
        self.jobs.borrow_mut().push(FakeJob {
            id,
            owner,
            step,
            done,
            ctx,
            cancelled: false,
            ended: None,
        });
        id as i32
    }
    unsafe fn job_cancel(&self, owner: u8, id: u32) -> i32 {
        self.rec(Call::JobCancel { owner, id });
        match self
            .jobs
            .borrow_mut()
            .iter_mut()
            .find(|j| j.id == id && j.owner == owner)
        {
            Some(j) => {
                j.cancelled = true;
                0
            }
            None => 0,
        }
    }
    fn native_available(&self, _kind: u32) -> bool {
        self.native_available
    }
    unsafe fn native_register(
        &self,
        kind: u32,
        owner: u32,
        page: u32,
        flags: u32,
        _title: *const u8,
        draw: u32,
        event: u32,
        _bindings: *const u32,
        words: *const u32,
        word_count: u32,
        arg: u32,
        with_arg: u32,
    ) -> i32 {
        let words = core::slice::from_raw_parts(words, word_count as usize).to_vec();
        self.rec(Call::NativeRegister {
            kind,
            owner,
            page,
            flags,
            draw,
            event,
            words,
            arg,
            with_arg,
        });
        self.native_status
    }
    unsafe fn native_remove(&self, kind: u32) {
        self.rec(Call::NativeRemove(kind))
    }
    unsafe fn native_turn(&self, _view: *const u8, _event: *const u8) -> u32 {
        self.rec(Call::NativeTurn);
        self.native_consumes
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
    unsafe fn info(&self, name_hash: u32) -> *const u32 {
        self.info_tables
            .iter()
            .find(|(h, _)| *h == name_hash)
            .map_or(core::ptr::null(), |(_, w)| w.as_ptr())
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

/// One call a service made on the firmware's parameter page, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum FwCall {
    PageValue {
        view: usize,
        code: u32,
    },
    Object {
        view: usize,
        code: u32,
        track: u32,
    },
    ObjectValue {
        obj: usize,
        code: u32,
    },
    Store {
        obj: usize,
        code: u32,
        raw: i32,
        track: u32,
    },
    ViewTrack {
        track_ref: usize,
    },
    ParamCode {
        view: usize,
        enc: u32,
    },
}

/// A firmware page that records what a service asks of it (`Fw`). The answers are fields: a test sets them with struct update syntax.
pub struct FakeFw {
    /// A mode key (FUNC, a trig or track mode) is held.
    pub held: bool,
    /// `object` finds no parameter object (answers 0).
    pub no_object: bool,
    /// What `page_value` answers.
    pub page_value: i32,
    /// What `object_value` answers.
    pub object_value: i32,
    /// What `view_track` answers.
    pub track: u32,
    /// What `param_code` answers.
    pub param_code: i32,
    pub calls: RefCell<Vec<FwCall>>,
}

impl Default for FakeFw {
    fn default() -> Self {
        FakeFw {
            held: false,
            no_object: false,
            page_value: 0x0500,
            object_value: 0xffff_f800u32 as i32, // a negative halfword with garbage above
            track: 0,
            param_code: -1,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl FakeFw {
    pub fn calls(&self) -> Vec<FwCall> {
        self.calls.borrow().clone()
    }
    fn rec(&self, c: FwCall) {
        self.calls.borrow_mut().push(c);
    }
}

impl Fw for FakeFw {
    fn mode_key_held(&self) -> bool {
        self.held
    }
    fn page_value(&self, view: usize, code: u32) -> i32 {
        self.rec(FwCall::PageValue { view, code });
        self.page_value
    }
    fn object(&self, view: usize, code: u32, track: u32) -> usize {
        self.rec(FwCall::Object { view, code, track });
        if self.no_object {
            0
        } else {
            0x7000
        }
    }
    fn object_value(&self, obj: usize, code: u32) -> i32 {
        self.rec(FwCall::ObjectValue { obj, code });
        self.object_value
    }
    fn store(&self, obj: usize, code: u32, raw: i32, track: u32) {
        self.rec(FwCall::Store {
            obj,
            code,
            raw,
            track,
        });
    }
    fn view_track(&self, track_ref: usize) -> u32 {
        self.rec(FwCall::ViewTrack { track_ref });
        self.track
    }
    fn param_code(&self, view: usize, enc: u32) -> i32 {
        self.rec(FwCall::ParamCode { view, enc });
        self.param_code
    }
}
