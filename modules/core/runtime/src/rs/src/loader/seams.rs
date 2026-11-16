//! What the loader needs from outside, as two traits (docs/plans/runtime-and-services.md section 13.2, R12-3a). The real types call the store, the arena, the
//! module entry and the registry; the fakes of the host tests log the calls. Each trait has one real implementation, so there is no `dyn`.

use crate::entry::Ctx;
use dkrt_abi::service::ServiceRow;

/// A staged file: `size` bytes at `ptr`, valid until the next `stage`.
#[derive(Clone, Copy)]
pub struct Staged {
    pub ptr: *const u8,
    pub size: u32,
}

/// An arena block: the host pointer to write through, and the address the module sees (the same number on the device).
#[derive(Clone, Copy)]
pub struct Block {
    pub ptr: *mut u8,
    pub addr: u32,
}

/// Where a module's bytes come from and where they go: the file store, the runtime's import table, the arena and the calls into module code.
pub trait Images {
    /// Read the file at the NUL-terminated store `path` into the staging buffer. Errors are the loader's result codes (`E_NOT_FOUND`, `E_SHA`, ...).
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32>;
    /// A block of at least `n` bytes; `None` when none fits.
    unsafe fn alloc(&mut self, n: u32) -> Option<Block>;
    unsafe fn free(&mut self, addr: u32);
    /// The address of the import `name` (NUL-terminated), 0 when the runtime has none (`import_addr`).
    unsafe fn import_addr(&mut self, name: *const u8) -> u32;
    /// The address of the service-only import `name` (`SVC_IMPORTS`: the `dkc_*` of interface C, `dk_service_declare_row`, `dkc_panic`), 0 when there is none. The loader
    /// asks only for a file of role 1, after `import_addr`.
    unsafe fn svc_import_addr(&mut self, _name: *const u8) -> u32 {
        0
    }
    /// The module whose absence leaves the import `name` without an address (`import_needs`).
    unsafe fn import_needs(&mut self, name: *const u8) -> Option<&'static str>;
    /// The files of the store whose path starts with `prefix` (NUL-terminated), in path order, at most `max` into `out`: the count, or a negative code (`E_BUSY`).
    unsafe fn list(&mut self, prefix: *const u8, out: *mut dkrt_sdk::DkrtStoreEntry, max: u32) -> i32;
    /// Call `init` of module record `rec` (the address `init` in module memory) with its context; its result.
    unsafe fn enter_init(&mut self, rec: usize, init: u32, ctx: *const Ctx) -> i32;
    unsafe fn enter_fini(&mut self, rec: usize, fini: u32, ctx: *const Ctx);
}

/// The registry of what a loaded file publishes (R12-4): the rows of a service file, and the rows of a module's records.
pub trait Registry {
    /// Does the runtime accept a service file with this ABI fingerprint (header word 0x3c)? 0: the file uses neither interface B nor C.
    fn abi_ok(&self, fingerprint: u32) -> bool;
    /// Publish the service row that `init` of record `rec` declared (`None`: it declared none, as every module today).
    unsafe fn add(&mut self, rec: usize, row: Option<&ServiceRow>) -> i32;
    /// Take everything record `rec` published down (idempotent).
    unsafe fn remove(&mut self, rec: usize);
}

/// The store, the arena and the module entry of the device.
pub struct RealImages;

impl Images for RealImages {
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32> {
        let found = crate::store::find(path)?;
        let r = crate::store::load_to_stage(&found);
        if r != dkrt_sdk::OK {
            return Err(r);
        }
        Ok(Staged { ptr: crate::store::stage() as *const u8, size: found.size })
    }
    unsafe fn list(&mut self, prefix: *const u8, out: *mut dkrt_sdk::DkrtStoreEntry, max: u32) -> i32 {
        crate::store::list(prefix, out, max)
    }
    unsafe fn alloc(&mut self, n: u32) -> Option<Block> {
        let p = crate::arena::alloc(n);
        if p.is_null() {
            None
        } else {
            Some(Block { ptr: p, addr: p as u32 })
        }
    }
    unsafe fn free(&mut self, addr: u32) {
        crate::arena::free(addr as *mut u8);
    }
    unsafe fn import_addr(&mut self, name: *const u8) -> u32 {
        crate::import_addr(name)
    }
    unsafe fn svc_import_addr(&mut self, name: *const u8) -> u32 {
        crate::svc_imports::svc_import_addr(name)
    }
    unsafe fn import_needs(&mut self, name: *const u8) -> Option<&'static str> {
        crate::import_needs(name)
    }
    unsafe fn enter_init(&mut self, rec: usize, init: u32, ctx: *const Ctx) -> i32 {
        let call: unsafe extern "C" fn(*const Ctx) -> i32 = core::mem::transmute(init as usize);
        crate::dlog::call_init(rec, call, ctx)
    }
    unsafe fn enter_fini(&mut self, rec: usize, fini: u32, ctx: *const Ctx) {
        #[cfg(test)]
        {
            let _ = ctx;
            crate::cleanup_tests::fini(rec, fini);
        }
        #[cfg(not(test))]
        {
            let call: unsafe extern "C" fn(*const Ctx) = core::mem::transmute(fini as usize);
            crate::dlog::call_fini(rec, call, ctx);
        }
    }
}

/// The runtime's own registry: the machine, parameter and page rows of a module's records, and the service row of a service file (`svc_rows.rs`).
pub struct RealRegistry;

impl Registry for RealRegistry {
    fn abi_ok(&self, fingerprint: u32) -> bool {
        fingerprint == 0 || fingerprint == dkrt_abi::service::SVC_ABI
    }
    unsafe fn add(&mut self, rec: usize, row: Option<&ServiceRow>) -> i32 {
        match row {
            None => dkrt_sdk::OK,
            Some(row) => crate::svc_rows::add(rec, row),
        }
    }
    unsafe fn remove(&mut self, rec: usize) {
        // the registry rows of a module record first (the service's own `remove` runs over the table with its row in), then the service row
        if rec < crate::MAX_MODULES {
            #[cfg(test)]
            crate::cleanup_tests::registry_remove(rec as u8 + 1);
            #[cfg(not(test))]
            crate::registry::remove(rec as u8 + 1);
        }
        crate::svc_rows::remove(rec);
    }
}
