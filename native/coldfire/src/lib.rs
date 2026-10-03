//! ColdFire V4e core for the MCF5441x (ISA_C, EMAC; no FPU on this device).
//!
//! Pure: no I/O, no threads, std only, so it can build for wasm32.
//! The decoder is generated from `tools/cfisa/coldfire.json` (encodings from
//! the public ColdFire manuals) by `tools/cfisa/gen.py`; semantics are
//! written by hand in `cpu.rs`.

pub mod cpu;
pub mod decode;
pub mod fused;
#[rustfmt::skip]
mod decode_gen;

pub use cpu::{Bus, BusError, Cpu, InterruptPolicy, RunState, Stop};
pub use decode::{Ea, Insn, Operand, Size, decode, decode_at};
pub use decode_gen::Form;
