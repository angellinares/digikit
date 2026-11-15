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
//!           "code": [{"address": 671088640, "hex": "..."}],  // loader byte address, code in boot-stream order
//!           "ops": [ {"poke": [[word_address, word], ...]},  // DM writes, normal-word addresses
//!                    {"call": {"pc": 1, "regs": {"R4": 5}, "max_steps": 400000,
//!                              "reads": [[word_address, count]], "read_regs": ["I6"]}} ]}
//! response {"build": "...", "results": [{"instructions": n, "halt": "breakpoint", "final_pc": pc,
//!                                        "regs": {"I6": v|null}, "reads": [[v|null, ...]],
//!                                        "stores": [[pc, address, width], ...]}]}
//! ```
//!
//! A call is the same as `CallSession.call` of tools/patchkit/pk_sharc.py: the named registers are set, every other
//! register and all of DM stay as the last call left them, a call at `pc` returns to `return_sw`, and
//! the run stops before it executes an instruction at `return_sw` or in `breaks`. `instructions` counts the
//! instructions executed. `halt` is `breakpoint`, `max-steps` or `trap: <reason>`. `stores` lists, in execution order, the
//! runs of bytes that instructions at a `watch` pc stored (addresses as the core keeps them: a byte that nothing loaded
//! or poked is at 0x2800_0000 + its address). Nothing is recorded, and nothing costs, without `watch`.
//!
//! `pokem` changes some bits of a word (a halfword or a byte lane of a frame word): a word that nothing wrote reads as 0, as
//! `CallSession.peek(addr) or 0` does in the Python rigs.
//!
//! One request runs on a fresh core and ends. `sharc-rig --serve` keeps the core: its first line is a request (code, return
//! point, breaks, watch, optionally ops), every later line is `{"ops": [...]}`, and each line gets one response line
//! (`{"error": "..."}` for a line it cannot run; the core stays as it was). The caller then runs a case block by block and
//! stops at the first failing block, as the Python rigs do.

mod json;

use json::Json;
use sharc_native::mem::Mem;
use sharc_native::rt::bnd;
use sharc_native::rt::{Int, V, VI};
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
}

fn reg_code(name: &str) -> Result<usize, String> {
    (0..128)
        .find(|&c| vectors::ureg_name(c) == name)
        .ok_or_else(|| format!("unknown register {name}"))
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
    let ops = match root.get("ops") {
        Some(o) => parse_ops(o)?,
        None => Vec::new(),
    };
    Ok(Request {
        return_sw,
        breaks,
        watch,
        code,
        ops,
    })
}

/// A core configured like the concrete Python core of `sharc_run.Runner` (no generated blocks, runtime decoding of the
/// loaded code, every register 0, the core MMRs at their reset values).
pub fn new_engine(code: &[(u32, Vec<u8>)]) -> Result<Engine, String> {
    let mut mem = Mem::new();
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
pub fn run_call(e: &mut Engine, req: &Request, call: &Call) -> CallResult {
    for &(code, value) in &call.regs {
        e.set_reg(code, V::c(value as Int));
    }
    e.fresh_call(call.pc, Some(req.return_sw as Int));
    let start = e.s.icount;
    let mut halt = String::from("max-steps");
    let mut stores = Vec::new();
    while e.s.icount - start < call.max_steps {
        let pc = e.s.pc_sw as u32;
        if pc == req.return_sw || req.breaks.contains(&pc) {
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
}

impl Session {
    /// The core of a request and the ops that the request carries.
    pub fn new(mut req: Request) -> Result<(Session, Vec<Op>), String> {
        let ops = std::mem::take(&mut req.ops);
        let e = new_engine(&req.code)?;
        Ok((Session { e, req }, ops))
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
                Op::Call(c) => out.push(run_call(&mut self.e, &self.req, c)),
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
                Json::Obj(vec![
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
                ])
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
}
