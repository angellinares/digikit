//! Abstract values of the partial evaluator: known at translation time
//! (static), known only at run time (a WebAssembly local), or aggregates
//! mixing both (partially static).

use super::ir::W;
use std::rc::Rc;

/// A Rust integer type. `Lit` is an unsuffixed literal not yet unified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IT {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    Isize,
    Lit,
}

impl IT {
    pub fn from_name(s: &str) -> Option<IT> {
        Some(match s {
            "i8" => IT::I8,
            "i16" => IT::I16,
            "i32" => IT::I32,
            "i64" => IT::I64,
            "i128" => IT::I128,
            "u8" => IT::U8,
            "u16" => IT::U16,
            "u32" => IT::U32,
            "u64" => IT::U64,
            "u128" => IT::U128,
            // The reference runtime runs natively on a 64-bit host.
            "usize" => IT::Usize,
            "isize" => IT::Isize,
            _ => return None,
        })
    }
    pub fn bits(self) -> u32 {
        match self {
            IT::I8 | IT::U8 => 8,
            IT::I16 | IT::U16 => 16,
            IT::I32 | IT::U32 => 32,
            IT::I64 | IT::U64 | IT::Usize | IT::Isize => 64,
            IT::I128 | IT::U128 | IT::Lit => 128,
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IT::I8 | IT::I16 | IT::I32 | IT::I64 | IT::I128 | IT::Isize | IT::Lit)
    }
    /// The value V converted (wrapped) to this type, as an i128. A U128
    /// above i128::MAX is not representable and wraps negative.
    pub fn wrap(self, v: i128) -> i128 {
        let b = self.bits();
        if b >= 128 {
            return v;
        }
        let m = (1i128 << b) - 1;
        let x = v & m;
        if self.signed() && x & (1i128 << (b - 1)) != 0 {
            x - (1i128 << b)
        } else {
            x
        }
    }
    pub fn min(self) -> i128 {
        match self {
            IT::I128 | IT::Lit => i128::MIN,
            IT::U128 => 0,
            t if t.signed() => -(1i128 << (t.bits() - 1)),
            _ => 0,
        }
    }
    pub fn max(self) -> i128 {
        match self {
            IT::I128 | IT::Lit | IT::U128 => i128::MAX,
            t if t.signed() => (1i128 << (t.bits() - 1)) - 1,
            t => (1i128 << t.bits()) - 1,
        }
    }
    /// How a dynamic value of this type is held.
    pub fn rep(self, range: (i128, i128)) -> Rep {
        match self.bits() {
            8 | 16 | 32 => Rep::I32,
            64 => Rep::I64,
            _ => {
                if range.0 >= i64::MIN as i128 && range.1 <= i64::MAX as i128 {
                    Rep::I64
                } else {
                    Rep::Wide
                }
            }
        }
    }
}

/// The representation of a dynamic integer: an i32 (the low bits; the
/// type's width and signedness say how to read them), an i64 (a 64-bit
/// type's bits, or a 128-bit type's exact value when its range fits), or a
/// pair of i64 locals (low, high) for a 128-bit value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rep {
    I32,
    I64,
    Wide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Int(IT, Rep),
    Bool,
    F64,
    F32,
}

impl Kind {
    pub fn w(self) -> W {
        match self {
            Kind::Int(_, Rep::I32) | Kind::Bool => W::I32,
            Kind::Int(_, _) => W::I64,
            Kind::F64 => W::F64,
            Kind::F32 => W::F32,
        }
    }
}

/// A dynamic scalar: its local(s), kind, the range of its value (ints) and,
/// when known, the finite set of values it can take.
#[derive(Clone, Debug)]
pub struct Dv {
    pub l: u32,
    /// The high half of a Wide value.
    pub l2: u32,
    pub k: Kind,
    pub lo: i128,
    pub hi: i128,
    pub vset: Option<Rc<Vec<i128>>>,
}

impl PartialEq for Dv {
    fn eq(&self, o: &Dv) -> bool {
        self.l == o.l && self.k == o.k
    }
}

