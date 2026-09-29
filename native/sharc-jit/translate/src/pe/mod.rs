//! The partial evaluator: runs the transpiled core over an instruction's
//! decoded fields and the facts known at translation time, emitting
//! WebAssembly for whatever depends on run-time values.

pub mod eval;
pub mod ir;
pub mod mach;
pub mod ops;
pub mod region;
pub mod types;
pub mod val;
pub mod wide;
