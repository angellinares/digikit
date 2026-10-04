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

    /// The f32 operand X is not a NaN (NaN returns all ones and sets AI).
    fn guard_not_nan(&mut self, x: Val) {
        let ok = self.bin(Bin::FEq, x, x);
        self.guard(ok);
    }

    /// RN = abs FX; AS carries the input's sign, AN is always clear.
    pub fn falu_abs(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        self.guard_not_nan(x);
        let r = self.un(Un::FAbs, x);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Fabs, vec![r, x]);
        Ok(())
    }

    /// RN = pass FX (finite inputs).
    pub fn falu_pass(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        self.float_result(rn, x)
    }

    /// RN = clip FX by FY: FX if |FX| < |FY|, else FY's magnitude with FX's
    /// sign. A NaN operand and an infinite result exit.
    pub fn falu_clip(&mut self, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let a = self.rd_f(rx)?;
        let b = self.rd_f(ry)?;
        self.guard_not_nan(a);
        self.guard_not_nan(b);
        let aa = self.un(Un::FAbs, a);
        let ab = self.un(Un::FAbs, b);
        let lt = self.bin(Bin::FLt, aa, ab);
        let abits = self.to_i(a);
        let bbits = self.to_i(b);
        let m = self.ci(0x7fff_ffff);
        let sg = self.ci(0x8000_0000);
        let mag = self.bin(Bin::And, bbits, m);
        let sign = self.bin(Bin::And, abits, sg);
        let clipped = self.bin(Bin::Or, mag, sign);
        let rbits = self.select(lt, abits, clipped);
        let r = self.to_f(rbits);
        self.float_result(rn, r)
    }

    /// comp(FX, FY): flags only; a NaN operand exits.
    pub fn falu_compare(&mut self, rx: u32, ry: u32) -> LR<()> {
        let a = self.rd_f(rx)?;
        let b = self.rd_f(ry)?;
        self.guard_not_nan(a);
        self.guard_not_nan(b);
        let eq = self.bin(Bin::FEq, a, b);
        let lt = self.bin(Bin::FLt, a, b);
        let gt = self.bin(Bin::FLt, b, a);
        self.compare_flag(eq, lt, gt, true)
    }

    /// The biased exponent field of f32 bits XB.
    fn exp_field(&mut self, xb: Val) -> Val {
        let k23 = self.ci(23);
        let sh = self.bin(Bin::ShrU, xb, k23);
        let ff = self.ci(0xff);
        self.bin(Bin::And, sh, ff)
    }

    /// Guard that the shift amount SH (signed) is within +-1000, so that
    /// adding it to an exponent field cannot wrap.
    fn guard_shift_range(&mut self, sh: Val) {
        let k = self.ci(1000);
        let t = self.bin(Bin::Add, sh, k);
        let lim = self.ci(2000);
        let ok = self.bin(Bin::LtU, t, lim);
        self.guard(ok);
    }

    /// Guard that the f32 bits XB are zero or a normal number; returns the
    /// "is zero" truth value and the exponent field.
    fn guard_zero_or_normal(&mut self, xb: Val) -> (Val, Val) {
        let e = self.exp_field(xb);
        let m = self.ci(0x7fff_ffff);
        let mag = self.bin(Bin::And, xb, m);
        let z = self.ci(0);
        let iszero = self.bin(Bin::Eq, mag, z);
        let one = self.ci(1);
        let em1 = self.bin(Bin::Sub, e, one);
        let lim = self.ci(254);
        let normal = self.bin(Bin::LtU, em1, lim);
        self.guard_any(iszero, normal);
        (iszero, e)
    }

    /// F32 bits with the exponent field NE and the sign and mantissa of XB.
    fn with_exponent(&mut self, xb: Val, ne: Val) -> Val {
        let keep = self.ci(0x807f_ffff);
        let k23 = self.ci(23);
        let mant = self.bin(Bin::And, xb, keep);
        let exp = self.bin(Bin::Shl, ne, k23);
        self.bin(Bin::Or, mant, exp)
    }

    /// RN = logb FX for a finite normal FX: the unbiased exponent.
    pub fn falu_logb(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        let xb = self.to_i(x);
        let e = self.exp_field(xb);
        let one = self.ci(1);
        let em1 = self.bin(Bin::Sub, e, one);
        let lim = self.ci(254);
        let ok = self.bin(Bin::LtU, em1, lim);
        self.guard(ok);
        let bias = self.ci(127);
        let r = self.bin(Bin::Sub, e, bias);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = scalb FX by RY for FX zero or normal and no overflow; an
    /// underflow flushes to a signed zero.
    pub fn falu_scalb(&mut self, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let x = self.rd_f(rx)?;
        let xb = self.to_i(x);
        let sh = self.rd_i(ry)?;
        self.guard_shift_range(sh);
        let (iszero, e) = self.guard_zero_or_normal(xb);
        let ne = self.bin(Bin::Add, e, sh);
        let k255 = self.ci(255);
        let notover = self.bin(Bin::LtS, ne, k255);
        self.guard_any(iszero, notover);
        let one = self.ci(1);
        let nem1 = self.bin(Bin::Sub, ne, one);
        let lim = self.ci(254);
        let inrange = self.bin(Bin::LtU, nem1, lim);
        let built = self.with_exponent(xb, ne);
        let sg = self.ci(0x8000_0000);
        let under = self.bin(Bin::And, xb, sg);
        let scaled = self.select(inrange, built, under);
        let bits = self.select(iszero, xb, scaled);
        let r = self.to_f(bits);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = float RX by RY: exact for results with an exponent field of at
    /// least 2 (below that the rounding of the conversion decides the flush).
    pub fn falu_float_by(&mut self, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let x = self.rd_i(rx)?;
        let sh = self.rd_i(ry)?;
        self.guard_shift_range(sh);
        let f = self.un(Un::IToF, x);
        let fb = self.to_i(f);
        let e = self.exp_field(fb);
        let m = self.ci(0x7fff_ffff);
        let mag = self.bin(Bin::And, fb, m);
        let z = self.ci(0);
        let iszero = self.bin(Bin::Eq, mag, z);
        let ne = self.bin(Bin::Add, e, sh);
        let two = self.ci(2);
        let nem2 = self.bin(Bin::Sub, ne, two);
        let lim = self.ci(253);
        let inrange = self.bin(Bin::LtU, nem2, lim);
        self.guard_any(iszero, inrange);
        let built = self.with_exponent(fb, ne);
        let bits = self.select(iszero, fb, built);
        let r = self.to_f(bits);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = fix / trunc FX [by RY]. FX must be zero or normal and the scaled
    /// value below 2^31 in magnitude. `trunc` rounds toward zero; `fix`
    /// follows MODE1.TRUNCATE (a requirement on the entry state).
    pub fn falu_fix(&mut self, rn: u32, rx: u32, by: Option<u32>, trunc: bool) -> LR<()> {
        let truncate = if trunc {
            true
        } else {
            let Some(m1) = self.env.mode1 else {
                return refuse("MODE1 unknown");
            };
            let set = (m1 >> 15) & 1 != 0;
            let req = Req::Mode1Bit { bit: 15, set };
            if !self.reqs.contains(&req) {
                self.reqs.push(req);
            }
            set
        };
        let x = self.rd_f(rx)?;
        let xb = self.to_i(x);
        let sh = match by {
            Some(ry) => {
                let sh = self.rd_i(ry)?;
                self.guard_shift_range(sh);
                sh
            }
            None => self.ci(0),
        };
        let (iszero, e) = self.guard_zero_or_normal(xb);
        let ne = self.bin(Bin::Add, e, sh);
        let k158 = self.ci(158);
        let small = self.bin(Bin::LtS, ne, k158);
        self.guard_any(iszero, small);
        let one = self.ci(1);
        let under = self.bin(Bin::LtS, ne, one);
        let zero_res = self.bin(Bin::Or, iszero, under);
        let built = self.with_exponent(xb, ne);
        let sf = self.to_f(built);
        let rounded = if truncate {
            sf
        } else {
            // Round to nearest even below 2^23 by adding and subtracting
            // 2^23 with the sign of the value; larger values are integers.
            let c = self.cf(0x4b00_0000);
            let nc = self.cf(0xcb00_0000);
            let k31 = self.ci(31);
            let neg = self.bin(Bin::ShrU, built, k31);
            let mg = self.select(neg, nc, c);
            let a = self.bin(Bin::FAdd, sf, mg);
            let r = self.bin(Bin::FSub, a, mg);
            let abs = self.un(Un::FAbs, sf);
            let isn = self.bin(Bin::FLt, abs, c);
            self.select(isn, r, sf)
        };
        let i = self.un(Un::FToI, rounded);
        let z = self.ci(0);
        let r = self.select(zero_res, z, i);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Falu, vec![r]);
        Ok(())
    }

    /// RN = recips FX for an exponent field of 1..=252: the reciprocal seed
    /// (`approx_recips` mode: 1/x rounded to f32, low 15 mantissa bits
    /// cleared).
    pub fn falu_recips(&mut self, rn: u32, rx: u32) -> LR<()> {
        if !self.env.approx_recips {
            return refuse("recips without approx_recips");
        }
        let x = self.rd_f(rx)?;
        let xb = self.to_i(x);
        let e = self.exp_field(xb);
        let one = self.ci(1);
        let em1 = self.bin(Bin::Sub, e, one);
        let lim = self.ci(252);
        let ok = self.bin(Bin::LtU, em1, lim);
        self.guard(ok);
        let c1 = self.cf(0x3f80_0000);
        let q = self.bin(Bin::FDiv, c1, x);
        let qb = self.to_i(q);
        let keep = self.ci(0xffff_8000);
        let seed = self.bin(Bin::And, qb, keep);
        let r = self.to_f(seed);
        self.wr_f(rn, r)?;
        self.pend_flag(FlagKind::Recips, vec![xb]);
        Ok(())
    }
}
