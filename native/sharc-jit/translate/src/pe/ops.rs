//! Arithmetic, comparisons, casts and primitive methods: folded when the
//! operands are static, emitted with concrete WebAssembly types otherwise.
//! A 128-bit Rust value is held in an i64 whenever its range fits.

use super::eval::{Fail, Flow, Pe, R, fail};
use super::ir::Node;
use super::types::RTy;
use super::ir::W;
use super::val::*;
use super::wide::fits64;
use std::rc::Rc;
use wasm_encoder::Instruction as I;

const CANON_NAN64: u64 = 0x7FF8_0000_0000_0000;
const CANON_NAN32: u32 = 0x7FC0_0000;

fn canon64(r: f64) -> f64 {
    if r.is_nan() { f64::from_bits(CANON_NAN64) } else { r }
}
fn canon32(r: f32) -> f32 {
    if r.is_nan() { f32::from_bits(CANON_NAN32) } else { r }
}

fn bitlen(x: i128) -> u32 {
    if x >= 0 { 128 - x.leading_zeros() } else { 128 - (!x).leading_zeros() }
}

/// [-(2^b), 2^b - 1] covering every value of b bits (two's complement).
fn span(b: u32) -> (i128, i128) {
    if b >= 127 {
        (i128::MIN, i128::MAX)
    } else {
        (-(1i128 << b), (1i128 << b) - 1)
    }
}

fn unify(a: IT, b: IT) -> IT {
    if a == IT::Lit { b } else { a }
}

impl<'p> Pe<'p> {
    // --------------------------------------------------------- stack pushes

    pub fn get_dv(&mut self, d: &Dv) {
        self.emit(I::LocalGet(d.l));
    }

    /// Push scalar V onto the stack as kind K (converting representations).
    pub fn push_as(&mut self, v: &Av, k: Kind) -> R<()> {
        match (v, k) {
            (Av::Int(x, _), Kind::Int(it, rep)) => {
                let x = it.wrap(*x);
                match rep {
                    Rep::I32 => self.emit(I::I32Const(x as i32)),
                    Rep::I64 => self.emit(I::I64Const(x as i64)),
                    Rep::Wide => return fail("wide constant"),
                }
            }
            (Av::Int(x, _), Kind::Bool) => self.emit(I::I32Const((*x != 0) as i32)),
            (Av::Bool(b), Kind::Bool) | (Av::Bool(b), Kind::Int(_, Rep::I32)) => self.emit(I::I32Const(*b as i32)),
            (Av::Bool(b), Kind::Int(_, Rep::I64)) => self.emit(I::I64Const(*b as i64)),
            (Av::F64(x), Kind::F64) => self.emit(I::F64Const((*x).into())),
            (Av::F32(x), Kind::F32) => self.emit(I::F32Const((*x).into())),
            (Av::D(d), _) => {
                if d.k == k {
                    self.get_dv(d);
                    return Ok(());
                }
                match (d.k, k) {
                    (Kind::Int(_, Rep::Wide), Kind::Int(_, Rep::I64)) => self.get_dv(d),
                    (Kind::Int(_, Rep::Wide), Kind::Int(_, Rep::I32)) => {
                        self.get_dv(d);
                        self.emit(I::I32WrapI64);
                    }
                    (_, Kind::Int(_, Rep::Wide)) => return fail("push of a 128-bit pair"),
                    (Kind::Int(_, a), Kind::Int(_, b)) | (Kind::Int(_, a), Kind::Int(_, b)) if a == b => self.get_dv(d),
                    (Kind::Bool, Kind::Int(_, Rep::I32)) | (Kind::Int(_, Rep::I32), Kind::Bool) => self.get_dv(d),
                    (Kind::Int(from, Rep::I32), Kind::Int(_, Rep::I64)) => {
                        self.get_dv(d);
                        if from.signed() {
                            self.emit(I::I64ExtendI32S);
                        } else {
                            self.emit(I::I64ExtendI32U);
                        }
                    }
                    (Kind::Bool, Kind::Int(_, Rep::I64)) => {
                        self.get_dv(d);
                        self.emit(I::I64ExtendI32U);
                    }
                    (Kind::Int(_, Rep::I64), Kind::Int(_, Rep::I32)) => {
                        self.get_dv(d);
                        self.emit(I::I32WrapI64);
                    }
                    _ => return fail(format!("cannot push {:?} as {k:?}", d.k)),
                }
            }
            _ => return fail(format!("cannot push {v:?} as {k:?}")),
        }
        Ok(())
    }

    /// A new dynamic value of kind K from the value on the stack.
    pub fn def(&mut self, k: Kind, lo: i128, hi: i128) -> Dv {
        let l = self.local(k.w());
        self.emit(I::LocalSet(l));
        Dv {
            l,
            l2: l,
            k,
            lo,
            hi,
            vset: None,
        }
    }

    pub fn def_int(&mut self, it: IT, rep: Rep, lo: i128, hi: i128) -> Av {
        Av::D(self.def(Kind::Int(it, rep), lo, hi))
    }

    pub fn def_bool(&mut self) -> Av {
        Av::D(self.def(Kind::Bool, 0, 1))
    }

    // ------------------------------------------------------------- binops

    pub fn binop(&mut self, op: &str, a: Av, b: Av) -> R<Av> {
        // Static folding.
        match (&a, &b) {
            (Av::Int(x, ta), Av::Int(y, tb)) => return self.fold_int(op, *x, *ta, *y, *tb),
            (Av::Bool(x), Av::Bool(y)) => {
                return Ok(Av::Bool(match op {
                    "&" | "&&" => *x && *y,
                    "|" | "||" => *x || *y,
                    "^" | "!=" => *x != *y,
                    "==" => *x == *y,
                    "<" => !*x && *y,
                    ">" => *x && !*y,
                    "<=" => *x <= *y,
                    ">=" => *x >= *y,
                    _ => return fail(format!("bool {op}")),
                }));
            }
            (Av::F64(x), Av::F64(y)) => return fold_f64(op, *x, *y),
            (Av::F32(x), Av::F32(y)) => return fold_f32(op, *x, *y),
            (Av::F64(x), Av::Int(y, IT::Lit)) => return fold_f64(op, *x, *y as f64),
            (Av::Int(x, IT::Lit), Av::F64(y)) => return fold_f64(op, *x as f64, *y),
            _ => {}
        }
        if let (Some(ka), Some(kb)) = (kind_of(&a), kind_of(&b)) {
            return match (ka, kb) {
                (Kind::F64, Kind::F64) | (Kind::F32, Kind::F32) => self.float_op(op, a, b, ka),
                (Kind::F64, Kind::Int(IT::Lit, _)) => {
                    let b = Av::F64(b.as_int().unwrap() as f64);
                    self.float_op(op, a, b, ka)
                }
                (Kind::Bool, Kind::Bool) => self.bool_op(op, a, b),
                (Kind::Int(..), Kind::Int(..)) => self.int_op(op, a, b),
                (Kind::Bool, Kind::Int(..)) | (Kind::Int(..), Kind::Bool) => self.bool_op(op, a, b),
                _ => fail(format!("{op} on {ka:?} and {kb:?}")),
            };
        }
        // Equality of aggregates.
        if op == "==" || op == "!=" {
            let eq = self.agg_eq(&a, &b)?;
            return if op == "==" { Ok(eq) } else { self.unary("!", eq) };
        }
        fail(format!("{op} on {a:?} and {b:?}"))
    }

