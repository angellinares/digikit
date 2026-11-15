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

#[cfg(test)]
mod tests {
    use super::*;
    use dkrt_abi::dsp::{
        FXW_ENABLED, FXW_P0, FXW_PC, FX_FREE_PC, FX_ROW_WORDS, F_FIRST, MIX_ONE, WIN_FX,
    };
    use dkrt_test_support::{Call, FakeCore};

    // `fxpage::info` reads the stub `fxpages_info` of this binary (see `fxpage`'s tests): no magic, so an image without effect pages. The page rows are
    // raw 32-bit addresses, so a host test cannot give it a real table.

    const REC: usize = 3;
    const FIRST_ROW: u32 = 6;

    /// A core whose `dynfx_info` says the effect table starts at row `FIRST_ROW`, with effect row 1 claimed by module record `REC` and published.
    fn core_with_row() -> FakeCore {
        let mut core = FakeCore::new();
        core.fx_table[F_FIRST] = FIRST_ROW;
        let d = core.dsp();
        d.fx_owner[1] = REC as u8 + 1;
        d.fx_pub[1] = 1;
        d.fx_pc[1] = 0x1234;
        core
    }

    /// An image without effect pages refuses an effect record before it writes anything, and its tick does nothing: the fx service runs over a fake core.
    #[test]
    fn without_effect_pages_a_record_is_refused_and_the_tick_is_silent() {
        let core = core_with_row();
        let rec = [0u8; fxpage::LEN_EFFECT as usize];
        let ctx = ApplyCtx {
            owner: REC as u8 + 1,
            reg: core::ptr::null(),
            base: 0,
            inf: core::ptr::null(),
            args: None,
        };
        unsafe {
            assert_eq!(
                <Fx as Service<FakeCore>>::apply_record(
                    &core,
                    fxpage::KIND_EFFECT,
                    rec.as_ptr(),
                    &ctx
                ),
                E_NO_REGISTRY
            );
            assert_eq!(
                <Fx as Service<FakeCore>>::apply_record(&core, 999, rec.as_ptr(), &ctx),
                OK,
                "a kind of another service is not its business"
            );
            <Fx as Service<FakeCore>>::tick(&core, tick_phase::LATE);
            <Fx as Service<FakeCore>>::remove(&core, REC as u8 + 1);
        }
        assert!(
            core.calls().is_empty(),
            "no parameter row, no link write: {:?}",
            core.calls()
        );
    }

    /// A declared effect takes its flags and defaults on its claimed row, and the loader reads its words in the row's publishing order.
    #[test]
    fn a_declared_effect_publishes_its_words_in_order() {
        let core = core_with_row();
        let par = [10, 11, 12, 13, 14, 15, 16, 17];
        unsafe {
            // effect ordinal 0 of the module is the claimed row 1; ordinal 1 does not exist
            assert_eq!(
                <Fx as Service<FakeCore>>::declare_fx(&core, REC, 0, Some(2), Some(par)),
                Some(true)
            );
            assert_eq!(
                <Fx as Service<FakeCore>>::declare_fx(&core, REC, 1, Some(2), None),
                Some(false)
            );
            let d = core.dsp();
            assert_eq!((d.fx_flags[1], d.fx_par[1]), (2, par));
            let at = FX_ROW_WORDS * (FIRST_ROW + 1);
            let step = |n: u32| {
                let mut want = n;
                <Fx as Service<FakeCore>>::dsp_row_step(&core, REC, &mut want, true)
                    .map(|w| (w.win, w.word, w.value))
            };
            assert_eq!(step(0), Some((WIN_FX, at + FXW_P0, 10)));
            assert_eq!(step(7), Some((WIN_FX, at + FXW_P0 + 7, 17)));
            assert_eq!(step(11), Some((WIN_FX, at + FXW_ENABLED, 1)));
            assert_eq!(
                step(17),
                Some((WIN_FX, at + FXW_PC, 0x1234)),
                "the pc goes last: it makes the row visible"
            );
            assert!(step(18).is_none(), "the row has 18 steps");
            assert_eq!(
                <Fx as Service<FakeCore>>::attribute(
                    &core,
                    dkrt_sdk::fault::SITE_FX_MASTER,
                    FIRST_ROW + 1
                ),
                Some(REC)
            );
        }
    }

    /// Clearing the rows of a module sends one sequence for the row (the pc to "free" first), drops its live values and frees the row.
    #[test]
    fn clearing_a_module_sends_the_free_pc_first_and_frees_the_row() {
        let core = core_with_row();
        unsafe {
            assert!(<Fx as Service<FakeCore>>::dsp_clear(&core, REC));
            let at = FX_ROW_WORDS * (FIRST_ROW + 1);
            let wire = core.link.borrow().t.words();
            assert_eq!(
                wire.first().map(|w| (w.0, w.1, w.2)),
                Some((WIN_FX, at + FXW_PC, FX_FREE_PC))
            );
            assert!(wire
                .iter()
                .any(|w| w.1 == at + dkrt_abi::dsp::FXW_MIX && w.2 == MIX_ONE));
            assert_eq!(core.dsp().fx_pub[1], 0);
            // nothing published any more: a second clear sends nothing
            let n = wire.len();
            assert!(<Fx as Service<FakeCore>>::dsp_clear(&core, REC));
            assert_eq!(core.link.borrow().t.words().len(), n);
            <Fx as Service<FakeCore>>::dsp_release(&core, REC);
            assert_eq!(core.dsp().fx_owner[1], 0);
        }
        assert!(!core.calls().iter().any(|c| matches!(c, Call::Log { .. })));
    }
}
