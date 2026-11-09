//! The primitives generated forms call (`gen_forms.rs`, written by
//! `tools/sharc_dev/sharc_lowergen.py` from `tools/sharc_dev/sharc_core`).
//!
//! A generated form is the decision tree the core's handler makes over the
//! instruction's fields and MODE1, with the effects of each leaf in program
//! order. This file holds what those effects mean for the kernel: it reuses
//! the lowering's window, anchor, circular-buffer and guard logic
//! (`mem_addr`, `Lower::post_value`) and keeps the bookkeeping the hand-written
//! forms do (register moves for the anchor hint, linear index writes).
//! Nothing here is specific to a form.

use super::mem_addr::{Width, Xfer};
use super::shift::shiftimm_field;
use super::sysops::MODE1_CODE;
use super::*;
use crate::rt::{UREG_PX, UREG_PX1, UREG_PX2};

/// MODE1's register-bank selection bits (`state._BANK_MASK`).
const BANK_BITS: u32 = 0x4F8;

/// MODE1.IRPTEN (bit 12) and MODE1.NESTM (bit 11): interrupt delivery and nesting.
const IRQ_ENABLE_BITS: u32 = 0x1800;

/// Where a value came from, for anchors and moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The register (UREG code below `NREGS`) the value is derived from.
    pub src: u8,
    /// The value of that register read at the start of the instruction.
    pub iv: Val,
    pub kind: TagKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagKind {
    /// The register itself.
    Rd,
    /// The register plus something (an address, a MODIFY result).
    Add,
    /// The post-modify update of the register.
    Post,
    /// The wrapped MODIFY result of the register (`g_circ`).
    Mod,
}

impl Lower {
    fn tag_of(&self, v: Val) -> Option<Tag> {
        self.gtag.get(v as usize).copied().flatten()
    }

    fn set_tag(&mut self, v: Val, t: Tag) {
        if self.gtag.len() <= v as usize {
            self.gtag.resize(v as usize + 1, None);
        }
        self.gtag[v as usize] = Some(t);
    }

    /// MODE1 bits MASK as the lowering knows them here; the region is made
    /// for those bits.
    pub fn m1(&mut self, mask: u32) -> u32 {
        self.mode1_mask |= mask;
        self.mode1_cur & mask
    }

    pub fn g_bin(&mut self, b: Bin, x: Val, y: Val) -> Val {
        // Both operands constants of the lowering: the result is one too.
        if let (Some(p), Some(q)) = (self.konst_of(x), self.konst_of(y)) {
            let (p, q) = (p as u32, q as u32);
            let k = match b {
                Bin::Add => Some(p.wrapping_add(q)),
                Bin::Sub => Some(p.wrapping_sub(q)),
                Bin::Mul => Some(p.wrapping_mul(q)),
                Bin::And => Some(p & q),
                Bin::Or => Some(p | q),
                Bin::Xor => Some(p ^ q),
                Bin::Shl => Some(p.wrapping_shl(q)),
                Bin::ShrU => Some(p.wrapping_shr(q)),
                _ => None,
            };
            if let Some(k) = k {
                return self.ci(k);
            }
        }
        let r = self.bin(b, x, y);
        if b == Bin::Add
            && let Some(t) = self.tag_of(x).or_else(|| self.tag_of(y))
            && t.kind != TagKind::Post
        {
            self.set_tag(
                r,
                Tag {
                    kind: TagKind::Add,
                    ..t
                },
            );
        }
        r
    }

    pub fn g_rd(&mut self, code: u32) -> LR<Val> {
        let v = if (16..48).contains(&code) {
            self.rd_i(code)?
        } else {
            self.rd_bits(code)?
        };
        if code < NREGS {
            self.set_tag(
                v,
                Tag {
                    src: code as u8,
                    iv: v,
                    kind: TagKind::Rd,
                },
            );
        }
        Ok(v)
    }