    fn agg_eq(&mut self, a: &Av, b: &Av) -> R<Av> {
        match (a, b) {
            (Av::Struct(n, x), Av::Struct(m, y)) if n == m => self.all_eq(x, y),
            (Av::Tuple(x), Av::Tuple(y)) | (Av::Tup(x), Av::Tup(y)) | (Av::Arr(x), Av::Arr(y)) => {
                if x.len() != y.len() {
                    return Ok(Av::Bool(false));
                }
                self.all_eq(x, y)
            }
            (Av::Enum(n, i, x), Av::Enum(m, j, y)) if n == m => {
                if i != j {
                    return Ok(Av::Bool(false));
                }
                self.all_eq(x, y)
            }
            (Av::DEnum(n, d, ps), Av::Enum(m, j, y)) | (Av::Enum(m, j, y), Av::DEnum(n, d, ps)) if n == m => {
                let d = (**d).clone();
                let c = self.binop("==", Av::D(d), Av::Int(*j as i128, IT::U32))?;
                let pe = self.all_eq(&ps[*j as usize], y)?;
                self.logic_and(c, pe)
            }
            (Av::Unit, Av::Unit) => Ok(Av::Bool(true)),
            (Av::Str(x), Av::Str(y)) => Ok(Av::Bool(x == y)),
            (Av::Fn(x), Av::Fn(y)) => Ok(Av::Bool(x == y)),
            _ => fail(format!("== on {a:?} and {b:?}")),
        }
    }

    fn all_eq(&mut self, x: &[Av], y: &[Av]) -> R<Av> {
        let mut c = Av::Bool(true);
        for (p, q) in x.iter().zip(y) {
            let e = self.binop("==", p.clone(), q.clone())?;
            c = self.logic_and(c, e)?;
            if c == Av::Bool(false) {
                break;
            }
        }
        Ok(c)
    }

    fn fold_int(&mut self, op: &str, x: i128, ta: IT, y: i128, tb: IT) -> R<Av> {
        let shift = op == "<<" || op == ">>";
        let t = if shift { ta } else { unify(ta, tb) };
        let r = match op {
            "+" => x.wrapping_add(y),
            "-" => x.wrapping_sub(y),
            "*" => x.wrapping_mul(y),
            "/" => {
                if y == 0 {
                    return fail("static division by zero");
                }
                x.wrapping_div(y)
            }
            "%" => {
                if y == 0 {
                    return fail("static remainder by zero");
                }
                x.wrapping_rem(y)
            }
            "&" => x & y,
            "|" => x | y,
            "^" => x ^ y,
            "<<" => {
                let n = (y as u32) & (t.bits().min(128) - 1);
                if t.bits() >= 128 { x.wrapping_shl(n) } else { t.wrap(x << n) }
            }
            ">>" => {
                let n = (y as u32) & (t.bits().min(128) - 1);
                if t.signed() {
                    x >> n
                } else {
                    // Unsigned: the value is non-negative here.
                    ((x as u128) >> n) as i128
                }
            }
            "==" => return Ok(Av::Bool(x == y)),
            "!=" => return Ok(Av::Bool(x != y)),
            "<" => return Ok(Av::Bool(x < y)),
            ">" => return Ok(Av::Bool(x > y)),
            "<=" => return Ok(Av::Bool(x <= y)),
            ">=" => return Ok(Av::Bool(x >= y)),
            _ => return fail(format!("int {op}")),
        };
        Ok(Av::Int(t.wrap(r), t))
    }

    fn bool_op(&mut self, op: &str, a: Av, b: Av) -> R<Av> {
        let wop = match op {
            "&" | "&&" => I::I32And,
            "|" | "||" => I::I32Or,
            "^" | "!=" => I::I32Xor,
            "==" => I::I32Eq,
            _ => return fail(format!("bool {op}")),
        };
        let fa = self.fact_of(&a);
        let fb = self.fact_of(&b);
        let fact = match op {
            "&" | "&&" if fa.is_some() || fb.is_some() => Some(super::eval::Fact::And(fa, fb)),
            "|" | "||" if fa.is_some() || fb.is_some() => Some(super::eval::Fact::Or(fa, fb)),
            _ => None,
        };
        self.push_as(&a, Kind::Bool)?;
        self.push_as(&b, Kind::Bool)?;
        self.emit(wop);
        let r = self.def_bool();
        if let (Some(f), Av::D(d)) = (fact, &r) {
            self.facts.insert(d.l, f);
        }
        Ok(r)
    }

    fn fact_of(&self, v: &Av) -> Option<Box<super::eval::Fact>> {
        match v {
            Av::D(d) => self.facts.get(&d.l).cloned().map(Box::new),
            _ => None,
        }
    }

    fn float_op(&mut self, op: &str, a: Av, b: Av, k: Kind) -> R<Av> {
        let f64k = k == Kind::F64;
        self.push_as(&a, k)?;
        self.push_as(&b, k)?;
        let (ins, cmp) = match (op, f64k) {
            ("+", true) => (I::F64Add, false),
            ("-", true) => (I::F64Sub, false),
            ("*", true) => (I::F64Mul, false),
            ("/", true) => (I::F64Div, false),
            ("==", true) => (I::F64Eq, true),
            ("!=", true) => (I::F64Ne, true),
            ("<", true) => (I::F64Lt, true),
            (">", true) => (I::F64Gt, true),
            ("<=", true) => (I::F64Le, true),
            (">=", true) => (I::F64Ge, true),
            ("+", false) => (I::F32Add, false),
            ("-", false) => (I::F32Sub, false),
            ("*", false) => (I::F32Mul, false),
            ("/", false) => (I::F32Div, false),
            ("==", false) => (I::F32Eq, true),
            ("!=", false) => (I::F32Ne, true),
            ("<", false) => (I::F32Lt, true),
            (">", false) => (I::F32Gt, true),
            ("<=", false) => (I::F32Le, true),
            (">=", false) => (I::F32Ge, true),
            _ => return fail(format!("float {op}")),
        };
        self.emit(ins);
        if cmp {
            return Ok(self.def_bool());
        }
        Ok(Av::D(self.def(k, 0, 0)))
    }

