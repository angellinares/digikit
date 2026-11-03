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
    /// A load whose value is left in `Lower::xout` (generated forms).
    LoadV,
    /// A store of an already read value (generated forms).
    StoreV(Val),
}

/// The width of a transfer: a normal word (4 bytes; the address is a byte
/// address scaled by 4 in the plain range), or a short word / byte in byte
/// space (`memory._access_modifier_scale`: modifier scaled by 2 / 1, the
/// address never mapped; the loaded value is zero- or, `signed` (`bwse`,
/// `swse`), sign-extended; a sign-extended store is not an instruction).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Width {
    pub bytes: u8,
    pub signed: bool,
}

impl Width {
    pub const NW: Width = Width {
        bytes: 4,
        signed: false,
    };

    /// The access width of a Type 3b / 4d `(l, x, w)` (PRM p.13-16/18,
    /// `encoding.ACCESS_WIDTHS`; with FORM4B the Type 4b table
    /// `TYPE4B_ACCESS_WIDTHS`), or None for a long word or an unknown row.
    pub fn from_fields(form4b: bool, d: &Dec) -> Option<Width> {
        let (l, x, w) = (d.field("l")?, d.field("x")?, d.field("w")?);
        let (bytes, signed) = match (l, x, w) {
            (0, 1, 1) if !form4b => (4, false),
            (1, 1, 1) if form4b => (4, false),
            (0, 0, 0) => (1, false),
            (0, 1, 0) => (1, true),
            (1, 0, 0) => (2, false),
            (1, 1, 0) => (2, true),
            _ => return None,
        };
        Some(Width { bytes, signed })
    }

    pub fn is_nw(self) -> bool {
        self.bytes == 4
    }

    /// The modifier's scale in bytes (`_access_modifier_scale`, for a
    /// normal word with the index register outside the mapped ranges).
    pub fn scale(self) -> u32 {
        self.bytes as u32
    }
}

/// Guarded dynamic windows for transfers whose address has no static range,
/// in every region (not only clusters). On unless `SHARC_FAST_NOGUARD` is set.
pub(super) fn guarded_xfers() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SHARC_FAST_NOGUARD").is_none())
}

impl Lower {
    /// A normal-word transfer through index register IC with modifier MC.
    pub fn dm_transfer(&mut self, ic: u32, mc: u32, post: bool, x: Xfer) -> LR<()> {
        self.dm_transfer_w(ic, mc, post, x, Width::NW)
    }

    /// A transfer of width W through index register IC with modifier MC.
    pub fn dm_transfer_w(&mut self, ic: u32, mc: u32, post: bool, x: Xfer, w: Width) -> LR<()> {
        if !self.env.assume_nw32 {
            return refuse("assume_nw32 off");
        }
        if matches!(x, Xfer::Store(_) | Xfer::StoreV(_)) && w.signed {
            return refuse("sign-extended store");
        }
        let iv = self.rd_i(ic)?;
        let mv = self.rd_i(mc)?;
        let k = self.ci(w.scale());
        let scaled = self.bin(Bin::Mul, mv, k);
        let modified = self.bin(Bin::Add, iv, scaled);
        let addr = if post { iv } else { modified };
        self.xfer_at(ic, iv, addr, post, x, w)?;
        if post {
            self.post_update_scaled(ic, iv, scaled, modified, w.scale() as i64)?;
        }
        Ok(())
    }