    pub fn g_wr(&mut self, code: u32, v: Val) -> LR<()> {
        if code == UREG_PX as u32 {
            // shortcut: the 32-bit fast path cannot represent Rn's low 8 bits.
            let low = self.ci(0);
            self.wr(UREG_PX1 as u32, low)?;
            return self.wr(UREG_PX2 as u32, v);
        }
        if code == MODE1_CODE {
            return self.g_wr_mode1(v);
        }
        if code < NREGS {
            if let Some(t) = self.tag_of(v) {
                match t.kind {
                    // Writing a register back unchanged is no write.
                    TagKind::Rd if t.src as u32 == code => return Ok(()),
                    TagKind::Rd if t.src as u32 != code => {
                        self.moves.push((code as u8, t.src));
                    }
                    TagKind::Add | TagKind::Mod
                        if (16..32).contains(&code) && (16..32).contains(&(t.src as u32)) =>
                    {
                        // `Ia = MODIFY(Ib, ...)`: a plain sum (`Add`; a circular
                        // destination is then written outside its buffer, see
                        // `commit`) or a wrapped one (`Mod`, `g_circ`).
                        self.note_modify(t.src as u32, code);
                    }
                    _ => {}
                }
            }
            if (16..48).contains(&code) {
                return self.wr_i(code, v);
            }
        }
        self.wr(code, v)
    }

    /// A write of MODE1: the new value must be a constant of the lowering.
    /// The kernel then runs on with MODE1 changed (`mode1_cur`, static) and
    /// writes the register like any other (the region's exit state).
    fn g_wr_mode1(&mut self, v: Val) -> LR<()> {
        let k = match self.konst_of(v) {
            Some(k) => k as u32,
            None => self.entry_value_of(v, "MODE1 written with a run-time value")?,
        };
        if k & !self.mode1_cur & IRQ_ENABLE_BITS != 0 {
            // A write that turns interrupt delivery on (IRPTEN, NESTM) makes a
            // source that is latched and unmasked deliverable at the next
            // instruction boundary. The region's entry check saw none (it was
            // off), so the kernel must not run on past the write: the
            // interpreter enters the interrupt after it.
            return refuse("MODE1 write enables interrupts");
        }
        if self.env.bank_model && (k ^ self.mode1_cur) & BANK_BITS != 0 {
            // A write that changes the bank selection starts a delayed one.
            return refuse("MODE1 write changes the register bank selection");
        }
        self.bank_pair()?;
        if k == self.mode1_cur {
            return Ok(());
        }
        if self.cond.is_some() {
            return refuse("conditional MODE1 write");
        }
        self.pending.writes.push((MODE1_CODE as u8, v));
        self.mode1_cur = k;
        self.mode1_mask = u32::MAX;
        Ok(())
    }

    fn index_anchor(&self, addr: Val) -> Option<(u32, Val, bool)> {
        let t = self.tag_of(addr)?;
        if !(16..32).contains(&(t.src as u32)) || t.kind == TagKind::Post {
            return None;
        }
        Some((t.src as u32, t.iv, addr == t.iv))
    }

    /// A load of BYTES bytes (4: a normal word) at byte address ADDR.
    pub fn g_load(&mut self, addr: Val, bytes: u8, signed: bool) -> LR<Val> {
        let w = Width { bytes, signed };
        self.xout = None;
        match self.index_anchor(addr) {
            Some((ic, iv, post)) => self.xfer_at(ic, iv, addr, post, Xfer::LoadV, w)?,
            None => self.mem_xfer_w(addr, Xfer::LoadV, w)?,
        }
        self.xout
            .take()
            .ok_or(Refuse("load produced no value".into()))
    }

    pub fn g_store(&mut self, addr: Val, v: Val, bytes: u8) -> LR<()> {
        let w = Width {
            bytes,
            signed: false,
        };
        match self.index_anchor(addr) {
            Some((ic, iv, post)) => self.xfer_at(ic, iv, addr, post, Xfer::StoreV(v), w),
            None => self.mem_xfer_w(addr, Xfer::StoreV(v), w),
        }
    }

    /// The post-modify update of index register IC (a UREG code 16..31) by
    /// DELTA (already scaled), IV its value: the new register value.
    pub fn g_postmod(&mut self, ic: u32, iv: Val, delta: Val, scale: i64) -> LR<Val> {
        if !(16..32).contains(&ic) {
            return refuse("post-modify of a non-index register");
        }
        let modified = self.bin(Bin::Add, iv, delta);
        let v = self.post_value(ic, iv, delta, modified, scale)?;
        self.set_tag(
            v,
            Tag {
                src: ic as u8,
                iv,
                kind: TagKind::Post,
            },
        );
        Ok(v)
    }

