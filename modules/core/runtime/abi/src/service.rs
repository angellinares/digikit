//! Interface B of docs/ARCHITECTURE.md: what the core calls on a service, and only what the service declared.
//!
//! A service implements `Service`. The core keeps one static table of the in-crate services (`ServiceTable` over `ServiceRow`s,
//! generated from `[[service]]` of sdk/sdk.toml into `gen_services.rs`): a row is the service's name, the record kinds it owns
//! (kind and length, from `[records.kinds]`) and one function pointer per method; there is no `dyn` and no table in RAM.
//! The core never names a service outside the generated table (docs/plans/runtime-and-services.md R7).
//!
//! What a service declares:
//! - record kinds of the `register` section (`len`), and their `check`, `apply` and `remove` (the registry walker calls them);
//! - the DSP section parts it owns: `dsp_check_*`, `dsp_admit`, `dsp_claim`, `dsp_release`, `dsp_clear`, `dsp_row_step`
//!   (the DSP loader in `dsp/mod.rs` keeps the area, the row table and the state machine);
//! - capacity counts (`caps`), the module behind a fault site (`attribute`), the guard's state of a machine type (`guard_state`);
//! - the declarations of a module's DSP rows (`has_voice`, `declare_voice`, `declare_fx`) and `on_loaded`.
use crate::core_api::CoreApi;
use crate::dsp::{DspSec, Job};
use crate::registry::ApplyArgs;
use core::ffi::c_void;
use dkrt_sdk::*;

/// The ABI fingerprint of interfaces B and C between the runtime and a service file (`dk_svc_abi`, header word 0x3c of a `.dks`).
///
/// It is the hash of the bytes of the boundary files of this crate (`FILES_HASH`: `service.rs`, `core_api.rs`, `link.rs`, `dsp.rs`, `registry.rs` and
/// `core_imports.rs`, written by `patchkit gen`) with the `size_of` of every `repr(C)` struct that crosses the boundary mixed in at compile time. The
/// sizes make the value depend on the target: the runtime and a service file built for the same device have the same value, a host build another one
/// (64-bit pointers). The runtime reports its value in INFO bytes 41 to 44 and the loader compares it with the word of a service file; 0 in the
/// word means the file uses neither interface. The value is never 0.
pub const SVC_ABI: u32 = svc_abi();

const fn svc_abi() -> u32 {
    use core::mem::size_of;
    let sizes = [
        size_of::<ServiceRow>(),
        size_of::<CheckCtx>(),
        size_of::<ApplyCtx>(),
        size_of::<Claim>(),
        size_of::<RowWrite>(),
        size_of::<GuardState>(),
        size_of::<DeclItem>(),
        size_of::<Declare>(),
        size_of::<ApplyArgs>(),
        size_of::<crate::registry::RegState>(),
        size_of::<Job>(),
        size_of::<DspSec>(),
        size_of::<crate::dsp::DspState>(),
        size_of::<crate::link::Slot>(),
        size_of::<crate::link::Owner>(),
        size_of::<crate::link::Ticket>(),
        size_of::<crate::link::LinkResult>(),
        size_of::<crate::link::Step>(),
        size_of::<crate::link::Sequence>(),
        size_of::<crate::link::Stream>(),
        size_of::<DkrtFx>(),
    ];
    let mut h = crate::gen_svc_abi::FILES_HASH;
    let mut i = 0;
    while i < sizes.len() {
        h = (h ^ sizes[i] as u32).wrapping_mul(0x0100_0193);
        i += 1;
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// The steps of the validation of a `register` section, in the order the registry ran them when it was one body. The walker runs
/// each step over the whole list of services before the next, and a service does the steps for its kinds over its records in file
/// order, so the first fault is the one the old body reported.
pub mod check_step {
    /// The most records of each kind.
    pub const COUNTS: u32 = 0;
    /// Records that name another record of the file (click actions).
    pub const ACTIONS: u32 = 1;
    /// Native pages.
    pub const NATIVE: u32 = 2;
    /// Effect records.
    pub const EFFECTS: u32 = 3;
    /// Records the device must support (pages).
    pub const DEVICE: u32 = 4;
    /// The parameter, page and machine records.
    pub const RECORDS: u32 = 5;
    /// Records that need an owner record of the file (a page needs its machine).
    pub const OWNERS: u32 = 6;
    /// Free rows for what the file adds.
    pub const ROWS: u32 = 7;
    pub const COUNT: u32 = 8;
}

/// The places in the core's tick where services run, in the order the core calls them (`ServiceList::tick`). A service names the phase of the tick work it owns.
pub mod tick_phase {
    /// After the I/O tick, before the files tick.
    pub const EARLY: u32 = 0;
    /// After the files tick.
    pub const LATE: u32 = 1;
    /// After the late phase, before the settings tick.
    pub const LAST: u32 = 2;
}

/// What the registry walker gives `check`: the `register` section (its layout is validated) and the registry's table.
#[repr(C)]
pub struct CheckCtx {
    pub reg: *const u8,
    pub image_size: u32,
    /// The words of `dynreg_info` (not null).
    pub inf: *const u32,
}

/// What the registry walker gives `apply`: the (checked) section of module record `owner - 1`, loaded at `base`.
#[repr(C)]
pub struct ApplyCtx<'a> {
    pub owner: u8,
    pub reg: *const u8,
    pub base: u32,
    pub inf: *const u32,
    /// The callback arguments the commit set (`registry::set_args`).
    pub args: Option<&'a ApplyArgs>,
}

/// What `dsp::reserve` gives `dsp_claim`: the module's range is placed at `base` (byte address), and the states of its voices and
/// effects follow image and bss in record order (`state_at` moves on).
#[repr(C)]
pub struct Claim<'a> {
    pub rec: usize,
    pub job: &'a Job,
    pub base: u32,
    pub byte_shift: u32,
    pub state_at: u32,
}

