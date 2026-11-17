//! Assembly tests (docs/plans/test-architecture.md T2 step 4): the seams between separately built artifacts, run on the host with the real packed outputs.
//!
//! Nothing here is a fake of an artifact. The files are the ones the build leaves in `dist/`: the packed fixtures (`dist/modules/<device>/*.dkm`), the service files
//! (`core/services/{fx,machines,lfo}`, `test/svc-test`) as the card holds them (the bound file of `dist/svc-bound/<device>/` where the service has hooks), the committed
//! runtime object (`modules/core/runtime/build/runtime.dko.json`, the immediate of its ABI check) and the info table of the patched image
//! (`dist/bytes/images/<device>/set-<set>.info`, written by `patchkit:device-images` from the plan of `patchkit apply`). They go through the code of the boot:
//! the search for providers on the card, the plan (needs, owners of kinds, capacity limits from the `cap_<kind>` entries), the role checks of a provider file (ABI word, stock
//! bytes of every hook site), the install of its hooks and slots over `FakePlatform`, and the load of a module without a DSP section.
//!
//! What the host cannot show stays out: no ColdFire code runs (in the loader cases the `init` of a service replays what its header table declares; one case runs the glue of each Rust service file natively and compares), and the DSP part of a module is
//! not loaded (its DSP loader needs the DSP). One case per seam shows that it fits and one the refusal that is typical for it. The edge cases of each part are unit
//! tests of that part; an assertion that needs a hand-built table or a changed rule belongs there, not here.
//!
//! The tests need the outputs of the Nx target `core/runtime:test-assembly` (its `dependsOn`). A device without its firmware image is skipped, as its device tests are.
use super::*;
use crate::fake_platform::{Call, FakePlatform};
use crate::test_lock::LOCK;
use dkneeds::keys::{name_hash, tag, Keys};
use dkrt_abi::plan::{Reason, Verdict};
use dkrt_abi::profiles;
use dkrt_abi::service::{Declare, Service, ServiceRow};
use dkrt_sdk::proto::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

#[derive(Clone, Copy)]
struct Dev {
    name: &'static str,
    profile: dkrt_abi::platform::DeviceProfile,
    /// The stock firmware the images and the bound files are made from (the acceptance cells).
    firmware: &'static str,
}

const DT2: Dev = Dev { name: "dt2", profile: profiles::DT2, firmware: "firmware/Digitakt_II_OS1.17.syx" };
const DN2: Dev = Dev { name: "dn2", profile: profiles::DN2, firmware: "firmware/Digitone_II_OS1.12.syx" };
const DEVICES: [Dev; 2] = [DT2, DN2];

impl Dev {
    /// False (with a note) when the firmware is not here: its images and bound files are not made, and nothing of this device can be assembled.
    fn has_firmware(&self) -> bool {
        let ok = repo().join(self.firmware).exists();
        if !ok {
            eprintln!("SKIP {}: {} is not here", self.name, self.firmware);
        }
        ok
    }
}

fn read(path: &Path, make: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}; run `{make}`", path.display()))
}

// ---- the artifacts ------------------------------------------------------------------------------------------------------------

/// The packed file of module `id`'s leaf name: a `.dkm`, or a `.dks` for a service.
fn packed(dev: Dev, leaf: &str, ext: &str, nx: &str) -> Vec<u8> {
    read(&repo().join(format!("dist/modules/{}/{leaf}.{ext}", dev.name)), &format!("pnpm nx:pilot run {nx}:pack"))
}

/// A service file as the card holds it: the bound file when the service has hooks (`<id>:bind`), else the packed one. A packed file with hooks and no bound file
/// is refused here: it carries the hooks without their trampolines and stock bytes.
fn service_file(dev: Dev, leaf: &str, nx: &str) -> Vec<u8> {
    let bound = repo().join(format!("dist/svc-bound/{}/{leaf}.dks", dev.name));
    if bound.exists() {
        return std::fs::read(bound).unwrap();
    }
    let f = packed(dev, leaf, "dks", nx);
    let table = table_of(&f);
    let k = Keys::parse(&table).unwrap();
    assert!(k.count(tag::HOOKS) == 0, "{leaf}.dks has hooks and no bound file: run `pnpm nx:pilot run {nx}:bind`");
    f
}

fn be(f: &[u8], at: u32) -> u32 {
    u32::from_be_bytes(f[at as usize..at as usize + 4].try_into().unwrap())
}

