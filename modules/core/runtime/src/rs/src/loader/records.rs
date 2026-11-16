//! The loader's records: one per loaded file, `REC_MAX` of them. Records below `MAX_MODULES` are the module records of `DKRT_STATE.modules` (their layout and
//! the tables the proof tools read stay as they are); the records after them are the provider files' (R12-3), kept in `DKRT_PROVIDERS`, a symbol of the patch plan beside `DKRT_STATE`, so host tools read them with the same layout.
//! Only the loader writes them. Provider records have no menu and no runtime-version cell: those tables stay `MAX_MODULES` long.

use crate::state::{DKRT_MENUS, DKRT_MODULE_RUNTIME, DKRT_PATHS};
use crate::{st, Module, MAX_MODULES};
pub use crate::state::REC_MAX;
use core::ptr::addr_of_mut;
use dkrt_sdk::PROVIDER_MAX;

/// The provider records, `Module` records like `DKRT_STATE.modules` (record `MAX_MODULES + j` is slot `j`); the load-table decoder reads them.
#[no_mangle]
pub static mut DKRT_PROVIDERS: [Module; PROVIDER_MAX] = unsafe { core::mem::zeroed() };
static mut PROVIDER_PATHS: [[u8; 64]; PROVIDER_MAX] = [[0; 64]; PROVIDER_MAX];

/// The record `rec` (below `REC_MAX`).
pub unsafe fn module(rec: usize) -> &'static mut Module {
    if rec < MAX_MODULES {
        &mut st().modules[rec]
    } else {
        &mut (*addr_of_mut!(DKRT_PROVIDERS))[rec - MAX_MODULES]
    }
}

/// The store path record `rec` was loaded from (NUL padded).
pub unsafe fn path(rec: usize) -> &'static mut [u8; 64] {
    if rec < MAX_MODULES {
        &mut (*addr_of_mut!(DKRT_PATHS))[rec]
    } else {
        &mut (*addr_of_mut!(PROVIDER_PATHS))[rec - MAX_MODULES]
    }
}

/// The runtime version record `rec` needs (`MODULES` reports it); a provider has no cell.
pub unsafe fn set_runtime(rec: usize, v: u32) {
    if rec < MAX_MODULES {
        (*addr_of_mut!(DKRT_MODULE_RUNTIME))[rec] = v;
    }
}

/// The address of record `rec`'s `menu` (0: none); a provider has none.
pub unsafe fn set_menu(rec: usize, addr: u32) {
    if rec < MAX_MODULES {
        (*addr_of_mut!(DKRT_MENUS))[rec] = addr;
    }
}

/// Test only: every provider record unused.
#[cfg(test)]
pub unsafe fn reset() {
    *addr_of_mut!(DKRT_PROVIDERS) = core::mem::zeroed();
    *addr_of_mut!(PROVIDER_PATHS) = [[0; 64]; PROVIDER_MAX];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    #[test]
    fn module_and_provider_records_are_apart_and_the_state_layout_holds() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            assert_eq!(REC_MAX, MAX_MODULES + PROVIDER_MAX);
            module(MAX_MODULES - 1).base = 1;
            module(MAX_MODULES).base = 2;
            module(REC_MAX - 1).base = 3;
            assert_eq!((st().modules[MAX_MODULES - 1].base, module(MAX_MODULES).base, module(REC_MAX - 1).base), (1, 2, 3));
            path(0)[0] = b'm';
            path(MAX_MODULES)[0] = b'p';
            assert_eq!((DKRT_PATHS[0][0], path(MAX_MODULES)[0], path(REC_MAX - 1)[0]), (b'm', b'p', 0));
            // a provider has no menu or runtime cell: the call is a no-op
            set_menu(MAX_MODULES, 7);
            set_runtime(REC_MAX - 1, 7);
            set_menu(0, 9);
            assert_eq!(DKRT_MENUS[0], 9);
            *st() = core::mem::zeroed();
            *addr_of_mut!(DKRT_PROVIDERS) = core::mem::zeroed();
            *addr_of_mut!(PROVIDER_PATHS) = [[0; 64]; PROVIDER_MAX];
            DKRT_PATHS[0][0] = 0;
            DKRT_MENUS[0] = 0;
        }
    }
}
