//! Actual loader/DSP cleanup under host-only transport and pointer side effects.
//! No device ACK/block-tail/callback quiescence claim. All shared runtime state is
//! serialized; fake CF addresses are logged at free/fini, never host-dereferenced.
use super::*;
use crate::core_api::{CoreApi, REAL_CORE};
use dkrt_abi::link::{Owner, Sequence};
use crate::test_lock::LOCK;
use std::vec::Vec;

const CF: u32 = 0x8000;
const IMAGE: u32 = 0xd000;
const PUB_PC: u32 = 0xc000;
static VOICE_INFO: [u32; 12] = [0x4459_4e49, 0, 1, 0x20000000, 4096, 0, 4, 8, 0, 0, 0, 0];
static FX_INFO: [u32; 8] = [0x4446_5849, 0, 1, 0, 0, 4, 0, 24];

#[derive(Clone, Copy)]
enum Completion {
    Ack,
    Refuse,
    ErrorAt(u32),
    Missing,
}
#[derive(Debug, PartialEq)]
enum Event {
    Word(u32, u32, u32),
    Fini,
    Registry,
    Free(u32),
}
struct Link {
    completion: Completion,
    original_failure: bool,
    tries: u32,
    accepted: u32,
    polls: u32,
    no_fx_info: bool,
    events: Vec<Event>,
}
static mut LINK: Link = Link {
    completion: Completion::Ack,
    original_failure: true,
    tries: 0,
    accepted: 0,
    polls: 0,
    no_fx_info: false,
    events: Vec::new(),
};

pub fn voice_info() -> *const u32 {
    VOICE_INFO.as_ptr()
}
pub unsafe fn fx_info() -> *const u32 {
    if LINK.no_fx_info {
        core::ptr::null()
    } else {
        FX_INFO.as_ptr()
    }
}
pub unsafe fn send_word(win: u32, at: u32, buf: *const u32, len: u32) -> i32 {
    assert_eq!(len, 4);
    LINK.tries += 1;
    if matches!(LINK.completion, Completion::Refuse) {
        return -1;
    }
    LINK.accepted += 1;
    LINK.events.push(Event::Word(win, at, *buf));
    0
}
pub unsafe fn send_image(_: u32, _: *const u8, _: u32) -> i32 {
    panic!("retained loader must not send another image")
}
pub unsafe fn poll_state() -> (u32, u32) {
    LINK.polls += 1;
    if LINK.original_failure {
        LINK.original_failure = false;
        return (3, 91);
    }
    match LINK.completion {
        Completion::Ack => (2, 0),
        Completion::ErrorAt(n) if LINK.accepted >= n => (3, 92),
        Completion::Missing => (1, 0),
        Completion::Refuse => panic!("refused write must not poll"),
        _ => (2, 0),
    }
}
pub unsafe fn fini(rec: usize, addr: u32) {
    assert_eq!((rec, addr), (0, 1));
    assert!(!entry::has_tick(0), "the tick stops before fini");
    assert_eq!(dsp::ds().row_pub[0], 0);
    assert_eq!(dsp::ds().fx_pub[0], 0);
    assert_eq!(dsp::ds().mod_len[0], 256); // invalidated but still owned at fini
    LINK.events.push(Event::Fini);
}
pub unsafe fn registry_remove(owner: u8) {
    assert_eq!(owner, 1);
    assert!(LINK.events.contains(&Event::Fini));
    assert_eq!(dsp::ds().mod_len[0], 256);
    LINK.events.push(Event::Registry);
}
pub unsafe fn arena_free(p: *mut u8) -> bool {
    if p as u32 == CF {
        assert_eq!(dsp::ds().mod_len[0], 0);
    }
    LINK.events.push(Event::Free(p as u32));
    true
}

/// One UI pass: the link settles and starts, then the loader steps (the order of `dkrt_tick`).
unsafe fn pass() {
    crate::link::pump();
    crate::loader::dsp_step();
}

