//! The boot planner (docs/plans/runtime-and-services.md R7): a pure function over the header tables of the selected modules. No module code runs and
//! nothing is written; it is the answer to "may this selection load, and in which order" before the first load.
//!
//! Input: for each module of the selection its header table (`dkneeds::keys::Keys`), or `None` when the file could not be read or has no table (the
//! loader reports that error itself; the planner accepts the module). Output (`Plan`): one `Verdict` per module (`dkrt_abi::plan`: accepted, or refused with a
//! `Reason`) and the order to load them in.
//!
//! - Order: a module that needs a service another module exports (`NEEDS`, `EXPORTS`), or whose keys are for a kind another module owns (`KEYS`, `KINDS`),
//!   loads after that module; otherwise the order is the selection's (the module id's position). A cycle refuses its modules (`NeedMissing`).
//! - Needs: a hard need of a service that is neither one of the runtime's (`[handle] services`) nor exported by an earlier module is refused (`NeedMissing`);
//!   a need of a newer version than the runtime is refused (`NeedVersion`). A service of the image (core/ui and the like) that is not there is the
//!   loader's `E_IMPORT`, with the missing module's name: the planner does not know the image.
//! - Exclusive: one owner for each kind (a service of the table, or a module's `KINDS`), slot and hook site; keys of a *generic* kind (`[records.kinds]`
//!   `generic`) are unique across the selection. The keys of the typed kinds (machines, effects) keep their own rules: a machine key twice takes a fresh type
//!   number, an effect id twice is `E_FX_ID_DUP` at the load.
//! - Fit: the module's `USES` (machine, parameter and page rows, the record block, the DSP cost) against the capacity the earlier accepted modules left, with the
//!   same function the menu uses (`dkrt_fit::fits_now` and `take`). The DSP load is `max(per-voice machine cost) * voices + sum(effect costs)` against the
//!   headroom of the device profile; the capacity's DSP entry keeps the running maximum and the sum. A module that takes the load over the headroom is
//!   refused with `Reason::Fit(CAPK_DSP_COST)`.
use dkneeds::keys::{name_hash, tag, Keys};
use dkrt_abi::plan::{Reason, Verdict};
use dkrt_abi::service::ServiceTable;
use dkrt_fit as fit;
use dkrt_sdk::{DkrtCap, CAP_MAX, SEL_MAX};

/// What the planner is told about the runtime it plans for.
pub struct Env<'a> {
    pub services: ServiceTable,
    /// The runtime version, packed major << 16 | minor << 8 | patch.
    pub runtime: u32,
    /// The capacity before the selection loads (`dkrt_capacity`).
    pub caps: &'a [DkrtCap],
}

/// The plan of a selection of `n` modules (at most `SEL_MAX`).
pub struct Plan {
    pub n: usize,
    /// The verdict of each module, by selection index.
    pub verdicts: [Verdict; SEL_MAX],
    /// The selection indices in load order (a refused module is in it too: the loader reports its verdict).
    pub order: [u8; SEL_MAX],
    /// The DSP load of the accepted modules (`used`) and the largest per-voice machine cost among them (`largest`): the DSP entry of the capacity after the plan.
    pub dsp_load: (u32, u32),
}

fn is_runtime_service(h: u32, services: &ServiceTable) -> bool {
    if h == name_hash("runtime") {
        return true;
    }
    let mut i = 0;
    while i < services.rows.len() {
        if name_hash(services.rows[i].name) == h {
            return true;
        }
        i += 1;
    }
    let mut k = 0;
    while k < dkrt_sdk::records::HANDLE_SERVICES.len() {
        if name_hash(dkrt_sdk::records::HANDLE_SERVICES[k]) == h {
            return true;
        }
        k += 1;
    }
    false
}

