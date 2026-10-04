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
