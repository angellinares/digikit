//! The ISA check record of ColdFire objects (`patchkit build` writes it, `patchkit pack` verifies it).

use patchkit::dkm;
use patchkit::error::{hex, unhex};
use patchkit::module::Library;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn opts() -> dkm::Options {
    dkm::Options {
        name: "test/x".into(),
        version: 0x010203,
        runtime_min: 1,
        trace: false,
        stereo: false,
        api_minor: 0,
    }
}

/// The record the Python build wrote equals the digest computed here, for every committed ColdFire object (the two implementations of
/// the framing agree), and a changed byte in any of them is found.
#[test]
fn every_committed_coldfire_object_carries_a_matching_record() {
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let mut n = 0;
    for m in lib.modules.iter() {
        for o in m.objects.iter().filter(|o| o.arch == "cf") {
            o.require_isa_checked()
                .unwrap_or_else(|e| panic!("{}: {}", m.id(), e.0));
            n += 1;
            // a changed code byte: refused
            let mut t = o.clone();
            if let Some(s) = t
                .sections
                .iter_mut()
                .find(|s| s.kind == "text" && s.size > 0)
            {
                let mut b = unhex(&s.hex).unwrap();
                b[0] ^= 1;
                s.hex = hex(&b);
                assert!(t.require_isa_checked().is_err(), "{}", m.id());
            }
        }
    }
    assert!(n >= 20, "only {n} ColdFire objects found");
}

/// The `size` line of `pack` is the header's own fields: file = header + slots + padding (under 32) + the listed sections.
#[test]
fn pack_size_summary_parts_sum_to_the_file() {
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let m = lib.get("test/dyn-fault").unwrap();
    let o = m.objects.iter().find(|o| o.arch == "cf").unwrap();
    let f = dkm::pack(o, &opts()).unwrap();
    let d = dkm::Dkm::parse(&f).unwrap();
    let line = d.size_summary(f.len());
    assert!(
        line.starts_with(&format!(
            "{} bytes (image {}, bss {}, reloc ",
            f.len(),
            d.image.len(),
            d.bss
        )),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "registry {}, dsp {})",
            d.register.len(),
            d.dsp.len()
        )),
        "{line}"
    );
    let listed = d.image.len()
        + d.relocs.len() * 8
        + d.imports.len() * dkm::IMPORT_LEN
        + d.register.len()
        + d.dsp.len();
    let pad = f.len() - dkm::HEADER - d.slots.len() * 8 - listed;
    assert!(pad < 32, "{pad} bytes of padding in {line}");
}

/// A committed object packs; the same object with a byte of its code changed (no new record) is refused by `pack`.
#[test]
fn pack_refuses_a_tampered_object() {
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let m = lib.get("test/dyn-fault").unwrap();
    let o = m.objects.iter().find(|o| o.arch == "cf").unwrap();
    assert!(dkm::pack(o, &opts()).is_ok());
    let mut t = o.clone();
    let s = t.sections.iter_mut().find(|s| s.kind == "text").unwrap();
    let mut b = unhex(&s.hex).unwrap();
    b[1] ^= 0x10;
    s.hex = hex(&b);
    let e = dkm::pack(&t, &opts()).unwrap_err().0;
    assert!(e.contains("ISA check record does not match"), "{e}");
    // without a record at all
    let mut u = o.clone();
    u.toolchain.remove(patchkit::dko::ISA_CHECK_KEY);
    assert!(
        dkm::pack(&u, &opts())
            .unwrap_err()
            .0
            .contains("no ISA check record")
    );
}
