//! Execution conditions and the kernel's flag state.
//!
//! A region with conditions (`IF cond ...`, conditional branches) or branches
//! tracks flags *lazily*. ASTATX is the entry value (`PSEUDO_FB0`, read only)
//! plus the effect of the last flag writer of each of three groups: the ALU
//! bits (AZ AV AN AC AS AI AF), the multiplier bits (MN MV MU MI) and the
//! shifter bits (SV SZ SS). A writer only records its kind and its one or two
//! source values in three pseudo registers per group (`pseudo_flag`); no flag
//! bit is computed. A condition reads the bits it needs: from the sources of
//! the last writer when that is known statically (earlier in the same block),
//! else from the pseudo registers with a select over the writer kinds the
//! region has. At an exit the glue applies each group's last writer with the
//! same `glue::apply_flag` the loop shape's replay uses, so the exit state is
//! the interpreter's.
//!
//! A conditional instruction is lowered as the unconditional one with all its
//! effects made conditional in `Lower::commit`: stores in a skipped block,
//! register and flag-state writes as `select(cond, new, old)`; the guards of
//! its domain checks are weakened to `ok | !cond` (an instruction that does
//! not execute cannot leave the fast domain).
//!
//! The condition itself follows `sequencer._predicate` for SISD: EQ NE LT LE
//! GE GT read AZ / AF AN AZ AV (ALUSAT baked in, see `Req::Mode1`), the
//! others a single ASTATX bit, optionally complemented. Bits the condition
//! reads must be known: bits no earlier writer of the region defines become
//! an entry requirement (`Req::FlagsKnown`).

use super::*;
use crate::fast::glue::{AC, AF, ALU_MASK, AN, AV, AZ, MN, MULT_MASK, SS, SV, SZ};

/// The forms whose condition gates the whole instruction (transfer, index
/// update and compute), as `forms_compute`/`forms_move` execute them for
/// SISD: the lowering of such a form may be wrapped in `commit`'s
/// conditional effects. A form that is not listed is refused when
/// conditional (add a form here together with a `fast_diff forms --cond`
/// run that covers it).
pub const COND_FORMS: &[&str] = &[
    "2a",
    "3a",
    "3b",
    "5a_move",
    "5b_move",
    "6a_mem",
    "6b_shiftimm",
];

const BTF: u32 = 1 << 18;
const MV: u32 = 1 << 7;
const ALUSAT: u32 = 1 << 13;
const SHIFT_MASK: u32 = SV | SZ | SS;

/// The ASTATX bits a condition code reads, and whether it complements the
/// single bit; None for a condition the fast tier does not lower.
pub fn cond_read(cond: u32) -> Option<(u32, bool)> {
    Some(match cond {
        0x1f => (0, false),
        0x00 => (AZ, false),
        0x10 => (AZ, true),
        0x01 | 0x02 | 0x11 | 0x12 => (AF | AN | AZ | AV, false),
        0x03 => (AC, false),
        0x13 => (AC, true),
        0x04 => (AV, false),
        0x14 => (AV, true),
        0x05 => (MV, false),
        0x15 => (MV, true),
        0x06 => (MN, false),
        0x16 => (MN, true),
        0x07 => (SV, false),
        0x17 => (SV, true),
        0x08 => (SZ, false),
        0x18 => (SZ, true),
        0x0d => (BTF, false),
        0x1d => (BTF, true),
        _ => return None,
    })
}

/// The flag group (0 ALU, 1 multiplier, 2 shifter) that defines ASTATX bit
/// B (a single-bit mask), if any writer does.
fn group_of_bit(b: u32) -> Option<u32> {
    if ALU_MASK & b != 0 {
        Some(0)
    } else if MULT_MASK & b != 0 {
        Some(1)
    } else if SHIFT_MASK & b != 0 {
        Some(2)
    } else {
        None
    }
}

fn group_mask(g: u32) -> u32 {
    [ALU_MASK, MULT_MASK, SHIFT_MASK][g as usize]
}

