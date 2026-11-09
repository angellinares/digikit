//! Helper calls from kernels (`Op::Helper`): an instruction the kernel does not
//! lower runs through the interpreter's one-instruction step on the engine
//! state, and the kernel goes on.
//!
//! The kernel writes the registers the instruction reads into the state
//! (`StSt32`; their known masks are already full: the region requires every
//! register it touches to be known at entry), calls `helper_exec`, and reads
//! the registers the instruction writes back. Everything else the interpreter
//! step could touch (the sequencer: PC, loop and PC stacks, the instruction
//! counters) is the kernel's: the step runs at a PC no loop ends at, and
//! `icount` / `steps` are put back, since the kernel counts the instruction.

use crate::rt::{Insn, Int, St};
use std::cell::Cell;

/// Helper calls made (for reports).
pub static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A PC inside no loop, no pending transfer and no special stride range.
const STEP_PC: Int = 0x7f_fff0;

/// One helper call site, leaked by the lowering (a kernel holds its address).
pub struct HelperRec {
    pub pc: u32,
    insn: Cell<Option<Insn>>,
    /// A generated body (`gen_helpers`) and the decoded fields it takes, else
    /// the interpreter's step.
    gen_body: Option<(usize, [i64; super::gen_helpers::NF])>,
}

impl HelperRec {
    /// The address of a leaked record, for `Op::Helper`.
    pub fn leak(pc: u32) -> u64 {
        Self::leak_gen(pc, None)
    }

    pub fn leak_gen(pc: u32, gen_body: Option<(usize, [i64; super::gen_helpers::NF])>) -> u64 {
        Box::leak(Box::new(HelperRec {
            pc,
            insn: Cell::new(None),
            gen_body,
        })) as *const HelperRec as u64
    }
}

/// Registers an instruction reads and writes (`gen_helpers::touch`).
#[derive(Default, Debug)]
pub struct Touch {
    pub reads: Vec<u32>,
    pub writes: Vec<u32>,
}

impl Touch {
    pub fn rd(&mut self, c: u32) {
        if !self.reads.contains(&c) {
            self.reads.push(c);
        }
    }
    pub fn wr(&mut self, c: u32) {
        if !self.writes.contains(&c) {
            self.writes.push(c);
        }
    }
}

/// What the helper bodies read of the machine: the engine state at run time,
/// the build-time facts (`Env`) when the lowering asks which registers an
/// instruction touches.
pub trait RegRead {
    fn reg(&self, c: usize) -> Option<u32>;
}

impl RegRead for St {
    #[inline(always)]
    fn reg(&self, c: usize) -> Option<u32> {
        let v = self.r[c];
        v.is_c().then_some(v.b)
    }
}

/// The build-time register facts as a `RegRead` (MODE1, L, B and the rest).
pub struct EnvRegs<'a>(pub &'a super::lower::Env);

impl RegRead for EnvRegs<'_> {
    fn reg(&self, c: usize) -> Option<u32> {
        match c {
            114 => Some(self.0.mode1),
            48..=63 => self.0.dag_l[c - 48],
            64..=79 => self.0.dag_b[c - 64],
            0..=47 => self.0.regs[c],
            _ => None,
        }
    }
}

/// A register a helper body may read: R, I, M, L, B, S and MODE1, fully known.
#[inline(always)]
pub fn rr<S: RegRead>(s: &S, c: i128) -> Option<u32> {
    if (0..96).contains(&c) || c == 114 {
        s.reg(c as usize)
    } else {
        None
    }
}

#[inline(always)]
fn map_address(a: u32) -> u32 {
    match crate::addressing::normal_word_to_byte(a as i128) {
        Some(m) => m as u32,
        None => a,
    }
}

/// A normal-word or sub-word load of plain RAM (`bnd::_dm_read_b` on a
/// concrete address); None when the interpreter's full rules are needed.
#[inline(always)]
pub fn hload(s: &St, a: u32, width: u32, signed: bool) -> Option<u32> {
    if !s.cfg.fast_mem || !matches!(width, 1 | 2 | 4) {
        return None;
    }
    let raw = s.mem.fast_read(map_address(a), width)?;
    Some(if signed && width < 4 {
        let sh = 32 - 8 * width;
        (((raw << sh) as i32) >> sh) as u32
    } else {
        raw
    })
}