/// No transfer is on the wire and the loader has none in flight.
unsafe fn idle_link() {
    *crate::link::lk() = crate::link::Link::new(crate::link::DspTransport);
    crate::link::lk().spin = 20_000;
    dsp::ds().ticket = 0;
}

unsafe fn seed(completion: Completion, fx: bool, published: bool, queued: bool) {
    *st() = core::mem::zeroed();
    *dsp::ds() = core::mem::zeroed();
    *boot::info() = core::mem::zeroed();
    dsp::init_once();
    LINK = Link {
        completion,
        original_failure: true,
        tries: 0,
        accepted: 0,
        polls: 0,
        no_fx_info: false,
        events: Vec::new(),
    };
    let s = st();
    s.modules[0].used = 1;
    s.modules[0].base = CF;
    s.modules[0].size = 256;
    s.modules[0].fini = 1;
    entry::publish_tick(0, 0x9000);
    let d = dsp::ds();
    d.phase = if published {
        dsp::PH_ROWS
    } else {
        dsp::PH_SEND
    };
    // a transfer of the loader is on the wire: the link answers it with the seeded failure at the next pump
    *crate::link::lk() = crate::link::Link::new(crate::link::DspTransport);
    crate::link::lk().spin = 20_000;
    d.ticket = crate::link::lk().test_in_flight(Owner::Loader).id as u32;
    d.pending.rec = 0;
    d.pending.boot_idx = 0;
    d.pending.send_buf = if published { 0 } else { IMAGE };
    d.row_steps = 0x11223344;
    d.mod_len[0] = 256;
    d.transfers = 1;
    if fx {
        d.fx_pc[0] = PUB_PC;
        d.fx_owner[0] = 1;
        d.fx_pub[0] = u8::from(published);
        d.fx_owner[1] = 1; // later row whose transfer fails
        d.mod_fx[0] = 3;
        d.step = 18;
    } else {
        d.row_pc[0] = PUB_PC;
        d.row_owner[0] = 1;
        d.row_pub[0] = u8::from(published);
        d.row_owner[1] = 1;
        d.mod_rows[0] = 3;
        d.step = 16; // the second voice row, after the latch-mask publication step
    }
    if queued {
        s.modules[1].used = 1;
        d.mod_off[1] = 256;
        d.mod_len[1] = 256;
        d.nwait = 1;
        d.waiting[0].rec = 1;
        d.waiting[0].send_buf = 0xe000;
    }
}

#[test]
fn voice_latch_mask_is_published_before_the_type() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, true, false);
        LINK.original_failure = false;
        let d = dsp::ds();
        idle_link();
        d.step = 13;
        d.row_pub[0] = 0;
        d.row_latched[0] = 5;
        d.row_type[0] = 17;
        while LINK.events.len() < 4 {
            pass();
        }
        let at = 24 * VOICE_INFO[6];
        assert_eq!(LINK.events, [Event::Word(1, at + 22, 5), Event::Word(1, at + 23, 0), Event::Word(1, at + 10, 0), Event::Word(1, at, 17)]);
        assert_eq!(dsp::ds().row_pub[0], 1);
    }
}

