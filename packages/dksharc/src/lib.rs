//! Static analysis of the SHARC+ DSP section of a module.
//!
//! [`Input`] holds the text of a module's DSP image with its relocations, imports and symbols; [`analyze`] decodes it with the
//! decoder of `sharc_rt` and builds the model of [`model`]: functions, basic blocks with their delay slots, DO loops, calls (module
//! functions and imports) and per-instruction memory operands at form level. A decode that is not confident is reported in
//! `Analysis::undecoded`, never guessed. Register definitions and uses per instruction ([`defuse`]) come from a table generated from the
//! SHARC core's semantics; [`Function::effects`] summarises them per function.

mod analyze;
pub mod defuse;
#[rustfmt::skip]
pub mod defuse_table;
pub mod dkds;
pub mod dko;
mod forms;
mod fxsum;
mod input;
mod model;
pub mod rules;
#[rustfmt::skip]
pub mod rules_table;

pub use analyze::analyze;
pub use input::{Entry, Input, L2_BYTE_BASE, L2_SW_BASE, TextReloc, TextSymbol};
pub use model::*;
