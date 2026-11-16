//! `samples` service, handlers (interface B): the track-table part of the DSP section and the listing of a loaded module's tracks.
//! It owns no record kind of the `register` section.
use crate::{dsp, tracks};
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::dsp::{DspSec, Job};
use dkrt_abi::service::{tick_phase, Claim, Service};

/// The samples service.
pub struct Samples;

impl<C: CoreApi> Service<C> for Samples {
    unsafe fn on_loaded(_core: &C, rec: usize) {
        tracks::ready(rec);
    }

    unsafe fn tick(core: &C, phase: u32) {
        if phase == tick_phase::EARLY {
            tracks::tick(core);
        }
    }

    unsafe fn dsp_check_sections(_core: &C, sec: &DspSec) -> i32 {
        dsp::check_sections(sec)
    }

    unsafe fn dsp_admit(_core: &C, job: &Job) -> i32 {
        dsp::admit(job)
    }

    unsafe fn dsp_claim(_core: &C, c: &mut Claim) {
        dsp::claim(c)
    }

    unsafe fn dsp_release(core: &C, rec: usize) {
        tracks::remove(core, rec);
    }
}
