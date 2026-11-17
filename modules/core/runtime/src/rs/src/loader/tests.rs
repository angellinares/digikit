//! The module lifecycle over fakes: `Loader<FakePlatform, FakeImages, FakeRegistry>` loads a small image, and every undo path gives back what the
//! step before it took. The image has no relocations, DSP section or records; the fakes log what the loader asks of the store, the arena, the module and
//! the registry.
use super::*;
use crate::fake_platform::FakePlatform;
use crate::sha256::Sha;
use crate::test_lock::LOCK;
use dkrt_abi::profiles;
use std::collections::BTreeMap;
use std::vec::Vec;

// the seams between the real packed artifacts (`core/runtime:test-assembly`)
mod assembly_tests;

const PATH: &[u8] = b"mods/a.dkm\0";
const IMAGE_OFF: usize = 160;
const FIRST_ADDR: u32 = 0x0010_0000;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Stage,
    Alloc(u32, u32),
    Free(u32),
    Init(usize, u32),
    Fini(usize, u32),
    Add(usize),
    /// `add` with the service row of a file.
    AddRow(usize, *const dkrt_abi::service::ServiceRow),
    Remove(usize),
    AbiOk(u32),
}

/// What the init of the module does.
#[derive(Clone, Copy)]
enum Init {
    Ok,
    Fails(i32),
    /// Asks for the service `core/usb-bulk` and returns `E_MISSING`.
    NeedsUsbBulk,
    /// Declares what the header table of a service says: (declare tag, first word, entry offset in the image) for a hook or an export, as `init` of a service does.
    Declare(&'static [(u16, u32, u32)]),
    /// A service file: declares `TEST_ROW`.
    DeclaresRow,
}

struct Nothing;
impl<C: dkrt_abi::core_api::CoreApi> dkrt_abi::service::Service<C> for Nothing {}
/// The row a service file declares in these tests; `svc-test` is a `[[service]]` that no runtime row owns.
static TEST_ROW: dkrt_abi::service::ServiceRow = dkrt_abi::service::ServiceRow::of::<Nothing, crate::core_api::RealCore>("svc-test", &[]);

struct FakeImages {
    files: BTreeMap<Vec<u8>, Vec<u8>>,
    staged: Vec<u8>,
    /// Live blocks: address and memory (16-byte units, so the host pointer is aligned).
    live: Vec<(u32, Vec<u128>)>,
    allocs: u32,
    /// The n-th alloc (from 1) returns `None`.
    fail_alloc: Option<u32>,
    init: Init,
    log: Vec<Ev>,
}

/// The one import the fake runtime has.
const KNOWN_IMPORT: &[u8] = b"dk_alloc\0";
const KNOWN_ADDR: u32 = 0x4000;
/// The one service-only import the fake runtime has.
const SVC_IMPORT: &[u8] = b"dkc_device_id\0";
const SVC_ADDR: u32 = 0x5000;

impl Images for FakeImages {
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32> {
        self.log.push(Ev::Stage);
        let n = cstr_len(path, 64);
        let key = core::slice::from_raw_parts(path, n).to_vec();
        match self.files.get(&key) {
            None => Err(E_NOT_FOUND),
            Some(f) => {
                self.staged = f.clone();
                Ok(Staged { ptr: self.staged.as_ptr(), size: self.staged.len() as u32 })
            }
        }
    }
    unsafe fn import_addr(&mut self, name: *const u8) -> u32 {
        if cstr_eq(name, KNOWN_IMPORT.as_ptr()) {
            KNOWN_ADDR
        } else {
            0
        }
    }
    unsafe fn svc_import_addr(&mut self, name: *const u8) -> u32 {
        if cstr_eq(name, SVC_IMPORT.as_ptr()) {
            SVC_ADDR
        } else {
            0
        }
    }
    unsafe fn import_needs(&mut self, name: *const u8) -> Option<&'static str> {
        if cstr_eq(name, b"dk_usb_bulk_open\0".as_ptr()) {
            Some("usb-bulk")
        } else {
            None
        }
    }
    unsafe fn alloc(&mut self, n: u32) -> Option<Block> {
        self.allocs += 1;
        if self.fail_alloc == Some(self.allocs) {
            return None;
        }
        let addr = FIRST_ADDR * self.allocs;
        let mut mem = vec![0u128; (n as usize + 15) / 16];
        let ptr = mem.as_mut_ptr() as *mut u8;
        self.live.push((addr, mem));
        self.log.push(Ev::Alloc(addr, n));
        Some(Block { ptr, addr })
    }
    unsafe fn free(&mut self, addr: u32) {
        let at = self.live.iter().position(|b| b.0 == addr).expect("free of a block that is not live");
        self.live.remove(at);
        self.log.push(Ev::Free(addr));
    }
    unsafe fn list(&mut self, prefix: *const u8, out: *mut dkrt_sdk::DkrtStoreEntry, max: u32) -> i32 {
        let n = cstr_len(prefix, 64);
        let prefix = core::slice::from_raw_parts(prefix, n);
        let mut k = 0;
        for (name, bytes) in &self.files {
            if k < max as usize && name.starts_with(prefix) {
                let e = &mut *out.add(k);
                e.path = [0; 64];
                e.path[..name.len()].copy_from_slice(name);
                e.size = bytes.len() as u32;
                k += 1;
            }
        }
        k as i32
    }
    unsafe fn enter_init(&mut self, rec: usize, init: u32, ctx: *const Ctx) -> i32 {
        self.log.push(Ev::Init(rec, init));
        match self.init {
            Init::Ok => OK,
            // (`ctx` is the context address as a u32: not a host pointer)
            Init::DeclaresRow => crate::collect::dk_service_declare_row(core::ptr::addr_of_mut!(crate::entry::DKRT_CTXS[rec]) as *mut u8, &TEST_ROW),
            Init::Fails(e) => e,
            Init::NeedsUsbBulk => {
                stub_missing("core/usb-bulk");
                E_MISSING
            }
            Init::Declare(items) => {
                // `init` is at image offset 0: its address is the base. The context pointer of the loader is 32 bits: on the host the test takes the real one.
                let _ = ctx;
                let ctx = core::ptr::addr_of_mut!(crate::entry::DKRT_CTXS[rec]) as *mut u8;
                // only a provider declares (a module in the same boot declares nothing)
                let items: &[(u16, u32, u32)] = if rec >= MAX_MODULES { items } else { &[] };
                for &(tag, word, entry) in items {
                    let at = (init + entry) as usize as *const u8;
                    let r = if tag == dkrt_abi::service::Declare::HOOK {
                        crate::collect::dk_service_declare_hook(ctx, word, at)
                    } else if tag == dkrt_abi::service::Declare::SLOT {
                        crate::collect::dk_service_declare_slot(ctx, word, 0, at)
                    } else {
                        crate::collect::dk_service_declare_export(ctx, word, at)
                    };
                    assert_eq!(r, OK);
                }
                OK
            }
        }
    }
    unsafe fn enter_fini(&mut self, rec: usize, fini: u32, _: *const Ctx) {
        self.log.push(Ev::Fini(rec, fini));
    }
}

