use dkui_host::driver::*;
use dkui_host::*;
use std::ffi::c_int;

extern "C" {
    fn tk_reset();
    fn tk_set(id: c_int, ms: u32, now: u32) -> c_int;
    fn tk_set_other(ms: u32, now: u32) -> c_int;
    fn tk_set_null() -> c_int;
    fn tk_run(now: u32, per_s: u32) -> u32;
    fn tk_calls(id: c_int) -> c_int;
    fn tk_stop_after(id: c_int, n: u32);
    fn tk_active() -> u32;
    fn tk_module_open() -> c_int;
    fn tk_module_finish();
    fn tk_module_polls() -> c_int;
    fn tk_module_fills() -> c_int;
    fn tk_module_stop() -> c_int;
    fn tk_module_restart(ms: u32) -> c_int;
    fn tk_module_has_cap() -> c_int;
}

const BLOCKS: u32 = 1500;
const PASSES: u32 = 120;

/// Runs the table once per clock unit from `from` to `to` (both included), like the UI loop does.
fn run_units(from: u32, to: u32, per_s: u32) {
    for now in from..=to {
        unsafe { tk_run(now, per_s) };
    }
}

#[test]
fn a_timer_fires_once_per_period_on_the_block_clock() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, 100, 5000), 0);
        // 100 ms are 150 blocks
        run_units(5001, 5149, BLOCKS);
        assert_eq!(tk_calls(0), 0, "one period has not passed");
        run_units(5150, 5150, BLOCKS);
        assert_eq!(tk_calls(0), 1);
        run_units(5151, 5299, BLOCKS);
        assert_eq!(tk_calls(0), 1);
        run_units(5300, 5300, BLOCKS);
        assert_eq!(tk_calls(0), 2);
        // a late pass fires once, not once per missed period
        tk_run(9000, BLOCKS);
        assert_eq!(tk_calls(0), 3);
    }
}

#[test]
fn without_a_block_clock_the_passes_time_the_period() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, 500, 0), 0);
        // 500 ms are 60 passes
        run_units(1, 59, PASSES);
        assert_eq!(tk_calls(0), 0);
        run_units(60, 60, PASSES);
        assert_eq!(tk_calls(0), 1);
        // a period under one unit still fires every unit
        tk_reset();
        assert_eq!(tk_set(0, 1, 0), 0);
        run_units(1, 3, PASSES);
        assert_eq!(tk_calls(0), 3);
    }
}

#[test]
fn the_clock_may_wrap() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, 100, u32::MAX - 10), 0);
        tk_run(u32::MAX - 1, BLOCKS);
        assert_eq!(tk_calls(0), 0);
        tk_run(139, BLOCKS); // 150 blocks after the start
        assert_eq!(tk_calls(0), 1);
    }
}

#[test]
fn setting_again_changes_the_period_and_restarts_it_stopping_removes() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, 100, 0), 0);
        assert_eq!(tk_set(0, 200, 100), 0, "same function and context: a change, not a second timer");
        assert_eq!(tk_active(), 1);
        tk_run(250, BLOCKS);
        assert_eq!(tk_calls(0), 0, "the restart counts from 100: 200 ms are 300 blocks");
        tk_run(400, BLOCKS);
        assert_eq!(tk_calls(0), 1);
        assert_eq!(tk_set(0, 0, 400), 0);
        assert_eq!(tk_active(), 0);
        assert_eq!(tk_set(0, 0, 400), 0, "stopping what does not run is not an error");
        tk_run(100_000, BLOCKS);
        assert_eq!(tk_calls(0), 1);
    }
}

#[test]
fn the_same_function_with_another_context_is_another_timer() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, 10, 0), 0);
        assert_eq!(tk_set(1, 10, 0), 0);
        assert_eq!(tk_active(), 2);
        tk_run(15, BLOCKS);
        assert_eq!((tk_calls(0), tk_calls(1)), (1, 1));
        assert_eq!(tk_set(0, 0, 15), 0);
        tk_run(30, BLOCKS);
        assert_eq!((tk_calls(0), tk_calls(1)), (1, 2));
    }
}

