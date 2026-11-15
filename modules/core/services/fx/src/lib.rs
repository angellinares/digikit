//! `fx` service: effect pages, FX parameters, routes and send amounts (docs/plans/runtime-and-services.md, R9).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi` (`CoreApi`, interface C) and the SDK crate.
//! Every function that needs the core is generic over `C: StaticCore` (a zero-sized core), so the code is the same as a direct call; the runtime
//! names its `RealCore` once, in the generated service table (`gen_services.rs`) and in `handlers::exports`.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod fxpage;
pub mod fxsend;
pub mod dsp;
pub mod records;
