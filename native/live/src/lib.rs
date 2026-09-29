//! Live audio output for the Digitakt II emulator, independent of the
//! native SHARC core (`native/sharc`, a separate lane) for now.
//!
//! Layout:
//! - `ring`: a lock-free SPSC ring buffer of stereo `f32` samples -- the
//!   only thing the real-time cpal callback (`audio`) touches.
//! - `q31`: Q31 (1.31 fixed-point, the SHARC ring-A/DAC format --
//!   `tools/sharc_dac.py`) <-> `f32` conversion.
//! - `repeater`: the bounded FIFO of SPI2 TX frames, repeating the last
//!   frame with its one-shot trig/release words cleared when it runs dry
//!   (`scratchpad/sharc-trig-arm.md`).
//! - `source`: `FrameSource` (the seam a native SHARC core will later
//!   fill), plus a WAV/PCM player and a test tone.
//! - `producer`: the thread that renders frames and pushes them into the
//!   ring, paced by the ring's fill level.
//! - `audio`: opens the cpal output device and drives its callback.
//! - `player`: `LivePlayer`, tying the above together.
//! - `sharc_source`: the native SHARC core as a frame source (a capture's
//!   frames rendered live); `sharc_lib` loads the core's shared library.
//! - `abi`: the C ABI `tools/live_audio.py` calls over ctypes.

pub mod abi;
pub mod audio;
pub mod player;
pub mod priority;
pub mod producer;
pub mod q31;
pub mod repeater;
pub mod resample;
pub mod ring;
pub mod sharc_lib;
pub mod sharc_source;
pub mod source;

pub use player::{LivePlayer, Stats};
pub use ring::{FRAME_LEN, SpscRing, StereoSample};

#[cfg(test)]
mod sharc_capture_tests;