unsafe fn assert_retained(fx: bool, pending_buf: u32) {
    let d = dsp::ds();
    assert!(dsp::retained());
    assert!(!entry::has_tick(0), "teardown stops the tick before fini");
    assert_eq!(d.pending.rec, 0);
    assert_eq!(d.pending.send_buf, pending_buf);
    assert_eq!(d.mod_len[0], 256);
    assert_eq!(d.mod_len[1], 256);
    // Publication metadata is inline now, rather than an arena-allocated step list.
    assert_eq!(if fx { d.fx_pc[0] } else { d.row_pc[0] }, PUB_PC);
    assert_eq!(d.nwait, 1);
    assert_eq!(d.waiting[0].send_buf, 0xe000);
    assert_eq!(d.transfers, 1);
    assert_eq!(d.failures, 1);
    assert_eq!(st().last_result, E_DSP_XFER);
    assert_eq!(boot::info().results[0], E_DSP_XFER);
    assert_eq!(st().modules[0].used, 1);
    assert_eq!(st().modules[0].base, CF);
    assert_eq!(dsp::cleanup_test_first_fit(256, 64), Some(512));
    if fx {
        assert_eq!((d.fx_owner[0], d.fx_pub[0]), (1, 1));
    } else {
        assert_eq!((d.row_owner[0], d.row_pub[0]), (1, 1));
    }
    assert!(!LINK
        .events
        .iter()
        .any(|e| matches!(e, Event::Fini | Event::Registry | Event::Free(_))));
    let frozen = (
        d.row_steps,
        d.ticks,
        d.step,
        LINK.tries,
        LINK.polls,
        LINK.events.len(),
    );
    for _ in 0..3 {
        pass();
    }
    let mut one = Sequence::new();
    one.push(dsp::WIN_FX, 0, 0xabcdef00);
    assert!(!REAL_CORE.link_run(Owner::Core, &one), "a retained link sends nothing");
    assert_eq!(dsp::reserve(2, &dsp::Job::empty()), E_BUSY);
    dsp::cancel(0);
    dsp::release_range(0);
    dsp::launch(d.waiting[0]);
    assert_eq!(
        (
            d.row_steps,
            d.ticks,
            d.step,
            LINK.tries,
            LINK.polls,
            LINK.events.len()
        ),
        frozen
    );
    assert_eq!(d.failures, 1);
    assert_eq!(d.nwait, 1);
    assert_eq!(d.pending.rec, 0);
}

unsafe fn failure_case(completion: Completion) {
    seed(completion, false, true, true);
    pass();
    assert_retained(false, 0);
    assert_eq!(
        dsp::ds().last_dl_err,
        if matches!(completion, Completion::ErrorAt(_)) {
            92
        } else {
            91
        }
    );
}
#[test]
fn dsp_step_reports_a_failure_without_acting_on_it() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, true, true);
        let d = dsp::ds();
        crate::link::pump();
        assert_eq!(dsp::step(), dsp::Advance::Failed(E_DSP_XFER));
        // the module is still up and the DSP loader still counts nothing: the loader decides next
        assert!(entry::has_tick(0));
        assert_eq!((d.failures, st().modules[0].used, boot::info().results[0]), (0, 1, 0));
        dsp::fail_done(false, E_DSP_XFER);
        assert!(dsp::retained());
        assert_eq!((d.failures, st().last_result), (1, E_DSP_XFER));
    }
}
#[test]
fn invalidation_refusal_retains_and_stops() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        failure_case(Completion::Refuse);
        assert_eq!(LINK.accepted, 0);
    }
}
#[test]
fn invalidation_error_retains_and_stops() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        failure_case(Completion::ErrorAt(1));
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn invalidation_missing_ack_retains_and_stops() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        failure_case(Completion::Missing);
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn fx_partial_cleanup_does_not_discard_publication() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::ErrorAt(3), true, true, true);
        pass();
        assert_retained(true, 0);
        assert_eq!(LINK.accepted, 3); // no subsequent cleanup writes after failure
    }
}
#[test]
fn missing_fx_info_retains_published_row() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, true, true, true);
        LINK.no_fx_info = true;
        pass();
        assert_retained(true, 0);
        assert_eq!(LINK.accepted, 0);
    }
}
unsafe fn successful_case(fx: bool, published: bool) {
    seed(Completion::Ack, fx, published, true);
    pass();
    assert!(!dsp::retained());
    assert!(!entry::has_tick(0));
    assert_eq!(st().modules[0].used, 0);
    assert_eq!(dsp::ds().mod_len[0], 0);
    assert_eq!((dsp::ds().row_owner[0], dsp::ds().fx_owner[0]), (0, 0));
    assert_eq!(dsp::cleanup_test_first_fit(256, 64), Some(0));
    assert!(LINK.events.contains(&Event::Fini));
    assert!(LINK.events.contains(&Event::Registry));
    assert!(LINK.events.contains(&Event::Free(CF)));
    assert_eq!(if fx { dsp::ds().fx_pc[0] } else { dsp::ds().row_pc[0] }, 0);
    assert_eq!(LINK.events.contains(&Event::Free(IMAGE)), !published);
    assert_eq!(dsp::ds().phase, dsp::PH_SEND);
    assert_eq!(dsp::ds().pending.rec, 1);
    assert_eq!(dsp::ds().nwait, 0);
    assert_eq!(dsp::ds().transfers, 2);
    assert_eq!(dsp::ds().failures, 1);
}
#[test]
fn acknowledged_voice_cleanup_reclaims_and_advances_queue() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        successful_case(false, true);
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn acknowledged_fx_cleanup_reclaims_after_all_words() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        successful_case(true, true);
        assert_eq!(LINK.accepted, 11);
    }
}
#[test]
fn before_publication_failure_is_reclaimable() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        successful_case(false, false);
        assert_eq!(LINK.accepted, 0);
    }
}
#[test]
fn never_started_cancel_is_reclaimable() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, false, false);
        dsp::ds().send_buf = IMAGE;
        dsp::cancel(0);
        assert_eq!(dsp::ds().mod_len[0], 0);
        assert!(LINK.events.contains(&Event::Free(IMAGE)));
        assert!(!LINK.events.contains(&Event::Free(CF)));
    }
}

