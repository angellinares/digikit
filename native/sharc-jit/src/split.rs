//! Split a `wasm-frames` build into a runtime module and one module per
//! block region: the shape a run-time block translator produces.
//!
//! Every block entry (`image::blocks_NN::b_*`, reached through the table
//! by `Dispatch`) is grouped with the region function it calls (`r_*`);
//! each group, with every other generated-image function it reaches
//! (copied, so shared variants are duplicated), becomes one module. A
//! block module imports the runtime's memory, table, globals and the
//! runtime functions it calls (`env.m`, `env.t0`, `env.g<i>`, `env.f<i>`)
//! and exports its entries (`e<table slot>`). The host installs those in
//! the runtime's table, so `Dispatch` calls them across modules.

use crate::wasmscan::{self, FuncType, Module, Ref};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, FunctionSection, GlobalType, ImportSection,
    MemoryType, RawSection, RefType, TableType, TypeSection, ValType,
};

/// Generated-image code (v0 mangling of `sharc_native::generated::image`).
pub fn is_image(name: &str) -> bool {
    name.contains("9generated5image")
}

fn valtype(b: u8) -> ValType {
    match b {
        0x7f => ValType::I32,
        0x7e => ValType::I64,
        0x7d => ValType::F32,
        0x7c => ValType::F64,
        0x7b => ValType::V128,
        0x70 => ValType::Ref(RefType::FUNCREF),
        0x6f => ValType::Ref(RefType::EXTERNREF),
        _ => panic!("value type {b:#x}"),
    }
}

/// One block module.
pub struct Part {
    /// Its region function's name (or the entry's, without one).
    pub name: String,
    /// (table slot, export name) of each block entry.
    pub entries: Vec<(u32, String)>,
    pub bytes: Vec<u8>,
    /// Functions copied into it.
    pub functions: usize,
}

pub struct Split {
    /// The runtime module: the input with the exports block modules need.
    pub runtime: Vec<u8>,
    pub parts: Vec<Part>,
}

fn callees(m: &Module, f: u32) -> Vec<u32> {
    wasmscan::refs(m.body(f))
        .into_iter()
        .filter_map(|r| match r {
            Ref::Func(x) => Some(x),
            _ => None,
        })
        .collect()
}

