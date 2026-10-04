//! Data-memory transfers with an immediate: 4a, 4b, 4d (index plus a signed
//! 6-bit offset, post- or pre-modify), 15a, 15b (pre-modify by a 32- or
//! 7-bit immediate, no update), 14a (absolute address), and the
//! address-only immediate modifies 19a, 19a_scaled and 7b.
//!
//! Same address class as `mem_addr`: normal-word accesses through an index
//! register are byte addresses scaled by 4 (`Req::NwPlain`), unconditional,
//! normal-word width only (byte, short and long-word variants are refused).
//! Every index register the kernel updates needs its length register L to be
//! zero at entry (`require_linear`), so no circular wrap is ever missed.

use super::mem_addr::Xfer;
use super::*;

/// Sign-extend the low BITS bits of V.
fn signed(v: i64, bits: u32) -> i64 {
    let m = 1i64 << (bits - 1);
    ((v & ((1i64 << bits) - 1)) ^ m) - m
}

impl Lower {
    /// A normal-word transfer at `Ic + words*4` (pre-modify, no update) or
    /// at `Ic` followed by `Ic += words*4` (post-modify).
    pub fn dm_imm(&mut self, ic: u32, words: i64, post: bool, x: Xfer) -> LR<()> {
        if !self.env.assume_nw32 {
            return refuse("assume_nw32 off");
        }
        let iv = self.rd_i(ic)?;
        // The 32-bit displacement wraps like the interpreter's sum.
        let off = self.ci((words.wrapping_mul(4)) as u32);
        let modified = self.bin(Bin::Add, iv, off);
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

    fn imm6(d: &Dec) -> LR<i64> {
        let hi = d.get("data[5:5]").ok_or(Refuse("data[5:5]".into()))?;
        let lo = d.get("data[4:0]").ok_or(Refuse("data[4:0]".into()))?;
        Ok(signed((hi << 5) | lo, 6))
    }

    fn xfer_dir(d: &Dec, code: u32) -> Xfer {
        if d.field("d") == Some(1) {
            Xfer::Store(code)
        } else {
            Xfer::Load(code)
        }
    }

    /// 4a: `DM(Ia, imm6)` / `DM(imm6, Ia)` with an optional compute.
    pub fn form_4a(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 4a");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("4a i".into()))? as u32 + bank;
        let dreg = d.field("dreg").ok_or(Refuse("4a dreg".into()))? as u32;
        let post = d.field("u") == Some(1);
        let off = Self::imm6(d)?;
        self.dm_imm(16 + i, off, post, Self::xfer_dir(d, dreg))?;
        self.compute_full(d.compute().ok_or(Refuse("4a compute".into()))?)
    }

    /// 4b (the 4-byte form) and 4d (the 6-byte form with the full width
    /// table): normal-word width only. No compute.
    pub fn form_4b_4d(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 4b/4d");
        }
        // 4b: (l, x, w) = (1, 1, 1) is the plain normal word; 4d, with the
        // 3b table: (0, 1, 1).
        let width = (d.field("l"), d.field("x"), d.field("w"));
        let want = if d.form == "4b" {
            (Some(1), Some(1), Some(1))
        } else {
            (Some(0), Some(1), Some(1))
        };
        if width != want {
            return refuse("4b/4d access width");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("4b i".into()))? as u32 + bank;
        let dreg = d.field("dreg").ok_or(Refuse("4b dreg".into()))? as u32;
        let post = d.field("u") == Some(1);
        let off = Self::imm6(d)?;
        self.dm_imm(16 + i, off, post, Self::xfer_dir(d, dreg))
    }

    /// 15b: `Ureg = DM(imm7, Ia)` / `DM(imm7, Ia) = Ureg`, no update.
    pub fn form_15b(&mut self, d: &Dec) -> LR<()> {
        if d.field("l") == Some(1) {
            return refuse("15b long word");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("15b i".into()))? as u32 + bank;
        let ureg = d.field("ureg").ok_or(Refuse("15b ureg".into()))? as u32;
        let off = signed(d.field("data").ok_or(Refuse("15b data".into()))?, 7);
        self.dm_imm(16 + i, off, false, Self::xfer_dir(d, ureg))
    }

    /// 15a: `Ureg = DM(imm32, Ia)`, no update.
    pub fn form_15a(&mut self, d: &Dec) -> LR<()> {
        if d.field("l") == Some(1) {
            return refuse("15a long word");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("15a i".into()))? as u32 + bank;
        let ureg = d.field("ureg").ok_or(Refuse("15a ureg".into()))? as u32;
        // The unsigned 32-bit field times 4, mod 2^32: the same bits as
        // the signed value times 4.
        let off = d.wide("addr").ok_or(Refuse("15a addr".into()))? as i32 as i64;
        self.dm_imm(16 + i, off, false, Self::xfer_dir(d, ureg))
    }

    /// 14a: an absolute normal-word address in the plain range (a mapped
    /// one is refused: its byte address is not the address).
    pub fn form_14a(&mut self, d: &Dec) -> LR<()> {
        if d.field("l") == Some(1) {
            return refuse("14a long word");
        }
        let ureg = d.field("ureg").ok_or(Refuse("14a ureg".into()))? as u32;
        let a = d.wide("addr").ok_or(Refuse("14a addr".into()))?;
        if !(0xE8000..0x400_0000).contains(&a) {
            return refuse("14a address not in the plain range");
        }
        let addr = self.ci(a);
        self.mem_xfer(addr, Self::xfer_dir(d, ureg))
    }

    /// 19a / 19a_scaled: `Ia = MODIFY(Ib, imm32)`, scaled by the access
    /// size for 19a_scaled.
    pub fn form_19a(&mut self, d: &Dec) -> LR<()> {
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let src_low = d.field("is").ok_or(Refuse("19a is".into()))? as u32;
        let dst_low = src_low ^ d.field("idis").ok_or(Refuse("19a idis".into()))? as u32;
        let (src, dst) = (src_low + bank, dst_low + bank);
        let mut delta = d.wide("data").ok_or(Refuse("19a data".into()))? as i32 as i64;
        if d.form == "19a_scaled" {
            if !self.env.assume_nw32 {
                return refuse("assume_nw32 off");
            }
            delta *= if d.field("w") == Some(1) { 4 } else { 2 };
        }
        let iv = self.rd_i(16 + src)?;
        let k = self.ci(delta as u32);
        let new = self.bin(Bin::Add, iv, k);
        self.require_linear(16 + src)?;
        self.require_linear(16 + dst)?;
        self.wr_i(16 + dst, new)
    }

    /// 7b: `Ia = MODIFY(Ib, Mc)` (plain, unscaled), unconditional.
    pub fn form_7b(&mut self, d: &Dec) -> LR<()> {
        if d.field("cond") != Some(0x1f) {
            return refuse("conditional 7b");
        }
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let src_low = ((d.get("is[2:2]").ok_or(Refuse("7b is".into()))?) << 2)
            | d.get("is[1:0]").ok_or(Refuse("7b is".into()))?;
        let src_low = src_low as u32;
        let dst_low = src_low ^ d.field("idis").ok_or(Refuse("7b idis".into()))? as u32;
        let m = d.field("m").ok_or(Refuse("7b m".into()))? as u32 + bank;
        let (src, dst) = (src_low + bank, dst_low + bank);
        let iv = self.rd_i(16 + src)?;
        let mv = self.rd_i(32 + m)?;
        let new = self.bin(Bin::Add, iv, mv);
        self.require_linear(16 + src)?;
        self.require_linear(16 + dst)?;
        self.wr_i(16 + dst, new)
    }
}