#[test]
fn on_loaded_runs_once_per_load_and_after_the_last_row() {
    use crate::handlers::tests::{log, Call, LOCK as FAKE_LOCK, TWO};
    let _f = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, true, false);
        idle_link();
        log().clear();
        let d = dsp::ds();
        d.row_owner = [0; dsp::MAX_ROWS];
        d.fx_owner = [0; dsp::MAX_FX];
        d.step = 0; // no row of module 0 in the fake's list: every row is in
        crate::loader::dsp_step_with(TWO);
        // each service hears it once, in list order, and the load is over
        assert_eq!(*log(), vec![Call::OnLoaded { svc: 1, rec: 0 }, Call::OnLoaded { svc: 2, rec: 0 }]);
        assert_eq!(dsp::ds().phase, dsp::PH_IDLE);
        assert_eq!(boot::info().loaded, 1);
        // an idle loader has nothing to list
        log().clear();
        crate::loader::dsp_step_with(TWO);
        assert!(log().is_empty());
    }
}

#[test]
fn attribution_names_the_module_of_every_site_the_watch_reports() {
    use crate::handlers::services;
    use dkrt_sdk::fault::{SITE_FX_MASTER, SITE_FX_SEND, SITE_NONE, SITE_VOICE};
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, true, false);
        let d = dsp::ds();
        d.row_owner[0] = 2;
        d.row_type[0] = 7;
        d.fx_owner[1] = 3;
        // a voice site: the module that owns the machine type
        assert_eq!(services().attribute(SITE_VOICE, 7), Some(1));
        assert_eq!(services().attribute(SITE_VOICE, 8), None);
        // every other site is an effect row of the chain's table (first row 0 in the test table)
        assert_eq!(services().attribute(SITE_FX_MASTER, 1), Some(2));
        assert_eq!(services().attribute(SITE_FX_SEND, 1), Some(2));
        assert_eq!(services().attribute(SITE_NONE, 1), Some(2));
        assert_eq!(services().attribute(9, 1), Some(2));
        assert_eq!(services().attribute(SITE_FX_MASTER, 0), None);
    }
}

// Optional diagnostic firmware link boundary, absent in this host fixture.
#[no_mangle]
pub extern "C" fn dl_diag(_: *mut u32, _: u32) -> i32 {
    0
}

