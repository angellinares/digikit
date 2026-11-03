//! Limits at their edges: exactly full is accepted, one more is refused with the right code.
use super::*;
use crate::sel;
use crate::{E_DSP_ROWS, E_LOADED, E_PAGE_FULL, E_PARAM_FULL, E_SEL_COUNT, E_TABLE_FULL, E_TOO_BIG, E_TYPE_RANGE, OK, SEL_MAX};

// ---- DSP placement ----

#[test]
fn first_fit_on_an_empty_area_starts_at_the_aligned_base() {
    assert_eq!(first_fit(0x1000, 64, &[], &[], 16, 4), Some(0));
    assert_eq!(first_fit(0x1002, 64, &[], &[], 16, 4), Some(2)); // the byte address 0x1004 is the first aligned one
    assert_eq!(first_fit(0x1000, 64, &[0, 0], &[0, 0], 16, 4), Some(0)); // unused records (length 0) are ignored
}

#[test]
fn first_fit_takes_an_area_that_is_exactly_the_size() {
    assert_eq!(first_fit(0, 64, &[], &[], 64, 1), Some(0));
    assert_eq!(first_fit(0, 64, &[], &[], 65, 1), None);
    assert_eq!(first_fit(0, 0, &[], &[], 0, 1), Some(0));
}

#[test]
fn first_fit_goes_past_used_ranges_and_into_a_gap_when_it_fits() {
    // used: [0,16) and [32,48); the gap [16,32) takes 16 bytes but not 17
    assert_eq!(first_fit(0, 64, &[0, 32], &[16, 16], 16, 1), Some(16));
    assert_eq!(first_fit(0, 64, &[0, 32], &[16, 16], 17, 1), None);
    assert_eq!(first_fit(0, 64, &[0], &[16], 48, 1), Some(16)); // exactly the rest
    assert_eq!(first_fit(0, 64, &[0], &[16], 49, 1), None);
}

#[test]
fn first_fit_keeps_the_alignment_after_it_moves_past_a_range() {
    // the range ends at 18; the next 8-aligned offset is 24
    assert_eq!(first_fit(0, 64, &[0], &[18], 8, 8), Some(24));
    assert_eq!(first_fit(0, 64, &[0], &[18], 40, 8), Some(24));
    assert_eq!(first_fit(0, 64, &[0], &[18], 41, 8), None);
    assert_eq!(first_fit(0, 64, &[0], &[18], 32, 8), Some(24));
}

#[test]
fn first_fit_uses_the_gap_left_by_an_unloaded_module() {
    // records listed out of order: the later offset comes first
    assert_eq!(first_fit(0, 96, &[48, 0], &[16, 16], 32, 1), Some(16));
}

// ---- row counting ----

#[test]
fn free_rows_counts_unowned_rows_below_the_reserved_limit() {
    assert_eq!(free_rows(&[0; 8], 8), 8);
    assert_eq!(free_rows(&[0; 8], 5), 5);
    assert_eq!(free_rows(&[0; 8], 0), 0);
    assert_eq!(free_rows(&[1, 0, 2, 0, 0, 0, 0, 0], 8), 6);
    assert_eq!(free_rows(&[1, 0, 2, 0, 0, 0, 0, 0], 3), 1); // only row 1 is free below 3
    assert_eq!(free_rows(&[0; 4], 100), 4); // a limit past the table does not add rows
    assert_eq!(free_rows(&[], 8), 0);
}

#[test]
fn used_rows_counts_owned_rows() {
    assert_eq!(used_rows(&[]), 0);
    assert_eq!(used_rows(&[0, 0]), 0);
    assert_eq!(used_rows(&[3, 0, 1, 1]), 3);
}

#[test]
fn dsp_rows_accepts_exactly_the_free_count_and_refuses_one_more() {
    assert_eq!(dsp_rows(0, 0), OK);
    assert_eq!(dsp_rows(3, 3), OK);
    assert_eq!(dsp_rows(4, 3), E_DSP_ROWS);
    assert_eq!(dsp_rows(1, 0), E_DSP_ROWS);
}

#[test]
fn a_full_voice_table_refuses_the_next_module() {
    let mut owner = [0u8; 8];
    for k in 0..8 {
        assert_eq!(dsp_rows(1, free_rows(&owner, 8)), OK);
        owner[k] = 1;
    }
    assert_eq!(dsp_rows(1, free_rows(&owner, 8)), E_DSP_ROWS);
}

// ---- registry ----

