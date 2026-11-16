//! The service over a `FakeCore` and a `FakeFw`: the record checks, `apply` and `remove` of parameter rows and click actions, the native page
//! record and the capacity counts. A machine or a page row is written at absolute addresses of the firmware's tables (`wr`), which a host cannot
//! reach: those parts run in the device tests (`core/dynreg`, `core/input`).
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::registry::w;
use dkrt_abi::service::*;
use dkrt_sdk::records::*;
use dkrt_sdk::*;
use dkrt_svc_machines::records::*;
use dkrt_svc_machines::slots;
use dkrt_test_support::{Call, FakeCore, FakeFw};
use std::sync::Mutex;
use std::vec::Vec;

/// The tests that drive the statics of the crate (the click state and `APPLIED`) take this lock.
static TEST_LOCK: Mutex<()> = Mutex::new(());

const P_FIRST: u32 = 1000;
const P_DYN0: u32 = 200;
const SLOT_BASE: u32 = 10;
const OWNER: u8 = 3;
const BASE: u32 = 0x4000;

/// The words of `dynreg_info` that the parameter, action and native page parts read; no page rows (`PG_ROWS` 0).
fn table() -> Vec<u32> {
    let mut t = std::vec![0u32; 96];
    t[w::M_DYN_N] = 8;
    t[w::P_FIRST] = P_FIRST;
    t[w::P_DYN0] = P_DYN0;
    t[w::P_DYN_N] = 16;
    t[w::PG_DYN_N] = 4;
    t[w::KNOBS] = 8;
    t[w::SLOT_BASE] = SLOT_BASE;
    t
}

fn core() -> FakeCore {
    let mut c = FakeCore::new();
    c.dynreg_table = table();
    c.first = P_DYN0;
    c
}

fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

/// A parameter record: its index in the file, template, slot, range and default, with the long and the short name.
fn param(index: u32, slot: u32, min: i32, max: i32, def: u32, name: &str) -> (u32, Vec<u8>) {
    let mut b = std::vec![0u8; LEN_PARAM as usize - 8];
    put(&mut b, 0, index);
    put(&mut b, 4, 7);
    put(&mut b, 8, slot);
    put(&mut b, 12, min as u32);
    put(&mut b, 16, max as u32);
    put(&mut b, 20, def);
    b[24..24 + name.len()].copy_from_slice(name.as_bytes());
    b[56..56 + 2].copy_from_slice(b"PR");
    (KIND_PARAM, b)
}

/// An action record: the parameter (index in the file) whose click runs the callback at image offset `off`.
fn action(pi: u32, off: u32) -> (u32, Vec<u8>) {
    let mut b = std::vec![0u8; LEN_ACTION as usize - 8];
    put(&mut b, 0, pi);
    put(&mut b, 4, off);
    (KIND_ACTION, b)
}

/// A native page record (version 1) of machine key `key` with callbacks at image offsets 2 and 4 and one value binding.
fn native(key: u32) -> (u32, Vec<u8>) {
    let mut b = std::vec![0u8; LEN_NATIVE_PAGE as usize - 8];
    put(&mut b, 0, 1);
    put(&mut b, 4, key);
    put(&mut b, 8, 2);
    put(&mut b, 12, 4);
    b[16..20].copy_from_slice(b"PAGE");
    put(&mut b, 40, 2); // binding 0: a literal value (kind 2) at position 0
    put(&mut b, 44, 9);
    (KIND_NATIVE_PAGE, b)
}

/// A `register` section: the header words, then each record as kind, whole length and body.
fn section(records: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut s = std::vec![0u8; 16];
    put(&mut s, 12, records.len() as u32);
    for (kind, body) in records {
        let mut r = std::vec![0u8; 8];
        put(&mut r, 0, *kind);
        put(&mut r, 4, body.len() as u32 + 8);
        s.extend_from_slice(&r);
        s.extend_from_slice(body);
    }
    s
}

fn check_ctx<'a>(reg: &'a [u8], inf: &'a [u32]) -> CheckCtx {
    CheckCtx {
        reg: reg.as_ptr(),
        image_size: 0x100,
        inf: inf.as_ptr(),
    }
}

fn run_check(core: &FakeCore, step: u32, reg: &[u8]) -> i32 {
    let inf = table();
    unsafe { <Machines as Service<FakeCore>>::check(core, step, &check_ctx(reg, &inf)) }
}

fn apply_ctx<'a>(reg: &'a [u8], inf: &'a [u32]) -> ApplyCtx<'a> {
    ApplyCtx {
        owner: OWNER,
        reg: reg.as_ptr(),
        base: BASE,
        inf: inf.as_ptr(),
        args: None,
    }
}