/// The last flag writer of a group as far as the current block knows.
#[derive(Clone, Copy)]
pub struct LastW {
    pub kind: FlagKind,
    pub s0: Val,
    pub s1: Val,
}

impl Lower {
    /// Conditions are in the region: the kernel tracks flags.
    pub fn flag_v(&self) -> bool {
        self.env.flag_v
    }

    pub fn new_label(&mut self) -> u32 {
        self.nlabels += 1;
        self.nlabels - 1
    }

    /// `!cond` as a 0/1 value (one per instruction).
    pub fn not_cond(&mut self) -> Val {
        if let Some(n) = self.ncond {
            return n;
        }
        let c = self.cond.expect("not_cond without a condition");
        let z = self.ci(0);
        let n = self.bin(Bin::Eq, c, z);
        self.ncond = Some(n);
        n
    }

    /// A guard's `ok`, true also when the instruction does not execute.
    pub fn weaken(&mut self, ok: Val) -> Val {
        if self.cond.is_none() {
            return ok;
        }
        let nc = self.not_cond();
        self.bin(Bin::Or, ok, nc)
    }

    /// The register's value before the instruction, any register the
    /// lowering tracks (a pseudo register has no entry requirement).
    pub fn rd_any(&mut self, c: usize) -> Val {
        if c < NREGS as usize {
            return self.rd(c as u32).expect("register in range");
        }
        self.regs[c].used = true;
        if let Some(v) = self.cur[c] {
            return v;
        }
        let var = self.reg_var(c);
        let ty = self.vars[var.0 as usize];
        let v = self.emit(ty, Op::GetVar(var));
        self.cur[c] = Some(v);
        v
    }

    fn bit(&mut self, v: Val, n: u32) -> Val {
        let s = if n == 0 {
            v
        } else {
            let k = self.ci(n);
            self.bin(Bin::ShrU, v, k)
        };
        let one = self.ci(1);
        self.bin(Bin::And, s, one)
    }

    /// Bit B (a single-bit mask) of the entry ASTATX, 0/1.
    fn entry_bit(&mut self, b: u32) -> Val {
        let fb0 = self.rd_any(PSEUDO_FB0 as usize);
        self.bit(fb0, b.trailing_zeros())
    }

    /// ASTATX bit B (a single-bit mask) now, as 0/1.
    fn read_bit(&mut self, b: u32) -> Val {
        let Some(g) = group_of_bit(b) else {
            return self.entry_bit(b);
        };
        if let Some(w) = self.lw[g as usize] {
            return self.bit_of_writer(w.kind, w.s0, w.s1, b);
        }
        // The last writer is only known at run time: the entry bit, or the
        // bit of whichever kind of writer ran last.
        let mut v = self.entry_bit(b);
        for id in 1..=4u32 {
            if self.plan.flag_kinds[g as usize] >> id & 1 == 0 {
                continue;
            }
            let Some(kind) = FlagKind::from_group_id(g, id) else {
                continue;
            };
            if kind == FlagKind::FmulForget {
                continue;
            }
            let k = self.rd_any(pseudo_flag(g, 0) as usize);
            let s0 = self.rd_any(pseudo_flag(g, 1) as usize);
            let s1 = self.rd_any(pseudo_flag(g, 2) as usize);
            let kv = self.bit_of_writer(kind, s0, s1, b);
            let idv = self.ci(id);
            let is = self.bin(Bin::Eq, k, idv);
            v = self.select(is, kv, v);
        }
        v
    }

    /// Bit B (single-bit mask) a writer of KIND with sources S0 S1 defines,
    /// as 0/1.
    fn bit_of_writer(&mut self, kind: FlagKind, s0: Val, s1: Val, b: u32) -> Val {
        let (_, bits) = self.flag_bits(kind, &[s0, s1], b);
        self.bit(bits, b.trailing_zeros())
    }