struct FakeRegistry {
    abi: bool,
    add_result: i32,
    log: Vec<Ev>,
    asked: core::cell::RefCell<Vec<Ev>>,
}

impl Registry for FakeRegistry {
    fn abi_ok(&self, fingerprint: u32) -> bool {
        // `abi_ok` takes `&self`: the log is behind a cell
        self.asked.borrow_mut().push(Ev::AbiOk(fingerprint));
        self.abi
    }
    unsafe fn add(&mut self, rec: usize, row: Option<&dkrt_abi::service::ServiceRow>) -> i32 {
        self.log.push(match row {
            None => Ev::Add(rec),
            Some(r) => Ev::AddRow(rec, r as *const _),
        });
        self.add_result
    }
    unsafe fn remove(&mut self, rec: usize) {
        self.log.push(Ev::Remove(rec));
    }
}

fn put(v: &mut [u8], off: u32, val: u32) {
    v[off as usize..off as usize + 4].copy_from_slice(&val.to_be_bytes());
}

/// Set the size field and the file hash after a change.
fn seal(v: &mut [u8]) {
    let n = v.len() as u32;
    put(v, dkneeds::H_SIZE, n);
    let mut sha = Sha::new();
    let mut got = [0u8; 32];
    unsafe {
        sha.update(v.as_ptr(), dkneeds::H_SHA);
        sha.update_zeros(32);
        sha.update(v.as_ptr().add(dkneeds::H_SHA as usize + 32), n - (dkneeds::H_SHA + 32));
        sha.finish(got.as_mut_ptr());
    }
    v[dkneeds::H_SHA as usize..dkneeds::H_SHA as usize + 32].copy_from_slice(&got);
}

