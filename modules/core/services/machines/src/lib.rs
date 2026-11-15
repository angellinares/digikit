//! `machines` service: run-time machine types, parameters, page clicks, native pages and the voice rows of the DSP section (docs/plans/runtime-and-services.md, R10).
//!
//! A crate of its own, linked into the runtime object. It reaches the runtime only through `dkrt-abi`: `CoreApi` (interface C: the registry and DSP loader
//! state, the type map, the parameter rows, the native page calls), and `Fw` (the firmware's parameter page, for a click's flow). Every function that needs
//! one takes it by reference (`core: &C`, `fw: &impl Fw`): the runtime passes its `RealCore` and `RuntimeFw` (zero-sized types, so the calls are static) and a
//! host test passes a `FakeCore` and a `FakeFw`. The click state (`pageclick`) and the state between `apply` and `apply_record` (`records`) are statics of
//! this crate: one machines service, one set. The runtime's hooks (`dkrt_page_key`, `dkrt_page_turn`) are thin wrappers in `handlers.rs`.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod dsp;
pub mod nativepage;
pub mod pageclick;
pub mod paramapi;
pub mod records;

/// The tests of this crate that drive its statics (the click state and `APPLIED`) take this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
