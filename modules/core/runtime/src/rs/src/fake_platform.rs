//! Host tests of the core paths that use the platform. `FakePlatform` implements `dkrt_abi::platform::Platform` over memory: a sector-addressed card
//! with a write log and injected errors, a block clock the test advances (including `None` and wrap), an interrupt mask that checks nesting,
//! captured MIDI output with a settable TX counter, the `*_info` tables as bytes, the presence of each optional provider, a reset flag and the OS
//! upgrade flag. It takes a `DeviceProfile` from the generated `profiles.rs`, the same facts the real platform reads from the device object.
//!
//! The contract table `tests/platform/contract.toml` names a host test for each check; `every_contract_row_names_a_host_test` keeps the names real.

use crate::card;
use crate::fake_storage::FakeStorage;
use crate::files::path_too_big;
use crate::platform::block_write_with;
use crate::store::TEST_LAYOUT;
use crate::transport::{reply_with, tx_room_with, TX_RING};
use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use dkrt_abi::platform::{blocks_since, locked, DeviceProfile, Platform, SamplePool, StagedWriter, Tasking};
use dkrt_abi::profiles;
use dkrt_abi::storage::{DirEntry, Stat, Storage, ROOT_SAMPLES};
use dkrt_sdk::store::Layout;
use dkrt_sdk::*;
use std::collections::BTreeMap;
use std::vec::Vec;

// The card, the card state and the transport buffers are statics: the tests that drive them hold the shared test lock.
use crate::test_lock::LOCK;

