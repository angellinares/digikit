//! `fx` service, record handlers (interface B): the effect records of the `register` section, and the effect parts of the DSP
//! section. The registry walker (`registry.rs`) and the DSP loader (`dsp/mod.rs`) call them through `handlers::Service`.
use crate::{fxpage, fxsend};
use core::marker::PhantomData;
use dkrt_abi::core_api::StaticCore;
use dkrt_abi::dsp::{DspSec, MAX_FX};
use dkrt_abi::mem::rd32;
use dkrt_abi::registry::each;
use dkrt_abi::service::{tick_phase, ApplyCtx, CheckCtx, Claim, Declare, RowWrite, Service};
use dkrt_sdk::*;

/// The fx service over the core `C` (the runtime's `RealCore`).
pub struct Fx<C>(PhantomData<C>);

impl<C: StaticCore> Service for Fx<C> {
    unsafe fn declare(d: &mut Declare) {
        d.kind(fxpage::KIND_EFFECT, fxpage::LEN_EFFECT);
    }

    unsafe fn check(step: u32, c: &CheckCtx) -> i32 {
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
                    let r = fxpage::check_record::<C>(b);
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
    unsafe fn apply_record(kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        if kind == fxpage::KIND_EFFECT {
            fxpage::apply::<C>(c.owner, body)
        } else {
            OK
        }
    }

    unsafe fn remove(owner: u8) {
        fxpage::remove::<C>(owner);
    }

    unsafe fn tick(phase: u32) {
        if phase == tick_phase::LATE {
            fxpage::tick::<C>();
        }
    }

    unsafe fn cap(kind: u8) -> Option<(u32, u32)> {
        use dkrt_sdk::proto::*;
        if kind == CAPK_FX_SLOTS {
            Some((crate::dsp::fx_free_rows::<C>(), crate::dsp::fx_used::<C>()))
        } else if kind == CAPK_SEND_SLOTS {
            Some((fxsend::slots::<C>(), fxsend::used()))
        } else {
            None
        }
    }

    unsafe fn attribute(site: u32, id: u32) -> Option<usize> {
        // every site but the voice's names an effect row (the master route, the send route, or a site this runtime does not know)
        if site != dkrt_sdk::fault::SITE_VOICE {
            crate::dsp::module_of_fx_table_row::<C>(id)
        } else {
            None
        }
    }

    unsafe fn declare_fx(
        rec: usize,
        ordinal: u32,
        flags: Option<u32>,
        par: Option<[u32; 8]>,
    ) -> Option<bool> {
        Some(crate::dsp::fx_set_declared::<C>(rec, ordinal, flags, par))
    }

    unsafe fn dsp_check_sections(sec: &DspSec) -> i32 {
        crate::dsp::check_sections::<C>(sec)
    }

    unsafe fn dsp_check_rows(sec: &DspSec) -> i32 {
        crate::dsp::check_rows::<C>(sec)
    }

    unsafe fn dsp_claim(c: &mut Claim) {
        crate::dsp::claim::<C>(c)
    }

    unsafe fn dsp_release(rec: usize) {
        crate::dsp::release::<C>(rec)
    }

    unsafe fn dsp_clear(rec: usize) -> bool {
        crate::dsp::clear_fx_rows::<C>(rec)
    }

    unsafe fn dsp_row_step(rec: usize, want: &mut u32, write: bool) -> Option<RowWrite> {
        crate::dsp::row_step::<C>(rec, want, write)
    }
}
