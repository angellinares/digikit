//! `.dko.json`: a relocatable object in canonical JSON (sorted keys, integers only), made by
//! `patchkit build` from a toolchain's output and consumed by the linker. The one model of the format: patchkit re-exports it.
//!
//! Units: ColdFire sections and symbols are bytes. SHARC+ text sections hold the instruction parcels in
//! boot-stream order (each 16-bit parcel little endian); text symbol values and relocation offsets count
//! parcels; data sections and their symbols count bytes. A SHARC relocation writes the target's natural
//! address: the short-word pc for a text symbol, the byte address for a data symbol or an extern value.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Dko {
    pub format: u32,
    /// `cf` or `sharc`
    pub arch: String,
    pub source_sha256: String,
    #[serde(default)]
    pub toolchain: BTreeMap<String, String>,
    pub sections: Vec<DkoSection>,
    pub symbols: Vec<DkoSymbol>,
    #[serde(default)]
    pub relocs: Vec<DkoReloc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DkoSection {
    pub name: String,
    /// `text`, `data` or `bss`
    pub kind: String,
    pub align: u32,
    pub size: u32,
    #[serde(default)]
    pub hex: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DkoSymbol {
    pub name: String,
    /// None: an undefined symbol the linker must resolve (an anchor or another object's export).
    pub section: Option<String>,
    pub value: u64,
    pub global: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DkoReloc {
    pub section: String,
    pub offset: u64,
    #[serde(rename = "type")]
    pub rtype: String,
    pub symbol: String,
    pub addend: i64,
}

/// The key of `Dko::toolchain` that records the ISA check (`patchkit build` writes it after `build::cf::check_isa` passed): the digest of the
/// object's sections, symbols and relocations. `patchkit pack` takes a ColdFire object only when the record matches the bytes it holds.
pub const ISA_CHECK_KEY: &str = "isa_check";

impl Dko {
    /// SHA-256 over the object's code and data and what ties it together, in a fixed framing (`patchkit build` computes it with this function):
    /// "dko-isa-v1", then sections (name, kind, align, size, bytes), symbols (name, section, defined flag, value, global) and relocations
    /// (section, offset, type, symbol, addend). Strings and byte strings are a u32 length and the bytes; numbers are big endian.
    pub fn isa_digest(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        fn s(h: &mut Sha256, x: &[u8]) {
            h.update((x.len() as u32).to_be_bytes());
            h.update(x);
        }
        let mut h = Sha256::new();
        h.update(b"dko-isa-v1");
        h.update((self.sections.len() as u32).to_be_bytes());
        for x in &self.sections {
            s(&mut h, x.name.as_bytes());
            s(&mut h, x.kind.as_bytes());
            h.update(x.align.to_be_bytes());
            h.update(x.size.to_be_bytes());
            s(&mut h, &unhex(&x.hex)?);
        }
        h.update((self.symbols.len() as u32).to_be_bytes());
        for x in &self.symbols {
            s(&mut h, x.name.as_bytes());
            s(&mut h, x.section.as_deref().unwrap_or("").as_bytes());
            h.update([x.section.is_some() as u8]);
            h.update(x.value.to_be_bytes());
            h.update([x.global as u8]);
        }
        h.update((self.relocs.len() as u32).to_be_bytes());
        for x in &self.relocs {
            s(&mut h, x.section.as_bytes());
            h.update(x.offset.to_be_bytes());
            s(&mut h, x.rtype.as_bytes());
            s(&mut h, x.symbol.as_bytes());
            h.update(x.addend.to_be_bytes());
        }
        Ok(hex(&h.finalize()))
    }

    /// Record the ISA check for the bytes the object holds now. The build calls its own checker first (`build::cf::check_isa`); this is for tests
    /// that make an object by hand.
    pub fn stamp_isa(&mut self) {
        let d = self.isa_digest().expect("object hex");
        self.toolchain.insert(ISA_CHECK_KEY.into(), d);
    }

    /// A ColdFire object is packed into a `.dkm` (code the runtime runs in supervisor mode) only when the build recorded a passing ISA
    /// check over exactly these bytes: a 68020-only or FPU form in it would trap on the device (tools/coldfire/isacheck.py). An object made
    /// by hand, edited after the build or built before the record existed is refused. This is a guard against accident, not a signature.
    pub fn require_isa_checked(&self) -> Result<(), String> {
        if self.arch != "cf" {
            return Ok(());
        }
        match self.toolchain.get(ISA_CHECK_KEY) {
            None => Err("the ColdFire object has no ISA check record: build it with `mise run module:build` (`patchkit build`)".to_string()),
            Some(r) if *r == self.isa_digest()? => Ok(()),
            Some(_) => Err("the ColdFire object's ISA check record does not match its bytes (edited after the build?): rebuild it with `mise run module:build`".to_string()),
        }
    }

    pub fn parse(text: &str) -> Result<Dko, String> {
        let o: Dko = serde_json::from_str(text).map_err(|e| format!("json: {e}"))?;
        if o.format != FORMAT {
            return Err(format!(
                "object format {} is not supported (expected {FORMAT})",
                o.format
            ));
        }
        if o.arch != "cf" && o.arch != "sharc" {
            return Err(format!("object arch {:?}: expected cf or sharc", o.arch));
        }
        for s in &o.sections {
            if !matches!(s.kind.as_str(), "text" | "data" | "bss") {
                return Err(format!("section {}: kind {:?}", s.name, s.kind));
            }
            if s.kind != "bss" && s.hex.len() != s.size as usize * 2 {
                return Err(format!(
                    "section {}: hex length does not match size {}",
                    s.name, s.size
                ));
            }
            if s.align == 0 || !s.align.is_power_of_two() {
                return Err(format!(
                    "section {}: align {} is not a power of two",
                    s.name, s.align
                ));
            }
        }
        Ok(o)
    }
}

fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err(format!("bad hex string of length {}", s.len()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad hex digit near {i}"))
        })
        .collect()
}
