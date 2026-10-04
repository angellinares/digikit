//! Shifter immediate computes: lshift, ashift (and the OR form of lshift),
//! fext (zero- and sign-extending).
//!
//! Flags (`flags.py _astatx_shift, _astatx_fext`): SS cleared, SZ set when
//! the shifted (or extracted) value is zero, SV set for a left shift
//! (shift) or when position + length exceeds 32 (fext).

use super::*;
use crate::fast::FlagKind;

impl Lower {
    /// The `shiftimm` compute of 6b_shiftimm and 6a_mem; DATAEX is the
    /// instruction's `dataex[3:0]` field.
    pub fn shiftimm(&mut self, field: u32, dataex: u32) -> LR<()> {
        let opcode = (field >> 16) & 0x3f;
        let data8 = (field >> 8) & 0xff;
        let rn = (field >> 4) & 0xf;
        let rx = field & 0xf;
        match opcode {
            0x00 | 0x01 | 0x08 => {
                let amount = data8 as u8 as i8 as i32;
                let arith = opcode & 1 == 1;
                let src = self.rd_i(rx)?;
                let shifted = if amount == 0 {
                    src
                } else if amount >= 32 {
                    self.ci(0)
                } else if amount <= -32 {
                    if arith {
                        let k = self.ci(31);
                        self.bin(Bin::ShrS, src, k)
                    } else {
                        self.ci(0)
                    }
                } else if amount > 0 {
                    let k = self.ci(amount as u32);
                    self.bin(Bin::Shl, src, k)
                } else {
                    let k = self.ci((-amount) as u32);
                    self.bin(if arith { Bin::ShrS } else { Bin::ShrU }, src, k)
                };
                let value = if opcode == 0x08 {
                    let old = self.rd_i(rn)?;
                    self.bin(Bin::Or, old, shifted)
                } else {
                    shifted
                };
                self.wr_i(rn, value)?;
                self.pend_flag(FlagKind::Shift { sv: amount > 0 }, vec![shifted]);
                Ok(())
            }
            0x10 | 0x12 => {
                let pos = data8 & 0x3f;
                let len = (dataex << 2) | (data8 >> 6);
                let src = self.rd_i(rx)?;
                let value = if len == 0 || pos >= 32 {
                    self.ci(0)
                } else {
                    let fl = len.min(32);
                    let sh = if pos == 0 {
                        src
                    } else {
                        let k = self.ci(pos);
                        self.bin(Bin::ShrU, src, k)
                    };
                    let field = if fl == 32 {
                        sh
                    } else {
                        let m = self.ci((1u32 << fl) - 1);
                        self.bin(Bin::And, sh, m)
                    };
                    if opcode == 0x12 && fl < 32 {
                        let k = self.ci(32 - fl);
                        let up = self.bin(Bin::Shl, field, k);
                        self.bin(Bin::ShrS, up, k)
                    } else {
                        field
                    }
                };
                self.wr_i(rn, value)?;
                self.pend_flag(FlagKind::Fext { sv: pos + len > 32 }, vec![value]);
                Ok(())
            }
            other => refuse(format!("shiftimm opcode {other:#x}")),
        }
    }

    /// `RN = [RN OR] lshift / ashift RX by RY` (the shifter computes): the
    /// signed low byte of RY is the amount, a left shift when positive.
    /// Opcode 0x00 lshift, 0x04 ashift, 0x20 OR-lshift.
    pub fn shift_reg(&mut self, opcode: u32, rn: u32, rx: u32, ry: u32) -> LR<()> {
        let arith = opcode == 0x04;
        let src = self.rd_i(rx)?;
        let amt_raw = self.rd_i(ry)?;
        let k24 = self.ci(24);
        let hi = self.bin(Bin::Shl, amt_raw, k24);
        let amt = self.bin(Bin::ShrS, hi, k24);
        let zero = self.ci(0);
        let isneg = self.bin(Bin::LtS, amt, zero);
        let negamt = self.bin(Bin::Sub, zero, amt);
        let mag = self.select(isneg, negamt, amt);
        let k31 = self.ci(31);
        let big = self.bin(Bin::GtU, mag, k31);
        let left = self.bin(Bin::Shl, src, mag);
        let right = self.bin(if arith { Bin::ShrS } else { Bin::ShrU }, src, mag);
        let v = self.select(isneg, right, left);
        // A magnitude of 32 or more: zero, except an arithmetic right shift
        // fills with the sign.
        let far = if arith {
            let fill = self.bin(Bin::ShrS, src, k31);
            self.select(isneg, fill, zero)
        } else {
            zero
        };
        let shifted = self.select(big, far, v);
        let value = if opcode == 0x20 {
            let old = self.rd_i(rn)?;
            self.bin(Bin::Or, old, shifted)
        } else {
            shifted
        };
        self.wr_i(rn, value)?;
        let pos = self.bin(Bin::GtS, amt, zero);
        self.pend_flag(FlagKind::ShiftDyn, vec![shifted, pos]);
        Ok(())
    }

    /// `RN = leftz RX`: the number of leading zero bits.
    pub fn shift_leftz(&mut self, rn: u32, rx: u32) -> LR<()> {
        let x = self.rd_i(rx)?;
        let r = self.un(Un::Clz, x);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Leftz, vec![x]);
        Ok(())
    }

    /// `btst RX by RY`: flags only. SV when the position is above 31, SZ when
    /// it is or the tested bit is clear.
    pub fn shift_btst(&mut self, rx: u32, ry: u32) -> LR<()> {
        let x = self.rd_i(rx)?;
        let pos = self.rd_i(ry)?;
        let k31 = self.ci(31);
        let oob = self.bin(Bin::GtU, pos, k31);
        let bit = self.bin(Bin::ShrU, x, pos);
        let one = self.ci(1);
        let b1 = self.bin(Bin::And, bit, one);
        let clear = self.bin(Bin::Xor, b1, one);
        let sz = self.bin(Bin::Or, oob, clear);
        let svb = self.bin(Bin::Shl, oob, one);
        let packed = self.bin(Bin::Or, sz, svb);
        self.pend_flag(FlagKind::Btst, vec![packed]);
        Ok(())
    }

    pub fn form_6b(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 6b");
        }
        let field = shiftimm_field(d)?;
        let dataex = d.field("dataex").ok_or(Refuse("dataex".into()))? as u32;
        self.shiftimm(field, dataex)
    }
}

pub fn shiftimm_field(d: &Dec) -> LR<u32> {
    let hi = d.get("shiftimm[22:16]").ok_or(Refuse("shiftimm".into()))?;
    let lo = d.get("shiftimm[15:0]").ok_or(Refuse("shiftimm".into()))?;
    Ok(((hi as u32) << 16) | lo as u32)
}