    /// The access itself, at byte address ADDR (the index register IC holds
    /// IV): through a static window when the address has a range, else, in
    /// clusters (`Env::guarded_dyn`) and for loads behind
    /// `SHARC_FAST_GUARDED_READS`, through a guarded dynamic window.
    pub fn xfer_at(
        &mut self,
        ic: u32,
        iv: Val,
        addr: Val,
        post: bool,
        x: Xfer,
        w: Width,
    ) -> LR<()> {
        let guarded = self.env.guarded_dyn || guarded_xfers();
        // The address has no static range. The window is anchored on the
        // register whose entry value IV is a known offset from, else (IV has
        // no range) on the register whose entry value it was moved and
        // post-modified from (`Lower::hint`). Only a hint: the guards make any anchor sound.
        let anchor = match self.abs[iv as usize] {
            Some(Abs {
                base: Base::Reg(c), ..
            }) => Some(c),
            Some(_) => None,
            None => self.hint[ic as usize],
        };
        // Earlier passes only collect facts: no windows are made for them.
        let dynamic = guarded
            && self.strict()
            && self.abs[addr as usize].is_none()
            && anchor.is_some()
            && !self.flat_dyn_cur;
        let iv_ranged = self.abs[iv as usize].is_some();
        // No range and no anchor: access the engine's memory through its page
        // table, guarding every access (`flat_access`).
        if !dynamic
            && self.flat_ok()
            && self.strict()
            && !iv_ranged
            && self.abs[addr as usize].is_none()
        {
            if w.is_nw() {
                self.guard_plain(iv);
                if !post {
                    self.guard_plain(addr);
                }
            }
            let write = matches!(x, Xfer::Store(_) | Xfer::StoreV(_));
            let (p, o) = self.flat_access(addr, w.bytes, write)?;
            return self.emit_access(p, o, x, w);
        }
        if w.is_nw() && (iv_ranged || !dynamic) {
            // The modifier is scaled by 4 only when IV is outside the
            // normal-word mapped ranges.
            self.require_plain(iv, 4)?;
        }
        if dynamic {
            // Resolve one plain span around the index register's entry value
            // and side-exit before this instruction if the wrapping u32
            // address falls outside it. A store reads its value after the
            // guard (a pure register read) and writes only after every guard
            // of the instruction (`pend_store_n`). The span is not part of
            // the proof: any span the memory vouches for is sound, and every
            // access is checked against it. A normal-word pre-modify whose
            // IV has no range needs IV plain (a mapped IV scales by 1): IV
            // must lie in the span too, which is wholly plain.
            let iv_in_span = w.is_nw() && !post && !iv_ranged;
            return self.dyn_xfer(anchor.unwrap(), addr, iv_in_span.then_some(iv), x, w);
        }
        if w.is_nw() && !post {
            self.require_plain(addr, 4)?;
        }
        self.mem_xfer_w(addr, x, w)
    }

    /// A guarded dynamic-window access at ADDR (and, with EXTRA, a second
    /// address that must lie in the same span) through the window anchored on
    /// index register ANCHOR.
    fn dyn_xfer(
        &mut self,
        anchor: u8,
        addr: Val,
        extra: Option<Val>,
        x: Xfer,
        w: Width,
    ) -> LR<()> {
        let write = matches!(x, Xfer::Store(_) | Xfer::StoreV(_));
        let (p, o, slot) = self.dynamic_window(addr, anchor, write)?;
        let lo = self.emit(Ty::I32, Op::Ctx32(CTX_WIN_EXT + 8 * slot));
        let span = self.emit(Ty::I32, Op::Ctx32(CTX_WIN_DYN_LEN + 4 * slot));
        let width = self.ci(w.bytes as u32);
        let max = self.bin(Bin::Sub, span, width);
        let rel = self.bin(Bin::Sub, addr, lo);
        let inside = self.bin(Bin::LeU, rel, max);
        // An empty span (a window that could not be resolved) never admits
        // an access: SPAN - WIDTH would wrap.
        let roomy = self.bin(Bin::LeU, width, span);
        let inside = self.bin(Bin::And, inside, roomy);
        self.guard(inside);
        if let Some(e) = extra {
            let rel = self.bin(Bin::Sub, e, lo);
            let inside = self.bin(Bin::LeU, rel, max);
            self.guard(inside);
        }
        self.emit_access(p, o, x, w)
    }

