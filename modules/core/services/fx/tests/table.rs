//! Host tests of the fx service over `FakeCore`: the image table of `core/fxpages` (`CoreApi::info`), the send limit that comes from it, and what the service does
//! when the table is missing or does not fit (the file refuses to load). The page rows and the hooks write raw 32-bit firmware addresses, which a host cannot
//! reach: the device tests (`core/fxpages`, with `fx.dks` on the card) cover those.
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::service::Service;
use dkrt_sdk::proto::CAPK_SEND_SLOTS;
use dkrt_sdk::{E_NO_REGISTRY, OK};
use dkrt_svc_fx::fxpage::{self, INFO_HASH};
use dkrt_svc_fx::records::Fx;
use dkrt_test_support::FakeCore;
use std::sync::Mutex;
use std::vec::Vec;

/// The service keeps its bound table in statics: one test at a time.
static LOCK: Mutex<()> = Mutex::new(());

const MAGIC: u32 = 0x4658_5047; // "FXPG"
const DEVICE: u32 = 1;

/// A version-`version` image table of `LIMIT_FX_SLOTS` = 3 effect rows, 32 values and `cells` send cells (words as the adapter writes them).
fn table(version: u32, device: u32, cells: u32) -> Vec<u32> {
    let mut t = std::vec![0u32; 64];
    t[0] = MAGIC;
    t[1] = version;
    t[2] = device;
    t[4] = 0x2c; // row size
    t[5] = 14; // the page number of row 0
    t[6] = 3; // rows
    t[8] = 8; // list capacity
    t[15] = 32; // values
    t[37] = 70; // the mark's array index
    t[38] = 0x46a7;
    t[39] = cells;
    t[61] = 13; // the track FX page
    t[63] = 0x80; // the storage record's spare halfwords
    t
}

fn core_with(table: Vec<u32>) -> FakeCore {
    let mut core = FakeCore::new();
    core.device = DEVICE;
    core.info_tables.push((INFO_HASH, table));
    core
}

#[test]
fn the_service_reads_its_table_by_the_hash_of_the_symbol_name() {
    // FNV-1a 32 of "fxpages_info": the name the runtime and the packer use for the symbol
    assert_eq!(INFO_HASH, dkneeds::keys::name_hash("fxpages_info"));
}

#[test]
fn without_a_table_or_with_one_that_does_not_fit_the_file_refuses_to_load() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        fxpage::unbind();
        // no `core/fxpages` in the image
        assert_eq!(fxpage::setup(&FakeCore::new()), E_NO_REGISTRY);
        // an older table has no words for the hooks
        assert_eq!(
            fxpage::setup(&core_with(table(2, DEVICE, 2))),
            E_NO_REGISTRY
        );
        // a table of another device
        assert_eq!(
            fxpage::setup(&core_with(table(3, DEVICE + 1, 2))),
            E_NO_REGISTRY
        );
        // a table that asks for more rows than the service holds state for
        let mut big = table(3, DEVICE, 2);
        big[6] = 99;
        assert_eq!(fxpage::setup(&core_with(big)), E_NO_REGISTRY);
        assert!(!fxpage::available(&FakeCore::new()));
    }
}

#[test]
fn a_current_table_binds_and_gives_the_pages_and_the_send_limit() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        fxpage::unbind();
        let core = core_with(table(3, DEVICE, 2));
        assert_eq!(fxpage::setup(&core), OK);
        assert!(fxpage::available(&core));
        // pages reserved by the image, none in use
        assert_eq!(fxpage::counts(&core), (3, 0));
        // the CAPACITY kind of the send cells comes from the table; another kind is not this service's
        assert_eq!(
            <Fx as Service<FakeCore>>::cap(&core, CAPK_SEND_SLOTS),
            Some((2, 0))
        );
        assert_eq!(<Fx as Service<FakeCore>>::cap(&core, 0xee), None);
    }
}

#[test]
fn the_send_limit_is_the_cells_of_the_table_up_to_what_the_service_holds() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        for (cells, limit) in [(0, 0), (1, 1), (2, 2), (5, 2)] {
            fxpage::unbind();
            let core = core_with(table(3, DEVICE, cells));
            assert_eq!(fxpage::setup(&core), OK);
            assert_eq!(
                dkrt_svc_fx::fxsend::slots(&core),
                limit,
                "{cells} cells in the table"
            );
        }
        // an image without a send page: no limit and no effect on the send route
        fxpage::unbind();
        assert_eq!(dkrt_svc_fx::fxsend::slots(&FakeCore::new()), 0);
    }
}

#[test]
fn the_fx_row_method_answers_no_fx_when_the_service_has_no_page() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        fxpage::unbind();
        let core = core_with(table(3, DEVICE, 2));
        assert_eq!(fxpage::setup(&core), OK);
        let mut out: dkrt_sdk::DkrtFx = core::mem::zeroed();
        assert_eq!(
            <Fx as Service<FakeCore>>::fx_entry(&core, 0, &mut out),
            dkrt_sdk::E_NOT_FOUND
        );
        let _ = core.device_id();
    }
}
