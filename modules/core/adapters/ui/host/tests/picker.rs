//! The sample picker (sdk/src/sample_picker.c) on the fake <dk/ui.h> and a fake dk_api: the SMP pool list, the browser, the INFO page, keys
//! and texts. The pure functions (sdk/src/picker_logic.c) have their own tests at the end.
use dkui_host::driver::*;
use dkui_host::*;
use std::ffi::{c_char, c_int, CStr, CString};

extern "C" {
    fn pf_reset(with_path: c_int);
    fn pf_init(handle: u32) -> c_int;
    fn pf_open_pool(current: c_int, with_pick: c_int);
    fn pf_fini();
    fn pf_add(path: *const c_char, dir: c_int, size: u32);
    fn pf_finish();
    fn pf_partial();
    fn pf_stale_notify();
    fn pf_fail(rc: c_int);
    fn pf_list_result(rc: c_int);
    fn pf_list_calls() -> c_int;
    fn pf_cancels() -> c_int;
    fn pf_list_dir() -> *const c_char;
    fn pf_client_calls() -> c_int;
    fn pf_track(t: c_int, status: c_int, path: *const c_char, frames: u32, rate: u32, ch: u32);
    fn pf_status(t: c_int, status: c_int);
    fn pf_add_result(rc: c_int);
    fn pf_adds() -> c_int;
    fn pf_clears() -> c_int;
    fn pf_last_index() -> c_int;
    fn pf_picks() -> c_int;
    fn pf_last_pick() -> c_int;
    fn pf_last_path() -> *const c_char;
    fn pf_ticking() -> c_int;

    fn pk_trunc(out: *mut c_char, name: *const c_char, max: c_int);
    fn pk_size_text(out: *mut c_char, bytes: u32);
    fn pk_secs_text(out: *mut c_char, frames: u32, rate: u32);
    fn pk_join(out: *mut c_char, cap: c_int, dir: *const c_char, leaf: *const c_char) -> c_int;
    fn pk_parent(dir: *mut c_char) -> c_int;
    fn pk_err_tail(out: *mut c_char, code: c_int);
    fn pk_err_info(out: *mut c_char, code: c_int);
    fn pk_pool_row(
        out: *mut c_char,
        index: c_int,
        current: c_int,
        status: c_int,
        name: *const c_char,
        frames: u32,
        rate: u32,
        channels: u32,
    );
}

const E_NOT_FOUND: i32 = -1;
const E_READ: i32 = -3;
const E_FORMAT: i32 = -5;
const E_TOO_BIG: i32 = -13;
const E_LOST: i32 = -15;
const E_POOL: i32 = -16;
const E_CHANGED: i32 = -17;
const E_SEL_PATH: i32 = -40;
const E_CONTRACT: i32 = -48;
const QUEUED: i32 = 1;
const LOADING: i32 = 2;
const READY: i32 = 3;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}
fn add(path: &str, dir: bool, size: u32) {
    unsafe { pf_add(c(path).as_ptr(), dir as c_int, size) }
}
fn track(t: i32, status: i32, path: &str, frames: u32, rate: u32, ch: u32) {
    unsafe { pf_track(t, status, c(path).as_ptr(), frames, rate, ch) }
}
fn last_path() -> String {
    unsafe { CStr::from_ptr(pf_last_path()) }
        .to_string_lossy()
        .into_owned()
}
fn list_dir() -> String {
    unsafe { CStr::from_ptr(pf_list_dir()) }
        .to_string_lossy()
        .into_owned()
}
/// A row of a list as render() shows it, without the two-character check column.
fn rows() -> Vec<String> {
    render().iter().map(|r| r[2..].to_string()).collect()
}
/// The row of pool entry `i` in the list (row 0 is + ADD).
fn entry(i: usize) -> String {
    rows()[1 + i].clone()
}

/// A session with the picker started and the pool list open (what a click on SMP does), the SMP value at `current`.
fn start_at(current: i32) -> std::sync::MutexGuard<'static, ()> {
    let g = session();
    unsafe {
        pf_reset(1);
        assert_eq!(pf_init(1), 0);
        pf_open_pool(current, 1);
    }
    assert_eq!(depth(), 1);
    g
}
fn start() -> std::sync::MutexGuard<'static, ()> {
    start_at(0)
}

