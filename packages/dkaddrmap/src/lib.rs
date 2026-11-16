//! The address map: names for code addresses (docs/ADDRMAP.md).
//!
//! A map file lists entries `{cpu, addr, size, name, layer}`. A lookup takes the maps of one run and the load table of its modules and
//! names an address with the most specific layer that holds it:
//!
//! 1. `our`: the runtime, core adapters and loaded `.dkm` modules (modules are link-relative and rebased by the load table),
//! 2. `anchor`: the firmware anchors of `devices/ports/`,
//! 3. `fw`: names from the reverse-engineering databases (developer machines only).
//!
//! No layer holding the address: the lookup answers nothing and the caller prints the raw address. For a return address the caller asks for
//! the address minus 2 ([`Resolver::lookup_return`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod snapshot;

pub const FORMAT: &str = "dk-addrmap";
pub const VERSION: u32 = 1;

/// What a map was made from. Maps of one run must agree on `image_sha256`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Key {
    /// `dt2` or `dn2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Firmware version, for example `1.17`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// SHA-256 of the stock `.syx` the map belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_sha256: Option<String>,
    /// SHA-256 of the patched `.syx` (a ship map written by `patchkit apply`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    /// The modules the patch placed, `id@source-hash`, in plan order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    /// A module map (`patchkit pack`): the module name the runtime reports, the SHA-256 of the object it was built from and of the `.dkm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dkm_sha256: Option<String>,
    /// A ship map: what a host needs to read the load table of the running image (slots, L2 bases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader: Option<snapshot::Loader>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Cpu {
    Cf,
    Sharc,
}

impl Cpu {
    pub fn parse(s: &str) -> Option<Cpu> {
        match s {
            "cf" => Some(Cpu::Cf),
            "sharc" => Some(Cpu::Sharc),
            _ => None,
        }
    }
}

/// Most specific first.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Our,
    Anchor,
    Fw,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub cpu: Cpu,
    /// ColdFire: byte address. SHARC: software (short-word) PC. With `module` set: the offset from the module's load address (SHARC: from the
    /// start of its DSP image, in short words).
    pub addr: u64,
    /// Bytes (SHARC: words) from `addr`; 0 holds the address `addr` only.
    pub size: u64,
    pub name: String,
    pub layer: Layer,
    /// The module (the name the runtime reports) whose load address `addr` is relative to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AddrMap {
    pub format: String,
    pub version: u32,
    /// `ship` (layers our and anchor), `module` (one module, link-relative) or `dev` (layer fw).
    pub kind: String,
    pub key: Key,
    pub entries: Vec<Entry>,
}

impl AddrMap {
    pub fn new(kind: &str, key: Key, entries: Vec<Entry>) -> AddrMap {
        AddrMap {
            format: FORMAT.into(),
            version: VERSION,
            kind: kind.into(),
            key,
            entries,
        }
    }

    pub fn parse(text: &str) -> Result<AddrMap, String> {
        let m: AddrMap = serde_json::from_str(text).map_err(|e| format!("address map: {e}"))?;
        if m.format != FORMAT {
            return Err(format!(
                "address map: format {:?}, expected {FORMAT}",
                m.format
            ));
        }
        if m.version != VERSION {
            return Err(format!(
                "address map: version {}, expected {VERSION}",
                m.version
            ));
        }
        Ok(m)
    }

    /// Canonical JSON: entries sorted by (cpu, layer, module, addr, name), so equal inputs give equal files.
    pub fn to_json(&self) -> String {
        let mut m = self.clone();
        m.entries.sort_by(|a, b| {
            (a.cpu, a.layer, &a.module, a.addr, &a.name)
                .cmp(&(b.cpu, b.layer, &b.module, b.addr, &b.name))
        });
        let mut s = serde_json::to_string(&m).expect("address map serialises");
        s.push('\n');
        s
    }
}

/// Where each module of a run is loaded (read from the runtime's module table).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoadTable {
    pub modules: Vec<Load>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Load {
    /// The name the runtime reports (`test/dyn-dn2`).
    pub id: String,
    pub base: u64,
    #[serde(default)]
    pub size: u64,
    /// Where the module's DSP image starts, as a software (short-word) PC; `sharc` entries of the module are relative to it. None: the module
    /// has no DSP part, or the reader did not find where it sits (its DSP entries stay unresolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsp_base: Option<u64>,
    /// The address of the module's `fini` function, when the table was read from the runtime (None: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fini: Option<u64>,
}

/// The loader description of the first map that has one.
pub fn loader_of(maps: &[AddrMap]) -> Option<&snapshot::Loader> {
    maps.iter().find_map(|m| m.key.loader.as_ref())
}

