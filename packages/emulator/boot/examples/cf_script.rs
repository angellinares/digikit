//! Run a patched ColdFire image through a script of memory operations (the digikit runtime proofs).
//!
//! usage: cf_script SYX SCRIPT.json [--base-sha HEX] [--card IMG] [--state FILE]
//!
//! Boots SYX (a patched image: `--base-sha` names the stock firmware it was made from) with the optional card image to ready,
//! or loads `--state` (a save_state blob, written when missing), then runs the script, a JSON array of operations. The
//! `DIGI_WRITE_WATCH*` and `DIGI_FLUSH_CHECK` variables are read at construction; the summary is written at the end.
//!   {"op":"run","insns":N}                          run about N instructions
//!   {"op":"poke","addr":"0x..","hex":"..."}         host write (not seen by the write watch)
//!   {"op":"poke32","addr":"0x..","value":"0x.."}
//!   {"op":"peek","addr":"0x..","len":N,"tag":"x"}   print the bytes as hex
//!   {"op":"peek","ptr":true,...}                    the address is the word stored at "addr" (one level of indirection)
//!   {"op":"call","pc":"0x..","args":[..],"max":N}   call a function (interrupts masked, like cf_call); prints d0, instruction count
//!   {"op":"wait32","addr":"0x..","eq":V | "ne":V,"max":N,"tag":"x"}   run until the word is (not) V, at most N instructions
//!   {"op":"button","code":N,"down":true|false}     a panel key event;  {"op":"turn","enc":N,"detents":N}  an encoder turn
//!   {"op":"snap","name":"x","addr":"0x..","len":N}  remember a memory range;  {"op":"diff","name":"x","max":N}  print where it differs now
//!   {"op":"overlay","tag":"x"}                      the card sectors the guest has written (count, ranges, a 32-bit sum of each range)
//!   {"op":"probe","pcs":["0x..",..]}                count executions of these addresses from now on;  {"op":"hits","tag":"x"}  print the counts
//! Numbers are strings (hex with 0x or decimal) or integers. Prints one JSON line per operation with the instruction count.

use elektron_native_boot::{CallSpec, Emulator};
use emmc_card::{Card, CardIdentity, RandomAccessRead};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};

struct Backing {
    file: Mutex<fs::File>,
    len: u64,
}
impl RandomAccessRead for Backing {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
        let Ok(mut file) = self.file.lock() else {
            return 0;
        };
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(out))
            .map(|_| out.len())
            .unwrap_or(0)
    }
}

fn num(v: &Value) -> u64 {
    let s = v
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string());
    match s.trim().strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).unwrap(),
        None => s.trim().parse::<u64>().unwrap(),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .map(|i| args[i + 1].clone())
}

