//! Records and exit checks used only by the diagnostic executable.

use serde::Serialize;

pub(crate) const VECTOR_RAM: u32 = 0x4000_0000;

pub(crate) const LIMIT: u64 = 1_000_000_000;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InstructionObservation {
    pub(crate) icount: u64,
    pub(crate) pc: u32,
    pub(crate) sr: u16,
}

pub(crate) fn stop_success(stop: &str, target: &str, ready: bool, icount: u64, limit: u64) -> bool {
    (target == "ready" && stop == "VERIFIED_READY" && ready)
        || (target == "limit" && stop == "instruction limit" && icount >= limit)
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "record fields are printed through Debug in the CLI report"
)]
pub(crate) struct TaskCreate {
    pub(crate) at: u64,
    pub(crate) tcb: u32,
    pub(crate) entry: u32,
    pub(crate) prio: u32,
    pub(crate) stack: u32,
    pub(crate) size: u32,
}
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "record fields are printed through Debug in the CLI report"
)]
pub(crate) struct Pend {
    pub(crate) at: u64,
    pub(crate) return_pc: u32,
    pub(crate) sem: u32,
    pub(crate) value: Option<u32>,
}
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "record fields are printed through Debug in the CLI report"
)]
pub(crate) struct Context {
    pub(crate) at: u64,
    pub(crate) current_tcb: u32,
    pub(crate) ready_cursor: u32,
}