    /// The instruction's execution condition (PEx only).
    pub fn g_cond(&mut self, cond: u32, y: bool) -> LR<()> {
        if y {
            return refuse("PEy condition");
        }
        let cv = self.cond_value(cond)?;
        self.cond = Some(cv);
        Ok(())
    }

    /// Apply the instruction's compute (`kind` "compute") or shift
    /// immediate ("shiftimm") on PEx, or on PEy (SIMD).
    pub fn g_apply(&mut self, d: &Dec, kind: &str, y: bool, short: bool) -> LR<()> {
        if y {
            self.pe_off = 80;
            let r = self.g_apply_pe(d, kind, short);
            self.pe_off = 0;
            return r;
        }
        self.g_apply_pe(d, kind, short)
    }

    fn g_apply_pe(&mut self, d: &Dec, kind: &str, short: bool) -> LR<()> {
        match kind {
            "compute" if short => {
                let f = d.field("compute").ok_or(Refuse("2c compute".into()))? as u32;
                self.compute_short(f)
            }
            "compute" => {
                let f = d.compute().ok_or(Refuse("compute field".into()))?;
                self.compute_full(f)
            }
            "shiftimm" => {
                let f = shiftimm_field(d)?;
                let dataex = d.field("dataex").ok_or(Refuse("dataex".into()))? as u32;
                self.shiftimm(f, dataex)
            }
            other => refuse(format!("apply {other}")),
        }
    }

    /// Lower a decoded instruction with the generated form of its type, if
    /// there is one. `Ok(false)`: the form is not generated.
    pub fn gen_lower(&mut self, d: &Dec) -> LR<bool> {
        gen_forms::dispatch(self, d)
    }
}

// ---------------------------------------------------------------------------
// Circular MODIFY and the baked L and B registers (lane F4)
// ---------------------------------------------------------------------------

impl Lower {
    /// Whether the region writes L or B of index register N (0..15): the
    /// entry values the circular lowering bakes are then not the values it
    /// would use (known from pass 1 on).
    pub fn lb_stale(&self, n: usize) -> bool {
        self.pass >= 1 && (self.plan.written[48 + n] || self.plan.written[64 + n])
    }

    /// The value of length register CODE (48..63) or base register CODE
    /// (64..79) the region is made for, with the requirement that the
    /// register has it at entry. A static symbol of the generated forms (the
    /// handlers test L and B of the source index register).
    pub fn sv_lb(&mut self, code: u32) -> LR<u32> {
        let n = (code & 15) as usize;
        if self.lb_stale(n) {
            return refuse("L/B of the index register written in the region");
        }
        let v = if code < 64 {
            self.env.dag_l[n]
        } else {
            self.env.dag_b[n]
        };
        let Some(v) = v else {
            return refuse("L/B of the index register unknown");
        };
        self.require_eq(code, v);
        Ok(v)
    }

    /// Type7a (nw) MODIFY scales the modifier by 4 in byte space, which is
    /// valid for a plain RAM address only: the check a normal-word access does
    /// through its window (the form has no access).
    pub fn g_plainmod(&mut self, iv: Val) -> LR<()> {
        if (self.env.guarded_dyn || super::mem_addr::guarded_xfers())
            && self.abs[iv as usize].is_none()
        {
            self.guard_plain(iv);
            Ok(())
        } else {
            self.require_plain(iv, 4)
        }
    }

    /// `iv + delta` corrected once by the buffer length, whatever IV is
    /// (`memory._circular_step`): `>= top` subtracts it, else `< base` adds
    /// it. No range is known for the result.
    pub fn wrap_unchecked(&mut self, iv: Val, delta: Val, base: u32, lenb: i64) -> Val {
        let top = (base as i64 + lenb) as u32;
        let t = self.bin(Bin::Add, iv, delta);
        let edge = self.ci(top);
        let over = self.bin(Bin::GeU, t, edge);
        let l = self.ci(lenb as u32);
        let back = self.bin(Bin::Sub, t, l);
        let low = self.ci(base);
        let under = self.bin(Bin::LtU, t, low);
        let fwd = self.bin(Bin::Add, t, l);
        let inner = self.select(under, fwd, t);
        self.select(over, back, inner)
    }