pub struct FakePlatform {
    pub profile: DeviceProfile,
    /// The card, by sector (a sector that was never written reads as `fill`), and the write log of the raw block write: `(sector, bytes)`.
    pub card: RefCell<BTreeMap<u32, [u8; 512]>>,
    pub fill: Cell<u8>,
    pub card_sectors: Cell<u32>,
    pub writes: RefCell<Vec<(u32, u32)>>,
    pub reads: RefCell<Vec<(u32, u32)>>,
    /// Sectors whose read or write fails (-1).
    pub bad_read: RefCell<Vec<u32>>,
    pub bad_write: RefCell<Vec<u32>>,
    /// The block clock; `None`: no counter.
    pub clock: Cell<Option<u32>>,
    /// The status register: 0 unmasked, 7 masked. `depth` counts masks not yet restored; `deepest` is the most it reached.
    pub sr: Cell<u32>,
    pub depth: Cell<u32>,
    pub deepest: Cell<u32>,
    /// MIDI output: `(port, message)`; `tx_in_flight` is the USB stack's backlog.
    pub midi: RefCell<Vec<(u32, Vec<u8>)>>,
    pub tx_in_flight: Cell<Option<u32>>,
    pub os_upgrade: Cell<bool>,
    pub reset: Cell<bool>,
    /// The exception hook word has been pointed at the runtime's hook.
    pub exc_hook: Cell<bool>,
    /// The `*_info` tables of the adapters, by name.
    pub info: RefCell<BTreeMap<&'static str, Vec<u8>>>,
    pub pool: Cell<bool>,
    pub staged: Cell<bool>,
    pub worker: Cell<u32>,
    pub queued: RefCell<Vec<u32>>,
    pub tasks: RefCell<Vec<u32>>,
    pub refuse_task: Cell<bool>,
    pub sems: RefCell<Vec<(u32, &'static str)>>,
    /// The files of the two roots (`Storage`).
    pub storage: FakeStorage,
}

impl FakePlatform {
    pub fn new(profile: DeviceProfile) -> Self {
        FakePlatform {
            profile,
            card: RefCell::new(BTreeMap::new()),
            fill: Cell::new(0xff),
            card_sectors: Cell::new(1 << 20),
            writes: RefCell::new(Vec::new()),
            reads: RefCell::new(Vec::new()),
            bad_read: RefCell::new(Vec::new()),
            bad_write: RefCell::new(Vec::new()),
            clock: Cell::new(Some(0)),
            sr: Cell::new(0),
            depth: Cell::new(0),
            deepest: Cell::new(0),
            midi: RefCell::new(Vec::new()),
            tx_in_flight: Cell::new(Some(0)),
            os_upgrade: Cell::new(false),
            reset: Cell::new(false),
            exc_hook: Cell::new(false),
            info: RefCell::new(BTreeMap::new()),
            pool: Cell::new(false),
            staged: Cell::new(false),
            worker: Cell::new(0),
            queued: RefCell::new(Vec::new()),
            tasks: RefCell::new(Vec::new()),
            refuse_task: Cell::new(false),
            sems: RefCell::new(Vec::new()),
            storage: FakeStorage::new(),
        }
    }
    pub fn sector(&self, s: u32) -> [u8; 512] {
        self.card.borrow().get(&s).copied().unwrap_or([self.fill.get(); 512])
    }
    pub fn put(&self, s: u32, data: [u8; 512]) {
        self.card.borrow_mut().insert(s, data);
    }
    /// Advance the block clock (wrapping); no effect without a counter.
    pub fn tick(&self, blocks: u32) {
        if let Some(b) = self.clock.get() {
            self.clock.set(Some(b.wrapping_add(blocks)));
        }
    }
}

impl Platform for FakePlatform {
    fn profile(&self) -> DeviceProfile {
        self.profile
    }
    fn image_id(&self) -> [u8; 8] {
        [0; 8]
    }
    fn blocks(&self) -> Option<u32> {
        self.clock.get()
    }
    fn irq_mask(&self) -> u32 {
        let old = self.sr.get();
        self.sr.set(7);
        self.depth.set(self.depth.get() + 1);
        self.deepest.set(self.deepest.get().max(self.depth.get()));
        old
    }
    fn irq_restore(&self, sr: u32) {
        assert!(self.depth.get() > 0, "restore without a mask");
        self.depth.set(self.depth.get() - 1);
        self.sr.set(sr);
    }
    fn flush(&self, _start: u32, _len: u32) {}
    unsafe fn block_read(&self, sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        self.reads.borrow_mut().push((sector, bytes));
        let n = bytes / 512;
        if sector + n > self.card_sectors.get() || (0..n).any(|i| self.bad_read.borrow().contains(&(sector + i))) {
            return -1;
        }
        for i in 0..n {
            core::ptr::copy_nonoverlapping(self.sector(sector + i).as_ptr(), buf.add(i as usize * 512), 512);
        }
        0
    }
    unsafe fn block_write_raw(&self, sector: u32, bytes: u32, buf: *const u8) -> i32 {
        self.writes.borrow_mut().push((sector, bytes));
        let n = bytes / 512;
        if sector + n > self.card_sectors.get() || (0..n).any(|i| self.bad_write.borrow().contains(&(sector + i))) {
            return -1;
        }
        for i in 0..n {
            let mut d = [0u8; 512];
            core::ptr::copy_nonoverlapping(buf.add(i as usize * 512), d.as_mut_ptr(), 512);
            self.put(sector + i, d);
        }
        0
    }
    unsafe fn midi_send(&self, port: u32, msg: &[u8]) {
        self.midi.borrow_mut().push((port, msg.to_vec()));
    }
    fn midi_tx_in_flight(&self) -> Option<u32> {
        self.tx_in_flight.get()
    }
    fn os_upgrade_active(&self) -> bool {
        self.os_upgrade.get()
    }
    unsafe fn track_type(&self, _track: u32) -> Option<i32> {
        None
    }
    unsafe fn key_down(&self, _key: u32) -> bool {
        false
    }
    fn install_exception_hook(&self) -> bool {
        let first = !self.exc_hook.get();
        self.exc_hook.set(true);
        first
    }
    fn soft_reset(&self) -> ! {
        self.reset.set(true);
        panic!("soft reset")
    }
}

impl Tasking for FakePlatform {
    unsafe fn sem_init(&self, sem: *mut u32, _count: u32) {
        self.sems.borrow_mut().push((sem as u32, "init"));
    }
    unsafe fn sem_pend(&self, sem: *mut u32) {
        self.sems.borrow_mut().push((sem as u32, "pend"));
    }
    unsafe fn sem_post(&self, sem: *mut u32) {
        self.sems.borrow_mut().push((sem as u32, "post"));
    }
    unsafe fn sleep_us(&self, _us: u32) {}
    unsafe fn task_create(&self, tcb: u32, _entry: u32, _priority: u32, _stack: u32, _bytes: u32) -> bool {
        if self.refuse_task.get() {
            return false;
        }
        self.tasks.borrow_mut().push(tcb);
        true
    }
    unsafe fn task_start(&self, _tcb: u32) {}
    unsafe fn current_task(&self) -> u32 {
        self.tasks.borrow().last().copied().unwrap_or(0)
    }
    unsafe fn alloc(&self, _bytes: u32) -> *mut u8 {
        core::ptr::null_mut()
    }
    unsafe fn free(&self, _p: *mut u8) {}
}

impl SamplePool for FakePlatform {
    fn present(&self) -> bool {
        self.pool.get()
    }
    unsafe fn worker(&self) -> u32 {
        self.worker.get()
    }
    unsafe fn worker_queue(&self, worker: u32, _name: *const u8, _job: *mut c_void, _done: *mut c_void) {
        self.queued.borrow_mut().push(worker);
    }
}

impl StagedWriter for FakePlatform {
    fn present(&self) -> bool {
        self.staged.get()
    }
}

impl Storage for FakePlatform {
    fn path_max(&self, root: u32) -> u32 {
        match root {
            ROOT_SAMPLES => self.profile.sample_path_max,
            _ => self.storage.path_max(root),
        }
    }
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32> {
        self.storage.stat(root, path)
    }
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
        self.storage.read_head(root, path, out)
    }
    unsafe fn stat_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(Stat, usize), i32> {
        self.storage.stat_head(root, path, out)
    }
    unsafe fn read_range(&self, root: u32, path: &[u8], offset: u32, out: &mut [u8]) -> Result<usize, i32> {
        self.storage.read_range(root, path, offset, out)
    }
    unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32> {
        self.storage.read_all(root, path, dst, cap)
    }
    fn list_state_bytes(&self, root: u32) -> usize {
        self.storage.list_state_bytes(root)
    }
    unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32> {
        self.storage.list_open(root, dir, state)
    }
    unsafe fn list_next(&self, state: *mut u8, out: &mut DirEntry) -> Result<dkrt_abi::storage::ListStep, i32> {
        self.storage.list_next(state, out)
    }
}

