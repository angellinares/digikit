//! Register definitions and uses of one decoded instruction.
//!
//! The data is the generated table [`crate::defuse_table`] (made by `tools/sharc_dev/sharc_defuse_gen.py` from the SHARC core's
//! semantics): the register effect of every compute field, the universal register facts and an effect program per instruction form.
//! This module is the small interpreter of those programs over the decoded fields. Registers are numbered as the core numbers them:
//! the universal register codes 0..127 (R0-R15, I, M, L, B, S, then the system registers) and the special registers from
//! [`NUREG`] (the multiplier accumulators and the bit FIFO).
//!
//! Nothing here guesses: a compute opcode the core does not model, a form without a program and a bit operation that is not
//! documented are listed in [`Effects::uncertain`].

use crate::defuse_table::*;
use crate::forms::Fields;
use crate::model::{Mode1Effect, Mode1Op};
use sharc_rt::decode::Decoded;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DagFile {
    I,
    M,
    L,
    B,
}

impl DagFile {
    const fn base(self) -> u8 {
        match self {
            DagFile::I => 16,
            DagFile::M => 32,
            DagFile::L => 48,
            DagFile::B => 64,
        }
    }
}

/// The DAG bank of a register field: fixed, or DAG2 (+8) when the field `g` is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bank {
    Zero,
    Eight,
    ByG,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    /// A register by code.
    Fixed(u8),
    /// A data register named by a field.
    R(&'static str),
    /// A universal register named by a field (a write also changes its aliases).
    Ureg(&'static str),
    /// The source Ureg of a register move (`srcureghigh`, `srcureglow`).
    UregSrc,
    /// A DAG register of the file, the field and the bank.
    Dag(DagFile, &'static str, Bank),
    /// The destination of a DAG modify: the file register `is ^ idis`.
    DagDst(DagFile, &'static str, &'static str, Bank),
    /// The source of a DAG modify: the file register `is`.
    DagSrc(DagFile, &'static str, Bank),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeKind {
    Full,
    Short,
    ShiftImm,
}

#[derive(Clone, Copy, Debug)]
pub enum Eff {
    Rd(Loc),
    Wr(Loc),
    /// A data move: the field `dir` is 1 for a store (the register is read), 0 for a load (written). `lw` names the long word field.
    Move {
        dir: &'static str,
        reg: Loc,
        lw: Option<&'static str>,
    },
    Compute(ComputeKind, &'static str),
    If {
        field: &'static str,
        any: &'static [i64],
        then: &'static [Eff],
    },
    /// Type 18a: a bit operation on a system register.
    BitOp,
    /// The PC, loop or status stack is pushed or popped.
    Stack,
    /// PUSH STS (true) or POP STS (false).
    Sts(bool),
}

/// The register effect of one compute opcode. The role masks index [`ROLES`]; the system registers and flag bits are by code.
#[derive(Clone, Copy, Debug)]
pub struct CompEff {
    pub modelled: bool,
    pub reads: u16,
    pub writes: u16,
    pub sys_reads: &'static [u8],
    pub sys_writes: &'static [u8],
    pub astat: u32,
}

impl CompEff {
    pub const UNMODELLED: CompEff = CompEff {
        modelled: false,
        reads: 0,
        writes: 0,
        sys_reads: &[],
        sys_writes: &[],
        astat: 0,
    };
}

/// What one instruction defines and uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub reads: BTreeSet<u8>,
    pub writes: BTreeSet<u8>,
    /// ASTATX bits the instruction defines or forgets.
    pub astat: u32,
    pub mode1: Option<Mode1Effect>,
    /// The PC, loop or status stack is pushed or popped.
    pub stack: bool,
    /// A register-to-register move `(source, destination)` (Type 5).
    pub moved: Option<(u8, u8)>,
    pub uncertain: Vec<String>,
}

pub fn reg_name(code: u8) -> String {
    let c = code as usize;
    if c < NUREG {
        UREG_NAMES[c].to_string()
    } else if let Some(n) = SPECIAL_NAMES.get(c - NUREG) {
        n.to_string()
    } else {
        format!("reg{code}")
    }
}

fn program(form: &str) -> Option<&'static [Eff]> {
    FORM_PROGRAMS
        .binary_search_by(|(n, _)| (*n).cmp(form))
        .ok()
        .map(|i| FORM_PROGRAMS[i].1)
}

/// The compute entry the decode priority of the core picks for a compute word, with the register of each role.
fn compute_entry(kind: ComputeKind, word: u32) -> (&'static CompEff, [Option<u8>; 10]) {
    // Role order of ROLES: rn rx ry rs rm ra rxm rym rxa rya.
    let mut r = [None; 10];
    let nib = |s: u32| Some(((word >> s) & 15) as u8);
    match kind {
        ComputeKind::Short => {
            r[0] = nib(4);
            r[1] = nib(0);
            (&COMPUTE_SHORT[((word >> 8) & 15) as usize], r)
        }
        ComputeKind::ShiftImm => {
            r[0] = nib(4);
            r[1] = nib(0);
            (&COMPUTE_SHIFTIMM[((word >> 16) & 0x3f) as usize], r)
        }
        ComputeKind::Full => {
            if word >> 17 == 0b100000 {
                r[0] = nib(8);
                return (
                    &COMPUTE_MRMOVE[((((word >> 16) & 1) << 4) | ((word >> 12) & 15)) as usize],
                    r,
                );
            }
            let (cu, opcode) = ((word >> 20) & 3, (word >> 12) & 0xff);
            r[0] = nib(8);
            r[1] = nib(4);
            r[2] = nib(0);
            // The multiply-accumulate rows are matched before the multifunction bit (the core's priority).
            if cu == 1 && (opcode == 0xb4 || opcode == 0xb0) {
                return (&COMPUTE_SINGLE[(256 + opcode) as usize], r);
            }
            if (word >> 22) & 1 == 1 {
                let cat = (word >> 16) & 0x3f;
                r = [None; 10];
                r[3] = Some((cat & 15) as u8);
                r[4] = nib(12);
                r[5] = nib(8);
                r[6] = Some(((word >> 6) & 3) as u8);
                r[7] = Some(4 + ((word >> 4) & 3) as u8);
                r[8] = Some(8 + ((word >> 2) & 3) as u8);
                r[9] = Some(12 + (word & 3) as u8);
                return (&COMPUTE_MULTIFN[cat as usize], r);
            }
            if cu == 0 && (opcode >> 4 == 0x7 || opcode >> 4 == 0xf) {
                r[3] = Some((opcode & 15) as u8);
            }
            (&COMPUTE_SINGLE[(cu * 256 + opcode) as usize], r)
        }
    }
}

struct Eval<'a> {
    f: &'a Fields<'a>,
    out: Effects,
}

impl Eval<'_> {
    fn field(&self, stem: &str) -> i64 {
        self.f.get(stem).unwrap_or(0)
    }

    fn bank(&self, b: Bank) -> u8 {
        match b {
            Bank::Zero => 0,
            Bank::Eight => 8,
            Bank::ByG => {
                if self.field("g") == 1 {
                    8
                } else {
                    0
                }
            }
        }
    }

    fn code(&self, loc: Loc) -> u8 {
        match loc {
            Loc::Fixed(c) => c,
            Loc::R(f) | Loc::Ureg(f) => self.field(f) as u8,
            Loc::UregSrc => {
                (((self.field("srcureghigh") << 2) | (self.field("srcureglow") & 3)) & 0x7f) as u8
            }
            Loc::Dag(file, f, bank) => file.base() + self.bank(bank) + self.field(f) as u8,
            Loc::DagDst(file, is, idis, bank) => {
                file.base() + self.bank(bank) + ((self.field(is) ^ self.field(idis)) & 7) as u8
            }
            Loc::DagSrc(file, is, bank) => file.base() + self.bank(bank) + self.field(is) as u8,
        }
    }

    fn read(&mut self, code: u8, lw: bool) {
        self.out.reads.insert(code);
        if lw && (code as usize) < NUREG && LW_MATE[code as usize] != 255 {
            self.out.reads.insert(LW_MATE[code as usize]);
        }
    }

    fn write(&mut self, code: u8, lw: bool, ureg: bool) {
        self.out.writes.insert(code);
        if ureg && (code as usize) < NUREG {
            for e in UREG_EXTRA_WRITES[code as usize] {
                self.out.writes.insert(*e);
            }
            if lw && LW_MATE[code as usize] != 255 && LW_LOADS_MATE[code as usize] {
                self.out.writes.insert(LW_MATE[code as usize]);
            }
        }
    }

    fn run(&mut self, effs: &[Eff]) {
        for e in effs {
            match *e {
                Eff::Rd(loc) => {
                    let c = self.code(loc);
                    self.read(c, false);
                }
                Eff::Wr(loc) => {
                    let c = self.code(loc);
                    self.write(c, false, matches!(loc, Loc::Ureg(_)));
                }
                Eff::Move { dir, reg, lw } => {
                    let store = self.field(dir) == 1;
                    let lw = lw.is_some_and(|l| self.field(l) == 1);
                    let c = self.code(reg);
                    let ureg = matches!(reg, Loc::Ureg(_));
                    if store {
                        self.read(c, ureg && lw);
                    } else {
                        self.write(c, ureg && lw, ureg);
                    }
                }
                Eff::Compute(kind, stem) => self.compute(kind, stem),
                Eff::If { field, any, then } => {
                    if any.contains(&self.field(field)) {
                        self.run(then);
                    }
                }
                Eff::BitOp => self.bit_op(),
                Eff::Stack => self.out.stack = true,
                Eff::Sts(push) => {
                    self.out.mode1 = Some(Mode1Effect {
                        op: if push {
                            Mode1Op::PushSts
                        } else {
                            Mode1Op::PopSts
                        },
                        mask: 0,
                        value: None,
                    });
                }
            }
        }
    }

    fn compute(&mut self, kind: ComputeKind, stem: &str) {
        let word = self.field(stem) as u32 & 0x7f_ffff;
        if word == 0 && kind == ComputeKind::Full {
            // A zero compute field is the compute NOP.
            return;
        }
        let (ent, roles) = compute_entry(kind, word);
        if !ent.modelled {
            self.out.uncertain.push(format!(
                "compute {:#x} ({}) is not modelled by the SHARC core",
                word,
                match kind {
                    ComputeKind::Full => "full",
                    ComputeKind::Short => "short",
                    ComputeKind::ShiftImm => "shift immediate",
                }
            ));
            return;
        }
        for (i, reg) in roles.iter().enumerate() {
            let Some(reg) = reg else { continue };
            if ent.reads & (1 << i) != 0 {
                self.out.reads.insert(*reg);
            }
            if ent.writes & (1 << i) != 0 {
                self.out.writes.insert(*reg);
            }
        }
        self.out.reads.extend(ent.sys_reads.iter().copied());
        self.out.writes.extend(ent.sys_writes.iter().copied());
        self.out.astat |= ent.astat;
    }

    fn bit_op(&mut self) {
        let sreg = self.field("sreg") as u8;
        let bop = self.field("bop");
        let code = USTAT1 + sreg;
        match bop {
            4 | 5 => {
                self.out.reads.insert(code);
                self.out.writes.insert(ASTATX);
                self.out.astat |= 1 << 18;
            }
            0..=2 => {
                self.out.reads.insert(code);
                self.write(code, false, true);
                if code == MODE1 {
                    let op = [Mode1Op::Set, Mode1Op::Clear, Mode1Op::Toggle][bop as usize];
                    self.out.mode1 = Some(Mode1Effect {
                        op,
                        mask: self.field("data") as u32,
                        value: None,
                    });
                }
            }
            _ => self
                .out
                .uncertain
                .push(format!("bit operation {bop} is not documented")),
        }
    }
}

