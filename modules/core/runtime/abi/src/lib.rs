//! dkrt-abi: the interfaces of the digikit runtime (docs/ARCHITECTURE.md, "Three interfaces") and the pure types and constants they share.
//!
//! - `core_api`: interface C, `CoreApi`: what a service calls in the core (parameter rows, the module table, settings, the log, jobs, the DSP link).
//! - `service`: interface B, `Service`, `ServiceRow` and `ServiceTable`: what the core calls on a service, with the context types of those calls.
//! - `platform`: `Platform`, the interface to the device and the firmware under the core, with `DeviceProfile` (`profiles`: one generated constant per device file).
//! - `plan`: `Verdict` and `Reason`, the boot planner's answer for a module (R7; R11 builds the module states on it).
//! - `dsp`, `registry`, `mem`: types, constants and helpers with no state. The core owns the objects (`DKRT_DSP`, `DKRT_REG`) and hands them out through `CoreApi`.
//!
//! This crate never depends on the runtime crate: a function that needs the core's state belongs to `CoreApi`, so the dependency arrows between the core and the
//! services point at this crate and nowhere else. `#![no_std]`; no allocation, no vtables.
#![no_std]

pub mod core_api;
pub mod dsp;
pub mod link;
pub mod mem;
pub mod plan;
pub mod platform;
pub mod profiles;
pub mod registry;
pub mod service;
pub mod storage;

/// Module records of the runtime (`[limits] modules` of the device files).
pub const MAX_MODULES: usize = dkrt_sdk::device::LIMIT_MODULES as usize;