    fn int_op(&mut self, op: &str, a: Av, b: Av) -> R<Av> {
        let (ta, ra) = int_info(&a);
        let (tb, rb) = int_info(&b);
        let shift = op == "<<" || op == ">>";
        let t = if shift { unify(ta, IT::I128) } else { unify(ta, tb) };
        let t = if t == IT::Lit { IT::I128 } else { t };
        // Static amount shifts and literal operands take the other's type.
        match op {
            "==" | "!=" | "<" | ">" | "<=" | ">=" => return self.int_cmp(op, &a, &b, t, ra, rb),
            _ => {}
        }
        if shift {
            return self.int_shift(op, &a, &b, t, ra, rb);
        }
        let range = match op {
            "+" => add_range(ra, rb),
            "-" => add_range(ra, (rb.1.checked_neg().unwrap_or(i128::MAX), rb.0.checked_neg().unwrap_or(i128::MAX))),
            "*" => mul_range(ra, rb),
            "&" => {
                if ra.0 >= 0 && rb.0 >= 0 {
                    Some((0, ra.1.min(rb.1)))
                } else if ra.0 >= 0 {
                    Some((0, ra.1))
                } else if rb.0 >= 0 {
                    Some((0, rb.1))
                } else {
                    Some(span(bitlen(ra.0).max(bitlen(ra.1)).max(bitlen(rb.0)).max(bitlen(rb.1))))
                }
            }
            "|" | "^" => {
                let bl = bitlen(ra.0).max(bitlen(ra.1)).max(bitlen(rb.0)).max(bitlen(rb.1));
                if ra.0 >= 0 && rb.0 >= 0 {
                    Some((0, if bl >= 127 { i128::MAX } else { (1i128 << bl) - 1 }))
                } else {
                    Some(span(bl))
                }
            }
            "/" | "%" => {
                let m = ra.0.unsigned_abs().max(ra.1.unsigned_abs()).min(i128::MAX as u128) as i128;
                Some((-m, m))
            }
            _ => return fail(format!("int {op}")),
        };
        let (lo, hi) = clamp_type(t, range);
        if t.bits() == 128 && (!fits64((lo, hi)) || !fits64(ra) || !fits64(rb) || is_wide(&a) || is_wide(&b)) {
            if matches!(op, "/" | "%") {
                return fail("128-bit division");
            }
            return self.wide_op(op, &a, &b, t, (lo, hi));
        }
        let rep = self.rep_for(t, (lo, hi), &[ra, rb])?;
        let k = Kind::Int(t, rep);
        let (wide32, ins) = (rep == Rep::I32, op);
        if matches!(op, "/" | "%") {
            // Division by zero is a Rust panic: leave it to the interpreter.
            self.push_as(&b, k)?;
            if wide32 {
                self.emit(I::I32Eqz);
            } else {
                self.emit(I::I64Eqz);
            }
            self.code.push(Node::BrIf(self.trap));
        }
        self.push_as(&a, k)?;
        self.push_as(&b, k)?;
        let signed = t.signed();
        self.emit(match (ins, wide32) {
            ("+", true) => I::I32Add,
            ("-", true) => I::I32Sub,
            ("*", true) => I::I32Mul,
            ("&", true) => I::I32And,
            ("|", true) => I::I32Or,
            ("^", true) => I::I32Xor,
            ("/", true) => {
                if signed {
                    I::I32DivS
                } else {
                    I::I32DivU
                }
            }
            ("%", true) => {
                if signed {
                    I::I32RemS
                } else {
                    I::I32RemU
                }
            }
            ("+", false) => I::I64Add,
            ("-", false) => I::I64Sub,
            ("*", false) => I::I64Mul,
            ("&", false) => I::I64And,
            ("|", false) => I::I64Or,
            ("^", false) => I::I64Xor,
            ("/", false) => {
                if signed {
                    I::I64DivS
                } else {
                    I::I64DivU
                }
            }
            ("%", false) => {
                if signed {
                    I::I64RemS
                } else {
                    I::I64RemU
                }
            }
            _ => return fail(format!("int {op}")),
        });
        self.normalize(t, rep);
        Ok(self.def_int(t, rep, lo, hi))
    }

    /// Keep a narrow type's i32 exact (zero/sign extended).
    fn normalize(&mut self, t: IT, rep: Rep) {
        if rep != Rep::I32 {
            return;
        }
        match t {
            IT::U8 => {
                self.emit(I::I32Const(0xFF));
                self.emit(I::I32And);
            }
            IT::U16 => {
                self.emit(I::I32Const(0xFFFF));
                self.emit(I::I32And);
            }
            IT::I8 => self.emit(I::I32Extend8S),
            IT::I16 => self.emit(I::I32Extend16S),
            _ => {}
        }
    }

    /// The representation of a result of type T with range R, given the
    /// operands' ranges (a 128-bit result held in an i64 needs every
    /// operand to fit too).
    fn rep_for(&self, t: IT, r: (i128, i128), ops: &[(i128, i128)]) -> R<Rep> {
        let rep = t.rep(r);
        if rep == Rep::Wide {
            return fail(format!("128-bit arithmetic (range {r:?})"));
        }
        if t.bits() == 128 {
            for o in ops {
                if o.0 < i64::MIN as i128 || o.1 > i64::MAX as i128 {
                    return fail("128-bit operand");
                }
            }
        }
        Ok(rep)
    }

    fn int_cmp(&mut self, op: &str, a: &Av, b: &Av, t: IT, ra: (i128, i128), rb: (i128, i128)) -> R<Av> {
        // Decide from ranges and value sets where possible.
        let known = |x: &Av| -> Option<Vec<i128>> {
            match x {
                Av::Int(v, _) => Some(vec![*v]),
                Av::D(d) => d.vset.as_ref().map(|s| (**s).clone()),
                _ => None,
            }
        };
        if let (Some(xs), Some(ys)) = (known(a), known(b)) {
            let mut any_t = false;
            let mut any_f = false;
            for x in &xs {
                for y in &ys {
                    if cmp_i(op, *x, *y) {
                        any_t = true;
                    } else {
                        any_f = true;
                    }
                }
            }
            if !any_f {
                return Ok(Av::Bool(true));
            }
            if !any_t {
                return Ok(Av::Bool(false));
            }
        }
        // Disjoint or ordered ranges.
        match op {
            "==" if ra.1 < rb.0 || rb.1 < ra.0 => return Ok(Av::Bool(false)),
            "!=" if ra.1 < rb.0 || rb.1 < ra.0 => return Ok(Av::Bool(true)),
            "<" if ra.1 < rb.0 => return Ok(Av::Bool(true)),
            "<" if ra.0 >= rb.1 => return Ok(Av::Bool(false)),
            "<=" if ra.1 <= rb.0 => return Ok(Av::Bool(true)),
            "<=" if ra.0 > rb.1 => return Ok(Av::Bool(false)),
            ">" if ra.0 > rb.1 => return Ok(Av::Bool(true)),
            ">" if ra.1 <= rb.0 => return Ok(Av::Bool(false)),
            ">=" if ra.0 >= rb.1 => return Ok(Av::Bool(true)),
            ">=" if ra.1 < rb.0 => return Ok(Av::Bool(false)),
            _ => {}
        }
        let lo = ra.0.min(rb.0);
        let hi = ra.1.max(rb.1);
        if t.bits() == 128 && (!fits64(ra) || !fits64(rb) || is_wide(a) || is_wide(b)) {
            return self.wide_cmp(op, a, b, t);
        }
        let rep = if t.bits() <= 32 { Rep::I32 } else { self.rep_for(t, (lo, hi), &[ra, rb])? };
        // 128-bit exact values compare signed; so do signed types.
        let signed = t.signed() || t.bits() == 128;
        let k = Kind::Int(t, rep);
        self.push_as(a, k)?;
        self.push_as(b, k)?;
        let w32 = rep == Rep::I32;
        let fact = match (a, b) {
            (Av::D(d), Av::Int(k, _)) => Some(super::eval::Fact::Cmp(d.l, cmp_op(op), *k)),
            (Av::Int(k, _), Av::D(d)) => Some(super::eval::Fact::Cmp(d.l, flip(op), *k)),
            _ => None,
        };
        self.emit(match (op, w32, signed) {
            ("==", true, _) => I::I32Eq,
            ("!=", true, _) => I::I32Ne,
            ("<", true, true) => I::I32LtS,
            ("<", true, false) => I::I32LtU,
            (">", true, true) => I::I32GtS,
            (">", true, false) => I::I32GtU,
            ("<=", true, true) => I::I32LeS,
            ("<=", true, false) => I::I32LeU,
            (">=", true, true) => I::I32GeS,
            (">=", true, false) => I::I32GeU,
            ("==", false, _) => I::I64Eq,
            ("!=", false, _) => I::I64Ne,
            ("<", false, true) => I::I64LtS,
            ("<", false, false) => I::I64LtU,
            (">", false, true) => I::I64GtS,
            (">", false, false) => I::I64GtU,
            ("<=", false, true) => I::I64LeS,
            ("<=", false, false) => I::I64LeU,
            (">=", false, true) => I::I64GeS,
            (">=", false, false) => I::I64GeU,
            _ => return fail(format!("compare {op}")),
        });
        let r = self.def_bool();
        if let (Some(f), Av::D(d)) = (fact, &r) {
            self.facts.insert(d.l, f);
        }
        Ok(r)
    }

