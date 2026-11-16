//! The SHARC-only rig: a call harness for module routines on the generated native core.
//!
//! A request (JSON, see [`run_request`]) names code to load, DM writes to make and calls to run. The caller does the
//! work that needs a toolchain (assembling, linking, placing sections) and the work that is module logic (which words
//! a note sets, which context a block gets); the rig only runs what it is told on the core and answers with what the
//! calls left. The instruction semantics are the generated core's: nothing is decoded or executed here.
//!
//! ```text
//! request  {"return_sw": 12120320,                          // pc a routine returns to (ends the call, not executed)
//!           "breaks":    [12120064],                         // more pcs that end a call (not executed)
//!           "watch":     [[11862016, 11862400]],             // optional: record the stores of instructions at these pcs
//!           "image":     "...",                              // optional: a DSP boot stream (the stock image) loaded first, hex
//!           "code": [{"address": 671088640, "hex": "..."}],  // loader byte address, code in boot-stream order (over the image)
//!           "ops": [ {"poke": [[word_address, word], ...]},  // DM writes, normal-word addresses
//!                    {"call": {"pc": 1, "regs": {"R4": 5}, "max_steps": 400000,
//!                              "reads": [[word_address, count]], "read_regs": ["I6"],
//!                              "stock_reads": [[loader_byte_address, count]],   // optional: bytes of the image as loaded
//!                              "breaks": [pc], "resume": true,                    // optional, see below
//!                              "specials": {"MSF": ["mask", "bits"]}, "read_specials": ["MSF"]}} ]}
//! response {"build": "...", "results": [{"instructions": n, "halt": "breakpoint", "final_pc": pc,
//!                                        "regs": {"I6": v|null}, "reads": [[v|null, ...]],
//!                                        "stores": [[pc, address, width], ...],
//!                                        "stock": ["hex"|null, ...],   // only for a call with "stock_reads"
//!                                        "specials": {"MSF": ["mask","bits"]|null},   // only for "read_specials"
//!                                        "profile": [[pc, count], ...]}]}   // only for a call with "profile": true
//! ```
//!
//! A call is the same as `CallSession.call` of tools/patchkit/pk_sharc.py: the named registers are set, every other
//! register and all of DM stay as the last call left them, a call at `pc` returns to `return_sw`, and
//! the run stops before it executes an instruction at `return_sw` or in `breaks`. `instructions` counts the
//! instructions executed. `halt` is `breakpoint`, `max-steps` or `trap: <reason>`. `stores` lists, in execution order, the
//! runs of bytes that instructions at a `watch` pc stored (addresses as the core keeps them: a byte that nothing loaded
//! or poked is at 0x2800_0000 + its address). Nothing is recorded, and nothing costs, without `watch`.
//! A call with `"profile": true` also answers `profile`: one `[pc, count]` row per pc the call executed, in pc order (the counts
//! add up to `instructions`; no key without the option, and the loop only counts when it is on).
//!
//! `image` is a boot stream as the firmware keeps it (section 7 of an image): it is parsed by the boot crate's own
//! `LoadedMemory::from_stream` (later blocks win, FILL blocks repeat their argument) and loaded before `code`, so the code
//! is placed over the stock bytes as `CallSession(base_memory=...)` does. `stock_reads` answers bytes of that image as it
//! was loaded (before any `code`, poke or store), `null` for a range the stream does not fully write.
//!
//! A call with `breaks` stops at those pcs and `return_sw` instead of the request's `breaks`. A call with `resume: true`
//! continues from the pc the last call stopped at (no new call frame, the stacks and registers stay; `regs` are still
//! set), as `Runner.run` does after a breakpoint.
//!
//! `specials` sets the 80-bit multiplier registers MRF, MRB, MSF, MSB before a call (`{"MSF": ["mask hex", "bits hex"]}`, every bit
//! known when the mask is 2^80 - 1) and `read_specials: ["MSF"]` answers `specials`, the same pairs after the call (`null` for a
//! register the core does not hold).
//!
//! `pokem` changes some bits of a word (a halfword or a byte lane of a frame word): a word that nothing wrote reads as 0, as
//! `CallSession.peek(addr) or 0` does in the Python rigs.
//!
//! One request runs on a fresh core and ends. `sharc-rig --serve` keeps the core: its first line is a request (code, return
//! point, breaks, watch, optionally ops), every later line is `{"ops": [...]}`, and each line gets one response line
//! (`{"error": "..."}` for a line it cannot run; the core stays as it was). The caller then runs a case block by block and
//! stops at the first failing block, as the Python rigs do.

mod json;

// The boot crate's stream reader, compiled in place (a dependency on the crate would change Cargo.lock, which is an
// input of the device tests). `dsp_stream` uses `crate::dsp_patch`.
#[allow(dead_code)]
#[path = "../../emulator/boot/src/dsp_patch.rs"]
mod dsp_patch;
#[allow(dead_code)]
#[path = "../../emulator/boot/src/dsp_stream.rs"]
mod dsp_stream;

use dsp_stream::LoadedMemory;
use json::Json;
use sharc_native::mem::Mem;
use sharc_native::rt::bnd;
use sharc_native::rt::{Int, MR, Spec, V, VI};
use sharc_native::{Engine, exec_insn, trap_name, vectors};