/// The header table of a file.
fn table_of(f: &[u8]) -> Vec<u8> {
    let (off, len) = (be(f, dkneeds::H_REG_OFF) as usize, be(f, dkneeds::H_REG_SIZE) as usize);
    f[off..off + len].to_vec()
}

/// The `"hex"` strings of an object file (`*.dko.json`), in file order.
fn hex_sections(json: &str) -> Vec<&str> {
    json.split("\"hex\": \"").skip(1).map(|s| s.split('"').next().unwrap()).collect()
}

/// The ABI word the object of a service carries (`dk_svc_abi`, an 8-digit hex string in the `.rodata.dk_svc_abi` section).
fn object_abi(json: &str) -> &str {
    let at = json.find("\".rodata.dk_svc_abi\"").expect("no dk_svc_abi section");
    let from = json[..at].rfind("\"hex\": \"").expect("no hex before dk_svc_abi") + 8;
    json[from..].split('"').next().unwrap()
}

/// The committed runtime object (its sections, joined): `cmpi.l #SVC_ABI,d0` of `abi_ok` is `0c80` and the word. A host build of the runtime has another `SVC_ABI`
/// (64-bit sizes), so the runtime of the device is the object.
fn runtime_object() -> String {
    let json = read(&repo().join("modules/core/runtime/build/runtime.dko.json"), "patchkit build modules/core/runtime");
    hex_sections(std::str::from_utf8(&json).unwrap()).join("|")
}

/// The registry of the device: the ABI check of the runtime object, the rows of the runtime's own registry.
struct DeviceRegistry {
    runtime: String,
}

impl Registry for DeviceRegistry {
    fn abi_ok(&self, fingerprint: u32) -> bool {
        fingerprint == 0 || self.runtime.contains(&std::format!("0c80{fingerprint:08x}"))
    }
    unsafe fn add(&mut self, rec: usize, row: Option<&ServiceRow>) -> i32 {
        RealRegistry.add(rec, row)
    }
    unsafe fn remove(&mut self, rec: usize) {
        RealRegistry.remove(rec)
    }
}

/// The info table of a patched image (`set-<set>.info`): name, hash, kind (0 data, 1 slot), address and the words of a data entry.
struct Image {
    entries: Vec<(String, u32, u32, Vec<u32>)>,
}

impl Image {
    fn read(dev: Dev, set: &str) -> Image {
        let path = repo().join(format!("dist/bytes/images/{}/set-{set}.info", dev.name));
        let text = String::from_utf8(read(&path, "pnpm nx:pilot run patchkit:device-images")).unwrap();
        let hex = |s: &str| u32::from_str_radix(s, 16).unwrap();
        let entries = text
            .lines()
            .map(|l| {
                let w: Vec<&str> = l.split(' ').collect();
                (w[0].to_string(), hex(w[1]), w[2].parse().unwrap(), w[4..].iter().map(|x| hex(x)).collect())
            })
            .collect();
        Image { entries }
    }

    /// The platform of the device with this image's table. `limits`: replacement values of the `cap_<kind>` words.
    fn platform(&self, dev: Dev, limits: &[(u8, u32)]) -> FakePlatform {
        let p = FakePlatform::new(dev.profile);
        for (_, hash, kind, words) in &self.entries {
            let mut words = if words.is_empty() { std::vec![0] } else { words.clone() };
            if let Some((_, v)) = limits.iter().find(|(k, _)| *hash == cap_hash(*k)) {
                words[0] = *v;
            }
            p.info_entries.borrow_mut().push((*hash, words));
            if *kind == 0 {
                p.info_data.borrow_mut().push(*hash);
            }
        }
        p
    }

    fn has(&self, hash: u32) -> bool {
        self.entries.iter().any(|e| e.1 == hash && !e.3.is_empty())
    }
}

fn cap_hash(kind: u8) -> u32 {
    CAP_INFO_HASHES[CAP_KINDS.iter().position(|k| *k == kind).unwrap()]
}