/// A 32-byte image with `init` at 0 and `fini` at 4, an empty header table, the relocations `relocs` (offset, kind word) and the import names `imports`.
fn module_file_with(relocs: &[(u32, u32)], imports: &[&[u8]]) -> Vec<u8> {
    let reloc_off = IMAGE_OFF + 32;
    let import_off = reloc_off + relocs.len() * 8;
    let keys_off = import_off + imports.len() * dkneeds::CF_IMPORT_LEN as usize;
    let mut v = vec![0u8; keys_off + 8];
    put(&mut v, dkneeds::H_MAGIC, dkneeds::MAGIC);
    put(&mut v, dkneeds::H_FORMAT, 1);
    put(&mut v, dkneeds::H_HDRSIZE, dkneeds::HEADER);
    v[dkneeds::H_NAME as usize] = b'a';
    put(&mut v, dkneeds::H_IMAGE_OFF, IMAGE_OFF as u32);
    put(&mut v, dkneeds::H_IMAGE_SIZE, 32);
    put(&mut v, dkneeds::H_ALIGN, 4);
    put(&mut v, dkneeds::H_RELOC_OFF, reloc_off as u32);
    put(&mut v, dkneeds::H_RELOC_N, relocs.len() as u32);
    put(&mut v, dkneeds::H_IMPORT_OFF, import_off as u32);
    put(&mut v, dkneeds::H_IMPORT_N, imports.len() as u32);
    put(&mut v, dkneeds::H_INIT, 0);
    put(&mut v, dkneeds::H_FINI, 4);
    put(&mut v, dkneeds::H_MENU, dkneeds::NONE);
    put(&mut v, dkneeds::H_TICK, dkneeds::NONE);
    put(&mut v, dkneeds::H_REG_OFF, keys_off as u32);
    put(&mut v, dkneeds::H_REG_SIZE, 8);
    for (i, (off, kind)) in relocs.iter().enumerate() {
        put(&mut v, (reloc_off + i * 8) as u32, *off);
        put(&mut v, (reloc_off + i * 8 + 4) as u32, *kind);
    }
    for (i, name) in imports.iter().enumerate() {
        let at = import_off + i * dkneeds::CF_IMPORT_LEN as usize;
        v[at..at + name.len()].copy_from_slice(name);
    }
    put(&mut v, keys_off as u32, dkneeds::keys::MAGIC);
    put(&mut v, keys_off as u32 + 4, (dkneeds::keys::VERSION as u32) << 16);
    seal(&mut v);
    v
}

fn module_file() -> Vec<u8> {
    module_file_with(&[], &[])
}

type Fake = Loader<FakePlatform, FakeImages, FakeRegistry>;

/// A loader in the boot phase over one file, with the runtime tables cleared.
unsafe fn loader(init: Init) -> Fake {
    loader_of(init, module_file())
}

unsafe fn loader_of(init: Init, file: Vec<u8>) -> Fake {
    *st() = core::mem::zeroed();
    records::reset();
    *dsp::ds() = core::mem::zeroed();
    dsp::init_once();
    CALLED_NEED = [0; NEEDS_LEN];
    crate::cleanup_tests::lenient_tick_drop();
    let mut files = BTreeMap::new();
    files.insert(PATH[..PATH.len() - 1].to_vec(), file);
    let imgs = FakeImages { files, staged: Vec::new(), live: Vec::new(), allocs: 0, fail_alloc: None, init, log: Vec::new() };
    let reg = FakeRegistry { abi: true, add_result: OK, log: Vec::new(), asked: Default::default() };
    let mut l = Loader::new(FakePlatform::new(profiles::DT2), imgs, reg);
    l.phase = Phase::Boot;
    l
}

#[test]
fn a_module_loads_and_unloads_and_gives_back_every_block() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::Ok);
        assert_eq!(l.load(PATH.as_ptr()), OK);
        let base = FIRST_ADDR;
        let s = st();
        assert_eq!((s.modules[0].used, s.modules[0].base, s.loads), (1, base, 1));
        assert_eq!(s.modules[0].fini, base + 4);
        // the module block stays; the record block of init was given back after the commit
        assert_eq!(l.m.live.iter().map(|b| b.0).collect::<Vec<_>>(), vec![base]);
        assert_eq!(l.m.log[0], Ev::Stage);
        assert!(l.m.log.contains(&Ev::Init(0, base)));
        assert_eq!(l.r.log, vec![Ev::Add(0)]);

        assert_eq!(l.unload_rec(0), OK);
        assert!(l.m.live.is_empty());
        assert_eq!(st().modules[0].used, 0);
        let fini = l.m.log.iter().position(|e| *e == Ev::Fini(0, base + 4)).expect("fini ran");
        let free = l.m.log.iter().position(|e| *e == Ev::Free(base)).expect("the module block is freed");
        assert!(fini < free, "fini runs before the memory goes");
        assert_eq!(l.r.log, vec![Ev::Add(0), Ev::Remove(0)]);
    }
}

#[test]
fn a_failed_init_undoes_the_load() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::Fails(E_INIT));
        assert_eq!(l.load(PATH.as_ptr()), E_INIT);
        assert!(l.m.live.is_empty(), "the module block and the record block are free");
        assert_eq!(st().modules[0].used, 0);
        assert_eq!(st().loads, 0);
        assert_eq!(l.r.log, vec![Ev::Remove(0)], "nothing was added, the undo still asks the registry");
        assert!(!l.m.log.iter().any(|e| matches!(e, Ev::Fini(..))), "a module whose init failed gets no fini");
    }
}

#[test]
fn a_failed_commit_undoes_the_load() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::Ok);
        l.r.add_result = E_NO_REGISTRY;
        assert_eq!(l.load(PATH.as_ptr()), E_NO_REGISTRY);
        assert!(l.m.live.is_empty());
        assert_eq!(st().modules[0].used, 0);
        assert_eq!(l.r.log, vec![Ev::Add(0), Ev::Remove(0)]);
        // the record can be used again
        l.r.add_result = OK;
        l.m.log.clear();
        l.r.log.clear();
        assert_eq!(l.load(PATH.as_ptr()), OK);
        assert_eq!(st().modules[0].used, 1);
    }
}