/// An invented device: samples in the store, a path limit no device file has.
const INVENTED: DeviceProfile = DeviceProfile { id: 9, name: 0x78797a00, samples_in_store: true, sample_path_max: 100, dsp_headroom: 50_000, dsp_voices: 8 };

const L: Layout = Layout { first: 1000, sectors: 4096 };

fn card_reset() {
    *card::info() = card::CardInfo {
        state: CARD_UNKNOWN,
        first_nonblank: u32::MAX,
        hash: 0,
        refused: 0,
        refused_range: 0,
        read_errors: 0,
        checked: 0,
        claims: 0,
        writes: 0,
    };
    TEST_LAYOUT.with(|c| c.set(Some(L)));
}

static mut BUF: [u8; 1024] = [0; 1024];

unsafe fn write(p: &FakePlatform, sector: u32, sectors: u32) -> i32 {
    block_write_with(p, sector, sectors * 512, core::ptr::addr_of!(BUF) as *const u8)
}

#[test]
fn every_profile_is_complete_and_distinct() {
    let mut ids = std::vec::Vec::new();
    for p in profiles::ALL {
        assert!(p.id != 0 && !ids.contains(&p.id), "{p:?}: ids are distinct and not 0");
        ids.push(p.id);
        let name = p.name.to_be_bytes();
        assert!(name[0] != 0 && name[3] == 0, "{p:?}: a name is 1 to 3 characters and a NUL");
        assert!((1..=254).contains(&p.sample_path_max));
    }
    // the platform asks the profile, with provided methods
    let f = FakePlatform::new(profiles::DN2);
    assert_eq!((f.device_id(), f.device_name(), f.samples_in_store(), f.sample_path_max()), (2, 0x646e3200, true, 151));
}

