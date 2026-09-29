//! 128-bit integers whose range does not fit an i64 (the multiplier's
//! 80-bit accumulators, Python-int shifts): a pair of i64 locals (low,
//! high), two's complement. Multiplication and shifts by run-time amounts
//! call runtime helpers; the rest is inline.

use super::eval::{Pe, R, fail};
use super::ir::W;
use super::mach::helper;
use super::ops::int_info;
use super::val::*;
use wasm_encoder::Instruction as I;

/// One half of a 128-bit operand: a constant or a local.
#[derive(Clone, Copy, Debug)]
pub enum Wp {
    C(i64),
    L(u32),
}

pub fn fits64(r: (i128, i128)) -> bool {
    r.0 >= i64::MIN as i128 && r.1 <= i64::MAX as i128
}

impl<'p> Pe<'p> {
    pub fn wpush(&mut self, p: Wp) {
        match p {
            Wp::C(x) => self.emit(I::I64Const(x)),
            Wp::L(l) => self.emit(I::LocalGet(l)),
        }
    }

    /// The (low, high) halves of integer V as a 128-bit value.
    pub fn wparts(&mut self, v: &Av) -> R<(Wp, Wp)> {
        match v {
            Av::Int(x, _) => Ok((Wp::C(*x as i64), Wp::C((*x >> 64) as i64))),
            Av::Bool(b) => Ok((Wp::C(*b as i64), Wp::C(0))),
            Av::D(d) => {
                let d = d.clone();
                match d.k {
                    Kind::Int(_, Rep::Wide) => Ok((Wp::L(d.l), Wp::L(d.l2))),
                    Kind::Int(t, rep) => {
                        let lo = if rep == Rep::I32 {
                            self.emit(I::LocalGet(d.l));
                            if t.signed() {
                                self.emit(I::I64ExtendI32S);
                            } else {
                                self.emit(I::I64ExtendI32U);
                            }
                            let l = self.local(W::I64);
                            self.emit(I::LocalSet(l));
                            l
                        } else {
                            d.l
                        };
                        // The value is exact for 128-bit types (range fits)
                        // and for signed types; an unsigned 64-bit type is
                        // non-negative.
                        let unsigned64 = t.bits() == 64 && !t.signed();
                        let hi = if unsigned64 || d.lo >= 0 {
                            Wp::C(0)
                        } else if d.hi < 0 {
                            Wp::C(-1)
                        } else {
                            self.emit(I::LocalGet(lo));
                            self.emit(I::I64Const(63));
                            self.emit(I::I64ShrS);
                            let h = self.local(W::I64);
                            self.emit(I::LocalSet(h));
                            Wp::L(h)
                        };
                        Ok((Wp::L(lo), hi))
                    }
                    Kind::Bool => {
                        self.emit(I::LocalGet(d.l));
                        self.emit(I::I64ExtendI32U);
                        let l = self.local(W::I64);
                        self.emit(I::LocalSet(l));
                        Ok((Wp::L(l), Wp::C(0)))
                    }
                    _ => fail("128-bit value of a float"),
                }
            }
            _ => fail(format!("128-bit operand {v:?}")),
        }
    }

    /// A 128-bit result in locals LO, HI with range R: an i64 when it fits.
    pub fn wresult(&mut self, t: IT, lo: u32, hi: u32, r: (i128, i128)) -> Av {
        let t = if t == IT::Lit { IT::I128 } else { t };
        if fits64(r) {
            return Av::D(Dv {
                l: lo,
                l2: lo,
                k: Kind::Int(t, Rep::I64),
                lo: r.0,
                hi: r.1,
                vset: None,
            });
        }
        Av::D(Dv {
            l: lo,
            l2: hi,
            k: Kind::Int(t, Rep::Wide),
            lo: r.0,
            hi: r.1,
            vset: None,
        })
    }

    fn wset(&mut self) -> u32 {
        let l = self.local(W::I64);
        self.emit(I::LocalSet(l));
        l
    }