    /// The wrapped update of index register IC by DELTA (bytes, any run-time
    /// value) for an access of SCALE bytes per unit, for a register the
    /// region also writes with values not known to lie in its buffer:
    /// B and L are baked, the entry value is not constrained.
    pub fn wrap_general(&mut self, ic: u32, iv: Val, delta: Val, scale: i64) -> LR<Val> {
        let n = (ic - 16) as usize;
        let len = self.env.dag_l[n].unwrap_or(0);
        let (Some(base), true) = (self.env.dag_b[n], len != 0) else {
            return refuse("circular buffer unusable (B unknown)");
        };
        let lenb = len as i64 * scale;
        if len > (1 << 28) || base as i64 + lenb > u32::MAX as i64 {
            return refuse("circular buffer beyond 2^32");
        }
        self.require_eq(48 + n as u32, len);
        self.require_eq(64 + n as u32, base);
        Ok(self.wrap_unchecked(iv, delta, base, lenb))
    }

    /// `memory._circular_wrap_const` for `Ia = MODIFY(Ib, ..)` with L of Ib
    /// not zero: BASE and LENB (buffer bytes) are the baked B and L scaled by
    /// the access size, DELTA the modifier in bytes (UNSIGNED: a register's
    /// 32-bit value, which the core compares unsigned; else a signed
    /// immediate). The core stops when |DELTA| > LENB, so a modifier the
    /// lowering cannot see (or one that large) is refused. The value is
    /// tagged as a MODIFY result of Ib (`TagKind::Mod`).
    pub fn g_circ(&mut self, iv: Val, delta: Val, base: u32, lenb: i64, unsigned: bool) -> LR<Val> {
        let Some(src) = self
            .tag_of(iv)
            .filter(|t| t.kind == TagKind::Rd && (16..32).contains(&(t.src as u32)))
            .map(|t| t.src as u32)
        else {
            return refuse("circular MODIFY of a value that is not an index register");
        };
        let tag = |l: &mut Lower, v: Val| {
            l.set_tag(
                v,
                Tag {
                    src: src as u8,
                    iv,
                    kind: TagKind::Mod,
                },
            );
            v
        };
        if self.pass == 0 {
            // Pass 0 only finds which registers are written.
            let v = self.bin(Bin::Add, iv, delta);
            return Ok(tag(self, v));
        }
        if lenb <= 0 {
            return refuse("circular MODIFY with L <= 0");
        }
        if base as i64 + lenb > u32::MAX as i64 || lenb > (1 << 30) {
            return refuse("circular buffer beyond 2^32");
        }
        let Some(d) = self.konst_of(delta) else {
            return refuse("circular modifier not constant");
        };
        if unsigned && d < 0 {
            return refuse("circular MODIFY by a negative M (the core reads M unsigned)");
        }
        if d.abs() > lenb {
            return refuse("circular modifier larger than the buffer");
        }
        let v = if (self.plan.plain | self.plain_out) >> src & 1 != 0 {
            self.wrap_unchecked(iv, delta, base, lenb)
        } else {
            // The entry value lies in the buffer (a requirement), so the
            // sum crosses at most one edge and the result has the buffer's
            // range.
            let w = Req::Within {
                c: src as u8,
                lo: base,
                hi: (base as i64 + lenb - 1) as u32,
            };
            if !self.reqs.contains(&w) {
                self.reqs.push(w);
            }
            self.wrap_add(iv, delta, d, base, lenb)?
        };
        Ok(tag(self, v))
    }

    /// The value V, read from a register the region never writes, as the
    /// constant it has when the region is made for (a requirement that the
    /// register still has it at entry). For a write the lowering needs to know
    /// statically (MODE1) of a value the code took from a register.
    pub fn entry_value_of(&mut self, v: Val, why: &str) -> LR<u32> {
        let Some(t) = self
            .tag_of(v)
            .filter(|t| t.kind == TagKind::Rd && t.iv == v)
        else {
            return refuse(why);
        };
        let c = t.src as usize;
        if self.pass >= 1 && self.plan.written[c] {
            return refuse(why);
        }
        let Some(k) = self.env.regs[c] else {
            return refuse(why);
        };
        self.require_eq(c as u32, k);
        Ok(k)
    }
}