/// One word the DSP loader writes to publish a row: window, word index and value. The loader collects the steps of one row into a link `Sequence`.
/// `published` is set once the write is accepted, when from then on the row may have words in the DSP (so a failure must clear them). `last` ends the
/// row (its type or pc word, the one that makes it visible). `undo` is the word that takes a step back when the sequence fails (`has_undo`).
#[repr(C)]
pub struct RowWrite {
    pub win: u32,
    pub word: u32,
    pub value: u32,
    pub published: Option<&'static mut u8>,
    pub last: bool,
    pub has_undo: bool,
    pub undo: u32,
}

/// What the guard counted for a machine type (`MODULES` reports it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct GuardState {
    pub faults: u32,
    pub disabled: bool,
}

/// One declaration of `Declare`: (table tag, first word, second word, third word); an unused word is 0.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeclItem {
    pub tag: u16,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

impl DeclItem {
    pub const NONE: DeclItem = DeclItem {
        tag: 0,
        a: 0,
        b: 0,
        c: 0,
    };
}

/// What a service declares at its init (declare-check-commit, docs/plans/runtime-and-services.md R7): the record kinds it owns, the slots and hook
/// sites it takes, the functions it exports and the resources it adds. The items have the shape of the header table's entries of a service module
/// (`KINDS`, `SLOTS`, `HOOKS`, `EXPORTS`, `RESOURCES`: tags 4 to 8 of packages/dkneeds/src/keys.rs), so the runtime compares them with the table
/// (the generated static of an in-crate service, the header table of a module) and a difference means the service FAILED.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Declare {
    /// (table tag, first word, second word, third word): `kind(kind, len)` (and the handler entry of a module), `slot(id, flags, entry)`, `hook(site, entry)`,
    /// `export(name hash, entry)`, `resource(id, limit)`; an unused word is 0.
    pub items: [DeclItem; Declare::MAX],
    pub n: usize,
    /// More declarations than `MAX` were made.
    pub full: bool,
}

impl Declare {
    /// Most declarations of one `init`: the machines service takes 21 hooks and a few slots and exports.
    pub const MAX: usize = 32;
    pub const KIND: u16 = 4;
    pub const SLOT: u16 = 5;
    pub const HOOK: u16 = 6;
    pub const EXPORT: u16 = 7;
    pub const RESOURCE: u16 = 8;
    /// The `ServiceRow` a service file declares (`dk_service_declare_row`): the row's address, low word in `a` and (a 64-bit host) high word in `b`. The header
    /// table has no such entry: `same_as` leaves it out when it compares the declarations with the table.
    pub const ROW: u16 = 9;