#[test]
fn a_need_of_init_is_returned_as_the_missing_service() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::NeedsUsbBulk);
        assert_eq!(l.load(PATH.as_ptr()), E_IMPORT);
        assert_eq!(&l.missing()[..8], b"usb-bulk");
        assert!(l.m.live.is_empty());
        assert_eq!(l.r.log, vec![Ev::Remove(0)]);
    }
}

#[test]
fn an_arena_that_is_full_gives_no_memory_error_and_keeps_nothing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // the module block
        let mut l = loader(Init::Ok);
        l.m.fail_alloc = Some(1);
        assert_eq!(l.load(PATH.as_ptr()), E_NOMEM);
        assert!(l.m.live.is_empty());
        assert!(!l.m.log.iter().any(|e| matches!(e, Ev::Init(..))));
        assert_eq!(st().modules[0].used, 0);
        // the record block of init: the module block goes again
        let mut l = loader(Init::Ok);
        l.m.fail_alloc = Some(2);
        assert_eq!(l.load(PATH.as_ptr()), E_NOMEM);
        assert!(l.m.live.is_empty());
        assert!(!l.m.log.iter().any(|e| matches!(e, Ev::Init(..))));
        assert_eq!(st().modules[0].used, 0);
    }
}

#[test]
fn a_load_outside_the_boot_or_of_a_missing_file_touches_nothing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::Ok);
        l.phase = Phase::Closed;
        assert_eq!(l.load(PATH.as_ptr()), E_BOOT_ONLY);
        assert!(l.m.log.is_empty());
        l.phase = Phase::Boot;
        assert_eq!(l.load(b"mods/none.dkm\0".as_ptr()), E_NOT_FOUND);
        assert_eq!(l.m.log, vec![Ev::Stage]);
        assert!(l.r.log.is_empty());
    }
}

#[test]
fn a_damaged_file_is_refused_before_anything_is_allocated() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader(Init::Ok);
        let f = l.m.files.get_mut(&PATH[..PATH.len() - 1].to_vec()).unwrap();
        f[IMAGE_OFF + 2] ^= 1; // the file hash no longer matches
        assert_eq!(l.load(PATH.as_ptr()), E_SHA);
        assert_eq!(l.m.allocs, 0);
        assert!(l.r.log.is_empty());
    }
}

#[test]
fn relocations_name_the_block_or_an_import_of_the_runtime() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // word at 8 holds 0x20 (relative to the block), word at 12 holds 4 (plus the import)
        let mut file = module_file_with(&[(8, 0), (12, 1 << 24)], &[b"dk_alloc"]);
        file[IMAGE_OFF + 11] = 0x20;
        file[IMAGE_OFF + 15] = 4;
        seal(&mut file);
        let mut l = loader_of(Init::Ok, file);
        assert_eq!(l.load(PATH.as_ptr()), OK);
        let mem = &l.m.live[0].1;
        let words = core::slice::from_raw_parts(mem.as_ptr() as *const u8, 32);
        assert_eq!(rd32(words.as_ptr().add(8)), FIRST_ADDR + 0x20);
        assert_eq!(rd32(words.as_ptr().add(12)), KNOWN_ADDR + 4);
    }
}

#[test]
fn an_import_of_a_module_that_is_not_there_names_it_and_allocates_nothing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut l = loader_of(Init::Ok, module_file_with(&[], &[b"dk_usb_bulk_open"]));
        assert_eq!(l.load(PATH.as_ptr()), E_IMPORT);
        assert_eq!(&l.missing()[..8], b"usb-bulk");
        assert_eq!(l.m.allocs, 0);
        // an import nobody knows is refused without a name
        let mut l = loader_of(Init::Ok, module_file_with(&[], &[b"dk_nothing"]));
        assert_eq!(l.load(PATH.as_ptr()), E_IMPORT);
        assert_eq!(l.missing()[0], 0);
    }
}

/// A service file (role 1) of `table` (a v2 header table), with `dsp` as its DSP section size word.
fn service_file(table: &dkneeds::keys::Table, dsp: u32) -> Vec<u8> {
    service_file_of(module_file(), table, dsp)
}

/// `service_file` over the module file `v`.
fn service_file_of(mut v: Vec<u8>, table: &dkneeds::keys::Table, dsp: u32) -> Vec<u8> {
    let keys_off = v.len() - 8;
    v.truncate(keys_off);
    let mut t = vec![0u8; dkneeds::keys::table_size(table)];
    dkneeds::keys::write_table(table, &mut t).unwrap();
    v.extend_from_slice(&t);
    put(&mut v, dkneeds::H_REG_SIZE, t.len() as u32);
    put(&mut v, dkneeds::H_ROLE, 1);
    put(&mut v, dkneeds::H_DSP_SIZE, dsp);
    seal(&mut v);
    v
}

const SITE: u32 = 0x400a_5eac;
const STOCK: [u8; 6] = [0x20, 0x52, 0x48, 0x78, 0x00, 0x02];
const SVC: &str = "svc-test";

