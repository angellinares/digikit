//! `__NAME__` service (docs/plans/runtime-and-services.md 5.12).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi` (`CoreApi`, interface C) and the SDK crate: every
//! function that needs the core takes it by reference (`core: &C`, `C: CoreApi`), so a host test passes a `FakeCore` and the runtime passes its `RealCore`.
//! Layout: `records` (the `Service` handlers: declare, check, apply, remove, tick), `slots` (the firmware slots and hooks the service fills), `dsp/`
//! (only when the service has parts of the DSP section: add the folder and `pub mod dsp;`), `tests/` (host tests over `FakeCore`).
//! The crate exports no symbol; a service that becomes a `.dks` exports one entry, `dk_module`, which carries its `ServiceRow`.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod records;
pub mod slots;