    pub const fn new() -> Declare {
        Declare {
            items: [DeclItem::NONE; Declare::MAX],
            n: 0,
            full: false,
        }
    }
    /// One more item; `full` is set when there is no room.
    pub fn add(&mut self, tag: u16, a: u32, b: u32, c: u32) {
        if self.n < Declare::MAX {
            self.items[self.n] = DeclItem { tag, a, b, c };
            self.n += 1;
        } else {
            self.full = true;
        }
    }
    /// A record kind the service owns, with the length of its record (the 8-byte header included).
    pub fn kind(&mut self, kind: u32, len: u32) {
        self.add(Declare::KIND, kind, len, 0);
    }
    pub fn slot(&mut self, slot: u32, flags: u32, entry: u32) {
        self.add(Declare::SLOT, slot, flags, entry);
    }
    pub fn hook(&mut self, site: u32, entry: u32) {
        self.add(Declare::HOOK, site, entry, 0);
    }
    pub fn export(&mut self, name_hash: u32, entry: u32) {
        self.add(Declare::EXPORT, name_hash, entry, 0);
    }
    pub fn resource(&mut self, id: u32, limit: u32) {
        self.add(Declare::RESOURCE, id, limit, 0);
    }
    /// The service file's row.
    pub fn row(&mut self, row: *const ServiceRow) {
        let at = row as usize as u64;
        self.add(Declare::ROW, at as u32, (at >> 32) as u32, 0);
    }
    /// The row of the `ROW` item, if one was declared.
    pub fn row_ptr(&self) -> Option<*const ServiceRow> {
        let mut i = 0;
        while i < self.n {
            let DeclItem { tag, a, b, .. } = self.items[i];
            if tag == Declare::ROW {
                return Some((((b as u64) << 32 | a as u64) as usize) as *const ServiceRow);
            }
            i += 1;
        }
        None
    }
    /// Whether `other` holds the same items (in any order): what a module declared in `init` against what its header table says.
    ///
    /// The `ROW` item of `self` (a service file's own row) has no entry in a header table and is left out.
    pub fn same_as(&self, other: &Declare) -> bool {
        let mut rows = 0;
        let mut r = 0;
        while r < self.n {
            if self.items[r].tag == Declare::ROW {
                rows += 1;
            }
            r += 1;
        }
        if self.full || other.full || self.n - rows != other.n {
            return false;
        }
        let has = |d: &Declare, x: DeclItem| {
            let mut k = 0;
            while k < d.n {
                if d.items[k] == x {
                    return true;
                }
                k += 1;
            }
            false
        };
        let mut i = 0;
        while i < self.n {
            if self.items[i].tag != Declare::ROW && !has(other, self.items[i]) {
                return false;
            }
            i += 1;
        }
        i = 0;
        while i < other.n {
            if !has(self, other.items[i]) {
                return false;
            }
            i += 1;
        }
        true
    }
    /// Whether the declarations are exactly the record kinds `kinds` (kind, length) and nothing else, in any order.
    pub fn matches_kinds(&self, kinds: &[(u32, u32)]) -> bool {
        if self.full || self.n != kinds.len() {
            return false;
        }
        let mut i = 0;
        while i < self.n {
            let DeclItem { tag, a, b, .. } = self.items[i];
            if tag != Declare::KIND {
                return false;
            }
            let mut found = false;
            let mut k = 0;
            while k < kinds.len() {
                if kinds[k].0 == a && kinds[k].1 == b {
                    found = true;
                }
                k += 1;
            }
            if !found {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// A service. Every method has a default that declares nothing; a service implements the ones it owns.
/// The core is passed by reference to every method that may touch it (`declare` and `fits` read nothing but their arguments); a service
/// that a host test drives is generic over `C: CoreApi` and the test passes a `FakeCore`.
#[allow(unused_variables)]
pub trait Service<C: CoreApi> {
    /// The service's declarations (`Declare`): called once at the runtime's init; the runtime compares them with the service's table.
    unsafe fn declare(d: &mut Declare) {}
    /// Whether the registry's table fits this service's state (the walker refuses with `E_NO_REGISTRY` when not).
    unsafe fn fits(inf: *const u32) -> bool {
        true
    }
    /// One step of the validation (`check_step`) for this service's records. Writes nothing.
    unsafe fn check(core: &C, step: u32, c: &CheckCtx) -> i32 {
        OK
    }
    /// Write this service's rows that the old body wrote before its per-record pass (parameters, actions, pages, machines).
    unsafe fn apply(core: &C, c: &ApplyCtx) -> i32 {
        OK
    }
    /// Write one record of this service's kinds, in file order after every `apply` (native pages and effect pages interleave as in the
    /// file). A kind this service does not own: OK.
    unsafe fn apply_record(core: &C, kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        OK
    }
    /// Give back everything module record `owner - 1` filled.
    unsafe fn remove(core: &C, owner: u8) {}

    /// `(limit, used)` of capacity kind `kind` (`proto::CAPK_*`) if this service counts it; a kind two services count is the sum of their counts.
    unsafe fn cap(core: &C, kind: u8) -> Option<(u32, u32)> {
        None
    }
    /// The module record behind a fault site and the id the DSP reported there (`fault::SITE_*`).
    unsafe fn attribute(core: &C, site: u32, id: u32) -> Option<usize> {
        None
    }
    /// The guard's counts for machine type `ty`, if this service owns machine types.
    unsafe fn guard_state(core: &C, ty: u32) -> Option<GuardState> {
        None
    }
    /// Every row of module `rec` is in the DSP.
    unsafe fn on_loaded(core: &C, rec: usize) {}
    /// The service's work of one UI-loop pass in `phase` (`tick_phase`).
    unsafe fn tick(core: &C, phase: u32) {}
    /// Effect `index` (0 ..) with a page, in row order, for DIGIKIT > FX ROUTES, into `out`: `OK`, or `E_NOT_FOUND` past the end (the default: this service has no effects).
    unsafe fn fx_entry(core: &C, index: u32, out: &mut DkrtFx) -> i32 {
        E_NOT_FOUND
    }
    /// The machine type numbers of the machine rows module `owner` (record number, 1 ..) filled, in type order, into `out`; how many (the default: none).
    unsafe fn machine_types_of(core: &C, owner: u8, out: &mut [u8; 8]) -> u32 {
        0
    }

    /// Did module `rec` claim a voice row for machine type `ty`?
    unsafe fn has_voice(core: &C, rec: usize, ty: u32) -> bool {
        false
    }
    /// The ranges and latch mask `init` declared for the voice of type `ty` (`has_voice` is true). `Some(E_*)` or `Some(OK)` if this
    /// service owns the voice.
    unsafe fn declare_voice(
        core: &C,
        rec: usize,
        ty: u32,
        rng: [u32; 8],
        latched: u32,
    ) -> Option<i32> {
        None
    }
    /// The route flags and defaults `init` declared for effect record `ordinal` of module `rec`; `Some(false)`: no such effect.
    unsafe fn declare_fx(
        core: &C,
        rec: usize,
        ordinal: u32,
        flags: Option<u32>,
        par: Option<[u32; 8]>,
    ) -> Option<bool> {
        None
    }

    /// DSP section: the entries of the tables this service owns (called in list order, after the header is validated).
    unsafe fn dsp_check_sections(core: &C, sec: &DspSec) -> i32 {
        OK
    }
    /// DSP section: free rows for this service's entries.
    unsafe fn dsp_check_rows(core: &C, sec: &DspSec) -> i32 {
        OK
    }
    /// DSP section: the machine types of the voices (after every service's rows were checked).
    unsafe fn dsp_check_types(core: &C, sec: &DspSec) -> i32 {
        OK
    }
    /// Refuse a reservation before anything is reserved (`OK` or an `E_*`).
    unsafe fn dsp_admit(core: &C, job: &Job) -> i32 {
        OK
    }
    /// Claim this service's rows for module `rec`.
    unsafe fn dsp_claim(core: &C, c: &mut Claim) {}
    /// Give back what `dsp_claim` took (the rows are cleared).
    unsafe fn dsp_release(core: &C, rec: usize) {}
    /// Free the rows of module `rec` this service published in the DSP; false when a write failed.
    unsafe fn dsp_clear(core: &C, rec: usize) -> bool {
        true
    }
    /// The next word to publish for module `rec`: `want` counts the steps since the first row; a row of this service takes its steps off
    /// `want`, and the row that holds the step returns its write. The word is read from the row only when `write` is set (a step in
    /// flight needs to know that a row exists, not what it writes).
    unsafe fn dsp_row_step(core: &C, rec: usize, want: &mut u32, write: bool) -> Option<RowWrite> {
        None
    }
}

/// One service in the table, as the core sees it: its name, the record kinds it owns and its methods as `extern "C"` thunks. The row is the
/// same type for every service and every core: `ServiceRow::of::<S, C>` builds it from `impl Service<C> for S` with generic thunks, so a row
/// built in another object (a service loaded from the card) has the same layout. A method the service does not implement points at the
/// default (`OK`, nothing). The first argument of every thunk is the core (`*const c_void`, a `&C` of the `C` the row was built for).
/// Results that are `Option` and values that are arrays travel C-shaped: out pointers with a `bool` and pointers to the words.
#[repr(C)]
pub struct ServiceRow {
    /// The name of `[[service]]` in sdk/sdk.toml (the owner a kind's declaration names): `name_len` bytes of UTF-8.
    pub name: *const u8,
    pub name_len: u32,
    /// The record kinds of the `register` section the service owns: `nkinds` pairs of (kind, whole record length with its header), from
    /// `[records.kinds]`.
    pub kinds: *const (u32, u32),
    pub nkinds: u32,
    declare: unsafe extern "C" fn(*const c_void, *mut Declare),
    fits: unsafe extern "C" fn(*const c_void, *const u32) -> bool,
    check: unsafe extern "C" fn(*const c_void, u32, *const CheckCtx) -> i32,
    apply: unsafe extern "C" fn(*const c_void, *const ApplyCtx) -> i32,
    apply_record: unsafe extern "C" fn(*const c_void, u32, *const u8, *const ApplyCtx) -> i32,
    remove: unsafe extern "C" fn(*const c_void, u8),
    cap: unsafe extern "C" fn(*const c_void, u8, *mut u32) -> bool,
    attribute: unsafe extern "C" fn(*const c_void, u32, u32, *mut usize) -> bool,
    guard_state: unsafe extern "C" fn(*const c_void, u32, *mut GuardState) -> bool,
    on_loaded: unsafe extern "C" fn(*const c_void, usize),
    tick: unsafe extern "C" fn(*const c_void, u32),
    fx_entry: unsafe extern "C" fn(*const c_void, u32, *mut DkrtFx) -> i32,
    machine_types_of: unsafe extern "C" fn(*const c_void, u8, *mut u8) -> u32,
    has_voice: unsafe extern "C" fn(*const c_void, usize, u32) -> bool,
    declare_voice:
        unsafe extern "C" fn(*const c_void, usize, u32, *const u32, u32, *mut i32) -> bool,
    declare_fx:
        unsafe extern "C" fn(*const c_void, usize, u32, *const u32, *const u32, *mut bool) -> bool,
    dsp_check_sections: unsafe extern "C" fn(*const c_void, *const DspSec) -> i32,
    dsp_check_rows: unsafe extern "C" fn(*const c_void, *const DspSec) -> i32,
    dsp_check_types: unsafe extern "C" fn(*const c_void, *const DspSec) -> i32,
    dsp_admit: unsafe extern "C" fn(*const c_void, *const Job) -> i32,
    dsp_claim: unsafe extern "C" fn(*const c_void, *mut Claim),
    dsp_release: unsafe extern "C" fn(*const c_void, usize),
    dsp_clear: unsafe extern "C" fn(*const c_void, usize) -> bool,
    dsp_row_step: unsafe extern "C" fn(*const c_void, usize, *mut u32, bool, *mut RowWrite) -> bool,
}

// A row is built from constants and never written.
unsafe impl Sync for ServiceRow {}

/// The thunks of `ServiceRow::of`: each casts the core back to `&C` and calls the method of `S`. A method that returns an `Option` writes the
/// value to its out pointer and returns whether there was one.
mod thunk {
    use super::*;

    pub unsafe extern "C" fn declare<C: CoreApi, S: Service<C>>(_: *const c_void, d: *mut Declare) {
        S::declare(&mut *d)
    }
    pub unsafe extern "C" fn fits<C: CoreApi, S: Service<C>>(
        _: *const c_void,
        inf: *const u32,
    ) -> bool {
        S::fits(inf)
    }
    pub unsafe extern "C" fn check<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        step: u32,
        c: *const CheckCtx,
    ) -> i32 {
        S::check(&*(core as *const C), step, &*c)
    }
    pub unsafe extern "C" fn apply<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        c: *const ApplyCtx,
    ) -> i32 {
        S::apply(&*(core as *const C), &*c)
    }
    pub unsafe extern "C" fn apply_record<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        kind: u32,
        body: *const u8,
        c: *const ApplyCtx,
    ) -> i32 {
        S::apply_record(&*(core as *const C), kind, body, &*c)
    }
    pub unsafe extern "C" fn remove<C: CoreApi, S: Service<C>>(core: *const c_void, owner: u8) {
        S::remove(&*(core as *const C), owner)
    }
    /// `out` points at two words: limit, used.
    pub unsafe extern "C" fn cap<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        kind: u8,
        out: *mut u32,
    ) -> bool {
        match S::cap(&*(core as *const C), kind) {
            Some((l, u)) => {
                *out = l;
                *out.add(1) = u;
                true
            }
            None => false,
        }
    }
    pub unsafe extern "C" fn attribute<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        site: u32,
        id: u32,
        out: *mut usize,
    ) -> bool {
        match S::attribute(&*(core as *const C), site, id) {
            Some(m) => {
                *out = m;
                true
            }
            None => false,
        }
    }
    pub unsafe extern "C" fn guard_state<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        ty: u32,
        out: *mut GuardState,
    ) -> bool {
        match S::guard_state(&*(core as *const C), ty) {
            Some(g) => {
                *out = g;
                true
            }
            None => false,
        }
    }
    pub unsafe extern "C" fn on_loaded<C: CoreApi, S: Service<C>>(core: *const c_void, rec: usize) {
        S::on_loaded(&*(core as *const C), rec)
    }
    pub unsafe extern "C" fn tick<C: CoreApi, S: Service<C>>(core: *const c_void, phase: u32) {
        S::tick(&*(core as *const C), phase)
    }
    pub unsafe extern "C" fn fx_entry<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        index: u32,
        out: *mut DkrtFx,
    ) -> i32 {
        S::fx_entry(&*(core as *const C), index, &mut *out)
    }
    pub unsafe extern "C" fn machine_types_of<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        owner: u8,
        out: *mut u8,
    ) -> u32 {
        S::machine_types_of(&*(core as *const C), owner, &mut *(out as *mut [u8; 8]))
    }
    pub unsafe extern "C" fn has_voice<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
        ty: u32,
    ) -> bool {
        S::has_voice(&*(core as *const C), rec, ty)
    }
    /// `rng` points at eight words; `out` receives the result when the service owns the voice.
    pub unsafe extern "C" fn declare_voice<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
        ty: u32,
        rng: *const u32,
        latched: u32,
        out: *mut i32,
    ) -> bool {
        let mut r = [0u32; 8];
        let mut i = 0;
        while i < 8 {
            r[i] = *rng.add(i);
            i += 1;
        }
        match S::declare_voice(&*(core as *const C), rec, ty, r, latched) {
            Some(v) => {
                *out = v;
                true
            }
            None => false,
        }
    }
    /// `flags` is null for `None`, else a pointer to one word; `par` is null for `None`, else a pointer to eight words.
    pub unsafe extern "C" fn declare_fx<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
        ordinal: u32,
        flags: *const u32,
        par: *const u32,
        out: *mut bool,
    ) -> bool {
        let f = if flags.is_null() { None } else { Some(*flags) };
        let p = if par.is_null() {
            None
        } else {
            let mut r = [0u32; 8];
            let mut i = 0;
            while i < 8 {
                r[i] = *par.add(i);
                i += 1;
            }
            Some(r)
        };
        match S::declare_fx(&*(core as *const C), rec, ordinal, f, p) {
            Some(v) => {
                *out = v;
                true
            }
            None => false,
        }
    }
    pub unsafe extern "C" fn dsp_check_sections<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        sec: *const DspSec,
    ) -> i32 {
        S::dsp_check_sections(&*(core as *const C), &*sec)
    }
    pub unsafe extern "C" fn dsp_check_rows<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        sec: *const DspSec,
    ) -> i32 {
        S::dsp_check_rows(&*(core as *const C), &*sec)
    }
    pub unsafe extern "C" fn dsp_check_types<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        sec: *const DspSec,
    ) -> i32 {
        S::dsp_check_types(&*(core as *const C), &*sec)
    }
    pub unsafe extern "C" fn dsp_admit<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        job: *const Job,
    ) -> i32 {
        S::dsp_admit(&*(core as *const C), &*job)
    }
    pub unsafe extern "C" fn dsp_claim<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        c: *mut Claim,
    ) {
        S::dsp_claim(&*(core as *const C), &mut *c)
    }
    pub unsafe extern "C" fn dsp_release<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
    ) {
        S::dsp_release(&*(core as *const C), rec)
    }
    pub unsafe extern "C" fn dsp_clear<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
    ) -> bool {
        S::dsp_clear(&*(core as *const C), rec)
    }
    pub unsafe extern "C" fn dsp_row_step<C: CoreApi, S: Service<C>>(
        core: *const c_void,
        rec: usize,
        want: *mut u32,
        write: bool,
        out: *mut RowWrite,
    ) -> bool {
        match S::dsp_row_step(&*(core as *const C), rec, &mut *want, write) {
            Some(w) => {
                *out = w;
                true
            }
            None => false,
        }
    }
}

