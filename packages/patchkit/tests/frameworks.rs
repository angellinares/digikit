//! `core/machines` over the real ports and stock images (skipped when the images are absent): what the framework
//! writes for one and for two added machine types, canonical type numbers, and the refusals.

use patchkit::apply::{self, Applied, Options};
use patchkit::firmware::Firmware;
use patchkit::module::{Library, Module};
use patchkit::port::Registry;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn image(name: &str) -> Option<Vec<u8>> {
    std::fs::read(root().join("firmware").join(name)).ok()
}

const DT2_117: &str = "Digitakt_II_OS1.17.syx";
const DN2_112: &str = "Digitone_II_OS1.12.syx";

fn registry() -> Registry {
    Registry::from_dir(&root().join("devices/ports")).unwrap()
}

fn library(extra: Vec<Module>) -> Option<Library> {
    let mut lib = Library::scan_repo_or_skip(&root())?;
    lib.modules.extend(extra);
    Some(lib)
}

fn run(file: &str, lib: &Library, ids: &[&str]) -> Option<patchkit::Result<Applied>> {
    let raw = image(file)?;
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    Some(apply::apply_with(
        &raw,
        &registry(),
        lib,
        &ids,
        Options::default(),
    ))
}

/// Memory of the patched MAIN OS: the image plus the segment chain behind it.
struct Main {
    data: Vec<u8>,
    base: u32,
    segs: Vec<(u32, Vec<u8>)>,
}

impl Main {
    fn new(syx: &[u8], stock_len: usize) -> Main {
        let fw = Firmware::parse(syx).unwrap();
        let s = fw.section(3).unwrap();
        let data = s.data.clone();
        let mut segs = vec![];
        let mut at = stock_len;
        loop {
            let len = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
            if len == u32::MAX {
                break;
            }
            let dest = u32::from_be_bytes(data[at + 4..at + 8].try_into().unwrap());
            segs.push((dest, data[at + 12..at + 12 + len as usize].to_vec()));
            at += 12 + len as usize;
        }
        Main {
            data,
            base: s.dest,
            segs,
        }
    }
    fn read(&self, addr: u32, n: usize) -> Vec<u8> {
        if let Some(o) = addr.checked_sub(self.base)
            && (o as usize + n) <= self.data.len()
        {
            return self.data[o as usize..o as usize + n].to_vec();
        }
        for (d, p) in &self.segs {
            if addr >= *d && (addr - d) as usize + n <= p.len() {
                return p[(addr - d) as usize..(addr - d) as usize + n].to_vec();
            }
        }
        panic!("{addr:#x}+{n} is not in the patched MAIN OS");
    }
    fn u32s(&self, addr: u32, n: usize) -> Vec<u32> {
        self.read(addr, n * 4)
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect()
    }
    fn cstr(&self, addr: u32) -> String {
        (0..64)
            .map(|i| self.read(addr + i, 1)[0])
            .take_while(|&b| b != 0)
            .map(|b| b as char)
            .collect()
    }
}

fn stock_len(file: &str) -> usize {
    Firmware::parse(&image(file).unwrap())
        .unwrap()
        .section(3)
        .unwrap()
        .data
        .len()
}

fn plan(a: &Applied) -> serde_json::Value {
    serde_json::to_value(&a.plan).unwrap()
}

fn reg<'a>(p: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    p["registries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no registry {name}"))
}

fn section<'a>(p: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    p["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no section {name}"))
}

const PAIR: [&str; 2] = ["core/machines", "core/params"];

#[test]
fn the_selection_order_does_not_change_the_bytes() {
    let Some(lib) = library(vec![]) else {
        return;
    };
    let Some(r1) = run(DT2_117, &lib, &PAIR) else {
        return;
    };
    let r2 = run(DT2_117, &lib, &["core/params", "core/machines"]).unwrap();
    assert!(r1.unwrap().syx == r2.unwrap().syx);
}

#[test]
fn every_instruction_the_framework_places_decodes() {
    let Some(lib) = library(vec![]) else {
        return;
    };
    for (file, ids) in [
        (DT2_117, &["core/machines"][..]),
        (DN2_112, &["core/machines"][..]),
    ] {
        let Some(r) = run(file, &lib, ids) else {
            continue;
        };
        let a = r.unwrap();
        let m = Main::new(&a.syx, stock_len(file));
        let mut n = 0;
        for w in a.plan.writes.iter().filter(|w| {
            w.kind == "place" && w.owner.starts_with("core/machines") && !w.code.is_empty()
        }) {
            for &(lo, hi) in &w.code {
                let mut at = lo;
                let bytes = m.read(lo as u32, (hi - lo) as usize);
                while at < hi {
                    let i = coldfire::decode_at(&bytes, lo as u32, (at - lo) as usize)
                        .unwrap_or_else(|| panic!("{at:#x}: does not decode"));
                    at += (i.len as u64).max(2);
                    n += 1;
                }
                assert_eq!(at, hi, "the code ends mid-instruction");
            }
        }
        assert!(n > 100, "{file}: only {n} instructions");
    }
}

