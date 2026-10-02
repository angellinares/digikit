//! Portable bounded Oracle boot diagnostic runtime.

#[cfg(target_arch = "wasm32")]
mod abi;
mod capture;
mod common;
#[cfg(feature = "play")]
pub mod pcm_play;
mod ram_clear;
mod runtime;
#[cfg(feature = "sharc")]
pub mod sharc_peer;
mod softfloat;
mod telemetry;

pub use capture::Dspi2Capture;
pub use telemetry::DiagnosticReport;

pub use runtime::{Emulator, Snapshot, Status};
pub use softfloat::ExecutionPolicy;