impl ServiceRow {
    /// The row of service `S` over core `C` that owns `kinds`.
    pub const fn of<S: Service<C>, C: CoreApi + 'static>(
        name: &'static str,
        kinds: &'static [(u32, u32)],
    ) -> ServiceRow {
        ServiceRow {
            name: name.as_ptr(),
            name_len: name.len() as u32,
            kinds: kinds.as_ptr(),
            nkinds: kinds.len() as u32,
            declare: thunk::declare::<C, S>,
            fits: thunk::fits::<C, S>,
            check: thunk::check::<C, S>,
            apply: thunk::apply::<C, S>,
            apply_record: thunk::apply_record::<C, S>,
            remove: thunk::remove::<C, S>,
            cap: thunk::cap::<C, S>,
            attribute: thunk::attribute::<C, S>,
            guard_state: thunk::guard_state::<C, S>,
            on_loaded: thunk::on_loaded::<C, S>,
            tick: thunk::tick::<C, S>,
            fx_entry: thunk::fx_entry::<C, S>,
            machine_types_of: thunk::machine_types_of::<C, S>,
            has_voice: thunk::has_voice::<C, S>,
            declare_voice: thunk::declare_voice::<C, S>,
            declare_fx: thunk::declare_fx::<C, S>,
            dsp_check_sections: thunk::dsp_check_sections::<C, S>,
            dsp_check_rows: thunk::dsp_check_rows::<C, S>,
            dsp_check_types: thunk::dsp_check_types::<C, S>,
            dsp_admit: thunk::dsp_admit::<C, S>,
            dsp_claim: thunk::dsp_claim::<C, S>,
            dsp_release: thunk::dsp_release::<C, S>,
            dsp_clear: thunk::dsp_clear::<C, S>,
            dsp_row_step: thunk::dsp_row_step::<C, S>,
        }
    }
    /// The service's name.
    pub fn name(&self) -> &'static str {
        unsafe {
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                self.name,
                self.name_len as usize,
            ))
        }
    }
    /// The record kinds the service owns: (kind, record length).
    pub fn kinds(&self) -> &'static [(u32, u32)] {
        unsafe { core::slice::from_raw_parts(self.kinds, self.nkinds as usize) }
    }
}

