//! `patchkit dkm-identity BEFORE_DIR AFTER_DIR`: did a change of the `.dkm` header table move anything that runs?
//!
//! For each `.dkm` file name in both directories the parts that run must be byte-identical: the image (code and data), the relocations, the imports,
//! the DSP section, the bss size, the alignment and the entry offsets. Only the header table, the offsets it moves, the size, the hash and the version the
//! module needs may differ. The DSP section is compared whole, so the PCM and the SHARC golden hashes cannot move. Pack both sets with
//! `patchkit pack-all --device DEV --out DIR MODULE...` before and after the change.

use crate::dkm::Dkm;
use crate::error::*;
use std::collections::BTreeSet;
use std::path::Path;

/// The result of a comparison.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Files whose running parts are identical.
    pub identical: usize,
    /// One line for each file that is in one directory only, or that differs, with the parts that differ.
    pub failures: Vec<String>,
}

/// The parts of the file `a` and `b` (bytes of two `.dkm`) that differ.
pub fn differing_parts(a: &[u8], b: &[u8]) -> Result<Vec<&'static str>> {
    let (a, b) = (Dkm::parse(a)?, Dkm::parse(b)?);
    let mut d = vec![];
    for (name, same) in [
        ("image", a.image == b.image),
        ("relocs", a.relocs == b.relocs),
        ("imports", a.imports == b.imports),
        ("dsp", a.dsp == b.dsp),
        ("bss", a.bss == b.bss),
        ("align", a.align == b.align),
        ("init", a.init == b.init),
        ("fini", a.fini == b.fini),
        ("menu", a.menu == b.menu),
        ("tick", a.tick == b.tick),
        ("name", a.name == b.name),
        ("version", a.version == b.version),
        ("role", a.role == b.role),
    ] {
        if !same {
            d.push(name);
        }
    }
    Ok(d)
}

fn names(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for e in std::fs::read_dir(dir).map_err(|e| err!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| err!("{}: {e}", dir.display()))?;
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".dkm") || n.ends_with(".dks") {
            out.insert(n);
        }
    }
    Ok(out)
}

/// Compare the `.dkm` files of two directories.
pub fn compare_dirs(before: &Path, after: &Path) -> Result<Report> {
    let (nb, na) = (names(before)?, names(after)?);
    let mut r = Report::default();
    for n in nb.symmetric_difference(&na) {
        let side = if nb.contains(n) { "before" } else { "after" };
        r.failures.push(format!("{n}: only in {side}"));
    }
    for n in nb.intersection(&na) {
        let read = |d: &Path| std::fs::read(d.join(n)).map_err(|e| err!("{n}: {e}"));
        match differing_parts(&read(before)?, &read(after)?) {
            Ok(d) if d.is_empty() => r.identical += 1,
            Ok(d) => r.failures.push(format!("{n}: differs in {}", d.join(", "))),
            Err(e) => r.failures.push(format!("{n}: {e}")),
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packed file of the stub module, with the header table `reg` (empty: the head only).
    fn file(reg: &[u8], dsp: &[u8]) -> Vec<u8> {
        let o = crate::dkm::Options {
            name: "test/x".into(),
            version: 0x010203,
            trace: false,
            stereo: false,
            gate: false,
            service: None,
        };
        crate::dkm::pack_keys(None, reg, dsp, &o).unwrap()
    }

    #[test]
    fn a_header_table_change_is_not_a_difference_and_a_changed_dsp_section_is() {
        let mut t = [0u8; 64];
        let n = dkneeds::keys::write_keys((1, 2, 0), 64, &[0x100], &[], &mut t).unwrap();
        let (plain, with_table) = (file(&[], &[]), file(&t[..n], &[]));
        assert_ne!(plain, with_table);
        assert_eq!(
            differing_parts(&plain, &with_table).unwrap(),
            Vec::<&str>::new()
        );
        // a DSP section that differs is a difference
        let (d1, d2) = (file(&t[..n], &[0u8; 16]), file(&t[..n], &[1u8; 16]));
        assert_eq!(differing_parts(&d1, &d2).unwrap(), vec!["dsp"]);
    }
}
