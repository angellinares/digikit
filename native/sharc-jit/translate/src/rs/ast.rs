//! Syntax tree of the Rust subset (see parse.rs).

use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    /// A path type with generic arguments: `Int`, `Option<V>`,
    /// `Tup<(Int, Sym)>`, `R<()>`, `crate::rt::V`.
    Path(String, Vec<Ty>),
    Tuple(Vec<Ty>),
    Ref(bool, Box<Ty>),
    Array(Box<Ty>, Option<Box<Expr>>),
    Slice(Box<Ty>),
    Fn,
    Infer,
}

impl Ty {
    pub fn unit() -> Ty {
        Ty::Tuple(vec![])
    }
    pub fn name(&self) -> &str {
        match self {
            Ty::Path(n, _) => n,
            _ => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pat {
    Wild,
    /// A binding (`x`, `mut x`, `ref x`).
    Bind(String),
    /// A path: a constant, a unit variant, `None`.
    Path(String),
    /// `Name(p, ...)`: tuple variant / tuple struct.
    TupleStruct(String, Vec<Pat>),
    Tuple(Vec<Pat>),
    Lit(Box<Expr>),
    Or(Vec<Pat>),
    /// `a..=b` or `a..b` (literal bounds).
    Range(Box<Expr>, Box<Expr>, bool),
    Struct(String, Vec<(String, Pat)>),
}

pub type E = Box<Expr>;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Int(u128, String),
    Float(f64, String),
    Bool(bool),
    Str(String),
    Char(char),
    /// A path: `x`, `S_FOO`, `super::flags::_f`, `V::UNK`.
    Path(String),
    Call(E, Vec<Expr>),
    /// `recv.name(args)`.
    Method(E, String, Vec<Expr>),
    Field(E, String),
    Index(E, E),
    Unary(&'static str, E),
    /// `&e` / `&mut e`.
    Ref(bool, E),
    Deref(E),
    Binary(&'static str, E, E),
    Cast(E, Ty),
    Try(E),
    If(E, Rc<Block>, Option<E>),
    Match(E, Vec<Arm>),
    Block(Rc<Block>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    Repeat(E, E),
    StructLit(String, Vec<(String, Expr)>, Option<E>),
    Range(Option<E>, Option<E>, bool),
    /// `matches!(e, pat if guard)`.
    Matches(E, Pat, Option<E>),
    /// `unreachable!(...)`, `panic!(...)`.
    Unreachable,
    Closure(Vec<(Pat, Ty)>, E),
    Loop(Rc<Block>, Option<String>),
    While(E, Rc<Block>),
    For(Pat, E, Rc<Block>),
    Break(Option<E>),
    Continue,
    Return(Option<E>),
    /// `let PAT = EXPR` inside an `if` condition (possibly in a `&&` chain).
    Let(Pat, E),
    /// `a = b` / `a op= b`, op "" for plain assignment.
    Assign(&'static str, E, E),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let(Pat, Option<Ty>, Option<Expr>),
    /// `let PAT = EXPR else { ... };`
    LetElse(Pat, Expr, std::rc::Rc<Block>),
    Expr(Expr),
    /// An expression statement with a trailing `;`.
    Semi(Expr),
    Item,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// The trailing expression, if any.
    pub tail: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub pat: Pat,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    /// Full path, e.g. "core::compute_alu::_alu_carry_impl",
    /// "rt::bnd::_add", "rt::V::is_c".
    pub path: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Rc<Block>,
    /// A method's receiver (`self`), by value or reference.
    pub has_self: bool,
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnDef),
    Const(String, Ty, Expr),
    Struct(String, Vec<(String, Ty)>, bool),
    Enum(String, Vec<(String, Vec<Ty>)>),
    TypeAlias(String, Ty),
}
