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
    Remove(usize),
}

/// What the init of the module does.
#[derive(Clone, Copy)]
enum Init {
    Ok,
    Fails(i32),
    /// Asks for the service `core/usb-bulk` and returns `E_MISSING`.
    NeedsUsbBulk,
}

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
    unsafe fn enter_init(&mut self, rec: usize, init: u32, _: *const Ctx) -> i32 {
        self.log.push(Ev::Init(rec, init));
        match self.init {
            Init::Ok => OK,
            Init::Fails(e) => e,
            Init::NeedsUsbBulk => {
                stub_missing("core/usb-bulk");
                E_MISSING
            }
        }
    }
    unsafe fn enter_fini(&mut self, rec: usize, fini: u32, _: *const Ctx) {
        self.log.push(Ev::Fini(rec, fini));
    }
}

struct FakeRegistry {
    add_result: i32,
    log: Vec<Ev>,
}

impl Registry for FakeRegistry {
    fn abi_ok(&self, _: u32) -> bool {
        true
    }
    unsafe fn add(&mut self, rec: usize, row: Option<&dkrt_abi::service::ServiceRow>) -> i32 {
        assert!(row.is_none());
        self.log.push(Ev::Add(rec));
        self.add_result
    }
    unsafe fn remove(&mut self, rec: usize) {
        self.log.push(Ev::Remove(rec));
    }
}

fn put(v: &mut [u8], off: u32, val: u32) {
    v[off as usize..off as usize + 4].copy_from_slice(&val.to_be_bytes());
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
    let n = v.len() as u32;
    put(&mut v, dkneeds::H_SIZE, n);
    let mut sha = Sha::new();
    let mut got = [0u8; 32];
    unsafe {
        sha.update(v.as_ptr(), dkneeds::H_SHA);
        sha.update_zeros(32);
        sha.update(v.as_ptr().add(dkneeds::H_SHA as usize + 32), n - (dkneeds::H_SHA + 32));
        sha.finish(got.as_mut_ptr());
    }
    v[dkneeds::H_SHA as usize..dkneeds::H_SHA as usize + 32].copy_from_slice(&got);
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
    *dsp::ds() = core::mem::zeroed();
    dsp::init_once();
    CALLED_NEED = [0; NEEDS_LEN];
    crate::cleanup_tests::lenient_tick_drop();
    let mut files = BTreeMap::new();
    files.insert(PATH[..PATH.len() - 1].to_vec(), file);
    let imgs = FakeImages { files, staged: Vec::new(), live: Vec::new(), allocs: 0, fail_alloc: None, init, log: Vec::new() };
    let reg = FakeRegistry { add_result: OK, log: Vec::new() };
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
        let n = file.len() as u32;
        // the sha covers the image: seal again
        let mut sha = Sha::new();
        let mut got = [0u8; 32];
        sha.update(file.as_ptr(), dkneeds::H_SHA);
        sha.update_zeros(32);
        sha.update(file.as_ptr().add(dkneeds::H_SHA as usize + 32), n - (dkneeds::H_SHA + 32));
        sha.finish(got.as_mut_ptr());
        file[dkneeds::H_SHA as usize..dkneeds::H_SHA as usize + 32].copy_from_slice(&got);
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