/// Registers a fresh core holds (tools/sharc_dev/sharc_run.py `make_state`: every register 0, then these).
const START_REGS: [(&str, i64); 6] = [
    ("M5", 0),
    ("M6", 1),
    ("M7", -1),
    ("M13", 0),
    ("M14", 1),
    ("M15", -1),
];
/// The core MMRs that reset to 0 (`CORE_MMR_RESET_VALUES`).
const RESET_MMRS: [u32; 6] = [0x30024, 0x31400, 0x31401, 0x31402, 0x3e000, 0x3e002];

#[derive(Debug)]
pub struct Call {
    pub pc: u32,
    pub regs: Vec<(usize, u32)>,
    pub max_steps: u64,
    pub reads: Vec<(u32, u32)>,
    pub read_regs: Vec<(String, usize)>,
    /// count the instructions executed at each pc (`CallResult::profile`)
    pub profile: bool,
    /// (loader byte address, count) of the image bytes to answer (`CallResult::stock`)
    pub stock_reads: Vec<(u32, u32)>,
    /// pcs that end this call instead of the request's `breaks`
    pub breaks: Option<Vec<u32>>,
    /// continue from the pc the last call stopped at
    pub resume: bool,
    /// 80-bit multiplier registers set before the call: (slot, mask, bits) of MRF, MRB, MSF or MSB
    pub specials: Vec<(usize, Int, Int)>,
    /// multiplier registers answered after the call: (name, slot)
    pub read_specials: Vec<(String, usize)>,
}

#[derive(Debug)]
pub enum Op {
    Poke(Vec<(u32, u32)>),
    /// (word address, mask, value): the new word is `(old & !mask) | (value & mask)`
    PokeMasked(Vec<(u32, u32, u32)>),
    Call(Call),
}

#[derive(Debug)]
pub struct Request {
    pub return_sw: u32,
    pub breaks: Vec<u32>,
    pub watch: Vec<(u32, u32)>,
    pub code: Vec<(u32, Vec<u8>)>,
    /// a boot stream loaded before `code`
    pub image: Option<Vec<u8>>,
    pub ops: Vec<Op>,
}

pub struct CallResult {
    pub instructions: u64,
    pub halt: String,
    pub final_pc: u32,
    pub regs: Vec<(String, Option<u32>)>,
    pub reads: Vec<Vec<Option<u32>>>,
    /// (pc, address, width) of the stores of instructions in the request's `watch` ranges
    pub stores: Vec<(u32, u32, u32)>,
    /// (pc, instructions executed there) in pc order, for a call with `profile`; empty otherwise
    pub profile: Vec<(u32, u64)>,
    /// the `stock_reads` of the call: the bytes, or None when the stream does not write all of them
    pub stock: Vec<Option<Vec<u8>>>,
    /// the `read_specials` of the call: (name, (mask, bits) when the register is known to the core)
    pub specials: Vec<(String, Option<(Int, Int)>)>,
}

fn reg_code(name: &str) -> Result<usize, String> {
    (0..128)
        .find(|&c| vectors::ureg_name(c) == name)
        .ok_or_else(|| format!("unknown register {name}"))
}

/// The slot of a multiplier register in `St::special` (MRF, MRB, MSF, MSB follow each other).
fn special_slot(name: &str) -> Result<usize, String> {
    ["MRF", "MRB", "MSF", "MSB"]
        .iter()
        .position(|&n| n == name)
        .ok_or_else(|| format!("{name} is not a multiplier register (MRF, MRB, MSF, MSB)"))
}

fn hex_int(v: &Json) -> Result<Int, String> {
    Int::from_str_radix(v.str()?, 16).map_err(|_| "a multiplier word is a hex string".to_string())
}

fn hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("hex string of odd length".into());
    }
    (0..text.len() / 2)
        .map(|i| {
            u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| "bad hex digit".to_string())
        })
        .collect()
}

fn u32_of(v: &Json) -> Result<u32, String> {
    let n = v.int()?;
    // words arrive as unsigned or as signed 32-bit values
    if (i32::MIN as i64..=u32::MAX as i64).contains(&n) {
        Ok(n as u32)
    } else {
        Err(format!("{n} is not a 32-bit word"))
    }
}