/// The capacity before anything loads, as `dkrt_capacity` builds it: the limits of the kinds the runtime counts itself are as large as they can be here (their
/// unit tests own them), the rest come from the image's `cap_<kind>` entries and the rows of the services loaded so far.
unsafe fn capacity(p: &FakePlatform) -> Vec<DkrtCap> {
    let table = crate::handlers::services();
    (0..CAP_KINDS.len())
        .map(|k| {
            let kind = CAP_KINDS[k];
            let big = 1 << 20;
            let base = if kind == CAPK_MODULES {
                (kind, MAX_MODULES as u32, 0, 0)
            } else if kind == CAPK_SELECTION {
                (kind, SEL_MAX as u32, 0, SEL_MAX as u32)
            } else if kind == CAPK_DSP_COST {
                (kind, p.profile.dsp_headroom, 0, 0)
            } else if kind == CAPK_DSP_VOICES {
                (kind, p.profile.dsp_voices, 0, 0)
            } else if [CAPK_ARENA, CAPK_STAGE, CAPK_DSP_AREA, CAPK_STORE, CAPK_SETTINGS].contains(&kind) {
                (kind, big, 0, big)
            } else {
                (kind, 0, 0, 0)
            };
            let v = crate::report::kind_value(p, &table, k, base);
            DkrtCap { kind: kind as u32, limit: v.1, used: v.2, largest: v.3 }
        })
        .collect()
}

// ---- the loader over a card ---------------------------------------------------------------------------------------------------

struct Nothing;
impl<C: dkrt_abi::core_api::CoreApi> Service<C> for Nothing {}

/// The store and the arena of a card: every import resolves against the runtime's own tables (`RealImages`); `init` of a service file replays what its header
/// table declares (no ColdFire code runs on the host).
struct Card {
    files: Vec<(Vec<u8>, Vec<u8>)>,
    staged: Vec<u8>,
    staged_path: Vec<u8>,
    live: Vec<(u32, Vec<u128>)>,
    allocs: u32,
}

impl Card {
    fn new(files: Vec<(String, Vec<u8>)>) -> Card {
        Card {
            files: files.into_iter().map(|(n, b)| (n.into_bytes(), b)).collect(),
            staged: Vec::new(),
            staged_path: Vec::new(),
            live: Vec::new(),
            allocs: 0,
        }
    }
}

impl Images for Card {
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32> {
        let key = core::slice::from_raw_parts(path, cstr_len(path, 64)).to_vec();
        match self.files.iter().find(|(n, _)| *n == key) {
            None => Err(E_NOT_FOUND),
            Some((_, bytes)) => {
                self.staged = bytes.clone();
                self.staged_path = key;
                Ok(Staged { ptr: self.staged.as_ptr(), size: self.staged.len() as u32 })
            }
        }
    }
    unsafe fn alloc(&mut self, n: u32) -> Option<Block> {
        self.allocs += 1;
        let addr = 0x0010_0000 * self.allocs;
        let mut mem = std::vec![0u128; (n as usize + 15) / 16];
        let ptr = mem.as_mut_ptr() as *mut u8;
        self.live.push((addr, mem));
        Some(Block { ptr, addr })
    }
    unsafe fn free(&mut self, addr: u32) {
        let at = self.live.iter().position(|b| b.0 == addr).expect("free of a block that is not live");
        self.live.remove(at);
    }
    unsafe fn import_addr(&mut self, name: *const u8) -> u32 {
        // The device table `IMPORTS` points at firmware symbols that a host test binary does not link: a name the SDK declares gets a stand-in address.
        let n = core::slice::from_raw_parts(name, cstr_len(name, 64));
        if dkrt_sdk::api::BUILTIN_NAMES.iter().any(|b| b.as_bytes() == n) {
            0x1000
        } else {
            0
        }
    }
    unsafe fn svc_import_addr(&mut self, name: *const u8) -> u32 {
        RealImages.svc_import_addr(name)
    }
    unsafe fn import_needs(&mut self, name: *const u8) -> Option<&'static str> {
        let _ = name;
        None
    }
    unsafe fn list(&mut self, prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
        let prefix = core::slice::from_raw_parts(prefix, cstr_len(prefix, 64)).to_vec();
        let mut k = 0;
        for (name, bytes) in &self.files {
            if k < max as usize && name.starts_with(&prefix) {
                let e = &mut *out.add(k);
                e.path = [0; 64];
                e.path[..name.len()].copy_from_slice(name);
                e.size = bytes.len() as u32;
                k += 1;
            }
        }
        k as i32
    }
    unsafe fn enter_init(&mut self, rec: usize, init: u32, _: *const Ctx) -> i32 {
        let f = &self.staged;
        if be(f, dkneeds::H_ROLE) != 1 {
            return OK; // a module's `init` is module code
        }
        let table = table_of(f);
        let keys = crate::collect::KeysCopy::from_bytes(&table).unwrap();
        let base = init - be(f, dkneeds::H_INIT);
        let at = |off: u32| (base + off) as usize as *const u8;
        let ctx = core::ptr::addr_of_mut!(crate::entry::DKRT_CTXS[rec]) as *mut u8;
        let mut kinds: Vec<(u32, u32)> = Vec::new();
        for i in 0..keys.decl.n {
            let d = keys.decl.items[i];
            let r = if d.tag == Declare::KIND {
                kinds.push((d.a, d.b));
                crate::collect::dk_service_declare_kind(ctx, d.a, d.b, at(d.c))
            } else if d.tag == Declare::SLOT {
                crate::collect::dk_service_declare_slot(ctx, d.a, d.b, at(d.c))
            } else if d.tag == Declare::HOOK {
                crate::collect::dk_service_declare_hook(ctx, d.a, at(d.b))
            } else if d.tag == Declare::EXPORT {
                crate::collect::dk_service_declare_export(ctx, d.a, at(d.b))
            } else {
                crate::collect::dk_service_declare_resource(ctx, d.a, d.b)
            };
            assert_eq!(r, OK);
        }
        // the row: the name of the file, the kinds of its table
        let stem = String::from_utf8(self.staged_path.clone()).unwrap();
        let stem = stem.trim_start_matches("services/").trim_end_matches(".dks").to_string();
        let name: &'static str = Box::leak(stem.into_boxed_str());
        let kinds: &'static [(u32, u32)] = Box::leak(kinds.into_boxed_slice());
        let row: &'static ServiceRow = Box::leak(Box::new(ServiceRow::of::<Nothing, crate::core_api::RealCore>(name, kinds)));
        crate::collect::dk_service_declare_row(ctx, row)
    }
    unsafe fn enter_fini(&mut self, _: usize, _: u32, _: *const Ctx) {}
}