/// The row pointers of a table, in a static (a raw pointer is not `Sync`; the rows are read-only).
#[repr(transparent)]
pub struct RowPtrs<const N: usize>(pub [*const ServiceRow; N]);

unsafe impl<const N: usize> Sync for RowPtrs<N> {}

impl<const N: usize> RowPtrs<N> {
    /// The pointers to `rows`, in order.
    pub const fn of(rows: &'static [ServiceRow; N]) -> RowPtrs<N> {
        let mut a = [core::ptr::null::<ServiceRow>(); N];
        let mut i = 0;
        while i < N {
            a[i] = &rows[i] as *const ServiceRow;
            i += 1;
        }
        RowPtrs(a)
    }
}

/// The pointers to `rows` followed by null pointers up to `M` entries (`M >= N`): the table of a core that adds the rows of loaded service files after its own.
pub const fn rows_with_tail<const N: usize, const M: usize>(
    rows: &'static [ServiceRow; N],
) -> [*const ServiceRow; M] {
    let mut a = [core::ptr::null::<ServiceRow>(); M];
    let mut i = 0;
    while i < N {
        a[i] = &rows[i] as *const ServiceRow;
        i += 1;
    }
    a
}

/// The table of services, in the order the core calls them: the walks below run the rows in table order (removal runs the other way). It is the
/// same type for every core; `core` is the `&C` that each row's thunks cast back.
#[derive(Clone, Copy)]
pub struct ServiceTable {
    pub rows: &'static [*const ServiceRow],
    /// Bit `i` set: row `i` FAILED its declare check (`check_declarations`); its kinds are unknown to `owner` and `len`.
    pub failed: u32,
    /// The core every call passes to the service (`&REAL_CORE` in the runtime's generated table).
    pub core: *const c_void,
}