/// The ops of a request, or of a `--serve` line.
pub fn parse_ops(list: &Json) -> Result<Vec<Op>, String> {
    let mut ops = Vec::new();
    for op in list.arr()? {
        if let Some(p) = op.get("poke") {
            let mut pairs = Vec::new();
            for pair in p.arr()? {
                let pair = pair.arr()?;
                if pair.len() != 2 {
                    return Err("a poke is [word_address, word]".into());
                }
                pairs.push((u32_of(&pair[0])?, u32_of(&pair[1])?));
            }
            ops.push(Op::Poke(pairs));
        } else if let Some(p) = op.get("pokem") {
            let mut triples = Vec::new();
            for t in p.arr()? {
                let t = t.arr()?;
                if t.len() != 3 {
                    return Err("a masked poke is [word_address, mask, value]".into());
                }
                triples.push((u32_of(&t[0])?, u32_of(&t[1])?, u32_of(&t[2])?));
            }
            ops.push(Op::PokeMasked(triples));
        } else if let Some(c) = op.get("call") {
            let mut regs = Vec::new();
            if let Some(r) = c.get("regs") {
                for (name, v) in r.obj()? {
                    regs.push((reg_code(name)?, u32_of(v)?));
                }
            }
            let mut reads = Vec::new();
            if let Some(r) = c.get("reads") {
                for pair in r.arr()? {
                    let pair = pair.arr()?;
                    if pair.len() != 2 {
                        return Err("a read is [word_address, count]".into());
                    }
                    reads.push((u32_of(&pair[0])?, u32_of(&pair[1])?));
                }
            }
            let mut specials = Vec::new();
            if let Some(r) = c.get("specials") {
                for (name, v) in r.obj()? {
                    let pair = v.arr()?;
                    if pair.len() != 2 {
                        return Err("a multiplier register is [mask, bits] as hex strings".into());
                    }
                    specials.push((special_slot(name)?, hex_int(&pair[0])?, hex_int(&pair[1])?));
                }
            }
            let mut read_specials = Vec::new();
            if let Some(r) = c.get("read_specials") {
                for n in r.arr()? {
                    let n = n.str()?;
                    read_specials.push((n.to_string(), special_slot(n)?));
                }
            }
            let mut stock_reads = Vec::new();
            if let Some(r) = c.get("stock_reads") {
                for pair in r.arr()? {
                    let pair = pair.arr()?;
                    if pair.len() != 2 {
                        return Err("a stock read is [loader_byte_address, count]".into());
                    }
                    stock_reads.push((u32_of(&pair[0])?, u32_of(&pair[1])?));
                }
            }
            let breaks = match c.get("breaks") {
                Some(b) => Some(b.arr()?.iter().map(u32_of).collect::<Result<_, _>>()?),
                None => None,
            };
            let mut read_regs = Vec::new();
            if let Some(r) = c.get("read_regs") {
                for n in r.arr()? {
                    let n = n.str()?;
                    read_regs.push((n.to_string(), reg_code(n)?));
                }
            }
            ops.push(Op::Call(Call {
                pc: u32_of(c.get("pc").ok_or("call has no pc")?)?,
                regs,
                max_steps: c.get("max_steps").map_or(Ok(100_000), Json::int)? as u64,
                reads,
                read_regs,
                profile: match c.get("profile") {
                    Some(Json::Bool(b)) => *b,
                    Some(_) => return Err("profile is true or false".into()),
                    None => false,
                },
                stock_reads,
                breaks,
                specials,
                read_specials,
                resume: match c.get("resume") {
                    Some(Json::Bool(b)) => *b,
                    Some(_) => return Err("resume is true or false".into()),
                    None => false,
                },
            }));
        } else {
            return Err("an op is {\"poke\": ...}, {\"pokem\": ...} or {\"call\": ...}".into());
        }
    }
    Ok(ops)
}