#[derive(Clone, Debug)]
pub enum Av {
    Int(i128, IT),
    Bool(bool),
    F64(f64),
    F32(f32),
    Unit,
    Str(Rc<str>),
    D(Dv),
    Tuple(Rc<Vec<Av>>),
    Struct(Rc<str>, Rc<Vec<Av>>),
    /// A known variant of an enum ("Option": None 0, Some 1; "Result": Ok 0,
    /// Err 1; generated/runtime enums in declaration order).
    Enum(Rc<str>, u32, Rc<Vec<Av>>),
    /// An enum whose variant is known at run time only: the discriminant
    /// (an i32 local) and each variant's payload (Undef where not taken).
    DEnum(Rc<str>, Box<Dv>, Rc<Vec<Vec<Av>>>),
    /// Arrays and slices (`&[..]`, `[T; N]`).
    Arr(Rc<Vec<Av>>),
    /// rt::Tup<T> with its items (a static length).
    Tup(Rc<Vec<Av>>),
    /// The machine state (`s: &mut St`).
    St,
    /// The run configuration `s.cfg` (static).
    Cfg,
    /// A function value (a path).
    Fn(Rc<str>),
    Range(i128, i128),
    Undef,
}

impl PartialEq for Av {
    fn eq(&self, o: &Av) -> bool {
        use Av::*;
        match (self, o) {
            (Int(a, _), Int(b, _)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (F64(a), F64(b)) => a.to_bits() == b.to_bits(),
            (F32(a), F32(b)) => a.to_bits() == b.to_bits(),
            (Unit, Unit) | (St, St) | (Cfg, Cfg) | (Undef, Undef) => true,
            (Str(a), Str(b)) => a == b,
            (D(a), D(b)) => a == b,
            (Tuple(a), Tuple(b)) | (Arr(a), Arr(b)) | (Tup(a), Tup(b)) => {
                Rc::ptr_eq(a, b) || a == b
            }
            (Struct(n, a), Struct(m, b)) => n == m && (Rc::ptr_eq(a, b) || a == b),
            (Enum(n, v, a), Enum(m, w, b)) => n == m && v == w && (Rc::ptr_eq(a, b) || a == b),
            (DEnum(n, d, a), DEnum(m, e, b)) => n == m && d == e && (Rc::ptr_eq(a, b) || a == b),
            (Fn(a), Fn(b)) => a == b,
            (Range(a, b), Range(c, d)) => a == c && b == d,
            _ => false,
        }
    }
}

pub fn none() -> Av {
    Av::Enum("Option".into(), 0, Rc::new(vec![]))
}
pub fn some(v: Av) -> Av {
    Av::Enum("Option".into(), 1, Rc::new(vec![v]))
}
pub fn ok(v: Av) -> Av {
    Av::Enum("Result".into(), 0, Rc::new(vec![v]))
}
pub fn st(name: &str, fields: Vec<Av>) -> Av {
    Av::Struct(name.into(), Rc::new(fields))
}
pub fn int(v: i128) -> Av {
    Av::Int(v, IT::I128)
}

impl Av {
    pub fn is_static(&self) -> bool {
        match self {
            Av::D(_) | Av::DEnum(..) | Av::Undef => false,
            Av::Tuple(v) | Av::Struct(_, v) | Av::Enum(_, _, v) | Av::Arr(v) | Av::Tup(v) => {
                v.iter().all(|x| x.is_static())
            }
            _ => true,
        }
    }
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Av::Int(v, _) => Some(*v),
            Av::Bool(b) => Some(*b as i128),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Av::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// The aggregate's items (tuple, struct fields, enum payload, array).
    pub fn items(&self) -> Option<&Rc<Vec<Av>>> {
        match self {
            Av::Tuple(v) | Av::Struct(_, v) | Av::Enum(_, _, v) | Av::Arr(v) | Av::Tup(v) => Some(v),
            _ => None,
        }
    }
}
