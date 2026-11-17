//! `FakeService`: a `Service` that records what the core calls (record kind, owner, bytes of `check`, `apply` and `remove`; the
//! declarations, attributions and `on_loaded`), and host tests of the core's walkers over it (docs/ARCHITECTURE.md "Boundaries and tests").
use super::*;
use dkrt_sdk::*;
use crate::registry::{apply_with, check_with, each, remove_with, DKRT_REG};
use dkrt_sdk::proto::CAPK_FX_SLOTS;
use crate::core_api::{CoreApi, RealCore, REAL_CORE};
use dkrt_abi::service::ServiceRow;
use std::vec::Vec;

/// Serializes the tests that use the fake's log and the registry's state.
pub(crate) use crate::test_lock::LOCK;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Check { svc: u8, step: u32, kind: u32, body: Vec<u8> },
    /// The rows a service writes before the per-record pass.
    Bulk { svc: u8, owner: u8 },
    Apply { svc: u8, owner: u8, kind: u32, body: Vec<u8> },
    Remove { svc: u8, owner: u8 },
    Attribute { svc: u8, site: u32, id: u32 },
    DeclareVoice { svc: u8, rec: usize, ty: u32 },
    OnLoaded { svc: u8, rec: usize },
    Tick { svc: u8, phase: u32 },
}

static mut LOG: Vec<Call> = Vec::new();
/// `(svc, kind)`: the service that refuses a record of that kind in `check`.
static mut REFUSE: (u8, u32) = (0, 0);
/// The service whose `apply` fails.
static mut FAIL_APPLY: (u8, u32) = (0, 0);

pub(crate) fn log() -> &'static mut Vec<Call> {
    unsafe { &mut *core::ptr::addr_of_mut!(LOG) }
}

/// The fake service number `N` owns record kind `100 + N`, a record of `8 + 4 * N` bytes.
pub(crate) struct Fake<const N: u8>;

pub(crate) const fn kind_of(n: u8) -> u32 {
    100 + n as u32
}

unsafe fn body(reg: *const u8, kind: u32, b: *const u8) -> Vec<u8> {
    let _ = (reg, kind);
    core::slice::from_raw_parts(b, 4 * (kind as usize - 100)).to_vec()
}

impl<const N: u8, C: CoreApi> Service<C> for Fake<N> {
    /// Service 1 checks its records at the step of native pages, service 2 at the step of effects: the order of the old body.
    unsafe fn check(_: &C, step: u32, c: &CheckCtx) -> i32 {
        let mine = if N == 1 { check_step::NATIVE } else { check_step::EFFECTS };
        let mut r = OK;
        if step != mine {
            return r;
        }
        each(c.reg, |kind, b| {
            if kind == kind_of(N) && r == OK {
                log().push(Call::Check { svc: N, step, kind, body: body(c.reg, kind, b) });
                if REFUSE == (N, kind) {
                    r = E_FORMAT;
                }
            }
        });
        r
    }

    unsafe fn apply(_: &C, c: &ApplyCtx) -> i32 {
        log().push(Call::Bulk { svc: N, owner: c.owner });
        OK
    }

    unsafe fn apply_record(_: &C, kind: u32, b: *const u8, c: &ApplyCtx) -> i32 {
        if kind != kind_of(N) {
            return OK;
        }
        log().push(Call::Apply { svc: N, owner: c.owner, kind, body: body(c.reg, kind, b) });
        if FAIL_APPLY == (N, kind) {
            E_FORMAT
        } else {
            OK
        }
    }

    unsafe fn remove(_: &C, owner: u8) {
        log().push(Call::Remove { svc: N, owner });
    }

    unsafe fn cap(_: &C, kind: u8) -> Option<(u32, u32)> {
        (kind == CAPK_FX_SLOTS).then_some((10 * N as u32, N as u32))
    }

    unsafe fn attribute(_: &C, site: u32, id: u32) -> Option<usize> {
        log().push(Call::Attribute { svc: N, site, id });
        if site == N as u32 {
            Some(id as usize + 100 * N as usize)
        } else {
            None
        }
    }

    unsafe fn on_loaded(_: &C, rec: usize) {
        log().push(Call::OnLoaded { svc: N, rec });
    }

    unsafe fn tick(_: &C, phase: u32) {
        log().push(Call::Tick { svc: N, phase });
    }

    /// Service 2 lists two effects (rows 7 and 9); service 1 has none (the default).
    unsafe fn fx_entry(_: &C, index: u32, out: &mut DkrtFx) -> i32 {
        if N == 2 && index < 2 {
            out.row = 7 + 2 * index;
            OK
        } else {
            E_NOT_FOUND
        }
    }