/// YES on + ADD: the browser opens and lists `entries` (path, is_dir, size) to the end.
fn browse(entries: &[(&str, bool, u32)]) {
    press_yes(0);
    for (p, d, s) in entries {
        add(p, *d, *s);
    }
    unsafe { pf_finish() };
}

#[test]
fn nothing_opens_without_a_handle_a_client_or_lists() {
    let _g = session();
    unsafe {
        pf_reset(1);
        assert_eq!(pf_init(0), -1, "no pool handle");
        pf_open_pool(0, 1);
    }
    assert_eq!(depth(), 0);
    unsafe {
        pf_reset(1);
        assert_eq!(pf_init(1), 0);
        set_cap(CAP_LIST, false);
        pf_open_pool(0, 1);
    }
    assert_eq!(depth(), 0, "no list capability");
}

#[test]
fn a_runtime_without_the_listing_api_cannot_open_the_pool() {
    let _g = session();
    unsafe {
        pf_reset(1);
        // an Api table that ends before files_client (API 1.3)
        extern "C" {
            fn pf_short_api();
        }
        pf_short_api();
        assert_eq!(pf_init(1), -1);
        pf_open_pool(0, 1);
    }
    assert_eq!(depth(), 0);
}

#[test]
fn the_listing_client_is_acquired_once_in_init() {
    let _g = start();
    assert_eq!(unsafe { pf_client_calls() }, 1);
    press_yes(0);
    assert_eq!(unsafe { pf_client_calls() }, 1);
}

#[test]
fn the_pool_list_is_add_then_sixteen_entries_numbered_like_the_knob() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(0, READY, "KICKS/KICK808", 57600, 48000, 2);
        track(1, READY, "SNARE_A", 19200, 48000, 1);
        track(2, LOADING, "LONG/PATH/LOOP", 0, 0, 0);
        track(3, QUEUED, "X", 0, 0, 0);
        track(4, E_POOL, "BIGONE", 0, 0, 0);
        pf_init(1);
        pf_open_pool(1, 1);
    }
    assert_eq!(title(), "SAMPLES");
    let r = rows();
    assert_eq!(r.len(), 17);
    assert_eq!(r[0], "+ ADD");
    assert_eq!(r[1], "00 KICK808 S 1.2s");
    assert_eq!(r[2], "01>SNARE_A M 0.4s", "the SMP value is marked");
    assert_eq!(r[3], "02 LOOP LOADING...");
    assert_eq!(r[4], "03 X QUEUED");
    assert_eq!(r[5], "04 BIGONE !POOL FULL");
    assert_eq!(r[6], "05 -");
    assert_eq!(r[16], "15 -");
    assert!(r.iter().all(|x| x.len() <= 23));
}

#[test]
fn without_sample_pool_path_the_rows_show_no_names() {
    let _g = session();
    unsafe {
        pf_reset(0);
        track(0, READY, "KICKS/KICK808", 48000, 48000, 2);
        pf_init(1);
        pf_open_pool(0, 1);
    }
    assert_eq!(entry(0), "00>S 1.0s");
}

#[test]
fn row_text_per_state_and_error_code() {
    let cases = [
        (E_NOT_FOUND, "!MISSING", "FILE NOT FOUND"),
        (E_READ, "!READ ERROR", "CARD READ FAILED"),
        (E_FORMAT, "!BAD FORMAT", "NEEDS PCM16 48K MONO OR STEREO"),
        (E_TOO_BIG, "!TOO BIG", "DOES NOT FIT THE POOL"),
        (E_LOST, "!LOST", "BACKING LOST, RELOAD FAILED"),
        (E_POOL, "!POOL FULL", "POOL FULL: CLEAR AN ENTRY"),
        (E_CHANGED, "!CHANGED", "FILE CHANGED SINCE SELECTION"),
        (E_SEL_PATH, "!BAD PATH", "PATH TOO LONG OR BAD"),
        (
            E_CONTRACT,
            "NO SAMPLE POOL",
            "NO SAMPLE POOL ON THIS DEVICE",
        ),
        (-99, "!ERR -99", "ERR -99"),
    ];
    for (code, tail, info) in cases {
        let mut b = [0 as c_char; 32];
        unsafe { pk_err_tail(b.as_mut_ptr(), code) };
        assert_eq!(
            unsafe { CStr::from_ptr(b.as_ptr()) }.to_str().unwrap(),
            tail
        );
        unsafe { pk_err_info(b.as_mut_ptr(), code) };
        assert_eq!(
            unsafe { CStr::from_ptr(b.as_ptr()) }.to_str().unwrap(),
            info
        );
        assert!(tail.len() <= 14);
        let mut row = [0 as c_char; 24];
        unsafe { pk_pool_row(row.as_mut_ptr(), 4, 0, code, c("LOOP").as_ptr(), 0, 0, 0) };
        let s = unsafe { CStr::from_ptr(row.as_ptr()) }.to_str().unwrap();
        assert!(s.ends_with(tail) && s.len() <= 23, "{s}");
    }
}

