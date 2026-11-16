//! `fx` service: effect pages, FX parameters, routes and send amounts (docs/plans/runtime-and-services.md, R9).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi` (`CoreApi`, interface C) and the SDK crate.
//! Every function that needs the core takes it by reference (`core: &C`, `C: CoreApi`): the runtime passes its `RealCore` (a zero-sized type, so the
//! calls are static and cost what a direct call costs) and a host test passes a `FakeCore`. The runtime names `RealCore` once, in the generated service
//! table (`gen_services.rs`), and in `handlers::exports`.
//! Layout (the service template, `sdk/templates/service`): `records` (the `Service` handlers), `slots` `tests/` (host tests over `FakeCore`); `fxpage`, `fxsend` and `dsp/` are the service's own modules.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod dsp;
pub mod fxpage;
pub mod fxsend;
pub mod records;
pub mod slots;
