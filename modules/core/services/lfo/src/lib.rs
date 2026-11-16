//! `lfo` service: the shapes an LFO is read through, in an owner-tracked table (docs/plans/runtime-and-services.md, R7 and R12-1).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi` (`CoreApi`, interface C) and the SDK crate: the
//! service never names the runtime. The runtime names `Lfo` once, in the generated service table (`gen_services.rs`). Layout: [`records`] (the
//! `lfo_shape` handlers), [`slots`] (none yet), `tests/` (host tests over `FakeCore`).
#![no_std]
#[cfg(test)]
extern crate std;
pub mod records;
pub mod slots;