#[test]
fn a_long_name_gives_way_to_the_error_text_and_holes_show_a_dash() {
    let mut row = [0 as c_char; 24];
    unsafe {
        pk_pool_row(
            row.as_mut_ptr(),
            0,
            0,
            E_POOL,
            c("KICK_LONG_NAME_FINAL").as_ptr(),
            100,
            48000,
            2,
        )
    };
    let s = unsafe { CStr::from_ptr(row.as_ptr()) }.to_str().unwrap();
    assert_eq!(s, "00 KICK_~NAL !POOL FULL");
    unsafe { pk_pool_row(row.as_mut_ptr(), 9, 0, E_CONTRACT, c("").as_ptr(), 0, 0, 0) };
    let s = unsafe { CStr::from_ptr(row.as_ptr()) }.to_str().unwrap();
    assert_eq!(s, "09 NO SAMPLE POOL");
    unsafe { pk_pool_row(row.as_mut_ptr(), 15, 1, 0, c("").as_ptr(), 0, 0, 0) };
    let s = unsafe { CStr::from_ptr(row.as_ptr()) }.to_str().unwrap();
    assert_eq!(s, "15>-");
}

#[test]
fn browser_lists_folders_first_sorted_without_case() {
    let _g = start();
    press_yes(0);
    assert_eq!(rows(), vec!["..  /", "READING..."]);
    assert_eq!(list_dir(), "");
    add("zeta", false, 2048);
    add("Bass", true, 0);
    add("apple", false, 100 * 1024);
    add("alpha", true, 0);
    add("Beta", false, 1500 * 1024);
    unsafe { pf_finish() };
    assert_eq!(
        rows(),
        vec![
            "..  /",
            "> alpha",
            "> Bass",
            "  apple 100K",
            "  Beta 1.5M",
            "  zeta 2K"
        ]
    );
}

#[test]
fn rows_stay_hidden_until_the_listing_ends() {
    let _g = start();
    press_yes(0);
    add("a", false, 1);
    unsafe { pf_partial() };
    assert_eq!(rows(), vec!["..  /", "READING..."]);
    unsafe { pf_finish() };
    assert_eq!(rows().len(), 2);
    assert_eq!(rows()[1], "  a 1K");
}

#[test]
fn a_long_name_keeps_its_start_and_end() {
    let _g = start();
    browse(&[
        ("KICK_LONG_NAME_THAT_ENDS_FINAL_MIX", false, 90 * 1024),
        ("FOLDER_WITH_A_VERY_LONG_NAME_HERE", true, 0),
    ]);
    let r = rows();
    assert_eq!(r[1], "> FOLDER_WITH~NAME_HERE");
    assert_eq!(r[2], "  KICK_LONG~NAL_MIX 90K");
    assert!(r.iter().all(|x| x.len() <= 23));
}

#[test]
fn the_cap_keeps_the_first_128_in_sort_order_and_counts_the_rest() {
    let _g = start();
    press_yes(0);
    // 200 files arriving in reverse order; a folder arrives last and still sorts first
    for i in (0..200).rev() {
        add(&format!("F{i:03}"), false, 1024);
    }
    add("DIR", true, 0);
    // the fake provider holds 400 rows
    unsafe { pf_finish() };
    let r = rows();
    assert_eq!(r.len(), 1 + 128 + 1);
    assert_eq!(r[1], "> DIR");
    assert_eq!(r[2], "  F000 1K");
    assert_eq!(r[128], "  F126 1K");
    assert_eq!(r[129], "+73 MORE");
}

#[test]
fn a_name_over_47_bytes_is_not_shown_and_counts_as_hidden() {
    let _g = start();
    browse(&[(&"N".repeat(48), false, 1024), ("ok", false, 1024)]);
    assert_eq!(rows(), vec!["..  /", "  ok 1K", "+1 MORE"]);
}