#[test]
fn card_writes_outside_the_store_are_refused_and_never_reach_the_card() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    card_reset();
    let f = FakePlatform::new(profiles::DT2);
    unsafe {
        assert_eq!(write(&f, L.first - 1, 1), E_RANGE);
        assert_eq!(write(&f, L.first + L.sectors, 1), E_RANGE);
        assert_eq!(block_write_with(&f, L.first, 100, core::ptr::addr_of!(BUF) as *const u8), E_RANGE, "not a multiple of 512");
    }
    assert!(f.writes.borrow().is_empty());
    assert_eq!(card::info().refused_range, 3);
}

#[test]
fn a_blank_card_is_claimed_before_the_first_data_write_and_ours_afterwards() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    card_reset();
    let f = FakePlatform::new(profiles::DN2);
    unsafe {
        assert_eq!(write(&f, L.first + 10, 1), OK);
    }
    assert_eq!(card::state(), CARD_OURS);
    let w = f.writes.borrow().clone();
    // the claim writes both header copies first, then the data
    assert_eq!(w, [(L.first + L.header_b(), 512), (L.first, 512), (L.first + 10, 512)]);
    assert_eq!(card::info().claims, 1);
    unsafe {
        assert_eq!(write(&f, L.first + 11, 1), OK);
    }
    assert_eq!(f.writes.borrow().len(), 4, "no second claim");
    assert_eq!(card::info().writes, 2);
}

#[test]
fn a_foreign_card_is_read_only_for_every_write() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    card_reset();
    let f = FakePlatform::new(profiles::DT2);
    f.put(L.first + 5, [0x55; 512]); // not blank and not ours
    unsafe {
        assert_eq!(write(&f, L.first + 10, 1), E_CARD_RO);
        assert_eq!(write(&f, L.sel_a(), 1), E_CARD_RO);
    }
    assert_eq!(card::state(), CARD_FOREIGN);
    assert!(f.writes.borrow().is_empty(), "nothing reached the card");
    assert_eq!(card::info().refused, 2);
}

#[test]
fn a_card_that_ends_before_the_store_has_no_store() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    card_reset();
    let f = FakePlatform::new(profiles::DT2);
    f.card_sectors.set(L.first + 100);
    unsafe {
        assert_eq!(write(&f, L.first + 10, 1), E_CARD_RO);
    }
    assert_eq!(card::state(), CARD_TOO_SMALL);
    assert!(f.writes.borrow().is_empty());
}

#[test]
fn an_unreadable_card_is_read_only() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    card_reset();
    let f = FakePlatform::new(profiles::DT2);
    f.bad_read.borrow_mut().push(L.first);
    unsafe {
        assert_eq!(write(&f, L.first + 10, 1), E_CARD_RO);
    }
    assert_eq!(card::state(), CARD_UNREADABLE);
    assert!(card::info().read_errors > 0);
}

#[test]
fn the_block_clock_wraps_and_reports_none_without_a_counter() {
    let f = FakePlatform::new(profiles::DT2);
    f.clock.set(Some(u32::MAX - 5));
    let start = f.blocks();
    f.tick(16);
    assert_eq!(f.blocks(), Some(10));
    assert_eq!(blocks_since(&f, start), Some(16), "the difference survives the wrap");
    f.clock.set(None);
    assert_eq!(blocks_since(&f, start), None, "no counter now");
    assert_eq!(blocks_since(&f, None), None, "no counter at the start");
    f.tick(5);
    assert_eq!(f.blocks(), None);
}

