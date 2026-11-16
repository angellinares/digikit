//! Synthetic `.dkm` files for the capacity tests of the runtime (docs/plans/capacity-isolation.md): a module of a chosen size, with a
//! chosen number of machines, parameters, pages, DSP bytes and voices, made from a small TOML spec instead of a fixture folder.
//!
//! The file is made by the same packer as a real module (`dkm::pack_data_only_max` or `dkm::pack_full_max`, `dkneeds::keys::write_keys`,
//! `dkmdsp::DspSection::encode`), so the format has one source. A spec without machines has the ColdFire code `moveq #0,d0; rts` (the stub is
//! `init`). A spec with machines packs the object of the fixture `tests/fixtures/modules/synth-decl` (`decl`): its `init` makes the builder calls
//! (machine, page, parameters) that its data table `synth_decl` asks for, and `synth` only replaces the bytes of that table (`decl_table`; layout in
//! the fixture's src/module.c). Everything else is data (`pad`, `fill`) or `bss`, which the file does not store. The DSP image is `fill` bytes (zero words are SHARC+ NOPs) and is never run: a voice names a type and an
//! entry at byte 0, and the capacity tests do not play it.
//!
//! ```toml
//! name = "cap/a1"       # module name, 1 to 31 characters
//! pad = 0               # data bytes behind the code (a multiple of 4): makes the file larger
//! fill = 0xa5           # the byte of `pad` (and of the DSP image)
//! bss = 0               # ColdFire bss bytes (a multiple of 4)
//! machines = 0          # machines m0, m1 ... (hashed keys), each with one page; `init` declares them
//! params = 0            # parameters, spread over the machines (machine i gets params / machines, the first params % machines one more)
//! pages = 0             # the [uses] page rows (Digitone II: one for each machine, so `machines` or more)
//! oversize = false      # allow a file over the staging buffer
//! [dsp]                 # a DSP section (optional)
//! image = 64            # bytes (a multiple of 4)
//! bss = 0               # DSP bss bytes (a multiple of 4)
//! align = 8             # 8, 16, 32 or 64
//! [[dsp.voice]]         # one per voice row
//! type = 16             # a pinned machine type number (put into the keys table, not declared), or
//! machine = "m0"        # a machine of this file (its hashed key)
//! state_bytes = 0       # per-voice state, and how many voices of it (DSP section version 3 when any is set)
//! voices = 0
//! ```

use crate::dkm::{self, Options};
use crate::dkmdsp::{DspSection, Voice};
use crate::dko::{Dko, DkoSection, DkoSymbol};
use crate::error::*;
use crate::register;
use crate::typed::Models;
use serde::Deserialize;

/// The fixture module whose object a spec with machines packs.
pub const DECL_MODULE: &str = "test/synth-decl";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub pad: u32,
    #[serde(default)]
    pub fill: u8,
    #[serde(default)]
    pub bss: u32,
    #[serde(default)]
    pub machines: u32,
    #[serde(default)]
    pub params: u32,
    #[serde(default)]
    pub pages: u32,
    #[serde(default)]
    pub oversize: bool,
    pub dsp: Option<DspSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DspSpec {
    pub image: u32,
    #[serde(default)]
    pub bss: u32,
    #[serde(default = "align8")]
    pub align: u32,
    #[serde(default)]
    pub voice: Vec<VoiceSpec>,
}

