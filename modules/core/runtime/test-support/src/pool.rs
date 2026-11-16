//! The sample backing plug-in point (`dkrt_pool.h`, the `dkpool_*` symbols of the sample-pool adapter), absent in a host test: version 0 (the runtime
//! refuses it), every load refused, no pending request. The runtime's tests and the unit tests of the samples service link these so the optional
//! symbols the service names resolve.
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
#[no_mangle]
pub extern "C" fn dkpool_cancel(_: u32) {
    panic!("host tests have no pending sample-pool requests")
}
#[no_mangle]
pub extern "C" fn dkpool_peaks(_: *const u8, _: *mut u8, _: u32) -> i32 {
    -48
}
#[no_mangle]
pub extern "C" fn dkpool_window_step(
    _: *const u8,
    _: *const u8,
    _: u32,
    _: u32,
    _: u32,
    _: *mut i16,
    _: *mut u32,
) -> i32 {
    -48
}