    unsafe fn declare_voice(_: &C, rec: usize, ty: u32, _: [u32; 8], _: u32) -> Option<i32> {
        log().push(Call::DeclareVoice { svc: N, rec, ty });
        if ty == N as u32 {
            Some(OK)
        } else {
            None
        }
    }
}

/// Fake service `N` owns record kind `100 + N`, a record of `8 + 4 * N` bytes.
static KINDS_1: [(u32, u32); 1] = [(101, 12)];
static KINDS_2: [(u32, u32); 1] = [(102, 16)];
static TWO_ROWS: [ServiceRow; 2] = [ServiceRow::of::<Fake<1>, crate::core_api::RealCore>("fake1", &KINDS_1), ServiceRow::of::<Fake<2>, crate::core_api::RealCore>("fake2", &KINDS_2)];
static TWO_ROWS_P: dkrt_abi::service::RowPtrs<2> = dkrt_abi::service::RowPtrs::of(&TWO_ROWS);
pub(crate) const TWO: ServiceTable = ServiceTable::new(&TWO_ROWS_P.0, &REAL_CORE);

/// The words of a `dynreg_info` that the walker reads: the magic and the device.
static TABLE: [u32; 100] = {
    let mut t = [0u32; 100];
    t[0] = 0x4459_4e52;
    t[2] = 1;
    t
};

pub(crate) fn dynreg_table() -> *const u32 {
    TABLE.as_ptr()
}

/// A `register` section of the given records `(kind, 4 * (kind - 100) body bytes filled with the record number)`.
fn section(kinds: &[u32]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0x444b_5247u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes()); // the device
    v.extend_from_slice(&(kinds.len() as u32).to_be_bytes());
    for (i, &k) in kinds.iter().enumerate() {
        let n = 4 * (k as usize - 100);
        v.extend_from_slice(&k.to_be_bytes());
        v.extend_from_slice(&(8 + n as u32).to_be_bytes());
        v.extend(core::iter::repeat(i as u8 + 1).take(n));
    }
    v
}

unsafe fn reset() {
    log().clear();
    REFUSE = (0, 0);
    FAIL_APPLY = (0, 0);
    (*core::ptr::addr_of_mut!(DKRT_REG)).registers = 0;
    (*core::ptr::addr_of_mut!(DKRT_REG)).removes = 0;
}

fn rec(svc: u8, i: u8) -> (u32, Vec<u8>) {
    (kind_of(svc), vec![i; 4 * svc as usize])
}

#[test]
fn a_mixed_file_checks_in_step_order_applies_in_file_order_and_removes_in_reverse() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset();
        // mixed kinds: service 1, 2, 1
        let sec = section(&[kind_of(1), kind_of(2), kind_of(1)]);
        assert_eq!(check_with(TWO, sec.as_ptr(), sec.len() as u32, 0), OK);
        assert_eq!(apply_with(TWO, 5, sec.as_ptr(), sec.len() as u32, 0x1000), OK);
        remove_with(TWO, 5);
        let (k1, b1) = rec(1, 1);
        let (k1b, b1b) = rec(1, 3);
        let (k2, b2) = rec(2, 2);
        let n = check_step::NATIVE;
        let e = check_step::EFFECTS;
        assert_eq!(
            *log(),
            vec![
                // check: step by step, so service 1's step (native pages) before service 2's (effects), its records in file order
                Call::Check { svc: 1, step: n, kind: k1, body: b1.clone() },
                Call::Check { svc: 1, step: n, kind: k1b, body: b1b.clone() },
                Call::Check { svc: 2, step: e, kind: k2, body: b2.clone() },
                // apply: every service's rows first, then each record in file order across the kinds
                Call::Bulk { svc: 1, owner: 5 },
                Call::Bulk { svc: 2, owner: 5 },
                Call::Apply { svc: 1, owner: 5, kind: k1, body: b1 },
                Call::Apply { svc: 2, owner: 5, kind: k2, body: b2 },
                Call::Apply { svc: 1, owner: 5, kind: k1b, body: b1b },
                // remove: the last service first
                Call::Remove { svc: 2, owner: 5 },
                Call::Remove { svc: 1, owner: 5 },
            ]
        );
        let r = &*core::ptr::addr_of!(DKRT_REG);
        assert_eq!((r.registers, r.removes), (1, 1));
    }
}