/// Whether item `(first word == w)` is in entry `t` (an index loop: the iterator forms are code of their own for each closure).
fn has_word(t: &Keys, tg: u16, w: u32) -> bool {
    let mut i = 0;
    while let Some(x) = t.words(tg, i) {
        if x.0 == w {
            return true;
        }
        i += 1;
    }
    false
}

fn exports(t: &Keys, h: u32) -> bool {
    has_word(t, tag::EXPORTS, h)
}

fn provides_kind(t: &Keys, kind: u32) -> bool {
    has_word(t, tag::KINDS, kind)
}

/// The first word that entry `a` and entry `b` of the two tables share (slots, hook sites).
fn shared(a: &Keys, b: &Keys, tg: u16) -> Option<u32> {
    let mut i = 0;
    while let Some(x) = a.words(tg, i) {
        if has_word(b, tg, x.0) {
            return Some(x.0);
        }
        i += 1;
    }
    None
}

fn is_generic(kind: u32) -> bool {
    let mut i = 0;
    while i < dkrt_sdk::records::GENERIC_KINDS.len() {
        if dkrt_sdk::records::GENERIC_KINDS[i] == kind {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether module `j` has to load after module `i`: it needs a service `i` exports, or has keys for a kind `i` owns.
fn after(j: &Keys, i: &Keys) -> bool {
    let mut k = 0;
    while let Some(n) = j.words(tag::NEEDS, k) {
        if n.2 & 1 != 0 && exports(i, n.0) {
            return true;
        }
        k += 1;
    }
    let mut g = 0;
    while let Some((kind, _)) = j.group(g) {
        if provides_kind(i, kind) {
            return true;
        }
        g += 1;
    }
    false
}

/// The load order: the selection's, moved only as far as the dependencies need (the smallest unplaced index whose providers are placed comes next). The
/// modules of a cycle follow in selection order and are not `placed`.
fn order_of(mods: &[Option<Keys>], order: &mut [u8; SEL_MAX], cyclic: &mut [bool; SEL_MAX]) {
    let n = mods.len();
    let mut placed = [false; SEL_MAX];
    let mut at = 0;
    while at < n {
        let mut pick = None;
        let mut j = 0;
        while j < n && pick.is_none() {
            if !placed[j] {
                let mut ready = true;
                if let Some(tj) = &mods[j] {
                    let mut i = 0;
                    while i < n {
                        if i != j && !placed[i] {
                            if let Some(ti) = &mods[i] {
                                if after(tj, ti) {
                                    ready = false;
                                }
                            }
                        }
                        i += 1;
                    }
                }
                if ready {
                    pick = Some(j);
                }
            }
            j += 1;
        }
        let j = match pick {
            Some(j) => j,
            None => {
                // a cycle: the rest in selection order
                let mut r = 0;
                while r < n {
                    if !placed[r] {
                        cyclic[r] = true;
                        placed[r] = true;
                        order[at] = r as u8;
                        at += 1;
                    }
                    r += 1;
                }
                return;
            }
        };
        placed[j] = true;
        order[at] = j as u8;
        at += 1;
    }
}

// ---- the boot's use: the tables are copied while `assign::plan` reads each file once ---------------------------------------------

use core::ptr::{addr_of, addr_of_mut};

/// The most bytes of a header table the boot planner keeps (a module with eight machines, three effects and nine needs has about 260; a larger table is not planned and the loader decides).
const TABLE_MAX: usize = 384;
static mut TABLES: [[u8; TABLE_MAX]; SEL_MAX] = [[0; TABLE_MAX]; SEL_MAX];
/// Bytes of the table kept for selection index `k`; 0: none (the file is missing, torn, of the old format or has a larger table).
static mut TABLE_LEN: [usize; SEL_MAX] = [0; SEL_MAX];

/// The DSP load of the accepted modules of the last plan: (load, largest per-voice cost). What `dkrt_capacity` reports as used and largest of `CAPK_DSP_COST`;
/// the module set is fixed per session, so the plan is the load.
static mut PLANNED_DSP: (u32, u32) = (0, 0);

pub unsafe fn planned_dsp() -> (u32, u32) {
    *addr_of!(PLANNED_DSP)
}

/// Forget the tables (the start of a boot's planning).
pub unsafe fn reset() {
    *addr_of_mut!(TABLE_LEN) = [0; SEL_MAX];
    *addr_of_mut!(PLANNED_DSP) = (0, 0);
}

/// Keep the header table of the `.dkm` `file` (the whole file in the staging buffer) for selection index `k`.
pub unsafe fn capture(k: usize, file: &[u8]) {
    if k >= SEL_MAX || file.len() < dkneeds::HEADER as usize {
        return;
    }
    let be = |at: usize| u32::from_be_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]) as usize;
    let (off, len) = (be(dkneeds::keys::H_KEYS_OFF as usize), be(dkneeds::keys::H_KEYS_SIZE as usize));
    if len == 0 || len > TABLE_MAX || off > file.len() || len > file.len() - off {
        return;
    }
    core::ptr::copy_nonoverlapping(file.as_ptr().add(off), (addr_of_mut!(TABLES) as *mut u8).add(k * TABLE_MAX), len);
    (*addr_of_mut!(TABLE_LEN))[k] = len;
}

/// Plan the selection `count` modules long from the tables `capture` kept.
pub unsafe fn run(count: usize) -> Plan {
    let mut caps = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let k = crate::dkrt_capacity(caps.as_mut_ptr(), CAP_MAX as u32) as usize;
    // the report counts the selection's paths as used from the start of the boot (`boot::selected`); the plan counts them one by one as it takes the modules
    let mut c = 0;
    while c < k {
        if caps[c].kind == dkrt_sdk::proto::CAPK_SELECTION as u32 {
            caps[c].used = 0;
            caps[c].largest = caps[c].limit;
        }
        // the plan starts with nothing loaded (the report counts the plan's own result from its end)
        if caps[c].kind == dkrt_sdk::proto::CAPK_DSP_COST as u32 {
            caps[c].used = 0;
            caps[c].largest = 0;
        }
        c += 1;
    }
    let tables = &*addr_of!(TABLES);
    let lens = &*addr_of!(TABLE_LEN);
    let mut mods: [Option<Keys>; SEL_MAX] = [None; SEL_MAX];
    let mut i = 0;
    while i < count.min(SEL_MAX) {
        if lens[i] != 0 {
            mods[i] = Keys::parse(&tables[i][..lens[i]]).ok();
        }
        i += 1;
    }
    let env = Env { services: crate::handlers::services(), runtime: dkrt_sdk::RUNTIME_VERSION, caps: &caps[..k] };
    let p = plan(&env, &mods[..count.min(SEL_MAX)]);
    *addr_of_mut!(PLANNED_DSP) = p.dsp_load;
    p
}

/// The needs of a module, as the fit function counts them.
fn needs_of_uses(t: &Keys) -> fit::Needs {
    let rb = t.record_bytes();
    fit::Needs {
        machine_rows: t.machine_rows(),
        param_rows: t.param_rows(),
        page_rows: t.page_rows(),
        dsp_cost_voice: t.dsp_cost_voice(),
        dsp_cost_fx: t.dsp_cost_fx(),
        arena: if rb != 0 { dkneeds::arena_block(rb).unwrap_or(u32::MAX) } else { 0 },
        ..fit::Needs::default()
    }
}

/// The verdict of module `j` against the accepted modules `done` (the ones before it in the order).
fn judge(env: &Env, mods: &[Option<Keys>], j: usize, done: &[bool; SEL_MAX], caps: &mut [DkrtCap]) -> Verdict {
    let t = match &mods[j] {
        Some(t) => t,
        None => return Verdict::Accepted,
    };
    // needs
    let mut ni = 0;
    while let Some(n) = t.words(tag::NEEDS, ni) {
        ni += 1;
        let (service, min_version, hard) = (n.0, n.1, n.2 & 1 != 0);
        if !hard {
            continue;
        }
        if is_runtime_service(service, &env.services) {
            if min_version > env.runtime {
                return Verdict::Refused(Reason::NeedVersion);
            }
            continue;
        }
        let mut found = false;
        let mut i = 0;
        while i < mods.len() {
            if i != j && done[i] {
                if let Some(ti) = &mods[i] {
                    found |= exports(ti, service);
                }
            }
            i += 1;
        }
        if !found {
            return Verdict::Refused(Reason::NeedMissing);
        }
    }
    // the kinds the module owns, the slots and hook sites it takes: one owner each
    let mut ki = 0;
    while let Some(d) = t.words(tag::KINDS, ki) {
        ki += 1;
        let kind = d.0;
        if env.services.owner(kind).is_some() {
            return Verdict::Refused(Reason::DuplicateOwner(kind));
        }
        let mut i = 0;
        while i < mods.len() {
            if i != j && done[i] {
                if let Some(ti) = &mods[i] {
                    if provides_kind(ti, kind) {
                        return Verdict::Refused(Reason::DuplicateOwner(kind));
                    }
                }
            }
            i += 1;
        }
    }
    let mut i = 0;
    while i < mods.len() {
        if i != j && done[i] {
            if let Some(ti) = &mods[i] {
                if let Some(site) = shared(t, ti, tag::SLOTS).or_else(|| shared(t, ti, tag::HOOKS)) {
                    return Verdict::Refused(Reason::DuplicateOwner(site));
                }
            }
        }
        i += 1;
    }
    // the keys: an owner for the kind, and no key twice for a generic kind
    let mut gi = 0;
    while let Some((kind, keys)) = t.group(gi) {
        gi += 1;
        let mut owned = env.services.owner(kind).is_some();
        let mut i = 0;
        while i < mods.len() {
            if i != j && done[i] {
                if let Some(ti) = &mods[i] {
                    owned |= provides_kind(ti, kind);
                }
            }
            i += 1;
        }
        owned |= provides_kind(t, kind);
        if !owned {
            return Verdict::Refused(Reason::UnownedKind(kind));
        }
        if is_generic(kind) {
            let mut n = 0;
            while n < keys.len() {
                let k = keys.get(n);
                n += 1;
                let mut i = 0;
                while i < mods.len() {
                    if i != j && done[i] {
                        if let Some(ti) = &mods[i] {
                            if ti.has_key(kind, k) {
                                return Verdict::Refused(Reason::DuplicateKey(kind, k));
                            }
                        }
                    }
                    i += 1;
                }
            }
        }
    }
    // fit
    let n = needs_of_uses(t);
    if let Some(s) = fit::fits_now(&n, caps) {
        return Verdict::Refused(Reason::Fit(s.kind));
    }
    fit::take(&n, caps);
    Verdict::Accepted
}

/// Plan a selection: `mods[k]` is the header table of selection index `k`.
pub fn plan(env: &Env, mods: &[Option<Keys>]) -> Plan {
    let n = mods.len().min(SEL_MAX);
    let mods = &mods[..n];
    let mut p = Plan { n, verdicts: [Verdict::Accepted; SEL_MAX], order: [0; SEL_MAX], dsp_load: (0, 0) };
    let mut cyclic = [false; SEL_MAX];
    order_of(mods, &mut p.order, &mut cyclic);
    let mut caps = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let k = env.caps.len().min(CAP_MAX);
    caps[..k].copy_from_slice(&env.caps[..k]);
    let mut done = [false; SEL_MAX];
    let mut at = 0;
    while at < n {
        let j = p.order[at] as usize;
        let v = if cyclic[j] { Verdict::Refused(Reason::NeedMissing) } else { judge(env, mods, j, &done, &mut caps[..k]) };
        p.verdicts[j] = v;
        done[j] = v.is_accepted();
        at += 1;
    }
    let mut c = 0;
    while c < k {
        if caps[c].kind == dkrt_sdk::proto::CAPK_DSP_COST as u32 {
            p.dsp_load = (caps[c].used, caps[c].largest);
        }
        c += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use dkneeds::keys::{write_table, KeyGroup, KindDecl, Need, Pair, Table, Use};
    use dkneeds::{KIND_EFFECT, KIND_MACHINE, RES_DSP_COST_FX, RES_DSP_COST_VOICE, RES_MACHINE_ROWS};
    use dkrt_abi::service::{Service, ServiceRow};
    use dkrt_sdk::proto::*;
    use dkrt_sdk::{E_CONTRACT, E_MISSING, E_PLAN_FIT, E_PLAN_KIND, E_PLAN_OWNER};
    use std::vec;
    use std::vec::Vec;

    struct S;
    impl Service for S {}
    static ROWS: [ServiceRow; 1] = [ServiceRow::of::<S>("machines", &[(KIND_MACHINE, 0x6c), (KIND_EFFECT, 0x1a4)])];

    fn bytes(t: &Table) -> Vec<u8> {
        let mut b = vec![0u8; dkneeds::keys::table_size(t)];
        write_table(t, &mut b).unwrap();
        b
    }

    fn caps() -> Vec<DkrtCap> {
        let c = |kind: u8, limit: u32| DkrtCap { kind: kind as u32, limit, used: 0, largest: limit };
        vec![c(CAPK_MODULES, 8), c(CAPK_SELECTION, 8), c(CAPK_MACHINE_ROWS, 4), c(CAPK_PARAM_ROWS, 16), c(CAPK_PAGE_ROWS, 8), c(CAPK_STAGE, 65536), c(CAPK_ARENA, 1 << 20)]
    }

    fn run(tables: &[Option<Vec<u8>>]) -> Plan {
        run_with(tables, caps())
    }

    /// The capacity of a device with 4 voices and a DSP headroom of 1000 instructions per block.
    fn dsp_caps() -> Vec<DkrtCap> {
        let mut c = caps();
        c.push(DkrtCap { kind: CAPK_DSP_COST as u32, limit: 1000, used: 0, largest: 0 });
        c.push(DkrtCap { kind: CAPK_DSP_VOICES as u32, limit: 4, used: 0, largest: 0 });
        c
    }

    fn run_with(tables: &[Option<Vec<u8>>], c: Vec<DkrtCap>) -> Plan {
        let env = Env { services: ServiceTable { rows: &ROWS, failed: 0 }, runtime: 0x000800, caps: &c };
        let keys: Vec<Option<Keys>> = tables.iter().map(|t| t.as_ref().map(|b| Keys::parse(b).unwrap())).collect();
        plan(&env, &keys)
    }

    const KEEP: Table<'static> = Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[] };

    #[test]
    fn a_module_with_no_table_and_a_module_with_nothing_to_share_are_accepted_in_selection_order() {
        let p = run(&[None, Some(bytes(&KEEP)), Some(bytes(&KEEP))]);
        assert_eq!(p.verdicts[..3], [Verdict::Accepted; 3]);
        assert_eq!(p.order[..3], [0, 1, 2]);
    }

    #[test]
    fn a_provider_loads_before_the_modules_that_need_it_and_a_need_nobody_provides_is_refused() {
        let svc = name_hash("lfo_bus");
        let consumer = Table { needs: &[Need { service: svc, min_version: 0x000800, hard: true }], ..KEEP };
        let provider = Table { exports: &[Pair { a: svc, b: 0x40 }], ..KEEP };
        // consumer first in the selection, provider second: the provider loads first
        let p = run(&[Some(bytes(&consumer)), Some(bytes(&provider))]);
        assert_eq!(p.order[..2], [1, 0]);
        assert_eq!(p.verdicts[..2], [Verdict::Accepted; 2]);
        // without the provider the need is missing; an optional need is not looked at
        let p = run(&[Some(bytes(&consumer))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing));
        assert_eq!(p.verdicts[0].result(), E_MISSING);
        let soft = Table { needs: &[Need { service: svc, min_version: 0x000800, hard: false }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&soft))]).verdicts[0], Verdict::Accepted);
        // a service of the runtime is always there, at a version up to the runtime's
        let ok = Table { needs: &[Need { service: name_hash("runtime"), min_version: 0x000800, hard: true }, Need { service: name_hash("ui"), min_version: 0x000700, hard: true }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&ok))]).verdicts[0], Verdict::Accepted);
        let newer = Table { needs: &[Need { service: name_hash("runtime"), min_version: 0x000801, hard: true }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&newer))]).verdicts[0].result(), E_CONTRACT);
        // two modules that need each other cannot load: both are refused
        let a = Table { needs: &[Need { service: 1, min_version: 0, hard: true }], exports: &[Pair { a: 2, b: 0 }], ..KEEP };
        let b = Table { needs: &[Need { service: 2, min_version: 0, hard: true }], exports: &[Pair { a: 1, b: 0 }], ..KEEP };
        let p = run(&[Some(bytes(&a)), Some(bytes(&b))]);
        assert_eq!(p.verdicts[..2], [Verdict::Refused(Reason::NeedMissing); 2]);
    }

    #[test]
    fn keys_for_a_kind_nobody_owns_are_refused_and_a_provider_makes_the_kind_owned() {
        let ks = [5u32];
        let unowned = Table { keys: &[KeyGroup { kind: 90, keys: &ks }], ..KEEP };
        let p = run(&[Some(bytes(&unowned))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::UnownedKind(90)));
        assert_eq!(p.verdicts[0].result(), E_PLAN_KIND);
        // a service of the table owns the machine kind
        let ms = [0x100u32];
        let owned = Table { keys: &[KeyGroup { kind: KIND_MACHINE, keys: &ms }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&owned))]).verdicts[0], Verdict::Accepted);
        // a module that declares the kind owns it, and the module with keys for it loads after
        let provider = Table { kinds: &[KindDecl { kind: 90, len: 16, entry: 8 }], ..KEEP };
        let p = run(&[Some(bytes(&unowned)), Some(bytes(&provider))]);
        assert_eq!(p.order[..2], [1, 0]);
        assert_eq!(p.verdicts[..2], [Verdict::Accepted; 2]);
    }

    #[test]
    fn a_kind_a_slot_or_a_hook_site_has_one_owner() {
        let dup_kind = Table { kinds: &[KindDecl { kind: KIND_MACHINE, len: 8, entry: 0 }], ..KEEP };
        let p = run(&[Some(bytes(&dup_kind))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::DuplicateOwner(KIND_MACHINE)));
        assert_eq!(p.verdicts[0].result(), E_PLAN_OWNER);
        let k = Table { kinds: &[KindDecl { kind: 90, len: 16, entry: 8 }], ..KEEP };
        let p = run(&[Some(bytes(&k)), Some(bytes(&k))]);
        assert_eq!(p.verdicts[..2], [Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(90))]);
        let slot = Table { slots: &[Pair { a: 7, b: 0 }], ..KEEP };
        let hook = Table { hooks: &[Pair { a: 9, b: 4 }], ..KEEP };
        let p = run(&[Some(bytes(&slot)), Some(bytes(&slot)), Some(bytes(&hook)), Some(bytes(&hook))]);
        assert_eq!(
            p.verdicts[..4],
            [Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(7)), Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(9))]
        );
    }

    #[test]
    fn the_keys_of_the_typed_kinds_are_not_exclusive_across_modules() {
        // a machine key twice takes a fresh type number (assign rule 4) and an effect id twice is E_FX_ID_DUP at the load: not the planner's
        let ms = [0x100u32];
        let m = Table { keys: &[KeyGroup { kind: KIND_MACHINE, keys: &ms }], ..KEEP };
        let p = run(&[Some(bytes(&m)), Some(bytes(&m))]);
        assert_eq!(p.verdicts[..2], [Verdict::Accepted; 2]);
    }

    #[test]
    fn a_full_selection_of_modules_that_use_nothing_is_accepted() {
        // eight modules, eight selection paths and eight module records: the plan counts the paths as it takes the modules, so all fit
        let tables: Vec<Option<Vec<u8>>> = (0..8).map(|_| Some(bytes(&KEEP))).collect();
        let p = run(&tables);
        assert_eq!(p.verdicts[..8], [Verdict::Accepted; 8]);
    }

    #[test]
    fn the_dsp_load_is_the_largest_machine_for_every_voice_plus_the_effects() {
        let voice = |n| bytes(&Table { uses: &[Use { resource: RES_DSP_COST_VOICE, count: n }], ..KEEP });
        let fx = |n| bytes(&Table { uses: &[Use { resource: RES_DSP_COST_FX, count: n }], ..KEEP });
        // 4 voices, headroom 1000: machines of 100 and 250 share the voices (250 * 4 = 1000), a third of 60 adds nothing
        let (m1, m2, m3) = (voice(100), voice(250), voice(60));
        let p = run_with(&[Some(m1.clone()), Some(m2.clone()), Some(m3.clone())], dsp_caps());
        assert_eq!(p.verdicts[..3], [Verdict::Accepted; 3]);
        assert_eq!(p.dsp_load, (1000, 250));
        // a machine of 251 takes 1004: refused with the DSP load as the reason, and it takes nothing
        let big = voice(251);
        let p = run_with(&[Some(m1.clone()), Some(big.clone())], dsp_caps());
        assert_eq!(p.verdicts[..2], [Verdict::Accepted, Verdict::Refused(Reason::Fit(CAPK_DSP_COST))]);
        assert_eq!(p.verdicts[1].result(), E_PLAN_FIT);
        assert_eq!(p.dsp_load, (400, 100));
        // effects add up on top of the voices: 800 for the voices, then 100 and 100 fit and one more instruction does not
        let (e1, e2, e3) = (fx(100), fx(100), fx(1));
        let m = voice(200);
        let p = run_with(&[Some(m.clone()), Some(e1.clone()), Some(e2.clone()), Some(e3.clone())], dsp_caps());
        assert_eq!(
            p.verdicts[..4],
            [Verdict::Accepted, Verdict::Accepted, Verdict::Accepted, Verdict::Refused(Reason::Fit(CAPK_DSP_COST))]
        );
        // a selection of effects only
        let p = run_with(&[Some(fx(1000)), Some(fx(1))], dsp_caps());
        assert_eq!(p.verdicts[..2], [Verdict::Accepted, Verdict::Refused(Reason::Fit(CAPK_DSP_COST))]);
        // a capacity without the DSP entries (an older runtime) is not judged
        assert_eq!(run(&[Some(voice(u32::MAX))]).verdicts[0], Verdict::Accepted);
    }

    #[test]
    fn the_uses_are_summed_against_the_capacity_the_earlier_modules_left() {
        let three = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 3 }], ..KEEP };
        let two = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 2 }], ..KEEP };
        let one = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 1 }], ..KEEP };
        // 4 machine rows: 3 fit, 2 more do not, 1 more does; a refused module takes nothing
        let p = run(&[Some(bytes(&three)), Some(bytes(&two)), Some(bytes(&one))]);
        assert_eq!(
            p.verdicts[..3],
            [Verdict::Accepted, Verdict::Refused(Reason::Fit(CAPK_MACHINE_ROWS)), Verdict::Accepted]
        );
        assert_eq!(p.verdicts[1].result(), E_PLAN_FIT);
    }
}
