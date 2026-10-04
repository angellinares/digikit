//! The runtime the generated SHARC+ core runs on: machine state and the
//! boundary functions the transpiled code calls ([`rt`]), the memory model
//! ([`mem`]), the instruction decoder ([`decode`]), address mapping
//! ([`addressing`]) and the instruction table of an image ([`insn_table`]).
//!
//! `sharc-gen` includes the generated code and `sharc-native` holds the
//! engine, the fast tier and the C ABI; this crate depends on neither.
//! Boundary functions generated code calls on its hot paths are `#[inline]`
//! (or generic), since a call across the crate boundary is otherwise not
//! inlined.

pub mod addressing;
pub mod decode;
pub mod insn_table;
pub mod mem;
pub mod rt;

use rt::St;

/// A block function's result.
pub const EXIT_NEXT: u32 = 0;
pub const EXIT_BUDGET: u32 = 1;
pub const EXIT_TRAP: u32 = 2;
/// Block code internal: EXIT_CHAIN + k continues in the k-th block its
/// region calls directly.
pub const EXIT_CHAIN: u32 = 16;
/// Direct block-to-block calls in a row before returning to the dispatcher.
pub const CHAIN_MAX: u32 = 32;

/// A chained block that was not generated: back to the dispatcher.
pub fn no_block(_s: &mut St) -> u32 {
    EXIT_NEXT
}

pub type BlockFn = fn(&mut St) -> u32;
