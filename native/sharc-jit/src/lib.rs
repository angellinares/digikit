//! SHARC+ block translation to WebAssembly (plan phase P1: measurements).
//!
//! - [`wasmscan`]: read a module and lift functions into modules of their
//!   own (the ahead-of-time block functions stand in for translator output).
//! - [`split`]: split a `wasm-frames` build into a runtime module plus one
//!   module per block region, linked through imports and the shared table,
//!   the shape a run-time translator produces.
//! - `bin/wasm_frames.rs`: the frame pack under wasmtime.
//! - `bin/jit_probe.rs`: compile latency and speed of the split modules.

pub mod host;
pub mod split;
pub mod wasmscan;