type Boot = Loader<FakePlatform, Card, DeviceRegistry>;

/// The runtime tables of the host tests as a device has them before the boot loads anything: no loaded service (the other tests of the runtime run with the
/// machines row of `svc_rows::test_machines`; it is put back when the guard goes), no module, no DSP load.
struct Clean(std::sync::MutexGuard<'static, ()>);

fn clean() -> Clean {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let _ = crate::handlers::services(); // the first call adds the test row
        for rec in MAX_MODULES..MAX_MODULES + PROVIDER_MAX {
            crate::svc_rows::remove(rec);
        }
        *st() = core::mem::zeroed();
        records::reset();
        *dsp::ds() = core::mem::zeroed();
        dsp::init_once();
        CALLED_NEED = [0; NEEDS_LEN];
    }
    Clean(g)
}

impl Drop for Clean {
    fn drop(&mut self) {
        unsafe {
            for rec in MAX_MODULES..MAX_MODULES + PROVIDER_MAX {
                crate::svc_rows::remove(rec);
            }
            crate::svc_rows::add(MAX_MODULES + PROVIDER_MAX - 1, crate::handlers::exports::machines_row());
        }
    }
}

/// Where a fixture's source module is built (`<project id>:pack`) and its file name.
struct Fixture {
    leaf: &'static str,
    nx: &'static str,
}

const SINE: Fixture = Fixture { leaf: "sine", nx: "test/sine" };
const DYN_DN2: Fixture = Fixture { leaf: "dyn-dn2", nx: "test/dyn-dn2" };
const FX_GAIN: Fixture = Fixture { leaf: "dyn-fx-gain", nx: "test/dyn-fx-gain" };
const LFO_SHAPE: Fixture = Fixture { leaf: "lfo-shape", nx: "test/lfo-shape" };
const SVC_USER: Fixture = Fixture { leaf: "svc-test-user", nx: "test/svc-test-user" };

/// The service files of the card: the three services of the runtime and the test service.
const SERVICES: [Fixture; 4] = [
    Fixture { leaf: "fx", nx: "core/services/fx" },
    Fixture { leaf: "machines", nx: "core/services/machines" },
    Fixture { leaf: "lfo", nx: "core/services/lfo" },
    Fixture { leaf: "svc-test", nx: "test/svc-test" },
];

fn card_of(dev: Dev, modules: &[&Fixture], services: &[Fixture]) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> =
        modules.iter().map(|m| (std::format!("modules/{}.dkm", m.leaf), packed(dev, m.leaf, "dkm", m.nx))).collect();
    for s in services {
        files.push((std::format!("services/{}.dks", s.leaf), service_file(dev, s.leaf, s.nx)));
    }
    files
}

