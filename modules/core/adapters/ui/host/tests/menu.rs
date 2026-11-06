//! The logic of core/digikit-menu on the fake <dk/ui.h>: the menu's own C source (modules/core/adapters/ui/host/tests/c/menu_fake.c) with a fake runtime.
use dkui_host::driver::*;
use dkui_host::*;
use std::ffi::{c_char, c_int, CStr, CString};

extern "C" {
    fn fk_reset();
    fn fk_add(path: *const c_char, flags: u32, arena: u32, fx_slots: u32, machines: u32);
    fn fk_select(path: *const c_char);
    fn fk_scan_result(r: c_int);
    fn fk_loaded(path: *const c_char, name: *const c_char);
    fn fk_fx(id: *const c_char, allowed: u32, saved: u32);
    fn fk_loaded_menu(path: *const c_char, name: *const c_char);
    fn fk_settings_runs() -> c_int;
    fn fk_settings_acts() -> c_int;
    fn fk_send_slots(n: u32);
    fn fk_restart_result(r: c_int);
    fn fk_open_digikit() -> c_int;
    fn fk_restarts() -> c_int;
    fn fk_selection_sets() -> c_int;
    fn fk_set_count() -> c_int;
    fn fk_set_path(i: c_int) -> *const c_char;
    fn fk_puts() -> c_int;
    fn fk_put_key(i: c_int) -> *const c_char;
    fn fk_put_val(i: c_int) -> c_int;
    fn fk_flushes() -> c_int;
    fn fk_tick(i: c_int) -> c_int;
}

const COMPAT: u32 = 1;
const NEEDS: u32 = 2;
const LOADED: u32 = 4;
const OK: u32 = COMPAT | NEEDS;
const KB: u32 = 1024;
// the rows of the DIGIKIT menu
const MODULES: usize = 3;

fn add(path: &str, flags: u32, arena_kb: u32, fx_slots: u32, machines: u32) {
    let p = CString::new(path).unwrap();
    unsafe { fk_add(p.as_ptr(), flags, arena_kb * KB, fx_slots, machines) }
}

fn select(path: &str) {
    let p = CString::new(path).unwrap();
    unsafe { fk_select(p.as_ptr()) }
}

fn loaded(path: &str, name: &str) {
    let (p, n) = (CString::new(path).unwrap(), CString::new(name).unwrap());
    unsafe { fk_loaded(p.as_ptr(), n.as_ptr()) }
}

/// A loaded module that exports dkm_menu.
fn loaded_menu(path: &str, name: &str) {
    let (p, n) = (CString::new(path).unwrap(), CString::new(name).unwrap());
    unsafe { fk_loaded_menu(p.as_ptr(), n.as_ptr()) }
}

fn fx(id: &str, allowed: u32, saved: u32) {
    let i = CString::new(id).unwrap();
    unsafe { fk_fx(i.as_ptr(), allowed, saved) }
}

fn cstr(p: *const c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A fresh session with DIGIKIT open. Fill the fake store first with `store`.
fn digikit() -> std::sync::MutexGuard<'static, ()> {
    let g = session();
    unsafe { fk_reset() };
    g
}

fn open_digikit() {
    assert_eq!(unsafe { fk_open_digikit() }, 0);
}

/// Opens MODULES from the menu.
fn open_modules() {
    open_digikit();
    press_yes(MODULES);
    assert_eq!(title(), "MODULES");
}

/// Three modules: a tool, an effect (loaded, with a route) and a machine too big for the arena.
fn store_of_three() {
    add("modules/tool.dkm", OK, 20, 0, 0);
    add("modules/gain-fx.dkm", OK | LOADED, 100, 1, 0);
    add("modules/huge.dkm", OK, 900, 0, 1);
}

#[test]
fn digikit_menu_rows() {
    let _g = digikit();
    open_digikit();
    assert_eq!(title(), "DIGIKIT");
    assert_eq!(render(), ["ABOUT", "HEALTH", "MEMORY", "MODULES"]);
}