    /// The execution condition `cond` (a 5-bit code) as a 0/1 value, and the
    /// ASTATX bits it reads recorded.
    pub fn cond_value(&mut self, cond: u32) -> LR<Val> {
        if !self.flag_v() {
            return refuse("condition in a region without kernel flags");
        }
        let Some((read, negate)) = cond_read(cond) else {
            return refuse(format!("condition {cond:#x}"));
        };
        self.read_mask |= read;
        self.need_flags_known |= read & !self.fm_static;
        if matches!(cond, 0x01 | 0x02 | 0x11 | 0x12) {
            // X = (!AF & (AN ^ (AV & !ALUSAT))) | (AF & AN) | AZ for LE/GT,
            // Y = (!AF & (AN ^ (AV & !ALUSAT))) | (AF & AN & !AZ) for LT/GE.
            let af = self.read_bit(AF);
            let an = self.read_bit(AN);
            let az = self.read_bit(AZ);
            let term = if self.env.mode1 & ALUSAT != 0 {
                an
            } else {
                // ALUSAT clear: the AV term applies.
                let av = self.read_bit(AV);
                self.bin(Bin::Xor, an, av)
            };
            self.mode1_mask |= ALUSAT;
            let nz = {
                let one = self.ci(1);
                self.bin(Bin::Xor, az, one)
            };
            let (x_af, x_nf, y_af, y_nf) = (
                self.bin(Bin::Or, an, az),
                self.bin(Bin::Or, term, az),
                self.bin(Bin::And, an, nz),
                term,
            );
            let (a, b) = if matches!(cond, 0x02 | 0x12) {
                (x_af, x_nf)
            } else {
                (y_af, y_nf)
            };
            let v = self.select(af, a, b);
            return Ok(if matches!(cond, 0x02 | 0x01) {
                v
            } else {
                let one = self.ci(1);
                self.bin(Bin::Xor, v, one)
            });
        }
        let v = self.read_bit(read);
        Ok(if negate {
            let one = self.ci(1);
            self.bin(Bin::Xor, v, one)
        } else {
            v
        })
    }

