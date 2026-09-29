//! MCF5441x timer and interrupt-controller peripherals (plan
//! `docs/plan-native-emulator.md`, P4 "timers + INTC" lane).
//!
//! Pure: no I/O, no threads, std only in the default build, so the models
//! (everything except the `trace` feature) build for wasm32. `machine::Timers`
//! is the entry point: it wires [`pit::PitBank`], [`dtim::DtimBank`] and
//! [`intc::IntcBank`] together and is the interface a later lane (DMA/SSI/
//! DSPI) or a whole-machine loop drives -- see its module docs.
//!
//! The `trace` feature (default; excluded from a wasm32 build with
//! `--no-default-features`) adds [`trace::Reader`], a parser for the
//! `DT2MMIO` v1 format `emu/mmiotrace.py` writes, and the `mmio-replay`
//! binary that checks this crate's models against a recorded trace -- see
//! `trace`'s module docs for the format and `bin/replay.rs` for the
//! comparison method.

pub mod dsp;
pub mod dspi;
pub mod dtim;
pub mod edma;
pub mod gpio;
pub mod intc;
pub mod machine;
pub mod pit;
pub mod regfile;
pub mod spilink;
pub mod sr;
pub mod ssi;

#[cfg(feature = "trace")]
pub mod trace;

pub use machine::{HostWrite, Raised, Timers};
pub use spilink::DmaLink;
