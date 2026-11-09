//! Portable bounded Oracle boot diagnostic runtime.

#[cfg(target_arch = "wasm32")]
mod abi;
mod capture;
pub mod card_file;
#[cfg(feature = "cf-mix")]
pub mod cfmix;
#[cfg(feature = "cf-profile")]
pub mod cfprof;
mod common;
mod cputime;
pub mod ddr_monitor;
pub mod dsp_patch;
pub mod dsp_stream;
mod guard;
mod late_send;
pub mod midi;
pub mod panel_leds;
#[cfg(feature = "play")]
pub mod pcm_play;
mod ram_clear;
mod runtime;
#[cfg(feature = "sharc")]
pub mod sharc_peer;
mod softfloat;
mod spans;
pub mod spikelog;
mod telemetry;
mod watch;
pub mod wav;

pub use capture::Dspi2Capture;
pub use common::FlashWrites;
pub use cputime::thread_cpu_ns;
pub use guard::{GUARD_HIT_LIMIT, GuardHit, GuardRange, GuardReport, parse_guard_spec};
pub use periph::clock;
pub use telemetry::DiagnosticReport;
pub use watch::{After, FlushHit, WatchConfig, Writer, parse_ranges};

pub use runtime::{
    CAPTURE_MAX, CallResult, CallSpec, Capture, Emulator, GuestCall, REG_LOG_MAX, RegHit, Snapshot,
    Status,
};
pub use softfloat::ExecutionPolicy;