#[test]
fn the_image_keeps_only_the_machine_hooks_that_stay_in_it() {
    let Some(lib) = library(vec![]) else {
        return;
    };
    for file in [DN2_112] {
        let Some(r) = run(file, &lib, &["core/machines"]) else {
            continue;
        };
        let a = r.unwrap();
        let p = plan(&a);
        // a machine is a run-time module: the registry has no patch-time entry
        assert_eq!(reg(&p, "machines")["entries"].as_array().unwrap().len(), 0);
        // no DSP selector table and no per-type byte on the Digitone II
        assert!(
            p["sections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["name"] != "mf_selector" && s["name"] != "mf_pertype")
        );
        let hooks: Vec<String> = p["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["anchor"].as_str().unwrap().to_string())
            .collect();
        // the image keeps the dispatch hook, the list entry and the frame type stub; the other hooks are the machines service file's (R12-6)
        for h in [
            "cf.machine.fn.dispatch",
            "cf.machine.fn.list.entry",
            "cf.machine.site.frametype",
        ] {
            assert!(hooks.iter().any(|x| x == h), "{file}: no hook on {h}");
        }
        for h in [
            "cf.machine.site.group",
            "cf.machine.site.type_reader.load",
            "cf.machine.site.owner_type_load",
            "cf.machine.fn.type_page.count",
        ] {
            assert!(
                !hooks.iter().any(|x| x == h),
                "{file}: the image hooks {h}, which machines.dks takes"
            );
        }
    }
}

/// `core/overdrive` edits the stock OVR Routing row (0x103) of core/params' copied table through `param_edits` (a section `edit`):
/// range, names and formatter; the other words of the row stay the stock ones. Its two frame hooks run the displaced instruction and
/// its two call sites are retargeted.
#[test]
fn core_overdrive_edits_the_stock_ovr_row_and_hooks_the_dsp() {
    let Some(lib) = library(vec![]) else {
        return;
    };
    let Some(r) = run(DT2_117, &lib, &["core/params", "core/overdrive"]) else {
        return;
    };
    let a = r.unwrap();
    let p = plan(&a);
    let t = section(&p, "pf_table");
    assert_eq!(t["appended_rows"], 0);
    let main = Main::new(&a.syx, stock_len(DT2_117));
    let s3 = Firmware::parse(&image(DT2_117).unwrap())
        .unwrap()
        .section(3)
        .unwrap()
        .clone();
    let stock = Main {
        data: s3.data,
        base: s3.dest,
        segs: vec![],
    };
    let row = |m: &Main, base: u64| m.u32s(base as u32 + 60 * 0x103, 15);
    let (got, was) = (
        row(&main, t["addr"].as_u64().unwrap()),
        row(&stock, t["from_addr"].as_u64().unwrap()),
    );
    let sym = |n: &str| a.plan.symbols[&format!("core/overdrive@0.1.0:{n}")] as u32;
    assert_eq!((got[2], got[3]), (0, 0xf00), "min, max");
    assert_eq!(main.cstr(got[10]), "OVR Type");
    assert_eq!(main.cstr(got[12]), "OD.TY");
    assert_eq!(got[13], sym("od_type_fmt"));
    for k in [0, 1, 4, 5, 6, 7, 8, 9, 11, 14] {
        assert_eq!(got[k], was[k], "word {k} stays stock");
    }
    let hooks: Vec<_> = p["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["module"] == "core/overdrive@0.1.0")
        .map(|h| {
            (
                h["hook"].as_str().unwrap(),
                h["displaced"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        hooks,
        [
            ("dsp.frame.hook.frame_rx.copy_cond", "run"),
            ("dsp.frame.hook.frame_rx.copy_block", "run")
        ]
    );
    let retargets = a
        .plan
        .writes
        .iter()
        .filter(|w| {
            w.what.starts_with("retarget dsp.voice.site.") && w.what.contains("overdrive_call")
        })
        .count();
    assert_eq!(retargets, 2);
}
