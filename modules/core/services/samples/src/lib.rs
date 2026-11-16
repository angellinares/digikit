//! `samples` service: per-track stereo samples, the sample-pool calls and the peak windows (docs/plans/runtime-and-services.md, R12-1).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi`: `CoreApi` (interface C: the link, the DSP loader state,
//! the job queue, the log and, with `SampleCore::host`, the few platform facts it reads). Every function that needs the core takes it by reference
//! (`core: &C`, `C: SampleCore`): the runtime passes its `RealCore` (a zero-sized type) and a host test passes a `FakeCore`. The state machine of a track is
//! `dkrt_sdk::sample_tracks`; [`tracks`] binds it to the pool adapter (the optional `dkpool_*` symbols of the image) and the table writes over the DSP link,
//! [`peakwin`] runs the peak windows on the job queue, [`dsp`] checks and claims the track-table record of a DSP section. The C entry points a module and
//! the adapters call (`dk_sample_pool_*`, `dkrt_pool_done`, `dkrt_pool_lost`, `dkrt_project_changed`) are thin wrappers in the runtime's `handlers.rs`.
//! Layout: `records` (the `Service` handlers), `slots` (none yet), `dsp/`, `tests/`.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod dsp;
pub mod peakwin;
pub mod records;
pub mod slots;
pub mod tracks;

/// The unit tests of this crate that drive its statics (the client table, the write queue, the peak windows) take this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
