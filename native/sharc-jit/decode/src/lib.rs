//! A firmware-free, pure-Rust, `std`-only (no crate dependencies at all)
//! port of the SHARC+ single-PC VISA decoder a run-time JIT calls on every
//! decode: `tools/sharc_core/sequencer.py::decode_at(LoadedMemory, None,
//! pc_sw)`, i.e. `tools/sharc_disasm.py::decode_confident_loaded()` --
//! `decode_loaded_at()` (the largest fully-mapped 6/4/2-byte window)
//! followed by `resolve_confident_width()`'s successor-confidence width
//! correction, both built on `tools/sharc_isa.py`'s `InstructionSet` over
//! `tools/sharcspec/decode_table.json` in VISA mode (`mode="visa"`).
//!
//! Nothing here reads a firmware image at build or run time: the decode
//! *table* (`tools/sharcspec/decode_table.json`) is public-manual-derived
//! data, embedded with `include_str!`; the *image* a caller decodes against
//! is supplied entirely at run time through [`ShortWords`], e.g. via
//! [`image::SegmentImage`] over a `tools/sharc_transpile_run.py::pack_image`
//! blob (also supplied by the caller, never read by this crate).
//!
//! ```
//! use sharc_decode::{Decoder, ShortWords};
//!
//! struct Empty;
//! impl ShortWords for Empty {
//!     fn read_sw(&self, _pc_sw: u32, _size_bytes: u32) -> Option<[u16; 3]> {
//!         None
//!     }
//! }
//!
//! let decoder = Decoder::new();
//! let decoded = decoder.decode_at(&Empty, 0);
//! assert_eq!(decoded.type_name, "unknown");
//! ```
//!
//! # Verification
//! See `native/sharc-jit/decode/examples/`: `compare_insns` checks every
//! record of the native code generator's own decode table
//! (`out/native/opt/gen-final/insns.bin`) against this crate; `compare_sweep`
//! checks a contiguous sweep of every short-word PC in an image's VISA code
//! range(s) against a JSON-lines dump of Python's own
//! `decode_confident_loaded`. Both need firmware-derived scratch files and
//! are not needed to build or use the library.

pub mod decode;
pub mod image;
pub mod isa;
mod json;

pub use decode::{Decoded, Decoder, ShortWords};
pub use image::{SegmentImage, parse_pack_image};
