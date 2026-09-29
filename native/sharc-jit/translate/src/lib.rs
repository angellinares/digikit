//! SHARC+ JIT translator (docs/plan-native-emulator.md, P2).
//!
//! Semantics are not written here: the translator reads the transpiled
//! core (tools/sharc_transpile.py's core_g.rs, tables.rs, syms.rs, from
//! tools/sharc_core) and the runtime's boundary functions
//! (native/sharc/src/rt.rs, rt/bnd.rs) as Rust source, and partially
//! evaluates them over each instruction's decoded fields into WebAssembly.

pub mod pe;

pub mod rs {
    pub mod ast;
    pub mod lex;
    pub mod parse;
}

/// The Rust sources the translator evaluates.
pub mod sources {
    #[cfg(sharc_gen)]
    pub const CORE_G: &str = include_str!(concat!(env!("SHARC_GEN_DIR"), "/core_g.rs"));
    #[cfg(sharc_gen)]
    pub const TABLES: &str = include_str!(concat!(env!("SHARC_GEN_DIR"), "/tables.rs"));
    #[cfg(sharc_gen)]
    pub const SYMS: &str = include_str!(concat!(env!("SHARC_GEN_DIR"), "/syms.rs"));
    pub const RT: &str = include_str!("../../../sharc/src/rt.rs");
    pub const BND: &str = include_str!("../../../sharc/src/rt/bnd.rs");
}

/// Parse every source into one program.
#[cfg(sharc_gen)]
pub fn program() -> Result<rs::parse::Program, String> {
    let mut p = rs::parse::Program::default();
    rs::parse::parse_into(&mut p, "rt", sources::RT).map_err(|e| format!("rt.rs: {e}"))?;
    rs::parse::parse_into(&mut p, "rt::bnd", sources::BND).map_err(|e| format!("bnd.rs: {e}"))?;
    rs::parse::parse_into(&mut p, "syms", sources::SYMS).map_err(|e| format!("syms.rs: {e}"))?;
    rs::parse::parse_into(&mut p, "tables", sources::TABLES)
        .map_err(|e| format!("tables.rs: {e}"))?;
    rs::parse::parse_into(&mut p, "core", sources::CORE_G).map_err(|e| format!("core_g.rs: {e}"))?;
    Ok(p)
}
