//! Host contract test of the module descriptors: the packed fixtures `test/dyn-fx-gain` (an effect, DSP section version 4) and `test/sine` (a machine,
//! version 3) go through the runtime's own DSP section check (`dsp::check`, which runs the checks of the fx, machines and samples services), and what the runtime
//! reads back is compared with what the module declares: the descriptor words of its assembler source, the `uses` of its module.toml, the symbols of its
//! pack output (address map). The layout numbers are the generated ones (`[format]`, `[fx]` and `[machine]` of sdk/sdk.toml), the same that the packer wrote the
//! file with. The fixtures are the Nx `pack` outputs `dist/modules/<device>/<leaf>.dkm` (this target depends on them).
//!
//! The DSP side is a table (`dyndsp_info`) that the test builds: the device, the area and the symbols the section imports.
use crate::cleanup_tests as ct;
use crate::{assign, collect};
use crate::dsp::{self, Job, INFO_MAGIC, I_AREA, I_AREA_BYTES, I_DEVICE, I_IMPORTS, I_MAGIC, I_NIMPORT, I_VFREE, I_VROWS, L2_BYTE_BASE};
use crate::test_lock::LOCK;
use dkneeds::{fx, keys::Keys, machine};
use dkrt_sdk::*;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

const DEVICES: [(&str, u32); 2] = [("dt2", 1), ("dn2", 2)];
const AREA_BYTES: u32 = 65536;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn fixture_text(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The packed fixture (Nx `test/<leaf>:pack`).
fn packed(device: &str, leaf: &str) -> Vec<u8> {
    // a service packs to `.dks`, every other module to `.dkm`
    let base = root().join(format!("dist/modules/{device}/{leaf}"));
    let p = if base.with_extension("dks").exists() { base.with_extension("dks") } else { base.with_extension("dkm") };
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e} (nx run test/{leaf}:pack)", p.display()))
}

