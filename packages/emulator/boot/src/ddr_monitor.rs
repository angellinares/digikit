//! Typed host diagnostic commands, independent of either generated DSP core.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DdrMonitorRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdrMonitorCommand {
    Arm(DdrMonitorRange),
    Status,
    Clear,
    Disarm,
}

impl DdrMonitorCommand {
    pub fn validate(self) -> Result<(), String> {
        if let Self::Arm(range) = self
            && !(0x8000_0000 <= range.start && range.start < range.end && range.end <= 0xa000_0000)
        {
            return Err("range must be nonempty DDR bytes within 0x80000000..0xa0000000".into());
        }
        Ok(())
    }
}

/// These labels describe the observed access path, not guaranteed guest
/// provenance: host peeks may call the CPU semantic helper too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DdrMonitorKind {
    CpuRead,
    CpuWrite,
    StorageRead,
    StorageWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DdrMonitorHit {
    pub address: u32,
    pub width: u32,
    pub kind: DdrMonitorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DdrMonitorStatus {
    /// Generated-core profile label, not a firmware-version assertion.
    pub active_core_profile: String,
    pub dsp_icount: u64,
    pub armed: bool,
    pub range: Option<DdrMonitorRange>,
    pub first: Option<DdrMonitorHit>,
}
