//! Floating-point multiply (the only multiplier op the fast tier lowers).
//!
//! The full form `FN = FX * FY` sets MN from the result's sign and clears
//! MV, MU, MI for results in the fast domain (finite normal or zero); the
//! sticky MUS, MVS, MIS then stay as they are. A denormal operand (the device
//! reads it as a zero) and a product below the smallest normal (the device
//! returns a zero and sets MU and MUS) leave the kernel. The short form
//! `FN = FX * FY` (2a_short, 2c) sets them like the full form (2a).

use super::*;
use crate::fast::FlagKind;
use crate::fast::glue::{MN, MU};

impl Lower {
    pub fn fmul(&mut self, rn: u32, rx: u32, ry: u32, forget_flags: bool) -> LR<()> {
        let x = self.rd_f_op(rx)?;
        let y = self.rd_f_op(ry)?;
        let r = self.bin(Bin::FMul, x, y);
        result_no_underflow(self, x, y, r);
        self.wr_f(rn, r)?;
        if forget_flags {
            self.pend_flag_none(FlagKind::FmulForget);
        } else {
            self.pend_flag(FlagKind::Fmul, vec![r]);
        }
        Ok(())
    }

    /// `RN = RX * RY (SSI)`: the low word of the signed product. A product
    /// that does not fit 32 bits sets MV (and the sticky MOS), so it exits;
    /// otherwise MN is the sign and MV MU MI are cleared.
    pub fn mul_ssi(&mut self, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let a = self.rd_i(rx)?;
        let b = self.rd_i(ry)?;
        let lo = self.bin(Bin::Mul, a, b);
        let hi = self.bin(Bin::MulHs, a, b);
        let k31 = self.ci(31);
        let sign = self.bin(Bin::ShrS, lo, k31);
        let fits = self.bin(Bin::Eq, hi, sign);
        self.guard(fits);
        self.wr_i(rn, lo)?;
        self.pend_flag(FlagKind::Fmul, vec![lo]);
        Ok(())
    }

    /// Short `RN = RN * RX` (ssi): the same signed multiply as the full
    /// form, with its exact MN MV MU MI (the core runs `MULTIPLY_SSI_RN`).
    pub fn mul_short(&mut self, rn: u32, rx: u32) -> LR<()> {
        self.mul_ssi(rn, rn, rx)
    }

    // -- fractional signed multiplies on MRF (SSF) ---------------------------
    //
    // The kernel keeps MRF in three variables (`PSEUDO_MR`): bits 31:0, bits
    // 63:32 and bits 79:64 (always masked to 0xFFFF). MV is set when bits
    // 79:63 are not all equal; MV also sets the sticky MOS, which the kernel
    // does not model, so such a result leaves the kernel before the
    // instruction. Otherwise MV and MI are clear, MN is bit 79 and MU says
    // that bits 79:32 are all equal and bits 31:0 are not zero
    // (`compute_mult._mr_flags`).

    /// The checks every MRF multiply needs: one processing element, not SIMD,
    /// no execution condition.
    fn mr_context(&self) -> LR<()> {
        if self.pe_off != 0 {
            return refuse("MRF multiply on PEy");
        }
        if self.mode1_cur & (1 << 21) != 0 {
            return refuse("MRF multiply in SIMD mode");
        }
        if self.cond.is_some() {
            return refuse("conditional MRF multiply");
        }
        Ok(())
    }

    /// The three words of the 80-bit fractional signed product `2 * RX * RY`.
    fn mr_product(&mut self, rx: u32, ry: u32) -> LR<(Val, Val, Val)> {
        let a = self.rd_i(rx)?;
        let b = self.rd_i(ry)?;
        let lo = self.bin(Bin::Mul, a, b);
        let hi = self.bin(Bin::MulHs, a, b);
        let k1 = self.ci(1);
        let k31 = self.ci(31);
        let p0 = self.bin(Bin::Shl, lo, k1);
        let hi2 = self.bin(Bin::Shl, hi, k1);
        let lo_top = self.bin(Bin::ShrU, lo, k31);
        let p1 = self.bin(Bin::Or, hi2, lo_top);
        let sign = self.bin(Bin::ShrS, hi, k31);
        let m16 = self.ci(0xffff);
        let p2 = self.bin(Bin::And, sign, m16);
        Ok((p0, p1, p2))
    }

    /// 1 when bits 79:63 of the accumulator words W1 W2 are all equal (MV
    /// clear).
    fn mr_fits(&mut self, w1: Val, w2: Val) -> Val {
        let k31 = self.ci(31);
        let s = self.bin(Bin::ShrS, w1, k31);
        let m16 = self.ci(0xffff);
        let ext = self.bin(Bin::And, s, m16);
        self.bin(Bin::Eq, w2, ext)
    }