const USTAT1: u8 = 112;

/// The effects of a decoded instruction. A condition is not considered here: the caller marks the instruction conditional.
pub fn effects(d: &Decoded) -> Effects {
    let f = Fields::new(d);
    let form = f.form();
    let Some(prog) = program(form) else {
        return Effects {
            uncertain: vec![format!("form {form} has no register model")],
            ..Effects::default()
        };
    };
    let mut ev = Eval {
        f: &f,
        out: Effects::default(),
    };
    ev.run(prog);
    let moved = matches!(form, "5a_move" | "5b_move")
        .then(|| (ev.code(Loc::UregSrc), ev.field("dstureg") as u8));
    let mut out = ev.out;
    out.moved = moved;
    if out.mode1.is_none() && out.writes.contains(&MODE1) {
        // A write of the whole register: the value is known only for a Type 17 immediate.
        let value = matches!(form, "17a").then(|| f.get("data").unwrap_or(0) as u32);
        out.mode1 = Some(Mode1Effect {
            op: Mode1Op::Full,
            mask: u32::MAX,
            value,
        });
    }
    out
}

/// The SIMD companion registers (S for R, ASTATY for ASTATX, ...) of a register set: what the same instruction also touches
/// when MODE1.PEYEN is set.
pub fn simd_companions(regs: &BTreeSet<u8>) -> BTreeSet<u8> {
    regs.iter()
        .filter(|c| (**c as usize) < NUREG)
        .map(|c| SIMD_COMPANION[*c as usize])
        .filter(|c| *c != 255)
        .collect()
}