/// The plan of a boot of `modules` from a card with `services`, over the platform of `image`: the loader with every module captured and the plan made. The
/// providers are not loaded yet.
unsafe fn planned(dev: Dev, image: &Image, limits: &[(u8, u32)], modules: &[&Fixture], services: &[Fixture]) -> (Boot, Plan) {
    let files = card_of(dev, modules, services);
    let p = image.platform(dev, limits);
    // the stock bytes of every hook site of a service file are live in the firmware (the bind compared them with it)
    for (_, f) in files.iter().filter(|(n, _)| n.starts_with("services/")) {
        let t = table_of(f);
        let keys = Keys::parse(&t).unwrap();
        let mut i = 0;
        while let Some(b) = keys.bind(i) {
            p.put_code(b.addr, &b.stock[..b.len as usize]);
            i += 1;
        }
    }
    let mut l = Loader::new(p, Card::new(files), DeviceRegistry { runtime: runtime_object() });
    l.reset_plan();
    for (k, m) in modules.iter().enumerate() {
        l.capture(k, &packed(dev, m.leaf, "dkm", m.nx));
    }
    let caps = capacity(&l.p);
    let plan = l.plan_with_caps(modules.len(), &caps);
    (l, plan)
}

/// The store paths of the providers the plan found.
fn found(l: &Boot, plan: &Plan) -> BTreeSet<String> {
    (0..plan.np).map(|j| String::from_utf8(l.found.path(j).to_vec()).unwrap()).collect()
}

/// The `BIND` items (site name hash, address) of the provider files that loaded, from the files themselves.
fn binds_of(l: &Boot, plan: &Plan) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for j in 0..plan.np {
        if l.loaded[j] {
            let path = String::from_utf8(l.found.path(j).to_vec()).unwrap();
            let (_, file) = l.m.files.iter().find(|(n, _)| *n == path.as_bytes()).unwrap();
            let t = table_of(file);
            let keys = Keys::parse(&t).unwrap();
            let mut i = 0;
            while let Some(b) = keys.bind(i) {
                out.push((b.site, b.addr));
                i += 1;
            }
        }
    }
    out
}

/// Every name the file imports resolves: against the runtime's tables (and the service-only ones for a service file) or an export of a loaded provider. The loader
/// does the same at a load; a module with a DSP section is not loaded on the host, so its imports are looked at here.
unsafe fn imports_resolve(l: &mut Boot, file: &[u8], what: &str) {
    let role = be(file, dkneeds::H_ROLE);
    let (off, n) = (be(file, dkneeds::H_IMPORT_OFF) as usize, be(file, dkneeds::H_IMPORT_N) as usize);
    for i in 0..n {
        let at = off + i * dkneeds::CF_IMPORT_LEN as usize;
        let name = &file[at..at + dkneeds::CF_IMPORT_LEN as usize];
        let len = name.iter().position(|c| *c == 0).expect("an import name without NUL");
        let mut a = l.m.import_addr(name.as_ptr());
        if a == 0 && role == 1 {
            a = l.m.svc_import_addr(name.as_ptr());
        }
        if a == 0 {
            a = l.exports.resolve(name_hash(std::str::from_utf8(&name[..len]).unwrap())).unwrap_or(0);
        }
        assert_ne!(a, 0, "{what} imports {}, which the runtime and the loaded services do not have", String::from_utf8_lossy(&name[..len]));
    }
}