pub fn parse_request(text: &str) -> Result<Request, String> {
    let root = json::parse(text)?;
    let need = |key: &str| root.get(key).ok_or_else(|| format!("request has no {key}"));
    let return_sw = u32_of(need("return_sw")?)?;
    let breaks = match root.get("breaks") {
        Some(b) => b.arr()?.iter().map(u32_of).collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    let mut watch = Vec::new();
    if let Some(w) = root.get("watch") {
        for pair in w.arr()? {
            let pair = pair.arr()?;
            if pair.len() != 2 {
                return Err("a watch range is [first_pc, end_pc]".into());
            }
            watch.push((u32_of(&pair[0])?, u32_of(&pair[1])?));
        }
    }
    let mut code = Vec::new();
    for seg in need("code")?.arr()? {
        let address = u32_of(seg.get("address").ok_or("code segment has no address")?)?;
        let hex = seg.get("hex").ok_or("code segment has no hex")?.str()?;
        code.push((address, hex_bytes(hex)?));
    }
    let image = match root.get("image") {
        Some(i) => Some(hex_bytes(i.str()?)?),
        None => None,
    };
    let ops = match root.get("ops") {
        Some(o) => parse_ops(o)?,
        None => Vec::new(),
    };
    Ok(Request {
        return_sw,
        breaks,
        watch,
        code,
        image,
        ops,
    })
}

/// A core configured like the concrete Python core of `sharc_run.Runner` (no generated blocks, runtime decoding of the
/// loaded code, every register 0, the core MMRs at their reset values).
pub fn new_engine(
    image: Option<&LoadedMemory>,
    stream: &[u8],
    code: &[(u32, Vec<u8>)],
) -> Result<Engine, String> {
    let mut mem = Mem::new();
    if let Some(image) = image {
        load_image(&mut mem, image, stream);
    }
    for (address, bytes) in code {
        mem.load(*address, bytes);
    }
    mem.reset();
    let mut e = Engine::new(mem);
    e.use_blocks = false;
    // options of tools/sharc_dev/sharc_transpile_run.py `to_native` for a fresh `Runner`
    for (key, value) in [
        (4, 1),
        (10, 0),
        (12, 1),
        (13, 1),
        (14, 64),
        (15, 0),
        (16, 0),
        (17, 1),
        (18, 0),
        (19, 0),
        (20, 0),
        (9, 0),
        (21, 0),
        (22, 0),
    ] {
        if e.set_option(key, value) != 0 {
            return Err(format!("the core refused option {key}"));
        }
    }
    for code in 0..128 {
        e.set_reg(code, V::c(0));
    }
    for (name, value) in START_REGS {
        e.set_reg(reg_code(name)?, V::c(value as u32 as Int));
    }
    for a in RESET_MMRS {
        e.s.mmr_put(a, V::c(0));
        e.s.core_mmr_reset.push(a);
    }
    e.s.set_mmr_windows();
    Ok(e)
}

/// Load what `image` (parsed from `stream`) writes into the loader image: the runs of present bytes of every block.
/// `LoadedMemory` has no iterator, so the block headers give the address ranges and `get` the bytes (later blocks win there).
fn load_image(mem: &mut Mem, image: &LoadedMemory, stream: &[u8]) {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut offset = 0usize;
    while offset + 16 <= stream.len() {
        let header = &stream[offset..offset + 16];
        if !matches!(header[3], 0xad | 0xac | 0xab) || header.iter().fold(0, |x, b| x ^ b) != 0 {
            break;
        }
        let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
        let (code, target, count) = (word(0), word(1), word(2));
        let fill = code & (1 << 8) != 0;
        let length = if fill { 0 } else { count as usize };
        if offset + 16 + length > stream.len() {
            break;
        }
        ranges.push((target, count));
        offset += 16 + length;
    }
    ranges.sort_unstable();
    let mut run: Vec<u8> = Vec::new();
    let mut run_at = 0u32;
    let mut done_to = 0u64; // bytes below this address are loaded already
    for (target, count) in ranges {
        let end = target as u64 + count as u64;
        for a in (target as u64).max(done_to)..end {
            match image.get(a as u32) {
                Some(b) if !run.is_empty() && run_at as u64 + run.len() as u64 == a => run.push(b),
                Some(b) => {
                    if !run.is_empty() {
                        mem.load(run_at, &run);
                        run.clear();
                    }
                    run_at = a as u32;
                    run.push(b);
                }
                None => {
                    if !run.is_empty() {
                        mem.load(run_at, &run);
                        run.clear();
                    }
                }
            }
        }
        done_to = done_to.max(end);
    }
    if !run.is_empty() {
        mem.load(run_at, &run);
    }
}

fn poke_word(e: &mut Engine, word_address: u32, word: u32) -> Result<(), String> {
    e.s.begin();
    let done = bnd::_dm_write(
        &mut e.s,
        VI::I(word_address as Int),
        4,
        V::c(word as Int),
        true,
    );
    match done {
        Ok(true) => {
            e.s.commit_host();
            Ok(())
        }
        _ => {
            e.s.rollback();
            Err(format!("cannot poke DM word {word_address:#x}"))
        }
    }
}

fn peek_word(e: &Engine, word_address: u32) -> Option<u32> {
    match bnd::_dm_read(&e.s, VI::I(word_address as Int), 4, false, true) {
        Ok(Some(v)) if v.is_c() => Some(v.b),
        _ => None,
    }
}

/// Run one call. The loop is the interpreter's own (`decode_at`, then `exec_insn`, as `Engine::step` does for one
/// instruction); it stops at the return point and the breaks before an instruction is decoded.
pub fn run_call(
    e: &mut Engine,
    req: &Request,
    stock: Option<&LoadedMemory>,
    call: &Call,
) -> CallResult {
    for &(code, value) in &call.regs {
        e.set_reg(code, V::c(value as Int));
    }
    for &(slot, mask, bits) in &call.specials {
        e.s.special[slot] = Spec::M(MR::new(mask, bits));
        e.s.special_present[slot] = true;
    }
    if !call.resume {
        e.fresh_call(call.pc, Some(req.return_sw as Int));
    }
    let breaks = call.breaks.as_deref().unwrap_or(&req.breaks);
    let start = e.s.icount;
    let mut halt = String::from("max-steps");
    let mut stores = Vec::new();
    let mut profile: std::collections::BTreeMap<u32, u64> = Default::default();
    while e.s.icount - start < call.max_steps {
        let pc = e.s.pc_sw as u32;
        if pc == req.return_sw || breaks.contains(&pc) {
            halt = "breakpoint".into();
            break;
        }
        let insn = match bnd::decode_at(&mut e.s, (), None, pc as Int) {
            Ok(i) => i,
            Err(t) => {
                halt = format!("trap: {}", trap_name(t));
                break;
            }
        };
        // The undo log of the instruction names the bytes it stored (`St::probe_note`): the engine's own record, so
        // the loop pays for it only at the pcs asked for.
        let watched = req.watch.iter().any(|&(lo, hi)| (lo..hi).contains(&pc));
        e.s.probe = watched.then(Vec::new);
        if call.profile {
            *profile.entry(pc).or_default() += 1;
        }
        let done = exec_insn(&mut e.s, insn);
        let log = e.s.probe.take();
        if let Err(t) = done {
            halt = format!("trap: {}", trap_name(t));
            break;
        }
        if let Some(log) = log {
            note_stores(&mut stores, pc, log.into_iter().map(|(a, _, _)| a));
        }
    }
    CallResult {
        instructions: e.s.icount - start,
        halt,
        final_pc: e.s.pc_sw as u32,
        regs: call
            .read_regs
            .iter()
            .map(|(n, c)| {
                let v = e.s.r[*c];
                (n.clone(), v.is_c().then_some(v.b))
            })
            .collect(),
        reads: call
            .reads
            .iter()
            .map(|&(a, n)| (0..n).map(|k| peek_word(e, a + k)).collect())
            .collect(),
        stores,
        profile: profile.into_iter().collect(),
        specials: call
            .read_specials
            .iter()
            .map(|(n, slot)| {
                let v = match (e.s.special_present[*slot], e.s.special[*slot]) {
                    (true, Spec::M(m)) => Some((m.mask, m.bits)),
                    _ => None,
                };
                (n.clone(), v)
            })
            .collect(),
        stock: call
            .stock_reads
            .iter()
            .map(|&(a, n)| {
                let stock = stock?;
                (0..n)
                    .map(|k| stock.get(a.wrapping_add(k)))
                    .collect::<Option<Vec<u8>>>()
            })
            .collect(),
    }
}

/// The bytes one instruction stored, as runs (`pc`, first address, width) in address order. Addresses are the core's
/// canonical ones: a store to a byte nothing loaded or poked lands at the loader alias (0x2800_0000 + address).
fn note_stores(out: &mut Vec<(u32, u32, u32)>, pc: u32, bytes: impl Iterator<Item = u32>) {
    let mut bytes: Vec<u32> = bytes.collect();
    bytes.sort_unstable();
    bytes.dedup();
    let mut at = 0;
    while at < bytes.len() {
        let mut end = at + 1;
        while end < bytes.len() && bytes[end] == bytes[end - 1].wrapping_add(1) {
            end += 1;
        }
        out.push((pc, bytes[at], (end - at) as u32));
        at = end;
    }
}

/// A core that keeps its state between batches of ops. The request's code, return point, breaks and watch ranges are fixed.
pub struct Session {
    e: Engine,
    req: Request,
    stock: Option<LoadedMemory>,
}

impl Session {
    /// The core of a request and the ops that the request carries.
    pub fn new(mut req: Request) -> Result<(Session, Vec<Op>), String> {
        let ops = std::mem::take(&mut req.ops);
        let stock = req
            .image
            .as_deref()
            .map(LoadedMemory::from_stream)
            .transpose()?;
        let e = new_engine(
            stock.as_ref(),
            req.image.as_deref().unwrap_or(&[]),
            &req.code,
        )?;
        Ok((Session { e, req, stock }, ops))
    }

    /// Run the ops in order. A poke error ends the batch: the pokes before it stay.
    pub fn ops(&mut self, ops: &[Op]) -> Result<Vec<CallResult>, String> {
        let mut out = Vec::new();
        for op in ops {
            match op {
                Op::Poke(pairs) => {
                    for &(a, w) in pairs {
                        poke_word(&mut self.e, a, w)?;
                    }
                }
                Op::PokeMasked(triples) => {
                    for &(a, mask, value) in triples {
                        let old = peek_word(&self.e, a).unwrap_or(0);
                        poke_word(&mut self.e, a, (old & !mask) | (value & mask))?;
                    }
                }
                Op::Call(c) => out.push(run_call(&mut self.e, &self.req, self.stock.as_ref(), c)),
            }
        }
        Ok(out)
    }
}

pub fn run(req: Request) -> Result<Vec<CallResult>, String> {
    let (mut session, ops) = Session::new(req)?;
    session.ops(&ops)
}

fn opt(v: Option<u32>) -> Json {
    v.map_or(Json::Null, |w| Json::Int(w as i64))
}

fn results_json(results: Vec<CallResult>) -> Json {
    Json::Arr(
        results
            .into_iter()
            .map(|r| {
                let profile = r.profile;
                let stock = (!r.stock.is_empty()).then_some(r.stock);
                let specials = r.specials;
                let mut fields = vec![
                    ("instructions".into(), Json::Int(r.instructions as i64)),
                    ("halt".into(), Json::Str(r.halt)),
                    ("final_pc".into(), Json::Int(r.final_pc as i64)),
                    (
                        "regs".into(),
                        Json::Obj(r.regs.into_iter().map(|(n, v)| (n, opt(v))).collect()),
                    ),
                    (
                        "reads".into(),
                        Json::Arr(
                            r.reads
                                .into_iter()
                                .map(|row| Json::Arr(row.into_iter().map(opt).collect()))
                                .collect(),
                        ),
                    ),
                    (
                        "stores".into(),
                        Json::Arr(
                            r.stores
                                .into_iter()
                                .map(|(pc, a, w)| {
                                    Json::Arr(vec![
                                        Json::Int(pc as i64),
                                        Json::Int(a as i64),
                                        Json::Int(w as i64),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ];
                if !specials.is_empty() {
                    fields.push((
                        "specials".into(),
                        Json::Obj(
                            specials
                                .into_iter()
                                .map(|(n, v)| {
                                    let row = v.map_or(Json::Null, |(mask, bits)| {
                                        Json::Arr(vec![
                                            Json::Str(format!("{mask:x}")),
                                            Json::Str(format!("{bits:x}")),
                                        ])
                                    });
                                    (n, row)
                                })
                                .collect(),
                        ),
                    ));
                }
                if let Some(stock) = stock {
                    fields.push((
                        "stock".into(),
                        Json::Arr(
                            stock
                                .into_iter()
                                .map(|b| {
                                    b.map_or(Json::Null, |b| {
                                        Json::Str(b.iter().map(|x| format!("{x:02x}")).collect())
                                    })
                                })
                                .collect(),
                        ),
                    ));
                }
                if !profile.is_empty() {
                    fields.push((
                        "profile".into(),
                        Json::Arr(
                            profile
                                .into_iter()
                                .map(|(pc, n)| {
                                    Json::Arr(vec![Json::Int(pc as i64), Json::Int(n as i64)])
                                })
                                .collect(),
                        ),
                    ));
                }
                Json::Obj(fields)
            })
            .collect(),
    )
}

fn response_line(results: Json) -> String {
    let mut out = String::new();
    Json::Obj(vec![
        ("build".into(), Json::Str(sharc_native::build_info())),
        ("results".into(), results),
    ])
    .write(&mut out);
    out.push('\n');
    out
}

/// Parse a request, run it and write the response JSON.
pub fn run_request(text: &str) -> Result<String, String> {
    let results = run(parse_request(text)?)?;
    Ok(response_line(results_json(results)))
}

/// `--serve`: the first line is a request, every later line `{"ops": [...]}`; `reply` gets one line per input line (an error line
/// is `{"error": "..."}`). The core is made by the first line and kept.
pub fn serve(
    lines: impl Iterator<Item = String>,
    mut reply: impl FnMut(&str),
) -> Result<(), String> {
    let mut session: Option<Session> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let answer = (|| -> Result<String, String> {
            match session.as_mut() {
                None => {
                    let (s, ops) = Session::new(parse_request(&line)?)?;
                    let s = session.insert(s);
                    Ok(response_line(results_json(s.ops(&ops)?)))
                }
                Some(s) => {
                    let root = json::parse(&line)?;
                    let ops = parse_ops(root.get("ops").ok_or("line has no ops")?)?;
                    Ok(response_line(results_json(s.ops(&ops)?)))
                }
            }
        })();
        match answer {
            Ok(text) => reply(&text),
            Err(e) => {
                let mut out = String::new();
                Json::Obj(vec![("error".into(), Json::Str(e))]).write(&mut out);
                out.push('\n');
                reply(&out);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0xB9_0000;
    const MR_ALL: Int = (1 << 80) - 1;
    const ALIAS: i64 = 0x2800_0000 + 2 * BASE as i64;

    fn request(ops: &str) -> String {
        // R1 = 3; R2 = 4 (two 32-bit instructions: four short words), then the return point
        format!(
            r#"{{"return_sw": {ret}, "breaks": [], "code": [{{"address": {ALIAS}, "hex": "810f0300820f0400"}}], "ops": [{ops}]}}"#,
            ret = BASE + 4
        )
    }

    #[test]
    fn json_round_trips_and_rejects_garbage() {
        let v = json::parse(r#"{"a": [1, -2, null, true], "b": "x\n\"y\""}"#).unwrap();
        let mut out = String::new();
        v.write(&mut out);
        assert_eq!(out, r#"{"a":[1,-2,null,true],"b":"x\n\"y\""}"#);
        assert!(json::parse("{\"a\": 1.5}").is_err());
        assert!(json::parse("[1, 2").is_err());
        assert!(json::parse("[1] x").is_err());
    }

    #[test]
    fn a_request_names_what_is_missing() {
        assert!(parse_request("{}").unwrap_err().contains("return_sw"));
        let bad =
            r#"{"return_sw": 1, "code": [], "ops": [{"call": {"pc": 1, "regs": {"NOPE": 1}}}]}"#;
        assert!(parse_request(bad).unwrap_err().contains("NOPE"));
        let odd = r#"{"return_sw": 1, "code": [{"address": 0, "hex": "abc"}], "ops": []}"#;
        assert!(parse_request(odd).is_err());
    }

    fn call(text: &str) -> Vec<CallResult> {
        run(parse_request(text).unwrap()).unwrap()
    }

    #[test]
    fn a_call_runs_to_its_return_point_and_stops_before_it() {
        let ops = format!(
            r#"{{"call": {{"pc": {BASE}, "regs": {{"R5": 9}}, "read_regs": ["R1", "R2", "R5"]}}}}"#
        );
        let r = &call(&request(&ops))[0];
        assert_eq!(r.halt, "breakpoint");
        assert_eq!(r.instructions, 2);
        assert_eq!(r.final_pc, BASE + 4);
        assert_eq!(
            r.regs,
            vec![
                ("R1".to_string(), Some(3)),
                ("R2".to_string(), Some(4)),
                ("R5".to_string(), Some(9))
            ]
        );
    }

    #[test]
    fn registers_and_memory_stay_between_calls() {
        let first = format!(r#"{{"call": {{"pc": {BASE}, "regs": {{"R7": 11}}}}}}"#);
        // the second call starts after the first instruction: R1 keeps 3, R7 keeps 11, one instruction runs
        let second = format!(
            r#"{{"call": {{"pc": {pc}, "reads": [[134217728, 2], [134217730, 1]], "read_regs": ["R1", "R7"]}}}}"#,
            pc = BASE + 2
        );
        let ops =
            format!(r#"{first}, {{"poke": [[134217728, 4294967295], [134217729, 5]]}}, {second}"#);
        let rs = call(&request(&ops));
        assert_eq!(rs[1].instructions, 1);
        assert_eq!(
            rs[1].regs,
            vec![("R1".to_string(), Some(3)), ("R7".to_string(), Some(11))]
        );
        // poked words read back; a word nothing wrote is unknown, not 0
        assert_eq!(rs[1].reads[0], vec![Some(0xFFFF_FFFF), Some(5)]);
        assert_eq!(rs[1].reads[1], vec![None]);
    }

    #[test]
    fn a_call_stops_at_its_step_limit_and_at_a_break() {
        let limit = format!(r#"{{"call": {{"pc": {BASE}, "max_steps": 1}}}}"#);
        let r = &call(&request(&limit))[0];
        assert_eq!((r.halt.as_str(), r.instructions), ("max-steps", 1));
        let text = request(&format!(r#"{{"call": {{"pc": {BASE}}}}}"#))
            .replace(r#""breaks": []"#, &format!(r#""breaks": [{}]"#, BASE + 2));
        let r = &call(&text)[0];
        assert_eq!(
            (r.halt.as_str(), r.instructions, r.final_pc),
            ("breakpoint", 1, BASE + 2)
        );
    }

    #[test]
    fn a_profile_call_counts_instructions_per_pc_and_other_calls_carry_none() {
        let plain = format!(r#"{{"call": {{"pc": {BASE}}}}}"#);
        assert!(call(&request(&plain))[0].profile.is_empty());
        let ops = format!(r#"{{"call": {{"pc": {BASE}, "profile": true}}}}"#);
        let r = &call(&request(&ops))[0];
        assert_eq!(r.profile, vec![(BASE, 1), (BASE + 2, 1)]);
        let text = run_request(&request(&ops)).unwrap();
        assert!(
            text.contains(&format!(r#""profile":[[{BASE},1],[{},1]]"#, BASE + 2)),
            "{text}"
        );
        assert!(!run_request(&request(&plain)).unwrap().contains("profile"));
        let bad = request(r#"{"call": {"pc": 1, "profile": 1}}"#);
        assert!(parse_request(&bad).unwrap_err().contains("profile"));
    }

    #[test]
    fn code_that_was_not_loaded_traps_without_counting() {
        let text = request(&format!(r#"{{"call": {{"pc": {}}}}}"#, BASE + 0x100));
        let r = &call(&text)[0];
        assert!(r.halt.starts_with("trap:"), "{}", r.halt);
        assert_eq!(r.instructions, 0);
    }

    #[test]
    fn a_poke_outside_mapped_memory_is_an_error() {
        let text = request(r#"{"poke": [[4294967295, 1]]}"#);
        assert!(run(parse_request(&text).unwrap()).is_err());
    }

    #[test]
    fn the_bytes_of_one_instruction_become_runs() {
        let mut out = Vec::new();
        note_stores(
            &mut out,
            7,
            [
                0x2000_0003,
                0x2000_0001,
                0x2000_0002,
                0x2000_0010,
                0x2000_0001,
            ]
            .into_iter(),
        );
        assert_eq!(out, vec![(7, 0x2000_0001, 3), (7, 0x2000_0010, 1)]);
    }

    #[test]
    fn a_masked_poke_changes_only_its_bits_and_reads_an_unwritten_word_as_zero() {
        let ops = format!(
            r#"{{"poke": [[134217728, 4294967295]]}},
            {{"pokem": [[134217728, 65535, 4660], [134217729, 4278190080, 2864434397]]}},
            {{"call": {{"pc": {BASE}, "reads": [[134217728, 2]]}}}}"#
        );
        let r = &call(&request(&ops))[0];
        // the first word keeps its upper half; the second was never written, so its other bits are 0
        assert_eq!(r.reads[0], vec![Some(0xFFFF_1234), Some(0xAA00_0000)]);
    }

    #[test]
    fn a_serve_session_keeps_the_core_between_lines_and_reports_a_bad_line() {
        let first = request(&format!(
            r#"{{"call": {{"pc": {BASE}, "regs": {{"R7": 11}}}}}}"#
        ));
        let lines = vec![
            first,
            "not json".to_string(),
            format!(
                r#"{{"ops": [{{"call": {{"pc": {}, "read_regs": ["R1", "R7"]}}}}]}}"#,
                BASE + 2
            ),
        ];
        let mut replies = Vec::new();
        serve(lines.into_iter(), |r| replies.push(r.to_string())).unwrap();
        assert_eq!(replies.len(), 3);
        assert!(replies[1].starts_with(r#"{"error""#), "{}", replies[1]);
        // R1 = 3 and R7 = 11 come from the first line's call: the core stayed
        assert!(
            replies[2].contains(r#""regs":{"R1":3,"R7":11}"#),
            "{}",
            replies[2]
        );
    }

    /// A boot-stream block (core 0 signature, XOR of the header zero) as the loader writes it.
    fn stream_block(code: u32, target: u32, count: u32, argument: u32, payload: &[u8]) -> Vec<u8> {
        let mut header = Vec::new();
        for w in [code, target, count, argument] {
            header.extend(w.to_le_bytes());
        }
        header[3] = 0xad;
        header[2] = 0;
        header[2] = header.iter().fold(0, |x, b| x ^ b);
        header.extend_from_slice(payload);
        header
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The image of the tests: the two instructions of `request` as a block, a FILL of DM words, and a later block that
    /// overwrites the second instruction's first parcel.
    fn image_request(extra: &str, ops: &str) -> String {
        let mut stream = stream_block(
            0,
            ALIAS as u32,
            8,
            0,
            &[0x81, 0x0f, 0x03, 0x00, 0x82, 0x0f, 0x04, 0x00],
        );
        stream.extend(stream_block(1 << 8, 0x2000_0000, 8, 0xaabbccdd, &[]));
        format!(
            r#"{{"return_sw": {ret}, "breaks": [], "image": "{img}", "code": [] {extra}, "ops": [{ops}]}}"#,
            ret = BASE + 4,
            img = hex(&stream)
        )
    }

    #[test]
    fn an_image_is_loaded_as_the_stock_memory_and_code_goes_over_it() {
        let ops = format!(
            r#"{{"call": {{"pc": {BASE}, "read_regs": ["R1", "R2"], "reads": [[536870912, 1], [536870916, 1]],
               "stock_reads": [[{ALIAS}, 8], [536870912, 6], [536870920, 1], [{}, 1]]}}}}"#,
            ALIAS - 1
        );
        let rs = call(&image_request("", &ops));
        let r = &rs[0];
        // the image's code runs; its FILL words read back (DM word at byte 0x20000000)
        assert_eq!(r.halt, "breakpoint");
        assert_eq!(
            r.regs,
            vec![("R1".to_string(), Some(3)), ("R2".to_string(), Some(4))]
        );
        // (an address this large is a byte address: a count steps by one byte, so the words are read one by one)
        assert_eq!(
            (r.reads[0][0], r.reads[1][0]),
            (Some(0xaabbccdd), Some(0xaabbccdd))
        );
        // stock reads: the stream's bytes; FILL repeats its argument little endian; a byte it does not write is null
        assert_eq!(
            r.stock[0],
            Some(vec![0x81, 0x0f, 0x03, 0x00, 0x82, 0x0f, 0x04, 0x00])
        );
        assert_eq!(r.stock[1], Some(vec![0xdd, 0xcc, 0xbb, 0xaa, 0xdd, 0xcc]));
        assert_eq!(r.stock[2], None);
        assert_eq!(r.stock[3], None);
        // code over the image: R2 = 9 instead of 4; the stock read still shows the stream
        let over = format!(r#", "code": [{{"address": {ALIAS}, "hex": "810f0300820f0900"}}]"#);
        let text = image_request("", &ops).replace(r#", "code": []"#, &over[..over.len()]);
        let r = &call(&text)[0];
        assert_eq!(r.regs[1], ("R2".to_string(), Some(9)));
        assert_eq!(
            r.stock[0],
            Some(vec![0x81, 0x0f, 0x03, 0x00, 0x82, 0x0f, 0x04, 0x00])
        );
    }

    #[test]
    fn stock_reads_without_an_image_are_null_and_a_bad_stream_is_an_error() {
        let ops = format!(r#"{{"call": {{"pc": {BASE}, "stock_reads": [[{ALIAS}, 1]]}}}}"#);
        assert_eq!(call(&request(&ops))[0].stock, vec![None]);
        let bad = format!(r#"{{"return_sw": 1, "code": [], "image": "00112233", "ops": []}}"#);
        assert!(run(parse_request(&bad).unwrap()).is_err());
    }

    #[test]
    fn a_call_can_name_its_own_breaks_and_resume_from_where_the_last_one_stopped() {
        let first = format!(
            r#"{{"call": {{"pc": {BASE}, "breaks": [{}], "read_regs": ["R1", "R2"]}}}}"#,
            BASE + 2
        );
        // R1 = 3 ran, R2 = 4 did not; the resumed call keeps the frame, runs R2 = 4 and ends at the return point
        let second = r#"{"call": {"pc": 1, "resume": true, "breaks": [], "read_regs": ["R2"]}}"#;
        let rs = call(&request(&format!("{first}, {second}")));
        assert_eq!(
            (rs[0].halt.as_str(), rs[0].instructions, rs[0].final_pc),
            ("breakpoint", 1, BASE + 2)
        );
        assert_eq!(rs[0].regs[1], ("R2".to_string(), Some(0)));
        assert_eq!(
            (rs[1].halt.as_str(), rs[1].instructions, rs[1].final_pc),
            ("breakpoint", 1, BASE + 4)
        );
        assert_eq!(rs[1].regs, vec![("R2".to_string(), Some(4))]);
    }

    #[test]
    fn the_registers_of_the_rig_tests_read_back_as_they_were_set() {
        let names = [
            "S0", "S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10", "S11", "S12", "S13",
            "S14", "S15", "ASTATX", "ASTATY", "STKYX", "STKYY", "LCNTR", "PX1", "PX2", "MODE1",
            "B0", "L3", "M5",
        ];
        let set: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, n)| format!(r#""{n}": {}"#, 0x3f01_0000u32 + 17 * i as u32))
            .collect();
        let read: Vec<String> = names.iter().map(|n| format!(r#""{n}""#)).collect();
        let ops = format!(
            r#"{{"call": {{"pc": {BASE}, "regs": {{{}}}, "read_regs": [{}]}}}}"#,
            set.join(", "),
            read.join(", ")
        );
        let r = &call(&request(&ops))[0];
        assert_eq!(r.halt, "breakpoint");
        for (i, (name, v)) in r.regs.iter().enumerate() {
            assert_eq!(*v, Some(0x3f01_0000u32 + 17 * i as u32), "{name}");
        }
    }

    #[test]
    fn multiplier_registers_are_set_before_a_call_and_read_after_it() {
        let ops = format!(
            r#"{{"call": {{"pc": {BASE}, "specials": {{"MSF": ["ffffffffffffffffffff", "123456789abcdef01234"]}},
               "read_specials": ["MSF", "MSB"]}}}}"#
        );
        let r = &call(&request(&ops))[0];
        assert_eq!(
            r.specials[0],
            ("MSF".to_string(), Some((MR_ALL, 0x123456789abcdef01234)))
        );
        assert_eq!(r.specials[1].1, None);
        let bad = request(r#"{"call": {"pc": 1, "read_specials": ["R1"]}}"#);
        assert!(parse_request(&bad).unwrap_err().contains("multiplier"));
    }
}
