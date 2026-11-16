//! Host tests of the `samples` service over `FakeCore`: the life of a module's track table (admit, claim, load, release) and the ticks of the service. The
//! service has no record kind, so its handlers are the DSP-section ones. The state of the client slots is a static of the crate: each test takes `LOCK`
//! and gives its slots back. The state machine of the tracks and the write queue have unit tests in `src/tracks.rs`, the peak windows in `src/peakwin.rs`.
use dkrt_abi::dsp::{DspSec, Job};
use dkrt_abi::service::{tick_phase, Claim, Service};
use dkrt_sdk::*;
use dkrt_svc_samples::records::Samples;
use dkrt_svc_samples::tracks;
use dkrt_test_support::FakeCore;
use std::sync::{Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(());

type S = Samples;

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The 32-byte track-table record of a DSP section (little-endian words): kind 2, the module's local id, the offsets of its records and control sets.
fn bank(local: u32) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, w) in [2u32, local, 0x100, 0x200].iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    b
}

fn job(bank: Option<&[u8; 32]>) -> Job {
    let mut j = Job::empty();
    if let Some(b) = bank {
        j.bank = b.as_ptr();
    }
    j
}

fn claim(core: &FakeCore, rec: usize, j: &Job) {
    let mut c = Claim {
        rec,
        job: j,
        base: 0x1000,
        byte_shift: 0,
        state_at: 0,
    };
    unsafe { <S as Service<FakeCore>>::dsp_claim(core, &mut c) };
}

#[test]
fn a_module_without_a_track_table_is_not_the_services_business() {
    let _g = lock();
    let core = FakeCore::new();
    unsafe {
        assert_eq!(<S as Service<FakeCore>>::dsp_admit(&core, &job(None)), OK);
        let mut other = bank(1);
        other[0] = 3; // another record kind
        assert_eq!(
            <S as Service<FakeCore>>::dsp_admit(&core, &job(Some(&other))),
            OK
        );
        <S as Service<FakeCore>>::dsp_claim(
            &core,
            &mut Claim {
                rec: 0,
                job: &job(None),
                base: 0,
                byte_shift: 0,
                state_at: 0,
            },
        );
        assert!(!tracks::has(0));
        // a section of another version is not checked here
        let sec: DspSec = std::mem::zeroed();
        assert_eq!(
            <S as Service<FakeCore>>::dsp_check_sections(&core, &sec),
            OK
        );
        <S as Service<FakeCore>>::dsp_release(&core, 0);
    }
    assert!(core.calls().is_empty());
}

#[test]
fn a_track_table_is_admitted_claimed_loaded_and_released() {
    let _g = lock();
    let core = FakeCore::new();
    let (b0, b1, b2) = (bank(1), bank(1), bank(1));
    unsafe {
        assert_eq!(
            <S as Service<FakeCore>>::dsp_admit(&core, &job(Some(&b0))),
            OK
        );
        claim(&core, 4, &job(Some(&b0)));
        assert!(tracks::has(4));
        claim(&core, 5, &job(Some(&b1)));
        assert!(tracks::has(5));
        // both client slots are taken: the next table is refused before the loader reserves anything
        assert_eq!(
            <S as Service<FakeCore>>::dsp_admit(&core, &job(Some(&b2))),
            E_TABLE_FULL
        );
        <S as Service<FakeCore>>::on_loaded(&core, 4);
        // a release gives the slot back; a module that holds none is left alone
        <S as Service<FakeCore>>::dsp_release(&core, 4);
        assert!(!tracks::has(4) && tracks::has(5));
        assert_eq!(
            <S as Service<FakeCore>>::dsp_admit(&core, &job(Some(&b2))),
            OK
        );
        <S as Service<FakeCore>>::dsp_release(&core, 4);
        <S as Service<FakeCore>>::dsp_release(&core, 5);
        assert!(!tracks::has(5));
    }
}

#[test]
fn the_tick_is_early_only_and_needs_the_pool_adapter() {
    let _g = lock();
    let core = FakeCore::new();
    unsafe {
        // no sample-pool adapter in the image: nothing runs
        core.host.pool.set(false);
        <S as Service<FakeCore>>::tick(&core, tick_phase::EARLY);
        // a late tick is not the service's
        core.host.pool.set(true);
        <S as Service<FakeCore>>::tick(&core, tick_phase::LATE);
    }
    assert!(core.calls().is_empty());
    assert!(core.link.borrow().t.wire.is_empty());
    unsafe {
        // with the adapter and no track table: a pass that writes nothing to the DSP
        <S as Service<FakeCore>>::tick(&core, tick_phase::EARLY);
    }
    assert!(core.link.borrow().t.wire.is_empty());
}
