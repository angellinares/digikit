//! Portable bounded Oracle boot diagnostic runtime.

#[cfg(target_arch = "wasm32")]
mod abi;
pub mod card_file;
mod capture;
#[cfg(feature = "cf-profile")]
pub mod cfprof;
#[cfg(feature = "cf-mix")]
pub mod cfmix;
mod common;
mod cputime;
mod guard;
pub mod midi;
pub mod ddr_monitor;
pub mod dsp_patch;
pub mod dsp_stream;
pub mod panel_leds;
#[cfg(feature = "play")]
pub mod pcm_play;
mod ram_clear;
mod runtime;
#[cfg(feature = "sharc")]
pub mod sharc_peer;
mod softfloat;
pub mod spikelog;
mod telemetry;
mod watch;
pub mod wav;

pub use capture::Dspi2Capture;
pub use periph::clock;
pub use common::FlashWrites;
pub use cputime::thread_cpu_ns;
pub use guard::{GUARD_HIT_LIMIT, GuardHit, GuardRange, GuardReport, parse_guard_spec};
pub use telemetry::DiagnosticReport;
pub use watch::{After, FlushHit, WatchConfig, Writer, parse_ranges};

pub use runtime::{
    CAPTURE_MAX, CallResult, CallSpec, Capture, Emulator, GuestCall, REG_LOG_MAX, RegHit, Snapshot,
    Status,
};
pub use softfloat::ExecutionPolicy;