/// A name for an address.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Hit {
    pub name: String,
    pub layer: Layer,
    /// The address the symbol starts at (absolute).
    pub sym_addr: u64,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

impl Hit {
    /// `name`, or `name+0xOFF`.
    pub fn text(&self) -> String {
        if self.offset == 0 {
            self.name.clone()
        } else {
            format!("{}+{:#x}", self.name, self.offset)
        }
    }
}

#[derive(Debug)]
struct Item {
    addr: u64,
    end: u64,
    name: String,
    module: Option<String>,
}

/// One layer of one CPU: entries by start address with the running maximum of the ends, so nested and overlapping ranges are found.
#[derive(Default, Debug)]
struct Index {
    items: Vec<Item>,
    max_end: Vec<u64>,
}

impl Index {
    fn build(mut items: Vec<Item>) -> Index {
        items.sort_by(|a, b| (a.addr, a.end, &a.name).cmp(&(b.addr, b.end, &b.name)));
        let mut max_end = Vec::with_capacity(items.len());
        let mut m = 0;
        for i in &items {
            m = m.max(i.end);
            max_end.push(m);
        }
        Index { items, max_end }
    }

    /// The smallest range that holds `addr` (the latest start among equals).
    fn find(&self, addr: u64) -> Option<&Item> {
        let mut i = self.items.partition_point(|x| x.addr <= addr);
        let mut best: Option<&Item> = None;
        while i > 0 && self.max_end[i - 1] > addr {
            i -= 1;
            let x = &self.items[i];
            if x.end > addr && best.is_none_or(|b| x.end - x.addr < b.end - b.addr) {
                best = Some(x);
            }
        }
        best
    }
}

#[derive(Default, Debug)]
pub struct Resolver {
    index: BTreeMap<(Cpu, Layer), Index>,
    /// Modules named by entries but missing from the load table: their entries are not in the index.
    pub unloaded: Vec<String>,
}

impl Resolver {
    /// Builds the lookup. Maps that name a stock image must name the same one. Module entries are rebased by `loads`.
    pub fn new(maps: &[AddrMap], loads: &LoadTable) -> Result<Resolver, String> {
        let mut image: Option<&str> = None;
        for m in maps {
            if let Some(h) = m.key.image_sha256.as_deref() {
                match image {
                    Some(p) if p != h => {
                        return Err(format!(
                            "the maps belong to different firmware images ({p} and {h})"
                        ));
                    }
                    _ => image = Some(h),
                }
            }
        }
        let base: BTreeMap<&str, &Load> =
            loads.modules.iter().map(|l| (l.id.as_str(), l)).collect();
        let mut items: BTreeMap<(Cpu, Layer), Vec<Item>> = BTreeMap::new();
        let mut unloaded: Vec<String> = vec![];
        for m in maps {
            for e in &m.entries {
                let addr = match &e.module {
                    None => e.addr,
                    Some(id) => match base.get(id.as_str()) {
                        // a ColdFire entry is relative to the load address, a SHARC entry to the start of the module's DSP image
                        Some(l) => match (e.cpu, l.dsp_base) {
                            (Cpu::Cf, _) => l.base + e.addr,
                            (Cpu::Sharc, Some(b)) => b + e.addr,
                            (Cpu::Sharc, None) => continue,
                        },
                        None => {
                            if !unloaded.contains(id) {
                                unloaded.push(id.clone());
                            }
                            continue;
                        }
                    },
                };
                items.entry((e.cpu, e.layer)).or_default().push(Item {
                    addr,
                    end: addr + e.size.max(1),
                    name: e.name.clone(),
                    module: e.module.clone(),
                });
            }
        }
        Ok(Resolver {
            index: items
                .into_iter()
                .map(|(k, v)| (k, Index::build(v)))
                .collect(),
            unloaded,
        })
    }

    /// The name of `addr`: the most specific layer that holds it.
    pub fn lookup(&self, cpu: Cpu, addr: u64) -> Option<Hit> {
        for layer in [Layer::Our, Layer::Anchor, Layer::Fw] {
            if let Some(x) = self.index.get(&(cpu, layer)).and_then(|i| i.find(addr)) {
                return Some(Hit {
                    name: x.name.clone(),
                    layer,
                    sym_addr: x.addr,
                    offset: addr - x.addr,
                    size: x.end - x.addr,
                    module: x.module.clone(),
                });
            }
        }
        None
    }

    /// The name of a return address: the call may be the last instruction of its function, so look at the address minus 2 (ColdFire).
    pub fn lookup_return(&self, cpu: Cpu, addr: u64) -> Option<Hit> {
        self.lookup(
            cpu,
            if cpu == Cpu::Cf {
                addr.wrapping_sub(2)
            } else {
                addr
            },
        )
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

#[cfg(test)]
mod tests;