/// The table of a service `SVC` that exports `ping` (image offset 8) and binds one hook at `SITE` (handler at 4, trampoline at 16).
fn svc_table<'a>(bind: &'a [dkneeds::keys::Bind<'a>]) -> dkneeds::keys::Table<'a> {
    use dkneeds::keys::{name_hash, Pair, Table};
    static PROVIDES: std::sync::OnceLock<[Pair; 1]> = std::sync::OnceLock::new();
    static EXPORTS: std::sync::OnceLock<[Pair; 1]> = std::sync::OnceLock::new();
    static HOOKS: std::sync::OnceLock<[Pair; 1]> = std::sync::OnceLock::new();
    Table {
        provides: PROVIDES.get_or_init(|| [Pair { a: name_hash(SVC), b: 0x000802 }]),
        exports: EXPORTS.get_or_init(|| [Pair { a: name_hash("dk_svctest_ping"), b: 8 }]),
        hooks: HOOKS.get_or_init(|| [Pair { a: name_hash("hook.a"), b: 4 }]),
        bind,
        ..Default::default()
    }
}

fn svc_bind(stock: &[u8]) -> Vec<dkneeds::keys::Bind<'_>> {
    vec![dkneeds::keys::Bind { site: dkneeds::keys::name_hash("hook.a"), addr: SITE, tramp: 16, stock }]
}

/// What `init` of that service declares: its hook (entry 4) and its export (entry 8).
fn svc_init() -> Init {
    use dkneeds::keys::name_hash;
    let items: Vec<(u16, u32, u32)> =
        vec![(dkrt_abi::service::Declare::HOOK, name_hash("hook.a"), 4), (dkrt_abi::service::Declare::EXPORT, name_hash("dk_svctest_ping"), 8)];
    Init::Declare(Box::leak(items.into_boxed_slice()))
}

/// A loader over a service file in the provider record `j`: the stock bytes are at the site.
unsafe fn provider_loader(file: Vec<u8>, init: Init) -> Fake {
    let l = loader_of(init, file);
    l.p.put_code(SITE, &STOCK);
    l
}

fn site_bytes(l: &Fake, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    unsafe { l.p.read_code(SITE, &mut out) };
    out
}

#[test]
fn a_provider_loads_into_its_own_record_publishes_its_exports_and_installs_its_hooks_last() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = provider_loader(service_file(&svc_table(&bind), 0), svc_init());
        assert_eq!(l.load_provider(PATH.as_ptr(), 1), OK);
        let rec = MAX_MODULES + 1;
        let base = records::module(rec).base;
        assert_eq!((records::module(rec).used, l.roles[rec], st().loads), (1, 1, 0), "a provider is no module of the count");
        assert_eq!(st().modules[0].used, 0, "no module record is taken");
        // the record is the host-visible DKRT_PROVIDERS slot 1: used, named, with the load base
        let published = &*core::ptr::addr_of!(records::DKRT_PROVIDERS);
        let name = &published[1].name;
        assert_eq!((published[1].used, published[1].base), (1, base));
        assert_ne!(name[0], 0, "the record carries the file's name");
        assert_eq!(published[0].used, 0);
        // the export resolves to base + 8; the site jumps to base + 16 (4ef9 and the address, big endian)
        assert_eq!(l.exports.resolve(dkneeds::keys::name_hash("dk_svctest_ping")), Some(base + 8));
        let t = base + 16;
        assert_eq!(site_bytes(&l, 6), [0x4e, 0xf9, (t >> 24) as u8, (t >> 16) as u8, (t >> 8) as u8, t as u8]);
        assert_eq!(l.installer.count(), 1);
        // the install is the last step: it comes after the init of the provider
        let init = l.m.log.iter().position(|e| matches!(e, Ev::Init(..))).unwrap();
        assert!(init < l.m.log.len());
        assert!(l.p.calls.borrow().contains(&crate::fake_platform::Call::Mask), "the install runs under the mask");
        // the table copy stays for the record
        assert!(l.provs[1].block.is_some());
    }
}

/// A `SLOTS` item may fill a `[[slot]]` word only: the same item naming an `[[info]]` data table refuses the file (`E_FORMAT`) and writes nothing.
#[test]
fn a_slot_fill_of_an_info_data_entry_is_refused_and_writes_nothing() {
    use dkneeds::keys::{name_hash, Slot, Table};
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    static SLOTS: std::sync::OnceLock<[Slot; 1]> = std::sync::OnceLock::new();
    unsafe {
        let slots = SLOTS.get_or_init(|| [Slot { id: name_hash("the.entry"), flags: 0, entry: 4 }]);
        for (data, want) in [(false, OK), (true, E_FORMAT)] {
            let bind = svc_bind(&STOCK);
            let table = Table { slots, ..svc_table(&bind) };
            let mut items = vec![(dkrt_abi::service::Declare::SLOT, name_hash("the.entry"), 4)];
            items.extend_from_slice(match svc_init() {
                Init::Declare(i) => i,
                _ => unreachable!(),
            });
            let mut l = provider_loader(service_file(&table, 0), Init::Declare(Box::leak(items.into_boxed_slice())));
            l.p.info_entries.borrow_mut().push((name_hash("the.entry"), vec![0]));
            if data {
                l.p.info_data.borrow_mut().push(name_hash("the.entry"));
            }
            assert_eq!(l.load_provider(PATH.as_ptr(), 1), want, "data entry: {data}");
            if data {
                assert_eq!(site_bytes(&l, 6), STOCK, "the site is stock");
                assert_eq!(l.installer.count(), 0, "nothing is installed");
                assert!(!l.p.calls.borrow().iter().any(|c| matches!(c, crate::fake_platform::Call::Word { .. })), "no slot word is written");
            }
        }
    }
}

