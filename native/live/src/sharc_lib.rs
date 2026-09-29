//! The native SHARC+ core loaded from its shared library (`native/sharc`'s
//! C ABI, `libsharc_native.dylib`), as a [`SharcCore`].
//!
//! This is the OS-specific edge of the SHARC frame source: `dlopen`/`dlsym`
//! from the platform's C library (declared here, no crate). The frame logic
//! itself (`sharc_source`) only sees the [`SharcCore`] trait, so a build
//! that links `native/sharc` directly (e.g. for WASM) can implement the
//! trait over its `Engine` instead.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;

use std::sync::Arc;

use crate::sharc_source::{
    AfterEnd, CaptureSource, CoreStats, LivePack, LiveSource, Peek, SharcCore,
};

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

const RTLD_NOW: c_int = 2;

type Handle = *mut c_void;
type CreateFn = unsafe extern "C" fn(*const u8, usize) -> Handle;
type DestroyFn = unsafe extern "C" fn(Handle);
type ImportFn = unsafe extern "C" fn(Handle, *const u8, usize) -> i32;
type StepFn = unsafe extern "C" fn(Handle, u32) -> i32;
type HaltFn = unsafe extern "C" fn(Handle, *mut u8, usize) -> i32;
type OptionFn = unsafe extern "C" fn(Handle, u32, i64) -> i32;
type StatsFn = unsafe extern "C" fn(Handle, *mut u64, usize) -> i32;
type SetRegFn = unsafe extern "C" fn(Handle, u32, u32, u32, u32) -> i32;
type PokeFn = unsafe extern "C" fn(Handle, u64, *const u8, usize, u32) -> i32;
type PeekFn = unsafe extern "C" fn(Handle, u64, u32) -> i64;
type FreshCallFn = unsafe extern "C" fn(Handle, u32, i64) -> i32;
type InfoFn = unsafe extern "C" fn(*mut u8, usize) -> i32;

/// The library's entry points (the library itself stays loaded for the
/// life of the process: it is never `dlclose`d).
#[derive(Clone, Copy)]
struct Api {
    create: CreateFn,
    destroy: DestroyFn,
    import: ImportFn,
    step: StepFn,
    halt: HaltFn,
    option: OptionFn,
    stats: StatsFn,
    set_reg: SetRegFn,
    poke: PokeFn,
    peek: PeekFn,
    fresh_call: FreshCallFn,
    info: InfoFn,
}

fn last_dl_error() -> String {
    // SAFETY: dlerror returns NULL or a NUL-terminated thread-local string.
    let p = unsafe { dlerror() };
    if p.is_null() {
        "unknown dlopen error".to_string()
    } else {
        // SAFETY: non-NULL, NUL-terminated (above).
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

impl Api {
    fn load(path: &Path) -> Result<Api, String> {
        let c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| "library path contains NUL".to_string())?;
        // SAFETY: C is a valid NUL-terminated path.
        let lib = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err(format!("dlopen {}: {}", path.display(), last_dl_error()));
        }
        let sym = |name: &str| -> Result<*mut c_void, String> {
            let n = CString::new(name).expect("symbol name");
            // SAFETY: LIB is a live dlopen handle, N a NUL-terminated name.
            let p = unsafe { dlsym(lib, n.as_ptr()) };
            if p.is_null() {
                Err(format!("{}: missing symbol {name}", path.display()))
            } else {
                Ok(p)
            }
        };
        // SAFETY: each symbol is the native/sharc C ABI function of that
        // name, whose signature the type aliases above mirror (lib.rs).
        unsafe {
            Ok(Api {
                create: std::mem::transmute::<*mut c_void, CreateFn>(sym("sharc_native_create")?),
                destroy: std::mem::transmute::<*mut c_void, DestroyFn>(sym(
                    "sharc_native_destroy",
                )?),
                import: std::mem::transmute::<*mut c_void, ImportFn>(sym(
                    "sharc_native_import_state",
                )?),
                step: std::mem::transmute::<*mut c_void, StepFn>(sym("sharc_native_step")?),
                halt: std::mem::transmute::<*mut c_void, HaltFn>(sym("sharc_native_halt_reason")?),
                option: std::mem::transmute::<*mut c_void, OptionFn>(sym(
                    "sharc_native_set_option",
                )?),
                stats: std::mem::transmute::<*mut c_void, StatsFn>(sym("sharc_native_stats")?),
                set_reg: std::mem::transmute::<*mut c_void, SetRegFn>(sym("sharc_native_set_reg")?),
                poke: std::mem::transmute::<*mut c_void, PokeFn>(sym("sharc_native_poke")?),
                peek: std::mem::transmute::<*mut c_void, PeekFn>(sym("sharc_native_peek")?),
                fresh_call: std::mem::transmute::<*mut c_void, FreshCallFn>(sym(
                    "sharc_native_fresh_call",
                )?),
                info: std::mem::transmute::<*mut c_void, InfoFn>(sym("sharc_native_info")?),
            })
        }
    }
}