// A table is read-only data (`rows` and `core` point at statics).
unsafe impl Sync for ServiceTable {}

impl ServiceTable {
    /// A table over `rows` that passes `core` to every method.
    #[inline]
    pub const fn new<C: CoreApi + 'static>(
        rows: &'static [*const ServiceRow],
        core: &'static C,
    ) -> ServiceTable {
        ServiceTable {
            rows,
            failed: 0,
            core: core as *const C as *const c_void,
        }
    }
    #[inline(always)]
    fn row(&self, i: usize) -> &'static ServiceRow {
        unsafe { &*self.rows[i] }
    }
    /// Declare-check-commit of the in-crate services: each row's `declare` against its table (the generated static `kinds`). Returns the mask of the
    /// rows whose declarations differ (bit `i`: row `i`); the caller keeps it in the table it hands out (`failed`).
    #[inline]
    pub unsafe fn check_declarations(&self) -> u32 {
        let mut failed = 0u32;
        let mut i = 0;
        while i < self.rows.len() && i < 32 {
            let mut d = Declare::new();
            (self.row(i).declare)(self.core, &mut d);
            if !d.matches_kinds(self.row(i).kinds()) {
                failed |= 1 << i;
            }
            i += 1;
        }
        failed
    }
    /// The row that owns record kind `kind`.
    #[inline]
    pub fn owner(&self, kind: u32) -> Option<&'static ServiceRow> {
        let mut i = 0;
        while i < self.rows.len() {
            if i < 32 && self.failed & (1 << i) != 0 {
                i += 1;
                continue;
            }
            let kinds = self.row(i).kinds();
            let mut k = 0;
            while k < kinds.len() {
                if kinds[k].0 == kind {
                    return Some(self.row(i));
                }
                k += 1;
            }
            i += 1;
        }
        None
    }
    /// The length of a record of `kind` (the whole record, header included); `None` when no service owns the kind.
    #[inline]
    pub fn len(&self, kind: u32) -> Option<u32> {
        let mut i = 0;
        while i < self.rows.len() {
            if i < 32 && self.failed & (1 << i) != 0 {
                i += 1;
                continue;
            }
            let kinds = self.row(i).kinds();
            let mut k = 0;
            while k < kinds.len() {
                if kinds[k].0 == kind {
                    return Some(kinds[k].1);
                }
                k += 1;
            }
            i += 1;
        }
        None
    }
    #[inline]
    pub unsafe fn fits(&self, inf: *const u32) -> bool {
        let mut i = 0;
        while i < self.rows.len() {
            if !(self.row(i).fits)(self.core, inf) {
                return false;
            }
            i += 1;
        }
        true
    }
    /// One step of the validation over the services in table order; the first failure.
    #[inline]
    pub unsafe fn check(&self, step: u32, c: &CheckCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).check)(self.core, step, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn apply(&self, c: &ApplyCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).apply)(self.core, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn apply_record(&self, kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).apply_record)(self.core, kind, body, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    /// Last service first.
    #[inline]
    pub unsafe fn remove(&self, owner: u8) {
        let mut i = self.rows.len();
        while i > 0 {
            i -= 1;
            (self.row(i).remove)(self.core, owner);
        }
    }
    /// `(limit, used)` of capacity kind `kind`: the sum of the counts of the services that count it; `None` when none does.
    #[inline]
    pub unsafe fn cap(&self, kind: u8) -> Option<(u32, u32)> {
        let mut r: Option<(u32, u32)> = None;
        let mut i = 0;
        while i < self.rows.len() {
            let mut lu = [0u32; 2];
            if (self.row(i).cap)(self.core, kind, lu.as_mut_ptr()) {
                let (l, u) = (lu[0], lu[1]);
                r = Some(match r {
                    Some((a, b)) => (a + l, b + u),
                    None => (l, u),
                });
            }
            i += 1;
        }
        r
    }
    #[inline]
    pub unsafe fn attribute(&self, site: u32, id: u32) -> Option<usize> {
        let mut i = 0;
        while i < self.rows.len() {
            let mut m = 0usize;
            if (self.row(i).attribute)(self.core, site, id, &mut m) {
                return Some(m);
            }
            i += 1;
        }
        None
    }
    #[inline]
    pub unsafe fn guard_state(&self, ty: u32) -> Option<GuardState> {
        let mut i = 0;
        while i < self.rows.len() {
            let mut g = GuardState {
                faults: 0,
                disabled: false,
            };
            if (self.row(i).guard_state)(self.core, ty, &mut g) {
                return Some(g);
            }
            i += 1;
        }
        None
    }
    #[inline]
    pub unsafe fn on_loaded(&self, rec: usize) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.row(i).on_loaded)(self.core, rec);
            i += 1;
        }
    }
    /// The machine type numbers of the rows module `owner` filled, from the first service that has any (the machines service file); 0 when none is loaded.
    #[inline]
    pub unsafe fn machine_types_of(&self, owner: u8, out: &mut [u8; 8]) -> usize {
        let mut i = 0;
        while i < self.rows.len() {
            let n = (self.row(i).machine_types_of)(self.core, owner, out.as_mut_ptr());
            if n != 0 {
                return n as usize;
            }
            i += 1;
        }
        0
    }
    #[inline]
    pub unsafe fn tick(&self, phase: u32) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.row(i).tick)(self.core, phase);
            i += 1;
        }
    }
    /// The `index`th effect with a page, from the first service that has one; `E_NOT_FOUND` when no service has effects (the fx service file is not loaded) or past the end.
    #[inline]
    pub unsafe fn fx_entry(&self, index: u32, out: &mut DkrtFx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            if (self.row(i).fx_entry)(self.core, index, out) == OK {
                return OK;
            }
            i += 1;
        }
        E_NOT_FOUND
    }
    #[inline]
    pub unsafe fn has_voice(&self, rec: usize, ty: u32) -> bool {
        let mut i = 0;
        while i < self.rows.len() {
            if (self.row(i).has_voice)(self.core, rec, ty) {
                return true;
            }
            i += 1;
        }
        false
    }
    #[inline]
    pub unsafe fn declare_voice(
        &self,
        rec: usize,
        ty: u32,
        rng: [u32; 8],
        latched: u32,
    ) -> Option<i32> {
        let mut i = 0;
        while i < self.rows.len() {
            let mut r = 0i32;
            if (self.row(i).declare_voice)(self.core, rec, ty, rng.as_ptr(), latched, &mut r) {
                return Some(r);
            }
            i += 1;
        }
        None
    }
    #[inline]
    pub unsafe fn declare_fx(
        &self,
        rec: usize,
        ordinal: u32,
        flags: Option<u32>,
        par: Option<[u32; 8]>,
    ) -> Option<bool> {
        let fl = flags.unwrap_or(0);
        let fp = if flags.is_some() {
            &fl as *const u32
        } else {
            core::ptr::null()
        };
        let pv = par.unwrap_or([0; 8]);
        let pp = if par.is_some() {
            pv.as_ptr()
        } else {
            core::ptr::null()
        };
        let mut i = 0;
        while i < self.rows.len() {
            let mut r = false;
            if (self.row(i).declare_fx)(self.core, rec, ordinal, fp, pp, &mut r) {
                return Some(r);
            }
            i += 1;
        }
        None
    }
    #[inline]
    pub unsafe fn dsp_check_sections(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).dsp_check_sections)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn dsp_check_rows(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).dsp_check_rows)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn dsp_check_types(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).dsp_check_types)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn dsp_admit(&self, job: &Job) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.row(i).dsp_admit)(self.core, job);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    #[inline]
    pub unsafe fn dsp_claim(&self, c: &mut Claim) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.row(i).dsp_claim)(self.core, c);
            i += 1;
        }
    }
    #[inline]
    pub unsafe fn dsp_release(&self, rec: usize) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.row(i).dsp_release)(self.core, rec);
            i += 1;
        }
    }
    #[inline]
    pub unsafe fn dsp_clear(&self, rec: usize) -> bool {
        let mut ok = true;
        let mut i = 0;
        while i < self.rows.len() {
            ok = (self.row(i).dsp_clear)(self.core, rec) && ok;
            i += 1;
        }
        ok
    }
    #[inline]
    pub unsafe fn dsp_row_step(&self, rec: usize, want: &mut u32, write: bool) -> Option<RowWrite> {
        let mut i = 0;
        while i < self.rows.len() {
            let mut w = RowWrite {
                win: 0,
                word: 0,
                value: 0,
                published: None,
                last: false,
                has_undo: false,
                undo: 0,
            };
            if (self.row(i).dsp_row_step)(self.core, rec, want, write, &mut w) {
                return Some(w);
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_is_an_item_of_its_own_that_the_header_table_cannot_hold() {
        let row = 0x1234usize as *const ServiceRow;
        let mut d = Declare::new();
        d.kind(90, 24);
        assert_eq!(d.row_ptr(), None);
        d.row(row);
        assert_eq!(d.n, 2);
        assert_eq!(d.row_ptr(), Some(row));
        // the table has the kind alone: the declarations with a row are the same as it
        let mut table = Declare::new();
        table.kind(90, 24);
        assert!(d.same_as(&table));
        // a table that has more, or another kind, is not the same
        let mut more = table;
        more.slot(1, 0, 0);
        assert!(!d.same_as(&more));
        let mut other = Declare::new();
        other.kind(91, 24);
        assert!(!d.same_as(&other));
        // and the walk of a static service's kinds does not take a row for a kind
        assert!(!d.matches_kinds(&[(90, 24)]));
    }

    #[test]
    fn the_fingerprint_is_not_zero_and_depends_on_the_files_hash() {
        assert_ne!(SVC_ABI, 0);
        assert_ne!(crate::gen_svc_abi::FILES_HASH, 0);
    }
}