#[test]
fn an_empty_folder_and_a_failed_listing() {
    let _g = start();
    browse(&[]);
    assert_eq!(rows(), vec!["..  /", "NO SAMPLES HERE"]);
    press_no();
    assert_eq!(depth(), 1);
    press_yes(0);
    add("a", false, 1);
    unsafe { pf_fail(-3) };
    assert_eq!(rows(), vec!["..  /", "LIST ERROR -3"]);
    press_no();
    unsafe { pf_list_result(-13) };
    press_yes(0);
    assert_eq!(rows(), vec!["..  /", "LIST ERROR -13"]);
}

#[test]
fn folders_open_and_no_goes_up_then_out_at_the_root() {
    let _g = start();
    browse(&[("DRUMS", true, 0)]);
    let calls = unsafe { pf_list_calls() };
    press_yes(1);
    assert_eq!(list_dir(), "DRUMS");
    assert_eq!(unsafe { pf_list_calls() }, calls + 1);
    assert_eq!(rows(), vec!["..  /DRUMS", "READING..."]);
    add("KICKS", true, 0);
    unsafe { pf_finish() };
    press_yes(1);
    assert_eq!(list_dir(), "DRUMS/KICKS");
    add("K1", false, 2048);
    unsafe { pf_finish() };
    assert_eq!(rows(), vec!["..  /DRUMS/KICKS", "  K1 2K"]);
    // NO goes up one folder at a time and stays in the browser
    press_no();
    assert_eq!((list_dir().as_str(), depth()), ("DRUMS", 2));
    // the ".." row does the same
    press_yes(0);
    assert_eq!((list_dir().as_str(), depth()), ("", 2));
    // at the root NO and ".." leave the browser for the pool list
    press_no();
    assert_eq!(depth(), 1);
    assert_eq!(title(), "SAMPLES");
    press_yes(0);
    press_yes(0);
    assert_eq!(depth(), 1);
}

#[test]
fn a_new_folder_cancels_the_listing_that_still_runs() {
    let _g = start();
    press_yes(0);
    let c0 = unsafe { pf_cancels() };
    press_no(); // leaves while reading
    assert_eq!(unsafe { pf_cancels() }, c0 + 1);
    press_yes(0);
    add("D", true, 0);
    unsafe { pf_finish() };
    press_yes(1);
    assert_eq!(
        unsafe { pf_cancels() },
        c0 + 1,
        "a finished listing is not cancelled"
    );
    // a notification of the old ticket changes nothing
    unsafe { pf_stale_notify() };
    assert_eq!(rows()[1], "READING...");
}

#[test]
fn yes_on_a_file_adds_the_full_path_picks_its_index_and_shows_the_list() {
    let _g = start_at(5);
    browse(&[("DRUMS", true, 0)]);
    press_yes(1);
    add("KICK_01", false, 90 * 1024);
    unsafe { pf_finish() };
    press_yes(1);
    assert_eq!(unsafe { pf_adds() }, 1);
    assert_eq!(last_path(), "DRUMS/KICK_01");
    assert_eq!(unsafe { pf_last_index() }, 0, "the first free entry");
    assert_eq!(
        (unsafe { pf_picks() }, unsafe { pf_last_pick() }),
        (1, 0),
        "the index goes to the machine"
    );
    assert_eq!(depth(), 1);
    assert_eq!(entry(0), "00>KICK_01 QUEUED", "the new entry is the current one");
    // no row keeps the check mark that core/ui sets on YES
    assert!(render().iter().all(|r| r.starts_with("  ")));
}

#[test]
fn a_second_file_takes_the_next_index_and_the_same_file_keeps_its_own() {
    let _g = start();
    browse(&[("A", false, 1024), ("B", false, 1024)]);
    press_yes(1);
    assert_eq!(unsafe { pf_last_pick() }, 0);
    browse(&[("A", false, 1024), ("B", false, 1024)]);
    press_yes(2);
    assert_eq!(unsafe { pf_last_pick() }, 1);
    assert_eq!((entry(0).as_str(), entry(1).as_str()), ("00 A QUEUED", "01>B QUEUED"));
    browse(&[("A", false, 1024), ("B", false, 1024)]);
    press_yes(1);
    assert_eq!(unsafe { pf_last_pick() }, 0, "A is already entry 0");
    assert_eq!(unsafe { pf_picks() }, 3);
    assert_eq!(entry(2), "02 -");
}