/// The seam of a fixture with its provider: the plan accepts the module and finds `want` on the card, the providers load (the ABI word, the stock bytes of every
/// hook site), their hooks and slots are installed as their bind records say, and a module without a DSP section loads.
unsafe fn fits(dev: Dev, set: &str, modules: &[&Fixture], want: &[&str]) {
    let image = Image::read(dev, set);
    let (mut l, mut plan) = planned(dev, &image, &[], modules, &SERVICES);
    let what = std::format!("{} {set} {:?}", dev.name, modules.iter().map(|m| m.leaf).collect::<Vec<_>>());
    for k in 0..modules.len() {
        assert_eq!(plan.verdicts[k], Verdict::Accepted, "{what}: {}", modules[k].leaf);
    }
    let got = found(&l, &plan);
    for w in want {
        assert!(got.contains(&std::format!("services/{w}.dks")), "{what}: the search found {got:?}, not {w}");
    }
    for j in 0..plan.np {
        assert_eq!(plan.provider_verdicts[j], Verdict::Accepted, "{what}: provider {j}");
    }
    l.load_providers(&plan);
    for j in 0..plan.np {
        assert!(l.loaded[j], "{what}: provider {} did not load", String::from_utf8_lossy(l.found.path(j)));
    }
    l.settle(&mut plan, modules.len());
    for k in 0..modules.len() {
        assert_eq!(plan.verdicts[k], Verdict::Accepted, "{what}: {} after the providers loaded", modules[k].leaf);
    }
    // the rows the files declared are in the table of the services
    let rows: Vec<&str> = crate::svc_rows::rows().iter().map(|r| unsafe { (**r).name() }).collect();
    for w in want {
        assert!(rows.contains(w), "{what}: no row {w} in {rows:?}");
    }
    // the hook sites of the files: one `jmp` written for each bind record, at its address, under one mask
    let binds = binds_of(&l, &plan);
    assert_eq!(l.installer.count(), binds.len(), "{what}: installed sites");
    let written: Vec<u32> = l.p.calls.borrow().iter().filter_map(|c| if let Call::Write { addr, .. } = c { Some(*addr) } else { None }).collect();
    let mut sites: Vec<u32> = binds.iter().map(|b| b.1).collect();
    sites.sort();
    let mut w2 = written.clone();
    w2.sort();
    assert_eq!(w2, sites, "{what}: the sites written are the bind records'");
    // each module: its imports resolve, and without a DSP section it loads
    for (k, m) in modules.iter().enumerate() {
        let file = packed(dev, m.leaf, "dkm", m.nx);
        imports_resolve(&mut l, &file, m.leaf);
        if be(&file, dkneeds::H_DSP_SIZE) == 0 {
            l.phase = Phase::Boot;
            let path = std::format!("modules/{}.dkm\0", m.leaf);
            let r = l.load(path.as_ptr());
            assert_eq!(r, OK, "{what}: load of {} (module {k})", m.leaf);
        }
    }
}

// ---- the cases ------------------------------------------------------------------------------------------------------------

#[test]
fn a_machine_loads_with_the_machines_service_file() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        unsafe {
            fits(*dev, "emulator", &[&SINE], &["machines"]);
        }
        let _ = clean_state();
        if dev.name == "dn2" {
            unsafe {
                fits(*dev, "emulator", &[&DYN_DN2], &["machines"]);
            }
        }
        let _ = clean_state();
    }
}

#[test]
fn an_effect_loads_with_the_fx_service_file() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        unsafe {
            fits(*dev, "emulator", &[&FX_GAIN], &["fx"]);
        }
        let _ = clean_state();
    }
}

#[test]
fn an_lfo_loads_with_the_lfo_service_file() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        unsafe {
            fits(*dev, "emulator", &[&LFO_SHAPE], &["lfo"]);
        }
        let _ = clean_state();
    }
}

#[test]
fn a_c_service_with_the_abi_word_0_is_accepted_and_its_user_loads() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        unsafe {
            fits(*dev, "emulator", &[&SVC_USER], &["svc-test"]);
        }
        let _ = clean_state();
    }
}

/// All services of a card at once, on both images of the device: the hook sites of the files do not overlap, the rows are all there, and the capacity is the image's.
#[test]
fn every_service_of_the_card_loads_together_on_the_emulator_and_the_device_image() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        for set in ["emulator", "device"] {
            let mut modules: Vec<&Fixture> = std::vec![&SINE, &FX_GAIN, &LFO_SHAPE, &SVC_USER];
            if dev.name == "dn2" {
                modules.push(&DYN_DN2);
            }
            unsafe {
                fits(*dev, set, &modules, &["machines", "fx", "lfo", "svc-test"]);
            }
            let _ = clean_state();
        }
    }
}

/// The capacity limits come from the image's `cap_<kind>` entries before any service file loads: the entries are in the image, and a module is judged by them.
#[test]
fn the_image_gives_the_limits_the_plan_judges_by() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        let image = Image::read(*dev, "emulator");
        for kind in [CAPK_MACHINE_ROWS, CAPK_PARAM_ROWS, CAPK_PAGE_ROWS, CAPK_FX_SLOTS] {
            assert!(image.has(cap_hash(kind)), "{}: the image has no cap_ entry with a word for capacity kind {kind}", dev.name);
        }
        // a limit under the uses of the module refuses it, naming the resource
        unsafe {
            let (_l, plan) = planned(*dev, &image, &[(CAPK_MACHINE_ROWS, 0)], &[&SINE], &SERVICES);
            assert_eq!(plan.verdicts[0], Verdict::Refused(Reason::Fit(CAPK_MACHINE_ROWS)), "{}", dev.name);
            assert_eq!(plan.verdicts[0].result(), E_PLAN_FIT);
        }
        let _ = clean_state();
    }
}