    pub fn wide_op(&mut self, op: &str, a: &Av, b: &Av, t: IT, r: (i128, i128)) -> R<Av> {
        let (alo, ahi) = self.wparts(a)?;
        let (blo, bhi) = self.wparts(b)?;
        let (lo, hi) = match op {
            "+" => {
                self.wpush(alo);
                self.wpush(blo);
                self.emit(I::I64Add);
                let lo = self.wset();
                self.wpush(ahi);
                self.wpush(bhi);
                self.emit(I::I64Add);
                self.emit(I::LocalGet(lo));
                self.wpush(alo);
                self.emit(I::I64LtU);
                self.emit(I::I64ExtendI32U);
                self.emit(I::I64Add);
                (lo, self.wset())
            }
            "-" => {
                self.wpush(alo);
                self.wpush(blo);
                self.emit(I::I64Sub);
                let lo = self.wset();
                self.wpush(ahi);
                self.wpush(bhi);
                self.emit(I::I64Sub);
                self.wpush(alo);
                self.wpush(blo);
                self.emit(I::I64LtU);
                self.emit(I::I64ExtendI32U);
                self.emit(I::I64Sub);
                (lo, self.wset())
            }
            "&" | "|" | "^" => {
                let ins = match op {
                    "&" => I::I64And,
                    "|" => I::I64Or,
                    _ => I::I64Xor,
                };
                self.wpush(alo);
                self.wpush(blo);
                self.emit(ins.clone());
                let lo = self.wset();
                self.wpush(ahi);
                self.wpush(bhi);
                self.emit(ins);
                (lo, self.wset())
            }
            "*" => {
                let (_, ra) = int_info(a);
                let (_, rb) = int_info(b);
                if ra.0 >= 0 && rb.0 >= 0 && ra.1 <= u64::MAX as i128 && rb.1 <= u64::MAX as i128 && (ra.1 as u128).checked_mul(rb.1 as u128).is_some_and(|p| p <= u64::MAX as u128) {
                    // Both non-negative and the product fits 64 unsigned
                    // bits: one multiply, high half zero.
                    self.wpush(alo);
                    self.wpush(blo);
                    self.emit(I::I64Mul);
                    let lo = self.wset();
                    self.emit(I::I64Const(0));
                    (lo, self.wset())
                } else {
                    self.wpush(alo);
                    self.wpush(ahi);
                    self.wpush(blo);
                    self.wpush(bhi);
                    self.emit(I::Call(helper("jit_mul128")));
                    self.scratch_pair()
                }
            }
            _ => return fail(format!("128-bit {op}")),
        };
        Ok(self.wresult(t, lo, hi, r))
    }

    fn scratch_pair(&mut self) -> (u32, u32) {
        let s = self.cx.lay.scratch as i32;
        self.emit(I::I32Const(s));
        self.emit(I::I64Load(super::ir::mem(0, 3)));
        let lo = self.wset();
        self.emit(I::I32Const(s));
        self.emit(I::I64Load(super::ir::mem(8, 3)));
        (lo, self.wset())
    }

    pub fn wide_shift(&mut self, op: &str, a: &Av, b: &Av, t: IT, r: (i128, i128)) -> R<Av> {
        let (alo, ahi) = self.wparts(a)?;
        let signed = t.signed() || t == IT::I128;
        let (lo, hi) = match b.as_int() {
            Some(n) => {
                let k = (n as u32) & 127;
                self.wide_shift_const(op, alo, ahi, k, signed)
            }
            None => {
                self.wpush(alo);
                self.wpush(ahi);
                let bv = self.cast_int_to(b, IT::U32)?;
                self.push_as(&bv, Kind::Int(IT::U32, Rep::I32))?;
                self.emit(I::I32Const(127));
                self.emit(I::I32And);
                if op == "<<" {
                    self.emit(I::Call(helper("jit_shl128")));
                } else {
                    self.emit(I::I32Const(signed as i32));
                    self.emit(I::Call(helper("jit_shr128")));
                }
                self.scratch_pair()
            }
        };
        Ok(self.wresult(t, lo, hi, r))
    }