#[test]
fn modules_list_has_one_row_per_file_with_kind_and_size() {
    let _g = digikit();
    store_of_three();
    select("modules/gain-fx.dkm");
    open_modules();
    // name (12), kind (4), size right aligned; the check mark starts from the saved selection
    assert_eq!(
        render(),
        [
            "  TOOL         TOOL   20K",
            "* GAIN-FX      FX    100K",
            "  HUGE         MACH  900K",
        ]
    );
}

#[test]
fn long_names_keep_their_start_and_their_end() {
    let _g = digikit();
    add("modules/dyn-fx-gain-send.dkm", OK, 10, 1, 0);
    add("modules/dyn-fx-gain-sendonly.dkm", OK, 10, 1, 0);
    open_modules();
    let r = render();
    assert_ne!(r[0], r[1]);
    assert!(
        r[0].contains("DYN-FX~-SEND") && r[1].contains("DYN-FX~DONLY"),
        "{r:?}"
    );
}

#[test]
fn empty_and_missing_store_texts() {
    let _g = digikit();
    open_modules();
    assert_eq!(render(), ["NO MODULES ON CARD"]);
    press_no();
    press_no();
    unsafe { fk_scan_result(-1) };
    open_modules();
    assert_eq!(render(), ["NO STORE"]);
}

#[test]
fn yes_toggles_the_mark_and_nothing_is_saved_yet() {
    let _g = digikit();
    store_of_three();
    open_modules();
    press_yes(0);
    assert_eq!(unsafe { fk_tick(0) }, 1);
    assert!(render()[0].starts_with("* TOOL"));
    press_yes(0);
    assert_eq!(unsafe { fk_tick(0) }, 0);
    assert_eq!(unsafe { fk_selection_sets() }, 0);
}

#[test]
fn a_module_that_does_not_fit_is_refused_with_a_confirm() {
    let _g = digikit();
    store_of_three();
    open_modules();
    press_yes(2);
    // the mark went back off
    assert_eq!(unsafe { fk_tick(2) }, 0);
    assert!(render()[2].starts_with("  HUGE"));
    let (kind, title, text, _) = dialog().expect("a dialog");
    assert_eq!(kind, DialogKind::Confirm);
    assert_eq!(title, "HUGE DOES NOT FIT");
    assert_eq!(text, "CPU 900K > 512K FREE");
    // it is a notice: either answer closes it and changes nothing
    answer_confirm(true);
    assert!(dialog().is_none());
    press_no();
    assert_eq!(depth(), 1); // no changes pending: NO went back at once
}

#[test]
fn fit_counts_what_is_checked_already() {
    let _g = digikit();
    add("modules/a.dkm", OK, 300, 0, 1);
    add("modules/b.dkm", OK, 300, 0, 1);
    open_modules();
    press_yes(0);
    assert!(dialog().is_none());
    press_yes(1); // 300K + 300K > 512K
    let (_, title, _, _) = dialog().expect("a dialog");
    assert_eq!(title, "B DOES NOT FIT");
    answer_confirm(false);
    // unchecking the first makes room
    press_yes(0);
    press_yes(1);
    assert!(dialog().is_none());
    assert_eq!((unsafe { fk_tick(0) }, unsafe { fk_tick(1) }), (0, 1));
}

#[test]
fn a_module_for_another_runtime_is_not_compatible() {
    let _g = digikit();
    add("modules/foreign.dkm", NEEDS, 10, 0, 1);
    open_modules();
    press_yes(0);
    assert_eq!(unsafe { fk_tick(0) }, 0);
    let (_, title, text, _) = dialog().expect("a dialog");
    assert_eq!(title, "FOREIGN NOT COMPATIBLE");
    assert!(text.contains("ANOTHER DEVICE OR RUNTIME"));
}

