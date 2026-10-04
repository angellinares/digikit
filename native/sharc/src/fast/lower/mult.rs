//! Floating-point multiply (the only multiplier op the fast tier lowers).
//!
//! The full form `FN = FX * FY` sets MN from the result's sign and clears
//! MV, MU, MI for results in the fast domain (finite normal or zero); the
//! sticky MUS, MVS, MIS then stay as they are. The short form
//! `FN = FX * FY` (2a_short, 2c) leaves MN MV MU MI unknown
//! (`MULT_FLAGS_FORGET`).

use super::*;
use crate::fast::FlagKind;

impl Lower {
    pub fn fmul(&mut self, rn: u32, rx: u32, ry: u32, forget_flags: bool) -> LR<()> {
        let x = self.rd_f(rx)?;
        let y = self.rd_f(ry)?;
        let r = self.bin(Bin::FMul, x, y);
        result_normal_or_zero(self, r);
        self.wr_f(rn, r)?;
        if forget_flags {
            self.pend_flag_none(FlagKind::FmulForget);
        } else {
            self.pend_flag(FlagKind::Fmul, vec![r]);
        }
        Ok(())
    }
}
