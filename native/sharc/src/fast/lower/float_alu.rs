//! Float ALU computes: fadd, fsub, fneg, float, trunc.
//!
//! The interpreter evaluates these in binary64 and rounds once to binary32,
//! which for add, subtract and multiply equals the binary32 operation. The
//! fast path implements only finite results; a NaN or an infinity (which the
//! interpreter reports through AV or AI and the sticky bits) exits before the
//! instruction. A denormal result is exact and shows only in its bit pattern. Flags: AZ and AN from the result bit pattern, AC AV AS AI
//! cleared, AF set (`flags.py _float_alu_updates`).

use super::*;
use crate::fast::FlagKind;

impl Lower {
    /// RN = FX op FY (floats), the result checked to be in the fast domain.
    pub fn falu_binary(&mut self, b: Bin, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        let y = self.rd_f(ry)?;
        let r = self.bin(b, x, y);
        self.float_result(rn, r)
    }

    pub fn falu_neg(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        let r = self.un(Un::FNeg, x);
        self.float_result(rn, r)
    }

    /// Guard the f32 result R, write it to RN and note the Falu flags.
    pub fn float_result(&mut self, rn: u32, r: Val) -> LR<()> {
        result_finite(self, r);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = float RX (int32 to float32, always in range).
    pub fn falu_float(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_i(rx)?;
        let r = self.un(Un::IToF, x);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = trunc FX: toward zero, only for |x| < 2^31.
    pub fn falu_trunc(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        let xb = self.to_i(x);
        let m = self.ci(0x7fff_ffff);
        let a = self.bin(Bin::And, xb, m);
        let lim = self.ci(0x4f00_0000);
        let ok = self.bin(Bin::LtU, a, lim);
        self.guard(ok);
        let r = self.un(Un::FToI, x);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }
}