    fn int_shift(&mut self, op: &str, a: &Av, b: &Av, t: IT, ra: (i128, i128), rb: (i128, i128)) -> R<Av> {
        let bits = t.bits().min(128);
        // Rust (release) masks the amount to the width.
        let (nlo, nhi) = if rb.0 >= 0 && rb.1 < bits as i128 { rb } else { (0, bits as i128 - 1) };
        let range = if op == "<<" {
            let m = ra.0.unsigned_abs().max(ra.1.unsigned_abs());
            if nhi >= 127 || (m != 0 && (128 - m.leading_zeros()) as i128 + nhi >= 127) {
                (t.min(), t.max())
            } else {
                let x = (ra.0 << nlo).min(ra.0 << nhi);
                let y = (ra.1 << nlo).max(ra.1 << nhi);
                (x.min(ra.0 << nhi), y)
            }
        } else if ra.0 >= 0 {
            (ra.0 >> nhi, ra.1 >> nlo)
        } else {
            (ra.0 >> nlo, ra.1.max(0) >> nlo)
        };
        let range = clamp_type(t, Some(range));
        if t.bits() == 128 && (!fits64(range) || !fits64(ra) || is_wide(a)) {
            return self.wide_shift(op, a, b, t, range);
        }
        let rep = self.rep_for(t, range, &[ra])?;
        let k = Kind::Int(t, rep);
        let w32 = rep == Rep::I32;
        self.push_as(a, k)?;
        // The amount, masked like Rust; a 128-bit value in an i64 shifts by
        // at most 63 (its range fits, so a larger left shift cannot occur and
        // a larger right shift gives the sign).
        let amt_kind = if w32 { Kind::Int(IT::U32, Rep::I32) } else { Kind::Int(IT::U64, Rep::I64) };
        match b {
            Av::Int(n, _) => {
                let n = (*n as u32) & (bits - 1);
                let n = if t.bits() == 128 { n.min(63) } else { n };
                if w32 {
                    self.emit(I::I32Const(n as i32));
                } else {
                    self.emit(I::I64Const(n as i64));
                }
            }
            _ => {
                let bv = self.cast_int_to(b, IT::U32)?;
                self.push_as(&bv, Kind::Int(IT::U32, Rep::I32))?;
                if t.bits() == 128 {
                    // min(n & 127, 63)
                    self.emit(I::I32Const(127));
                    self.emit(I::I32And);
                    let l = self.local(super::ir::W::I32);
                    self.emit(I::LocalTee(l));
                    self.emit(I::I32Const(63));
                    self.emit(I::LocalGet(l));
                    self.emit(I::I32Const(63));
                    self.emit(I::I32LtU);
                    self.emit(I::Select);
                }
                if !w32 {
                    self.emit(I::I64ExtendI32U);
                }
                let _ = amt_kind;
            }
        }
        let signed = t.signed() || t.bits() == 128;
        self.emit(match (op, w32, signed) {
            ("<<", true, _) => I::I32Shl,
            (">>", true, true) => I::I32ShrS,
            (">>", true, false) => I::I32ShrU,
            ("<<", false, _) => I::I64Shl,
            (">>", false, true) => I::I64ShrS,
            (">>", false, false) => I::I64ShrU,
            _ => unreachable!(),
        });
        self.normalize(t, rep);
        Ok(self.def_int(t, rep, range.0, range.1))
    }

    // -------------------------------------------------------------- unary

    pub fn unary(&mut self, op: &str, v: Av) -> R<Av> {
        match (&v, op) {
            (Av::Bool(b), "!") => return Ok(Av::Bool(!b)),
            (Av::Int(x, t), "!") => return Ok(Av::Int(t.wrap(!x), *t)),
            (Av::Int(x, t), "-") => return Ok(Av::Int(t.wrap(x.wrapping_neg()), *t)),
            (Av::F64(x), "-") => return Ok(Av::F64(-x)),
            (Av::F32(x), "-") => return Ok(Av::F32(-x)),
            _ => {}
        }
        let Av::D(d) = &v else {
            return fail(format!("unary {op} on {v:?}"));
        };
        let d = d.clone();
        if let Kind::Int(t, Rep::Wide) = d.k {
            return match op {
                "!" => {
                    let r = (d.hi.checked_neg().map(|x| x - 1).unwrap_or(i128::MIN), d.lo.checked_neg().map(|x| x - 1).unwrap_or(i128::MAX));
                    self.wide_op("^", &v, &Av::Int(-1, t), t, (r.0.min(r.1), r.0.max(r.1)))
                }
                _ => {
                    let r = (d.hi.checked_neg().unwrap_or(i128::MIN), d.lo.checked_neg().unwrap_or(i128::MAX));
                    self.wide_op("-", &Av::Int(0, t), &v, t, r)
                }
            };
        }
        match (d.k, op) {
            (Kind::Bool, "!") => {
                let f = self.facts.get(&d.l).cloned();
                self.get_dv(&d);
                self.emit(I::I32Eqz);
                let r = self.def_bool();
                if let (Some(f), Av::D(n)) = (f, &r) {
                    self.facts.insert(n.l, super::eval::Fact::Not(Box::new(f)));
                }
                Ok(r)
            }
            (Kind::Int(t, rep), "!") => {
                let (lo, hi) = if t.signed() || t.bits() == 128 { (-d.hi - 1, -d.lo - 1) } else { (t.min(), t.max()) };
                let (lo, hi) = clamp_type(t, Some((lo, hi)));
                let rep2 = self.rep_for(t, (lo, hi), &[(d.lo, d.hi)])?;
                if rep2 != rep {
                    return fail("! changes representation");
                }
                self.get_dv(&d);
                if rep == Rep::I32 {
                    self.emit(I::I32Const(-1));
                    self.emit(I::I32Xor);
                } else {
                    self.emit(I::I64Const(-1));
                    self.emit(I::I64Xor);
                }
                self.normalize(t, rep);
                Ok(self.def_int(t, rep, lo, hi))
            }
            (Kind::Int(t, _), "-") => {
                let z = Av::Int(0, t);
                self.binop("-", z, Av::D(d))
            }
            (Kind::F64, "-") => {
                self.get_dv(&d);
                self.emit(I::F64Neg);
                Ok(Av::D(self.def(Kind::F64, 0, 0)))
            }
            (Kind::F32, "-") => {
                self.get_dv(&d);
                self.emit(I::F32Neg);
                Ok(Av::D(self.def(Kind::F32, 0, 0)))
            }
            _ => fail(format!("unary {op} on {:?}", d.k)),
        }
    }