#[test]
fn the_side_panel_has_load_route_and_info() {
    let _g = digikit();
    store_of_three();
    loaded("modules/gain-fx.dkm", "test/gain-fx");
    fx("test/gain-fx", 3, 1);
    select("modules/gain-fx.dkm");
    open_modules();
    // a tool and a machine have no route; an effect that is loaded has
    press_right(0);
    assert_eq!(render(), ["LOAD", "INFO"]);
    choose_panel_row(0); // LOAD checks the row
    assert_eq!(unsafe { fk_tick(0) }, 1);
    press_right(0);
    assert_eq!(render(), ["UNLOAD", "INFO"]);
    press_no();
    press_right(1);
    assert_eq!(render(), ["UNLOAD", "ROUTE...", "INFO"]);
    choose_panel_row(0); // UNLOAD
    assert_eq!(unsafe { fk_tick(1) }, 0);
    // LOAD on a row that does not fit is refused as YES is
    press_right(2);
    choose_panel_row(0);
    assert_eq!(dialog().unwrap().1, "HUGE DOES NOT FIT");
}

#[test]
fn a_loaded_module_with_a_menu_gets_settings_in_the_side_panel() {
    let _g = digikit();
    store_of_three();
    loaded_menu("modules/tool.dkm", "digikit/tool");
    loaded("modules/gain-fx.dkm", "test/gain-fx");
    fx("test/gain-fx", 3, 1);
    select("modules/tool.dkm");
    select("modules/gain-fx.dkm");
    open_modules();
    // only the module with a dkm_menu has the row; a loaded effect without one does not
    press_right(0);
    assert_eq!(render(), ["UNLOAD", "SETTINGS...", "INFO"]);
    press_no();
    press_right(1);
    assert_eq!(render(), ["UNLOAD", "ROUTE...", "INFO"]);
    press_no();
    // a module that is in the store but not loaded has none
    press_right(2);
    assert_eq!(render(), ["LOAD", "INFO"]);
    press_no();
    // the module's dkm_menu runs only when the user opens SETTINGS..., and again at each open
    assert_eq!(unsafe { fk_settings_runs() }, 0);
    press_right(0);
    choose_panel_row(1);
    assert_eq!(title(), "TOOL"); // the module name without its author
    assert_eq!(render(), ["HELD 0", "RELEASE"]);
    assert_eq!(unsafe { fk_settings_runs() }, 1);
    press_yes(1);
    assert_eq!(unsafe { fk_settings_acts() }, 1);
    press_no();
    press_right(0);
    choose_panel_row(1);
    assert_eq!(unsafe { fk_settings_runs() }, 2);
    assert_eq!(depth(), 3); // DIGIKIT, MODULES, the settings
}

#[test]
fn info_page_shows_the_module() {
    let _g = digikit();
    store_of_three();
    open_modules();
    press_right(0);
    choose_panel_row(1);
    assert_eq!(title(), "INFO");
    let r = render();
    assert_eq!(r[0], "TOOL");
    assert_eq!(r[1], "VERSION 0.1.0");
    assert_eq!(r[2], "SIZE 20K");
    assert!(r.contains(&"FITS".to_string()), "{r:?}");
    assert_eq!(r.last().unwrap(), "STATUS NOT LOADED");
}

#[test]
fn route_choice_is_limited_to_what_the_effect_allows() {
    let _g = digikit();
    add("modules/both.dkm", OK | LOADED, 10, 1, 0);
    add("modules/master-only.dkm", OK | LOADED, 10, 1, 0);
    loaded("modules/both.dkm", "test/both");
    loaded("modules/master-only.dkm", "test/master-only");
    fx("test/both", 3, 1);
    fx("test/master-only", 1, 1);
    unsafe { fk_send_slots(2) };
    open_modules();
    press_right(0);
    choose_panel_row(1);
    let (kind, title, _, items) = dialog().unwrap();
    assert_eq!((kind, title.as_str()), (DialogKind::Choice, "ROUTE"));
    assert_eq!(items, ["SEND", "MASTER"]);
    pick_choice(0);
    // a change of the route is a pending change
    press_no();
    let (_, title, text, _) = dialog().unwrap();
    assert_eq!(
        (title.as_str(), text.as_str()),
        ("APPLY & RESTART?", "1 CHANGE")
    );
    answer_confirm(false);
    // the second effect allows the master only
    open_modules();
    press_right(1);
    choose_panel_row(1);
    assert_eq!(dialog().unwrap().3, ["MASTER"]);
}