    /// The `FlagKind::Mfix` source: MN and MU at their ASTATX positions.
    fn mr_flag_src(&mut self, w0: Val, w1: Val, w2: Val) -> Val {
        let k = self.ci(15 - MN.trailing_zeros());
        let top = self.bin(Bin::ShrU, w2, k);
        let mn_mask = self.ci(MN);
        let mn = self.bin(Bin::And, top, mn_mask);
        // Bits 79:32 all zero, or all one.
        let z = self.ci(0);
        let any = self.bin(Bin::Or, w1, w2);
        let all0 = self.bin(Bin::Eq, any, z);
        let hi_ones = self.ci(0xffff_0000);
        let w2_ext = self.bin(Bin::Or, w2, hi_ones);
        let both = self.bin(Bin::And, w1, w2_ext);
        let ones = self.ci(u32::MAX);
        let all1 = self.bin(Bin::Eq, both, ones);
        let upper = self.bin(Bin::Or, all0, all1);
        let low_set = self.bin(Bin::Ne, w0, z);
        let mu = self.bin(Bin::And, upper, low_set);
        let ks = self.ci(MU.trailing_zeros());
        let mu_bit = self.bin(Bin::Shl, mu, ks);
        self.bin(Bin::Or, mn, mu_bit)
    }

    /// Buffer the write of the three accumulator words.
    fn mr_write(&mut self, w0: Val, w1: Val, w2: Val) {
        self.wr_any(PSEUDO_MR, w0);
        self.wr_any(PSEUDO_MR + 1, w1);
        self.wr_any(PSEUDO_MR + 2, w2);
    }

    /// `MRF = RX * RY (SSF)`.
    pub fn mr_mul(&mut self, rx: u32, ry: u32) -> LR<()> {
        self.mr_context()?;
        let (p0, p1, p2) = self.mr_product(rx, ry)?;
        let fits = self.mr_fits(p1, p2);
        self.guard(fits);
        let src = self.mr_flag_src(p0, p1, p2);
        self.mr_write(p0, p1, p2);
        self.pend_flag(FlagKind::Mfix, vec![src]);
        Ok(())
    }

    /// `MRF = MRF + RX * RY (SSF)`: an 80-bit add with the carries between
    /// the words.
    pub fn mr_mac(&mut self, rx: u32, ry: u32) -> LR<()> {
        self.mr_context()?;
        let (p0, p1, p2) = self.mr_product(rx, ry)?;
        let a0 = self.rd_any(PSEUDO_MR as usize);
        let a1 = self.rd_any(PSEUDO_MR as usize + 1);
        let a2 = self.rd_any(PSEUDO_MR as usize + 2);
        let s0 = self.bin(Bin::Add, a0, p0);
        let c0 = self.bin(Bin::LtU, s0, p0);
        let t = self.bin(Bin::Add, a1, p1);
        let ca = self.bin(Bin::LtU, t, p1);
        let s1 = self.bin(Bin::Add, t, c0);
        let cb = self.bin(Bin::LtU, s1, c0);
        // At most one of the two carries out of the middle word is set.
        let c1 = self.bin(Bin::Or, ca, cb);
        let u = self.bin(Bin::Add, a2, p2);
        let v = self.bin(Bin::Add, u, c1);
        let m16 = self.ci(0xffff);
        let s2 = self.bin(Bin::And, v, m16);
        let fits = self.mr_fits(s1, s2);
        self.guard(fits);
        let src = self.mr_flag_src(s0, s1, s2);
        self.mr_write(s0, s1, s2);
        self.pend_flag(FlagKind::Mfix, vec![src]);
        Ok(())
    }

    /// `RN = SAT MRF (SF)`: bits 63:32 of MRF clamped to the signed 64-bit
    /// range. MRF stays; MN and MU follow the accumulator before the clamp
    /// and MV is clear.
    pub fn mr_sat(&mut self, rn: u32) -> LR<()> {
        self.mr_context()?;
        let a0 = self.rd_any(PSEUDO_MR as usize);
        let a1 = self.rd_any(PSEUDO_MR as usize + 1);
        let a2 = self.rd_any(PSEUDO_MR as usize + 2);
        let fits = self.mr_fits(a1, a2);
        let k15 = self.ci(15);
        let neg = self.bin(Bin::ShrU, a2, k15);
        let min = self.ci(0x8000_0000);
        let max = self.ci(0x7fff_ffff);
        let clamp = self.select(neg, min, max);
        let res = self.select(fits, a1, clamp);
        self.wr_i(rn, res)?;
        let src = self.mr_flag_src(a0, a1, a2);
        self.pend_flag(FlagKind::Mfix, vec![src]);
        Ok(())
    }
}