    // --------------------------------------------------------------- casts

    pub fn cast(&mut self, v: Av, to: &RTy) -> R<Av> {
        match to {
            RTy::Int(it) => match &v {
                Av::F64(x) => Ok(Av::Int(f64_to_int(*x, *it), *it)),
                Av::F32(x) => Ok(Av::Int(f64_to_int(*x as f64, *it), *it)),
                Av::D(d) if matches!(d.k, Kind::F64 | Kind::F32) => self.float_to_int(d.clone(), *it),
                _ => self.cast_int_to(&v, *it),
            },
            RTy::F64 | RTy::F32 => {
                let k = if *to == RTy::F64 { Kind::F64 } else { Kind::F32 };
                match &v {
                    Av::Int(x, t) => {
                        let f = if t.signed() || t.bits() == 128 { *x as f64 } else { (*x as u128) as f64 };
                        Ok(if k == Kind::F64 { Av::F64(f) } else { Av::F32(*x as f32) })
                    }
                    Av::F64(x) => Ok(if k == Kind::F64 { v.clone() } else { Av::F32(*x as f32) }),
                    Av::F32(x) => Ok(if k == Kind::F64 { Av::F64(*x as f64) } else { v.clone() }),
                    Av::D(d) => {
                        let d = d.clone();
                        self.get_dv(&d);
                        match (d.k, k) {
                            (Kind::F64, Kind::F64) | (Kind::F32, Kind::F32) => {}
                            (Kind::F64, Kind::F32) => self.emit(I::F32DemoteF64),
                            (Kind::F32, Kind::F64) => self.emit(I::F64PromoteF32),
                            (Kind::Int(t, Rep::I32), _) => self.emit(match (k, t.signed()) {
                                (Kind::F64, true) => I::F64ConvertI32S,
                                (Kind::F64, false) => I::F64ConvertI32U,
                                (_, true) => I::F32ConvertI32S,
                                _ => I::F32ConvertI32U,
                            }),
                            (Kind::Int(t, Rep::I64), _) => self.emit(match (k, t.signed() || t.bits() == 128) {
                                (Kind::F64, true) => I::F64ConvertI64S,
                                (Kind::F64, false) => I::F64ConvertI64U,
                                (_, true) => I::F32ConvertI64S,
                                _ => I::F32ConvertI64U,
                            }),
                            _ => return fail("float cast"),
                        }
                        Ok(Av::D(self.def(k, 0, 0)))
                    }
                    _ => fail(format!("cast {v:?} to float")),
                }
            }
            RTy::Bool => Ok(v),
            _ => fail(format!("cast to {to:?}")),
        }
    }

    pub fn cast_int_to(&mut self, v: &Av, it: IT) -> R<Av> {
        match v {
            Av::Int(x, _) => Ok(Av::Int(it.wrap(*x), it)),
            Av::Bool(b) => Ok(Av::Int(*b as i128, it)),
            Av::D(d) => {
                let d = d.clone();
                let (from, frep) = match d.k {
                    Kind::Int(t, r) => (t, r),
                    Kind::Bool => (IT::U8, Rep::I32),
                    _ => return fail("int cast of a float"),
                };
                if from == it {
                    return Ok(Av::D(d));
                }
                // The value's range in the target type.
                let fits = d.lo >= it.min() && d.hi <= it.max();
                let (lo, hi) = if fits { (d.lo, d.hi) } else { (it.min(), it.max()) };
                if frep == Rep::Wide || (it.bits() == 128 && !fits64((lo, hi))) {
                    return self.cast_wide(&d, it, (lo, hi), fits);
                }
                let rep = self.rep_for(it, (lo, hi), &[])?;
                let k = Kind::Int(it, rep);
                // Same bits and same representation: relabel.
                self.get_dv(&d);
                match (frep, rep) {
                    (Rep::I32, Rep::I32) => {
                        if !fits || it.bits() < 32 {
                            self.normalize(it, rep);
                        }
                    }
                    (Rep::I32, Rep::I64) => {
                        if from.signed() {
                            self.emit(I::I64ExtendI32S);
                        } else {
                            self.emit(I::I64ExtendI32U);
                        }
                        // A negative value into an unsigned 64-bit type keeps
                        // its bits (two's complement), which the i64 holds.
                    }
                    (Rep::I64, Rep::I32) => {
                        self.emit(I::I32WrapI64);
                        self.normalize(it, rep);
                    }
                    (Rep::I64, Rep::I64) => {}
                    _ => return fail("128-bit cast"),
                }
                let mut nd = self.def(k, lo, hi);
                if fits {
                    nd.vset = d.vset.clone();
                }
                Ok(Av::D(nd))
            }
            _ => fail(format!("int cast of {v:?}")),
        }
    }

    /// An integer cast where the source or the result is a 128-bit pair.
    fn cast_wide(&mut self, d: &Dv, it: IT, r: (i128, i128), fits: bool) -> R<Av> {
        let (lo, hi) = self.wparts(&Av::D(d.clone()))?;
        if it.bits() == 128 {
            self.wpush(lo);
            let l = self.local(W::I64);
            self.emit(I::LocalSet(l));
            self.wpush(hi);
            let h = self.local(W::I64);
            self.emit(I::LocalSet(h));
            let mut v = self.wresult(it, l, h, r);
            if let (Av::D(n), true) = (&mut v, fits) {
                n.vset = d.vset.clone();
            }
            return Ok(v);
        }
        // Narrower: the low bits.
        self.wpush(lo);
        let rep = if it.bits() <= 32 { Rep::I32 } else { Rep::I64 };
        if rep == Rep::I32 {
            self.emit(I::I32WrapI64);
            self.normalize(it, rep);
        }
        Ok(self.def_int(it, rep, r.0, r.1))
    }

    fn float_to_int(&mut self, d: Dv, it: IT) -> R<Av> {
        // Rust `as` saturates (NaN -> 0); a 128-bit target saturates at the
        // i64 bounds (callers range-check against int32, SUBSET.md).
        self.get_dv(&d);
        let (rep, lo, hi) = match it.bits() {
            8 | 16 | 32 => (Rep::I32, it.min(), it.max()),
            64 => (Rep::I64, it.min(), it.max()),
            _ => (Rep::I64, i64::MIN as i128, i64::MAX as i128),
        };
        let f64k = d.k == Kind::F64;
        match (rep, it.signed() || it.bits() == 128, f64k) {
            (Rep::I32, true, true) => self.emit(I::I32TruncSatF64S),
            (Rep::I32, false, true) => self.emit(I::I32TruncSatF64U),
            (Rep::I32, true, false) => self.emit(I::I32TruncSatF32S),
            (Rep::I32, false, false) => self.emit(I::I32TruncSatF32U),
            (_, true, true) => self.emit(I::I64TruncSatF64S),
            (_, false, true) => self.emit(I::I64TruncSatF64U),
            (_, true, false) => self.emit(I::I64TruncSatF32S),
            (_, false, false) => self.emit(I::I64TruncSatF32U),
        }
        if it.bits() < 32 {
            return fail("narrow float cast");
        }
        Ok(self.def_int(it, rep, lo, hi))
    }

