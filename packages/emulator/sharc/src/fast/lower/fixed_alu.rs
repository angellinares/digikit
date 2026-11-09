//! Fixed-point ALU computes: add, subtract, negate, pass, and/or/xor/not,
//! increment, decrement, min, max, comp and compu.
//!
//! Flags (`flags.py _arith_flag_bits`): AC AV AN AZ from the adder, AS AI AF
//! cleared; the logical ops set AN and AZ from the result and clear the rest.
//!
//! An add, subtract, increment, decrement or negate that overflows with
//! ALUSAT clear wraps and runs on: the kernel collects the overflow and the
//! glue latches AOS (`PSEUDO_AOS`). With ALUSAT set or in SIMD, and for abs,
//! an overflow leaves the kernel before the instruction: the interpreter
//! latches AOS and saturates (finding 103).

use super::cond::ALUSAT;
use super::*;
use crate::fast::FlagKind;

impl Lower {
    /// Handle a possible overflow. Bit 31 of `both` is set when the add or
    /// subtract overflows. With ALUSAT clear on PEx the result wraps and only
    /// AOS is left to do: the kernel collects `both` for the glue. Otherwise
    /// the kernel leaves before the instruction and the interpreter runs it.
    #[track_caller]
    fn alu_overflow(&mut self, both: Val) {
        if self.mode1_cur & (ALUSAT | (1 << 21)) != 0 || self.pe_off != 0 {
            let minus1 = self.ci(0xffff_ffff);
            let no_overflow = self.bin(Bin::GtS, both, minus1);
            self.guard(no_overflow);
            return;
        }
        self.mode1_mask |= ALUSAT;
        let seen = self.rd_any(PSEUDO_AOS as usize);
        let seen = self.bin(Bin::Or, seen, both);
        self.wr_any(PSEUDO_AOS, seen);
    }

    pub fn ialu_add(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Add, a, b);
        // Signed overflow: both operands differ in sign from the result.
        let xa = self.bin(Bin::Xor, a, r);
        let xb = self.bin(Bin::Xor, b, r);
        let both = self.bin(Bin::And, xa, xb);
        self.alu_overflow(both);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Iadd, vec![a, b]);
        Ok(())
    }

    pub fn ialu_sub(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Sub, a, b);
        // Signed overflow: the operands differ in sign and the result's sign
        // differs from the minuend's.
        let xab = self.bin(Bin::Xor, a, b);
        let xar = self.bin(Bin::Xor, a, r);
        let both = self.bin(Bin::And, xab, xar);
        self.alu_overflow(both);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Isub, vec![a, b]);
        Ok(())
    }

    pub fn ialu_logical(&mut self, rn: u32, r: Val) -> LR<()> {
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Logical, vec![r]);
        Ok(())
    }

    /// RN = RX + 1 (flags of the add).
    pub fn ialu_inc(&mut self, rn: u32, a: Val) -> LR<()> {
        let one = self.ci(1);
        self.ialu_add(rn, a, one)
    }

    /// RN = RX - 1 (flags of the subtract).
    pub fn ialu_dec(&mut self, rn: u32, a: Val) -> LR<()> {
        let one = self.ci(1);
        self.ialu_sub(rn, a, one)
    }

    /// RN = min / max (signed) of A and B; logical flags of the result.
    pub fn ialu_minmax(&mut self, rn: u32, a: Val, b: Val, max: bool) -> LR<()> {
        // min takes B when A > B, max takes B when A < B (equal: same value).
        let c = self.bin(if max { Bin::LtS } else { Bin::GtS }, a, b);
        let r = self.select(c, b, a);
        self.ialu_logical(rn, r)
    }

    /// RN = abs RX (`compute_alu.alu_abs`): the value is `0 - RX` for a
    /// negative RX (so INT_MIN stays INT_MIN) and RX otherwise; the flags
    /// are those of that adder with AS = the input's sign (`FlagKind::Iabs`).
    pub fn ialu_abs(&mut self, rn: u32, a: Val) -> LR<()> {
        let z = self.ci(0);
        let min = self.ci(0x8000_0000);
        let not_min = self.bin(Bin::Ne, a, min);
        self.guard(not_min);
        let neg = self.bin(Bin::LtS, a, z);
        let negated = self.bin(Bin::Sub, z, a);
        let r = self.select(neg, negated, a);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Iabs, vec![a]);
        Ok(())
    }

    /// comp / compu: flags only (no result register).
    pub fn ialu_compare(&mut self, a: Val, b: Val, signed: bool) -> LR<()> {
        let eq = self.bin(Bin::Eq, a, b);
        let lt = self.bin(if signed { Bin::LtS } else { Bin::LtU }, a, b);
        let gt = self.bin(if signed { Bin::GtS } else { Bin::GtU }, a, b);
        self.compare_flag(eq, lt, gt, false)
    }

    /// The flag source of a compare from its three outcome bits (0 or 1):
    /// value = eq | lt << 2 | gt << 31. The compare also shifts its result
    /// (bit 31) into the CACC history, a pseudo register that starts as the
    /// entry ASTATX bits 24-31 (`PSEUDO_CACC`).
    pub fn compare_flag(&mut self, eq: Val, lt: Val, gt: Val, float: bool) -> LR<()> {
        if self.pe_off != 0 {
            // PEy's compare shifts ASTATY's CACC history, which is not tracked.
            return refuse("compare on PEy");
        }
        let two = self.ci(2);
        let k31 = self.ci(31);
        let l = self.bin(Bin::Shl, lt, two);
        let g = self.bin(Bin::Shl, gt, k31);
        let e = self.bin(Bin::Or, eq, l);
        let value = self.bin(Bin::Or, e, g);
        let old = self.rd_any(PSEUDO_CACC as usize);
        let one = self.ci(1);
        let shifted = self.bin(Bin::ShrU, old, one);
        let keep = self.ci(0x7f00_0000);
        let kept = self.bin(Bin::And, shifted, keep);
        let top = self.ci(0x8000_0000);
        let bit = self.bin(Bin::And, value, top);
        let hist = self.bin(Bin::Or, kept, bit);
        self.wr_any(PSEUDO_CACC, hist);
        self.has_compare = true;
        if let Some(x) = &self.cfgx {
            self.compare_pcs.push(x.pc);
        }
        if self.fg_static != 0 {
            self.cmp_after_forget = true;
        }
        self.pend_flag(FlagKind::Compare { float }, vec![value]);
        Ok(())
    }
}
