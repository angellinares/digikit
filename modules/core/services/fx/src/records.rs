//! `fx` service, record handlers (interface B): the effect records of the `register` section, and the effect parts of the DSP
//! section. The registry walker (`registry.rs`) and the DSP loader (`dsp/mod.rs`) call them through `handlers::Service`.
use crate::{fxpage, fxsend};
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::dsp::{DspSec, MAX_FX};
use dkrt_abi::mem::rd32;
use dkrt_abi::registry::each;
use dkrt_abi::service::{tick_phase, ApplyCtx, CheckCtx, Claim, Declare, RowWrite, Service};
use dkrt_sdk::*;

/// The fx service; every method works over the core it is given.
pub struct Fx;

impl<C: CoreApi> Service<C> for Fx {
    unsafe fn declare(d: &mut Declare) {
        d.kind(fxpage::KIND_EFFECT, fxpage::LEN_EFFECT);
    }

    unsafe fn check(core: &C, step: u32, c: &CheckCtx) -> i32 {
        use dkrt_abi::service::check_step as st;
        if step == st::COUNTS {
            // the most effects the chain takes
            let mut ne = 0u32;
            each(c.reg, |kind, _| {
                if kind == fxpage::KIND_EFFECT {
                    ne += 1;
                }
            });
            if ne as usize > MAX_FX {
                return E_FORMAT;
            }
            OK
        } else if step == st::EFFECTS {
            // effect pages need core/fxpages; each record is checked on its own, and an effect has one page
            let mut fx_seen = 0u32;
            let mut bad_fx = OK;
            each(c.reg, |kind, b| {
                if kind == fxpage::KIND_EFFECT && bad_fx == OK {
                    let r = fxpage::check_record(core, b);
                    let fx = rd32(b);
                    if r == OK && fx < 32 && fx_seen & (1 << fx) != 0 {
                        bad_fx = E_FORMAT;
                    } else if r != OK {
                        bad_fx = r;
                    }
                    if fx < 32 {
                        fx_seen |= 1 << fx;
                    }
                }
            });
            bad_fx
        } else {
            OK
        }
    }

    /// An effect record, in file order: its page, parameter rows and list entry are written (the effect's row in the chain is claimed).
    unsafe fn apply_record(core: &C, kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        if kind == fxpage::KIND_EFFECT {
            fxpage::apply(core, c.owner, body)
        } else {
            OK
        }
    }

    unsafe fn remove(core: &C, owner: u8) {
        fxpage::remove(core, owner);
    }

    unsafe fn tick(core: &C, phase: u32) {
        if phase == tick_phase::LATE {
            fxpage::tick(core);
        }
    }

    unsafe fn fx_entry(core: &C, index: u32, out: &mut DkrtFx) -> i32 {
        fxsend::fx_entry(core, index, out)
    }

    unsafe fn cap(core: &C, kind: u8) -> Option<(u32, u32)> {
        use dkrt_sdk::proto::*;
        if kind == CAPK_FX_SLOTS {
            Some((crate::dsp::fx_free_rows(core), crate::dsp::fx_used(core)))
        } else if kind == CAPK_SEND_SLOTS {
            Some((fxsend::slots(core), fxsend::used()))
        } else {
            None
        }
    }

    unsafe fn attribute(core: &C, site: u32, id: u32) -> Option<usize> {
        // every site but the voice's names an effect row (the master route, the send route, or a site this runtime does not know)
        if site != dkrt_sdk::fault::SITE_VOICE {
            crate::dsp::module_of_fx_table_row(core, id)
        } else {
            None
        }
    }

    unsafe fn declare_fx(
        core: &C,
        rec: usize,
        ordinal: u32,
        flags: Option<u32>,
        par: Option<[u32; 8]>,
    ) -> Option<bool> {
        Some(crate::dsp::fx_set_declared(core, rec, ordinal, flags, par))
    }

    unsafe fn dsp_check_sections(core: &C, sec: &DspSec) -> i32 {
        crate::dsp::check_sections(core, sec)
    }

    unsafe fn dsp_check_rows(core: &C, sec: &DspSec) -> i32 {
        crate::dsp::check_rows(core, sec)
    }

    unsafe fn dsp_claim(core: &C, c: &mut Claim) {
        crate::dsp::claim(core, c)
    }

    unsafe fn dsp_release(core: &C, rec: usize) {
        crate::dsp::release(core, rec)
    }

    unsafe fn dsp_clear(core: &C, rec: usize) -> bool {
        crate::dsp::clear_fx_rows(core, rec)
    }

    unsafe fn dsp_row_step(core: &C, rec: usize, want: &mut u32, write: bool) -> Option<RowWrite> {
        crate::dsp::row_step(core, rec, want, write)
    }
}