#[test]
fn browsing_alone_adds_nothing_and_picks_nothing() {
    let _g = start();
    browse(&[("A", false, 1024)]);
    press_no();
    assert_eq!(unsafe { pf_adds() }, 0);
    assert_eq!(unsafe { pf_clears() }, 0);
    assert_eq!(unsafe { pf_picks() }, 0);
}

#[test]
fn a_folder_has_no_side_panel_and_a_file_has_add_and_info() {
    let _g = start();
    browse(&[("D", true, 0), ("S1", false, 4096)]);
    let before = depth();
    press_right(1); // the folder
    assert_eq!(depth(), before);
    press_right(2);
    assert_eq!(render(), vec!["SELECT", "INFO"]);
    choose_panel_row(0);
    assert_eq!((unsafe { pf_adds() }, last_path().as_str()), (1, "S1"));
    assert_eq!(depth(), 1);

    browse(&[("D", true, 0), ("S1", false, 4096)]);
    press_right(2);
    choose_panel_row(1);
    assert_eq!(title(), "INFO");
    assert_eq!(render(), vec!["S1", "S1", "SIZE 4096 BYTES"]);
}

#[test]
fn an_add_at_the_path_limit_is_refused_by_the_picker_without_a_call() {
    let _g = start();
    // five folders of 47 bytes make a 239-byte path; a 20-byte file over 255 is too long
    let dir = "d".repeat(47);
    press_yes(0);
    for _ in 0..5 {
        add(&dir, true, 0);
        unsafe { pf_finish() };
        press_yes(1);
    }
    assert_eq!(list_dir().len(), 5 * 47 + 4);
    add(&"f".repeat(20), false, 1024);
    unsafe { pf_finish() };
    press_yes(1);
    assert_eq!(unsafe { pf_adds() }, 0);
    assert_eq!(unsafe { pf_picks() }, 0);
    assert_eq!(depth(), 1);
    assert_eq!(rows()[0], "+ ADD  !BAD PATH");
    // a file that just fits is passed on, and the refusal goes away
    press_yes(0);
    add(&"g".repeat(15), false, 1024);
    unsafe { pf_finish() };
    press_yes(1);
    assert_eq!(unsafe { pf_adds() }, 1);
    assert_eq!(last_path().len(), 255);
    assert_eq!(rows()[0], "+ ADD");
}

#[test]
fn a_refusal_shows_on_the_add_row_and_picks_nothing() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(0, READY, "OLD", 48000, 48000, 2);
        pf_init(1);
        pf_open_pool(0, 1);
        pf_add_result(E_POOL);
    }
    browse(&[("NEW", false, 1024)]);
    press_yes(1);
    assert_eq!(unsafe { pf_adds() }, 1);
    assert_eq!(depth(), 1, "no dialog, the browser closed");
    assert!(dialog().is_none());
    assert_eq!(rows()[0], "+ ADD  !POOL FULL");
    assert_eq!(entry(0), "00>OLD S 1.0s", "the old entry is unchanged");
    assert_eq!(unsafe { pf_picks() }, 0);
    // the next add that works clears the refusal
    unsafe { pf_add_result(0) };
    browse(&[("NEW", false, 1024)]);
    press_yes(1);
    assert_eq!(rows()[0], "+ ADD");
    assert_eq!(unsafe { pf_last_pick() }, 1);
}

#[test]
fn a_device_without_a_pool_says_so_on_the_add_row() {
    let _g = start();
    unsafe { pf_add_result(E_CONTRACT) };
    browse(&[("A", false, 1024)]);
    press_yes(1);
    assert_eq!(rows()[0], "+ ADD  NO SAMPLE POOL");
}

#[test]
fn yes_on_an_entry_picks_it_and_closes_the_list_but_not_on_a_hole() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(3, READY, "A", 100, 48000, 1);
        pf_init(1);
        pf_open_pool(0, 1);
    }
    press_yes(1 + 7); // a hole
    assert_eq!((unsafe { pf_picks() }, depth()), (0, 1));
    press_yes(1 + 3);
    assert_eq!((unsafe { pf_picks() }, unsafe { pf_last_pick() }), (1, 3));
    assert_eq!(depth(), 0, "back to the machine page");
}

#[test]
fn no_in_the_list_returns_to_the_page_without_a_pick() {
    let _g = start();
    press_no();
    assert_eq!((depth(), unsafe { pf_picks() }), (0, 0));
}

