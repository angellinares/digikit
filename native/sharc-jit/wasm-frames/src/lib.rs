//! The native SHARC+ core as a WebAssembly module that replays a frame
//! pack (`native/sharc` `frames`), for timing it under wasmtime, Node and
//! browsers against the native build.
//!
//! The host supplies one import, `host.now_ns() -> f64` (a monotonic clock
//! in nanoseconds), and drives the exports:
//!
//! ```text
//! sw_alloc(len) -> ptr          a buffer for the pack bytes
//! sw_open(ptr, len) -> frames   parse the pack (takes the buffer), load
//! sw_blocks(on)                 generated blocks on/off (off: interpreter)
//! sw_reload() -> 0|-1           back to the start state
//! sw_frame(k) -> instructions   run frame K; -1 on an error (sw_error)
//! sw_last_ns() -> f64           the block handler call's time
//! sw_hash(out)                  SHA-256 of the canonical state (32 bytes)
//! sw_info(out, cap) -> len      the build description (JSON)
//! sw_error(out, cap) -> len     the last error's text
//! sw_single_steps() -> f64      instructions run by the interpreter so far
//! ```

use sharc_native::Engine;
use sharc_native::frames::{self, Pack};
use std::cell::RefCell;

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    fn now_ns() -> f64;
}

fn clock() -> u64 {
    // SAFETY: a host function with no arguments and no memory access.
    unsafe { now_ns() as u64 }
}

struct Run {
    pack: Pack<'static>,
    e: Engine,
    last_ns: f64,
}

thread_local! {
    static RUN: RefCell<Option<Run>> = const { RefCell::new(None) };
    static ERR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fail(msg: String) {
    ERR.with(|e| *e.borrow_mut() = msg);
}

fn copy_out(text: &[u8], out: *mut u8, cap: usize) -> i32 {
    let n = text.len().min(cap);
    // SAFETY: the host passes CAP writable bytes at OUT.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, n) };
    n as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// PTR/LEN come from one `sw_alloc(LEN)` call; the buffer is kept for good.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_open(ptr: *mut u8, len: usize) -> i32 {
    // SAFETY: allocated by sw_alloc(len) as a Vec<u8> of that length.
    let bytes: &'static [u8] = unsafe { Vec::from_raw_parts(ptr, len, len) }.leak();
    let run = frames::parse(bytes).and_then(|pack| {
        let e = frames::load(&pack)?;
        Ok(Run {
            pack,
            e,
            last_ns: 0.0,
        })
    });
    match run {
        Ok(r) => {
            let n = r.pack.frames.len() as i32;
            RUN.with(|x| *x.borrow_mut() = Some(r));
            n
        }
        Err(w) => {
            fail(w);
            -1
        }
    }
}

fn with_run<T>(f: impl FnOnce(&mut Run) -> T) -> T {
    RUN.with(|x| f(x.borrow_mut().as_mut().expect("sw_open first")))
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_blocks(on: i32) {
    with_run(|r| r.e.use_blocks = on != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_first() -> u32 {
    with_run(|r| r.pack.first)
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_reload() -> i32 {
    with_run(|r| match frames::reload(&mut r.e, &r.pack) {
        Ok(()) => 0,
        Err(w) => {
            fail(w);
            -1
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_frame(k: u32) -> f64 {
    with_run(|r| {
        let Some(&data) = r.pack.frames.get(k as usize) else {
            fail(format!("no frame {k}"));
            return -1.0;
        };
        match frames::frame(&mut r.e, &r.pack, data, &clock) {
            Ok(o) => {
                r.last_ns = o.handler_time as f64;
                o.instructions as f64
            }
            Err(w) => {
                fail(format!("frame {}: {w}", r.pack.first + k));
                -1.0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_last_ns() -> f64 {
    with_run(|r| r.last_ns)
}

#[unsafe(no_mangle)]
pub extern "C" fn sw_single_steps() -> f64 {
    with_run(|r| r.e.stats.single_steps as f64)
}

/// # Safety
/// OUT points to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_hash(out: *mut u8) {
    let h = with_run(|r| frames::state_hash(&r.e));
    copy_out(&h, out, 32);
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_info(out: *mut u8, cap: usize) -> i32 {
    copy_out(sharc_native::build_info().as_bytes(), out, cap)
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_error(out: *mut u8, cap: usize) -> i32 {
    ERR.with(|e| copy_out(e.borrow().as_bytes(), out, cap))
}
