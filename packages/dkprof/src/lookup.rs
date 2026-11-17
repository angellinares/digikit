//! Address lookup and load table decode: JSON requests and answers (the same shapes `patchkit addr` prints, docs/ADDRMAP.md).

use dkaddrmap::snapshot::{self, Snapshot};
use dkaddrmap::{AddrMap, Cpu, LoadTable, Resolver};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Deserialize)]
pub struct Addr {
    pub cpu: Cpu,
    pub addr: u64,
}

#[derive(Deserialize)]
pub struct LookupReq {
    pub maps: Vec<AddrMap>,
    #[serde(default)]
    pub loads: LoadTable,
    /// Raw bytes of runtime symbols as hex (`DKRT_STATE`, `DKRT_DSP`, `dyndsp_info`); decoded with the loader header of the maps.
    #[serde(default)]
    pub snapshot: BTreeMap<String, String>,
    /// Module id to the software PC where its DSP image starts.
    #[serde(default)]
    pub dsp_base: BTreeMap<String, u64>,
    /// Look up the address minus 2 (a return address).
    #[serde(default)]
    pub ret: bool,
    pub addrs: Vec<Addr>,
}

fn decode(maps: &[AddrMap], snapshot: &BTreeMap<String, String>) -> Result<LoadTable, String> {
    let loader = dkaddrmap::loader_of(maps)
        .ok_or("a snapshot needs a ship map (it holds the loader header)")?;
    let mut snap = Snapshot::new();
    for (sym, hex) in snapshot {
        snap.insert(
            sym.clone(),
            snapshot::parse_hex(hex).map_err(|e| format!("snapshot {sym}: {e}"))?,
        );
    }
    snapshot::decode_loads(loader, &snap)
}

/// `addr_lookup`: `{"unloaded": [...], "results": [{"cpu", "addr", "name", "text", "layer", "sym_addr", "offset", "size", "module"}]}`; `name` is null
/// when no layer holds the address.
pub fn addr_lookup(req: &LookupReq) -> Result<Value, String> {
    let mut loads = req.loads.clone();
    if !req.snapshot.is_empty() {
        loads
            .modules
            .extend(decode(&req.maps, &req.snapshot)?.modules);
    }
    for (id, pc) in &req.dsp_base {
        match loads.modules.iter_mut().find(|l| &l.id == id) {
            Some(l) => l.dsp_base = Some(*pc),
            None => return Err(format!("dsp_base {id}: the module is not loaded")),
        }
    }
    let r = Resolver::new(&req.maps, &loads)?;
    let results: Vec<Value> = req
        .addrs
        .iter()
        .map(|a| {
            let hit = if req.ret {
                r.lookup_return(a.cpu, a.addr)
            } else {
                r.lookup(a.cpu, a.addr)
            };
            json!({
                "cpu": a.cpu, "addr": a.addr,
                "name": hit.as_ref().map(|h| &h.name),
                "text": hit.as_ref().map(|h| h.text()),
                "layer": hit.as_ref().map(|h| h.layer),
                "sym_addr": hit.as_ref().map(|h| h.sym_addr),
                "offset": hit.as_ref().map(|h| h.offset),
                "size": hit.as_ref().map(|h| h.size),
                "module": hit.as_ref().and_then(|h| h.module.clone()),
            })
        })
        .collect();
    Ok(json!({"unloaded": r.unloaded, "results": results}))
}

/// The text view of an `addr_lookup` answer: `0xADDR name+0xOFF` per line (`?` when unnamed).
pub fn lookup_text(answer: &Value) -> String {
    let mut s = String::new();
    for r in answer["results"].as_array().into_iter().flatten() {
        let _ = writeln!(
            s,
            "{:#x} {}",
            r["addr"].as_u64().unwrap_or(0),
            r["text"].as_str().unwrap_or("?")
        );
    }
    s
}

#[derive(Deserialize)]
pub struct LoadTableReq {
    pub maps: Vec<AddrMap>,
    /// Raw bytes of runtime symbols as hex. Without them the answer lists what to read.
    #[serde(default)]
    pub snapshot: BTreeMap<String, String>,
}

/// `load_table`: the load table of a snapshot, or (no snapshot) the symbols and byte counts to read.
pub fn load_table(req: &LoadTableReq) -> Result<Value, String> {
    if req.snapshot.is_empty() {
        let l = dkaddrmap::loader_of(&req.maps).ok_or(
            "no map describes the load table (a ship map from `patchkit apply --addrmap` does)",
        )?;
        return Ok(json!({"reads": l.reads()}));
    }
    Ok(json!(decode(&req.maps, &req.snapshot)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dkaddrmap::snapshot::Loader;
    use dkaddrmap::{Entry, Key, Layer};

    fn ship() -> AddrMap {
        AddrMap::new(
            "ship",
            Key {
                loader: Some(Loader {
                    modules: 2,
                    l2_byte_base: 0x2000_0000,
                    l2_sw_base: 0xb8_0000,
                    dsp: false,
                    providers: 0,
                }),
                ..Key::default()
            },
            vec![Entry {
                cpu: Cpu::Cf,
                addr: 0x4000_0000,
                size: 0x10,
                name: "rt".into(),
                layer: Layer::Our,
                module: None,
            }],
        )
    }

    #[test]
    fn lookup_names_an_address_and_a_return_address() {
        let req = LookupReq {
            maps: vec![ship()],
            loads: LoadTable::default(),
            snapshot: BTreeMap::new(),
            dsp_base: BTreeMap::new(),
            ret: true,
            addrs: vec![
                Addr {
                    cpu: Cpu::Cf,
                    addr: 0x4000_0010,
                },
                Addr {
                    cpu: Cpu::Cf,
                    addr: 0x10,
                },
            ],
        };
        let a = addr_lookup(&req).unwrap();
        assert_eq!(lookup_text(&a), "0x40000010 rt+0xe\n0x10 ?\n");
        assert!(a["results"][1]["name"].is_null());
    }

    #[test]
    fn a_snapshot_is_decoded_with_the_loader_header_and_without_one_the_reads_are_listed() {
        let reads = load_table(&LoadTableReq {
            maps: vec![ship()],
            snapshot: BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(reads["reads"][0]["symbol"], "DKRT_STATE");
        assert_eq!(reads["reads"][0]["len"], 40 + 52 * 2);
        let zero = "00".repeat(40 + 52 * 2);
        let t = load_table(&LoadTableReq {
            maps: vec![ship()],
            snapshot: BTreeMap::from([("DKRT_STATE".to_string(), zero)]),
        })
        .unwrap();
        assert_eq!(t, json!({"modules": []}));
        let none = load_table(&LoadTableReq {
            maps: vec![],
            snapshot: BTreeMap::new(),
        });
        assert!(none.is_err());
    }
}
