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
use dkrt_sdk::*;

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
pub struct CheckCtx {
    pub reg: *const u8,
    pub image_size: u32,
    /// The words of `dynreg_info` (not null).
    pub inf: *const u32,
}

/// What the registry walker gives `apply`: the (checked) section of module record `owner - 1`, loaded at `base`.
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
pub struct Claim<'a> {
    pub rec: usize,
    pub job: &'a Job,
    pub base: u32,
    pub byte_shift: u32,
    pub state_at: u32,
}

/// One word the DSP loader writes to publish a row: window, word index and value. The loader collects the steps of one row into a link `Sequence`.
/// `published` is set once the write is accepted, when from then on the row may have words in the DSP (so a failure must clear them). `last` ends the
/// row (its type or pc word, the one that makes it visible). `undo` is the word that takes a step back when the sequence fails.
pub struct RowWrite {
    pub win: u32,
    pub word: u32,
    pub value: u32,
    pub published: Option<&'static mut u8>,
    pub last: bool,
    pub undo: Option<u32>,
}

/// What the guard counted for a machine type (`MODULES` reports it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GuardState {
    pub faults: u32,
    pub disabled: bool,
}

/// What a service declares at its init (declare-check-commit, docs/plans/runtime-and-services.md R7): the record kinds it owns, the slots and hook
/// sites it takes, the functions it exports and the resources it adds. The items have the shape of the header table's entries of a service module
/// (`KINDS`, `SLOTS`, `HOOKS`, `EXPORTS`, `RESOURCES`: tags 4 to 8 of packages/dkneeds/src/keys.rs), so the runtime compares them with the table
/// (the generated static of an in-crate service, the header table of a module) and a difference means the service FAILED.
#[derive(Clone, Copy)]
pub struct Declare {
    /// (table tag, first word, second word, third word): `kind(kind, len)` (and the handler entry of a module), `slot(id, flags)`, `hook(site, entry)`,
    /// `export(name hash, entry)`, `resource(id, limit)`; an unused word is 0.
    pub items: [(u16, u32, u32, u32); Declare::MAX],
    pub n: usize,
    /// More declarations than `MAX` were made.
    pub full: bool,
}

impl Declare {
    pub const MAX: usize = 12;
    pub const KIND: u16 = 4;
    pub const SLOT: u16 = 5;
    pub const HOOK: u16 = 6;
    pub const EXPORT: u16 = 7;
    pub const RESOURCE: u16 = 8;