#[test]
fn a_card_without_the_service_file_refuses_its_user_naming_the_service() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        let image = Image::read(*dev, "emulator");
        unsafe {
            let (_l, plan) = planned(*dev, &image, &[], &[&SINE], &SERVICES[..1]); // fx only
            assert_eq!(plan.verdicts[0], Verdict::Refused(Reason::NeedMissing(name_hash("machines"))), "{}", dev.name);
            assert_eq!(plan.verdicts[0].result(), E_MISSING);
        }
        let _ = clean_state();
    }
}

/// A service file built for another ABI is refused by its own load; one whose hook site is not stock changes nothing.
#[test]
fn a_provider_with_another_abi_word_or_a_site_that_is_not_stock_is_refused_before_anything_is_written() {
    let _g = clean();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        let image = Image::read(*dev, "emulator");
        unsafe {
            // the word of the file is not the one of the runtime object
            let (mut l, plan) = planned(*dev, &image, &[], &[&FX_GAIN], &SERVICES);
            let j = (0..plan.np).find(|j| l.found.path(*j) == b"services/fx.dks").expect("fx.dks is found");
            let at = l.m.files.iter().position(|(n, _)| n == b"services/fx.dks").unwrap();
            let word = be(&l.m.files[at].1, dkneeds::H_SVC_ABI);
            assert_ne!(word, 0, "fx.dks has an ABI word");
            put(&mut l.m.files[at].1, dkneeds::H_SVC_ABI, word ^ 1);
            seal(&mut l.m.files[at].1);
            l.phase = Phase::Boot;
            let path = "services/fx.dks\0";
            assert_eq!(l.load_provider(path.as_ptr(), j), E_SVC_ABI, "{}", dev.name);
            assert!(l.m.live.is_empty(), "nothing is allocated for a refused file");
        }
        let _ = clean_state();
        unsafe {
            // a site whose live bytes are not the stock bytes of the bind record
            let (mut l, plan) = planned(*dev, &image, &[], &[&FX_GAIN], &SERVICES);
            let j = (0..plan.np).find(|j| l.found.path(*j) == b"services/fx.dks").unwrap();
            let file = l.m.files.iter().find(|(n, _)| n == b"services/fx.dks").unwrap().1.clone();
            let t = table_of(&file);
            let keys = Keys::parse(&t).unwrap();
            let b = keys.bind(0).expect("fx.dks is bound");
            l.p.put_code(b.addr, &[0xff]);
            l.phase = Phase::Boot;
            assert_eq!(l.load_provider(b"services/fx.dks\0".as_ptr(), j), E_HOOK_STOCK, "{}", dev.name);
            assert!(!l.p.calls.borrow().iter().any(|c| matches!(c, Call::Write { .. } | Call::Word { .. })), "no site is written");
            assert_eq!(l.installer.count(), 0);
        }
        let _ = clean_state();
    }
}

// ---- consistency of the generated artifacts (no firmware needed) ----------------------------------------------------------------

/// The committed objects of the service files carry the ABI word the runtime object compares with (`dk_svc_abi` against the immediate of `abi_ok`), and the packed
/// file of each device has it in header word 0x3c. A source change that moves `SVC_ABI` leaves a service object stale: the loader then refuses every service file.
#[test]
fn the_service_objects_and_files_carry_the_abi_word_of_the_runtime_object() {
    let runtime = runtime_object();
    for s in SERVICES.iter().take(3) {
        let json = read(&repo().join(format!("modules/core/services/{}/dks/build/{}.dko.json", s.leaf, s.leaf)), "patchkit build modules/core");
        let abi = object_abi(std::str::from_utf8(&json).unwrap()).to_string();
        assert!(
            runtime.contains(&std::format!("0c80{abi}")),
            "{}.dko.json carries ABI word {abi}, the runtime object another: patchkit build modules/core/services/{}/dks",
            s.leaf,
            s.leaf
        );
        for dev in DEVICES {
            let file = packed(dev, s.leaf, "dks", s.nx);
            assert_eq!(std::format!("{:08x}", be(&file, dkneeds::H_SVC_ABI)), abi, "{} {}: the packed file has another ABI word", dev.name, s.leaf);
        }
    }
}