/// A store of plain RAM (`bnd::_dm_write_nolog_b`); false when the interpreter
/// must do it.
#[inline(always)]
pub fn hstore_ok(s: &mut St, a: u32, v: u32, width: u8) -> bool {
    s.cfg.fast_mem
        && matches!(width, 1 | 2 | 4)
        && s.mem.fast_write(map_address(a), width as u32, v).is_some()
}

/// The new value of index register IC after a post-modify by DELTA (already
/// scaled): `memory._post_modify_index` on known values.
#[inline(always)]
pub fn hpost<S: RegRead>(s: &S, ic: u32, iv: u32, delta: u32, scale: i64) -> Option<u32> {
    let n = (ic - 16) as usize;
    let plain = iv.wrapping_add(delta);
    let len = s.reg(48 + n)?;
    let mode1 = s.reg(114)?;
    if len == 0 || mode1 & (1 << 24) == 0 {
        return Some(plain);
    }
    let base = s.reg(64 + n)? as u64;
    let len = len as u64 * scale as u64;
    let c = plain as u64;
    Some(if c >= base + len {
        (c - len) as u32
    } else if c < base {
        (c + len) as u32
    } else {
        plain
    })
}

/// Runs the instruction of REC on the state ST; 0 when it completed, 1 when
/// it trapped (undone: the interpreter runs it again at the exit and traps).
pub extern "C" fn helper_exec(st: *mut u8, rec: u64) -> u32 {
    // SAFETY: the kernel passes the state it runs on and a record the
    // lowering leaked.
    let (s, r) = unsafe { (&mut *st.cast::<St>(), &*(rec as *const HelperRec)) };
    // One thread runs the DSP: a plain load and store is enough.
    CALLS.store(
        CALLS.load(std::sync::atomic::Ordering::Relaxed) + 1,
        std::sync::atomic::Ordering::Relaxed,
    );
    if let Some((id, f)) = &r.gen_body {
        return super::gen_helpers::run(*id, s, f);
    }
    let insn = match r.insn.get() {
        Some(i) => i,
        None => {
            // As `Engine::step` decodes: from the memory the region's code
            // words were checked against, else from the image's table.
            let i = if s.runtime_decode {
                match crate::rt::bnd::decode_at(s, (), None, r.pc as Int) {
                    Ok(i) => Some(i),
                    Err(t) => {
                        eprintln!(
                            "fast: helper decode at {:#x}: {}",
                            r.pc,
                            crate::trap_name(t)
                        );
                        None
                    }
                }
            } else {
                (s.insn_at)(r.pc as Int)
            };
            let Some(i) = i else {
                return 1;
            };
            r.insn.set(Some(i));
            i
        }
    };
    let (pc, icount, steps) = (s.pc_sw, s.icount, s.steps);
    s.pc_sw = STEP_PC;
    let res = crate::exec_insn(s, insn);
    s.pc_sw = pc;
    s.icount = icount;
    s.steps = steps;
    if res.is_err() {
        return 1;
    }
    0
}

/// MODE1 = 0, L = B = 0, nothing else known: what `cfg` assumes when it only
/// asks whether an instruction can be a helper.
pub struct NoRegs;

impl RegRead for NoRegs {
    fn reg(&self, c: usize) -> Option<u32> {
        matches!(c, 48..=79 | 114).then_some(0)
    }
}

/// A generated helper body for an instruction: the body, its decoded fields
/// and the registers it reads and writes.
pub struct Plan {
    pub id: usize,
    pub f: [i64; super::gen_helpers::NF],
    pub touch: Touch,
}

/// The plan for D, or None when no generated body covers it (a form without
/// one, a conditional or compute-carrying leaf, an unknown field).
pub fn plan<S: RegRead>(d: &super::decode_view::Dec, s: &S) -> Option<Plan> {
    let id = super::gen_helpers::FORMS
        .iter()
        .position(|n| *n == d.form)?;
    let mut f = [0i64; super::gen_helpers::NF];
    let labels = super::gen_helpers::labels(id);
    if labels.len() > f.len() {
        return None;
    }
    for (i, l) in labels.iter().enumerate() {
        f[i] = d.get(l)?;
    }
    let mut touch = Touch::default();
    if !super::gen_helpers::touch(id, s, &f, &mut touch) {
        return None;
    }
    // Registers beyond R, I, M, L, B, S have no copy in the kernel.
    if touch.reads.iter().chain(&touch.writes).any(|&c| c >= 96) {
        return None;
    }
    Some(Plan { id, f, touch })
}