/// One engine of the loaded library.
pub struct LibCore {
    api: Api,
    handle: Handle,
}

// SAFETY: an engine is plain owned memory with no thread affinity; LibCore
// owns its handle exclusively and is moved, never shared, between threads.
unsafe impl Send for LibCore {}

impl LibCore {
    /// Load the library at PATH and create an engine over IMAGE (the pack's
    /// image blob, `tools/sharc_transpile_run.py pack_image`).
    pub fn open(path: &Path, image: &[u8]) -> Result<LibCore, String> {
        let api = Api::load(path)?;
        // SAFETY: IMAGE is IMAGE.len() readable bytes.
        let handle = unsafe { (api.create)(image.as_ptr(), image.len()) };
        if handle.is_null() {
            return Err("sharc_native_create rejected the image blob".to_string());
        }
        Ok(LibCore { api, handle })
    }

    /// The build's JSON description (core and image hashes, block count).
    pub fn info(&self) -> String {
        let mut buf = vec![0u8; 4096];
        // SAFETY: BUF has buf.len() writable bytes.
        let n = unsafe { (self.api.info)(buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }
}

impl Drop for LibCore {
    fn drop(&mut self) {
        // SAFETY: HANDLE came from create and is not used afterwards.
        unsafe { (self.api.destroy)(self.handle) };
    }
}

/// Load LIB and PACK and build the capture source over them; also the
/// library's build description.
pub fn capture_source(
    lib: &Path,
    pack: &Path,
    after: AfterEnd,
    gain: f32,
) -> Result<(CaptureSource<LibCore>, String), String> {
    let pack = Arc::new(LivePack::load(pack)?);
    let core = LibCore::open(lib, pack.image())?;
    let info = core.info();
    pack.check_library(&info)?;
    let source = CaptureSource::new(core, pack, after, gain)?;
    Ok((source, info))
}

/// Load LIB and the state pack PACK and build the live source over them
/// (see `LiveSource`); with CARD, refuse a pack fed from another card image
/// (`LivePack::check_card`). Also the library's build description.
pub fn live_source(
    lib: &Path,
    pack: &Path,
    gain: f32,
    card: Option<&str>,
) -> Result<(LiveSource<LibCore>, String), String> {
    let pack = LivePack::load(pack)?;
    if let Some(card) = card {
        pack.check_card(card)?;
    }
    let core = LibCore::open(lib, pack.image())?;
    let info = core.info();
    pack.check_library(&info)?;
    let source = LiveSource::new(core, &pack, gain)?;
    Ok((source, info))
}

impl SharcCore for LibCore {
    fn import_state(&mut self, blob: &[u8]) -> Result<(), i32> {
        // SAFETY: live handle; BLOB covers its length.
        match unsafe { (self.api.import)(self.handle, blob.as_ptr(), blob.len()) } {
            0 => Ok(()),
            code => Err(code),
        }
    }

    fn set_option(&mut self, key: u32, value: i64) -> i32 {
        // SAFETY: live handle.
        unsafe { (self.api.option)(self.handle, key, value) }
    }

    fn poke(&mut self, address: u64, data: &[u8], width: u32) -> i32 {
        // SAFETY: live handle; DATA covers its length.
        unsafe { (self.api.poke)(self.handle, address, data.as_ptr(), data.len(), width) }
    }

    fn peek(&mut self, address: u64, width: u32) -> Peek {
        // SAFETY: live handle.
        let v = unsafe { (self.api.peek)(self.handle, address, width) };
        if v < 0 {
            Peek::Mmr
        } else if v & (1 << 32) != 0 {
            Peek::Known(v as u32)
        } else {
            Peek::Unknown
        }
    }

    fn fresh_call(&mut self, pc: u32) {
        // SAFETY: live handle.
        unsafe { (self.api.fresh_call)(self.handle, pc, -1) };
    }

    fn set_reg_const(&mut self, code: u32, value: u32) {
        // SAFETY: live handle.
        unsafe { (self.api.set_reg)(self.handle, code, 1, value, 0) };
    }

    fn step(&mut self, n: u32) -> u32 {
        // SAFETY: live handle.
        unsafe { (self.api.step)(self.handle, n) }.max(0) as u32
    }

    fn halt_reason(&mut self, buf: &mut [u8]) -> usize {
        // SAFETY: live handle; BUF has buf.len() writable bytes.
        let n = unsafe { (self.api.halt)(self.handle, buf.as_mut_ptr(), buf.len()) };
        n.max(0) as usize
    }

    fn stats(&mut self) -> CoreStats {
        let mut v = [0u64; 7];
        // SAFETY: live handle; V has 7 u64s.
        unsafe { (self.api.stats)(self.handle, v.as_mut_ptr(), v.len()) };
        CoreStats {
            instructions: v[0],
            block_entries: v[1],
            block_instructions: v[2],
            single_steps: v[3],
            traps: v[4],
        }
    }
}