    /// `if c { a } else { b }` for scalar values, as a select.
    pub fn select(&mut self, c: &Av, a: Av, b: Av) -> R<Av> {
        match c {
            Av::Bool(true) => return Ok(a),
            Av::Bool(false) => return Ok(b),
            _ => {}
        }
        if a == b {
            return Ok(a);
        }
        if is_wide(&a) || is_wide(&b) {
            let (t, ra) = int_info(&a);
            let (_, rb) = int_info(&b);
            let (alo, ahi) = self.wparts(&a)?;
            let (blo, bhi) = self.wparts(&b)?;
            self.wpush(alo);
            self.wpush(blo);
            self.push_as(c, Kind::Bool)?;
            self.emit(I::Select);
            let l = self.local(W::I64);
            self.emit(I::LocalSet(l));
            self.wpush(ahi);
            self.wpush(bhi);
            self.push_as(c, Kind::Bool)?;
            self.emit(I::Select);
            let h = self.local(W::I64);
            self.emit(I::LocalSet(h));
            return Ok(self.wresult(t, l, h, (ra.0.min(rb.0), ra.1.max(rb.1))));
        }
        let k = match (kind_of(&a), kind_of(&b)) {
            (Some(Kind::Int(t, _)), Some(Kind::Int(u, _))) => {
                let t = unify(t, u);
                let t = if t == IT::Lit { IT::I128 } else { t };
                let (ra, rb) = (int_info(&a).1, int_info(&b).1);
                let r = (ra.0.min(rb.0), ra.1.max(rb.1));
                Kind::Int(t, self.rep_for(t, r, &[])?)
            }
            (Some(x), Some(y)) if x == y => x,
            (Some(Kind::Bool), Some(Kind::Bool)) => Kind::Bool,
            _ => return fail(format!("select of {a:?} and {b:?}")),
        };
        self.push_as(&a, k)?;
        self.push_as(&b, k)?;
        self.push_as(c, Kind::Bool)?;
        self.emit(I::Select);
        let (lo, hi) = match k {
            Kind::Int(..) => {
                let (ra, rb) = (int_info(&a).1, int_info(&b).1);
                (ra.0.min(rb.0), ra.1.max(rb.1))
            }
            _ => (0, 1),
        };
        let mut d = self.def(k, lo, hi);
        let vs = |x: &Av| -> Option<Vec<i128>> {
            match x {
                Av::Int(v, _) => Some(vec![*v]),
                Av::D(d) => d.vset.as_ref().map(|s| (**s).clone()),
                _ => None,
            }
        };
        if let (Some(mut x), Some(y)) = (vs(&a), vs(&b)) {
            for v in y {
                if !x.contains(&v) {
                    x.push(v);
                }
            }
            x.sort();
            d.vset = Some(Rc::new(x));
        }
        Ok(Av::D(d))
    }

    // ------------------------------------------------------ primitive methods

    pub fn prim_method(&mut self, rv: Av, name: &str, args: Vec<Av>, hint: Option<&RTy>) -> R<Flow> {
        let _ = hint;
        // Option / Result.
        if let Av::Enum(n, k, p) = &rv
            && (&**n == "Option" || &**n == "Result")
        {
            let is_some = *k == 1 && &**n == "Option" || *k == 0 && &**n == "Result";
            return Ok(Flow::V(match name {
                "is_some" | "is_ok" => Av::Bool(is_some),
                "is_none" | "is_err" => Av::Bool(!is_some),
                "unwrap" | "expect" => {
                    if !is_some {
                        // A panic in Rust: unreachable in a correct run.
                        self.code.push(Node::Br(self.trap));
                        return Ok(Flow::Div);
                    }
                    p[0].clone()
                }
                "unwrap_or" => {
                    if is_some {
                        p[0].clone()
                    } else {
                        args[0].clone()
                    }
                }
                "ok_or" => {
                    if is_some {
                        ok(p[0].clone())
                    } else {
                        Av::Enum("Result".into(), 1, Rc::new(vec![args[0].clone()]))
                    }
                }
                "copied" | "cloned" => rv.clone(),
                _ => return fail(format!("Option::{name}")),
            }));
        }
        if let Av::DEnum(n, d, p) = &rv
            && (&**n == "Option" || &**n == "Result")
        {
            let d = (**d).clone();
            let some_k = if &**n == "Option" { 1 } else { 0 };
            return Ok(Flow::V(match name {
                "is_some" | "is_ok" => self.binop("==", Av::D(d), Av::Int(some_k, IT::U32))?,
                "is_none" | "is_err" => self.binop("!=", Av::D(d), Av::Int(some_k, IT::U32))?,
                "unwrap" | "expect" => {
                    // None traps (a Rust panic).
                    let c = self.binop("!=", Av::D(d), Av::Int(some_k, IT::U32))?;
                    self.push_as(&c, Kind::Bool)?;
                    self.code.push(Node::BrIf(self.trap));
                    p[some_k as usize][0].clone()
                }
                "unwrap_or" => {
                    let c = self.binop("==", Av::D(d), Av::Int(some_k, IT::U32))?;
                    let x = p[some_k as usize][0].clone();
                    return self
                        .fork(&c, move |_| Ok(Flow::V(x)), {
                            let y = args[0].clone();
                            move |_| Ok(Flow::V(y))
                        })
                        .map(|f| f);
                }
                "ok_or" => {
                    let c = self.binop("==", Av::D(d), Av::Int(some_k, IT::U32))?;
                    let x = ok(p[some_k as usize][0].clone());
                    let e = Av::Enum("Result".into(), 1, Rc::new(vec![args[0].clone()]));
                    return self.fork(&c, move |_| Ok(Flow::V(x)), move |_| Ok(Flow::V(e)));
                }
                "copied" | "cloned" => rv.clone(),
                _ => return fail(format!("Option::{name} (run-time)")),
            }));
        }
        // Tup (static length).
        if let Av::Tup(items) = &rv {
            return Ok(Flow::V(match name {
                "len" => Av::Int(items.len() as i128, IT::Usize),
                "is_empty" => Av::Bool(items.is_empty()),
                "get" => {
                    let k = args[0].as_int().ok_or_else(|| Fail("Tup::get index".into()))?;
                    items.get(k as usize).cloned().ok_or_else(|| Fail("Tup::get range".into()))?
                }
                "at" => {
                    let k = args[0].as_int().ok_or_else(|| Fail("Tup::at index".into()))?;
                    let n = items.len() as i128;
                    let j = if k < 0 { k + n } else { k };
                    if j < 0 || j >= n {
                        self.code.push(Node::Br(self.trap));
                        return Ok(Flow::Div);
                    }
                    ok(items[j as usize].clone())
                }
                "contains" => {
                    let mut c = Av::Bool(false);
                    for x in items.iter() {
                        let e = self.binop("==", x.clone(), args[0].clone())?;
                        c = self.logic_or(c, e)?;
                    }
                    c
                }
                "slice" => {
                    let (Some(lo), Some(hi)) = (args[0].as_int(), args[1].as_int()) else {
                        return fail("Tup::slice bounds");
                    };
                    let n = items.len() as i128;
                    let norm = |x: i128| -> usize { (if x < 0 { x + n } else { x }).clamp(0, n) as usize };
                    let (a, b) = (norm(lo), norm(hi));
                    Av::Tup(Rc::new(if b <= a { vec![] } else { items[a..b].to_vec() }))
                }
                _ => return fail(format!("Tup::{name}")),
            }));
        }
        if let Av::Arr(items) = &rv {
            return Ok(Flow::V(match name {
                "len" => Av::Int(items.len() as i128, IT::Usize),
                "is_empty" => Av::Bool(items.is_empty()),
                "contains" => {
                    let mut c = Av::Bool(false);
                    for x in items.iter() {
                        let e = self.binop("==", x.clone(), args[0].clone())?;
                        c = self.logic_or(c, e)?;
                    }
                    c
                }
                _ => return fail(format!("slice::{name}")),
            }));
        }
        if let Av::Range(a, b) = &rv {
            return Ok(Flow::V(match name {
                "contains" => {
                    let x = args[0].clone();
                    let c1 = self.binop(">=", x.clone(), Av::Int(*a, IT::Lit))?;
                    let c2 = self.binop("<", x, Av::Int(*b, IT::Lit))?;
                    self.logic_and(c1, c2)?
                }
                _ => return fail(format!("Range::{name}")),
            }));
        }
        self.num_method(rv, name, args).map(Flow::V)
    }