/// The declarations of the real `init` of a service file (its glue, `dk_svc_declare` of `service!`, run natively over the runtime's own `dk_service_declare_*`) against the
/// header table of the file as the card holds it (the bound file) for the device: the same kinds with their lengths, slots with their flags, hooks, exports and resources. The loader makes the same
/// comparison at the commit (`decl_matches`) and refuses a difference with `E_DECL_KIND`; the other cases here replay the table and cannot see one. The entry addresses are not
/// compared: a host function has no offset in the file (an entry is checked by its presence and its name hash).
#[test]
fn the_init_of_each_service_file_declares_what_its_header_table_carries() {
    fn dt2() -> u32 {
        profiles::DT2.id
    }
    fn dn2() -> u32 {
        profiles::DN2.id
    }
    // (tag, a, b) with the address words left out
    fn plain(tag: u16, a: u32, b: u32) -> (u16, u32, u32) {
        if tag == Declare::HOOK || tag == Declare::EXPORT {
            (tag, a, 0)
        } else {
            (tag, a, b)
        }
    }
    fn name(tag: u16) -> &'static str {
        match tag {
            t if t == Declare::KIND => "KINDS",
            t if t == Declare::SLOT => "SLOTS",
            t if t == Declare::HOOK => "HOOKS",
            t if t == Declare::EXPORT => "EXPORTS",
            _ => "RESOURCES",
        }
    }
    let _g = clean();
    let services: Vec<(&Fixture, crate::handlers::dks_decls::DeclareFn)> = crate::handlers::dks_decls::declarations()
        .iter()
        .map(|(leaf, declare)| (SERVICES.iter().find(|s| s.leaf == *leaf).unwrap(), *declare))
        .collect();
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        for (s, declare) in &services {
            // the bound file: the packed one carries the hooks of both devices, `bind` keeps those of the device
            let file = service_file(*dev, s.leaf, s.nx);
            let header = crate::collect::KeysCopy::from_bytes(&table_of(&file)).unwrap().decl;
            let mut want: Vec<(u16, u32, u32)> = (0..header.n).map(|i| plain(header.items[i].tag, header.items[i].a, header.items[i].b)).collect();
            let mut got: Vec<(u16, u32, u32)> = Vec::new();
            unsafe {
                let mut col = std::boxed::Box::new(crate::collect::Collect::new());
                crate::collect::begin(&mut *col);
                let ctx = crate::entry::bind(0) as *mut u8;
                let r = declare(ctx, if dev.name == "dt2" { dt2 } else { dn2 });
                crate::collect::end();
                crate::entry::unbind(0);
                assert_eq!(r, OK, "{} {}: init refused its own declarations", dev.name, s.leaf);
                for i in 0..col.decl.n {
                    let d = col.decl.items[i];
                    if d.tag != Declare::ROW {
                        got.push(plain(d.tag, d.a, d.b));
                    }
                }
            }
            got.sort();
            want.sort();
            let show = |v: &[(u16, u32, u32)], other: &[(u16, u32, u32)]| {
                v.iter().filter(|x| !other.contains(x)).map(|x| std::format!("{} {:08x}", name(x.0), x.1)).collect::<Vec<_>>()
            };
            assert!(
                got == want,
                "{} {}: init declares what the header table does not carry {:?}; the table carries what init does not declare {:?}",
                dev.name,
                s.leaf,
                show(&got, &want),
                show(&want, &got)
            );
        }
    }
}

/// The hook words of a bound file have no device mask left (bind writes plain offsets, which the runtime compares with what `init` declares).
#[test]
fn a_bound_service_file_has_plain_hook_offsets() {
    for dev in DEVICES.iter().filter(|d| d.has_firmware()) {
        for s in &SERVICES {
            let file = service_file(*dev, s.leaf, s.nx);
            let t = table_of(&file);
            let keys = Keys::parse(&t).unwrap();
            for i in 0..keys.count(tag::HOOKS) {
                let w = keys.words(tag::HOOKS, i).unwrap();
                assert_eq!(w.1 >> 24, 0, "{} {}: hook {i} still has a device mask", dev.name, s.leaf);
            }
            assert_eq!(keys.bind_count(), keys.count(tag::HOOKS), "{} {}: a hook without its bind record", dev.name, s.leaf);
        }
    }
}

/// Between the cases the loader tables are reset (the guard of the test holds the lock for all of them).
fn clean_state() {
    unsafe {
        for rec in MAX_MODULES..MAX_MODULES + PROVIDER_MAX {
            crate::svc_rows::remove(rec);
        }
        *st() = core::mem::zeroed();
        records::reset();
        *dsp::ds() = core::mem::zeroed();
        dsp::init_once();
        CALLED_NEED = [0; NEEDS_LEN];
    }
}