    /// The page and offset of a guarded access of BYTES (1, 2 or 4) bytes at
    /// byte address ADDR, through the engine memory's page table
    /// (`Mem::fast_read` / `fast_write` inline): the access leaves the kernel
    /// (before the instruction) unless the page exists, every byte is present
    /// (loads) or already an overlay byte (stores, which also need the page
    /// not code-watched). Loads must stay on one page; stores must stay inside
    /// one 32-bit word of the bitmaps. No address range is needed.
    pub fn flat_access(&mut self, addr: Val, bytes: u8, write: bool) -> LR<(Val, Val)> {
        use crate::mem::{PAGE_BITS, PAGE_DIRTY_OFF, PAGE_PRESENT_OFF, PAGE_SIZE};
        let ptr64 = std::mem::size_of::<usize>() == 8;
        #[cfg(test)]
        let ptr64 = self.flat_ptr64.unwrap_or(ptr64);
        let eff = self.emit(Ty::I64, Op::Ctx64(CTX_FLAT_EFF));
        let bits = self.ci(PAGE_BITS);
        let idx = self.bin(Bin::ShrU, addr, bits);
        let psh = self.ci(if ptr64 { 3 } else { 2 });
        let eoff = self.bin(Bin::Shl, idx, psh);
        let lo = self.emit(Ty::I32, Op::Load32 { base: eff, off: eoff });
        let page = if ptr64 {
            let four = self.ci(4);
            let hoff = self.bin(Bin::Add, eoff, four);
            let hi = self.emit(Ty::I32, Op::Load32 { base: eff, off: hoff });
            let any = self.bin(Bin::Or, lo, hi);
            let zero = self.ci(0);
            let nz = self.bin(Bin::Ne, any, zero);
            self.guard(nz);
            self.emit(Ty::I64, Op::Load64 { base: eff, off: eoff })
        } else {
            let zero = self.ci(0);
            let nz = self.bin(Bin::Ne, lo, zero);
            self.guard(nz);
            self.emit(Ty::I64, Op::Un(Un::Zext, lo))
        };
        let mask16 = self.ci((1 << PAGE_BITS) - 1);
        let o16 = self.bin(Bin::And, addr, mask16);
        let c31 = self.ci(31);
        let bit = self.bin(Bin::And, o16, c31);
        let (offset, room) = if write || bytes == 1 {
            // Keep stores inside one bitmap word.
            (bit, 32 - bytes as u32)
        } else {
            (o16, PAGE_SIZE as u32 - bytes as u32)
        };
        let room = self.ci(room);
        let fits = self.bin(Bin::LeU, offset, room);
        self.guard(fits);
        let c5 = self.ci(5);
        let word = self.bin(Bin::ShrU, o16, c5);
        let c2 = self.ci(2);
        let word4 = self.bin(Bin::Shl, word, c2);
        let map = self.ci(if write { PAGE_DIRTY_OFF } else { PAGE_PRESENT_OFF });
        let moff = self.bin(Bin::Add, word4, map);
        let bm = self.emit(Ty::I32, Op::Load32 { base: page, off: moff });
        let ones = self.ci((1u32 << bytes) - 1);
        let mask = self.bin(Bin::Shl, ones, bit);
        let have = self.bin(Bin::And, bm, mask);
        let ok = self.bin(Bin::Eq, have, mask);
        self.guard(ok);
        if !write && bytes > 1 {
            // The page guard keeps both bitmap reads in bounds. Only accesses
            // crossing a bitmap word need the remaining low bits in the next.
            let done = self.new_label();
            let room = self.ci(32 - bytes as u32);
            let fits_word = self.bin(Bin::LeU, bit, room);
            self.body.push(Inst::BrIf {
                c: fits_word,
                target: done,
            });
            let extra = self.ci(bytes as u32 - 1);
            let last = self.bin(Bin::Add, o16, extra);
            let last_word = self.bin(Bin::ShrU, last, c5);
            let last_word4 = self.bin(Bin::Shl, last_word, c2);
            let last_moff = self.bin(Bin::Add, last_word4, map);
            let last_bm = self.emit(
                Ty::I32,
                Op::Load32 {
                    base: page,
                    off: last_moff,
                },
            );
            let c32 = self.ci(32);
            let shift = self.bin(Bin::Sub, c32, bit);
            let tail = self.bin(Bin::ShrU, ones, shift);
            let have = self.bin(Bin::And, last_bm, tail);
            let ok = self.bin(Bin::Eq, have, tail);
            self.guard(ok);
            self.body.push(Inst::Label(done));
        }
        if write {
            let watch = self.emit(Ty::I64, Op::Ctx64(CTX_FLAT_WATCH));
            let w = self.emit(Ty::I32, Op::LoadN { base: watch, off: idx, bytes: 1 });
            let zero = self.ci(0);
            let quiet = self.bin(Bin::Eq, w, zero);
            self.guard(quiet);
        }
        Ok((page, o16))
    }

    /// The load or store of W at window pointer P plus byte offset O.
    fn emit_access(&mut self, p: Val, o: Val, x: Xfer, w: Width) -> LR<()> {
        match x {
            Xfer::Load(_) | Xfer::LoadV => {
                let v = if w.is_nw() {
                    self.emit(Ty::I32, Op::Load32 { base: p, off: o })
                } else {
                    let v = self.emit(
                        Ty::I32,
                        Op::LoadN {
                            base: p,
                            off: o,
                            bytes: w.bytes,
                        },
                    );
                    if w.signed {
                        let sh = self.ci(32 - 8 * w.bytes as u32);
                        let up = self.bin(Bin::Shl, v, sh);
                        self.bin(Bin::ShrS, up, sh)
                    } else {
                        v
                    }
                };
                match x {
                    Xfer::Load(code) => self.wr(code, v),
                    _ => {
                        self.xout = Some(v);
                        Ok(())
                    }
                }
            }
            Xfer::Store(code) => {
                let val = self.rd_bits(code)?;
                self.pend_store_n(p, o, val, w.bytes);
                Ok(())
            }
            Xfer::StoreV(val) => {
                self.pend_store_n(p, o, val, w.bytes);
                Ok(())
            }
        }
    }