    fn num_method(&mut self, v: Av, name: &str, args: Vec<Av>) -> R<Av> {
        // Static.
        match (&v, name) {
            (Av::F64(x), _) => {
                return Ok(match name {
                    "is_nan" => Av::Bool(x.is_nan()),
                    "is_infinite" => Av::Bool(x.is_infinite()),
                    "is_finite" => Av::Bool(x.is_finite()),
                    "to_bits" => Av::Int(x.to_bits() as i128, IT::U64),
                    "abs" => Av::F64(x.abs()),
                    "trunc" => Av::F64(x.trunc()),
                    "round_ties_even" => Av::F64(x.round_ties_even()),
                    "sqrt" => Av::F64(canon64(x.sqrt())),
                    "copysign" => match args[0] {
                        Av::F64(y) => Av::F64(x.copysign(y)),
                        _ => return self.num_method_dyn(v, name, args),
                    },
                    _ => return fail(format!("f64::{name}")),
                });
            }
            (Av::F32(x), _) => {
                return Ok(match name {
                    "is_nan" => Av::Bool(x.is_nan()),
                    "is_infinite" => Av::Bool(x.is_infinite()),
                    "to_bits" => Av::Int(x.to_bits() as i128, IT::U32),
                    "abs" => Av::F32(x.abs()),
                    _ => return fail(format!("f32::{name}")),
                });
            }
            (Av::Int(x, t), _) if args.iter().all(|a| a.as_int().is_some()) => {
                let a0 = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let a1 = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
                return Ok(match name {
                    "wrapping_shl" => {
                        let b = t.bits().min(128);
                        Av::Int(t.wrap(x.wrapping_shl((a0 as u32) & (b - 1))), *t)
                    }
                    "wrapping_pow" => Av::Int(t.wrap(x.wrapping_pow(a0 as u32)), *t),
                    "leading_zeros" => {
                        let b = t.bits();
                        let u = (*x as u128) & if b >= 128 { u128::MAX } else { (1u128 << b) - 1 };
                        Av::Int((u.leading_zeros() - (128 - b)) as i128, IT::U32)
                    }
                    "unsigned_abs" => Av::Int(x.unsigned_abs() as i128, IT::U128),
                    "abs" => Av::Int(t.wrap(x.wrapping_abs()), *t),
                    "min" => Av::Int((*x).min(a0), *t),
                    "max" => Av::Int((*x).max(a0), *t),
                    "clamp" => Av::Int((*x).clamp(a0, a1), *t),
                    "count_ones" => Av::Int(((*x as u128) & mask_bits(t.bits())).count_ones() as i128, IT::U32),
                    "signum" => Av::Int(x.signum(), *t),
                    _ => return fail(format!("int::{name}")),
                });
            }
            _ => {}
        }
        self.num_method_dyn(v, name, args)
    }

    fn num_method_dyn(&mut self, v: Av, name: &str, args: Vec<Av>) -> R<Av> {
        let k = kind_of(&v).ok_or_else(|| Fail(format!("method {name} on {v:?}")))?;
        match (k, name) {
            (Kind::F64, "is_nan") => {
                self.push_as(&v, k)?;
                self.push_as(&v, k)?;
                self.emit(I::F64Ne);
                Ok(self.def_bool())
            }
            (Kind::F32, "is_nan") => {
                self.push_as(&v, k)?;
                self.push_as(&v, k)?;
                self.emit(I::F32Ne);
                Ok(self.def_bool())
            }
            (Kind::F64, "is_infinite") => {
                self.push_as(&v, k)?;
                self.emit(I::F64Abs);
                self.emit(I::F64Const(f64::INFINITY.into()));
                self.emit(I::F64Eq);
                Ok(self.def_bool())
            }
            (Kind::F32, "is_infinite") => {
                self.push_as(&v, k)?;
                self.emit(I::F32Abs);
                self.emit(I::F32Const(f32::INFINITY.into()));
                self.emit(I::F32Eq);
                Ok(self.def_bool())
            }
            (Kind::F64, "is_finite") => {
                self.push_as(&v, k)?;
                self.emit(I::F64Abs);
                self.emit(I::F64Const(f64::INFINITY.into()));
                self.emit(I::F64Lt);
                Ok(self.def_bool())
            }
            (Kind::F64, "to_bits") => {
                self.push_as(&v, k)?;
                self.emit(I::I64ReinterpretF64);
                Ok(self.def_int(IT::U64, Rep::I64, 0, u64::MAX as i128))
            }
            (Kind::F32, "to_bits") => {
                self.push_as(&v, k)?;
                self.emit(I::I32ReinterpretF32);
                Ok(self.def_int(IT::U32, Rep::I32, 0, u32::MAX as i128))
            }
            (Kind::F64, "abs" | "trunc" | "round_ties_even" | "sqrt") => {
                self.push_as(&v, k)?;
                self.emit(match name {
                    "abs" => I::F64Abs,
                    "trunc" => I::F64Trunc,
                    "round_ties_even" => I::F64Nearest,
                    _ => I::F64Sqrt,
                });
                Ok(Av::D(self.def(k, 0, 0)))
            }
            (Kind::F32, "abs") => {
                self.push_as(&v, k)?;
                self.emit(I::F32Abs);
                Ok(Av::D(self.def(k, 0, 0)))
            }
            (Kind::F64, "copysign") => {
                self.push_as(&v, k)?;
                self.push_as(&args[0], k)?;
                self.emit(I::F64Copysign);
                Ok(Av::D(self.def(k, 0, 0)))
            }
            (Kind::Int(t, _), "min" | "max") => {
                let other = args[0].clone();
                let c = self.binop(if name == "min" { "<" } else { ">" }, other.clone(), v.clone())?;
                let r = self.select(&c, other, v)?;
                Ok(match r {
                    Av::D(mut d) => {
                        d.k = match d.k {
                            Kind::Int(_, rep) => Kind::Int(t, rep),
                            k => k,
                        };
                        Av::D(d)
                    }
                    x => x,
                })
            }
            (Kind::Int(t, rep), "leading_zeros") => {
                let (_, r) = int_info(&v);
                self.push_as(&v, Kind::Int(t, rep))?;
                match (rep, t.bits()) {
                    (Rep::I32, b) => {
                        self.emit(I::I32Clz);
                        if b < 32 {
                            self.emit(I::I32Const(32 - b as i32));
                            self.emit(I::I32Sub);
                        }
                    }
                    (Rep::I64, 64) => {
                        self.emit(I::I64Clz);
                        self.emit(I::I32WrapI64);
                    }
                    (Rep::I64, _) if r.0 >= 0 => {
                        // A non-negative 128-bit value held in an i64.
                        self.emit(I::I64Clz);
                        self.emit(I::I32WrapI64);
                        self.emit(I::I32Const(64));
                        self.emit(I::I32Add);
                    }
                    _ => return fail("leading_zeros of a possibly negative 128-bit value"),
                }
                Ok(self.def_int(IT::U32, Rep::I32, 0, t.bits() as i128))
            }
            (Kind::Int(_, _), "clamp") => {
                let (lo, hi) = (args[0].clone(), args[1].clone());
                let c1 = self.binop("<", v.clone(), lo.clone())?;
                let x = self.select(&c1, lo, v)?;
                let c2 = self.binop(">", x.clone(), hi.clone())?;
                self.select(&c2, hi, x)
            }
            (Kind::Int(t, _), "wrapping_shl") => {
                let b = t.bits().min(128) as i128;
                let n = self.binop("&", args[0].clone(), Av::Int(b - 1, IT::U32))?;
                self.binop("<<", v, n)
            }
            (Kind::Int(t, _), "abs") => {
                let z = Av::Int(0, t);
                let neg = self.unary("-", v.clone())?;
                let c = self.binop("<", v.clone(), z)?;
                self.select(&c, neg, v)
            }
            (Kind::Int(_, _), "unsigned_abs") => {
                let (_, r) = int_info(&v);
                if r.0 >= 0 {
                    return self.cast_int_to(&v, IT::U128);
                }
                fail("unsigned_abs of a signed run-time value")
            }
            _ => fail(format!("{name} on run-time {k:?}")),
        }
    }