#[test]
fn a_service_import_is_linked_for_a_service_file_only() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // the word at 12 holds 4, plus the import
        let bind = svc_bind(&STOCK);
        let base = || {
            let mut f = module_file_with(&[(12, 1 << 24)], &[b"dkc_device_id"]);
            f[IMAGE_OFF + 15] = 4;
            f
        };
        let mut svc = service_file_of(base(), &svc_table(&bind), 0);
        seal(&mut svc);
        let mut l = provider_loader(svc, svc_init());
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), OK);
        let words = core::slice::from_raw_parts(l.m.live[0].1.as_ptr() as *const u8, 32);
        assert_eq!(rd32(words.as_ptr().add(12)), SVC_ADDR + 4);
        // a module has no such import: the name is unknown to it, and nothing is allocated
        let mut f = base();
        seal(&mut f);
        let mut l = loader_of(Init::Ok, f);
        assert_eq!(l.load(PATH.as_ptr()), E_IMPORT);
        assert_eq!(l.m.allocs, 0);
        assert_eq!(l.missing()[0], 0);
    }
}

/// A service file of a table with a provides pair only (no hook, no export): `init` declares just its row.
fn row_service_file() -> Vec<u8> {
    use dkneeds::keys::{name_hash, Pair, Table};
    static PROVIDES: std::sync::OnceLock<[Pair; 1]> = std::sync::OnceLock::new();
    let table = Table { provides: PROVIDES.get_or_init(|| [Pair { a: name_hash(SVC), b: 0x000802 }]), ..Default::default() };
    service_file(&table, 0)
}

#[test]
fn the_row_a_service_file_declares_is_added_at_the_commit() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let rec = MAX_MODULES;
        let mut l = loader_of(Init::DeclaresRow, row_service_file());
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), OK);
        assert_eq!(l.r.log, vec![Ev::AddRow(rec, &TEST_ROW)]);
        // (a provider record has no run-time unload: its row goes with a failed load, below)
        // a module declares no row: `init` cannot, the import is not in its table
        let mut l = loader(Init::Ok);
        assert_eq!(l.load(PATH.as_ptr()), OK);
        assert_eq!(l.r.log, vec![Ev::Add(0)]);
    }
}

#[test]
fn a_row_the_registry_refuses_undoes_the_load() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let rec = MAX_MODULES;
        let mut l = loader_of(Init::DeclaresRow, row_service_file());
        l.r.add_result = E_PLAN_OWNER;
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_PLAN_OWNER);
        assert!(l.m.live.is_empty());
        assert_eq!(l.r.log, vec![Ev::AddRow(rec, &TEST_ROW), Ev::Remove(rec)]);
    }
}

#[test]
fn the_real_registry_puts_the_row_into_the_service_table_and_takes_it_out() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut r = RealRegistry;
        // the host tests of the runtime run with the machines row loaded (`svc_rows::test_machines`): the loaded rows start at one
        let fixed = crate::gen_services::STATIC + 1;
        assert_eq!(services().rows.len(), fixed);
        assert_eq!(r.add(0, None), OK);
        assert_eq!(r.add(2, Some(&TEST_ROW)), OK);
        let t = services();
        assert_eq!(t.rows.len(), fixed + 1);
        assert!(core::ptr::eq(t.rows[fixed], &TEST_ROW));
        // the in-crate prefix and its failed mask are as before, and one row has one owner
        assert_eq!(t.failed, services().failed);
        assert_eq!(r.add(3, Some(&TEST_ROW)), E_PLAN_OWNER);
        // (`RealRegistry::remove` also runs the registry's own remove, which the cleanup tests count: the row part alone here)
        crate::svc_rows::remove(3);
        assert_eq!(services().rows.len(), fixed + 1);
        crate::svc_rows::remove(2);
        assert_eq!(services().rows.len(), fixed);
    }
}

#[test]
fn a_hook_site_that_is_not_stock_refuses_the_provider_before_anything_is_allocated() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = provider_loader(service_file(&svc_table(&bind), 0), svc_init());
        l.p.put_code(SITE + 3, &[0x99]);
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_HOOK_STOCK);
        assert_eq!(l.m.allocs, 0, "the check runs before the image is placed");
        assert!(!l.m.log.iter().any(|e| matches!(e, Ev::Init(..))));
        assert_eq!(site_bytes(&l, 6), [0x20, 0x52, 0x48, 0x99, 0x00, 0x02]);
        assert_eq!((l.installer.count(), l.exports.count()), (0, 0));
    }
}

