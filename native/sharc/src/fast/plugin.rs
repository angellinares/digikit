//! A kernel backend in a shared library, for binaries that do not link one
//! (`SHARC_FAST_BACKEND=/path/libsharc_fast_cl.dylib`). The boundary is a C
//! ABI over the serialised kernel (`Kernel::encode`), so host and plug-in
//! need not share a Rust type layout:
//!
//! ```c
//! void*    sharc_fast_compile(const uint32_t* words, size_t n, char* err, size_t cap);
//! uint32_t sharc_fast_run(void* kernel, uint8_t* ctx);
//! void     sharc_fast_free(void* kernel);
//! ```

#![cfg(unix)]

use super::ir::*;
use std::ffi::{CString, c_char, c_void};

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

type CompileFn = unsafe extern "C" fn(*const u32, usize, *mut c_char, usize) -> *mut c_void;
type RunFn = unsafe extern "C" fn(*mut c_void, *mut u8) -> u32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

#[derive(Clone, Copy)]
pub struct PluginBackend {
    compile: CompileFn,
    run: RunFn,
    free: FreeFn,
}

struct PluginKernel {
    handle: *mut c_void,
    run: RunFn,
    free: FreeFn,
}

impl Drop for PluginKernel {
    fn drop(&mut self) {
        // SAFETY: the handle came from `sharc_fast_compile` and is freed once.
        unsafe { (self.free)(self.handle) }
    }
}

impl CompiledKernel for PluginKernel {
    unsafe fn run(&self, ctx: *mut u8) -> u32 {
        unsafe { (self.run)(self.handle, ctx) }
    }
}

impl PluginBackend {
    pub fn open(path: &str) -> Result<PluginBackend, String> {
        let c = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: plain dlopen/dlsym of a library the user named.
        unsafe {
            let h = dlopen(c.as_ptr(), 2);
            if h.is_null() {
                let e = dlerror();
                return Err(if e.is_null() {
                    format!("dlopen {path} failed")
                } else {
                    std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
                });
            }
            let sym = |name: &str| -> Result<*mut c_void, String> {
                let n = CString::new(name).unwrap();
                let p = dlsym(h, n.as_ptr());
                if p.is_null() {
                    Err(format!("{path}: no symbol {name}"))
                } else {
                    Ok(p)
                }
            };
            Ok(PluginBackend {
                compile: std::mem::transmute::<*mut c_void, CompileFn>(sym("sharc_fast_compile")?),
                run: std::mem::transmute::<*mut c_void, RunFn>(sym("sharc_fast_run")?),
                free: std::mem::transmute::<*mut c_void, FreeFn>(sym("sharc_fast_free")?),
            })
        }
    }
}

impl KernelBackend for PluginBackend {
    fn name(&self) -> &'static str {
        "plugin"
    }

    fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String> {
        let words = k.encode();
        let mut err = vec![0 as c_char; 512];
        // SAFETY: the plug-in reads `words` and writes at most `err.len()`.
        let h = unsafe { (self.compile)(words.as_ptr(), words.len(), err.as_mut_ptr(), err.len()) };
        if h.is_null() {
            // SAFETY: the plug-in NUL-terminates the message.
            let msg = unsafe { std::ffi::CStr::from_ptr(err.as_ptr()) };
            return Err(msg.to_string_lossy().into_owned());
        }
        Ok(Box::new(PluginKernel {
            handle: h,
            run: self.run,
            free: self.free,
        }))
    }
}
