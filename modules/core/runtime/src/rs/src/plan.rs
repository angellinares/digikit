//! The boot planner (docs/plans/runtime-and-services.md R7): a pure function over the header tables of the selected modules. No module code runs and
//! nothing is written; it is the answer to "may this selection load, and in which order" before the first load.
//!
//! Input: for each module of the selection its header table (`dkneeds::keys::Keys`), or `None` when the file could not be read or has no table (the
//! loader reports that error itself; the planner accepts the module). Output (`Plan`): one `Verdict` per module (`dkrt_abi::plan`: accepted, or refused with a
//! `Reason`) and the order to load them in.
//!
//! - Providers (R12-2): the service files (`.dks`) the boot found for the hard needs of the selection come in their own arrays (`PROVIDER_MAX`), apart from
//!   the module arrays: a provider never takes a module record. All providers are planned and load before the modules (a module never provides a service),
//!   in dependency order among themselves.
//! - Order: a module that needs a service a provider provides (`NEEDS`, `PROVIDES`), or whose keys are for a kind another module owns (`KEYS`, `KINDS`),
//!   loads after that module; otherwise the order is the selection's (the module id's position). A cycle refuses its modules (`NeedMissing`).
//! - Needs: a hard need of a service that is neither one of the runtime's (`[handle] services`) nor provided by an accepted provider is refused (`NeedMissing`);
//!   a need of a newer version than the runtime, or than the provider provides, is refused (`NeedVersion`). A service of the image (core/ui and the like) that
//!   is not there is the loader's `E_IMPORT`, with the missing module's name: the planner does not know the image.
//! - Exclusive: one owner for each kind (a service of the table, or a module's `KINDS`), slot and hook site; keys of a *generic* kind (`[records.kinds]`
//!   `generic`) are unique across the selection. The keys of the typed kinds (machines, effects) keep their own rules: a machine key twice takes a fresh type
//!   number, an effect id twice is `E_FX_ID_DUP` at the load.
//! - Fit: the module's `USES` (machine, parameter and page rows, the record block, the DSP cost) against the capacity the earlier accepted modules left, with the
//!   same function the menu uses (`dkrt_fit::fits_now` and `take`). The DSP load is `max(per-voice machine cost) * voices + sum(effect costs)` against the
//!   headroom of the device profile; the capacity's DSP entry keeps the running maximum and the sum. A module that takes the load over the headroom is
//!   refused with `Reason::Fit(CAPK_DSP_COST)`.
use dkneeds::keys::{name_hash, tag, Keys};
use dkrt_abi::plan::{Reason, Verdict};
use crate::handlers::ServiceTable;
use dkrt_fit as fit;
use dkrt_sdk::{DkrtCap, CAP_MAX, PROVIDER_MAX, SEL_MAX};

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
    /// The number of providers planned (at most `PROVIDER_MAX`).
    pub np: usize,
    /// The verdict of each provider, by provider index.
    pub provider_verdicts: [Verdict; PROVIDER_MAX],
    /// The provider indices in load order; all of them load before the modules (a refused provider is in it too).
    pub provider_order: [u8; PROVIDER_MAX],
}

