//! Host tests of the `lfo` service through the runtime (the crate `dkrt-svc-lfo` has its own over `FakeCore`): the proof that a kind the registry, the collector, the loader, dkneeds and patchkit do not name works end to end
//! (R7): a fixture's record goes through `dk_record_add`'s collector, the `register` section, the registry walkers and the planner.
use super::exports::{owner_of, shape, used, Lfo, MAX_SHAPES, SAMPLES};
use crate::collect::{self, Collect, KeysCopy, Plan, Profile};
use super::tests::LOCK;
use crate::plan::{self, Env};
use crate::registry;
use dkneeds::keys::{write_table, KeyGroup, Keys, Table};
use crate::core_api::RealCore;
use crate::svc_rows;
use dkrt_abi::plan::{Reason, Verdict};
use dkrt_abi::service::ServiceRow;
use dkrt_sdk::proto::*;
use dkrt_sdk::records::{KIND_LFO_SHAPE, KINDS_LFO, LEN_LFO_SHAPE};
use dkrt_sdk::*;
use std::vec;
use std::vec::Vec;

const DT2: Profile = Profile {
    device: 1,
    pages: false,
    slot_base: 0,
    clone_of: 0,
    level: 10,
    own_dsp: false,
    tune_code: 0xca,
    tune_slot: 0,
    level_code: 0xd1,
    level_knob: 7,
    nslots: 6,
    slots: [1, 2, 4, 5, 6, 7, 0, 0],
    tpl: [0xcc, 0xd1, 0x47, 0x32, 0xca, 0xcb],
};

/// The body of a shape record: the id, then the 32 samples (a ramp from `from`), big-endian.
fn body(id: u32, from: i16) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    for s in 0..SAMPLES as i16 {
        b.extend_from_slice(&(from + 100 * s).to_be_bytes());
    }
    b
}

