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

pub use runtime::{CAPTURE_MAX, Capture, Emulator, GuestCall, REG_LOG_MAX, RegHit, Snapshot, Status};
pub use softfloat::ExecutionPolicy;