// The sample backing plug-in point, absent in this host fixture (version 0 is refused).
#[no_mangle]
pub extern "C" fn dkpool_version() -> u32 {
    0
}
#[no_mangle]
pub extern "C" fn dkpool_load(_: u32, _: *const u8, _: *const u8, _: *mut u8) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpool_release(_: *const u8) {}
#[no_mangle]
pub extern "C" fn dkpool_check(_: *const u8) -> i32 {
    -15
}
#[no_mangle]
pub extern "C" fn dkpool_free_frames() -> u32 {
    0
}
#[no_mangle]
pub extern "C" fn dkpool_tick() {}

// core/sdhc-large, absent in this host fixture.
#[no_mangle]
pub extern "C" fn sdhc_large_range(_: u32, _: u32) {}
#[no_mangle]
pub extern "C" fn sdhc_large_ctl(_: u32, _: u32) -> u32 {
    0
}

// The +Drive write adapter, absent in this host fixture (version 0 is refused).
#[no_mangle]
pub extern "C" fn dkpw_version() -> u32 {
    0
}
#[no_mangle]
pub extern "C" fn dkpw_begin(_: *const u8, _: u32, _: *const u8, _: u32) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpw_write(_: *const u8, _: u32) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpw_close(_: *mut u8) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpw_abort() {}
#[no_mangle]
pub extern "C" fn dkpw_delete(_: *const u8) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpw_recover(_: *mut u32) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpw_staging() -> *const u8 {
    b"/.dkstage\0".as_ptr()
}

#[no_mangle]
pub extern "C" fn dkpool_cancel(_: u32) {
    panic!("host cleanup tests have no pending sample-pool requests")
}

// No firmware file systems or DSP tick callback are installed in this host fixture.
#[no_mangle]
pub static dkrt_device_id: u32 = 0;
#[no_mangle]
pub static dkrt_sample_fs: [u32; 4] = [0; 4];
#[no_mangle]
pub static dkrt_files_fs: [u32; 3] = [0; 3];
#[no_mangle]
pub static dkrt_store_first: u32 = 0;
#[no_mangle]
pub static dkrt_store_sectors: u32 = 0;
#[no_mangle]
pub extern "C" fn dl_tick_cb_drop(start: u32, end: u32) {
    assert_eq!((start, end), (CF, CF + 256));
    unsafe { assert!(LINK.events.contains(&Event::Fini)); }
}

/// The other firmware anchors that the platform tests keep reachable: nothing in the host tests may write the card or send MIDI through them
/// (the tests of platform behaviour use `FakePlatform`), and the effect pages' table is not in the image.
#[export_name = "cf.storage.fn.block.write"]
pub extern "C" fn block_write(_: u32, _: u32, _: *const u8) -> i32 {
    panic!("host tests must not write firmware storage")
}
#[export_name = "cf.midi.fn.send"]
pub extern "C" fn midi_send(_: u32, _: *const u8, _: u32) {
    panic!("host tests must not send MIDI through the firmware")
}
#[no_mangle]
pub extern "C" fn fxpages_info() {}

#[export_name = "cf.storage.fn.block.read"]
pub extern "C" fn block_read(_: u32, _: u32, _: *mut u8) -> i32 {
    panic!("host cleanup tests must not read firmware storage")
}

unsafe extern "C" fn test_sink(_: *const crate::dlog::Ev) {}

/// A module without `fini` that set a sink: the unload releases the channel (it would point into freed memory).
#[test]
fn unload_of_a_module_without_fini_releases_its_sink() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        seed(Completion::Ack, false, true, true);
        st().modules[0].fini = 0;
        LINK.events.push(Event::Fini); // the registry hook of this harness expects a fini before it; none runs
        let prev = crate::dlog::enter_module(0, crate::dlog::PHASE_INIT);
        assert_eq!(crate::dlog::dk_sink_set(0, test_sink as *const () as usize as *mut u8), OK);
        crate::dlog::leave_module(prev);
        assert!(crate::dlog::sink_on(0));
        pass();
        assert_eq!(st().modules[0].used, 0);
        assert!(!crate::dlog::sink_on(0));
    }
}
