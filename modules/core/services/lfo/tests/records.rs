//! Host tests of the `lfo` service over `FakeCore`: what it declares, how its handlers check, apply and remove a `lfo_shape` record, and that it ignores a
//! record of another service. The statics of the crate (the shape table) are shared, so each test takes `LOCK` and starts from an empty table.
//! The runtime's own tests (`handlers/lfo_tests.rs` in the runtime crate) drive the same service through the collector, the registry walkers and the planner.
use dkrt_abi::service::{check_step, ApplyCtx, CheckCtx, Declare, Service};
use dkrt_sdk::records::{KIND_LFO_SHAPE, LEN_LFO_SHAPE};
use dkrt_sdk::*;
use dkrt_svc_lfo::records::{owner_of, shape, used, Lfo, MAX_SHAPES, SAMPLES};
use dkrt_test_support::FakeCore;
use std::sync::{Mutex, MutexGuard};
use std::vec::Vec;

static LOCK: Mutex<()> = Mutex::new(());

/// The lock, with an empty shape table.
fn fresh() -> (MutexGuard<'static, ()>, FakeCore) {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let core = FakeCore::new();
    for owner in 0..=8u8 {
        unsafe { <Lfo as Service<FakeCore>>::remove(&core, owner) };
    }
    assert_eq!(used(), 0);
    (g, core)
}

/// The body of a shape record: the id, then the 32 samples (a ramp from `from`), big-endian.
fn body(id: u32, from: i16) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    for s in 0..SAMPLES as i16 {
        b.extend_from_slice(&(from + 100 * s).to_be_bytes());
    }
    b
}

/// A `register` section of shape records: the 16-byte header (the count at word 3), then each record's kind, length and body.
fn section(shapes: &[(u32, i16)]) -> Vec<u8> {
    let mut s = vec![0u8; 16];
    s[12..16].copy_from_slice(&(shapes.len() as u32).to_be_bytes());
    for (id, from) in shapes {
        s.extend_from_slice(&KIND_LFO_SHAPE.to_be_bytes());
        s.extend_from_slice(&LEN_LFO_SHAPE.to_be_bytes());
        s.extend_from_slice(&body(*id, *from));
    }
    s
}

fn check(core: &FakeCore, step: u32, sec: &[u8]) -> i32 {
    let c = CheckCtx {
        reg: sec.as_ptr(),
        image_size: 0x1000,
        inf: core::ptr::null(),
    };
    unsafe { <Lfo as Service<FakeCore>>::check(core, step, &c) }
}

fn apply(core: &FakeCore, owner: u8, kind: u32, body: &[u8]) -> i32 {
    let c = ApplyCtx {
        owner,
        reg: core::ptr::null(),
        base: 0,
        inf: core::ptr::null(),
        args: None,
    };
    unsafe { <Lfo as Service<FakeCore>>::apply_record(core, kind, body.as_ptr(), &c) }
}

#[test]
fn the_service_declares_its_one_kind_and_nothing_else() {
    let mut d = Declare::new();
    unsafe { <Lfo as Service<FakeCore>>::declare(&mut d) };
    let mut want = Declare::new();
    want.kind(KIND_LFO_SHAPE, LEN_LFO_SHAPE);
    assert!(d.same_as(&want));
}

#[test]
fn a_shape_goes_through_check_apply_and_remove() {
    let (_g, core) = fresh();
    let sec = section(&[(7, -1000)]);
    assert_eq!(check(&core, check_step::RECORDS, &sec), OK);
    assert_eq!(check(&core, check_step::ROWS, &sec), OK);
    assert_eq!(apply(&core, 1, KIND_LFO_SHAPE, &body(7, -1000)), OK);
    let s = shape(7).expect("apply_record kept the shape");
    assert_eq!((s[0], s[1], s[31]), (-1000, -900, 2100));
    assert_eq!((owner_of(7), used()), (1, 1));
    unsafe { <Lfo as Service<FakeCore>>::remove(&core, 2) };
    assert_eq!(used(), 1, "another module's removal keeps the shape");
    unsafe { <Lfo as Service<FakeCore>>::remove(&core, 1) };
    assert_eq!((shape(7), owner_of(7), used()), (None, 0, 0));
    assert!(
        core.calls().is_empty(),
        "the service asks nothing of the core"
    );
}

#[test]
fn an_id_of_zero_an_id_twice_and_an_id_that_a_module_keeps_are_refused() {
    let (_g, core) = fresh();
    assert_eq!(
        check(&core, check_step::RECORDS, &section(&[(0, 0)])),
        E_FORMAT
    );
    assert_eq!(
        check(&core, check_step::RECORDS, &section(&[(9, 0), (9, 1)])),
        E_TYPE_USED
    );
    assert_eq!(
        check(&core, check_step::RECORDS, &section(&[(9, 0), (10, 1)])),
        OK
    );
    assert_eq!(apply(&core, 1, KIND_LFO_SHAPE, &body(7, 0)), OK);
    assert_eq!(
        check(&core, check_step::ROWS, &section(&[(7, 0)])),
        E_TYPE_USED
    );
    assert_eq!(check(&core, check_step::ROWS, &section(&[(8, 0)])), OK);
}

#[test]
fn a_full_table_refuses_the_file_at_the_check_and_the_record_at_apply() {
    let (_g, core) = fresh();
    let many: Vec<(u32, i16)> = (100..100 + MAX_SHAPES as u32 + 1).map(|i| (i, 0)).collect();
    assert_eq!(
        check(&core, check_step::ROWS, &section(&many)),
        E_TABLE_FULL
    );
    for i in 0..MAX_SHAPES as u32 {
        assert_eq!(apply(&core, 1, KIND_LFO_SHAPE, &body(100 + i, 0)), OK);
    }
    assert_eq!(apply(&core, 1, KIND_LFO_SHAPE, &body(999, 0)), E_TABLE_FULL);
    assert_eq!(used(), MAX_SHAPES);
}

#[test]
fn a_record_of_another_service_and_the_other_check_steps_are_not_its_business() {
    let (_g, core) = fresh();
    assert_eq!(apply(&core, 1, 999, &body(7, 0)), OK);
    assert_eq!(used(), 0);
    let dup = section(&[(9, 0), (9, 1)]);
    assert_eq!(check(&core, check_step::COUNTS, &dup), OK);
    assert_eq!(check(&core, check_step::NATIVE, &dup), OK);
}