#[test]
fn a_click_without_a_pick_callback_still_lists_and_adds() {
    let _g = session();
    unsafe {
        pf_reset(1);
        pf_init(1);
        pf_open_pool(-1, 0);
    }
    browse(&[("A", false, 1024)]);
    press_yes(1);
    assert_eq!(unsafe { pf_adds() }, 1);
    assert_eq!(unsafe { pf_picks() }, 0);
    assert_eq!(entry(0), "00>A QUEUED");
}

#[test]
fn yes_on_an_entry_without_a_pick_callback_only_closes_the_list() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(3, READY, "A", 100, 48000, 1);
        pf_init(1);
        pf_open_pool(-1, 0);
    }
    press_yes(1 + 3);
    assert_eq!((unsafe { pf_picks() }, depth()), (0, 0));
}

#[test]
fn the_timer_runs_while_a_load_runs_and_stops_when_it_ends() {
    let _g = start();
    assert!(unsafe { pf_ticking() } == 0 && tick_periods().is_empty());
    browse(&[("A", false, 1024)]);
    press_yes(1);
    assert_eq!(tick_periods(), vec![250]);
    assert_eq!(entry(0), "00>A QUEUED");
    // the runtime moves on without a key press; the tick shows it
    unsafe { pf_status(0, LOADING) };
    advance_ms(249);
    assert_eq!(entry(0), "00>A QUEUED");
    advance_ms(1);
    assert_eq!(entry(0), "00>A LOADING...");
    unsafe { pf_track(0, READY, c("A").as_ptr(), 24000, 48000, 1) };
    advance_ms(250);
    assert_eq!(entry(0), "00>A M 0.5s");
    assert!(
        tick_periods().is_empty(),
        "the timer stops with the last load"
    );
    // one timer for several loads
    unsafe {
        pf_status(1, LOADING);
        pf_status(2, LOADING);
    }
    press_no(); // out of the pool list
    unsafe { pf_open_pool(0, 1) };
    assert_eq!(tick_periods(), vec![250]);
    unsafe { pf_status(1, READY) };
    advance_ms(250);
    assert_eq!(tick_periods(), vec![250]);
    unsafe { pf_status(2, READY) };
    advance_ms(250);
    assert!(tick_periods().is_empty());
}

#[test]
fn leaving_the_pool_list_stops_the_timer() {
    let _g = start();
    unsafe { pf_status(0, LOADING) };
    browse(&[]);
    press_no();
    press_no(); // browser, then the pool list
    assert_eq!(depth(), 0);
    advance_ms(250);
    assert!(tick_periods().is_empty());
    unsafe { pf_fini() };
}

#[test]
fn without_a_timer_capability_the_rows_still_show_the_state_on_the_next_key() {
    let _g = session();
    set_cap(CAP_TICK, false);
    unsafe {
        pf_reset(1);
        track(0, LOADING, "A", 0, 0, 0);
        pf_init(1);
        pf_open_pool(0, 1);
    }
    assert_eq!(entry(0), "00>A LOADING...");
    assert!(tick_periods().is_empty());
}

#[test]
fn clear_removes_the_entry_with_one_press_and_leaves_a_hole() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(2, READY, "A", 100, 48000, 1);
        track(3, READY, "B", 100, 48000, 1);
        pf_init(1);
        pf_open_pool(0, 1);
    }
    press_right(1 + 2);
    assert_eq!(render(), vec!["CLEAR", "INFO"]);
    choose_panel_row(0);
    assert_eq!(unsafe { pf_clears() }, 1);
    assert_eq!(entry(2), "02 -");
    assert_eq!(entry(3), "03 B M 0.0s", "the next entry keeps its index");
    // + ADD has no panel
    let d = depth();
    press_right(0);
    assert_eq!(depth(), d);
}

