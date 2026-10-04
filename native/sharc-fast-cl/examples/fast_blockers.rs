//! Which DO-loop bodies of the loaded image the fast tier can build, and what
//! stops the others.
//!
//!     fast_blockers IMAGE STATE START:END[,START:END|PC...]
//!
//! START and END are the loop body's first and last instruction addresses in
//! hex (short words). Each loop is made the active top loop of the engine
//! state STATE (any captured state: only its modifier registers matter, the
//! baked constants), then built with `build_loop` and compiled.
//! A bare PC (no loop) is built as a CFG region with no active loop.
//! Each is built in both kernel shapes: the loop shape (`build_loop`, loops
//! only) and the CFG shape (`build_cfg`). A refusal prints the lowering's
//! reason; an address-range refusal at this generic state is not a form
//! blocker.

use sharc_native::Engine;
use sharc_native::fast::ir::KernelBackend;
use sharc_native::fast::region::{build_cfg, build_loop};
use sharc_native::mem::Mem;
use sharc_native::rt::Loop;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(&args[1]).expect("state");
    let mut e = Engine::from_image(&image).expect("image blob");
    e.import(&state).expect("state blob");
    e.enable_runtime_decode(Mem::read_sw);
    for (k, v) in [
        (1, 1),
        (4, 1),
        (5, 1),
        (6, 573_627_620),
        (9, 1),
        (10, 0),
        (11, 1),
        (12, 1),
        (21, 1),
        (22, 1),
        (23, 0xB8_8AAB),
        (24, 0xB8_8A49),
        (25, 0xB8_8ABB),
    ] {
        assert_eq!(e.set_option(k, v), 0, "option {k}");
    }
    if std::env::var_os("BLK_DBG").is_some() {
        for c in 16..48 {
            let v = e.s.r[c];
            eprintln!("r{c} = {:x}/{:x}", v.b, v.m);
        }
        eprintln!(
            "pc {:x} loops {} mode1 {:x}/{:x}",
            e.s.pc_sw,
            e.s.loops.items().len(),
            e.s.r[114].b,
            e.s.r[114].m
        );
    }
    let mut be = sharc_fast_cl::ClBackend::default();
    let mode = e.s.loops.items().last().map_or(0, |l| l.mode);
    for item in args[2].split(',') {
        let hex = |x: &str| i64::from_str_radix(x.trim_start_matches("0x"), 16).unwrap();
        let (start, end) = match item.split_once(':') {
            Some((a, b)) => (hex(a), Some(hex(b))),
            None => (hex(item), None),
        };
        let s = &mut e.s;
        match end {
            Some(end) => {
                s.loops.n = 1;
                s.loops.a[0] = Loop {
                    start_sw: start,
                    end_sw: end,
                    remaining: 8,
                    mode,
                };
            }
            None => s.loops.n = 0,
        }
        let label = match end {
            Some(end) => format!("{start:#x}..{end:#x}"),
            None => format!("{start:#x}"),
        };
        for shape in ["loop", "cfg"] {
            let built = match shape {
                "loop" if end.is_some() => build_loop(s, start as u32),
                "loop" => continue,
                _ => build_cfg(s, start as u32),
            };
            match built {
                Err(r) => println!("{label} [{shape}]: REFUSED {}", r.0),
                Ok(r) => match be.compile(&r.kernel) {
                    Err(err) => println!("{label} [{shape}]: compile failed {err}"),
                    Ok(_) => {
                        if std::env::var_os("BLK_DBG").is_some() {
                            println!(
                                "  reqs {:?}\n  known {:?} inputs {:?}",
                                r.reqs, r.known, r.inputs
                            );
                        }
                        println!(
                            "{label} [{shape}]: built, {} insns, {} windows, {} reqs, kernel {} ops",
                            r.len(),
                            r.wins.len(),
                            r.reqs.len(),
                            r.kernel.inst_count()
                        );
                    }
                },
            }
        }
    }
}
