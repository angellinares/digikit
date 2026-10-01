//! Minimal raw ABI for the browser host. JSON output remains valid until the
//! next ABI call that replaces it.

use std::{cell::RefCell, slice};

use crate::{Emulator, ExecutionPolicy};

const ALLOCATION_LIMIT: usize = 32 * 1024 * 1024;

thread_local! {
    static EMULATOR: RefCell<Option<Emulator>> = const { RefCell::new(None) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn result(value: serde_json::Value) {
    RESULT.with(|output| *output.borrow_mut() = serde_json::to_vec(&value).expect("JSON"));
}

fn failure(error: impl ToString) -> i32 {
    result(serde_json::json!({"error": error.to_string()}));
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_alloc(len: usize) -> *mut u8 {
    if len > ALLOCATION_LIMIT {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

/// The caller must pass exactly the pointer and length returned by `digi_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len <= ALLOCATION_LIMIT {
        unsafe {
            drop(Box::from_raw(slice::from_raw_parts_mut(ptr, len)));
        }
    }
}

unsafe fn load_with_policy(ptr: *const u8, len: usize, policy: ExecutionPolicy) -> i32 {
    if ptr.is_null() || len == 0 || len > ALLOCATION_LIMIT {
        return failure("invalid SYX input");
    }
    let syx = unsafe { slice::from_raw_parts(ptr, len) };
    match Emulator::new_with_policy(syx, None, policy) {
        Ok(mut emulator) => {
            let snapshot = emulator.snapshot();
            EMULATOR.with(|slot| *slot.borrow_mut() = Some(emulator));
            result(serde_json::json!({"snapshot": snapshot}));
            0
        }
        Err(error) => failure(error),
    }
}

/// Loads the reference-compatible default runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load(ptr: *const u8, len: usize) -> i32 {
    unsafe { load_with_policy(ptr, len, ExecutionPolicy::Reference) }
}

/// Loads the opt-in SoftfloatAbiV1 prototype; callers must not assume exact
/// callee scratch-register, stack-scratch, CCR, or timing equivalence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load_softfloat_abi_v1(ptr: *const u8, len: usize) -> i32 {
    unsafe { load_with_policy(ptr, len, ExecutionPolicy::SoftfloatAbiV1) }
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_step(budget: u32) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => {
            result(serde_json::json!({"snapshot": emulator.step_chunk(budget)}));
            0
        }
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_button(code: u8, down: u8) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => match emulator.button(code, down != 0) {
            Ok(()) => {
                result(serde_json::json!({"ok": true}));
                0
            }
            Err(error) => failure(error),
        },
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_turn(encoder: u8, delta: i32) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => match emulator.turn(encoder, delta) {
            Ok(()) => {
                result(serde_json::json!({"ok": true}));
                0
            }
            Err(error) => failure(error),
        },
        None => failure("no emulator loaded"),
    })
}

/// Read-only report; does not consume a pending display frame.
#[unsafe(no_mangle)]
pub extern "C" fn digi_diagnostics() -> i32 {
    EMULATOR.with(|slot| match slot.borrow().as_ref() {
        Some(emulator) => {
            result(serde_json::json!(emulator.diagnostics()));
            0
        }
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_stop() {
    EMULATOR.with(|slot| *slot.borrow_mut() = None);
    result(serde_json::json!({"ok": true}));
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_result_ptr() -> *const u8 {
    RESULT.with(|output| output.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_result_len() -> usize {
    RESULT.with(|output| output.borrow().len())
}
