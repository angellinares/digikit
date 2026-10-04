//! Register moves, immediate loads and the compute dispatch shared by the
//! compute-carrying forms.

use super::*;

impl Lower {
    pub fn form_2a(&mut self, d: &Dec) -> LR<()> {
        if let Some(c) = d.field("cond")
            && c != 0x1f
        {
            return refuse("conditional 2a");
        }
        let field = d.compute().ok_or(Refuse("2a compute".into()))?;
        if field == 0 {
            return refuse("empty compute");
        }
        self.compute_full(field)
    }

    pub fn form_2c(&mut self, d: &Dec) -> LR<()> {
        let field = d.field("compute").ok_or(Refuse("2c compute".into()))? as u32;
        self.compute_short(field)
    }

    /// 5a_move / 5b_move: Rd = Rs (and for 5a a compute).
    pub fn form_5_move(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional move");
        }
        let hi = d.field("srcureghigh").ok_or(Refuse("move src".into()))? as u32;
        let l1 = d
            .field("srcureglow[1:1]")
            .ok_or(Refuse("move src".into()))? as u32;
        let l0 = d
            .field("srcureglow[0:0]")
            .ok_or(Refuse("move src".into()))? as u32;
        let src = (hi << 2) | (l1 << 1) | l0;
        let dst = d.field("dstureg").ok_or(Refuse("move dst".into()))? as u32;
        let v = self.rd_bits(src)?;
        self.wr(dst, v)?;
        if d.form == "5a_move" {
            let field = d.compute().ok_or(Refuse("5a compute".into()))?;
            self.compute_full(field)?;
        }
        Ok(())
    }

    /// 17a: Ureg = 32-bit immediate.
    pub fn form_17a(&mut self, d: &Dec) -> LR<()> {
        let ureg = d.field("ureg").ok_or(Refuse("17a ureg".into()))? as u32;
        let value = d.wide("data").ok_or(Refuse("17a data".into()))?;
        let v = self.ci(value);
        self.wr(ureg, v)
    }

    /// A full compute field (`compute._compute`); 0 means none. Anything not
    /// listed is refused so the region is not built.
    pub fn compute_full(&mut self, field: u32) -> LR<()> {
        if field == 0 {
            return Ok(());
        }
        if field >> 17 == 0b100000 {
            return refuse("MR data move");
        }
        if (field >> 22) & 1 == 1 {
            return self.multifn(field);
        }
        let cu = (field >> 20) & 3;
        let opcode = (field >> 12) & 0xff;
        let rn = (field >> 8) & 0xf;
        let rx = (field >> 4) & 0xf;
        let ry = field & 0xf;
        if cu == 0 && matches!(opcode >> 4, 0x7 | 0xf) {
            return self.dual_addsub(field);
        }
        match (cu, opcode) {
            (0, 0x01) => {
                let (a, b) = (self.rd_i(rx)?, self.rd_i(ry)?);
                self.ialu_add(rn, a, b)
            }
            (0, 0x02) => {
                let (a, b) = (self.rd_i(rx)?, self.rd_i(ry)?);
                self.ialu_sub(rn, a, b)
            }
            (0, 0x22) => {
                let a = self.rd_i(rx)?;
                let z = self.ci(0);
                self.ialu_sub(rn, z, a)
            }
            (0, 0x21) => {
                let a = self.rd_i(rx)?;
                self.ialu_logical(rn, a)
            }
            (0, 0x40..=0x42) => {
                let (a, b) = (self.rd_i(rx)?, self.rd_i(ry)?);
                let op = [Bin::And, Bin::Or, Bin::Xor][(opcode - 0x40) as usize];
                let r = self.bin(op, a, b);
                self.ialu_logical(rn, r)
            }
            (0, 0x43) => {
                let a = self.rd_i(rx)?;
                let r = self.un(Un::Not, a);
                self.ialu_logical(rn, r)
            }
            (0, 0x81) => self.falu_binary(Bin::FAdd, rn, rx, ry),
            (0, 0x82) => self.falu_binary(Bin::FSub, rn, rx, ry),
            (0, 0xa2) => self.falu_neg(rn, rx),
            (0, 0xca) => self.falu_float(rn, rx),
            (0, 0xcd) => self.falu_trunc(rn, rx),
            (0, 0x29) => {
                let a = self.rd_i(rx)?;
                self.ialu_inc(rn, a)
            }
            (0, 0x2a) => {
                let a = self.rd_i(rx)?;
                self.ialu_dec(rn, a)
            }
            (0, 0x0a | 0x0b) => {
                let (a, b) = (self.rd_i(rx)?, self.rd_i(ry)?);
                self.ialu_compare(a, b, opcode == 0x0a)
            }
            (0, 0x61 | 0x62) => {
                let (a, b) = (self.rd_i(rx)?, self.rd_i(ry)?);
                self.ialu_minmax(rn, a, b, opcode == 0x62)
            }
            (0, 0xa1) => self.falu_pass(rn, rx),
            (0, 0xb0) => self.falu_abs(rn, rx),
            (0, 0xe3) => self.falu_clip(rn, rx, ry),
            (0, 0x8a) => self.falu_compare(rx, ry),
            (0, 0xc1) => self.falu_logb(rn, rx),
            (0, 0xbd) => self.falu_scalb(rn, rx, ry),
            (0, 0xc4) => self.falu_recips(rn, rx),
            (0, 0xc9) => self.falu_fix(rn, rx, None, false),
            (0, 0xd9) => self.falu_fix(rn, rx, Some(ry), false),
            (0, 0xdd) => self.falu_fix(rn, rx, Some(ry), true),
            (0, 0xda) => self.falu_float_by(rn, rx, ry),
            (1, 0x30) => self.fmul(rn, rx, ry, false),
            (1, 0x70) => self.mul_ssi(rn, rx, ry),
            (2, 0x00 | 0x04 | 0x20) => self.shift_reg(opcode, rn, rx, ry),
            (2, 0x88) => self.shift_leftz(rn, rx),
            (2, 0xcc) => self.shift_btst(rx, ry),
            _ => refuse(format!("compute cu={cu} opcode={opcode:#x}")),
        }
    }

    /// A short (12-bit) compute (`compute_multi.SHORT_OPS`).
    pub fn compute_short(&mut self, field: u32) -> LR<()> {
        let opcode = (field >> 8) & 0xf;
        let rn = (field >> 4) & 0xf;
        let rx = field & 0xf;
        match opcode {
            0x0 => {
                let (a, b) = (self.rd_i(rn)?, self.rd_i(rx)?);
                self.ialu_add(rn, a, b)
            }
            0x1 => {
                let (a, b) = (self.rd_i(rn)?, self.rd_i(rx)?);
                self.ialu_sub(rn, a, b)
            }
            0x2 => {
                let b = self.rd_i(rx)?;
                self.ialu_logical(rn, b)
            }
            0x3 => {
                let (a, b) = (self.rd_i(rn)?, self.rd_i(rx)?);
                self.ialu_compare(a, b, true)
            }
            0x4 => {
                let a = self.rd_i(rx)?;
                let r = self.un(Un::Not, a);
                self.ialu_logical(rn, r)
            }
            0x5 => {
                let a = self.rd_i(rx)?;
                self.ialu_inc(rn, a)
            }
            0x6 => {
                let a = self.rd_i(rx)?;
                self.ialu_dec(rn, a)
            }
            0x7 => self.mul_short(rn, rx),
            0xb => self.falu_compare(rn, rx),
            0x8 => self.falu_binary(Bin::FAdd, rn, rn, rx),
            0x9 => self.falu_binary(Bin::FSub, rn, rn, rx),
            0xa => self.falu_float(rn, rx),
            0xc..=0xe => {
                let (a, b) = (self.rd_i(rn)?, self.rd_i(rx)?);
                let op = [Bin::And, Bin::Or, Bin::Xor][(opcode - 0xc) as usize];
                let r = self.bin(op, a, b);
                self.ialu_logical(rn, r)
            }
            0xf => self.fmul(rn, rn, rx, true),
            _ => refuse(format!("short compute {opcode:#x}")),
        }
    }
}