    fn wide_shift_const(&mut self, op: &str, alo: Wp, ahi: Wp, k: u32, signed: bool) -> (u32, u32) {
        let k = k as i64;
        if k == 0 {
            self.wpush(alo);
            let lo = self.wset();
            self.wpush(ahi);
            return (lo, self.wset());
        }
        if op == "<<" {
            if k < 64 {
                self.wpush(alo);
                self.emit(I::I64Const(k));
                self.emit(I::I64Shl);
                let lo = self.wset();
                self.wpush(ahi);
                self.emit(I::I64Const(k));
                self.emit(I::I64Shl);
                self.wpush(alo);
                self.emit(I::I64Const(64 - k));
                self.emit(I::I64ShrU);
                self.emit(I::I64Or);
                (lo, self.wset())
            } else {
                self.emit(I::I64Const(0));
                let lo = self.wset();
                self.wpush(alo);
                self.emit(I::I64Const(k - 64));
                self.emit(I::I64Shl);
                (lo, self.wset())
            }
        } else {
            let shr = if signed { I::I64ShrS } else { I::I64ShrU };
            if k < 64 {
                self.wpush(alo);
                self.emit(I::I64Const(k));
                self.emit(I::I64ShrU);
                self.wpush(ahi);
                self.emit(I::I64Const(64 - k));
                self.emit(I::I64Shl);
                self.emit(I::I64Or);
                let lo = self.wset();
                self.wpush(ahi);
                self.emit(I::I64Const(k));
                self.emit(shr);
                (lo, self.wset())
            } else {
                self.wpush(ahi);
                self.emit(I::I64Const(k - 64));
                self.emit(shr.clone());
                let lo = self.wset();
                if signed {
                    self.wpush(ahi);
                    self.emit(I::I64Const(63));
                    self.emit(I::I64ShrS);
                } else {
                    self.emit(I::I64Const(0));
                }
                (lo, self.wset())
            }
        }
    }

    pub fn wide_cmp(&mut self, op: &str, a: &Av, b: &Av, t: IT) -> R<Av> {
        let (alo, ahi) = self.wparts(a)?;
        let (blo, bhi) = self.wparts(b)?;
        let signed = t.signed() || t == IT::I128;
        match op {
            "==" | "!=" => {
                self.wpush(alo);
                self.wpush(blo);
                self.emit(I::I64Eq);
                self.wpush(ahi);
                self.wpush(bhi);
                self.emit(I::I64Eq);
                self.emit(I::I32And);
                if op == "!=" {
                    self.emit(I::I32Eqz);
                }
            }
            _ => {
                // x < y with (x, y) = (a, b) or (b, a); negated for >= / <=.
                let (x, y, neg) = match op {
                    "<" => ((alo, ahi), (blo, bhi), false),
                    ">" => ((blo, bhi), (alo, ahi), false),
                    ">=" => ((alo, ahi), (blo, bhi), true),
                    _ => ((blo, bhi), (alo, ahi), true),
                };
                self.wpush(x.1);
                self.wpush(y.1);
                self.emit(if signed { I::I64LtS } else { I::I64LtU });
                self.wpush(x.1);
                self.wpush(y.1);
                self.emit(I::I64Eq);
                self.wpush(x.0);
                self.wpush(y.0);
                self.emit(I::I64LtU);
                self.emit(I::I32And);
                self.emit(I::I32Or);
                if neg {
                    self.emit(I::I32Eqz);
                }
            }
        }
        Ok(self.def_bool())
    }

    /// Store 128-bit V into the locals (LO, HI) (a join's materialization).
    pub fn wstore(&mut self, v: &Av, lo: u32, hi: u32) -> R<()> {
        let (a, b) = self.wparts(v)?;
        self.wpush(a);
        self.emit(I::LocalSet(lo));
        self.wpush(b);
        self.emit(I::LocalSet(hi));
        Ok(())
    }
}