fn peek(emu: &mut Emulator, addr: u32, len: usize) -> Vec<u8> {
    emu.peek_bytes(addr, len).unwrap()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: cf_script SYX SCRIPT.json [--base-sha HEX] [--card IMG] [--state FILE]");
        std::process::exit(2);
    }
    let syx = fs::read(&args[1]).unwrap();
    let script: Value = serde_json::from_slice(&fs::read(&args[2]).unwrap()).unwrap();
    let card = flag(&args, "--card").map(|p| {
        let file = fs::File::open(&p).unwrap();
        let len = file.metadata().unwrap().len();
        Card::with_backing(
            CardIdentity::from_env().unwrap(),
            Some(Box::new(Backing {
                file: Mutex::new(file),
                len,
            })),
        )
        .unwrap()
    });
    let mut emu = match flag(&args, "--base-sha") {
        Some(b) => Emulator::new_patched(&syx, card, &b).unwrap(),
        None => Emulator::new(&syx, card).unwrap(),
    };
    let state = flag(&args, "--state");
    let loaded = match &state {
        Some(p) if fs::metadata(p).is_ok() => {
            emu.load_state(&fs::read(p).unwrap()).unwrap();
            true
        }
        _ => false,
    };
    if !loaded {
        loop {
            let snap = emu.step_chunk(250_000);
            let s = &snap.status;
            if s.ready {
                break;
            }
            if s.error.is_some() || s.icount >= 2_000_000_000 {
                eprintln!("boot failed: {:?} at {}", s.error, s.icount);
                std::process::exit(3);
            }
        }
        if let Some(p) = &state {
            fs::write(p, emu.save_state().unwrap()).unwrap();
        }
    }
    println!("{}", json!({"op": "ready", "icount": emu.icount()}));
    let step = |emu: &mut Emulator, n: u64| {
        let end = emu.icount() + n;
        while emu.icount() < end {
            let left = (end - emu.icount()).min(100_000) as u32;
            let snap = emu.step_chunk(left);
            if let Some(e) = &snap.status.error {
                eprintln!("emulator error: {e}");
                std::process::exit(4);
            }
        }
    };
    let mut snaps: std::collections::HashMap<String, (u32, Vec<u8>)> = Default::default();
    for op in script.as_array().unwrap() {
        let name = op["op"].as_str().unwrap();
        let tag = op.get("tag").cloned().unwrap_or(Value::Null);
        match name {
            "run" => {
                let t0 = emu.icount();
                step(&mut emu, num(&op["insns"]));
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "ran": emu.icount() - t0})
                );
            }
            "poke" => {
                let bytes: Vec<u8> = op["hex"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .chunks(2)
                    .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
                    .collect();
                emu.poke_bytes(num(&op["addr"]) as u32, &bytes).unwrap();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "poke32" => {
                emu.poke32(num(&op["addr"]) as u32, num(&op["value"]) as u32)
                    .unwrap();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "peek" => {
                let mut addr = num(&op["addr"]) as u32;
                if op.get("ptr").is_some_and(|v| v.as_bool() == Some(true)) {
                    addr = u32::from_be_bytes(peek(&mut emu, addr, 4).try_into().unwrap());
                }
                let b = peek(&mut emu, addr, num(&op["len"]) as usize);
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "hex": hex})
                );
            }
            "call" => {
                let spec = CallSpec {
                    pc: num(&op["pc"]) as u32,
                    args: op["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|v| num(v) as u32)
                        .collect(),
                    sp: op.get("sp").map_or(0x47fe_0000, |v| num(v) as u32),
                    ret_pc: op.get("ret").map_or(0x47fd_f000, |v| num(v) as u32),
                    sr: op.get("sr").map_or(0x2700, |v| num(v) as u16),
                    max_insns: op.get("max").map_or(50_000_000, num),
                };
                let r = emu.call_function(&spec).unwrap();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "ok": r.finished && r.error.is_none(), "d0": r.d[0],
                           "insns": r.insns, "error": r.error})
                );
            }
            "button" => {
                emu.button(num(&op["code"]) as u8, op["down"].as_bool().unwrap())
                    .unwrap();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "turn" => {
                let d = op["detents"].as_i64().unwrap() as i32;
                emu.turn(num(&op["enc"]) as u8, d).unwrap();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "snap" => {
                let (addr, len) = (num(&op["addr"]) as u32, num(&op["len"]) as usize);
                let mut buf = Vec::with_capacity(len);
                while buf.len() < len {
                    let n = (len - buf.len()).min(1 << 20);
                    buf.extend(peek(&mut emu, addr + buf.len() as u32, n));
                }
                snaps.insert(op["name"].as_str().unwrap().to_owned(), (addr, buf));
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "diff" => {
                let (addr, old) = snaps[op["name"].as_str().unwrap()].clone();
                let mut now = Vec::with_capacity(old.len());
                while now.len() < old.len() {
                    let n = (old.len() - now.len()).min(1 << 20);
                    now.extend(peek(&mut emu, addr + now.len() as u32, n));
                }
                // changed ranges, merged when closer than 64 bytes
                let mut ranges: Vec<(usize, usize)> = Vec::new();
                for (i, (a, b)) in old.iter().zip(&now).enumerate() {
                    if a != b {
                        match ranges.last_mut() {
                            Some(r) if i - r.1 < 64 => r.1 = i + 1,
                            _ => ranges.push((i, i + 1)),
                        }
                    }
                }
                let max = op.get("max").map_or(40, |v| num(v) as usize);
                let shown: Vec<Value> = ranges
                    .iter()
                    .take(max)
                    .map(|&(a, b)| {
                        let n = (b - a).min(16);
                        let hx = |v: &[u8]| v[a..a + n].iter().map(|x| format!("{x:02x}")).collect::<String>();
                        json!({"addr": format!("{:#x}", addr as usize + a), "len": b - a, "old": hx(&old), "new": hx(&now)})
                    })
                    .collect();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "ranges": ranges.len(),
                           "bytes": ranges.iter().map(|r| r.1 - r.0).sum::<usize>(), "first": shown})
                );
            }
            "overlay" => {
                let mut secs: Vec<u64> = emu.card_overlay().iter().map(|o| o.sector).collect();
                secs.sort_unstable();
                let mut runs: Vec<(u64, u64)> = Vec::new();
                for s in secs {
                    match runs.last_mut() {
                        Some(r) if s == r.1 => r.1 = s + 1,
                        _ => runs.push((s, s + 1)),
                    }
                }
                let shown: Vec<String> = runs
                    .iter()
                    .take(60)
                    .map(|r| format!("{:#x}+{}", r.0, r.1 - r.0))
                    .collect();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "runs": runs.len(), "first": shown})
                );
            }
            "probe" => {
                let pcs: Vec<u32> = op["pcs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| num(v) as u32)
                    .collect();
                emu.probe_pcs(&pcs);
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount()})
                );
            }
            "hits" => {
                let h: Vec<Value> = emu
                    .probe_hits()
                    .iter()
                    .map(|(pc, n)| json!([format!("{pc:#x}"), n]))
                    .collect();
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "hits": h})
                );
            }
            "wait32" => {
                let addr = num(&op["addr"]) as u32;
                let max = num(&op["max"]);
                let t0 = emu.icount();
                let mut done = false;
                while emu.icount() - t0 < max {
                    step(&mut emu, 20_000);
                    let w = u32::from_be_bytes(peek(&mut emu, addr, 4).try_into().unwrap()) as u64;
                    if op.get("eq").is_some_and(|v| w == num(v))
                        || op.get("ne").is_some_and(|v| w != num(v))
                    {
                        done = true;
                        break;
                    }
                }
                println!(
                    "{}",
                    json!({"op": name, "tag": tag, "icount": emu.icount(), "waited": emu.icount() - t0, "ok": done})
                );
            }
            _ => panic!("unknown op {name}"),
        }
    }
    emu.finish_write_watch();
}
