//! Actual loader/DSP cleanup under host-only transport and pointer side effects.
//! No device ACK/block-tail/callback quiescence claim. All shared runtime state is
//! serialized; fake CF addresses are logged at free/fini, never host-dereferenced.
use super::*;
use std::sync::Mutex;
use std::vec::Vec;

static LOCK: Mutex<()> = Mutex::new(());
const CF: u32 = 0x8000;
const IMAGE: u32 = 0xd000;
const STEPS: u32 = 0xc000;
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
    s.modules[0].slot_mask = 1;
    s.modules[0].slot_fn[0] = 0x9000;
    s.slots[0] = 0x9000;
    let d = dsp::ds();
    d.phase = if published {
        dsp::PH_ROWS
    } else {
        dsp::PH_SEND
    };
    d.started = 1;
    d.pending.rec = 0;
    d.pending.boot_idx = 0;
    d.pending.send_buf = if published { 0 } else { IMAGE };
    d.word = 0x11223344;
    d.mod_len[0] = 256;
    d.step_list[0] = STEPS;
    d.step_n[0] = 1;
    d.transfers = 1;
    if fx {
        d.fx_owner[0] = 1;
        d.fx_pub[0] = u8::from(published);
        d.fx_owner[1] = 1; // later row whose transfer fails
        d.mod_fx[0] = 3;
        d.step = 18;
    } else {
        d.row_owner[0] = 1;
        d.row_pub[0] = u8::from(published);
        d.row_owner[1] = 1;
        d.mod_rows[0] = 3;
        d.step = 15;
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

unsafe fn assert_retained(fx: bool, pending_buf: u32) {
    let d = dsp::ds();
    assert!(dsp::retained());
    assert_eq!(d.pending.rec, 0);
    assert_eq!(d.pending.send_buf, pending_buf);
    assert_eq!(d.mod_len[0], 256);
    assert_eq!(d.mod_len[1], 256);
    assert_eq!(d.step_list[0], STEPS);
    assert_eq!(d.nwait, 1);
    assert_eq!(d.waiting[0].send_buf, 0xe000);
    assert_eq!(d.transfers, 1);
    assert_eq!(d.failures, 1);
    assert_eq!(st().last_result, E_DSP_XFER);
    assert_eq!(boot::info().results[0], E_DSP_XFER);
    assert_eq!(st().modules[0].used, 1);
    assert_eq!(st().modules[0].base, CF);
    assert_eq!(st().slots[0], 0);
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
        d.word,
        d.ticks,
        d.step,
        LINK.tries,
        LINK.polls,
        LINK.events.len(),
    );
    for _ in 0..3 {
        dsp::step();
    }
    assert!(!dsp::fx_word_start(0, 0, 0xabcdef00));
    assert_eq!(dsp::sample_send(0, core::ptr::null(), 4), -2);
    assert_eq!(dsp::reserve(2, &dsp::Job::empty()), E_BUSY);
    dsp::cancel(0);
    dsp::release_range(0);
    dsp::launch(d.waiting[0]);
    assert_eq!(
        (
            d.word,
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
    dsp::step();
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
fn invalidation_refusal_retains_and_stops() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        failure_case(Completion::Refuse);
        assert_eq!(LINK.accepted, 0);
    }
}
#[test]
fn invalidation_error_retains_and_stops() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        failure_case(Completion::ErrorAt(1));
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn invalidation_missing_ack_retains_and_stops() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        failure_case(Completion::Missing);
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn fx_partial_cleanup_does_not_discard_publication() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        seed(Completion::ErrorAt(3), true, true, true);
        dsp::step();
        assert_retained(true, 0);
        assert_eq!(LINK.accepted, 3); // no subsequent cleanup writes after failure
    }
}
#[test]
fn missing_fx_info_retains_published_row() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        seed(Completion::Ack, true, true, true);
        LINK.no_fx_info = true;
        dsp::step();
        assert_retained(true, 0);
        assert_eq!(LINK.accepted, 0);
    }
}
#[test]
fn cf_entered_timeout_retains_image_and_closes_slots_before_fini() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        seed(Completion::Ack, false, false, true);
        st().entered[0] = 1;
        dsp::step();
        // No DSP publication existed, but CF drain failure still retains everything.
        assert!(dsp::retained());
        assert_eq!(dsp::ds().pending.send_buf, IMAGE);
        assert_eq!(dsp::ds().mod_len[0], 256);
        assert_eq!(st().modules[0].used, 1);
        assert_eq!(st().slots[0], 0);
        assert!(LINK.events.is_empty());
        let word = dsp::ds().word;
        dsp::step();
        assert_eq!(dsp::ds().word, word);
        assert_eq!(dsp::ds().failures, 1);
        assert_eq!(dsp::ds().nwait, 1);
    }
}
unsafe fn successful_case(fx: bool, published: bool) {
    seed(Completion::Ack, fx, published, true);
    dsp::step();
    assert!(!dsp::retained());
    assert_eq!(st().modules[0].used, 0);
    assert_eq!(dsp::ds().mod_len[0], 0);
    assert_eq!((dsp::ds().row_owner[0], dsp::ds().fx_owner[0]), (0, 0));
    assert_eq!(dsp::cleanup_test_first_fit(256, 64), Some(0));
    assert!(LINK.events.contains(&Event::Fini));
    assert!(LINK.events.contains(&Event::Registry));
    assert!(LINK.events.contains(&Event::Free(CF)));
    assert!(LINK.events.contains(&Event::Free(STEPS)));
    assert_eq!(LINK.events.contains(&Event::Free(IMAGE)), !published);
    assert_eq!(dsp::ds().phase, dsp::PH_SEND);
    assert_eq!(dsp::ds().pending.rec, 1);
    assert_eq!(dsp::ds().nwait, 0);
    assert_eq!(dsp::ds().transfers, 2);
    assert_eq!(dsp::ds().failures, 1);
}
#[test]
fn acknowledged_voice_cleanup_reclaims_and_advances_queue() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        successful_case(false, true);
        assert_eq!(LINK.accepted, 1);
    }
}
#[test]
fn acknowledged_fx_cleanup_reclaims_after_all_words() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        successful_case(true, true);
        assert_eq!(LINK.accepted, 11);
    }
}
#[test]
fn before_publication_failure_is_reclaimable() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        successful_case(false, false);
        assert_eq!(LINK.accepted, 0);
    }
}
#[test]
fn never_started_cancel_is_reclaimable() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        seed(Completion::Ack, false, false, false);
        dsp::ds().send_buf = IMAGE;
        dsp::cancel(0);
        assert_eq!(dsp::ds().mod_len[0], 0);
        assert!(LINK.events.contains(&Event::Free(IMAGE)));
        assert!(!LINK.events.contains(&Event::Free(CF)));
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