#[test]
fn send_is_not_offered_when_every_send_slot_is_taken() {
    let _g = digikit();
    add("modules/a.dkm", OK | LOADED, 10, 1, 0);
    add("modules/b.dkm", OK | LOADED, 10, 1, 0);
    loaded("modules/a.dkm", "test/a");
    loaded("modules/b.dkm", "test/b");
    fx("test/a", 3, 2); // on the send bus already
    fx("test/b", 3, 1); // on the master
    unsafe { fk_send_slots(1) };
    open_modules();
    press_right(1);
    choose_panel_row(1);
    assert_eq!(dialog().unwrap().3, ["MASTER"]);
    press_no(); // NO closes a choice without an answer
    assert!(dialog().is_none());
}

#[test]
fn no_without_changes_just_goes_back() {
    let _g = digikit();
    store_of_three();
    select("modules/tool.dkm");
    open_modules();
    assert_eq!(depth(), 2);
    press_no();
    assert_eq!((depth(), title().as_str()), (1, "DIGIKIT"));
    assert!(dialog().is_none());
    // a mark that was set and unset again is no change
    open_modules();
    press_yes(1);
    press_yes(1);
    press_no();
    assert_eq!(depth(), 1);
    assert!(dialog().is_none());
}

#[test]
fn no_with_changes_asks_to_apply_and_restart() {
    let _g = digikit();
    store_of_three();
    select("modules/tool.dkm");
    open_modules();
    press_yes(0); // unchecks the saved one
    press_yes(1); // checks another
    press_no();
    // the list stays and the question is asked
    assert_eq!(title(), "MODULES");
    let (kind, title, text, _) = dialog().unwrap();
    assert_eq!(
        (kind, title.as_str(), text.as_str()),
        (DialogKind::Confirm, "APPLY & RESTART?", "2 CHANGES")
    );
    answer_confirm(true);
    assert_eq!(unsafe { fk_selection_sets() }, 1);
    assert_eq!(unsafe { fk_set_count() }, 1);
    assert_eq!(cstr(unsafe { fk_set_path(0) }), "modules/gain-fx.dkm");
    assert_eq!(unsafe { fk_flushes() }, 1);
    assert_eq!(unsafe { fk_restarts() }, 1);
}

#[test]
fn no_on_the_question_drops_the_changes_and_closes_the_list() {
    let _g = digikit();
    store_of_three();
    open_modules();
    press_yes(0);
    press_no();
    assert!(dialog().is_some());
    press_no(); // NO answers the confirm
    assert_eq!(unsafe { fk_selection_sets() }, 0);
    assert_eq!(unsafe { fk_restarts() }, 0);
    assert_eq!((depth(), title().as_str()), (1, "DIGIKIT"));
    assert_eq!(unsafe { fk_tick(0) }, 0);
    // a fresh list starts from the saved selection again
    open_modules();
    assert!(render()[0].starts_with("  TOOL"));
    press_no();
    assert_eq!(depth(), 1);
}

#[test]
fn apply_writes_the_changed_routes_and_restarts() {
    let _g = digikit();
    add("modules/both.dkm", OK | LOADED, 10, 1, 0);
    loaded("modules/both.dkm", "test/both");
    fx("test/both", 3, 1); // master
    unsafe { fk_send_slots(2) };
    select("modules/both.dkm");
    open_modules();
    press_right(0);
    choose_panel_row(1);
    pick_choice(0); // SEND
    press_no();
    answer_confirm(true);
    assert_eq!(unsafe { fk_puts() }, 1);
    assert_eq!(cstr(unsafe { fk_put_key(0) }), "fx/route/test/both");
    assert_eq!(unsafe { fk_put_val(0) }, 1);
    assert_eq!((unsafe { fk_flushes() }, unsafe { fk_restarts() }), (1, 1));
}

#[test]
fn a_refused_restart_is_shown() {
    let _g = digikit();
    store_of_three();
    unsafe { fk_restart_result(-43) };
    open_modules();
    press_yes(0);
    press_no();
    answer_confirm(true);
    let (_, title, text, _) = dialog().expect("the refusal");
    assert_eq!((title.as_str(), text.as_str()), ("NO RESTART", "CARD BUSY"));
}