    /// Guard that the byte address V lies in a range where a normal-word
    /// access is plain (`glue::NW_PLAIN`, a subset of the addresses
    /// `addressing::normal_word_to_byte` leaves alone): leave before the
    /// instruction otherwise.
    #[track_caller]
    pub fn guard_plain(&mut self, v: Val) {
        let mut any: Option<Val> = None;
        for &(lo, hi) in super::super::glue::NW_PLAIN.iter() {
            let base = self.ci(lo as u32);
            let rel = self.bin(Bin::Sub, v, base);
            let len = self.ci((hi - lo) as u32);
            let inside = self.bin(Bin::LtU, rel, len);
            any = Some(match any {
                Some(a) => self.bin(Bin::Or, a, inside),
                None => inside,
            });
        }
        if let Some(ok) = any {
            self.guard(ok);
        }
    }

    /// The load or store at byte address ADDR (already checked to be a
    /// plain normal-word address).
    pub fn mem_xfer(&mut self, addr: Val, x: Xfer) -> LR<()> {
        self.mem_xfer_w(addr, x, Width::NW)
    }

    /// The load or store of W at byte address ADDR (a normal word already
    /// checked to be plain; a byte or short word needs no such check).
    pub fn mem_xfer_w(&mut self, addr: Val, x: Xfer, w: Width) -> LR<()> {
        match x {
            Xfer::Load(_) | Xfer::LoadV => {
                let (p, o) = self.window_access(addr, w.bytes as i64, false)?;
                self.emit_access(p, o, x, w)
            }
            Xfer::StoreV(_) => {
                let (p, o) = self.window_access(addr, w.bytes as i64, true)?;
                self.emit_access(p, o, x, w)
            }
            Xfer::Store(code) => {
                // The value is read before the window is declared.
                let val = self.rd_bits(code)?;
                let (p, o) = self.window_access(addr, w.bytes as i64, true)?;
                self.pend_store_n(p, o, val, w.bytes);
                Ok(())
            }
        }
    }

    /// The kernel updates index register IC (UREG code 16..31): its length
    /// register L must be zero (no circular buffer, so the update is
    /// the plain sum whatever MODE1.CBUFEN says). A requirement on L at entry.
    pub fn require_linear(&mut self, ic: u32) -> LR<()> {
        if !(16..32).contains(&ic) {
            return refuse("require_linear: not an index register");
        }
        if self.lb_stale((ic - 16) as usize) {
            // The entry L is not the L the update sees: a write of L or B
            // earlier in the region can turn on the wrap.
            return refuse("L/B of the index register written in the region");
        }
        if self.env.dag_l[(ic - 16) as usize].is_some_and(|l| l != 0) {
            // MODIFY wraps on B/L whatever MODE1.CBUFEN says; not lowered.
            return refuse("MODIFY on an index register with L != 0");
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
        let Some(w) = Width::from_fields(false, d) else {
            return refuse("3b access width");
        };
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
        self.dm_transfer_w(16 + i, 32 + m, post, x, w)
    }

    /// 3d: the 48-bit re-encoding of 3b with byte and short accesses (no
    /// compute). Only the ACCESS group (w = 0) without the exclusive monitor
    /// (ex = 0) is lowered, as `forms_move._type_3d` runs it.
    pub fn form_3d(&mut self, d: &Dec) -> LR<()> {
        if d.field("w") != Some(0) {
            return refuse("3d WACCESS");
        }
        if d.field("ex") != Some(0) {
            return refuse("3d exclusive access");
        }
        let Some(w) = Width::from_fields(false, d) else {
            return refuse("3d access width");
        };
        let bank = if d.field("g") == Some(1) { 8 } else { 0 };
        let i = d.field("i").ok_or(Refuse("3d i".into()))? as u32 + bank;
        let m = d.field("m").ok_or(Refuse("3d m".into()))? as u32 + bank;
        let post = d.field("u") == Some(1);
        let ureg = d.field("ureg").ok_or(Refuse("3d ureg".into()))? as u32;
        let x = if d.field("d") == Some(1) {
            Xfer::Store(ureg)
        } else {
            Xfer::Load(ureg)
        };
        self.dm_transfer_w(16 + i, 32 + m, post, x, w)
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
            if (self.env.guarded_dyn || guarded_xfers()) && self.abs[iv as usize].is_none() {
                // No range for IV: check it per execution instead of at
                // entry (a failing guard leaves before the instruction).
                self.guard_plain(iv);
            } else {
                self.require_plain(iv, 4)?;
            }
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
        self.note_modify(16 + src, 16 + dst);
        self.wr_i(16 + dst, new)?;
        self.compute_full(d.compute().ok_or(Refuse("7a compute".into()))?)?;
        Ok(())
    }
}