#[test]
fn the_first_fault_is_the_first_step_that_finds_one_and_a_failed_apply_stops_the_file() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset();
        // an effect-step fault precedes nothing: the native step runs first, as in the old body, wherever the records sit in the file
        let sec = section(&[kind_of(2), kind_of(1), kind_of(2)]);
        REFUSE = (2, kind_of(2));
        assert_eq!(check_with(TWO, sec.as_ptr(), sec.len() as u32, 0), E_FORMAT);
        assert_eq!(log().len(), 2); // service 1's record, then service 2's first (refused) record; the second is not looked at
        assert!(matches!(log()[0], Call::Check { svc: 1, .. }));
        reset();
        REFUSE = (1, kind_of(1));
        assert_eq!(check_with(TWO, sec.as_ptr(), sec.len() as u32, 0), E_FORMAT);
        assert_eq!(log().len(), 1, "service 1 refused at its step, so service 2's step never ran");

        // apply stops at the first failing record in file order; later records of any kind are not applied
        reset();
        FAIL_APPLY = (1, kind_of(1));
        assert_eq!(apply_with(TWO, 5, sec.as_ptr(), sec.len() as u32, 0), E_FORMAT);
        let applied: Vec<_> = log().iter().filter(|c| matches!(c, Call::Apply { .. })).collect();
        assert_eq!(applied.len(), 2, "the effect record before the failing one was applied, the one after was not");
        assert_eq!((*core::ptr::addr_of!(DKRT_REG)).registers, 0, "a failed apply is not a registration");
    }
}

#[test]
fn the_walker_refuses_a_kind_no_service_declared_and_a_wrong_length() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset();
        // kind 99 is no one's
        let mut bad = section(&[kind_of(1)]);
        bad[12..16].copy_from_slice(&2u32.to_be_bytes());
        bad.extend_from_slice(&99u32.to_be_bytes());
        bad.extend_from_slice(&8u32.to_be_bytes());
        assert_eq!(check_with(TWO, bad.as_ptr(), bad.len() as u32, 0), E_FORMAT);
        assert!(log().is_empty());
        let mut wrong = section(&[kind_of(2)]);
        wrong[20..24].copy_from_slice(&12u32.to_be_bytes()); // the record's length word: a kind-1 length
        assert_eq!(check_with(TWO, wrong.as_ptr(), wrong.len() as u32, 0), E_FORMAT);
        assert!(log().is_empty());
    }
}

#[test]
fn capacity_sums_the_declared_counts() {
    unsafe {
        // 10 + 20 slots, 1 + 2 used: two services declare the same kind
        assert_eq!(TWO.cap(CAPK_FX_SLOTS), Some((30, 3)));
        assert_eq!(TWO.cap(CAPK_FX_SLOTS + 1), None);
    }
}

#[test]
fn attribution_asks_the_services_in_order_until_one_answers() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset();
        assert_eq!(TWO.attribute(2, 7), Some(207));
        assert_eq!(
            *log(),
            vec![Call::Attribute { svc: 1, site: 2, id: 7 }, Call::Attribute { svc: 2, site: 2, id: 7 }]
        );
        assert_eq!(TWO.attribute(9, 7), None);
        reset();
        assert_eq!(TWO.declare_voice(0, 1, [0; 8], 0), Some(OK));
        assert_eq!(*log(), vec![Call::DeclareVoice { svc: 1, rec: 0, ty: 1 }]);
    }
}

#[test]
fn a_tick_phase_reaches_every_service_in_list_order() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset();
        TWO.tick(tick_phase::EARLY);
        TWO.tick(tick_phase::LATE);
        assert_eq!(
            *log(),
            vec![
                Call::Tick { svc: 1, phase: tick_phase::EARLY },
                Call::Tick { svc: 2, phase: tick_phase::EARLY },
                Call::Tick { svc: 1, phase: tick_phase::LATE },
                Call::Tick { svc: 2, phase: tick_phase::LATE },
            ]
        );
    }
}

/// A service that declares a kind its table does not carry, one that declares a wrong length, and one that matches.
struct Good;
impl<C: CoreApi> Service<C> for Good {
    unsafe fn declare(d: &mut Declare) {
        d.kind(70, 16);
    }
}
struct WrongLen;
impl<C: CoreApi> Service<C> for WrongLen {
    unsafe fn declare(d: &mut Declare) {
        d.kind(71, 20);
    }
}
struct Extra;
impl<C: CoreApi> Service<C> for Extra {
    unsafe fn declare(d: &mut Declare) {
        d.kind(72, 8);
        d.slot(1, 0, 0);
    }
}
struct Silent;
impl<C: CoreApi> Service<C> for Silent {}