#[test]
fn the_table_has_room_for_four_and_refuses_the_fifth_and_a_null_function() {
    let _g = session();
    unsafe {
        tk_reset();
        for id in 0..4 {
            assert_eq!(tk_set(id, 10, 0), 0);
        }
        assert_eq!(tk_set_other(10, 0), -1, "full");
        assert_eq!(tk_set(2, 20, 0), 0, "a change needs no room");
        assert_eq!(tk_set(3, 0, 0), 0);
        assert_eq!(tk_set_other(10, 0), 0, "a stopped timer gave its room back");
        assert_eq!(tk_set_null(), -1);
        assert_eq!(tk_active(), 4);
    }
}

#[test]
fn a_callback_may_stop_its_own_timer_and_run_reports_what_is_left() {
    let _g = session();
    unsafe {
        tk_reset();
        tk_stop_after(0, 2);
        assert_eq!(tk_set(0, 10, 0), 0);
        assert_eq!(tk_set(1, 10, 0), 0);
        assert_eq!(tk_run(15, BLOCKS), 2);
        assert_eq!(tk_run(30, BLOCKS), 1, "timer 0 stopped itself in its second call");
        assert_eq!(tk_calls(0), 2);
        assert_eq!(tk_run(45, BLOCKS), 1);
        assert_eq!(tk_calls(0), 2);
        assert_eq!(tk_calls(1), 3);
    }
}

#[test]
fn a_period_over_the_limit_is_cut() {
    let _g = session();
    unsafe {
        tk_reset();
        assert_eq!(tk_set(0, u32::MAX, 0), 0);
        // 1,000,000 ms are 1,500,000 blocks
        tk_run(1_499_999, BLOCKS);
        assert_eq!(tk_calls(0), 0);
        tk_run(1_500_000, BLOCKS);
        assert_eq!(tk_calls(0), 1);
    }
}

#[test]
fn a_module_screen_redraws_while_a_load_runs_and_stops_its_timer() {
    let _g = session();
    unsafe {
        assert_eq!(tk_module_has_cap(), 1);
        assert_eq!(tk_module_open(), 0);
        assert_eq!(tick_periods(), [100]);
        let fills = tk_module_fills();
        assert_eq!(render(), ["LOADING..."]);
        advance_ms(99);
        assert_eq!(tk_module_polls(), 0);
        advance_ms(1);
        assert_eq!(tk_module_polls(), 1);
        assert_eq!(tk_module_fills(), fills + 1, "the poll refreshed the screen");
        advance_ms(300);
        assert_eq!(tk_module_polls(), 4);
        tk_module_finish();
        advance_ms(100);
        assert_eq!(render(), ["READY"]);
        assert!(tick_periods().is_empty(), "the callback stopped its timer");
        advance_ms(1000);
        assert_eq!(tk_module_polls(), 5);
    }
}

#[test]
fn a_module_can_change_the_period_and_stop_the_timer_from_outside() {
    let _g = session();
    unsafe {
        assert_eq!(tk_module_open(), 0);
        assert_eq!(tk_module_restart(250), 0);
        assert_eq!(tick_periods(), [250]);
        advance_ms(249);
        assert_eq!(tk_module_polls(), 0);
        advance_ms(1);
        assert_eq!(tk_module_polls(), 1);
        assert_eq!(tk_module_stop(), 0);
        assert_eq!(tk_module_stop(), 0);
        advance_ms(1000);
        assert_eq!(tk_module_polls(), 1);
    }
}

#[test]
fn without_the_capability_the_call_does_nothing_and_says_so() {
    let _g = session();
    set_cap(CAP_TICK, false);
    unsafe {
        assert_eq!(tk_module_has_cap(), 0);
        assert_eq!(tk_module_open(), -1);
        assert!(tick_periods().is_empty());
        advance_ms(500);
        assert_eq!(tk_module_polls(), 0);
    }
}
