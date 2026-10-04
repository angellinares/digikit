//! Data-memory transfers and index arithmetic: 3a, 3b, 3c, 6a_mem (a
//! normal-word DM/PM access with post- or pre-modify), 7a (modify).
//!
//! Addresses in these forms are byte addresses in the range where a
//! normal-word access is not mapped (`addressing::normal_word_to_byte` is
//! None) and the modifier is scaled by 4 (`assume_nw32`). That is a
//! requirement on the registers' values at entry (`Req::NwPlain`), checked
//! once per call over the whole range the region will touch.

use super::shift::shiftimm_field;
use super::*;

/// What a transfer does with the value.
#[derive(Clone, Copy)]
pub enum Xfer {
    Load(u32),
    Store(u32),
}

impl Lower {
    /// A normal-word transfer through index register IC with modifier MC.
    pub fn dm_transfer(&mut self, ic: u32, mc: u32, post: bool, x: Xfer) -> LR<()> {
        if !self.env.assume_nw32 {
            return refuse("assume_nw32 off");
        }
        let iv = self.rd_i(ic)?;
        let mv = self.rd_i(mc)?;
        let four = self.ci(4);
        let scaled = self.bin(Bin::Mul, mv, four);
        let modified = self.bin(Bin::Add, iv, scaled);
        let addr = if post { iv } else { modified };
        self.require_plain(iv, 4)?;
        if !post {
            self.require_plain(addr, 4)?;
        }
        self.mem_xfer(addr, x)?;
        if post {
            self.require_linear(ic)?;
            self.wr_i(ic, modified)?;
        }
        Ok(())
    }

    /// The load or store at byte address ADDR (already checked to be a
    /// plain normal-word address).
    pub fn mem_xfer(&mut self, addr: Val, x: Xfer) -> LR<()> {
        match x {
            Xfer::Load(code) => {
                let (p, o) = self.window_access(addr, 4, false)?;
                let v = self.emit(Ty::I32, Op::Load32 { base: p, off: o });
                self.wr(code, v)?;
            }
            Xfer::Store(code) => {
                let val = self.rd_bits(code)?;
                let (p, o) = self.window_access(addr, 4, true)?;
                self.pend_store(p, o, val);
            }
        }
        Ok(())
    }

    /// The kernel updates index register IC (UREG code 16..31): its length
    /// register L must be zero (no circular buffer, so the update is
    /// the plain sum whatever MODE1.CBUFEN says). A requirement on L at entry.
    pub fn require_linear(&mut self, ic: u32) -> LR<()> {
        if !(16..32).contains(&ic) {
            return refuse("require_linear: not an index register");
        }
        let r = Req::Eq((ic + 32) as u8, 0);
        if !self.reqs.contains(&r) {
            self.reqs.push(r);
        }
        Ok(())
    }

    pub fn form_3a(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 3a");
        }
        if d.field("l") != Some(0) {
            return refuse("3a long word");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("3a i".into()))? as u32 + bank;
        let m = d.field("m").ok_or(Refuse("3a m".into()))? as u32 + bank;
        let post = d.field("u") == Some(1);
        let ureg = d.field("ureg").ok_or(Refuse("3a ureg".into()))? as u32;
        let x = if d.field("d") == Some(1) {
            Xfer::Store(ureg)
        } else {
            Xfer::Load(ureg)
        };
        self.dm_transfer(16 + i, 32 + m, post, x)?;
        self.compute_full(d.compute().ok_or(Refuse("3a compute".into()))?)?;
        Ok(())
    }

    pub fn form_3b(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 3b");
        }
        let width = (d.field("l"), d.field("x"), d.field("w"));
        if width != (Some(0), Some(1), Some(1)) {
            return refuse("3b access width");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("3b i".into()))? as u32 + bank;
        let m = d.field("m").ok_or(Refuse("3b m".into()))? as u32 + bank;
        let post = d.field("u") == Some(1);
        let ureg = d.field("ureg").ok_or(Refuse("3b ureg".into()))? as u32;
        let x = if d.field("d") == Some(1) {
            Xfer::Store(ureg)
        } else {
            Xfer::Load(ureg)
        };
        self.dm_transfer(16 + i, 32 + m, post, x)
    }

    pub fn form_3c(&mut self, d: &Dec) -> LR<()> {
        let i = d.field("dmi").ok_or(Refuse("3c dmi".into()))? as u32;
        let m = d.field("dmm").ok_or(Refuse("3c dmm".into()))? as u32;
        let dreg = d.field("dreg").ok_or(Refuse("3c dreg".into()))? as u32;
        let x = if d.field("d") == Some(1) {
            Xfer::Store(dreg)
        } else {
            Xfer::Load(dreg)
        };
        self.dm_transfer(16 + i, 32 + m, true, x)
    }

    pub fn form_6a_mem(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 6a");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("6a i".into()))? as u32 + bank;
        let m = d.field("m").ok_or(Refuse("6a m".into()))? as u32 + bank;
        let dreg = d.field("dreg").ok_or(Refuse("6a dreg".into()))? as u32;
        let x = if d.field("d") == Some(1) {
            Xfer::Store(dreg)
        } else {
            Xfer::Load(dreg)
        };
        self.dm_transfer(16 + i, 32 + m, true, x)?;
        let dataex = d.field("dataex").ok_or(Refuse("dataex".into()))? as u32;
        self.shiftimm(shiftimm_field(d)?, dataex)
    }

    /// 7a: Ia = MODIFY(Ib, Mc) with an optional compute.
    pub fn form_7a(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 7a");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let src_low = ((d.field("is[2:2]").ok_or(Refuse("7a is".into()))? as u32) << 2)
            | d.field("is[1:0]").ok_or(Refuse("7a is".into()))? as u32;
        let dst_low = src_low ^ d.field("idis").ok_or(Refuse("7a idis".into()))? as u32;
        let modifier = d.field("m").ok_or(Refuse("7a m".into()))? as u32 + bank;
        let (src, dst) = (src_low + bank, dst_low + bank);
        // (sw) scales by 2, (nw) by 4 in byte space, plain MODIFY not at all.
        let scale: i64 = if d.get("l") == Some(1) {
            2
        } else if d.get("w") == Some(1) {
            4
        } else {
            1
        };
        let iv = self.rd_i(16 + src)?;
        let mv = self.rd_i(32 + modifier)?;
        if scale == 4 {
            if !self.env.assume_nw32 {
                return refuse("assume_nw32 off");
            }
            self.require_plain(iv, 4)?;
        }
        let scaled = if scale == 1 {
            mv
        } else {
            let k = self.ci(scale as u32);
            self.bin(Bin::Mul, mv, k)
        };
        let new = self.bin(Bin::Add, iv, scaled);
        self.require_linear(16 + src)?;
        self.require_linear(16 + dst)?;
        self.wr_i(16 + dst, new)?;
        self.compute_full(d.compute().ok_or(Refuse("7a compute".into()))?)?;
        Ok(())
    }
}