    pub const fn new() -> Declare {
        Declare {
            items: [(0, 0, 0, 0); Declare::MAX],
            n: 0,
            full: false,
        }
    }
    /// One more item; `full` is set when there is no room.
    pub fn add(&mut self, tag: u16, a: u32, b: u32, c: u32) {
        if self.n < Declare::MAX {
            self.items[self.n] = (tag, a, b, c);
            self.n += 1;
        } else {
            self.full = true;
        }
    }
    /// A record kind the service owns, with the length of its record (the 8-byte header included).
    pub fn kind(&mut self, kind: u32, len: u32) {
        self.add(Declare::KIND, kind, len, 0);
    }
    pub fn slot(&mut self, slot: u32, flags: u32) {
        self.add(Declare::SLOT, slot, flags, 0);
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
    /// Whether `other` holds the same items (in any order): what a module declared in `init` against what its header table says.
    pub fn same_as(&self, other: &Declare) -> bool {
        if self.full || other.full || self.n != other.n {
            return false;
        }
        let has = |d: &Declare, x: (u16, u32, u32, u32)| {
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
            if !has(other, self.items[i]) || !has(self, other.items[i]) {
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
            let (tag, a, b, _) = self.items[i];
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

/// One in-crate service in the static table: its name, the record kinds it owns and its methods as function pointers. `ServiceRow::of`
/// builds it from the `impl Service`; a method the service does not implement points at the default (`OK`, `None`).
pub struct ServiceRow<C: CoreApi + 'static> {
    /// The name of `[[service]]` in sdk/sdk.toml (the owner a kind's declaration names).
    pub name: &'static str,
    /// The record kinds of the `register` section the service owns: (kind, whole record length with its header), from `[records.kinds]`.
    pub kinds: &'static [(u32, u32)],
    declare: unsafe fn(&mut Declare),
    fits: unsafe fn(*const u32) -> bool,
    check: unsafe fn(&C, u32, &CheckCtx) -> i32,
    apply: unsafe fn(&C, &ApplyCtx) -> i32,
    apply_record: unsafe fn(&C, u32, *const u8, &ApplyCtx) -> i32,
    remove: unsafe fn(&C, u8),
    cap: unsafe fn(&C, u8) -> Option<(u32, u32)>,
    attribute: unsafe fn(&C, u32, u32) -> Option<usize>,
    guard_state: unsafe fn(&C, u32) -> Option<GuardState>,
    on_loaded: unsafe fn(&C, usize),
    tick: unsafe fn(&C, u32),
    has_voice: unsafe fn(&C, usize, u32) -> bool,
    declare_voice: unsafe fn(&C, usize, u32, [u32; 8], u32) -> Option<i32>,
    declare_fx: unsafe fn(&C, usize, u32, Option<u32>, Option<[u32; 8]>) -> Option<bool>,
    dsp_check_sections: unsafe fn(&C, &DspSec) -> i32,
    dsp_check_rows: unsafe fn(&C, &DspSec) -> i32,
    dsp_check_types: unsafe fn(&C, &DspSec) -> i32,
    dsp_admit: unsafe fn(&C, &Job) -> i32,
    dsp_claim: unsafe fn(&C, &mut Claim),
    dsp_release: unsafe fn(&C, usize),
    dsp_clear: unsafe fn(&C, usize) -> bool,
    dsp_row_step: unsafe fn(&C, usize, &mut u32, bool) -> Option<RowWrite>,
}

impl<C: CoreApi + 'static> ServiceRow<C> {
    /// The row of service `S` that owns `kinds`.
    pub const fn of<S: Service<C>>(
        name: &'static str,
        kinds: &'static [(u32, u32)],
    ) -> ServiceRow<C> {
        ServiceRow {
            name,
            kinds,
            declare: S::declare,
            fits: S::fits,
            check: S::check,
            apply: S::apply,
            apply_record: S::apply_record,
            remove: S::remove,
            cap: S::cap,
            attribute: S::attribute,
            guard_state: S::guard_state,
            on_loaded: S::on_loaded,
            tick: S::tick,
            has_voice: S::has_voice,
            declare_voice: S::declare_voice,
            declare_fx: S::declare_fx,
            dsp_check_sections: S::dsp_check_sections,
            dsp_check_rows: S::dsp_check_rows,
            dsp_check_types: S::dsp_check_types,
            dsp_admit: S::dsp_admit,
            dsp_claim: S::dsp_claim,
            dsp_release: S::dsp_release,
            dsp_clear: S::dsp_clear,
            dsp_row_step: S::dsp_row_step,
        }
    }
}

/// The static table of services, in the order the core calls them: the walks below run the rows in table order (removal runs the other way).
pub struct ServiceTable<C: CoreApi + 'static> {
    pub rows: &'static [ServiceRow<C>],
    /// Bit `i` set: row `i` FAILED its declare check (`check_declarations`); its kinds are unknown to `owner` and `len`.
    pub failed: u32,
    /// The core every call passes to the service (`&REAL_CORE` in the runtime's generated table).
    pub core: &'static C,
}

impl<C: CoreApi + 'static> Clone for ServiceTable<C> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C: CoreApi + 'static> Copy for ServiceTable<C> {}

impl<C: CoreApi + 'static> ServiceTable<C> {
    /// Declare-check-commit of the in-crate services: each row's `declare` against its table (the generated static `kinds`). Returns the mask of the
    /// rows whose declarations differ (bit `i`: row `i`); the caller keeps it in the table it hands out (`failed`).
    pub unsafe fn check_declarations(&self) -> u32 {
        let mut failed = 0u32;
        let mut i = 0;
        while i < self.rows.len() && i < 32 {
            let mut d = Declare::new();
            (self.rows[i].declare)(&mut d);
            if !d.matches_kinds(self.rows[i].kinds) {
                failed |= 1 << i;
            }
            i += 1;
        }
        failed
    }
    /// The row that owns record kind `kind`.
    pub fn owner(&self, kind: u32) -> Option<&'static ServiceRow<C>> {
        let mut i = 0;
        while i < self.rows.len() {
            if i < 32 && self.failed & (1 << i) != 0 {
                i += 1;
                continue;
            }
            let mut k = 0;
            while k < self.rows[i].kinds.len() {
                if self.rows[i].kinds[k].0 == kind {
                    return Some(&self.rows[i]);
                }
                k += 1;
            }
            i += 1;
        }
        None
    }
    /// The length of a record of `kind` (the whole record, header included); `None` when no service owns the kind.
    pub fn len(&self, kind: u32) -> Option<u32> {
        let mut i = 0;
        while i < self.rows.len() {
            if i < 32 && self.failed & (1 << i) != 0 {
                i += 1;
                continue;
            }
            let mut k = 0;
            while k < self.rows[i].kinds.len() {
                if self.rows[i].kinds[k].0 == kind {
                    return Some(self.rows[i].kinds[k].1);
                }
                k += 1;
            }
            i += 1;
        }
        None
    }
    pub unsafe fn fits(&self, inf: *const u32) -> bool {
        let mut i = 0;
        while i < self.rows.len() {
            if !(self.rows[i].fits)(inf) {
                return false;
            }
            i += 1;
        }
        true
    }
    /// One step of the validation over the services in table order; the first failure.
    pub unsafe fn check(&self, step: u32, c: &CheckCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].check)(self.core, step, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn apply(&self, c: &ApplyCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].apply)(self.core, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn apply_record(&self, kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].apply_record)(self.core, kind, body, c);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    /// Last service first.
    pub unsafe fn remove(&self, owner: u8) {
        let mut i = self.rows.len();
        while i > 0 {
            i -= 1;
            (self.rows[i].remove)(self.core, owner);
        }
    }
    /// `(limit, used)` of capacity kind `kind`: the sum of the counts of the services that count it; `None` when none does.
    pub unsafe fn cap(&self, kind: u8) -> Option<(u32, u32)> {
        let mut r: Option<(u32, u32)> = None;
        let mut i = 0;
        while i < self.rows.len() {
            if let Some((l, u)) = (self.rows[i].cap)(self.core, kind) {
                r = Some(match r {
                    Some((a, b)) => (a + l, b + u),
                    None => (l, u),
                });
            }
            i += 1;
        }
        r
    }
    pub unsafe fn attribute(&self, site: u32, id: u32) -> Option<usize> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some(m) = (self.rows[i].attribute)(self.core, site, id) {
                return Some(m);
            }
            i += 1;
        }
        None
    }
    pub unsafe fn guard_state(&self, ty: u32) -> Option<GuardState> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some(g) = (self.rows[i].guard_state)(self.core, ty) {
                return Some(g);
            }
            i += 1;
        }
        None
    }
    pub unsafe fn on_loaded(&self, rec: usize) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.rows[i].on_loaded)(self.core, rec);
            i += 1;
        }
    }
    pub unsafe fn tick(&self, phase: u32) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.rows[i].tick)(self.core, phase);
            i += 1;
        }
    }
    pub unsafe fn has_voice(&self, rec: usize, ty: u32) -> bool {
        let mut i = 0;
        while i < self.rows.len() {
            if (self.rows[i].has_voice)(self.core, rec, ty) {
                return true;
            }
            i += 1;
        }
        false
    }
    pub unsafe fn declare_voice(
        &self,
        rec: usize,
        ty: u32,
        rng: [u32; 8],
        latched: u32,
    ) -> Option<i32> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some(r) = (self.rows[i].declare_voice)(self.core, rec, ty, rng, latched) {
                return Some(r);
            }
            i += 1;
        }
        None
    }
    pub unsafe fn declare_fx(
        &self,
        rec: usize,
        ordinal: u32,
        flags: Option<u32>,
        par: Option<[u32; 8]>,
    ) -> Option<bool> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some(r) = (self.rows[i].declare_fx)(self.core, rec, ordinal, flags, par) {
                return Some(r);
            }
            i += 1;
        }
        None
    }
    pub unsafe fn dsp_check_sections(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].dsp_check_sections)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn dsp_check_rows(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].dsp_check_rows)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn dsp_check_types(&self, sec: &DspSec) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].dsp_check_types)(self.core, sec);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn dsp_admit(&self, job: &Job) -> i32 {
        let mut i = 0;
        while i < self.rows.len() {
            let r = (self.rows[i].dsp_admit)(self.core, job);
            if r != OK {
                return r;
            }
            i += 1;
        }
        OK
    }
    pub unsafe fn dsp_claim(&self, c: &mut Claim) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.rows[i].dsp_claim)(self.core, c);
            i += 1;
        }
    }
    pub unsafe fn dsp_release(&self, rec: usize) {
        let mut i = 0;
        while i < self.rows.len() {
            (self.rows[i].dsp_release)(self.core, rec);
            i += 1;
        }
    }
    pub unsafe fn dsp_clear(&self, rec: usize) -> bool {
        let mut ok = true;
        let mut i = 0;
        while i < self.rows.len() {
            ok = (self.rows[i].dsp_clear)(self.core, rec) && ok;
            i += 1;
        }
        ok
    }
    pub unsafe fn dsp_row_step(&self, rec: usize, want: &mut u32, write: bool) -> Option<RowWrite> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some(w) = (self.rows[i].dsp_row_step)(self.core, rec, want, write) {
                return Some(w);
            }
            i += 1;
        }
        None
    }
}
