//! Host tests of the `__NAME__` service over `FakeCore`: what it declares and that it asks nothing of the core when it has nothing to do. Add a case
//! for each handler the service overrides (check, apply_record, remove, tick, ...); the other services' `tests/` show the pattern.
use dkrt_abi::service::{tick_phase, Declare, Service};
use dkrt_svc___NAME__::records::__CAMEL__;
use dkrt_test_support::FakeCore;

#[test]
fn the_service_declares_what_the_manifest_gives_it() {
    let mut d = Declare::new();
    unsafe { <__CAMEL__ as Service<FakeCore>>::declare(&mut d) };
    assert_eq!(d.n, 0, "no record kind yet: add one `d.kind` and one [records.kinds] entry together");
}

#[test]
fn a_tick_and_a_removal_ask_nothing_of_the_core() {
    let core = FakeCore::new();
    unsafe {
        <__CAMEL__ as Service<FakeCore>>::tick(&core, tick_phase::EARLY);
        <__CAMEL__ as Service<FakeCore>>::remove(&core, 1);
    }
    assert!(core.calls().is_empty());
}