fn be(b: &[u8], at: u32) -> u32 {
    let at = at as usize;
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn le(b: &[u8], at: u32) -> u32 {
    let at = at as usize;
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn put_be(b: &mut [u8], at: u32, v: u32) {
    b[at as usize..at as usize + 4].copy_from_slice(&v.to_be_bytes());
}
fn put_le(b: &mut [u8], at: u32, v: u32) {
    b[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
}

/// The DSP section of a packed file: (offset, size).
fn section(f: &[u8]) -> (u32, u32) {
    (be(f, dkneeds::H_DSP_OFF), be(f, dkneeds::H_DSP_SIZE))
}

/// The `dyndsp_info` table of a DSP with `device` that exports every name in `exports` (word layout: `I_*` of the abi).
fn info_table(device: u32, exports: &[String]) -> Vec<u32> {
    let mut w = std::vec![0u32; I_IMPORTS + 8 * exports.len()];
    w[I_MAGIC] = INFO_MAGIC;
    w[I_DEVICE] = device;
    w[I_AREA] = L2_BYTE_BASE;
    w[I_AREA_BYTES] = AREA_BYTES;
    w[I_VROWS] = dkneeds::MAX_VOICES; // the voice rows of core/dyndsp, all free
    w[I_VFREE] = dkneeds::MAX_VOICES;
    w[I_NIMPORT] = exports.len() as u32;
    for (i, name) in exports.iter().enumerate() {
        let at = I_IMPORTS + 8 * i;
        let mut bytes = [0u8; 32];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        for k in 0..7 {
            w[at + k] = u32::from_ne_bytes([bytes[4 * k], bytes[4 * k + 1], bytes[4 * k + 2], bytes[4 * k + 3]]);
        }
        w[at + 7] = 0x00b8_0000 + 2 * i as u32; // a nonzero address
    }
    w
}

/// The names the DSP section imports.
fn imports_of(f: &[u8]) -> Vec<String> {
    let (off, _) = section(f);
    let (at, n) = (be(f, off + dkneeds::DSP_H_IMPORT_OFF), be(f, off + dkneeds::DSP_H_IMPORT_N));
    (0..n)
        .map(|i| {
            let e = (off + at + i * dkneeds::DSP_IMPORT_LEN) as usize;
            let raw = &f[e..e + dkneeds::DSP_IMPORT_LEN as usize];
            String::from_utf8(raw[..raw.iter().position(|b| *b == 0).unwrap()].to_vec()).unwrap()
        })
        .collect()
}

/// Run the runtime's check of the DSP section of `f` on a DSP of `device`: the result code and what the check found.
fn check(f: &[u8], device: u32) -> (i32, Job) {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // an effect module's DSP section is checked by the fx service, a file of the card
    let _fx = crate::handlers::fx_fixture::LoadedFx::new();
    let info = info_table(device, &imports_of(f));
    let mut job = Job::empty();
    let (off, size) = section(f);
    // the boot's type assignment for the machines of the header table (`plan` does it before the loads)
    let mut keys = [0u32; 8];
    let n = collect::machine_keys(f, &mut keys);
    unsafe {
        let _info = ct::VoiceInfoGuard::set(info.as_ptr());
        assign::seed_for_test(&keys[..n]);
        let r = dsp::check(f.as_ptr(), off, size, f.len() as u32, &mut job);
        assign::seed_for_test(&[]);
        (r, job)
    }
}

/// The words of the descriptor a module's assembler source declares (`.VAR _<prefix>_desc[N] = value;`), constants of the contract resolved.
fn declared_desc(asm: &str, prefix: &str) -> Vec<u32> {
    let text = fixture_text(asm);
    let head = std::format!(".VAR _{prefix}_desc");
    let mut words = Vec::new();
    for line in text.lines().filter(|l| l.starts_with(&head)) {
        let (lhs, rhs) = line.split_once('=').unwrap();
        let index = lhs.trim_start_matches(&head).trim();
        assert_eq!(index.parse::<usize>().unwrap_or(0), words.len(), "descriptor words in order: {line}");
        let v = rhs.trim().trim_end_matches(';').trim();
        words.push(match v {
            "DK_FX_MAGIC" => fx::DK_FX_MAGIC,
            "DK_FX_V1" => fx::DK_FX_V1,
            "DK_MAGIC" => machine::DK_MAGIC,
            "DK_MACHINE_V1" => machine::DK_MACHINE_V1,
            "DK_MAX_VOICES" => machine::DK_MAX_VOICES,
            n => n.parse().unwrap_or_else(|_| panic!("unknown constant {n} in {asm}")),
        });
    }
    words
}

/// The image byte offset of a DSP text symbol of the pack output (the address map counts short words).
fn symbol_offset(addrmap: &str, symbol: &str) -> u32 {
    let at = addrmap.find(&std::format!("\"name\":\"{symbol}\"")).unwrap_or_else(|| panic!("{symbol} is not in the address map"));
    let head = &addrmap[..at];
    let n = &head[head.rfind("\"addr\":").unwrap() + 7..];
    n[..n.find(',').unwrap()].parse::<u32>().unwrap() * 2
}

/// `key = N` of the `[uses.<device>]` table of a module.toml.
fn declared_use(manifest: &str, device: &str, key: &str) -> u32 {
    let table = std::format!("[uses.{device}]");
    let body = &manifest[manifest.find(&table).unwrap() + table.len()..];
    let body = &body[..body.find("\n[").unwrap_or(body.len())];
    let line = body.lines().find(|l| l.starts_with(key)).unwrap();
    line.split('=').nth(1).unwrap().trim().parse().unwrap()
}

const FX: (&str, &str, &str, &str) = ("dyn-fx-gain", "tests/fixtures/modules/dyn-fx-gain", "src/dsp/dyn_fx_gain.s", "dg");
const MACHINE: (&str, &str, &str, &str) = ("sine", "tests/fixtures/modules/sine", "src/dsp/sine.s", "sn");

#[test]
fn the_effect_fixture_passes_the_check_and_reads_back_what_it_declares() {
    let (leaf, dir, asm, prefix) = FX;
    let decl = declared_desc(&std::format!("{dir}/{asm}"), prefix);
    // the source declares the whole descriptor of the contract: no word more or less
    assert_eq!(decl.len(), fx::DK_FX_DESC_WORDS as usize);
    let (declared_params, declared_state) = (decl[fx::DK_FX_DESC_W_N_PARAMS as usize], decl[fx::DK_FX_DESC_W_STATE_BYTES as usize]);
    for (device, id) in DEVICES {
        let f = packed(device, leaf);
        let (r, job) = check(&f, id);
        assert_eq!(r, OK, "{device}");
        assert_eq!((job.nfx, job.nvoice, job.vstride), (1, 0, dkneeds::VOICE_V3), "{device}");
        let addrmap = fixture_text(&std::format!("dist/modules/{device}/{leaf}.addrmap.json"));
        unsafe {
            let rec = |word: u32| be(core::slice::from_raw_parts(job.fxs, dkneeds::FX_LEN as usize), word);
            assert_eq!(rec(dkneeds::FX_PROCESS), symbol_offset(&addrmap, &std::format!("_{prefix}_process")), "{device}");
            assert_eq!(rec(dkneeds::FX_NPARAMS), declared_params, "{device}");
            assert_eq!(rec(dkneeds::FX_STATE), declared_state, "{device}");
            // the packer writes a valid placeholder (master and send, default master); the commit of `dk_fx_routes` decides the real routes
            assert_eq!(rec(dkneeds::FX_FLAGS), dkneeds::FXF_SEND, "{device}");
            // the descriptor the record points at is the declared one (little endian words in the DSP image)
            let (soff, _) = section(&f);
            let desc = soff + be(&f, soff + dkneeds::DSP_H_IMAGE_OFF) + rec(dkneeds::FX_DESC);
            let words: Vec<u32> = (0..fx::DK_FX_DESC_WORDS).map(|k| le(&f, desc + 4 * k)).collect();
            assert_eq!(words, decl, "{device}");
        }
    }
}

#[test]
fn the_machine_fixture_passes_the_check_and_reads_back_what_it_declares() {
    let (leaf, dir, asm, prefix) = MACHINE;
    let decl = declared_desc(&std::format!("{dir}/{asm}"), prefix);
    assert_eq!(decl.len(), machine::DK_DESC_WORDS as usize);
    let manifest = fixture_text(&std::format!("{dir}/module.toml"));
    for (device, id) in DEVICES {
        let f = packed(device, leaf);
        let (r, job) = check(&f, id);
        assert_eq!(r, OK, "{device}");
        assert_eq!((job.nvoice, job.nfx, job.vstride), (1, 0, dkneeds::VOICE_V3), "{device}");
        let addrmap = fixture_text(&std::format!("dist/modules/{device}/{leaf}.addrmap.json"));
        let e = unsafe { core::slice::from_raw_parts(job.voices, dkneeds::VOICE_V3 as usize) };
        let word = |at: u32| be(e, at);
        assert_eq!(word(dkneeds::VOICE_ENTRY), symbol_offset(&addrmap, &std::format!("_{prefix}_render")), "{device}");
        assert_eq!(word(dkneeds::VOICE_NOTE_ON), symbol_offset(&addrmap, &std::format!("_{prefix}_note_on")), "{device}");
        assert_eq!(word(dkneeds::VOICE_STATE_BYTES), decl[machine::DK_DESC_W_STATE_BYTES as usize], "{device}");
        assert_eq!(word(dkneeds::VOICE_CLEAR_BYTES), decl[machine::DK_DESC_W_STATE_CLEAR_BYTES as usize], "{device}");
        assert_eq!(word(dkneeds::VOICE_VOICES), decl[machine::DK_DESC_W_VOICES as usize], "{device}");
        assert_eq!(word(dkneeds::VOICE_V3_RESERVED), 0, "{device}");
        // the voice is the machine of the header table, and the table asks for the rows module.toml declares
        let (kt, ks) = (be(&f, dkneeds::H_REG_OFF), be(&f, dkneeds::H_REG_SIZE));
        let keys = Keys::parse(&f[kt as usize..(kt + ks) as usize]).unwrap();
        assert!(keys.has_key(dkneeds::KIND_MACHINE, word(dkneeds::VOICE_TYPE)), "{device}");
        // the runtime's own reader of the table finds the same machine key
        let mut found = [0u32; 8];
        assert_eq!(collect::machine_keys(&f, &mut found), 1, "{device}");
        assert_eq!(found[0], word(dkneeds::VOICE_TYPE), "{device}");
        assert_eq!(keys.machine_rows(), declared_use(&manifest, device, "machine_rows"), "{device}");
        assert_eq!(keys.param_rows(), declared_use(&manifest, device, "param_rows"), "{device}");
    }
}

#[test]
fn a_section_of_another_device_is_refused() {
    let f = packed("dn2", "sine");
    assert_eq!(check(&f, 1).0, E_DSP_DEVICE);
    let f = packed("dt2", "dyn-fx-gain");
    assert_eq!(check(&f, 2).0, E_DSP_DEVICE);
}

/// `f` with `edit` applied to the DSP section, checked on a DSP of the file's device: the code.
fn broken(device: &str, leaf: &str, edit: impl Fn(&mut [u8], u32, u32)) -> i32 {
    let mut f = packed(device, leaf);
    let (off, _) = section(&f);
    let id = DEVICES.iter().find(|d| d.0 == device).unwrap().1;
    let image = be(&f, off + dkneeds::DSP_H_IMAGE_OFF);
    edit(&mut f, off, image);
    check(&f, id).0
}

#[test]
fn a_section_with_the_wrong_magic_is_refused() {
    assert_eq!(broken("dt2", "dyn-fx-gain", |f, off, _| put_be(f, off + dkneeds::DSP_H_MAGIC, 0)), E_DSP_FORMAT);
    assert_eq!(broken("dt2", "sine", |f, off, _| put_be(f, off + dkneeds::DSP_H_MAGIC, 0)), E_DSP_FORMAT);
}

#[test]
fn an_effect_whose_descriptor_disagrees_with_its_record_is_refused() {
    // the descriptor's magic
    let r = broken("dt2", "dyn-fx-gain", |f, off, image| {
        let desc = off + image + be(f, off + be(f, off + dkneeds::DSP_H_FX_OFF) + dkneeds::FX_DESC);
        put_le(f, desc + fx::DK_FX_DESC_MAGIC, 0);
    });
    assert_eq!(r, E_DSP_FORMAT);
    // the parameter count of the record against the descriptor's
    let r = broken("dt2", "dyn-fx-gain", |f, off, _| {
        let rec = off + be(f, off + dkneeds::DSP_H_FX_OFF);
        put_be(f, rec + dkneeds::FX_NPARAMS, 2);
    });
    assert_eq!(r, E_DSP_FORMAT);
    // the state bytes of the record against the descriptor's
    let r = broken("dt2", "dyn-fx-gain", |f, off, _| {
        let rec = off + be(f, off + dkneeds::DSP_H_FX_OFF);
        put_be(f, rec + dkneeds::FX_STATE, 8);
    });
    assert_eq!(r, E_DSP_FORMAT);
}

#[test]
fn an_effect_descriptor_that_is_shorter_than_the_contract_is_refused() {
    // the descriptor starts so close to the end of the image that the contract's `DK_FX_DESC_BYTES` do not fit
    let r = broken("dt2", "dyn-fx-gain", |f, off, _| {
        let image_size = be(f, off + dkneeds::DSP_H_IMAGE_SIZE);
        let rec = off + be(f, off + dkneeds::DSP_H_FX_OFF);
        put_be(f, rec + dkneeds::FX_DESC, image_size - 4 * (fx::DK_FX_DESC_WORDS - 1));
    });
    assert_eq!(r, E_DSP_FORMAT);
}

#[test]
fn a_machine_voice_with_a_bad_entry_is_refused() {
    // note_on past the image
    let r = broken("dt2", "sine", |f, off, _| {
        let image_size = be(f, off + dkneeds::DSP_H_IMAGE_SIZE);
        let voice = off + be(f, off + dkneeds::DSP_H_VOICE_OFF);
        put_be(f, voice + dkneeds::VOICE_NOTE_ON, image_size);
    });
    assert_eq!(r, E_DSP_FORMAT);
    // more bytes cleared at note-on than the voice's state has
    let r = broken("dt2", "sine", |f, off, _| {
        let voice = off + be(f, off + dkneeds::DSP_H_VOICE_OFF);
        let state = be(f, voice + dkneeds::VOICE_STATE_BYTES);
        put_be(f, voice + dkneeds::VOICE_CLEAR_BYTES, state + 4);
    });
    assert_eq!(r, E_DSP_FORMAT);
}

// The test-only service `svc-test` (`.dks`) and its user (`.dkm`): the role word, the find and the plan on the packed files.
mod service_fixtures {
    use super::*;
    use crate::core_api::{CoreApi, REAL_CORE};
    use crate::fake_storage::FakeStorage;
    use crate::loader::discover::{find, Found};
    use crate::handlers::ServiceTable;
    use crate::plan::{plan_with, Env};
    use crate::loader::role::{role_of, role_ok};
    use dkneeds::keys::name_hash;
    use dkrt_abi::plan::{Reason, Verdict};
    use dkrt_abi::service::{Service, ServiceRow};
    use dkrt_abi::storage::{Storage, ROOT_STORE};

    struct S;
    impl<C: CoreApi> Service<C> for S {}
    static ROWS: [ServiceRow; 1] = [ServiceRow::of::<S, crate::core_api::RealCore>("machines", &[])];
    static ROWS_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ROWS);

    #[repr(C, align(16))]
    struct Block([u8; 1024]);

    fn head(f: &[u8]) -> [u8; 64] {
        f[..64].try_into().unwrap()
    }

    /// The header table of a packed file.
    fn table(f: &[u8]) -> &[u8] {
        let (off, size) = (be(f, dkneeds::H_REG_OFF) as usize, be(f, dkneeds::H_REG_SIZE) as usize);
        &f[off..off + size]
    }

    fn caps() -> Vec<DkrtCap> {
        let c = |kind: u8, limit: u32| DkrtCap { kind: kind as u32, limit, used: 0, largest: limit };
        vec![c(proto::CAPK_MODULES, 8), c(proto::CAPK_SELECTION, 8), c(proto::CAPK_ARENA, 1 << 20)]
    }

    #[test]
    fn the_service_has_role_1_and_a_version_2_table_and_the_user_is_an_ordinary_module_with_a_need() {
        for (dev, _) in DEVICES {
            let (svc, user) = (packed(dev, "svc-test"), packed(dev, "svc-test-user"));
            assert_eq!((role_of(&head(&svc)), role_of(&head(&user))), (1, 0), "{dev}");
            let (ks, ku) = (Keys::parse(table(&svc)).unwrap(), Keys::parse(table(&user)).unwrap());
            assert_eq!((ks.version(), ku.version()), (2, 1), "{dev}");
            assert_eq!(ks.words(dkneeds::keys::tag::PROVIDES, 0).unwrap().0, name_hash("svc-test"));
            assert_eq!(ku.words(dkneeds::keys::tag::NEEDS, 0).unwrap().0, name_hash("svc-test"));
            assert_eq!(role_ok(1, &ks), Ok(()), "{dev}");
            assert_eq!(role_ok(0, &ku), Ok(()), "{dev}");
            // the user's table is no service table, and the provider's is no module table
            assert_eq!(role_ok(1, &ku), Err(E_FORMAT), "{dev}");
            assert_eq!(role_ok(0, &ks), Err(E_FORMAT), "{dev}");
        }
    }

    fn found(dev: &str) -> (Vec<u8>, Found) {
        let svc = packed(dev, "svc-test");
        let mut fs = crate::loader::discover::fake::cards(vec![("services/svc-test.dks".into(), svc.clone())]);
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let mut out = Found::new();
        unsafe { find(&mut fs, &services, &[name_hash("svc-test")], &mut out) };
        (svc, out)
    }

    #[test]
    fn the_find_takes_the_packed_provider_from_the_services_folder_for_the_users_need() {
        for (dev, _) in DEVICES {
            let (svc, out) = found(dev);
            assert_eq!(out.n, 1, "{dev}");
            assert_eq!(out.path(0), b"services/svc-test.dks");
            assert_eq!(out.table(0), table(&svc));
        }
    }

    #[test]
    fn the_plan_loads_the_provider_before_the_user_and_the_user_alone_is_missing_its_service() {
        for (dev, _) in DEVICES {
            let user = packed(dev, "svc-test-user");
            let (svc, _) = found(dev);
            let (mods, provs) = ([Some(Keys::parse(table(&user)).unwrap())], [Some(Keys::parse(table(&svc)).unwrap())]);
            let c = caps();
            let env = Env { services: ServiceTable::new(&ROWS_P.0, &REAL_CORE), runtime: 0x000900, caps: &c };
            let p = plan_with(&env, &mods, &provs);
            assert_eq!((p.n, p.np), (1, 1), "{dev}");
            assert_eq!((p.provider_verdicts[0], p.verdicts[0]), (Verdict::Accepted, Verdict::Accepted), "{dev}");
            let p = plan_with(&env, &mods, &[]);
            assert_eq!(p.verdicts[0], Verdict::Refused(Reason::NeedMissing(name_hash("svc-test"))), "{dev}");
            assert_eq!(p.verdicts[0].result(), E_MISSING, "{dev}");
        }
    }
}