/// Press and release of encoder `enc` on `VIEW`, with the page answering that `code` is under it.
fn click(core: &FakeCore, code: i32, track: u32, enc: u32) -> slots::Pending {
    let fw = FakeFw {
        param_code: code,
        track,
        ..FakeFw::default()
    };
    let event = |flags: u32| [0u32, 0, 0, 0, flags]; // the key code at +0xc, the flags at +0x10
    let mut press = event(slots::F_PRESS);
    let mut release = event(slots::F_RELEASE);
    press[3] = slots::FIRST_CODE + enc;
    release[3] = slots::FIRST_CODE + enc;
    unsafe {
        // the view's track reference word (`view + 0x74`) is read before the page is asked: a block of words stands in for the view
        let view = [0u32; 0x40];
        let v = view.as_ptr() as *const u8;
        slots::page_key(core, &fw, v, press.as_ptr() as *const u8);
        slots::page_key(core, &fw, v, release.as_ptr() as *const u8);
        slots::pending()
    }
}

#[test]
fn the_records_of_a_good_section_pass_every_check_step() {
    let core = core();
    let reg = section(&[
        param(0, SLOT_BASE, 0, 255, 0, "LEVEL"),
        param(1, SLOT_BASE + 1, -64, 63, 0, "PAN"),
        action(1, 8),
    ]);
    for step in [
        check_step::COUNTS,
        check_step::ACTIONS,
        check_step::NATIVE,
        check_step::DEVICE,
        check_step::RECORDS,
        check_step::OWNERS,
        check_step::ROWS,
    ] {
        assert_eq!(run_check(&core, step, &reg), OK, "step {step}");
    }
}

#[test]
fn a_bad_action_a_bad_parameter_or_a_page_without_page_rows_is_refused() {
    let core = core();
    // an action for a parameter the file does not have, and two actions for one parameter
    let reg = section(&[param(0, SLOT_BASE, 0, 255, 0, "LEVEL"), action(1, 8)]);
    assert_eq!(run_check(&core, check_step::ACTIONS, &reg), E_FORMAT);
    let reg = section(&[
        param(0, SLOT_BASE, 0, 255, 0, "LEVEL"),
        action(0, 8),
        action(0, 12),
    ]);
    assert_eq!(run_check(&core, check_step::ACTIONS, &reg), E_FORMAT);
    // a callback outside the image
    let reg = section(&[param(0, SLOT_BASE, 0, 255, 0, "LEVEL"), action(0, 0x100)]);
    assert_eq!(run_check(&core, check_step::ACTIONS, &reg), E_FORMAT);
    // a slot below the first knob slot, a range upside down, an index out of order
    for bad in [
        param(0, SLOT_BASE - 1, 0, 255, 0, "LEVEL"),
        param(0, SLOT_BASE, 9, 1, 0, "LEVEL"),
        param(2, SLOT_BASE, 0, 1, 0, "LEVEL"),
    ] {
        assert_eq!(
            run_check(&core, check_step::RECORDS, &section(&[bad])),
            E_FORMAT
        );
    }
    // pages need a device that has page rows (the table has none)
    let reg = section(&[(KIND_PAGE, std::vec![0u8; LEN_PAGE as usize - 8])]);
    assert_eq!(run_check(&core, check_step::DEVICE, &reg), E_FORMAT);
}

#[test]
fn fits_refuses_a_table_with_more_rows_than_the_state_has() {
    let mut inf = table();
    assert!(unsafe { <Machines as Service<FakeCore>>::fits(inf.as_ptr()) });
    inf[w::P_DYN_N] = limits::PARAM_ROWS as u32 + 1;
    assert!(!unsafe { <Machines as Service<FakeCore>>::fits(inf.as_ptr()) });
}

#[test]
fn apply_fills_the_parameter_rows_and_arms_the_click_and_remove_gives_them_back() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut core = core();
    let inf = table();
    let reg = section(&[
        param(0, SLOT_BASE, 0, 255, 17, "LEVEL"),
        param(1, SLOT_BASE + 1, -64, 63, 0, "PAN"),
        action(1, 8),
    ]);
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::apply(&core, &apply_ctx(&reg, &inf)) },
        OK
    );
    // the parameters sit in the first free rows, with their ranges and the owner
    assert_eq!(
        core.calls(),
        [
            Call::ParamRow {
                code: P_DYN0,
                min: 0,
                max: 255,
                def: 17
            },
            Call::ParamRow {
                code: P_DYN0 + 1,
                min: -64i32 as u32,
                max: 63,
                def: 0
            },
        ]
    );
    assert_eq!(&core.reg_state().param_owner[..3], [OWNER, OWNER, 0]);
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::cap(&core, dkrt_sdk::proto::CAPK_PARAM_ROWS) },
        Some((16, 2))
    );
    // the first parameter has no action: the stock click, nothing is queued
    core.owners = std::vec![OWNER, OWNER, 0];
    assert_eq!(click(&core, P_DYN0 as i32, 6, 1).callback, 0);
    // a click on the second parameter queues the callback at the module's load address plus its image offset
    let p = click(&core, (P_DYN0 + 1) as i32, 6, 0);
    assert_eq!((p.callback, p.track, p.code), (BASE + 8, 6, P_DYN0 + 1));
    assert_eq!(
        slots::click_context(&core, &p).map(|c| c.owner),
        Some(OWNER)
    );
    // remove: the rows are free, the action is disarmed and the queued click of the unloaded module is dropped
    core.calls.borrow_mut().clear();
    unsafe { <Machines as Service<FakeCore>>::remove(&core, OWNER) };
    assert_eq!(&core.reg_state().param_owner[..3], [0, 0, 0]);
    assert_eq!(
        core.calls().len(),
        2,
        "both parameter rows are written back as gap rows"
    );
    assert_eq!(unsafe { slots::pending() }.callback, 0);
    let p = click(&core, (P_DYN0 + 1) as i32, 6, 2);
    assert_eq!(p.callback, 0, "no action is armed any more");
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::cap(&core, dkrt_sdk::proto::CAPK_PARAM_ROWS) },
        Some((16, 0))
    );
}

