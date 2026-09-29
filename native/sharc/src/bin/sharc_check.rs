//! Block check: run each generated block from the entry state of every
//! captured vector (tools/sharc_rsvec.py) and compare what it leaves with
//! what the Python reference left.
//!
//!     sharc-check out/native/vectors.txt [-v]

use sharc_native::mem::Mem;
use sharc_native::vectors::{self, Vector};
use sharc_native::{EXIT_BUDGET, EXIT_NEXT, EXIT_TRAP, Engine, trap_name};
use std::collections::BTreeMap;

fn run(e: &mut Engine, v: &Vector) -> Option<Vec<String>> {
    let f = e.dispatch.get(v.block as i128)?;
    e.s.mem.reset();
    vectors::load_entry(&mut e.s, v);
    e.s.icount = 0;
    // The reference stopped when control left the block: a region runs on
    // into the next block of its own, so give it exactly that many
    // instructions (it then leaves with EXIT_BUDGET at the next block).
    e.s.limit = v.instructions;
    // Block code runs only where the dispatcher would run it.
    let code = if e.s.cfg.block_ok && e.s.loops_ok {
        f(&mut e.s)
    } else {
        EXIT_BUDGET
    };
    let done = code == EXIT_NEXT || (code == EXIT_BUDGET && e.s.icount == v.instructions);
    let mut diffs = Vec::new();
    if code == EXIT_TRAP {
        let t = e.s.trap.map(trap_name).unwrap_or_default();
        diffs.push(format!("trapped at {:#x}: {t}", e.s.pc_sw));
    } else if !done {
        diffs.push(format!("block did not run (exit code {code})"));
    }
    diffs.extend(vectors::compare(&e.s, v, e.s.pc_sw as u32));
    if done && e.s.icount != v.instructions {
        diffs.push(format!(
            "instructions {} != reference {}",
            e.s.icount, v.instructions
        ));
    }
    Some(diffs)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("out/native/vectors.txt");
    let verbose = args.iter().any(|a| a == "-v");
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let vs = vectors::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut e = Engine::new(Mem::new());
    if e.dispatch.count == 0 {
        eprintln!("no block code: build with SHARC_GEN_DIR (tools/sharc_rsgen.py output)");
        std::process::exit(2);
    }
    let mut per_block: BTreeMap<u32, (u32, u32, u64)> = BTreeMap::new();
    let (mut ok, mut bad, mut skipped) = (0, 0, 0);
    for v in &vs {
        let Some(diffs) = run(&mut e, v) else {
            skipped += 1;
            continue;
        };
        let entry = per_block.entry(v.block).or_default();
        entry.2 += v.instructions;
        if diffs.is_empty() {
            ok += 1;
            entry.0 += 1;
        } else {
            bad += 1;
            entry.1 += 1;
            println!("MISMATCH block {:#x} sample {}:", v.block, v.sample);
            for d in diffs.iter().take(if verbose { 1000 } else { 8 }) {
                println!("    {d}");
            }
        }
    }
    println!("block      pass fail  ref-instructions");
    for (b, (p, f, n)) in &per_block {
        println!("{b:#08x}  {p:4} {f:4}  {n}");
    }
    println!(
        "{ok} vectors match, {bad} differ, {skipped} without a block; {} blocks checked",
        per_block.len()
    );
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