#[test]
fn entry_info_page_shows_index_name_path_format_and_state() {
    let _g = session();
    unsafe {
        pf_reset(1);
        track(2, READY, "DRUMS/KICKS/KICK808", 57600, 48000, 2);
        track(3, E_FORMAT, "X/BAD", 0, 0, 0);
        track(4, LOADING, "L", 0, 0, 0);
        pf_init(1);
        pf_open_pool(0, 1);
    }
    press_right(1 + 2);
    choose_panel_row(1);
    assert_eq!(title(), "INFO");
    assert_eq!(
        render(),
        vec![
            "SAMPLE 2",
            "KICK808",
            "DRUMS/KICKS/",
            "KICK808",
            "STEREO 48000 HZ",
            "57600 FRAMES",
            "LENGTH 1.2s",
            "STATE READY"
        ]
    );
    press_no();
    press_right(1 + 3);
    choose_panel_row(1);
    assert_eq!(
        render(),
        vec![
            "SAMPLE 3",
            "BAD",
            "X/BAD",
            "STATE",
            "NEEDS PCM16 48K",
            "MONO OR STEREO"
        ]
    );
    press_no();
    press_right(1 + 4);
    choose_panel_row(1);
    assert_eq!(render().last().unwrap(), "STATE LOADING");
    press_no();
    press_right(1 + 9);
    choose_panel_row(1);
    assert_eq!(render(), vec!["SAMPLE 9", "EMPTY"]);
}

// ---- the pure functions ----

fn text(f: impl FnOnce(*mut c_char)) -> String {
    let mut b = [0 as c_char; 64];
    f(b.as_mut_ptr());
    unsafe { CStr::from_ptr(b.as_ptr()) }
        .to_str()
        .unwrap()
        .to_string()
}

#[test]
fn truncation_keeps_start_and_end() {
    let t = |n: &str, max| text(|o| unsafe { pk_trunc(o, c(n).as_ptr(), max) });
    assert_eq!(t("KICK", 10), "KICK");
    assert_eq!(t("ABCDEFGHIJ", 10), "ABCDEFGHIJ");
    assert_eq!(t("ABCDEFGHIJK", 10), "ABCDE~HIJK");
    assert_eq!(t("ABCDEFGHIJK", 10).len(), 10);
    assert_eq!(t("ABCDEFGHIJK", 2), "AB");
    assert_eq!(t("ABCDEFGHIJK", 0), "");
}

#[test]
fn size_and_length_text() {
    let s = |b| text(|o| unsafe { pk_size_text(o, b) });
    assert_eq!(
        (
            s(0).as_str(),
            s(1).as_str(),
            s(1024).as_str(),
            s(1025).as_str()
        ),
        ("0K", "1K", "1K", "2K")
    );
    assert_eq!(s(999 * 1024), "999K");
    assert_eq!(s(1024 * 1024), "1.0M");
    assert_eq!(s(5 * 1024 * 1024 / 2), "2.5M");
    assert_eq!(s(u32::MAX), "4096.0M");
    let l = |f, r| text(|o| unsafe { pk_secs_text(o, f, r) });
    assert_eq!(l(57600, 48000), "1.2s");
    assert_eq!(l(0, 48000), "0.0s");
    assert_eq!(l(48000 * 99, 48000), "99.0s");
    assert_eq!(l(48000 * 123, 48000), "123s");
    assert_eq!(l(5, 0), "0.0s");
}

#[test]
fn path_join_and_parent() {
    let j = |d: &str, l: &str| {
        let mut b = [0 as c_char; 300];
        let n = unsafe { pk_join(b.as_mut_ptr(), 300, c(d).as_ptr(), c(l).as_ptr()) };
        (
            n,
            unsafe { CStr::from_ptr(b.as_ptr()) }
                .to_str()
                .unwrap()
                .to_string(),
        )
    };
    assert_eq!(j("", "A"), (1, "A".into()));
    assert_eq!(j("A/B", "C"), (5, "A/B/C".into()));
    // 255 bytes fit, 256 do not
    let d = "d".repeat(200);
    assert_eq!(j(&d, &"f".repeat(54)).0, 255);
    assert_eq!(j(&d, &"f".repeat(55)).0, -1);
    assert_eq!(j("A", "").0, -1);
    let mut b = [0 as c_char; 8];
    assert_eq!(
        unsafe { pk_join(b.as_mut_ptr(), 4, c("AB").as_ptr(), c("C").as_ptr()) },
        -1,
        "cap"
    );

    let p = |s: &str| {
        let mut b = [0 as c_char; 64];
        for (i, ch) in s.bytes().enumerate() {
            b[i] = ch as c_char;
        }
        let r = unsafe { pk_parent(b.as_mut_ptr()) };
        (
            r,
            unsafe { CStr::from_ptr(b.as_ptr()) }
                .to_str()
                .unwrap()
                .to_string(),
        )
    };
    assert_eq!(p("A/B/C"), (1, "A/B".into()));
    assert_eq!(p("A"), (1, "".into()));
    assert_eq!(p(""), (0, "".into()));
}