pub(crate) fn is_runtime_service(h: u32, services: &ServiceTable) -> bool {
    if h == name_hash("runtime") {
        return true;
    }
    let mut i = 0;
    // the static prefix only: the rows after it are loaded services
    let n = if services.rows.len() < crate::gen_services::STATIC { services.rows.len() } else { crate::gen_services::STATIC };
    while i < n {
        if name_hash(unsafe { &*services.rows[i] }.name()) == h {
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

/// The version of service `h` that entry `t` provides (`PROVIDES`), if it does.
fn provided_version(t: &Keys, h: u32) -> Option<u32> {
    let mut i = 0;
    while let Some(x) = t.words(tag::PROVIDES, i) {
        if x.0 == h {
            return Some(x.1);
        }
        i += 1;
    }
    None
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

/// Whether entry `j` has to load after entry `i`: it needs a service `i` provides, or has keys for a kind `i` owns.
fn after(j: &Keys, i: &Keys) -> bool {
    let mut k = 0;
    while let Some(n) = j.words(tag::NEEDS, k) {
        if n.2 & 1 != 0 && provided_version(i, n.0).is_some() {
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

/// The most bytes of a header table the boot planner keeps (a module with eight machines, three effects and nine needs has about 260; a larger table is not planned and the loader decides).
pub(crate) const TABLE_MAX: usize = 384;

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

/// The refusal for the hard needs of entry `t`, if one is not met: a service of the runtime (at most the runtime's version) or one an accepted provider
/// (`pdone`, not `skip`: the provider's own index, `usize::MAX` for a module) provides at the needed version or newer.
fn check_needs(env: &Env, t: &Keys, provs: &[Option<Keys>], pdone: &[bool; PROVIDER_MAX], skip: usize) -> Option<Reason> {
    let mut ni = 0;
    while let Some(n) = t.words(tag::NEEDS, ni) {
        ni += 1;
        let (service, min_version, hard) = (n.0, n.1, n.2 & 1 != 0);
        if !hard {
            continue;
        }
        if is_runtime_service(service, &env.services) {
            if min_version > env.runtime {
                return Some(Reason::NeedVersion);
            }
            continue;
        }
        let mut found = false;
        let mut met = false;
        let mut i = 0;
        while i < provs.len() {
            if i != skip && pdone[i] {
                if let Some(ti) = &provs[i] {
                    if let Some(v) = provided_version(ti, service) {
                        found = true;
                        if v >= min_version {
                            met = true;
                        }
                    }
                }
            }
            i += 1;
        }
        if !found {
            return Some(Reason::NeedMissing(service));
        }
        if !met {
            return Some(Reason::NeedVersion);
        }
    }
    None
}

/// Fit `t`'s uses against `caps` and take them. A provider takes no module record: the module and selection entries are left as they were.
fn take_fit(t: &Keys, caps: &mut [DkrtCap], record: bool) -> Option<u8> {
    let mut saved = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; 2];
    let mut at = [usize::MAX; 2];
    if !record {
        let mut s = 0;
        let mut c = 0;
        while c < caps.len() {
            let k = caps[c].kind;
            if (k == dkrt_sdk::proto::CAPK_MODULES as u32 || k == dkrt_sdk::proto::CAPK_SELECTION as u32) && s < 2 {
                saved[s] = caps[c];
                at[s] = c;
                s += 1;
                // room for the record the fit function counts
                caps[c].limit = caps[c].limit.saturating_add(1);
            }
            c += 1;
        }
    }
    let n = needs_of_uses(t);
    let short = fit::fits_now(&n, caps).map(|s| s.kind);
    if short.is_none() {
        fit::take(&n, caps);
    }
    let mut s = 0;
    while s < 2 {
        if at[s] != usize::MAX {
            caps[at[s]] = saved[s];
        }
        s += 1;
    }
    short
}

/// The first kind, slot or hook site of `t` that another owner has: a kind of the runtime's table, or a kind, slot or hook site of an accepted module (`done`, not
/// index `skip_m`) or of an accepted provider (`pdone`, not index `skip_p`). One rule for modules and providers (a service module's KINDS, SLOTS and HOOKS).
#[allow(clippy::too_many_arguments)]
fn owner_clash(
    env: &Env,
    t: &Keys,
    mods: &[Option<Keys>],
    done: &[bool; SEL_MAX],
    skip_m: usize,
    provs: &[Option<Keys>],
    pdone: &[bool; PROVIDER_MAX],
    skip_p: usize,
) -> Option<u32> {
    let mut ki = 0;
    while let Some(d) = t.words(tag::KINDS, ki) {
        ki += 1;
        let kind = d.0;
        if env.services.owner(kind).is_some() {
            return Some(kind);
        }
        let mut i = 0;
        while i < mods.len() {
            if i != skip_m && done[i] {
                if let Some(ti) = &mods[i] {
                    if provides_kind(ti, kind) {
                        return Some(kind);
                    }
                }
            }
            i += 1;
        }
        let mut i = 0;
        while i < provs.len() {
            if i != skip_p && pdone[i] {
                if let Some(ti) = &provs[i] {
                    if provides_kind(ti, kind) {
                        return Some(kind);
                    }
                }
            }
            i += 1;
        }
    }
    let mut i = 0;
    while i < mods.len() {
        if i != skip_m && done[i] {
            if let Some(ti) = &mods[i] {
                if let Some(site) = shared(t, ti, tag::SLOTS).or_else(|| shared(t, ti, tag::HOOKS)) {
                    return Some(site);
                }
            }
        }
        i += 1;
    }
    let mut i = 0;
    while i < provs.len() {
        if i != skip_p && pdone[i] {
            if let Some(ti) = &provs[i] {
                if let Some(site) = shared(t, ti, tag::SLOTS).or_else(|| shared(t, ti, tag::HOOKS)) {
                    return Some(site);
                }
            }
        }
        i += 1;
    }
    None
}

/// The refusal for the keys of `t`, if any: a group of keys for a kind nobody owns (the runtime's table, `t` itself, an accepted module or provider), or a key of a
/// generic kind that an accepted module (`done`, not index `skip_m`) or provider (`pdone`, not index `skip_p`) has too. One rule for modules and providers.
#[allow(clippy::too_many_arguments)]
fn keys_clash(
    env: &Env,
    t: &Keys,
    mods: &[Option<Keys>],
    done: &[bool; SEL_MAX],
    skip_m: usize,
    provs: &[Option<Keys>],
    pdone: &[bool; PROVIDER_MAX],
    skip_p: usize,
) -> Option<Reason> {
    let mut gi = 0;
    while let Some((kind, keys)) = t.group(gi) {
        gi += 1;
        let mut owned = env.services.owner(kind).is_some();
        let mut i = 0;
        while i < mods.len() {
            if i != skip_m && done[i] {
                if let Some(ti) = &mods[i] {
                    owned |= provides_kind(ti, kind);
                }
            }
            i += 1;
        }
        owned |= provides_kind(t, kind);
        let mut p = 0;
        while p < provs.len() {
            if p != skip_p && pdone[p] {
                if let Some(tp) = &provs[p] {
                    owned |= provides_kind(tp, kind);
                }
            }
            p += 1;
        }
        if !owned {
            return Some(Reason::UnownedKind(kind));
        }
        if is_generic(kind) {
            let mut n = 0;
            while n < keys.len() {
                let k = keys.get(n);
                n += 1;
                let mut i = 0;
                while i < mods.len() {
                    if i != skip_m && done[i] {
                        if let Some(ti) = &mods[i] {
                            if ti.has_key(kind, k) {
                                return Some(Reason::DuplicateKey(kind, k));
                            }
                        }
                    }
                    i += 1;
                }
                let mut p = 0;
                while p < provs.len() {
                    if p != skip_p && pdone[p] {
                        if let Some(tp) = &provs[p] {
                            if tp.has_key(kind, k) {
                                return Some(Reason::DuplicateKey(kind, k));
                            }
                        }
                    }
                    p += 1;
                }
            }
        }
    }
    None
}

/// The verdict of provider `j` against the accepted providers `pdone` (the ones before it in the order).
fn judge_provider(env: &Env, provs: &[Option<Keys>], j: usize, pdone: &[bool; PROVIDER_MAX], caps: &mut [DkrtCap]) -> Verdict {
    let t = match &provs[j] {
        Some(t) => t,
        None => return Verdict::Accepted,
    };
    if let Some(r) = check_needs(env, t, provs, pdone, j) {
        return Verdict::Refused(r);
    }
    // one owner for each service
    let mut k = 0;
    while let Some(x) = t.words(tag::PROVIDES, k) {
        k += 1;
        let h = x.0;
        if is_runtime_service(h, &env.services) {
            return Verdict::Refused(Reason::DuplicateOwner(h));
        }
        let mut i = 0;
        while i < provs.len() {
            if i != j && pdone[i] {
                if let Some(ti) = &provs[i] {
                    if provided_version(ti, h).is_some() {
                        return Verdict::Refused(Reason::DuplicateOwner(h));
                    }
                }
            }
            i += 1;
        }
    }
    if let Some(site) = owner_clash(env, t, &[], &[false; SEL_MAX], usize::MAX, provs, pdone, j) {
        return Verdict::Refused(Reason::DuplicateOwner(site));
    }
    if let Some(r) = keys_clash(env, t, &[], &[false; SEL_MAX], usize::MAX, provs, pdone, j) {
        return Verdict::Refused(r);
    }
    match take_fit(t, caps, false) {
        Some(kind) => Verdict::Refused(Reason::Fit(kind)),
        None => Verdict::Accepted,
    }
}

/// The verdict of module `j` against the accepted providers `pdone` and the accepted modules `done` (the ones before it in the order).
fn judge(
    env: &Env,
    mods: &[Option<Keys>],
    provs: &[Option<Keys>],
    j: usize,
    done: &[bool; SEL_MAX],
    pdone: &[bool; PROVIDER_MAX],
    caps: &mut [DkrtCap],
) -> Verdict {
    let t = match &mods[j] {
        Some(t) => t,
        None => return Verdict::Accepted,
    };
    // needs
    if let Some(r) = check_needs(env, t, provs, pdone, usize::MAX) {
        return Verdict::Refused(r);
    }
    // the kinds the module owns, the slots and hook sites it takes: one owner each
    if let Some(site) = owner_clash(env, t, mods, done, j, provs, pdone, usize::MAX) {
        return Verdict::Refused(Reason::DuplicateOwner(site));
    }
    // the keys: an owner for the kind, and no key twice for a generic kind
    if let Some(r) = keys_clash(env, t, mods, done, j, provs, pdone, usize::MAX) {
        return Verdict::Refused(r);
    }
    // fit
    match take_fit(t, caps, true) {
        Some(kind) => Verdict::Refused(Reason::Fit(kind)),
        None => Verdict::Accepted,
    }
}

/// The first hard need of `t` that only a provider which failed to load could have met: its service hash. `loaded[i]` is true when provider `i` loaded.
/// A need no provider provides is not here: the plan refused it already. One rule for modules and providers (`settle` and the provider loop of the boot).
pub fn failed_need(services: &ServiceTable, t: &Keys, provs: &[Option<Keys>], loaded: &[bool]) -> Option<u32> {
    let mut ni = 0;
    while let Some(n) = t.words(tag::NEEDS, ni) {
        ni += 1;
        if n.2 & 1 == 0 || is_runtime_service(n.0, services) {
            continue;
        }
        let mut offered = false;
        let mut met = false;
        let mut i = 0;
        while i < provs.len() {
            if let Some(ti) = &provs[i] {
                if provided_version(ti, n.0).is_some() {
                    offered = true;
                    if loaded[i] {
                        met = true;
                    }
                }
            }
            i += 1;
        }
        if offered && !met {
            return Some(n.0);
        }
    }
    None
}

/// The settle step of the boot: after the providers loaded (`loaded[i]`: provider `i` is live), a module the plan accepted whose hard need only a failed
/// provider could meet is refused with `NeedMissing(hash)`. Pure: nothing runs and nothing is written but the verdicts. The resources the plan reserved for
/// such a module stay counted for the modules after it (the plan is not redone).
pub fn settle(services: &ServiceTable, plan: &mut Plan, mods: &[Option<Keys>], provs: &[Option<Keys>], loaded: &[bool]) {
    let mut k = 0;
    while k < plan.n.min(mods.len()) {
        if plan.verdicts[k].is_accepted() {
            if let Some(t) = &mods[k] {
                if let Some(h) = failed_need(services, t, provs, loaded) {
                    plan.verdicts[k] = Verdict::Refused(Reason::NeedMissing(h));
                }
            }
        }
        k += 1;
    }
}

/// Plan a selection without providers: `mods[k]` is the header table of selection index `k`.
pub fn plan(env: &Env, mods: &[Option<Keys>]) -> Plan {
    plan_with(env, mods, &[])
}

/// Plan a selection with its providers: `provs[k]` is the header table of provider index `k` (at most `PROVIDER_MAX`). The providers are judged first, in
/// dependency order, then the modules.
pub fn plan_with(env: &Env, mods: &[Option<Keys>], provs: &[Option<Keys>]) -> Plan {
    let n = mods.len().min(SEL_MAX);
    let mods = &mods[..n];
    let np = provs.len().min(PROVIDER_MAX);
    let provs = &provs[..np];
    let mut p = Plan {
        n,
        verdicts: [Verdict::Accepted; SEL_MAX],
        order: [0; SEL_MAX],
        dsp_load: (0, 0),
        np,
        provider_verdicts: [Verdict::Accepted; PROVIDER_MAX],
        provider_order: [0; PROVIDER_MAX],
    };
    let mut caps = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let k = env.caps.len().min(CAP_MAX);
    caps[..k].copy_from_slice(&env.caps[..k]);
    let mut pdone = [false; PROVIDER_MAX];
    let mut porder = [0u8; SEL_MAX];
    let mut pcyclic = [false; SEL_MAX];
    order_of(provs, &mut porder, &mut pcyclic);
    let mut pat = 0;
    while pat < np {
        let j = porder[pat] as usize;
        p.provider_order[pat] = j as u8;
        let v = if pcyclic[j] { Verdict::Refused(Reason::NeedMissing(0)) } else { judge_provider(env, provs, j, &pdone, &mut caps[..k]) };
        p.provider_verdicts[j] = v;
        pdone[j] = v.is_accepted();
        pat += 1;
    }
    let mut cyclic = [false; SEL_MAX];
    order_of(mods, &mut p.order, &mut cyclic);
    let mut done = [false; SEL_MAX];
    let mut at = 0;
    while at < n {
        let j = p.order[at] as usize;
        let v = if cyclic[j] { Verdict::Refused(Reason::NeedMissing(0)) } else { judge(env, mods, provs, j, &done, &pdone, &mut caps[..k]) };
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
    use dkneeds::keys::{write_table, KeyGroup, KindDecl, Need, Pair, Slot, Table, Use};
    use dkneeds::{KIND_EFFECT, KIND_MACHINE, RES_DSP_COST_FX, RES_DSP_COST_VOICE, RES_MACHINE_ROWS};
    use crate::core_api::{CoreApi, REAL_CORE};
    use dkrt_abi::service::{Service, ServiceRow};
    use dkrt_sdk::proto::*;
    use dkrt_sdk::{E_CONTRACT, E_MISSING, E_PLAN_FIT, E_PLAN_KIND, E_PLAN_OWNER};
    use std::vec;
    use std::vec::Vec;

    struct S;
    impl<C: CoreApi> Service<C> for S {}
    static ROWS: [ServiceRow; 1] = [ServiceRow::of::<S, crate::core_api::RealCore>("machines", &[(KIND_MACHINE, 0x6c), (KIND_EFFECT, 0x1a4)])];
    static ROWS_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ROWS);

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
        let env = Env { services: ServiceTable::new(&ROWS_P.0, &REAL_CORE), runtime: 0x000800, caps: &c };
        let keys: Vec<Option<Keys>> = tables.iter().map(|t| t.as_ref().map(|b| Keys::parse(b).unwrap())).collect();
        plan(&env, &keys)
    }

    fn run_p(mods: &[Option<Vec<u8>>], provs: &[Option<Vec<u8>>]) -> Plan {
        let c = caps();
        let env = Env { services: ServiceTable::new(&ROWS_P.0, &REAL_CORE), runtime: 0x000800, caps: &c };
        fn parse(tables: &[Option<Vec<u8>>]) -> Vec<Option<Keys<'_>>> {
            tables.iter().map(|t| t.as_ref().map(|b| Keys::parse(b).unwrap())).collect()
        }
        plan_with(&env, &parse(mods), &parse(provs))
    }

    const KEEP: Table<'static> =
        Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[], provides: &[], bind: &[] };

    #[test]
    fn a_module_with_no_table_and_a_module_with_nothing_to_share_are_accepted_in_selection_order() {
        let p = run(&[None, Some(bytes(&KEEP)), Some(bytes(&KEEP))]);
        assert_eq!(p.verdicts[..3], [Verdict::Accepted; 3]);
        assert_eq!(p.order[..3], [0, 1, 2]);
    }

    #[test]
    fn a_service_a_dks_provides_is_never_the_runtimes_even_when_the_sdk_has_handles_for_it() {
        // `fx` has handle types in the SDK but is a service file: a module's hard need of it needs the provider (E_MISSING), it is not always there
        let consumer = Table { needs: &[Need { service: name_hash("fx"), min_version: 0, hard: true }], ..KEEP };
        let p = run(&[Some(bytes(&consumer))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(name_hash("fx"))));
    }

    #[test]
    fn a_module_need_is_refused_when_nothing_provides_it_and_the_runtime_services_are_always_there() {
        let svc = name_hash("lfo_bus");
        let consumer = Table { needs: &[Need { service: svc, min_version: 0x000800, hard: true }], ..KEEP };
        // without a provider the need is missing; an optional need is not looked at
        let p = run(&[Some(bytes(&consumer))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(svc)));
        assert_eq!(p.verdicts[0].result(), E_MISSING);
        let soft = Table { needs: &[Need { service: svc, min_version: 0x000800, hard: false }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&soft))]).verdicts[0], Verdict::Accepted);
        // a service of the runtime is always there, at a version up to the runtime's
        let ok = Table { needs: &[Need { service: name_hash("runtime"), min_version: 0x000800, hard: true }, Need { service: name_hash("ui"), min_version: 0x000700, hard: true }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&ok))]).verdicts[0], Verdict::Accepted);
        let newer = Table { needs: &[Need { service: name_hash("runtime"), min_version: 0x000801, hard: true }], ..KEEP };
        assert_eq!(run(&[Some(bytes(&newer))]).verdicts[0].result(), E_CONTRACT);
    }

    fn need(service: u32, min_version: u32) -> Need {
        Need { service, min_version, hard: true }
    }

    #[test]
    fn a_provider_is_ordered_before_its_dependents() {
        let svc = name_hash("lfo_bus");
        let consumer = Table { needs: &[need(svc, 0x000100)], ..KEEP };
        let provider = Table { provides: &[Pair { a: svc, b: 0x000100 }], ..KEEP };
        let p = run_p(&[Some(bytes(&consumer))], &[Some(bytes(&provider))]);
        assert_eq!((p.n, p.np), (1, 1));
        assert_eq!(p.verdicts[0], Verdict::Accepted);
        assert_eq!(p.provider_verdicts[0], Verdict::Accepted);
        // a provider that needs a provider: the one needed is first, whatever the find order was
        let (a, b) = (name_hash("a"), name_hash("b"));
        let top = Table { needs: &[need(b, 0)], provides: &[Pair { a, b: 0 }], ..KEEP };
        let base = Table { provides: &[Pair { a: b, b: 0 }], ..KEEP };
        let user = Table { needs: &[need(a, 0)], ..KEEP };
        let p = run_p(&[Some(bytes(&user))], &[Some(bytes(&top)), Some(bytes(&base))]);
        assert_eq!(p.provider_order[..2], [1, 0]);
        assert_eq!(p.provider_verdicts[..2], [Verdict::Accepted; 2]);
        assert_eq!(p.verdicts[0], Verdict::Accepted);
        // providers that need each other cannot load, and the module that needs one is missing its service
        let x = Table { needs: &[need(2, 0)], provides: &[Pair { a: 1, b: 0 }], ..KEEP };
        let y = Table { needs: &[need(1, 0)], provides: &[Pair { a: 2, b: 0 }], ..KEEP };
        let user = Table { needs: &[need(1, 0)], ..KEEP };
        let p = run_p(&[Some(bytes(&user))], &[Some(bytes(&x)), Some(bytes(&y))]);
        assert_eq!(p.provider_verdicts[..2], [Verdict::Refused(Reason::NeedMissing(0)); 2]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(1)));
    }

    #[test]
    fn a_need_with_no_provider_is_missing() {
        let consumer = Table { needs: &[need(name_hash("lfo_bus"), 0)], ..KEEP };
        let other = Table { provides: &[Pair { a: name_hash("other"), b: 0 }], ..KEEP };
        let p = run_p(&[Some(bytes(&consumer))], &[Some(bytes(&other))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(name_hash("lfo_bus"))));
        assert_eq!(p.verdicts[0].result(), E_MISSING);
    }

    #[test]
    fn a_provider_older_than_the_need_is_a_contract_error() {
        let svc = name_hash("lfo_bus");
        let provider = Table { provides: &[Pair { a: svc, b: 0x010000 }], ..KEEP };
        let newer = Table { needs: &[need(svc, 0x010100)], ..KEEP };
        let same = Table { needs: &[need(svc, 0x010000)], ..KEEP };
        let p = run_p(&[Some(bytes(&newer)), Some(bytes(&same))], &[Some(bytes(&provider))]);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedVersion));
        assert_eq!(p.verdicts[0].result(), E_CONTRACT);
        assert_eq!(p.verdicts[1], Verdict::Accepted);
    }

    #[test]
    fn two_providers_of_one_service_have_one_owner() {
        let svc = name_hash("lfo_bus");
        let provider = Table { provides: &[Pair { a: svc, b: 0 }], ..KEEP };
        let p = run_p(&[], &[Some(bytes(&provider)), Some(bytes(&provider))]);
        assert_eq!(p.provider_verdicts[..2], [Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(svc))]);
        assert_eq!(p.provider_verdicts[1].result(), E_PLAN_OWNER);
        // a service of the runtime is not for a provider to take
        let rt = Table { provides: &[Pair { a: name_hash("runtime"), b: 0 }], ..KEEP };
        assert_eq!(run_p(&[], &[Some(bytes(&rt))]).provider_verdicts[0], Verdict::Refused(Reason::DuplicateOwner(name_hash("runtime"))));
    }

    #[test]
    fn the_uses_of_a_provider_count_against_fit_without_a_module_record() {
        let three = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 3 }], ..KEEP };
        let two = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 2 }], ..KEEP };
        let three_p = Table { provides: &[Pair { a: 6, b: 0 }], ..three };
        // 4 machine rows: the provider takes 3, so the module of 2 does not fit
        let p = run_p(&[Some(bytes(&two))], &[Some(bytes(&three_p))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Accepted);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::Fit(CAPK_MACHINE_ROWS)));
        // a provider over the capacity is refused
        let five = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 5 }], provides: &[Pair { a: 7, b: 0 }], ..KEEP };
        let p = run_p(&[], &[Some(bytes(&five))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Refused(Reason::Fit(CAPK_MACHINE_ROWS)));
        // the providers take no module record: six providers and eight modules fit the eight records
        let provs: Vec<Option<Vec<u8>>> = (0..6u32).map(|k| Some(bytes(&Table { provides: &[Pair { a: 100 + k, b: 0 }], ..KEEP }))).collect();
        let mods: Vec<Option<Vec<u8>>> = (0..8).map(|_| Some(bytes(&KEEP))).collect();
        let p = run_p(&mods, &provs);
        assert_eq!(p.provider_verdicts[..6], [Verdict::Accepted; 6]);
        assert_eq!(p.verdicts[..8], [Verdict::Accepted; 8]);
    }

    #[test]
    fn module_exports_no_longer_meet_a_need() {
        let svc = name_hash("lfo_bus");
        let consumer = Table { needs: &[need(svc, 0)], ..KEEP };
        let exporter = Table { exports: &[Pair { a: svc, b: 0x40 }], ..KEEP };
        let p = run(&[Some(bytes(&consumer)), Some(bytes(&exporter))]);
        assert_eq!(p.verdicts[..2], [Verdict::Refused(Reason::NeedMissing(svc)), Verdict::Accepted]);
        assert_eq!(p.order[..2], [0, 1]);
    }

    #[test]
    fn settle_refuses_the_dependents_of_a_provider_that_did_not_load() {
        fn parse(tables: &[Vec<u8>]) -> Vec<Option<Keys<'_>>> {
            tables.iter().map(|b| Some(Keys::parse(b).unwrap())).collect()
        }
        let (a, b) = (name_hash("a"), name_hash("b"));
        let user_a = Table { needs: &[need(a, 0)], ..KEEP };
        let user_b = Table { needs: &[need(b, 0)], ..KEEP };
        let user_rt = Table { needs: &[need(name_hash("runtime"), 0)], ..KEEP };
        let prov_a = Table { provides: &[Pair { a, b: 0 }], ..KEEP };
        let prov_b = Table { provides: &[Pair { a: b, b: 0 }], ..KEEP };
        let (mt, pt) = ([bytes(&user_a), bytes(&user_b), bytes(&user_rt)], [bytes(&prov_a), bytes(&prov_b)]);
        let (mods, provs) = (parse(&mt), parse(&pt));
        let c = caps();
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let env = Env { services, runtime: 0x000800, caps: &c };
        let mut p = plan_with(&env, &mods, &provs);
        assert_eq!(p.verdicts[..3], [Verdict::Accepted; 3]);
        // both loaded: nothing changes
        settle(&services, &mut p, &mods, &provs, &[true, true]);
        assert_eq!(p.verdicts[..3], [Verdict::Accepted; 3]);
        // provider b failed: only the module that needs b is refused, with b's hash in the detail
        settle(&services, &mut p, &mods, &provs, &[true, false]);
        assert_eq!(p.verdicts[..3], [Verdict::Accepted, Verdict::Refused(Reason::NeedMissing(b)), Verdict::Accepted]);
        assert_eq!(p.verdicts[1].result(), E_MISSING);
        assert_eq!(failed_need(&services, mods[1].as_ref().unwrap(), &provs, &[true, false]), Some(b));
        assert_eq!(failed_need(&services, mods[0].as_ref().unwrap(), &provs, &[true, false]), None);
    }

    #[test]
    fn a_refused_provider_fails_the_modules_that_need_it() {
        let svc = name_hash("lfo_bus");
        let five = Table { uses: &[Use { resource: RES_MACHINE_ROWS, count: 5 }], provides: &[Pair { a: svc, b: 0 }], ..KEEP };
        let consumer = Table { needs: &[need(svc, 0)], ..KEEP };
        let p = run_p(&[Some(bytes(&consumer))], &[Some(bytes(&five))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Refused(Reason::Fit(CAPK_MACHINE_ROWS)));
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(svc)));
        assert_eq!(p.verdicts[0].result(), E_MISSING);
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
    fn modules_that_own_a_kind_the_other_has_keys_for_form_a_cycle_and_are_refused() {
        let ks = [5u32];
        // A owns kind 90 and has keys for 91; B owns 91 and has keys for 90: neither can load first
        let a = Table { kinds: &[KindDecl { kind: 90, len: 16, entry: 8 }], keys: &[KeyGroup { kind: 91, keys: &ks }], ..KEEP };
        let b = Table { kinds: &[KindDecl { kind: 91, len: 16, entry: 8 }], keys: &[KeyGroup { kind: 90, keys: &ks }], ..KEEP };
        let p = run(&[Some(bytes(&a)), Some(bytes(&b))]);
        assert_eq!(p.verdicts[..2], [Verdict::Refused(Reason::NeedMissing(0)); 2]);
    }

    #[test]
    fn the_keys_of_a_provider_are_judged_like_the_keys_of_a_module() {
        let g = dkrt_sdk::records::GENERIC_KINDS[0];
        let ks = [5u32];
        let svc = |n: &str| [Pair { a: name_hash(n), b: 0 }];
        let (pa, pb) = (svc("a"), svc("b"));
        // a provider with keys for a kind nobody owns
        let unowned = Table { provides: &pa, keys: &[KeyGroup { kind: 90, keys: &ks }], ..KEEP };
        let p = run_p(&[], &[Some(bytes(&unowned))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Refused(Reason::UnownedKind(90)));
        assert_eq!(p.provider_verdicts[0].result(), E_PLAN_KIND);
        // a generic key twice: the later provider is refused
        let first = Table { provides: &pa, kinds: &[KindDecl { kind: g, len: 16, entry: 8 }], keys: &[KeyGroup { kind: g, keys: &ks }], ..KEEP };
        let second = Table { provides: &pb, keys: &[KeyGroup { kind: g, keys: &ks }], ..KEEP };
        let p = run_p(&[], &[Some(bytes(&first)), Some(bytes(&second))]);
        assert_eq!(p.provider_verdicts[..2], [Verdict::Accepted, Verdict::Refused(Reason::DuplicateKey(g, 5))]);
        // a module with a key of a provider is refused (the providers are judged first)
        let user = Table { keys: &[KeyGroup { kind: g, keys: &ks }], ..KEEP };
        let p = run_p(&[Some(bytes(&user))], &[Some(bytes(&first))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Accepted);
        assert_eq!(p.verdicts[0], Verdict::Refused(Reason::DuplicateKey(g, 5)));
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
        let slot = Table { slots: &[Slot { id: 7, flags: 0, entry: 0 }], ..KEEP };
        let hook = Table { hooks: &[Pair { a: 9, b: 4 }], ..KEEP };
        let p = run(&[Some(bytes(&slot)), Some(bytes(&slot)), Some(bytes(&hook)), Some(bytes(&hook))]);
        assert_eq!(
            p.verdicts[..4],
            [Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(7)), Verdict::Accepted, Verdict::Refused(Reason::DuplicateOwner(9))]
        );
    }

    #[test]
    fn a_providers_kinds_slots_and_hooks_have_one_owner_with_the_modules_and_the_other_providers() {
        let svc = |n: &str| Pair { a: name_hash(n), b: 0 };
        let (pa, pb) = ([svc("a")], [svc("b")]);
        let k = [KindDecl { kind: 90, len: 16, entry: 8 }];
        let slot = [Slot { id: 7, flags: 0, entry: 0 }];
        let hook = [Pair { a: 9, b: 4 }];
        let owner = |n| Verdict::Refused(Reason::DuplicateOwner(n));
        // the runtime's kind
        let dup = Table { provides: &pa, kinds: &[KindDecl { kind: KIND_MACHINE, len: 8, entry: 0 }], ..KEEP };
        assert_eq!(run_p(&[], &[Some(bytes(&dup))]).provider_verdicts[0], owner(KIND_MACHINE));
        // another provider's kind, slot and hook site
        for (what, first, second, site) in [
            ("kind", Table { provides: &pa, kinds: &k, ..KEEP }, Table { provides: &pb, kinds: &k, ..KEEP }, 90),
            ("slot", Table { provides: &pa, slots: &slot, ..KEEP }, Table { provides: &pb, slots: &slot, ..KEEP }, 7),
            ("hook", Table { provides: &pa, hooks: &hook, ..KEEP }, Table { provides: &pb, hooks: &hook, ..KEEP }, 9),
        ] {
            let p = run_p(&[], &[Some(bytes(&first)), Some(bytes(&second))]);
            assert_eq!(p.provider_verdicts[..2], [Verdict::Accepted, owner(site)], "{what}");
            assert_eq!(p.provider_verdicts[1].result(), E_PLAN_OWNER);
        }
        // a module that takes what an accepted provider owns is refused (the providers are judged first)
        let prov = Table { provides: &pa, kinds: &k, slots: &slot, ..KEEP };
        let by_kind = Table { kinds: &k, ..KEEP };
        let by_slot = Table { slots: &slot, ..KEEP };
        let p = run_p(&[Some(bytes(&by_kind)), Some(bytes(&by_slot))], &[Some(bytes(&prov))]);
        assert_eq!(p.provider_verdicts[0], Verdict::Accepted);
        assert_eq!(p.verdicts[..2], [owner(90), owner(7)]);
        // a module with keys for a kind a provider owns has an owner for it
        let keys = [0x100u32];
        let user = Table { keys: &[KeyGroup { kind: 90, keys: &keys }], ..KEEP };
        let p = run_p(&[Some(bytes(&user))], &[Some(bytes(&prov))]);
        assert_eq!(p.verdicts[0], Verdict::Accepted);
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
