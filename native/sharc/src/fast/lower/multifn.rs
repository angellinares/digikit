//! Multifunction computes: the multiplier and the ALU in one instruction, and
//! the dual add/subtract forms (`compute_multi.py`, `compute_alu.py`).
//!
//! * MUL/ALU add and subtract (mf=1, category 0x18, 0x19):
//!   `Fm = Fxm * Fym, Fa = Fxa +- Fya`. Operands come from fixed register
//!   quads (Fxm F0-3, Fym F4-7, Fxa F8-11, Fya F12-15). Both results are
//!   written (Fm first, so Fa wins when the destinations coincide).
//! * MUL dual add/subtract (mf=1, category 0x30-0x3f):
//!   `Fm = Fxm * Fym, Fa = Fxa + Fya, Fs = Fxa - Fya`, Fs in the low nibble
//!   of the category.
//! * ALU dual add/subtract (mf=0, cu=0, opcode 0x7s fixed / 0xfs float).
//!
//! Flags: the ALU half sets them as the single-function float op does
//! (`Falu`; the dual forms OR the add's and the subtract's AZ and AN); the
//! multiplier half forgets MN MV MU MI (`FmulForget`) and its sticky bits do
//! not change. No flag depends on the product, and a denormal product is
//! exact in the interpreter's binary64, so only a NaN or infinity product
//! exits. The ALU results must be finite, as for the single-function float
//! ops.
//!
//! Other MUL/ALU categories (convert, average, abs, min, max) are refused.

use super::*;
use crate::fast::FlagKind;

impl Lower {
    pub fn multifn(&mut self, field: u32) -> LR<()> {
        let category = (field >> 16) & 0x3f;
        let rm = (field >> 12) & 0xf;
        let ra = (field >> 8) & 0xf;
        let rxm = (field >> 6) & 3;
        let rym = 4 + ((field >> 4) & 3);
        let rxa = 8 + ((field >> 2) & 3);
        let rya = 12 + (field & 3);
        match category {
            0x18 | 0x19 => {
                let (xm, ym) = (self.rd_f(rxm)?, self.rd_f(rym)?);
                let (xa, ya) = (self.rd_f(rxa)?, self.rd_f(rya)?);
                let m = self.bin(Bin::FMul, xm, ym);
                let b = if category == 0x18 {
                    Bin::FAdd
                } else {
                    Bin::FSub
                };
                let a = self.bin(b, xa, ya);
                result_finite(self, m);
                result_finite(self, a);
                self.wr_f(rm, m)?;
                self.wr_f(ra, a)?;
                self.pend_flag(FlagKind::Falu, vec![a]);
                self.pend_flag_also(FlagKind::FmulForget, Vec::new());
                Ok(())
            }
            0x30..=0x3f => {
                let rs = category & 0xf;
                let (xm, ym) = (self.rd_f(rxm)?, self.rd_f(rym)?);
                let (xa, ya) = (self.rd_f(rxa)?, self.rd_f(rya)?);
                let m = self.bin(Bin::FMul, xm, ym);
                let sum = self.bin(Bin::FAdd, xa, ya);
                let dif = self.bin(Bin::FSub, xa, ya);
                result_finite(self, m);
                result_finite(self, sum);
                result_finite(self, dif);
                self.wr_f(rm, m)?;
                self.wr_f(ra, sum)?;
                self.wr_f(rs, dif)?;
                self.pend_flag(FlagKind::FaluOr, vec![sum, dif]);
                self.pend_flag_also(FlagKind::FmulForget, Vec::new());
                Ok(())
            }
            _ => refuse(format!("multifunction category {category:#x}")),
        }
    }

    /// Single-function dual add/subtract (`RN = RX + RY, RS = RX - RY`).
    pub fn dual_addsub(&mut self, field: u32) -> LR<()> {
        let opcode = (field >> 12) & 0xff;
        let rn = (field >> 8) & 0xf;
        let rx = (field >> 4) & 0xf;
        let ry = field & 0xf;
        let rs = opcode & 0xf;
        if opcode >> 4 == 0xf {
            let (x, y) = (self.rd_f(rx)?, self.rd_f(ry)?);
            let sum = self.bin(Bin::FAdd, x, y);
            let dif = self.bin(Bin::FSub, x, y);
            result_finite(self, sum);
            result_finite(self, dif);
            self.wr_f(rn, sum)?;
            self.wr_f(rs, dif)?;
            self.pend_flag(FlagKind::FaluOr, vec![sum, dif]);
        } else {
            let (x, y) = (self.rd_i(rx)?, self.rd_i(ry)?);
            let sum = self.bin(Bin::Add, x, y);
            let dif = self.bin(Bin::Sub, x, y);
            self.wr_i(rn, sum)?;
            self.wr_i(rs, dif)?;
            self.pend_flag(FlagKind::IaddSubOr, vec![x, y]);
        }
        Ok(())
    }
}
