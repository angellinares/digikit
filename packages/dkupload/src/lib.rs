//! Host side of the digikit module upload protocol (`docs/design/digikit-upload.md`).
//!
//! - `codec`: framing and 8-in-7 packing (the one copy; patchkit re-exports `encode_8in7`).
//! - `proto`: status codes, request bodies, reply parsers.
//! - `put`: one upload as a sans-io state machine (BEGIN, DATA with 2 in flight, END, retry by offset).
//! - `session`: a `Transport` (send a SysEx message, receive the next with a timeout) and the commands.
//! - `mock`: an in-memory device that follows the spec's state rules, for tests.
//! - `wasm`: the C ABI the web app loads.
pub mod codec;
#[rustfmt::skip]
mod gen_proto;
pub mod mock;
pub mod proto;
pub mod put;
pub mod session;
pub mod wasm;

pub use codec::{
    Message, decode, decode_8in7, encode, encode_8in7, encode_request, path_ok, sha256,
};
pub use proto::{Entry, Error, Info, ListPage, Status};
pub use put::PutMachine;
pub use session::{Progress, Session, Transport, repack_missing};
