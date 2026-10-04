//! The generated native SHARC+ core, apart from the engine so that editing
//! the engine or the fast tier does not recompile it.
//!
//! `tools/sharc_transpile.py` and `tools/sharc_rsgen.py` write the code
//! under `out/` (the block part embeds firmware); build with
//! `SHARC_GEN_DIR=<that directory>` to include it. Without it this crate
//! holds only the runtime fallbacks.
//!
//! The generated code names its runtime as `crate::rt::*`, `crate::canon::`,
//! `crate::EXIT_NEXT` and so on, as it did when everything was one crate.
//! `sharc-rt` provides those paths below. Two boundary functions need
//! generated data, so this crate wraps them: `decode_at` (the string table)
//! and `_load_normal_ureg` (the core's `_write_ureg`).

pub use sharc_rt::*;

use sharc_rt::rt::Sym;
#[cfg(not(sharc_gen))]
use sharc_rt::rt::{RT_SYMS, TRAP_INDEX, s_set_r};

/// The runtime under the paths the generated code uses, with the two
/// boundary functions that need generated data replaced.
pub mod rt {
    pub use sharc_rt::rt::*;

    pub mod bnd {
        use super::*;
        pub use sharc_rt::rt::bnd::*;

        /// sequencer.decode_at (the table is the image's, or the decoder's).
        #[inline(always)]
        pub fn decode_at(s: &mut St, _data: (), _base_sw: Option<Int>, pc_sw: Int) -> R<Insn> {
            decode_at_with(s, pc_sw, crate::sym_of)
        }

        /// memory._load_normal_ureg.
        #[inline(always)]
        pub fn _load_normal_ureg(s: &mut St, space: Sym, address: VI, code: Int) -> R<Option<V>> {
            _load_normal_ureg_with(s, space, address, code, crate::write_ureg)
        }
    }
}

/// The core's register write (without generated code: the runtime's).
#[inline(always)]
fn write_ureg(s: &mut St, code: Int, v: V) -> R<()> {
    #[cfg(sharc_gen)]
    {
        generated::core_g::state::_write_ureg(s, code, v)
    }
    #[cfg(not(sharc_gen))]
    {
        if matches!(code, 100 | 101) {
            return Err(TRAP_INDEX);
        }
        s_set_r(s, code, v)
    }
}

use sharc_rt::rt::{Int, R, St, V};

/// The string table's name for a symbol.
#[cfg(sharc_gen)]
pub fn sym_name(s: Sym) -> &'static str {
    generated::syms::SYM_NAMES
        .get(s as usize)
        .copied()
        .unwrap_or("?")
}

#[cfg(not(sharc_gen))]
pub fn sym_name(s: Sym) -> &'static str {
    RT_SYMS.get(s as usize).copied().unwrap_or("?")
}

/// Intern NAME into the generated string table (None if it is unknown).
pub fn sym_of(name: &str) -> Option<Sym> {
    #[cfg(sharc_gen)]
    {
        generated::syms::SYM_NAMES
            .iter()
            .position(|&n| n == name)
            .map(|i| i as Sym)
    }
    #[cfg(not(sharc_gen))]
    {
        RT_SYMS.iter().position(|&n| n == name).map(|i| i as Sym)
    }
}

/// The instruction table paths the generated image uses.
pub mod canon {
    pub use sharc_rt::insn_table::{InsnTable, insn_table};
}

#[cfg(sharc_gen)]
#[allow(
    clippy::all,
    unused,
    non_snake_case,
    non_upper_case_globals,
    unreachable_code
)]
pub mod generated {
    pub mod syms {
        include!(concat!(env!("SHARC_GEN_DIR"), "/syms.rs"));
    }
    pub mod tables {
        include!(concat!(env!("SHARC_GEN_DIR"), "/tables.rs"));
    }
    /// The core with every function forced inline (block code).
    pub mod core_i {
        include!(concat!(env!("SHARC_GEN_DIR"), "/core_i.rs"));
    }
    /// The core with normal inlining (the one-instruction interpreter).
    pub mod core_g {
        include!(concat!(env!("SHARC_GEN_DIR"), "/core_g.rs"));
    }
    #[cfg(sharc_image)]
    pub mod image {
        include!(concat!(env!("SHARC_GEN_DIR"), "/image.rs"));
    }
}