#[test]
fn a_provider_whose_init_fails_leaves_no_export_no_site_and_no_block() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = provider_loader(service_file(&svc_table(&bind), 0), Init::Fails(E_INIT));
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_INIT);
        assert!(l.m.live.is_empty(), "the image and the table copy are free");
        assert_eq!(records::module(MAX_MODULES).used, 0);
        assert_eq!((l.installer.count(), l.exports.count()), (0, 0));
        assert_eq!(site_bytes(&l, 6), STOCK);
        assert!(!l.p.calls.borrow().iter().any(|c| matches!(c, crate::fake_platform::Call::Write { .. } | crate::fake_platform::Call::Mask)), "no site was written");
    }
}

#[test]
fn a_declaration_that_differs_from_the_table_refuses_the_provider_and_the_site_stays_stock() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        // init declares nothing: the HOOKS and EXPORTS of the table are not repeated
        let mut l = provider_loader(service_file(&svc_table(&bind), 0), Init::Ok);
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_DECL_KIND);
        assert!(l.m.live.is_empty());
        assert_eq!((l.installer.count(), l.exports.count()), (0, 0));
        assert_eq!(site_bytes(&l, 6), STOCK);
    }
}

#[test]
fn a_provider_in_safe_mode_installs_nothing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = provider_loader(service_file(&svc_table(&bind), 0), svc_init());
        l.installer.set_armed(false);
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), OK);
        assert_eq!(site_bytes(&l, 6), STOCK);
        assert_eq!(l.installer.count(), 0);
    }
}

#[test]
fn a_provider_with_a_dsp_section_is_refused_until_the_dsp_part_of_a_service_exists() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = provider_loader(service_file(&svc_table(&bind), 64), svc_init());
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_FORMAT);
        assert_eq!(l.m.allocs, 0);
    }
}

#[test]
fn a_service_file_loads_only_as_a_provider_and_a_module_file_only_as_a_module() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let svc = service_file(&svc_table(&bind), 0);
        let mut l = provider_loader(svc.clone(), svc_init());
        assert_eq!(l.load(PATH.as_ptr()), E_FORMAT);
        assert_eq!(l.m.allocs, 0);
        let mut l = loader(Init::Ok);
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_FORMAT);
        // a provider whose ABI the registry refuses stops before anything is allocated; the module asks nothing
        let mut svc2 = svc;
        put(&mut svc2, dkneeds::H_SVC_ABI, 0xabcd_0123);
        seal(&mut svc2);
        let mut l = provider_loader(svc2, svc_init());
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), OK);
        assert_eq!(*l.r.asked.borrow(), vec![Ev::AbiOk(0xabcd_0123)]);
        let mut l = loader(Init::Ok);
        assert_eq!(l.load(PATH.as_ptr()), OK);
        assert!(l.r.asked.borrow().is_empty());
        let mut svc3 = service_file(&svc_table(&bind), 0);
        put(&mut svc3, dkneeds::H_SVC_ABI, 1);
        seal(&mut svc3);
        let mut l = provider_loader(svc3, svc_init());
        l.r.abi = false;
        assert_eq!(l.load_provider(PATH.as_ptr(), 0), E_SVC_ABI);
        assert_eq!(l.m.allocs, 0);
    }
}

#[test]
fn a_module_import_that_the_runtime_lacks_resolves_to_a_published_export_and_not_before() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // word at 12 holds 4 (plus the import)
        let mut file = module_file_with(&[(12, 1 << 24)], &[b"dk_svctest_ping"]);
        file[IMAGE_OFF + 15] = 4;
        seal(&mut file);
        let mut l = loader_of(Init::Ok, file);
        assert_eq!(l.load(PATH.as_ptr()), E_IMPORT, "no provider is loaded yet");
        l.exports.publish(MAX_MODULES, dkneeds::keys::name_hash("dk_svctest_ping"), 0x7000).unwrap();
        assert_eq!(l.load(PATH.as_ptr()), OK);
        let mem = &l.m.live.last().unwrap().1;
        let words = core::slice::from_raw_parts(mem.as_ptr() as *const u8, 32);
        assert_eq!(rd32(words.as_ptr().add(12)), 0x7000 + 4);
    }
}

/// A module file (role 0) whose header table is `table`, with the relocation `(12, import 0)` of the import `dk_svctest_ping`.
fn consumer_file(table: &dkneeds::keys::Table) -> Vec<u8> {
    let mut v = module_file_with(&[(12, 1 << 24)], &[b"dk_svctest_ping"]);
    let keys_off = v.len() - 8;
    v.truncate(keys_off);
    let mut t = vec![0u8; dkneeds::keys::table_size(table)];
    dkneeds::keys::write_table(table, &mut t).unwrap();
    v.extend_from_slice(&t);
    put(&mut v, dkneeds::H_REG_SIZE, t.len() as u32);
    v[IMAGE_OFF + 15] = 4;
    seal(&mut v);
    v
}