/// What a fixture module's `init` does: `dk_record_add` for each shape; then the commit lowers the collection to a `register` section.
fn section(shapes: &[(u32, i16)]) -> Vec<u8> {
    let mut c = Collect::new();
    for (id, from) in shapes {
        let b = body(*id, *from);
        assert_eq!(c.add_record(KIND_LFO_SHAPE, b.as_ptr(), b.len() as u32), OK);
    }
    let keys = KeysCopy::empty();
    let mut plan = Plan::new();
    let size = collect::lower(&c, &DT2, &keys, &[false; limits::MACHINE_ROWS], 0x1000, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = vec![0u8; size];
    collect::lower(&c, &DT2, &keys, &[false; limits::MACHINE_ROWS], 0x1000, buf.as_mut_ptr(), size, &mut plan).unwrap();
    buf
}

/// The lfo service as the runtime has it: a service file (`lfo.dks`), not a row of the image. Its row joins the table as `Registry::add` does at the commit of the
/// loaded file, and leaves it as `Registry::remove` does at the unload.
struct Loaded;

/// The module record of the service file.
const FILE_REC: usize = 0;

impl Loaded {
    fn new() -> Loaded {
        static ROW: ServiceRow = ServiceRow::of::<Lfo, RealCore>("lfo", &KINDS_LFO);
        assert_eq!(unsafe { svc_rows::add(FILE_REC, &ROW) }, OK);
        Loaded
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        unsafe { svc_rows::remove(FILE_REC) };
    }
}

fn clear() {
    for owner in 1..=crate::MAX_MODULES as u8 {
        unsafe { registry::remove(owner) };
    }
}

#[test]
fn a_shape_record_goes_from_the_builder_call_to_the_table_and_back() {
    let _g = LOCK.lock().unwrap();
    let _l = Loaded::new();
    clear();
    let sec = section(&[(7, -1000)]);
    // the section is the header and one record of the kind's length
    assert_eq!(sec.len(), 16 + LEN_LFO_SHAPE as usize);
    unsafe {
        assert_eq!(registry::check(sec.as_ptr(), sec.len() as u32, 0), OK);
        assert_eq!(registry::apply(1, sec.as_ptr(), sec.len() as u32, 0x1000), OK);
    }
    let s = shape(7).expect("the walker applied the record");
    assert_eq!((s[0], s[1], s[31]), (-1000, -900, -1000 + 3100));
    assert_eq!((owner_of(7), used()), (1, 1));
    unsafe { registry::remove(1) };
    assert_eq!((shape(7), owner_of(7), used()), (None, 0, 0), "the walker removed what the module kept");
}

#[test]
fn a_second_owner_of_a_shape_id_and_an_id_twice_or_zero_are_refused() {
    let _g = LOCK.lock().unwrap();
    let _l = Loaded::new();
    clear();
    let a = section(&[(7, 0)]);
    unsafe {
        assert_eq!(registry::check(a.as_ptr(), a.len() as u32, 0), OK);
        assert_eq!(registry::apply(1, a.as_ptr(), a.len() as u32, 0x1000), OK);
        // another module with the same id is refused at the check, and what the first kept is unchanged
        assert_eq!(registry::check(a.as_ptr(), a.len() as u32, 0), E_TYPE_USED);
        let other = section(&[(8, 5)]);
        assert_eq!(registry::check(other.as_ptr(), other.len() as u32, 0), OK);
        // an id twice in one file, and the id 0
        let twice = section(&[(9, 0), (9, 1)]);
        assert_eq!(registry::check(twice.as_ptr(), twice.len() as u32, 0), E_TYPE_USED);
        let zero = section(&[(0, 0)]);
        assert_eq!(registry::check(zero.as_ptr(), zero.len() as u32, 0), E_FORMAT);
        // a table that cannot take the file's shapes
        let many: Vec<(u32, i16)> = (100..100 + MAX_SHAPES as u32).map(|i| (i, 0)).collect();
        let big = section(&many[..MAX_SHAPES]);
        assert_eq!(registry::check(big.as_ptr(), big.len() as u32, 0), E_TABLE_FULL);
        registry::remove(1);
    }
    assert_eq!(used(), 0);
}

#[test]
fn a_record_of_a_kind_that_is_not_generic_or_has_the_wrong_length_never_reaches_the_service() {
    let b = body(1, 0);
    let mut c = Collect::new();
    assert_eq!(c.add_record(KIND_LFO_SHAPE, b.as_ptr(), b.len() as u32 - 4), E_FORMAT);
    assert_eq!(c.ng, 0);
}

fn table(keys: &[(u32, &[u32])]) -> Vec<u8> {
    let groups: Vec<KeyGroup> = keys.iter().map(|(kind, ks)| KeyGroup { kind: *kind, keys: ks }).collect();
    let t = Table { keys: &groups, ..Table::default() };
    let mut b = vec![0u8; dkneeds::keys::table_size(&t)];
    write_table(&t, &mut b).unwrap();
    b
}

fn plan_of(tables: &[Vec<u8>]) -> plan::Plan {
    let mut caps = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let c = |kind: u8, limit: u32| DkrtCap { kind: kind as u32, limit, used: 0, largest: limit };
    caps[0] = c(CAPK_MODULES, 8);
    caps[1] = c(CAPK_SELECTION, 8);
    let env = Env { services: crate::handlers::services(), runtime: dkrt_sdk::RUNTIME_VERSION, caps: &caps[..2] };
    let keys: Vec<Option<Keys>> = tables.iter().map(|t| Some(Keys::parse(t).unwrap())).collect();
    plan::plan(&env, &keys)
}

#[test]
fn the_planner_knows_the_kind_from_the_service_table_refuses_an_unowned_kind_and_a_second_owner() {
    let _g = LOCK.lock().unwrap();
    let _l = Loaded::new();
    let one = table(&[(KIND_LFO_SHAPE, &[7])]);
    let same = table(&[(KIND_LFO_SHAPE, &[7, 8])]);
    let other = table(&[(KIND_LFO_SHAPE, &[9])]);
    // the lfo service owns the kind: a module with a shape key is accepted
    let p = plan_of(&[one.clone()]);
    assert_eq!(p.verdicts[0], Verdict::Accepted);
    // the same shape key in a second module is refused, another key is not
    let p = plan_of(&[one.clone(), same, other]);
    assert_eq!(p.verdicts[..3], [Verdict::Accepted, Verdict::Refused(Reason::DuplicateKey(KIND_LFO_SHAPE, 7)), Verdict::Accepted]);
    assert_eq!(p.verdicts[1].result(), E_PLAN_KEY);
    // a kind no service owns
    let p = plan_of(&[table(&[(77, &[1])])]);
    assert_eq!(p.verdicts[0], Verdict::Refused(Reason::UnownedKind(77)));
    assert_eq!(p.verdicts[0].result(), E_PLAN_KIND);
}
