//! `__NAME__` service (docs/plans/runtime-and-services.md 5.12).
//!
//! A neutral crate, loaded as the service file `dks/` makes of it. It reaches the runtime only through `dkrt-abi` (`CoreApi`, interface C) and the SDK crate: every
//! function that needs the core takes it by reference (`core: &C`, `C: CoreApi`), so a host test passes a `FakeCore` and the runtime passes its `RealCore`.
//! Layout: `records` (the `Service` handlers: declare, check, apply, remove, tick), `slots` (the firmware slots and hooks the service fills), `dsp/`
//! (only when the service has parts of the DSP section: add the folder and `pub mod dsp;`), `tests/` (host tests over `FakeCore`).
//! The crate exports only `dk_module` (the entry that carries its `ServiceRow`) and `DKRT_*` state; it has no assembler and names no firmware (that is `dks/`).
#![no_std]
#[cfg(test)]
extern crate std;
pub mod records;
pub mod slots;