#[test]
fn interrupt_masks_nest_and_restore_the_caller_state() {
    let f = FakePlatform::new(profiles::DT2);
    let r = locked(&f, || {
        assert_eq!(f.sr.get(), 7);
        locked(&f, || {
            assert_eq!(f.depth.get(), 2);
            11
        }) + 1
    });
    assert_eq!(r, 12);
    assert_eq!((f.depth.get(), f.sr.get(), f.deepest.get()), (0, 0, 2));
    // a caller that was already masked stays masked
    f.sr.set(5);
    locked(&f, || ());
    assert_eq!(f.sr.get(), 5);
}

#[test]
fn a_reply_goes_to_the_usb_port_as_one_sysex_message_and_is_dropped_without_room() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = FakePlatform::new(profiles::DT2);
    unsafe {
        assert_eq!(reply_with(&f, 1, 5, 0, 0), 1);
        let m = f.midi.borrow().clone();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, 2, "port 2 is USB");
        let w = &m[0].1;
        assert_eq!(&w[..4], [0xf0, 0x7d, 0x64, 0x6b]);
        assert_eq!(*w.last().unwrap(), 0xf7);
        assert_eq!(w[5], 0x41, "the command with the reply bit");
        assert_eq!(w[6], 5, "the sequence number");

        let dropped = crate::transport::DKRT_UPLOAD_DROPPED;
        f.tx_in_flight.set(Some(TX_RING));
        assert!(!tx_room_with(&f, 20), "a full ring has no room");
        assert_eq!(reply_with(&f, 1, 6, 0, 0), 1);
        assert_eq!(f.midi.borrow().len(), 1, "nothing was sent");
        assert_eq!(crate::transport::DKRT_UPLOAD_DROPPED, dropped.wrapping_add(1));
        f.tx_in_flight.set(None);
        assert!(!tx_room_with(&f, 20), "no counter: no reply rather than a hang");
        f.tx_in_flight.set(Some(0));
        assert!(tx_room_with(&f, 20));
    }
}

#[test]
fn an_os_upgrade_stops_the_resets_the_runtime_would_make() {
    let f = FakePlatform::new(profiles::DT2);
    assert!(crate::fault::panic_resets(&f, 0));
    assert!(!crate::fault::panic_resets(&f, 1), "no reset loop in safe mode");
    assert!(crate::watch::hang_may_reset(&f));
    f.os_upgrade.set(true);
    assert!(!crate::fault::panic_resets(&f, 0));
    assert!(!crate::watch::hang_may_reset(&f));
}

#[test]
fn the_optional_providers_are_absent_until_the_image_has_them() {
    let f = FakePlatform::new(profiles::DT2);
    assert!(!SamplePool::present(&f));
    assert!(!StagedWriter::present(&f));
    f.pool.set(true);
    f.staged.set(true);
    assert!(SamplePool::present(&f));
    assert!(StagedWriter::present(&f));
    unsafe {
        f.worker.set(0x1234);
        f.worker_queue(f.worker(), b"DkJob\0".as_ptr(), core::ptr::null_mut(), core::ptr::null_mut());
    }
    assert_eq!(*f.queued.borrow(), [0x1234]);
}

#[test]
fn the_exception_hook_is_installed_once_and_asked_for_again_harmlessly() {
    let f = FakePlatform::new(profiles::DT2);
    assert!(f.install_exception_hook(), "the first call changes the word");
    assert!(!f.install_exception_hook(), "the next passes find it ours");
}

