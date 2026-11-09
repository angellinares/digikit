//! patchkit: firmware modules for Digitakt II and Digitone II.
//!
//! One implementation for the command line and the browser (`wasm32`). It does everything at patch time:
//! `.syx` decode and encode, aPLib pack, HMAC seal, port lockfiles, linking, apply, verify. Toolchains
//! and firmware databases are used only when modules and ports are built (Python, `tools/patchkit/pk_*.py`).

/// A hash of this crate's sources (build.rs): a cache of patched images keys on it.
pub const SRC_ID: &str = env!("PATCHKIT_SRC_ID");

#[macro_use]
pub mod error;
pub mod aplib;
pub mod devdata;
pub mod devgen;
pub mod firmware;
pub mod flash;
pub mod flash_rpc;
pub mod fwgen;
pub mod imageid;
pub mod syx;

pub use error::{Error, Result};
pub mod abigen;
pub mod apply;
pub mod bootrom;
pub mod build;
pub mod desc;
pub mod dkm;
pub mod dkmdsp;
pub mod dkmsynth;
pub mod dko;
pub mod dsp;
pub mod dspscan;
pub mod explain;
pub mod expr;
pub mod fit;
pub mod link;
pub mod live;
pub mod modgen;
pub mod module;
pub mod port;
pub mod preflight;
pub mod prim;
pub mod record;
pub mod register;
pub mod sdkgen;
pub mod typed;
pub mod verify;
pub mod wasm;