/// A capacity with room for everything the tests take.
fn wide_caps() -> Vec<DkrtCap> {
    use dkrt_sdk::proto::*;
    [CAPK_MODULES, CAPK_SELECTION, CAPK_MACHINE_ROWS, CAPK_PARAM_ROWS, CAPK_PAGE_ROWS, CAPK_ARENA]
        .iter()
        .map(|k| DkrtCap { kind: *k as u32, limit: 1 << 20, used: 0, largest: 1 << 20 })
        .collect()
}

const SVC_PATH: &[u8] = b"services/svc-test.dks";

/// A loader in the boot phase whose store holds the provider `svc` at `services/svc-test.dks` and whose selection is the one module `consumer` at `PATH`.
unsafe fn staged_boot(svc: Vec<u8>, consumer: Vec<u8>) -> Fake {
    let mut l = provider_loader(consumer.clone(), svc_init());
    let mut at = SVC_PATH.to_vec();
    l.m.files.insert(at.clone(), svc.clone());
    at.clear();
    l.reset_plan();
    l.capture(0, &consumer);
    l
}

fn user_table() -> dkneeds::keys::Table<'static> {
    static NEEDS: std::sync::OnceLock<[dkneeds::keys::Need; 1]> = std::sync::OnceLock::new();
    dkneeds::keys::Table {
        needs: NEEDS.get_or_init(|| [dkneeds::keys::Need { service: dkneeds::keys::name_hash(SVC), min_version: 0x000802, hard: true }]),
        ..Default::default()
    }
}

#[test]
fn the_boot_finds_the_provider_for_a_hard_need_loads_it_first_and_the_consumer_resolves_its_import() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let bind = svc_bind(&STOCK);
        let mut l = staged_boot(service_file(&svc_table(&bind), 0), consumer_file(&user_table()));
        let mut plan = l.plan_with_caps(1, &wide_caps());
        assert_eq!((plan.n, plan.np), (1, 1), "the search found the provider");
        assert_eq!(l.found.path(0), SVC_PATH);
        l.load_providers(&plan);
        assert_eq!(l.loaded[0], true);
        let rec = MAX_MODULES;
        assert_eq!(records::module(rec).used, 1);
        l.settle(&mut plan, 1);
        assert_eq!(plan.verdicts[0], dkrt_abi::plan::Verdict::Accepted);
        // the module stage: the import is the provider's export
        l.phase = Phase::Boot;
        assert_eq!(l.load(PATH.as_ptr()), OK);
        let mem = &l.m.live.last().unwrap().1;
        let words = core::slice::from_raw_parts(mem.as_ptr() as *const u8, 32);
        assert_eq!(rd32(words.as_ptr().add(12)), records::module(rec).base + 8 + 4);
    }
}

#[test]
fn no_search_without_an_unmet_need_and_a_failed_provider_refuses_its_dependents_with_the_hash() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // a selection with no need: no listing, no provider, the plan as before
        let mut l = staged_boot(service_file(&svc_table(&svc_bind(&STOCK)), 0), module_file());
        let allocs = l.m.allocs;
        let plan = l.plan_with_caps(1, &wide_caps());
        assert_eq!((plan.np, l.m.allocs), (0, allocs), "nothing was searched");
        // the provider's site is not stock: the provider fails, and its dependent is refused before the module stage
        let bind = svc_bind(&STOCK);
        let mut l = staged_boot(service_file(&svc_table(&bind), 0), consumer_file(&user_table()));
        l.p.put_code(SITE, &[0u8; 6]);
        let mut plan = l.plan_with_caps(1, &wide_caps());
        assert_eq!(plan.np, 1);
        l.load_providers(&plan);
        assert_eq!((l.loaded[0], records::module(MAX_MODULES).used), (false, 0));
        l.settle(&mut plan, 1);
        let hash = dkneeds::keys::name_hash(SVC);
        assert_eq!(plan.verdicts[0], dkrt_abi::plan::Verdict::Refused(dkrt_abi::plan::Reason::NeedMissing(hash)));
        assert_eq!(plan.verdicts[0].result(), E_MISSING);
        // the site stayed stock and the table holds no site
        assert_eq!(l.installer.count(), 0);
    }
}

#[test]
fn the_real_registry_accepts_zero_and_its_own_fingerprint_only() {
    let r = RealRegistry;
    assert!(r.abi_ok(0));
    assert!(r.abi_ok(dkrt_abi::service::SVC_ABI));
    assert!(!r.abi_ok(dkrt_abi::service::SVC_ABI ^ 1));
}

#[test]
fn the_role_word_picks_the_steps() {
    type Steps = RoleSteps<FakePlatform, FakeImages, FakeRegistry>;
    assert!(Steps::of(1).check as usize == Steps::SERVICE.check as usize);
    assert!(Steps::of(0).check as usize == Steps::MODULE.check as usize);
    // a word that is neither is a module, as the loader always took it
    assert!(Steps::of(7).check as usize == Steps::MODULE.check as usize);
}
