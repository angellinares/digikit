//! Fixed-point ALU computes: add, subtract, negate, pass, and/or/xor/not,
//! increment, decrement, min, max, comp and compu.
//!
//! Flags (`flags.py _arith_flag_bits`): AC AV AN AZ from the adder, AS AI AF
//! cleared; the logical ops set AN and AZ from the result and clear the rest.

use super::*;
use crate::fast::FlagKind;

impl Lower {
    pub fn ialu_add(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Add, a, b);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Iadd, vec![a, b]);
        Ok(())
    }

    pub fn ialu_sub(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Sub, a, b);
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
        self.pend_flag(FlagKind::Compare { float }, vec![value]);
        Ok(())
    }
}