fn align8() -> u32 {
    8
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceSpec {
    /// A pinned type number (the hash part of the key is 0).
    #[serde(rename = "type")]
    pub type_no: Option<u32>,
    /// A machine of the file: the voice names it by its hashed key.
    pub machine: Option<String>,
    #[serde(default)]
    pub state_bytes: u32,
    #[serde(default)]
    pub voices: u32,
}

impl Spec {
    pub fn parse(text: &str) -> Result<Spec> {
        Ok(toml::from_str(text)?)
    }
}

/// The id of machine `i` of a synthetic module.
pub fn machine_id(i: u32) -> String {
    format!("m{i}")
}

/// The table `synth_decl` of SPEC (32-bit big-endian words): the machine count, then per machine its index (the row of its key in the keys table), its parameter count and a record
/// (min, max, default) for each parameter.
pub fn decl_table(spec: &Spec) -> Vec<u8> {
    let mut words = vec![spec.machines];
    for i in 0..spec.machines {
        let n = spec.params / spec.machines + u32::from(i < spec.params % spec.machines);
        words.push(i);
        words.push(n);
        for _ in 0..n {
            words.extend([0, 127, 0]);
        }
    }
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

/// The object of a spec with machines: DECL with the table of `synth_decl` replaced, and the `pad` and `bss` sections added.
fn with_decl(spec: &Spec, decl: &Dko) -> Result<Dko> {
    let mut obj = decl.clone();
    let at = obj
        .symbols
        .iter()
        .find(|s| s.name == "synth_decl")
        .and_then(|s| s.section.clone())
        .ok_or_else(|| err!("the fixture object has no `synth_decl`"))?;
    let table = decl_table(spec);
    let sec = obj
        .sections
        .iter_mut()
        .find(|s| s.name == at)
        .ok_or_else(|| err!("the fixture object has no section {at}"))?;
    sec.size = table.len() as u32;
    sec.align = 4;
    sec.hex = hex(&table);
    add_padding(spec, &mut obj);
    obj.stamp_isa();
    Ok(obj)
}

fn add_padding(spec: &Spec, obj: &mut Dko) {
    if spec.pad > 0 {
        obj.sections.push(DkoSection {
            name: ".synth_pad".into(),
            kind: "data".into(),
            align: 4,
            size: spec.pad,
            hex: hex(&vec![spec.fill; spec.pad as usize]),
        });
    }
    if spec.bss > 0 {
        obj.sections.push(DkoSection {
            name: ".synth_bss".into(),
            kind: "bss".into(),
            align: 4,
            size: spec.bss,
            hex: String::new(),
        });
    }
}

/// The payload bytes of the record block that `init` of a spec with machines commits on DEV (runtime collect.rs `lower`): the 16-byte header, a
/// MACHINE record (0x6c bytes) and a PARAM record (0x50) each, and on the Digitone II a page record (0x60) for each machine. 0 for a spec without machines.
pub fn record_bytes(spec: &Spec, dev: &str) -> u32 {
    if spec.machines == 0 {
        return 0;
    }
    16 + spec.machines * 0x6c
        + spec.params * 0x50
        + if dev == "dn2" {
            spec.machines * 0x60
        } else {
            0
        }
}

/// Make the `.dkm` of SPEC for device DEV (`dt2`, `dn2`): the device gives the parameter slots and the DSP section its device word. DECL is the
/// object of the fixture synth-decl, needed for a spec with machines.
pub fn synth(spec: &Spec, dev: &str, models: &Models, decl: Option<&Dko>) -> Result<Vec<u8>> {
    models
        .by_device
        .get(dev)
        .ok_or_else(|| err!("no device model for {dev:?}"))?;
    if spec.pad % 4 != 0 || spec.bss % 4 != 0 {
        return Err(err!("{}: pad and bss are multiples of 4", spec.name));
    }
    let obj = if spec.machines > 0 {
        let decl = decl.ok_or_else(|| {
            err!(
                "{}: a spec with machines needs the object of tests/fixtures/modules/synth-decl",
                spec.name
            )
        })?;
        with_decl(spec, decl)?
    } else {
        let mut obj = Dko {
            format: crate::dko::FORMAT,
            arch: "cf".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![DkoSection {
                name: ".text".into(),
                kind: "text".into(),
                align: 4,
                size: 4,
                hex: hex(&[0x70, 0x00, 0x4e, 0x75]), // moveq #0,d0; rts
            }],
            symbols: vec![DkoSymbol {
                name: "dk_init_stub".into(),
                section: Some(".text".into()),
                value: 0,
                global: true,
            }],
            relocs: vec![],
        };
        add_padding(spec, &mut obj);
        // a made-up object holds nothing but the stub and data: the packer's record of an ISA check is for built objects
        obj.stamp_isa();
        obj
    };

    let device = register::device_id(dev)?;
    let key = |m: &str| register::machine_key(&spec.name, m);
    if spec.pages > 0 && spec.machines == 0 {
        return Err(err!(
            "{}: a page belongs to a machine of the file, the spec has none",
            spec.name
        ));
    }
    // the keys table: the row counts and the machine keys, then the pinned types the voices name (a voice names a key of the table)
    if spec.machines == 0 && spec.params > 0 {
        return Err(err!(
            "{}: parameters belong to a machine of the file, the spec has none",
            spec.name
        ));
    }
    let mut machine_keys: Vec<u32> = (0..spec.machines).map(|i| key(&machine_id(i))).collect();
    if let Some(d) = &spec.dsp {
        for t in d.voice.iter().filter_map(|v| v.type_no) {
            if !machine_keys.contains(&t) {
                machine_keys.push(t);
            }
        }
    }
    let rows = (spec.machines as u32, spec.params as u32, spec.pages as u32);
    let rbytes = record_bytes(spec, dev);
    let mut keys = vec![0u8; dkneeds::keys::keys_size(rows, rbytes, machine_keys.len(), 0)];
    dkneeds::keys::write_keys(rows, rbytes, &machine_keys, &[], &mut keys)
        .map_err(|e| err!("{}: keys table: {e:?}", spec.name))?;
    let reg_bytes = keys;

    let dsp_bytes = match &spec.dsp {
        None => vec![],
        Some(d) => {
            if d.image == 0 || d.image % 4 != 0 || d.bss % 4 != 0 {
                return Err(err!(
                    "{}: dsp image and bss are multiples of 4 and the image is not empty",
                    spec.name
                ));
            }
            let mut voices = vec![];
            for v in &d.voice {
                let type_no = match (&v.type_no, &v.machine) {
                    (Some(t), None) => *t,
                    (None, Some(m)) => {
                        if !(0..spec.machines).any(|i| machine_id(i) == *m) {
                            return Err(err!(
                                "{}: voice names machine {m:?}, which the file does not have",
                                spec.name
                            ));
                        }
                        key(m)
                    }
                    _ => return Err(err!("{}: a voice has `type` or `machine`", spec.name)),
                };
                voices.push(Voice {
                    type_no,
                    entry: 0,
                    note_on: 0,
                    state_bytes: v.state_bytes,
                    clear_bytes: 0,
                    voices: v.voices,
                    ranges: [0; 8],
                    latched: 0,
                });
            }
            DspSection {
                sample_tracks: None,
                device,
                image: vec![spec.fill; d.image as usize],
                bss: d.bss,
                align: d.align,
                imports: vec![],
                relocs: vec![],
                voices,
                fxs: vec![],
            }
            .encode()
        }
    };
    let opts = Options {
        name: spec.name.clone(),
        version: 0x00_01_00,
        trace: false,
        stereo: false,
        gate: false,
        service: None,
    };
    let max = if spec.oversize {
        usize::MAX
    } else {
        dkm::max_file()
    };
    if spec.machines > 0 {
        dkm::pack_full_max(Some(&obj), &reg_bytes, &dsp_bytes, &opts, max)
    } else {
        dkm::pack_data_only_max(&obj, &reg_bytes, &dsp_bytes, &opts, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkm::Dkm;

    fn models() -> Models {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../devices");
        Models::from_dir(&root).unwrap()
    }

    fn make(text: &str, dev: &str) -> Vec<u8> {
        synth(&Spec::parse(text).unwrap(), dev, &models(), Some(&decl())).unwrap()
    }

    /// A stand-in for the fixture object: a `dk_module` with no entry points and a table `synth_decl` (the built fixture needs the toolchain).
    fn decl() -> Dko {
        let section = |name: &str, kind: &str, size: u32| DkoSection {
            name: name.into(),
            kind: kind.into(),
            align: 4,
            size,
            hex: hex(&vec![0; size as usize]),
        };
        let sym = |name: &str, sec: &str| DkoSymbol {
            name: name.into(),
            section: Some(sec.into()),
            value: 0,
            global: true,
        };
        let mut obj = Dko {
            format: crate::dko::FORMAT,
            arch: "cf".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![
                DkoSection {
                    hex: hex(&[0x70, 0x00, 0x4e, 0x75]),
                    ..section(".text", "text", 4)
                },
                section(".rodata.dk_module", "data", 16),
                section(".data.synth_decl", "data", 16),
            ],
            symbols: vec![
                sym("dk_module", ".rodata.dk_module"),
                sym("synth_decl", ".data.synth_decl"),
            ],
            relocs: vec![],
        };
        obj.stamp_isa();
        obj
    }

    #[test]
    fn a_plain_module_is_a_stub_with_the_chosen_size() {
        let f = make(
            "name = \"cap/a\"\npad = 100\nbss = 4096\nfill = 0xa5\n",
            "dt2",
        );
        let d = Dkm::parse(&f).unwrap();
        assert_eq!(d.name, "cap/a");
        // the loader allocates image + bss: code 4 + pad 100
        assert_eq!((d.image.len(), d.bss), (104, 4096));
        assert_eq!(&d.image[..4], &[0x70, 0x00, 0x4e, 0x75]);
        assert!(d.image[4..].iter().all(|b| *b == 0xa5));
        assert_eq!(d.init, Some(0));
        assert!(d.register.len() == dkneeds::keys::HEAD && d.dsp.is_empty());
        // deterministic
        assert_eq!(
            f,
            make(
                "name = \"cap/a\"\npad = 100\nbss = 4096\nfill = 0xa5\n",
                "dt2"
            )
        );
    }

    #[test]
    fn the_keys_table_has_the_rows_and_the_machine_keys_on_both_devices() {
        for dev in ["dt2", "dn2"] {
            let pages = if dev == "dn2" { 8 } else { 0 };
            let f = make(
                &format!("name = \"cap/r\"\nmachines = 8\nparams = 16\npages = {pages}\n"),
                dev,
            );
            let d = Dkm::parse(&f).unwrap();
            let k = dkneeds::keys::Keys::parse(&d.register).unwrap();
            assert_eq!(
                (k.machine_rows(), k.param_rows(), k.page_rows()),
                (8, 16, pages)
            );
            // a machine's key is the hashed key of "<module>:m<i>"
            assert_eq!(
                k.machine_keys().nth(3),
                Some(register::machine_key("cap/r", "m3"))
            );
        }
        let e = synth(
            &Spec::parse("name = \"x\"\npages = 2\n").unwrap(),
            "dn2",
            &models(),
            None,
        )
        .unwrap_err();
        assert!(e.0.contains("page belongs to a machine"), "{}", e.0);
    }

    #[test]
    fn the_table_spreads_the_parameters_over_the_machines_and_replaces_the_fixture_data() {
        let spec = Spec::parse("name = \"cap/t\"\nmachines = 3\nparams = 4\n").unwrap();
        let w: Vec<u32> = decl_table(&spec)
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect();
        // 3 machines; 2, 1 and 1 parameters, each (min, max, default) = (0, 127, 0)
        assert_eq!(
            w,
            [
                3, 0, 2, 0, 127, 0, 0, 127, 0, 1, 1, 0, 127, 0, 2, 1, 0, 127, 0
            ]
        );
        let f = make(
            "name = \"cap/t\"\nmachines = 3\nparams = 4\npad = 8\nfill = 7\n",
            "dt2",
        );
        let d = Dkm::parse(&f).unwrap();
        let table = decl_table(&spec);
        assert!(d.image.windows(table.len()).any(|x| x == table.as_slice()));
        assert!(d.image.ends_with(&[7; 8]));
    }

    #[test]
    fn a_dsp_section_has_the_sizes_and_voices_asked_for() {
        let f = make(
            "name = \"cap/d\"\nmachines = 1\n[dsp]\nimage = 64\nbss = 4096\nalign = 16\n\
             [[dsp.voice]]\ntype = 17\n[[dsp.voice]]\nmachine = \"m0\"\nstate_bytes = 32\nvoices = 2\n",
            "dn2",
        );
        let d = Dkm::parse(&f).unwrap();
        let s = DspSection::parse(&d.dsp).unwrap();
        assert_eq!((s.image.len(), s.bss, s.align), (64, 4096, 16));
        assert_eq!(s.device, register::device_id("dn2").unwrap());
        assert_eq!(s.voices.len(), 2);
        assert_eq!(s.voices[0].type_no, 17);
        assert_eq!(s.voices[1].type_no, register::machine_key("cap/d", "m0"));
        assert_eq!((s.voices[1].state_bytes, s.voices[1].voices), (32, 2));
    }

    #[test]
    fn an_oversize_file_needs_the_flag_and_bad_specs_are_refused() {
        let big = "name = \"cap/big\"\npad = 70000\n";
        let e = synth(&Spec::parse(big).unwrap(), "dt2", &models(), None).unwrap_err();
        assert!(e.0.contains("runtime stages"), "{}", e.0);
        let f = make(&format!("{big}oversize = true\n"), "dt2");
        assert!(f.len() > dkm::max_file());
        for bad in [
            "name = \"x\"\npad = 3\n",
            "name = \"\"\n",
            "name = \"x\"\n[dsp]\nimage = 0\n",
            "name = \"x\"\n[dsp]\nimage = 8\n[[dsp.voice]]\n",
            "name = \"x\"\n[dsp]\nimage = 8\n[[dsp.voice]]\nmachine = \"m9\"\n",
            "name = \"x\"\nsurprise = 1\n",
        ] {
            let r = Spec::parse(bad).and_then(|s| synth(&s, "dt2", &models(), None));
            assert!(r.is_err(), "accepted: {bad}");
        }
    }
}