    /// Tup::push / Tup::set on a local: the new Tup and the call's result.
    pub fn tup_mut(&mut self, rv: Av, name: &str, args: Vec<Av>) -> R<(Av, Flow)> {
        let Av::Tup(items) = rv else { unreachable!() };
        let mut items = (*items).clone();
        match name {
            "push" => {
                if items.len() >= 8 {
                    self.code.push(Node::Br(self.trap));
                    return Ok((Av::Tup(Rc::new(items)), Flow::Div));
                }
                items.push(args[0].clone());
            }
            _ => {
                let k = args[0].as_int().ok_or_else(|| Fail("Tup::set index".into()))?;
                let n = items.len() as i128;
                let j = if k < 0 { k + n } else { k };
                if j < 0 || j >= n {
                    self.code.push(Node::Br(self.trap));
                    return Ok((Av::Tup(Rc::new(items)), Flow::Div));
                }
                items[j as usize] = args[1].clone();
            }
        }
        Ok((Av::Tup(Rc::new(items)), Flow::V(ok(Av::Unit))))
    }
}

fn mask_bits(b: u32) -> u128 {
    if b >= 128 { u128::MAX } else { (1u128 << b) - 1 }
}

fn f64_to_int(x: f64, it: IT) -> i128 {
    if x.is_nan() {
        return 0;
    }
    match it.bits() {
        8 | 16 | 32 | 64 => {
            let t = x.trunc();
            if t <= it.min() as f64 {
                it.min()
            } else if t >= it.max() as f64 {
                it.max()
            } else {
                t as i128
            }
        }
        // 128-bit targets saturate at the i64 bounds, like the emitted code.
        _ => (x as i64) as i128,
    }
}

fn cmp_op(op: &str) -> &'static str {
    match op {
        "==" => "==",
        "!=" => "!=",
        "<" => "<",
        ">" => ">",
        "<=" => "<=",
        _ => ">=",
    }
}

/// K OP x  ==  x FLIP(OP) K
fn flip(op: &str) -> &'static str {
    match op {
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        "==" => "==",
        _ => "!=",
    }
}

fn cmp_i(op: &str, x: i128, y: i128) -> bool {
    match op {
        "==" => x == y,
        "!=" => x != y,
        "<" => x < y,
        ">" => x > y,
        "<=" => x <= y,
        _ => x >= y,
    }
}

fn fold_f64(op: &str, x: f64, y: f64) -> R<Av> {
    Ok(match op {
        "+" => Av::F64(canon64(x + y)),
        "-" => Av::F64(canon64(x - y)),
        "*" => Av::F64(canon64(x * y)),
        "/" => Av::F64(canon64(x / y)),
        "==" => Av::Bool(x == y),
        "!=" => Av::Bool(x != y),
        "<" => Av::Bool(x < y),
        ">" => Av::Bool(x > y),
        "<=" => Av::Bool(x <= y),
        ">=" => Av::Bool(x >= y),
        _ => return fail(format!("f64 {op}")),
    })
}

fn fold_f32(op: &str, x: f32, y: f32) -> R<Av> {
    Ok(match op {
        "+" => Av::F32(canon32(x + y)),
        "-" => Av::F32(canon32(x - y)),
        "*" => Av::F32(canon32(x * y)),
        "/" => Av::F32(canon32(x / y)),
        "==" => Av::Bool(x == y),
        "!=" => Av::Bool(x != y),
        "<" => Av::Bool(x < y),
        ">" => Av::Bool(x > y),
        "<=" => Av::Bool(x <= y),
        ">=" => Av::Bool(x >= y),
        _ => return fail(format!("f32 {op}")),
    })
}

pub fn is_wide(v: &Av) -> bool {
    matches!(v, Av::D(d) if matches!(d.k, Kind::Int(_, Rep::Wide)))
}

pub fn kind_of(v: &Av) -> Option<Kind> {
    match v {
        Av::Int(_, t) => Some(Kind::Int(*t, Rep::I64)),
        Av::Bool(_) => Some(Kind::Bool),
        Av::F64(_) => Some(Kind::F64),
        Av::F32(_) => Some(Kind::F32),
        Av::D(d) => Some(d.k),
        _ => None,
    }
}

/// An integer operand's type and range.
pub fn int_info(v: &Av) -> (IT, (i128, i128)) {
    match v {
        Av::Int(x, t) => (*t, (*x, *x)),
        Av::Bool(b) => (IT::U8, (*b as i128, *b as i128)),
        Av::D(d) => match d.k {
            Kind::Int(t, _) => (t, (d.lo, d.hi)),
            _ => (IT::U8, (0, 1)),
        },
        _ => (IT::I128, (i128::MIN, i128::MAX)),
    }
}

fn add_range(a: (i128, i128), b: (i128, i128)) -> Option<(i128, i128)> {
    Some((a.0.checked_add(b.0)?, a.1.checked_add(b.1)?))
}

fn mul_range(a: (i128, i128), b: (i128, i128)) -> Option<(i128, i128)> {
    let c = [a.0.checked_mul(b.0)?, a.0.checked_mul(b.1)?, a.1.checked_mul(b.0)?, a.1.checked_mul(b.1)?];
    Some((*c.iter().min().unwrap(), *c.iter().max().unwrap()))
}

/// The range of a result of type T: R when it fits the type (no wrap),
/// else the type's whole range.
fn clamp_type(t: IT, r: Option<(i128, i128)>) -> (i128, i128) {
    match r {
        Some((lo, hi)) if lo >= t.min() && hi <= t.max() => (lo, hi),
        _ => (t.min(), t.max()),
    }
}