    /// The ASTATX effect of a writer of KIND: the bits it defines (`mask`)
    /// and their values, computed only for the bits in WANT.
    pub(super) fn flag_bits(&mut self, kind: FlagKind, srcs: &[Val], want: u32) -> (u32, Val) {
        let mut parts: Vec<Val> = Vec::new();
        let mask;
        match kind {
            FlagKind::Falu => {
                mask = ALU_MASK;
                let r = self.to_i(srcs[0]);
                if want & AZ != 0 {
                    let m = self.ci(0x7fff_ffff);
                    let mag = self.bin(Bin::And, r, m);
                    let z = self.ci(0);
                    parts.push(self.bin(Bin::Eq, mag, z));
                }
                if want & AN != 0 {
                    parts.push(self.sign_to(r, 2));
                }
                if want & AF != 0 {
                    parts.push(self.ci(AF));
                }
            }
            FlagKind::Fmul => {
                mask = MULT_MASK;
                if want & MN != 0 {
                    let r = self.to_i(srcs[0]);
                    parts.push(self.sign_to(r, 6));
                }
            }
            FlagKind::FmulForget => {
                mask = MULT_MASK;
            }
            FlagKind::Iadd | FlagKind::Isub => {
                mask = ALU_MASK;
                let sub = kind == FlagKind::Isub;
                let a = self.to_i(srcs[0]);
                let b = self.to_i(srcs[1]);
                let res = self.bin(if sub { Bin::Sub } else { Bin::Add }, a, b);
                let (mut cout, mut into) = (None, None);
                if want & (AC | AV) != 0 {
                    // Carry out of bit 31.
                    cout = Some(if sub {
                        self.bin(Bin::GeU, a, b)
                    } else {
                        self.bin(Bin::LtU, res, a)
                    });
                }
                if want & AV != 0 {
                    // Carry into bit 31: from the low 31 bits (plus the carry
                    // in of a subtract, `a + !b + 1`).
                    let m = self.ci(0x7fff_ffff);
                    let al = self.bin(Bin::And, a, m);
                    let bl = if sub {
                        let nb = self.un(Un::Not, b);
                        self.bin(Bin::And, nb, m)
                    } else {
                        self.bin(Bin::And, b, m)
                    };
                    let mut low = self.bin(Bin::Add, al, bl);
                    if sub {
                        let one = self.ci(1);
                        low = self.bin(Bin::Add, low, one);
                    }
                    let k31 = self.ci(31);
                    into = Some(self.bin(Bin::ShrU, low, k31));
                }
                if want & AC != 0 {
                    let k = self.ci(3);
                    parts.push(self.bin(Bin::Shl, cout.unwrap(), k));
                }
                if want & AV != 0 {
                    let x = self.bin(Bin::Xor, into.unwrap(), cout.unwrap());
                    let k = self.ci(1);
                    parts.push(self.bin(Bin::Shl, x, k));
                }
                if want & AN != 0 {
                    parts.push(self.sign_to(res, 2));
                }
                if want & AZ != 0 {
                    let z = self.ci(0);
                    parts.push(self.bin(Bin::Eq, res, z));
                }
            }
            FlagKind::Logical => {
                mask = ALU_MASK;
                let r = self.to_i(srcs[0]);
                if want & AZ != 0 {
                    let z = self.ci(0);
                    parts.push(self.bin(Bin::Eq, r, z));
                }
                if want & AN != 0 {
                    parts.push(self.sign_to(r, 2));
                }
            }
            FlagKind::Shift { sv } | FlagKind::Fext { sv } => {
                mask = SHIFT_MASK;
                if want & SZ != 0 {
                    let r = self.to_i(srcs[0]);
                    let z = self.ci(0);
                    let is0 = self.bin(Bin::Eq, r, z);
                    let k = self.ci(12);
                    parts.push(self.bin(Bin::Shl, is0, k));
                }
                if sv && want & SV != 0 {
                    parts.push(self.ci(SV));
                }
            }
        }
        let mut bits = parts.pop().unwrap_or_else(|| self.ci(0));
        while let Some(p) = parts.pop() {
            bits = self.bin(Bin::Or, bits, p);
        }
        (mask, bits)
    }

    /// A flag writer ran: note its kind and sources (no flag bit is computed;
    /// see the module comment). Under an execution condition the notes are
    /// selected like any register write.
    pub(super) fn flag_note(&mut self, kind: FlagKind, srcs: &[Val]) -> LR<()> {
        let (g, id) = kind.group_id();
        self.flag_kinds_out[g as usize] |= 1 << id;
        if kind == FlagKind::FmulForget {
            self.forget_mask |= MULT_MASK;
            self.fm_static &= !MULT_MASK;
        } else if self.cond.is_none() {
            // Only a writer that certainly runs defines its bits.
            self.fm_static |= group_mask(g);
        }
        let s0 = match srcs.first() {
            Some(&v) => self.to_i(v),
            None => self.ci(0),
        };
        let s1 = match srcs.get(1) {
            Some(&v) => self.to_i(v),
            None => self.ci(0),
        };
        let kid = self.ci(id);
        self.wr_any(pseudo_flag(g, 0), kid);
        self.wr_any(pseudo_flag(g, 1), s0);
        self.wr_any(pseudo_flag(g, 2), s1);
        self.lw[g as usize] = if self.cond.is_none() {
            Some(LastW { kind, s0, s1 })
        } else {
            None
        };
        Ok(())
    }

    /// Bit 31 of R moved to bit position POS (0/1 << pos).
    fn sign_to(&mut self, r: Val, pos: u32) -> Val {
        let k = self.ci(31 - pos);
        let s = self.bin(Bin::ShrU, r, k);
        let m = self.ci(1 << pos);
        self.bin(Bin::And, s, m)
    }

    /// Buffer a write of a pseudo register.
    pub fn wr_any(&mut self, code: u32, v: Val) {
        self.pending.writes.push((code as u8, v));
    }
}