#[test]
fn registry_rows_accept_exactly_full_and_refuse_one_more_with_the_right_code() {
    assert_eq!(registry_rows(0, 0, 0, 0), OK);
    assert_eq!(registry_rows(4, 4, 9, 9), OK);
    assert_eq!(registry_rows(5, 4, 9, 9), E_PAGE_FULL);
    assert_eq!(registry_rows(4, 4, 10, 9), E_PARAM_FULL);
    assert_eq!(registry_rows(1, 0, 0, 0), E_PAGE_FULL);
    assert_eq!(registry_rows(0, 0, 1, 0), E_PARAM_FULL);
}

#[test]
fn registry_rows_report_the_page_limit_before_the_parameter_limit() {
    assert_eq!(registry_rows(5, 4, 10, 9), E_PAGE_FULL);
}

#[test]
fn machine_type_must_lie_in_the_run_time_range_and_clone_a_stock_machine() {
    // run-time types 16..24, stock machines below 60
    assert_eq!(machine_type(16, 16, 8, 59, 60), OK);
    assert_eq!(machine_type(23, 16, 8, 0, 60), OK);
    assert_eq!(machine_type(15, 16, 8, 0, 60), E_TYPE_RANGE);
    assert_eq!(machine_type(24, 16, 8, 0, 60), E_TYPE_RANGE);
    assert_eq!(machine_type(0, 16, 8, 0, 60), E_TYPE_RANGE); // no number was assigned
    assert_eq!(machine_type(16, 16, 8, 60, 60), E_TYPE_RANGE);
}

// ---- selection ----

#[test]
fn a_selection_may_name_up_to_sel_max_paths() {
    assert_eq!(sel::check_count(0), OK);
    assert_eq!(sel::check_count(SEL_MAX as u32), OK);
    assert_eq!(sel::check_count(SEL_MAX as u32 + 1), E_SEL_COUNT);
    assert_eq!(sel::check_count(u32::MAX), E_SEL_COUNT);
}

#[test]
fn select_one_checks_the_count_and_the_copy_index() {
    assert_eq!(sel::check_one(SEL_MAX as u32, 0), OK);
    assert_eq!(sel::check_one(SEL_MAX as u32, 1), OK);
    assert_eq!(sel::check_one(SEL_MAX as u32 + 1, 0), E_SEL_COUNT);
    assert_eq!(sel::check_one(1, 2), E_SEL_COUNT);
}

#[test]
fn the_mailbox_list_takes_sel_max_paths_and_refuses_the_next() {
    assert_eq!(sel::check_slot(0), OK);
    assert_eq!(sel::check_slot(SEL_MAX - 1), OK);
    assert_eq!(sel::check_slot(SEL_MAX), E_SEL_COUNT);
}

// ---- stage and module table ----

#[test]
fn a_file_must_fit_the_staging_buffer_and_not_be_empty() {
    assert_eq!(stage_fit(1, 1000), OK);
    assert_eq!(stage_fit(1000, 1000), OK);
    assert_eq!(stage_fit(1001, 1000), E_TOO_BIG);
    assert_eq!(stage_fit(0, 1000), E_TOO_BIG);
}

#[test]
fn a_read_must_fit_the_callers_buffer() {
    assert_eq!(read_fit(0, 0), OK);
    assert_eq!(read_fit(64, 64), OK);
    assert_eq!(read_fit(65, 64), E_TOO_BIG);
}

const NAMES: [&[u8]; 4] = [b"a", b"b", b"c", b"d"];

#[test]
fn pick_record_takes_the_first_free_record() {
    let used = [true, false, true, false];
    assert_eq!(pick_record(4, |i| used[i], |i| NAMES[i] == b"x"), Ok(1));
}

#[test]
fn pick_record_refuses_a_name_that_is_loaded_even_after_a_free_record() {
    let used = [false, true, false, false];
    assert_eq!(pick_record(4, |i| used[i], |i| NAMES[i] == b"b"), Err(E_LOADED));
}

#[test]
fn pick_record_takes_the_last_record_and_refuses_when_the_table_is_full() {
    let mut used = [true, true, true, false];
    assert_eq!(pick_record(4, |i| used[i], |_| false), Ok(3));
    used[3] = true;
    assert_eq!(pick_record(4, |i| used[i], |_| false), Err(E_TABLE_FULL));
    assert_eq!(pick_record(0, |_| false, |_| false), Err(E_TABLE_FULL));
}

#[test]
fn a_loaded_name_is_reported_before_a_full_table() {
    assert_eq!(pick_record(4, |_| true, |i| NAMES[i] == b"d"), Err(E_LOADED));
}

#[test]
fn the_name_of_a_free_record_is_not_compared() {
    // record 0 is free but holds the stale name "a"
    let used = [false, true, true, true];
    assert_eq!(pick_record(4, |i| used[i], |i| NAMES[i] == b"a"), Ok(0));
}
