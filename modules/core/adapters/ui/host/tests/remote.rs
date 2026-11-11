//! The settings page of digikit/remote (modules/tools/remote/src/c/remote.c) on the fake <dk/platform/ui.h>, with a fake core/input.
use dkui_host::driver::*;
use dkui_host::*;
use std::ffi::c_int;

extern "C" {
    fn r_init() -> c_int;
    fn r_cmd(cmd: c_int, body: *const u8, len: c_int) -> c_int;
    fn r_page() -> *mut DkUiMenu;
    fn r_key_down(code: c_int) -> c_int;
    fn r_fail_next(result: c_int);
}

const KEY: c_int = 0x38;
const TURN: c_int = 0x39;

fn cmd(c: c_int, body: &[u8]) -> c_int {
    unsafe { r_cmd(c, body.as_ptr(), body.len() as c_int) }
}

/// The module is initialised once per process; each test builds the page in its own UI model.
fn page() -> (std::sync::MutexGuard<'static, ()>, *mut DkUiMenu) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let g = session();
    ONCE.call_once(|| assert_eq!(unsafe { r_init() }, 0));
    (g, unsafe { r_page() })
}

#[test]
fn the_page_shows_what_the_module_holds_and_releases_it() {
    let (_g, p) = page();
    assert_eq!(open(p), 0);
    assert_eq!(title(), "REMOTE CONTROL");
    assert_eq!(render(), ["KEYS HELD 0", "LAST: NONE", "RELEASE ALL KEYS"]);
    assert_eq!(cmd(KEY, &[5, 1]), 0);
    assert_eq!(cmd(KEY, &[100, 1]), 0);
    open(p); // an open menu is filled again
    assert_eq!(
        render(),
        ["KEYS HELD 2", "LAST: KEY 100D", "RELEASE ALL KEYS"]
    );
    assert_eq!(
        (unsafe { r_key_down(5) }, unsafe { r_key_down(100) }),
        (1, 1)
    );
    // the action lets go of every key it holds and the page shows it
    press_yes(2);
    assert_eq!(
        (unsafe { r_key_down(5) }, unsafe { r_key_down(100) }),
        (0, 0)
    );
    assert_eq!(
        render(),
        ["KEYS HELD 0", "LAST: RELEASE", "RELEASE ALL KEYS"]
    );
    // a key up takes it off the count
    assert_eq!(cmd(KEY, &[7, 1]), 0);
    assert_eq!(cmd(KEY, &[7, 0]), 0);
    open(p);
    assert_eq!(render()[..2], ["KEYS HELD 0", "LAST: KEY 7U"]);
}

#[test]
fn turns_and_errors_are_shown() {
    let (_g, p) = page();
    assert_eq!(cmd(TURN, &[3, 5]), 1);
    open(p);
    assert_eq!(render()[1], "LAST: TURN 3 5");
    // a key that core/input refuses is not held, and the page says so
    unsafe { r_fail_next(-1) };
    assert_eq!(cmd(KEY, &[9, 1]), -1);
    open(p);
    let r = render();
    assert_eq!(r[0], "KEYS HELD 0");
    assert!(r.contains(&"ERROR -1".to_string()), "{r:?}");
    assert_eq!(unsafe { r_key_down(9) }, 0);
}