#[test]
fn the_kernel_calls_go_to_the_platform() {
    let f = FakePlatform::new(profiles::DT2);
    unsafe {
        assert!(f.task_create(0x100, 0, 5, 0x200, 1024));
        assert_eq!(f.current_task(), 0x100);
        f.refuse_task.set(true);
        assert!(!f.task_create(0x300, 0, 5, 0x400, 1024));
    }
}

#[test]
fn the_info_tables_are_bytes_the_fake_hands_out() {
    let f = FakePlatform::new(profiles::DT2);
    f.info.borrow_mut().insert("dynreg_info", std::vec![0x44, 0x4b, 0x52, 0x47]);
    assert_eq!(f.info.borrow().get("dynreg_info").map(|v| v.len()), Some(4));
    assert!(f.info.borrow().get("dyndsp_info").is_none(), "a table that is not in the image is absent");
}

#[test]
fn sample_paths_are_limited_by_the_profile_not_by_the_device() {
    // the store root: 159 bytes whatever the device
    for p in [profiles::DT2, profiles::DN2, INVENTED] {
        let f = FakePlatform::new(p);
        assert!(!path_too_big(&f, files_store(), 159));
        assert!(path_too_big(&f, files_store(), 160));
        assert!(!path_too_big(&f, files_samples(), 0), "the root itself");
        let max = p.sample_path_max as usize;
        assert!(!path_too_big(&f, files_samples(), max), "{p:?}");
        assert!(path_too_big(&f, files_samples(), max + 1), "{p:?}");
    }
    // the numbers the device files give today
    assert_eq!((profiles::DN2.sample_path_max, profiles::DT2.sample_path_max), (151, 254));
}

fn files_store() -> u32 {
    dkrt_sdk::files::STORE
}
fn files_samples() -> u32 {
    dkrt_sdk::files::SAMPLES
}

#[test]
fn the_samples_root_follows_the_capability_and_the_staged_writer() {
    use crate::store_writer::{root_route, ROOT_SAMPLES, ROOT_STORE};
    let store = |p: DeviceProfile| FakePlatform::new(p);
    // a device with its samples in the store: the `samples/` folder, no adapter needed (DN2 and an invented device alike)
    for p in [profiles::DN2, INVENTED] {
        assert_eq!(root_route(&store(p), ROOT_SAMPLES), Ok((b"samples/".as_slice(), false)), "{p:?}");
    }
    // a device with the firmware's folder filesystem (DT2): the staged writer serves it, or the root does not exist
    let f = store(profiles::DT2);
    assert_eq!(root_route(&f, ROOT_SAMPLES), Err(E_CONTRACT));
    f.staged.set(true);
    assert_eq!(root_route(&f, ROOT_SAMPLES), Ok((b"".as_slice(), true)));
    // the store root is the same everywhere
    assert_eq!(root_route(&f, ROOT_STORE), Ok((b"".as_slice(), false)));
    assert_eq!(root_route(&f, 99), Err(E_CONTRACT));
}

/// The contract table is data (`tests/platform/contract.toml`): each `[[check]]` has an `id`, and a `host` test that must exist in this file, or a
/// `host_pending` reason; a `device` check that the device test (`platform-contract.device.test.ts`) runs, or a `device_pending` reason.
#[test]
fn every_contract_row_names_a_host_test() {
    let table = include_str!("../../../tests/platform/contract.toml");
    let this = include_str!("fake_platform.rs");
    let mut rows = 0;
    for block in table.split("[[check]]").skip(1) {
        let get = |key: &str| {
            block.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
            })
        };
        let id = get("id").expect("a check has an id");
        rows += 1;
        match get("host") {
            Some(name) => assert!(this.contains(&std::format!("fn {name}(")), "{id}: no host test {name}"),
            None => assert!(get("host_pending").is_some(), "{id}: a host test or a reason"),
        }
        assert!(get("device").is_some() || get("device_pending").is_some(), "{id}: a device check or a reason");
        assert!(get("what").is_some(), "{id}: says what it checks");
    }
    assert!(rows >= 10, "the checks of the design and the exception hook");
}
