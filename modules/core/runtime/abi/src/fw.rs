//! Interface to the firmware's parameter page, for services (`Fw`): what a service needs of the page the user is on, with no firmware name.
//!
//! The machines service reads and writes a parameter from a click's flow through it (`paramapi`), and asks the page which parameter lies under an
//! encoder. The runtime implements it once (`RuntimeFw`, `fw.rs` of the runtime crate: the page's own vtable slots, the key state, the stock
//! setter); a host test implements it with a fake (`dkrt-test-support::FakeFw`). A function that needs it takes `fw: &impl Fw`.

/// Logical key indexes the stock parameter page tests before it writes (FUNC and the trig/track modes).
pub const MODE_KEYS: [u32; 3] = [0x10, 0x11, 0x16];

/// What the firmware gives a service (one implementation on the device, one fake in the tests).
pub trait Fw {
    /// Any of `MODE_KEYS` is held.
    fn mode_key_held(&self) -> bool;
    /// The page's own value of `code`, raw (the page's track, no p-lock step).
    fn page_value(&self, view: usize, code: u32) -> i32;
    /// The parameter object of `code` for `track` (0: none).
    fn object(&self, view: usize, code: u32, track: u32) -> usize;
    /// The value of `code` in `obj`, raw.
    fn object_value(&self, obj: usize, code: u32) -> i32;
    /// The stock absolute setter.
    fn store(&self, obj: usize, code: u32, raw: i32, track: u32);
    /// The track the page shows (`track_ref` is the page's track reference word).
    fn view_track(&self, track_ref: usize) -> u32;
    /// The parameter code under encoder `enc` of the page `view` shows (a negative code or 0: none). The page's own `encoder -> code` function.
    fn param_code(&self, view: usize, enc: u32) -> i32;
}
