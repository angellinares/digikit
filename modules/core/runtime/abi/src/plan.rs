//! The verdict of the boot planner (docs/plans/runtime-and-services.md R7), defined once: the planner (`plan.rs` of the runtime) returns one for each
//! module of the selection, and the loader refuses a module before it loads when it is `Refused`. R11 builds the module states (`state`) and their UI on this
//! type; it does not add another verdict.
use dkrt_sdk::*;

/// Why the planner refused a module. `code` is the load status the module's result shows (`LOADS`, the boot info).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A hard need names a service that needs a newer runtime than this one.
    NeedVersion,
    /// A hard need names a service (its name hash) that neither the runtime nor a provider that loaded provides.
    NeedMissing(u32),
    /// The header table gives keys for a record kind that no service owns.
    UnownedKind(u32),
    /// Another module of the selection owns the kind, the slot or the hook site the module takes.
    DuplicateOwner(u32),
    /// Another module of the selection has the same key for the kind.
    DuplicateKey(u32, u32),
    /// The module's uses do not fit what the earlier modules left of the resource (a `CAPK_*` kind).
    Fit(u8),
}

impl Reason {
    /// The load status (`[load.status]`).
    pub fn code(&self) -> i32 {
        // if-let chains, not a `match`: a dense match becomes a PC-relative jump table, which is not ColdFire code
        if *self == Reason::NeedVersion {
            E_CONTRACT
        } else if let Reason::NeedMissing(_) = *self {
            E_MISSING
        } else if let Reason::UnownedKind(_) = *self {
            E_PLAN_KIND
        } else if let Reason::DuplicateOwner(_) = *self {
            E_PLAN_OWNER
        } else if let Reason::DuplicateKey(..) = *self {
            E_PLAN_KEY
        } else {
            E_PLAN_FIT
        }
    }
}

impl Reason {
    /// The two words `LOADS` v2 and `DkrtBootInfo.detail` carry for a refused module (`[load.state]`): the kind (`UnownedKind`), the service hash (`NeedMissing`), the kind, slot or hook site
    /// (`DuplicateOwner`), the kind and the key (`DuplicateKey`), the capacity kind (`Fit`); zeros when the code says it all.
    pub fn detail(&self) -> [u32; 2] {
        if let Reason::UnownedKind(k) = *self {
            [k, 0]
        } else if let Reason::NeedMissing(h) = *self {
            [h, 0]
        } else if let Reason::DuplicateOwner(k) = *self {
            [k, 0]
        } else if let Reason::DuplicateKey(kind, key) = *self {
            [kind, key]
        } else if let Reason::Fit(c) = *self {
            [c as u32, 0]
        } else {
            [0, 0]
        }
    }
}

/// The planner's answer for one module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    Refused(Reason),
}

impl Verdict {
    /// `OK` for an accepted module, else the reason's load status.
    pub fn result(&self) -> i32 {
        if let Verdict::Refused(r) = *self {
            r.code()
        } else {
            OK
        }
    }
    pub fn is_accepted(&self) -> bool {
        matches!(self, Verdict::Accepted)
    }
}