pub fn split(bytes: &[u8]) -> Split {
    let m = Module::parse(bytes);
    let name = |f: u32| m.names.get(&f).map(String::as_str).unwrap_or("");
    let imported = m.n_imported_funcs;
    let local_image = |f: u32| f >= imported && is_image(name(f));
    // Entries: table functions in the generated image, grouped by region.
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut entries: Vec<u32> = m
        .table_slot
        .keys()
        .copied()
        .filter(|&f| local_image(f))
        .collect();
    entries.sort();
    for &e in &entries {
        let region = callees(&m, e)
            .into_iter()
            .find(|&c| local_image(c) && name(c).contains("r_"))
            .unwrap_or(e);
        groups.entry(region).or_default().push(e);
    }
    let mut need_funcs: BTreeSet<u32> = BTreeSet::new();
    let mut need_globals: BTreeSet<u32> = BTreeSet::new();
    let mut parts = Vec::new();
    for (&region, ents) in &groups {
        // Closure over generated-image callees.
        let mut set: Vec<u32> = ents.clone();
        if !set.contains(&region) {
            set.push(region);
        }
        let mut k = 0;
        while k < set.len() {
            for c in callees(&m, set[k]) {
                if local_image(c) && !set.contains(&c) {
                    set.push(c);
                }
            }
            k += 1;
        }
        // Index spaces of the new module.
        let mut types: Vec<FuncType> = Vec::new();
        let mut type_ix: HashMap<u32, u32> = HashMap::new();
        let mut ty = |t: u32, types: &mut Vec<FuncType>| -> u32 {
            *type_ix.entry(t).or_insert_with(|| {
                types.push(m.types[t as usize].clone());
                types.len() as u32 - 1
            })
        };
        let mut imp_funcs: Vec<u32> = Vec::new();
        let mut imp_globals: Vec<u32> = Vec::new();
        for &f in &set {
            for r in wasmscan::refs(m.body(f)) {
                match r {
                    Ref::Func(c) if !set.contains(&c) && !imp_funcs.contains(&c) => {
                        imp_funcs.push(c)
                    }
                    Ref::Global(g) if !imp_globals.contains(&g) => imp_globals.push(g),
                    Ref::Table(t) => assert_eq!(t, 0, "one table"),
                    _ => {}
                }
            }
        }
        let fix = |f: u32| -> u32 {
            match imp_funcs.iter().position(|&x| x == f) {
                Some(i) => i as u32,
                None => imp_funcs.len() as u32 + set.iter().position(|&x| x == f).unwrap() as u32,
            }
        };
        let mut imports = ImportSection::new();
        imports.import(
            "env",
            "m",
            MemoryType {
                minimum: 0,
                maximum: None,
                memory64: false,
                shared: false,
                page_size_log2: None,
            },
        );
        imports.import(
            "env",
            "t0",
            TableType {
                element_type: RefType::FUNCREF,
                table64: false,
                minimum: 0,
                maximum: None,
                shared: false,
            },
        );
        for &g in &imp_globals {
            let (vt, mutable) = m.globals[g as usize];
            imports.import(
                "env",
                &format!("g{g}"),
                GlobalType {
                    val_type: valtype(vt),
                    mutable,
                    shared: false,
                },
            );
            need_globals.insert(g);
        }
        for &c in &imp_funcs {
            let t = ty(m.func_types[c as usize], &mut types);
            imports.import("env", &format!("f{c}"), EntityType::Function(t));
            need_funcs.insert(c);
        }
        let mut funcs = FunctionSection::new();
        let mut code = CodeSection::new();
        for &f in &set {
            funcs.function(ty(m.func_types[f as usize], &mut types));
        }
        for &f in &set {
            let body = wasmscan::rewrite(m.body(f), &mut |r| match r {
                Ref::Func(c) => fix(c),
                Ref::Global(g) => imp_globals.iter().position(|&x| x == g).unwrap() as u32,
                Ref::Type(t) => ty(t, &mut types),
                Ref::Table(_) => 0,
            });
            code.raw(&body);
        }
        let mut type_sec = TypeSection::new();
        for t in &types {
            type_sec.ty().function(
                t.params.iter().map(|&b| valtype(b)),
                t.results.iter().map(|&b| valtype(b)),
            );
        }
        let mut exports = ExportSection::new();
        let mut ent = Vec::new();
        for &e in ents {
            let slot = m.table_slot[&e];
            let nm = format!("e{slot}");
            exports.export(&nm, ExportKind::Func, fix(e));
            ent.push((slot, nm));
        }
        let mut out = wasm_encoder::Module::new();
        out.section(&type_sec)
            .section(&imports)
            .section(&funcs)
            .section(&exports)
            .section(&code);
        parts.push(Part {
            name: name(region)
                .rsplit("image")
                .next()
                .unwrap_or("")
                .to_string(),
            entries: ent,
            bytes: out.finish(),
            functions: set.len(),
        });
    }
    // The runtime: the input with the extra exports.
    let mut runtime = wasm_encoder::Module::new();
    for (id, r) in &m.sections {
        if *id == 7 {
            let mut data = Vec::new();
            let extra = 2 + need_funcs.len() + need_globals.len();
            wasmscan::put_uleb(&mut data, (m.exports.len() + extra) as u64);
            let mut p = r.start;
            wasmscan::uleb(bytes, &mut p);
            data.extend_from_slice(&bytes[p..r.end]);
            let mut ex = ExportSection::new();
            let mem = m.exports.iter().find(|e| e.1 == 2).expect("memory export");
            ex.export("m", ExportKind::Memory, mem.2);
            ex.export("t0", ExportKind::Table, 0);
            for &g in &need_globals {
                ex.export(&format!("g{g}"), ExportKind::Global, g);
            }
            for &f in &need_funcs {
                ex.export(&format!("f{f}"), ExportKind::Func, f);
            }
            // ExportSection encodes "count, entries": keep the entries.
            let mut enc = Vec::new();
            wasm_encoder::Encode::encode(&ex, &mut enc);
            let mut q = 0;
            wasmscan::uleb(&enc, &mut q); // section size
            wasmscan::uleb(&enc, &mut q); // count
            data.extend_from_slice(&enc[q..]);
            runtime.section(&RawSection { id: 7, data: &data });
        } else {
            runtime.section(&RawSection {
                id: *id,
                data: &bytes[r.clone()],
            });
        }
    }
    Split {
        runtime: runtime.finish(),
        parts,
    }
}