#[test]
fn a_native_page_record_registers_its_callbacks_through_the_core_and_other_kinds_do_nothing() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let core = core();
    let inf = table();
    let (kind, body) = native(0x2a);
    let reg = section(&[(kind, body.clone())]);
    let ctx = apply_ctx(&reg, &inf);
    unsafe {
        assert_eq!(
            <Machines as Service<FakeCore>>::apply_record(&core, KIND_PARAM, body.as_ptr(), &ctx),
            OK
        );
        assert!(
            core.calls().is_empty(),
            "a kind of another service or a parameter record writes nothing"
        );
        assert_eq!(
            <Machines as Service<FakeCore>>::apply_record(
                &core,
                KIND_NATIVE_PAGE,
                body.as_ptr(),
                &ctx
            ),
            OK
        );
    }
    assert_eq!(
        core.calls(),
        [Call::NativeRegister {
            kind: KIND_NATIVE_PAGE,
            owner: 0x2a,
            page: 1,
            flags: 0,
            draw: BASE + 2,
            event: BASE + 4,
            words: Vec::new(),
            arg: 0,
            with_arg: 0
        }]
    );
    // a refusal of the firmware adapter is the record's status
    let mut core = core;
    core.native_status = E_NO_REGISTRY;
    assert_eq!(
        unsafe {
            <Machines as Service<FakeCore>>::apply_record(
                &core,
                KIND_NATIVE_PAGE,
                body.as_ptr(),
                &ctx,
            )
        },
        E_NO_REGISTRY
    );
    // and a firmware without the native page calls refuses the record when it is checked
    core.native_available = false;
    assert_eq!(
        run_check(&core, check_step::NATIVE, &section(&[(kind, body)])),
        E_NO_REGISTRY
    );
}

#[test]
fn the_capacity_counts_need_a_registry_and_the_ui_pass_runs_the_click_last() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut core = FakeCore::new();
    for kind in [
        dkrt_sdk::proto::CAPK_MACHINE_ROWS,
        dkrt_sdk::proto::CAPK_PARAM_ROWS,
        dkrt_sdk::proto::CAPK_PAGE_ROWS,
    ] {
        assert_eq!(
            unsafe { <Machines as Service<FakeCore>>::cap(&core, kind) },
            Some((0, 0)),
            "no registry"
        );
    }
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::cap(&core, 0xff) },
        None,
        "a kind another service counts"
    );
    core.dynreg_table = table();
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::cap(&core, dkrt_sdk::proto::CAPK_MACHINE_ROWS) },
        Some((8, 0))
    );
    assert_eq!(
        unsafe { <Machines as Service<FakeCore>>::cap(&core, dkrt_sdk::proto::CAPK_PAGE_ROWS) },
        Some((4, 0))
    );
    // no click is queued: the pass in any phase runs nothing
    unsafe { <Machines as Service<FakeCore>>::tick(&core, tick_phase::LAST) };
}

#[test]
fn the_machine_types_of_a_module_are_its_machine_rows_in_type_order() {
    let core = core();
    let s = core.reg_state();
    s.mach_owner[1] = 4;
    s.mach_owner[5] = 4;
    s.mach_owner[2] = 9;
    let mut out = [0u8; limits::MACHINE_ROWS];
    let first = dkrt_sdk::device::DYN_TYPE_FIRST as u8;
    assert_eq!(types_of(&core, 4, &mut out), 2);
    assert_eq!(&out[..2], [first + 1, first + 5]);
    assert_eq!(types_of(&core, 7, &mut out), 0);
}

#[test]
fn the_machine_types_are_a_row_method_of_the_service() {
    let core = core();
    core.reg_state().mach_owner[3] = 2;
    let mut out = [0u8; 8];
    let n = unsafe { <Machines as Service<FakeCore>>::machine_types_of(&core, 2, &mut out) };
    assert_eq!(n, 1);
    assert_eq!(out[0], dkrt_sdk::device::DYN_TYPE_FIRST as u8 + 3);
    assert_eq!(unsafe { <Machines as Service<FakeCore>>::machine_types_of(&core, 5, &mut out) }, 0);
}