static DECL_ROWS: [ServiceRow; 4] = [
    ServiceRow::of::<Good, crate::core_api::RealCore>("good", &[(70, 16)]),
    ServiceRow::of::<WrongLen, crate::core_api::RealCore>("wronglen", &[(71, 16)]),
    ServiceRow::of::<Extra, crate::core_api::RealCore>("extra", &[(72, 8)]),
    ServiceRow::of::<Silent, crate::core_api::RealCore>("silent", &[(73, 8)]),
];
static DECL_ROWS_P: dkrt_abi::service::RowPtrs<4> = dkrt_abi::service::RowPtrs::of(&DECL_ROWS);

#[test]
fn a_service_whose_declarations_differ_from_its_table_is_failed_and_its_kinds_are_unknown() {
    let t = ServiceTable::new(&DECL_ROWS_P.0, &REAL_CORE);
    let failed = unsafe { t.check_declarations() };
    assert_eq!(failed, 0b1110, "the matching service passes; a wrong length, an extra item and a missing declaration fail");
    let t = ServiceTable { failed, ..t };
    assert_eq!(t.len(70), Some(16));
    for kind in [71, 72, 73] {
        assert_eq!(t.len(kind), None);
        assert!(t.owner(kind).is_none());
    }
}

#[test]
fn the_in_crate_services_declare_what_the_generated_table_carries() {
    // the real rows: samples owns none, machines six kinds, fx one
    assert_eq!(unsafe { crate::gen_services::TABLE.check_declarations() }, 0);
}

mod table {
    use super::*;

    struct A;
    impl<C: CoreApi> Service<C> for A {}
    struct B;
    impl<C: CoreApi> Service<C> for B {
        unsafe fn dsp_clear(_: &C, _: usize) -> bool {
            false
        }
        unsafe fn cap(_: &C, kind: u8) -> Option<(u32, u32)> {
            (kind == 3).then_some((6, 1))
        }
    }

    static ROWS: [ServiceRow; 2] = [ServiceRow::of::<A, crate::core_api::RealCore>("a", &[(1, 8)]), ServiceRow::of::<B, crate::core_api::RealCore>("b", &[(2, 12)])];
    static ROWS_P: dkrt_abi::service::RowPtrs<2> = dkrt_abi::service::RowPtrs::of(&ROWS);
    const T: ServiceTable = ServiceTable::new(&ROWS_P.0, &REAL_CORE);

    #[test]
    fn a_table_asks_each_service_and_ands_the_clears() {
        assert_eq!(T.len(1), Some(8));
        assert_eq!(T.len(2), Some(12));
        assert_eq!(T.len(3), None);
        assert_eq!(T.owner(2).map(|r| r.name()), Some("b"));
        assert!(T.owner(9).is_none());
        unsafe {
            assert!(!T.dsp_clear(0));
            assert!(ServiceTable { rows: &ROWS_P.0[..1], ..T }.dsp_clear(0));
        }
    }

    #[test]
    fn a_kind_two_services_count_is_the_sum_of_their_counts() {
        static TWO_B: [ServiceRow; 2] = [ServiceRow::of::<B, crate::core_api::RealCore>("b1", &[]), ServiceRow::of::<B, crate::core_api::RealCore>("b2", &[])];
        static TWO_B_P: dkrt_abi::service::RowPtrs<2> = dkrt_abi::service::RowPtrs::of(&TWO_B);
        unsafe {
            assert_eq!(ServiceTable { rows: &TWO_B_P.0, ..T }.cap(3), Some((12, 2)));
            assert_eq!(T.cap(3), Some((6, 1)));
            assert_eq!(T.cap(4), None);
        }
    }
}

/// The effect list of DIGIKIT > FX ROUTES is the row method of the first service that has effects: the effects of that service in order, "no fx" past its end,
/// and "no fx" at once when no service in the table has effects (the fx service file is not on the card).
#[test]
fn fx_entry_answers_from_the_service_that_has_effects_and_no_fx_without_one() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut out: DkrtFx = core::mem::zeroed();
        assert_eq!(TWO.fx_entry(0, &mut out), OK);
        assert_eq!(out.row, 7);
        assert_eq!(TWO.fx_entry(1, &mut out), OK);
        assert_eq!(out.row, 9);
        assert_eq!(TWO.fx_entry(2, &mut out), E_NOT_FOUND, "past the end");
        // a table of services without effects, and an empty table
        static ONE_ROW: [ServiceRow; 1] = [ServiceRow::of::<Fake<1>, RealCore>("fake1", &KINDS_1)];
        static ONE_ROW_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ONE_ROW);
        let one = ServiceTable::new(&ONE_ROW_P.0, &REAL_CORE);
        assert_eq!(one.fx_entry(0, &mut out), E_NOT_FOUND);
        let none = ServiceTable::new(&[], &REAL_CORE);
        assert_eq!(none.fx_entry(0, &mut out), E_NOT_FOUND);
    }
}
