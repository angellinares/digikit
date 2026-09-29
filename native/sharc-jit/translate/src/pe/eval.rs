//! The evaluator: statements, expressions, patterns, calls (always
//! inlined), and the joins that merge abstract states where run-time
//! control flow meets.

use super::ir::{Hole, Label, Locals, Node, W};
use super::mach::{Ctx, MState};
use super::types::{RTy, Types};
use super::val::*;
use crate::rs::ast::*;
use crate::rs::parse::Program;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_encoder::Instruction as I;

#[derive(Debug, Clone)]
pub struct Fail(pub String);

/// What a run-time boolean says about integer locals where it holds.
#[derive(Debug, Clone)]
pub enum Fact {
    /// local OP constant
    Cmp(u32, &'static str, i128),
    And(Option<Box<Fact>>, Option<Box<Fact>>),
    Or(Option<Box<Fact>>, Option<Box<Fact>>),
    Not(Box<Fact>),
}

pub type R<T> = Result<T, Fail>;

pub fn fail<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail(msg.into()))
}

/// How evaluation of an expression ended.
#[derive(Debug, Clone)]
pub enum Flow {
    V(Av),
    /// Control left: a return, a trap, a jump out.
    Div,
    Brk,
    Cont,
}

macro_rules! val {
    ($e:expr) => {
        match $e? {
            Flow::V(v) => v,
            other => return Ok(other),
        }
    };
}

pub struct RetFrame {
    pub label: Label,
    pub edges: Vec<(MState, Av, Hole)>,
}

pub struct Pe<'p> {
    pub prog: &'p Program,
    pub ty: &'p Types,
    pub cx: &'p Ctx<'p>,
    pub loc: Locals,
    pub code: Vec<Node>,
    pub holes: Vec<Vec<Node>>,
    pub labels: u32,
    pub env: Vec<Vec<(Rc<str>, Av)>>,
    pub module: Rc<str>,
    pub frames: Vec<RetFrame>,
    pub ms: MState,
    pub trap: Label,
    pub depth: u32,
    pub resolved: HashMap<(Rc<str>, String), Option<String>>,
    pub consts: HashMap<String, Av>,
    /// Instructions evaluated (a budget against runaway unrolling).
    pub steps: u64,
    /// Region-wide: each register's home locals (value, mask), the
    /// registers whose mask the prologue must find all known, and the
    /// registers written back at exits.
    pub homes: std::collections::BTreeMap<u32, (u32, u32)>,
    pub needs_known: std::collections::BTreeSet<u32>,
    pub written: std::collections::BTreeSet<u32>,
    /// Boolean local -> what it says (range refinement at run-time ifs).
    pub facts: HashMap<u32, Fact>,
}

impl<'p> Pe<'p> {
    pub fn new(prog: &'p Program, ty: &'p Types, cx: &'p Ctx<'p>, params: u32) -> Pe<'p> {
        Pe {
            prog,
            ty,
            cx,
            loc: Locals::new(params),
            code: Vec::new(),
            holes: Vec::new(),
            labels: 0,
            env: Vec::new(),
            module: "core".into(),
            frames: Vec::new(),
            ms: MState::new(),
            trap: u32::MAX,
            depth: 0,
            resolved: HashMap::new(),
            consts: HashMap::new(),
            steps: 0,
            homes: Default::default(),
            needs_known: Default::default(),
            written: Default::default(),
            facts: HashMap::new(),
        }
    }

    // ------------------------------------------------------------- basics

    pub fn label(&mut self) -> Label {
        self.labels += 1;
        self.labels
    }
    pub fn hole(&mut self) -> Hole {
        self.holes.push(Vec::new());
        (self.holes.len() - 1) as Hole
    }
    pub fn emit(&mut self, i: I<'static>) {
        self.code.push(Node::I(i));
    }
    pub fn local(&mut self, w: W) -> u32 {
        self.loc.add(w)
    }

    /// Run F with its code going into hole H (appended).
    pub fn in_hole<T>(&mut self, h: Hole, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::take(&mut self.code);
        let r = f(self);
        let code = std::mem::replace(&mut self.code, saved);
        self.holes[h as usize].extend(code);
        r
    }

    // ------------------------------------------------------------ variables

    pub fn lookup(&self, name: &str) -> Option<&Av> {
        for scope in self.env.iter().rev() {
            for (n, v) in scope.iter().rev() {
                if &**n == name {
                    return Some(v);
                }
            }
        }
        None
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Av> {
        for scope in self.env.iter_mut().rev() {
            for (n, v) in scope.iter_mut().rev() {
                if &**n == name {
                    return Some(v);
                }
            }
        }
        None
    }

    pub fn bind(&mut self, name: &str, v: Av) {
        if self.env.is_empty() {
            self.env.push(Vec::new());
        }
        self.env.last_mut().unwrap().push((name.into(), v));
    }

    // ------------------------------------------------------------ resolution

    /// The full path of an item named PATH from the current module.
    pub fn resolve(&mut self, path: &str) -> Option<String> {
        let key = (self.module.clone(), path.to_string());
        if let Some(r) = self.resolved.get(&key) {
            return r.clone();
        }
        let r = self.resolve_uncached(path);
        self.resolved.insert(key, r.clone());
        r
    }

    fn resolve_uncached(&self, path: &str) -> Option<String> {
        let mut module: Vec<&str> = self.module.split("::").collect();
        let mut rest = path;
        let mut explicit = false;
        loop {
            if let Some(r) = rest.strip_prefix("super::") {
                module.pop();
                rest = r;
                explicit = true;
            } else if let Some(r) = rest.strip_prefix("crate::generated::core_g::") {
                module = vec!["core"];
                rest = r;
                explicit = true;
            } else if let Some(r) = rest.strip_prefix("crate::generated::") {
                module = vec![];
                rest = r;
                explicit = true;
            } else if let Some(r) = rest.strip_prefix("crate::") {
                module = vec![];
                rest = r;
                explicit = true;
            } else if let Some(r) = rest.strip_prefix("self::") {
                rest = r;
                explicit = true;
            } else {
                break;
            }
        }
        let base = module.join("::");
        let cand = crate::rs::parse::join(&base, rest);
        if self.prog.items.contains_key(&cand) {
            return Some(cand);
        }
        if explicit {
            // crate::rt::bnd::x style: absolute from the top.
            if self.prog.items.contains_key(rest) {
                return Some(rest.to_string());
            }
        }
        // Type-associated items: `V::c` -> rt::V::c (the type's module).
        if let Some((tname, item)) = rest.split_once("::")
            && let Some(full) = self.ty.full_path(tname)
        {
            let c = format!("{full}::{item}");
            if self.prog.items.contains_key(&c) {
                return Some(c);
            }
        }
        let mut mods = vec![base.clone()];
        let mut k = 0;
        while k < mods.len() && k < 16 {
            if let Some(g) = self.prog.globs.get(&mods[k]) {
                for m in g {
                    if !mods.contains(m) {
                        mods.push(m.clone());
                    }
                }
            }
            k += 1;
        }
        for m in &mods[1..] {
            let c = crate::rs::parse::join(m, rest);
            if self.prog.items.contains_key(&c) {
                return Some(c);
            }
        }
        if self.prog.items.contains_key(rest) {
            return Some(rest.to_string());
        }
        None
    }

    // ------------------------------------------------------------ constants

    pub fn const_value(&mut self, full: &str) -> R<Av> {
        if let Some(v) = self.consts.get(full) {
            return Ok(v.clone());
        }
        let Some(Item::Const(_, ty, e)) = self.prog.items.get(full) else {
            return fail(format!("not a constant: {full}"));
        };
        let (ty, e) = (ty.clone(), e.clone());
        let hint = self.ty.resolve(&ty);
        let saved_mod = std::mem::replace(&mut self.module, module_of(full, false).into());
        let saved_env = std::mem::take(&mut self.env);
        let saved_code = std::mem::take(&mut self.code);
        let r = self.expr(&e, Some(&hint));
        self.env = saved_env;
        self.module = saved_mod;
        let code = std::mem::replace(&mut self.code, saved_code);
        let v = match r? {
            Flow::V(v) => v,
            _ => return fail(format!("constant {full} diverges")),
        };
        if !code.is_empty() || !v.is_static() {
            return fail(format!("constant {full} is not static"));
        }
        let v = self.coerce_static(v, &hint);
        self.consts.insert(full.to_string(), v.clone());
        Ok(v)
    }

    /// Give an unsuffixed literal (possibly nested) the type HINT.
    pub fn coerce_static(&self, v: Av, hint: &RTy) -> Av {
        match (&v, hint) {
            (Av::Int(x, IT::Lit), RTy::Int(it)) => Av::Int(it.wrap(*x), *it),
            (Av::Int(x, IT::Lit), RTy::F64) => Av::F64(*x as f64),
            (Av::Tuple(items), RTy::Tuple(ts)) if items.len() == ts.len() => Av::Tuple(Rc::new(
                items.iter().zip(ts).map(|(i, t)| self.coerce_static(i.clone(), t)).collect(),
            )),
            (Av::Arr(items), RTy::Array(t)) => {
                Av::Arr(Rc::new(items.iter().map(|i| self.coerce_static(i.clone(), t)).collect()))
            }
            (_, RTy::Ref(t)) => self.coerce_static(v, t),
            _ => v,
        }
    }

    // ----------------------------------------------------------- statements

    pub fn block(&mut self, b: &Block, hint: Option<&RTy>) -> R<Flow> {
        self.env.push(Vec::new());
        let r = self.block_inner(b, hint);
        self.env.pop();
        r
    }

    fn block_inner(&mut self, b: &Block, hint: Option<&RTy>) -> R<Flow> {
        for s in &b.stmts {
            match s {
                Stmt::Let(pat, ty, init) => {
                    let hint = ty.as_ref().map(|t| self.ty.resolve(t));
                    let v = match init {
                        Some(e) => val!(self.expr(e, hint.as_ref())),
                        None => Av::Undef,
                    };
                    let v = match &hint {
                        Some(h) => self.coerce_static(v, h),
                        None => v,
                    };
                    self.bind_pat(pat, v)?;
                }
                Stmt::LetElse(pat, e, els) => {
                    let v = val!(self.expr(e, None));
                    match self.pat_test(pat, &v)? {
                        PatRes::Yes(b) => {
                            for (n, x) in b {
                                self.bind(&n, x);
                            }
                        }
                        PatRes::No => {
                            let f = self.block(els, None)?;
                            if let Flow::V(_) = f {
                                return fail("let-else block does not diverge");
                            }
                            return Ok(f);
                        }
                        PatRes::Dyn(c, b) => {
                            // if !c { else-block (diverges) }
                            let els = els.clone();
                            let f = self.fork(&c, |_| Ok(Flow::V(Av::Unit)), |pe| pe.block(&els, None))?;
                            if let Flow::V(_) = f {
                                for (n, x) in b {
                                    self.bind(&n, x);
                                }
                            } else {
                                return Ok(f);
                            }
                        }
                    }
                }
                Stmt::Expr(e) | Stmt::Semi(e) => {
                    let _ = val!(self.expr(e, None));
                }
                Stmt::Item => {}
            }
        }
        match &b.tail {
            Some(e) => self.expr(e, hint),
            None => Ok(Flow::V(Av::Unit)),
        }
    }

    pub fn bind_pat(&mut self, pat: &Pat, v: Av) -> R<()> {
        match pat {
            // `let A = ..`: an irrefutable single name binds, whatever its case.
            Pat::Path(n) if !n.contains("::") && n != "None" => {
                self.bind(n, v);
                Ok(())
            }
            Pat::Bind(n) => {
                self.bind(n, v);
                Ok(())
            }
            Pat::Wild => Ok(()),
            Pat::Tuple(ps) => {
                let items = match &v {
                    Av::Tuple(items) => items.clone(),
                    _ => return fail(format!("tuple pattern on {v:?}")),
                };
                if items.len() != ps.len() {
                    return fail("tuple pattern arity");
                }
                for (p, x) in ps.iter().zip(items.iter()) {
                    self.bind_pat(p, x.clone())?;
                }
                Ok(())
            }
            _ => match self.pat_test(pat, &v)? {
                PatRes::Yes(b) => {
                    for (n, x) in b {
                        self.bind(&n, x);
                    }
                    Ok(())
                }
                _ => fail(format!("refutable let pattern {pat:?}")),
            },
        }
    }

    // ---------------------------------------------------------- expressions

    pub fn expr(&mut self, e: &Expr, hint: Option<&RTy>) -> R<Flow> {
        self.steps += 1;
        if self.steps > 4_000_000 {
            return fail("evaluation budget");
        }
        match e {
            Expr::Int(v, suffix) => {
                let it = if suffix.is_empty() {
                    match hint {
                        Some(RTy::Int(it)) => *it,
                        Some(RTy::F64) => return Ok(Flow::V(Av::F64(*v as f64))),
                        _ => IT::Lit,
                    }
                } else {
                    IT::from_name(suffix).unwrap_or(IT::Lit)
                };
                Ok(Flow::V(Av::Int(it.wrap(*v as i128), it)))
            }
            Expr::Float(v, suffix) => Ok(Flow::V(if suffix == "f32" || matches!(hint, Some(RTy::F32)) {
                Av::F32(*v as f32)
            } else {
                Av::F64(*v)
            })),
            Expr::Bool(b) => Ok(Flow::V(Av::Bool(*b))),
            Expr::Str(s) => Ok(Flow::V(Av::Str(s.as_str().into()))),
            Expr::Char(c) => Ok(Flow::V(Av::Int(*c as i128, IT::U32))),
            Expr::Path(p) => self.path(p, hint).map(Flow::V),
            Expr::Call(f, args) => self.call(f, args, hint),
            Expr::Method(recv, name, args) => self.method(recv, name, args, hint),
            Expr::Field(b, f) => {
                let bv = val!(self.expr(b, None));
                self.field(&bv, f).map(Flow::V)
            }
            Expr::Index(b, i) => {
                let bv = val!(self.expr(b, None));
                let iv = val!(self.expr(i, Some(&RTy::Int(IT::Usize))));
                self.index(&bv, &iv).map(Flow::V)
            }
            Expr::Unary(op, x) => {
                let v = val!(self.expr(x, hint));
                self.unary(op, v).map(Flow::V)
            }
            Expr::Ref(_, x) | Expr::Deref(x) => self.expr(x, hint.map(strip_ref)),
            Expr::Binary(op, a, b) => self.binary(op, a, b, hint),
            Expr::Cast(x, t) => {
                let to = self.ty.resolve(t);
                let v = val!(self.expr(x, None));
                self.cast(v, &to).map(Flow::V)
            }
            Expr::Try(x) => {
                let v = val!(self.expr(x, None));
                self.try_(v)
            }
            Expr::If(c, then, els) => {
                if has_let(c) {
                    let mut conds = Vec::new();
                    conjuncts(c, &mut conds);
                    return self.if_chain(Rc::new(conds), 0, then.clone(), els.clone(), hint);
                }
                let cv = val!(self.expr(c, Some(&RTy::Bool)));
                self.if_(cv, then.clone(), els.clone(), hint)
            }
            Expr::Let(..) => fail("let outside an if condition"),
            Expr::Match(scrut, arms) => {
                let sv = val!(self.expr(scrut, None));
                self.match_arms(&sv, arms, 0, hint)
            }
            Expr::Block(b) => self.block(b, hint),
            Expr::Tuple(items) => {
                let hints: Vec<Option<RTy>> = match hint {
                    Some(RTy::Tuple(ts)) if ts.len() == items.len() => ts.iter().cloned().map(Some).collect(),
                    _ => vec![None; items.len()],
                };
                let mut out = Vec::new();
                for (x, h) in items.iter().zip(hints.iter()) {
                    out.push(val!(self.expr(x, h.as_ref())));
                }
                if out.is_empty() {
                    return Ok(Flow::V(Av::Unit));
                }
                Ok(Flow::V(Av::Tuple(Rc::new(out))))
            }
            Expr::Array(items) => {
                let h = match hint.map(strip_ref) {
                    Some(RTy::Array(t)) => Some((**t).clone()),
                    _ => None,
                };
                let mut out = Vec::new();
                for x in items {
                    let v = val!(self.expr(x, h.as_ref()));
                    out.push(match &h {
                        Some(h) => self.coerce_static(v, h),
                        None => v,
                    });
                }
                Ok(Flow::V(Av::Arr(Rc::new(out))))
            }
            Expr::Repeat(x, n) => {
                let h = match hint.map(strip_ref) {
                    Some(RTy::Array(t)) => Some((**t).clone()),
                    _ => None,
                };
                let v = val!(self.expr(x, h.as_ref()));
                let n = val!(self.expr(n, Some(&RTy::Int(IT::Usize))));
                let Some(n) = n.as_int() else {
                    return fail("dynamic array length");
                };
                Ok(Flow::V(Av::Arr(Rc::new(vec![v; n as usize]))))
            }
            Expr::StructLit(path, fields, base) => self.struct_lit(path, fields, base.as_deref()),
            Expr::Range(a, b, incl) => {
                let a = match a {
                    Some(a) => val!(self.expr(a, Some(&RTy::Int(IT::I128)))),
                    None => Av::Int(0, IT::I128),
                };
                let b = match b {
                    Some(b) => val!(self.expr(b, Some(&RTy::Int(IT::I128)))),
                    None => return fail("open range"),
                };
                match (a.as_int(), b.as_int()) {
                    (Some(x), Some(y)) => Ok(Flow::V(Av::Range(x, if *incl { y + 1 } else { y }))),
                    _ => fail("dynamic range"),
                }
            }
            Expr::Matches(x, pat, guard) => {
                let v = val!(self.expr(x, None));
                let r = self.pat_test(pat, &v)?;
                let (c, binds) = match r {
                    PatRes::Yes(b) => (Av::Bool(true), b),
                    PatRes::No => return Ok(Flow::V(Av::Bool(false))),
                    PatRes::Dyn(c, b) => (c, b),
                };
                match guard {
                    None => Ok(Flow::V(c)),
                    Some(g) => {
                        self.env.push(binds);
                        let gv = self.expr(g, Some(&RTy::Bool));
                        self.env.pop();
                        let gv = match gv? {
                            Flow::V(v) => v,
                            f => return Ok(f),
                        };
                        self.logic_and(c, gv).map(Flow::V)
                    }
                }
            }
            Expr::Unreachable => {
                self.code.push(Node::Br(self.trap));
                Ok(Flow::Div)
            }
            Expr::Closure(..) => fail("closure"),
            Expr::Loop(b, _) => self.loop_(b),
            Expr::While(c, b) => self.while_(c, b),
            Expr::For(pat, it, body) => self.for_(pat, it, body),
            Expr::Break(v) => {
                if v.is_some() {
                    return fail("break with value");
                }
                Ok(Flow::Brk)
            }
            Expr::Continue => Ok(Flow::Cont),
            Expr::Return(v) => {
                let v = match v {
                    Some(x) => val!(self.expr(x, None)),
                    None => Av::Unit,
                };
                self.ret(v)
            }
            Expr::Assign(op, lhs, rhs) => self.assign(op, lhs, rhs),
        }
    }

    /// `return V` from the current inlined function.
    pub fn ret(&mut self, v: Av) -> R<Flow> {
        if let Av::Enum(n, 1, _) = &v
            && &**n == "Result"
        {
            // Err: every caller applies `?`, so it is the instruction's trap.
            self.code.push(Node::Br(self.trap));
            return Ok(Flow::Div);
        }
        let h = self.hole();
        let Some(fr) = self.frames.last_mut() else {
            return fail("return outside a function");
        };
        fr.edges.push((self.ms.clone(), v, h));
        let l = fr.label;
        self.code.push(Node::Hole(h));
        self.code.push(Node::Br(l));
        Ok(Flow::Div)
    }

    fn try_(&mut self, v: Av) -> R<Flow> {
        match &v {
            Av::Enum(n, 0, p) if &**n == "Result" => Ok(Flow::V(p[0].clone())),
            Av::Enum(n, 1, _) if &**n == "Result" => {
                self.code.push(Node::Br(self.trap));
                Ok(Flow::Div)
            }
            Av::DEnum(n, d, p) if &**n == "Result" => {
                // if disc == Err { trap }
                let d = (**d).clone();
                self.emit(I::LocalGet(d.l));
                self.code.push(Node::BrIf(self.trap));
                Ok(Flow::V(p[0][0].clone()))
            }
            _ => fail(format!("? on {v:?}")),
        }
    }

    fn assign(&mut self, op: &str, lhs: &Expr, rhs: &Expr) -> R<Flow> {
        // Erased state writes (observability only).
        if let Expr::Field(b, f) = lhs
            && matches!(&**b, Expr::Path(p) if p == "s")
            && (f == "steps" || f == "at_loaded_entry")
        {
            return Ok(Flow::V(Av::Unit));
        }
        let hint = self.place_type_hint(lhs);
        let mut v = val!(self.expr(rhs, hint.as_ref()));
        if !op.is_empty() {
            let cur = val!(self.expr(lhs, None));
            v = self.binop(op, cur, v)?;
        }
        self.store_place(lhs, v)?;
        Ok(Flow::V(Av::Unit))
    }

    fn place_type_hint(&self, lhs: &Expr) -> Option<RTy> {
        if let Expr::Path(p) = lhs
            && let Some(v) = self.lookup(p)
        {
            return match v {
                Av::Int(_, it) => Some(RTy::Int(*it)),
                Av::D(d) => match d.k {
                    Kind::Int(it, _) => Some(RTy::Int(it)),
                    _ => None,
                },
                _ => None,
            };
        }
        None
    }

    fn store_place(&mut self, lhs: &Expr, v: Av) -> R<()> {
        match lhs {
            Expr::Path(p) => {
                let old_it = match self.lookup(p) {
                    Some(Av::Int(_, it)) => Some(*it),
                    Some(Av::D(d)) => match d.k {
                        Kind::Int(it, _) => Some(it),
                        _ => None,
                    },
                    Some(_) => None,
                    None => return fail(format!("assignment to unknown {p}")),
                };
                let v = match (v, old_it) {
                    (Av::Int(x, IT::Lit), Some(it)) => Av::Int(it.wrap(x), it),
                    (v, _) => v,
                };
                *self.lookup_mut(p).unwrap() = v;
                Ok(())
            }
            Expr::Field(b, f) => {
                if let Expr::Path(p) = &**b
                    && p == "s"
                {
                    return self.st_store(f, v);
                }
                let bv = match self.expr(b, None)? {
                    Flow::V(x) => x,
                    _ => return fail("place diverges"),
                };
                let nb = self.set_field(bv, f, v)?;
                self.store_place(b, nb)
            }
            Expr::Index(b, i) => {
                let bv = match self.expr(b, None)? {
                    Flow::V(x) => x,
                    _ => return fail("place diverges"),
                };
                let iv = match self.expr(i, Some(&RTy::Int(IT::Usize)))? {
                    Flow::V(x) => x,
                    _ => return fail("index diverges"),
                };
                let Some(k) = iv.as_int() else {
                    return fail("dynamic index store");
                };
                let nb = match bv {
                    Av::Arr(items) => {
                        let mut items = (*items).clone();
                        if k < 0 || k as usize >= items.len() {
                            return fail("index store out of range");
                        }
                        items[k as usize] = v;
                        Av::Arr(Rc::new(items))
                    }
                    _ => return fail("index store on a non-array"),
                };
                self.store_place(b, nb)
            }
            Expr::Deref(x) => self.store_place(x, v),
            _ => fail(format!("unsupported place {lhs:?}")),
        }
    }

    pub fn set_field(&self, bv: Av, f: &str, v: Av) -> R<Av> {
        match bv {
            Av::Struct(n, items) => {
                let k = self.field_pos(&n, f)?;
                let mut items = (*items).clone();
                items[k] = v;
                Ok(Av::Struct(n, Rc::new(items)))
            }
            Av::Tuple(items) => {
                let k: usize = f.parse().map_err(|_| Fail("tuple field".into()))?;
                let mut items = (*items).clone();
                items[k] = v;
                Ok(Av::Tuple(Rc::new(items)))
            }
            _ => fail(format!("field store .{f} on {bv:?}")),
        }
    }

    fn field_pos(&self, n: &str, f: &str) -> R<usize> {
        if let Some(k) = self.ty.field_index(n, f) {
            return Ok(k);
        }
        match f.parse::<usize>() {
            Ok(k) => Ok(k),
            Err(_) => fail(format!("no field {n}.{f}")),
        }
    }

    pub fn field(&mut self, bv: &Av, f: &str) -> R<Av> {
        match bv {
            Av::St => self.st_field(f),
            Av::Cfg => self.cfg_field(f),
            Av::Struct(n, items) => {
                let k = self.field_pos(n, f)?;
                items.get(k).cloned().ok_or_else(|| Fail(format!("field {n}.{f}")))
            }
            Av::Tuple(items) => {
                let k: usize = f.parse().map_err(|_| Fail(format!("tuple field {f}")))?;
                items.get(k).cloned().ok_or_else(|| Fail("tuple index".into()))
            }
            _ => fail(format!("field .{f} of {bv:?}")),
        }
    }

    fn index(&mut self, b: &Av, i: &Av) -> R<Av> {
        let Some(k) = i.as_int() else {
            return self.dyn_index(b, i);
        };
        match b {
            Av::Arr(items) | Av::Tup(items) => {
                if k < 0 || k as usize >= items.len() {
                    // A Rust panic: out of range.
                    return fail("static index out of range");
                }
                Ok(items[k as usize].clone())
            }
            _ => fail(format!("index into {b:?}")),
        }
    }

    /// A[i] with i known at run time: a chain of selects over the items.
    fn dyn_index(&mut self, b: &Av, i: &Av) -> R<Av> {
        let items = match b {
            Av::Arr(items) => items.clone(),
            _ => return fail("dynamic index into a non-array"),
        };
        if items.is_empty() || items.len() > 64 || !items.iter().all(|x| x.as_int().is_some()) {
            return fail("dynamic index into a non-integer array");
        }
        let Av::D(d) = i else { return fail("index value") };
        let mut out = items[items.len() - 1].clone();
        for k in (0..items.len() - 1).rev() {
            let eq = self.binop("==", Av::D(d.clone()), Av::Int(k as i128, IT::Usize))?;
            out = self.select(&eq, items[k].clone(), out)?;
        }
        Ok(out)
    }

    fn struct_lit(&mut self, path: &str, fields: &[(String, Expr)], base: Option<&Expr>) -> R<Flow> {
        let name = path.rsplit("::").next().unwrap().to_string();
        let Some(defs) = self.ty.struct_fields(&name).cloned() else {
            return fail(format!("unknown struct {path}"));
        };
        let mut items: Vec<Av> = match base {
            Some(b) => match val!(self.expr(b, None)) {
                Av::Struct(_, it) => (*it).clone(),
                other => return fail(format!("struct base {other:?}")),
            },
            None => vec![Av::Undef; defs.len()],
        };
        for (f, e) in fields {
            let k = defs.iter().position(|(n, _)| n == f).ok_or_else(|| Fail(format!("no field {f}")))?;
            let h = self.ty.resolve(&defs[k].1);
            let v = val!(self.expr(e, Some(&h)));
            items[k] = self.coerce_static(v, &h);
        }
        Ok(Flow::V(Av::Struct(name.into(), Rc::new(items))))
    }

    // --------------------------------------------------------------- paths

    fn path(&mut self, p: &str, hint: Option<&RTy>) -> R<Av> {
        if let Some(v) = self.lookup(p) {
            return Ok(v.clone());
        }
        match p {
            "s" => return Ok(Av::St),
            "None" => return Ok(none()),
            "true" => return Ok(Av::Bool(true)),
            "false" => return Ok(Av::Bool(false)),
            _ => {}
        }
        if let Some(v) = self.prim_const(p) {
            return Ok(v);
        }
        if let Some(full) = self.resolve(p) {
            match self.prog.items.get(&full) {
                Some(Item::Const(..)) => return self.const_value(&full),
                Some(Item::Fn(_)) => return Ok(Av::Fn(full.into())),
                _ => {}
            }
        }
        // A unit enum variant `E::V`.
        if let Some((e, v)) = p.rsplit_once("::") {
            let e = e.rsplit("::").next().unwrap();
            if let Some(k) = self.ty.variant_index(e, v) {
                return Ok(Av::Enum(e.into(), k, Rc::new(vec![])));
            }
        }
        let _ = hint;
        fail(format!("unresolved path {p} in {}", self.module))
    }

    fn prim_const(&self, p: &str) -> Option<Av> {
        let (t, c) = p.rsplit_once("::")?;
        if let Some(it) = IT::from_name(t) {
            return match c {
                "MAX" => Some(Av::Int(it.max(), it)),
                "MIN" => Some(Av::Int(it.min(), it)),
                "BITS" => Some(Av::Int(it.bits() as i128, IT::U32)),
                _ => None,
            };
        }
        if t == "f64" {
            return match c {
                "INFINITY" => Some(Av::F64(f64::INFINITY)),
                "NEG_INFINITY" => Some(Av::F64(f64::NEG_INFINITY)),
                "NAN" => Some(Av::F64(f64::NAN)),
                "MAX" => Some(Av::F64(f64::MAX)),
                "MIN_POSITIVE" => Some(Av::F64(f64::MIN_POSITIVE)),
                _ => None,
            };
        }
        if t == "f32" {
            return match c {
                "INFINITY" => Some(Av::F32(f32::INFINITY)),
                "NEG_INFINITY" => Some(Av::F32(f32::NEG_INFINITY)),
                "MAX" => Some(Av::F32(f32::MAX)),
                _ => None,
            };
        }
        None
    }

    // --------------------------------------------------------------- calls

    fn args(&mut self, args: &[Expr], hints: &[Option<RTy>]) -> R<Result<Vec<Av>, Flow>> {
        let mut out = Vec::new();
        for (k, a) in args.iter().enumerate() {
            let h = hints.get(k).cloned().flatten();
            match self.expr(a, h.as_ref())? {
                Flow::V(v) => out.push(match &h {
                    Some(h) => self.coerce_static(v, h),
                    None => v,
                }),
                f => return Ok(Err(f)),
            }
        }
        Ok(Ok(out))
    }

    fn call(&mut self, f: &Expr, args: &[Expr], hint: Option<&RTy>) -> R<Flow> {
        let Expr::Path(p) = f else {
            return fail("call of a non-path");
        };
        // Constructors.
        match p.as_str() {
            "Some" | "Ok" | "Err" => {
                let inner = match (p.as_str(), hint) {
                    ("Some", Some(RTy::Named(n, a))) if &**n == "Option" => a.first().cloned(),
                    ("Ok", Some(RTy::Named(n, a))) if &**n == "Result" => a.first().cloned(),
                    _ => None,
                };
                let v = match self.args(args, &[inner])? {
                    Ok(mut v) => v.remove(0),
                    Err(f) => return Ok(f),
                };
                return Ok(Flow::V(match p.as_str() {
                    "Some" => some(v),
                    "Ok" => ok(v),
                    _ => Av::Enum("Result".into(), 1, Rc::new(vec![v])),
                }));
            }
            _ if p.ends_with("::default") && p != "Default::default"
                && args.is_empty()
                && self.resolve(p).is_none() =>
            {
                let tn = p.trim_end_matches("::default").rsplit("::").next().unwrap_or("");
                let t = RTy::Named(tn.into(), vec![]);
                return self.default_of(&t).map(Flow::V);
            }
            "Default::default" => {
                let Some(h) = hint else {
                    return fail("Default::default without a type");
                };
                let h = h.clone();
                return self.default_of(&h).map(Flow::V);
            }
            _ => {}
        }
        // Enum variant constructors E::V(..).
        if let Some((e, v)) = p.rsplit_once("::") {
            let ename = e.rsplit("::").next().unwrap();
            if let Some(vars) = self.ty.variants(ename).cloned()
                && let Some(k) = vars.iter().position(|(n, _)| n == v)
            {
                let hints: Vec<Option<RTy>> = vars[k].1.iter().map(|t| Some(self.ty.resolve(t))).collect();
                let vals = match self.args(args, &hints)? {
                    Ok(v) => v,
                    Err(f) => return Ok(f),
                };
                return Ok(Flow::V(Av::Enum(ename.into(), k as u32, Rc::new(vals))));
            }
        }
        // Tuple struct constructors.
        let short = p.rsplit("::").next().unwrap();
        if let Some(fields) = self.ty.struct_fields(short).cloned()
            && short.chars().next().is_some_and(|c| c.is_uppercase())
        {
            let hints: Vec<Option<RTy>> = fields.iter().map(|(_, t)| Some(self.ty.resolve(t))).collect();
            let vals = match self.args(args, &hints)? {
                Ok(v) => v,
                Err(f) => return Ok(f),
            };
            return Ok(Flow::V(Av::Struct(short.into(), Rc::new(vals))));
        }
        let full = self.resolve(p);
        // Machine-state and runtime intrinsics.
        let key = full.clone().unwrap_or_else(|| p.clone());
        if self.is_intrinsic(&key) {
            let vals = match self.args(args, &self.intrinsic_hints(&key))? {
                Ok(v) => v,
                Err(f) => return Ok(f),
            };
            return self.intrinsic(&key, vals, hint);
        }
        let Some(full) = full else {
            return fail(format!("unresolved function {p} in {}", self.module));
        };
        let Some(Item::Fn(fd)) = self.prog.items.get(&full) else {
            return fail(format!("{full} is not a function"));
        };
        let fd = fd.clone();
        let hints: Vec<Option<RTy>> = fd.params.iter().map(|pr| Some(self.ty.resolve(&pr.ty))).collect();
        let vals = match self.args(args, &hints)? {
            Ok(v) => v,
            Err(f) => return Ok(f),
        };
        self.call_fn(&fd, vals, None)
    }

    pub fn call_fn(&mut self, fd: &FnDef, args: Vec<Av>, recv: Option<Av>) -> R<Flow> {
        if self.depth > 48 {
            return fail(format!("inlining too deep at {}", fd.path));
        }
        self.depth += 1;
        let saved_env = std::mem::take(&mut self.env);
        let saved_mod = std::mem::replace(&mut self.module, module_of(&fd.path, recv.is_some() || fd.has_self).into());
        let saved_code = std::mem::take(&mut self.code);
        self.env.push(Vec::new());
        if let Some(r) = recv {
            self.bind("self", r);
        }
        let mut res = Ok(());
        for (p, v) in fd.params.iter().zip(args) {
            let h = self.ty.resolve(&p.ty);
            let v = self.coerce_static(v, &h);
            if let Err(e) = self.bind_pat(&p.pat, v) {
                res = Err(e);
                break;
            }
        }
        let label = self.label();
        self.frames.push(RetFrame { label, edges: Vec::new() });
        let ret_hint = self.ty.resolve(&fd.ret);
        let body = fd.body.clone();
        let r = res.and_then(|_| self.block(&body, Some(&ret_hint)));
        let frame = self.frames.pop().unwrap();
        self.env = saved_env;
        self.module = saved_mod;
        self.depth -= 1;
        let r = match r {
            Ok(f) => f,
            Err(e) => {
                self.code = saved_code;
                return Err(Fail(format!("{} <- {}", e.0, fd.path)));
            }
        };
        let mut edges = frame.edges;
        let early = !edges.is_empty();
        match r {
            Flow::V(v) => {
                let v = self.coerce_static(v, &ret_hint);
                if let Av::Enum(n, 1, _) = &v
                    && &**n == "Result"
                {
                    self.code.push(Node::Br(self.trap));
                } else {
                    let h = self.hole();
                    self.code.push(Node::Hole(h));
                    edges.push((self.ms.clone(), v, h));
                }
            }
            Flow::Div => {}
            Flow::Brk | Flow::Cont => {
                self.code = saved_code;
                return fail("break out of a function");
            }
        }
        let body_code = std::mem::replace(&mut self.code, saved_code);
        if edges.is_empty() {
            self.code.push(Node::Block(label, body_code));
            return Ok(Flow::Div);
        }
        if edges.len() == 1 && !early {
            self.code.extend(body_code);
            let (ms, v, _) = edges.pop().unwrap();
            self.ms = ms;
            return Ok(Flow::V(v));
        }
        self.code.push(Node::Block(label, body_code));
        let (ms, v) = self.join_edges(edges)?;
        self.ms = ms;
        Ok(Flow::V(v))
    }

    fn method(&mut self, recv: &Expr, name: &str, args: &[Expr], hint: Option<&RTy>) -> R<Flow> {
        // `s.method(..)`: runtime state methods.
        let rv = val!(self.expr(recv, None));
        let vals = match self.args(args, &[])? {
            Ok(v) => v,
            Err(f) => return Ok(f),
        };
        // Mutating methods on a local place (Tup::push, Tup::set).
        if matches!(name, "push" | "set")
            && let Av::Tup(_) = rv
        {
            let (nv, res) = self.tup_mut(rv, name, vals)?;
            self.store_place(recv, nv)?;
            return Ok(res);
        }
        // Struct methods defined in the program.
        let tname: Option<String> = match &rv {
            Av::Struct(n, _) | Av::Enum(n, _, _) => {
                if matches!(&**n, "Option" | "Result") {
                    None
                } else {
                    Some(n.to_string())
                }
            }
            Av::DEnum(n, _, _) if !matches!(&**n, "Option" | "Result") => Some(n.to_string()),
            Av::St => Some("St".into()),
            Av::Cfg => Some("Cfg".into()),
            _ => None,
        };
        if let Some(t) = &tname {
            let key = format!("{t}::{name}");
            if self.is_intrinsic(&key) {
                let mut all = vec![rv];
                all.extend(vals);
                return self.intrinsic(&key, all, hint);
            }
            if let Some(full) = self.ty.full_path(t).map(|p| format!("{p}::{name}"))
                && let Some(Item::Fn(fd)) = self.prog.items.get(&full)
            {
                let fd = fd.clone();
                return self.call_fn(&fd, vals, Some(rv));
            }
        }
        self.prim_method(rv, name, vals, hint)
    }

    pub fn default_of(&mut self, t: &RTy) -> R<Av> {
        Ok(match t {
            RTy::Int(it) => Av::Int(0, *it),
            RTy::Bool => Av::Bool(false),
            RTy::F64 => Av::F64(0.0),
            RTy::F32 => Av::F32(0.0),
            RTy::Unit => Av::Unit,
            RTy::Tuple(ts) => {
                let mut v = Vec::new();
                for t in ts {
                    v.push(self.default_of(t)?);
                }
                Av::Tuple(Rc::new(v))
            }
            RTy::Ref(inner) => match &**inner {
                RTy::Named(n, _) if &**n == "Fields" => self.cx.fields_none(),
                RTy::Named(n, _) if &**n == "Insn" => self.cx.insn_none(),
                other => self.default_of(other)?,
            },
            RTy::Named(n, args) => match &**n {
                "Option" => none(),
                "Tup" => Av::Tup(Rc::new(vec![])),
                _ => {
                    if let Some(full) = self.ty.full_path(n).map(|p| format!("{p}::default"))
                        && let Some(Item::Fn(fd)) = self.prog.items.get(&full)
                    {
                        let fd = fd.clone();
                        return match self.call_fn(&fd, vec![], None)? {
                            Flow::V(v) => Ok(v),
                            _ => fail("default diverges"),
                        };
                    }
                    let Some(fields) = self.ty.struct_fields(n).cloned() else {
                        return fail(format!("no default for {n}{args:?}"));
                    };
                    let mut v = Vec::new();
                    for (_, ft) in &fields {
                        let rt = self.ty.resolve(ft);
                        v.push(self.default_of(&rt)?);
                    }
                    Av::Struct(n.clone(), Rc::new(v))
                }
            },
            RTy::Array(_) => Av::Arr(Rc::new(vec![])),
            _ => return fail(format!("no default for {t:?}")),
        })
    }

    // ------------------------------------------------------------- control

    fn if_(&mut self, c: Av, then: Rc<Block>, els: Option<Box<Expr>>, hint: Option<&RTy>) -> R<Flow> {
        let hint = hint.cloned();
        match c {
            Av::Bool(true) => self.block(&then, hint.as_ref()),
            Av::Bool(false) => match els {
                Some(e) => self.expr(&e, hint.as_ref()),
                None => Ok(Flow::V(Av::Unit)),
            },
            Av::D(_) => {
                let h2 = hint.clone();
                self.fork(
                    &c,
                    move |pe| pe.block(&then, hint.as_ref()),
                    move |pe| match els {
                        Some(e) => pe.expr(&e, h2.as_ref()),
                        None => Ok(Flow::V(Av::Unit)),
                    },
                )
            }
            other => fail(format!("if on {other:?}")),
        }
    }

    /// `if C1 && let P = E && ... { then } else { els }`.
    fn if_chain(
        &mut self,
        conds: Rc<Vec<Expr>>,
        k: usize,
        then: Rc<Block>,
        els: Option<Box<Expr>>,
        hint: Option<&RTy>,
    ) -> R<Flow> {
        let hint = hint.cloned();
        if k == conds.len() {
            return self.block(&then, hint.as_ref());
        }
        let else_f = |pe: &mut Self, els: Option<Box<Expr>>, hint: Option<RTy>| -> R<Flow> {
            match els {
                Some(e) => pe.expr(&e, hint.as_ref()),
                None => Ok(Flow::V(Av::Unit)),
            }
        };
        let (c, binds) = match &conds[k] {
            Expr::Let(p, e) => {
                let v = val!(self.expr(e, None));
                match self.pat_test(p, &v)? {
                    PatRes::Yes(b) => (Av::Bool(true), b),
                    PatRes::No => (Av::Bool(false), vec![]),
                    PatRes::Dyn(c, b) => (c, b),
                }
            }
            e => (val!(self.expr(e, Some(&RTy::Bool))), vec![]),
        };
        match c {
            Av::Bool(true) => {
                self.env.push(binds);
                let r = self.if_chain(conds, k + 1, then, els, hint.as_ref());
                self.env.pop();
                r
            }
            Av::Bool(false) => else_f(self, els, hint),
            c => {
                let els2 = els.clone();
                let h2 = hint.clone();
                self.fork(
                    &c,
                    move |pe| {
                        pe.env.push(binds);
                        let r = pe.if_chain(conds, k + 1, then, els, hint.as_ref());
                        pe.env.pop();
                        r
                    },
                    move |pe| else_f(pe, els2, h2),
                )
            }
        }
    }

    /// Run A where C holds and B where it does not, then join.
    pub fn fork(
        &mut self,
        c: &Av,
        a: impl FnOnce(&mut Self) -> R<Flow>,
        b: impl FnOnce(&mut Self) -> R<Flow>,
    ) -> R<Flow> {
        let c = match c {
            Av::Bool(true) => return a(self),
            Av::Bool(false) => return b(self),
            Av::D(d) => d.clone(),
            _ => return fail("fork on a non-boolean"),
        };
        let outer = std::mem::take(&mut self.code);
        let env0 = self.env.clone();
        let ms0 = self.ms.clone();
        let fact = self.facts.get(&c.l).cloned();
        if let Some(f) = &fact {
            self.assume(f, true);
        }
        let ra = a(self);
        let code_a = std::mem::take(&mut self.code);
        let env_a = std::mem::replace(&mut self.env, env0);
        let ms_a = std::mem::replace(&mut self.ms, ms0);
        let ra = match ra {
            Ok(r) => r,
            Err(e) => {
                self.code = outer;
                return Err(e);
            }
        };
        if let Some(f) = &fact {
            self.assume(f, false);
        }
        let rb = b(self);
        let code_b = std::mem::take(&mut self.code);
        self.code = outer;
        let rb = rb?;
        let env_b = std::mem::take(&mut self.env);
        let ms_b = std::mem::take(&mut self.ms);
        self.get_dv(&c);
        match (ra, rb) {
            (Flow::V(va), Flow::V(vb)) => {
                let ha = self.hole();
                let hb = self.hole();
                let mut ca = code_a;
                let mut cb = code_b;
                ca.push(Node::Hole(ha));
                cb.push(Node::Hole(hb));
                // Both arms pure and short: still an if (Cranelift makes
                // selects where it pays).
                self.code.push(Node::If(ca, cb));
                let hs = [ha, hb];
                let env = self.join_env(&[env_a, env_b], &hs)?;
                let ms = self.join_ms(&[ms_a, ms_b], &hs)?;
                let v = self.join_n(&[va, vb], &hs)?;
                self.env = env;
                self.ms = ms;
                Ok(Flow::V(v))
            }
            (Flow::V(va), other) => {
                self.code.push(Node::If(code_a, code_b));
                self.env = env_a;
                self.ms = ms_a;
                if matches!(other, Flow::Div) {
                    Ok(Flow::V(va))
                } else {
                    fail("break/continue under a run-time condition")
                }
            }
            (other, Flow::V(vb)) => {
                self.code.push(Node::If(code_a, code_b));
                self.env = env_b;
                self.ms = ms_b;
                if matches!(other, Flow::Div) {
                    Ok(Flow::V(vb))
                } else {
                    fail("break/continue under a run-time condition")
                }
            }
            (Flow::Div, Flow::Div) => {
                self.code.push(Node::If(code_a, code_b));
                self.env = env_a;
                self.ms = ms_a;
                Ok(Flow::Div)
            }
            _ => fail("break/continue under a run-time condition"),
        }
    }

    /// Narrow the ranges of the integer locals FACT constrains, where it
    /// is TRUTH.
    pub fn assume(&mut self, f: &Fact, truth: bool) {
        match f {
            Fact::Cmp(x, op, k) => {
                let op = if truth { *op } else { negate(op) };
                let k = *k;
                let (lo, hi) = match op {
                    "==" => (k, k),
                    "<" => (i128::MIN, k.saturating_sub(1)),
                    "<=" => (i128::MIN, k),
                    ">" => (k.saturating_add(1), i128::MAX),
                    ">=" => (k, i128::MAX),
                    _ => {
                        // != : only a bound equal to K moves.
                        self.refine_local(*x, i128::MIN, i128::MAX, Some(k));
                        return;
                    }
                };
                self.refine_local(*x, lo, hi, None);
            }
            Fact::And(a, b) if truth => {
                for x in [a, b].into_iter().flatten() {
                    self.assume(x, true);
                }
            }
            Fact::Or(a, b) if !truth => {
                for x in [a, b].into_iter().flatten() {
                    self.assume(x, false);
                }
            }
            Fact::Not(x) => self.assume(x, !truth),
            _ => {}
        }
    }

    fn refine_local(&mut self, x: u32, lo: i128, hi: i128, not: Option<i128>) {
        let mut env = std::mem::take(&mut self.env);
        for scope in env.iter_mut() {
            for (_, v) in scope.iter_mut() {
                refine_av(v, x, lo, hi, not);
            }
        }
        self.env = env;
        let mut ms = std::mem::take(&mut self.ms);
        for v in ms.r.iter_mut().chain(ms.old.iter_mut()) {
            refine_av(v, x, lo, hi, not);
        }
        refine_av(&mut ms.pc, x, lo, hi, not);
        refine_av(&mut ms.pending, x, lo, hi, not);
        self.ms = ms;
    }

    fn match_arms(&mut self, sv: &Av, arms: &[Arm], k: usize, hint: Option<&RTy>) -> R<Flow> {
        if k >= arms.len() {
            // No arm matched: a Rust match is exhaustive, so this is
            // unreachable at run time.
            self.code.push(Node::Br(self.trap));
            return Ok(Flow::Div);
        }
        let arm = &arms[k];
        let r = self.pat_test(&arm.pat, sv)?;
        let (cond, binds) = match r {
            PatRes::No => return self.match_arms(sv, arms, k + 1, hint),
            PatRes::Yes(b) => (Av::Bool(true), b),
            PatRes::Dyn(c, b) => (c, b),
        };
        let cond = match &arm.guard {
            None => cond,
            Some(g) => {
                self.env.push(binds.clone());
                let gv = self.expr(g, Some(&RTy::Bool));
                self.env.pop();
                let gv = match gv? {
                    Flow::V(v) => v,
                    f => return Ok(f),
                };
                self.logic_and(cond, gv)?
            }
        };
        let hint = hint.cloned();
        let body = arm.body.clone();
        match cond {
            Av::Bool(true) => {
                self.env.push(binds);
                let r = self.expr(&body, hint.as_ref());
                self.env.pop();
                r
            }
            Av::Bool(false) => self.match_arms(sv, arms, k + 1, hint.as_ref()),
            _ => {
                let sv2 = sv.clone();
                let arms2: Vec<Arm> = arms.to_vec();
                let h2 = hint.clone();
                self.fork(
                    &cond,
                    move |pe| {
                        pe.env.push(binds);
                        let r = pe.expr(&body, hint.as_ref());
                        pe.env.pop();
                        r
                    },
                    move |pe| pe.match_arms(&sv2, &arms2, k + 1, h2.as_ref()),
                )
            }
        }
    }

    fn for_(&mut self, pat: &Pat, it: &Expr, body: &Rc<Block>) -> R<Flow> {
        let iv = val!(self.expr(it, None));
        let items: Vec<Av> = match &iv {
            Av::Range(a, b) => (*a..*b).map(|x| Av::Int(x, IT::I128)).collect(),
            Av::Arr(v) | Av::Tup(v) => (**v).clone(),
            Av::Struct(n, f) if &**n == "PyRange" => {
                let (Some(mut cur), Some(stop), Some(step)) = (f[0].as_int(), f[1].as_int(), f[2].as_int()) else {
                    return fail("dynamic py_range");
                };
                let mut v = Vec::new();
                while (step > 0 && cur < stop) || (step < 0 && cur > stop) {
                    v.push(Av::Int(cur, IT::I128));
                    cur += step;
                    if v.len() > 4096 {
                        return fail("py_range too long");
                    }
                }
                v
            }
            other => return fail(format!("for over {other:?}")),
        };
        // Range items take the type of the bounds as used; keep i128/usize
        // literal-compatible.
        for x in items {
            self.env.push(Vec::new());
            self.bind_pat(pat, x)?;
            let r = self.block(body, None);
            self.env.pop();
            match r? {
                Flow::V(_) | Flow::Cont => {}
                Flow::Brk => break,
                Flow::Div => return Ok(Flow::Div),
            }
        }
        Ok(Flow::V(Av::Unit))
    }

    fn loop_(&mut self, b: &Rc<Block>) -> R<Flow> {
        for _ in 0..4096 {
            match self.block(b, None)? {
                Flow::V(_) | Flow::Cont => {}
                Flow::Brk => return Ok(Flow::V(Av::Unit)),
                Flow::Div => return Ok(Flow::Div),
            }
        }
        fail("loop does not end at translation time")
    }

    fn while_(&mut self, c: &Expr, b: &Rc<Block>) -> R<Flow> {
        for _ in 0..4096 {
            let cv = val!(self.expr(c, Some(&RTy::Bool)));
            match cv {
                Av::Bool(true) => {}
                Av::Bool(false) => return Ok(Flow::V(Av::Unit)),
                _ => return fail("while on a run-time condition"),
            }
            match self.block(b, None)? {
                Flow::V(_) | Flow::Cont => {}
                Flow::Brk => return Ok(Flow::V(Av::Unit)),
                Flow::Div => return Ok(Flow::Div),
            }
        }
        fail("while does not end at translation time")
    }

    // ------------------------------------------------------------- patterns

    pub fn pat_test(&mut self, p: &Pat, v: &Av) -> R<PatRes> {
        match p {
            Pat::Wild => Ok(PatRes::Yes(vec![])),
            Pat::Bind(n) => Ok(PatRes::Yes(vec![(n.as_str().into(), v.clone())])),
            Pat::Tuple(ps) => {
                let Some(items) = v.items().cloned() else {
                    return fail(format!("tuple pattern on {v:?}"));
                };
                self.pats_all(ps, &items)
            }
            Pat::Or(alts) => {
                let mut conds = Vec::new();
                for a in alts {
                    match self.pat_test(a, v)? {
                        PatRes::Yes(b) => {
                            if conds.is_empty() {
                                return Ok(PatRes::Yes(b));
                            }
                            conds.push(Av::Bool(true));
                            break;
                        }
                        PatRes::No => {}
                        PatRes::Dyn(c, b) => {
                            if !b.is_empty() {
                                return fail("binding in an or-pattern");
                            }
                            conds.push(c);
                        }
                    }
                }
                if conds.is_empty() {
                    return Ok(PatRes::No);
                }
                let mut c = conds[0].clone();
                for x in &conds[1..] {
                    c = self.logic_or(c, x.clone())?;
                }
                Ok(match c {
                    Av::Bool(true) => PatRes::Yes(vec![]),
                    Av::Bool(false) => PatRes::No,
                    c => PatRes::Dyn(c, vec![]),
                })
            }
            Pat::Lit(e) => {
                let lit = match self.expr(e, None)? {
                    Flow::V(x) => x,
                    _ => return fail("pattern literal"),
                };
                self.eq_test(v, lit)
            }
            Pat::Range(lo, hi, incl) => {
                let lo = match self.expr(lo, None)? {
                    Flow::V(x) => x,
                    _ => return fail("pattern range"),
                };
                let hi = match self.expr(hi, None)? {
                    Flow::V(x) => x,
                    _ => return fail("pattern range"),
                };
                let a = self.binop(">=", v.clone(), lo)?;
                let b = self.binop(if *incl { "<=" } else { "<" }, v.clone(), hi)?;
                let c = self.logic_and(a, b)?;
                Ok(match c {
                    Av::Bool(true) => PatRes::Yes(vec![]),
                    Av::Bool(false) => PatRes::No,
                    c => PatRes::Dyn(c, vec![]),
                })
            }
            Pat::Path(path) => {
                if path == "None" {
                    return self.variant_test(v, "Option", 0, &[]);
                }
                // A constant, or a unit variant.
                if let Some((e, var)) = path.rsplit_once("::") {
                    let ename = e.rsplit("::").next().unwrap();
                    if self.ty.variants(ename).is_some()
                        && let Some(k) = self.ty.variant_index(ename, var)
                    {
                        return self.variant_test(v, ename, k, &[]);
                    }
                }
                let c = self.path(path, None)?;
                self.eq_test(v, c)
            }
            Pat::TupleStruct(path, subs) => {
                let (ename, k): (String, u32) = match path.as_str() {
                    "Some" => ("Option".into(), 1),
                    "Ok" => ("Result".into(), 0),
                    "Err" => ("Result".into(), 1),
                    _ => {
                        let (e, var) = path.rsplit_once("::").unwrap_or(("", path));
                        let ename = e.rsplit("::").next().unwrap_or("");
                        match self.ty.variant_index(ename, var) {
                            Some(k) if !ename.is_empty() => (ename.to_string(), k),
                            _ => {
                                // A tuple struct pattern.
                                let Some(items) = v.items().cloned() else {
                                    return fail(format!("pattern {path} on {v:?}"));
                                };
                                return self.pats_all(subs, &items);
                            }
                        }
                    }
                };
                self.variant_test(v, &ename, k, subs)
            }
            Pat::Struct(..) => fail("struct pattern"),
        }
    }

    fn pats_all(&mut self, ps: &[Pat], items: &[Av]) -> R<PatRes> {
        if ps.len() != items.len() {
            return fail("pattern arity");
        }
        let mut binds = Vec::new();
        let mut cond = Av::Bool(true);
        for (p, x) in ps.iter().zip(items) {
            match self.pat_test(p, x)? {
                PatRes::No => return Ok(PatRes::No),
                PatRes::Yes(b) => binds.extend(b),
                PatRes::Dyn(c, b) => {
                    binds.extend(b);
                    cond = self.logic_and(cond, c)?;
                }
            }
        }
        Ok(match cond {
            Av::Bool(true) => PatRes::Yes(binds),
            Av::Bool(false) => PatRes::No,
            c => PatRes::Dyn(c, binds),
        })
    }

    fn variant_test(&mut self, v: &Av, ename: &str, k: u32, subs: &[Pat]) -> R<PatRes> {
        match v {
            Av::Enum(n, var, payload) => {
                if &**n != ename {
                    return fail(format!("variant pattern {ename} on {n}"));
                }
                if *var != k {
                    return Ok(PatRes::No);
                }
                if subs.is_empty() {
                    return Ok(PatRes::Yes(vec![]));
                }
                self.pats_all(subs, payload)
            }
            Av::DEnum(n, d, payloads) => {
                if &**n != ename {
                    return fail(format!("variant pattern {ename} on {n}"));
                }
                let d = (**d).clone();
                if let Some(vs) = &d.vset
                    && !vs.contains(&(k as i128))
                {
                    return Ok(PatRes::No);
                }
                let c = self.binop("==", Av::D(d), Av::Int(k as i128, IT::U32))?;
                if subs.is_empty() {
                    return Ok(PatRes::Dyn(c, vec![]));
                }
                let payload = payloads[k as usize].clone();
                match self.pats_all(subs, &payload)? {
                    PatRes::No => Ok(PatRes::No),
                    PatRes::Yes(b) => Ok(PatRes::Dyn(c, b)),
                    PatRes::Dyn(c2, b) => {
                        // The payload test may read an absent payload when
                        // the variant differs; it is pure, so compute it
                        // anyway and combine.
                        let c = self.logic_and(c, c2)?;
                        Ok(PatRes::Dyn(c, b))
                    }
                }
            }
            _ => fail(format!("variant pattern {ename} on {v:?}")),
        }
    }

    fn eq_test(&mut self, v: &Av, c: Av) -> R<PatRes> {
        let r = self.binop("==", v.clone(), c)?;
        Ok(match r {
            Av::Bool(true) => PatRes::Yes(vec![]),
            Av::Bool(false) => PatRes::No,
            c => PatRes::Dyn(c, vec![]),
        })
    }

    // ---------------------------------------------------------------- joins

    pub fn join_edges(&mut self, edges: Vec<(MState, Av, Hole)>) -> R<(MState, Av)> {
        let hs: Vec<Hole> = edges.iter().map(|e| e.2).collect();
        let mut mss = Vec::new();
        let mut vs = Vec::new();
        for (m, v, _) in edges {
            mss.push(m);
            vs.push(v);
        }
        let ms = self.join_ms(&mss, &hs)?;
        let v = self.join_n(&vs, &hs)?;
        Ok((ms, v))
    }

    fn join_env(&mut self, envs: &[Vec<Vec<(Rc<str>, Av)>>], hs: &[Hole]) -> R<Vec<Vec<(Rc<str>, Av)>>> {
        let base = &envs[0];
        let mut out = Vec::new();
        for (si, scope) in base.iter().enumerate() {
            let mut ns = Vec::new();
            for (vi, (name, _)) in scope.iter().enumerate() {
                let vals: Vec<Av> = envs.iter().map(|e| e[si][vi].1.clone()).collect();
                ns.push((name.clone(), self.join_n(&vals, hs)?));
            }
            out.push(ns);
        }
        Ok(out)
    }

    pub fn join_ms(&mut self, ms: &[MState], hs: &[Hole]) -> R<MState> {
        let mut out = ms[0].clone();
        for c in 0..out.r.len() {
            let vals: Vec<Av> = ms.iter().map(|m| m.r[c].clone()).collect();
            if vals.iter().any(|v| matches!(v, Av::Undef)) && !vals.iter().all(|v| matches!(v, Av::Undef)) {
                // Unmodified on some paths: their value is the entry one.
                let entry = self.entry_reg(c as u32)?;
                let vals: Vec<Av> = vals.into_iter().map(|v| if matches!(v, Av::Undef) { entry.clone() } else { v }).collect();
                out.r[c] = self.join_n(&vals, hs)?;
            } else {
                out.r[c] = self.join_n(&vals, hs)?;
            }
            let vals: Vec<Av> = ms.iter().map(|m| m.old[c].clone()).collect();
            if vals.iter().any(|v| matches!(v, Av::Undef)) && !vals.iter().all(|v| matches!(v, Av::Undef)) {
                let entry = self.entry_reg(c as u32)?;
                let vals: Vec<Av> = vals.into_iter().map(|v| if matches!(v, Av::Undef) { entry.clone() } else { v }).collect();
                out.old[c] = self.join_n(&vals, hs)?;
            } else {
                out.old[c] = self.join_n(&vals, hs)?;
            }
        }
        let pcs: Vec<Av> = ms.iter().map(|m| m.pc.clone()).collect();
        out.pc = self.join_n(&pcs, hs)?;
        let ps: Vec<Av> = ms.iter().map(|m| m.pending.clone()).collect();
        out.pending = self.join_n(&ps, hs)?;
        out.logged = ms.iter().any(|m| m.logged);
        Ok(out)
    }

    /// The join of VALS, one per incoming edge; values that differ are
    /// materialized into fresh locals in each edge's hole.
    pub fn join_n(&mut self, vals: &[Av], hs: &[Hole]) -> R<Av> {
        let first = &vals[0];
        if vals[1..].iter().all(|v| v == first) {
            return Ok(first.clone());
        }
        let defined: Vec<&Av> = vals.iter().filter(|v| !matches!(v, Av::Undef)).collect();
        if defined.is_empty() {
            return Ok(Av::Undef);
        }
        let d0 = defined[0];
        match d0 {
            Av::Struct(n, items) => {
                let mut out = Vec::new();
                for k in 0..items.len() {
                    let col: Vec<Av> = vals
                        .iter()
                        .map(|v| match v {
                            Av::Struct(m, it) if m == n => it[k].clone(),
                            _ => Av::Undef,
                        })
                        .collect();
                    if vals.iter().any(|v| !matches!(v, Av::Struct(m, _) if m == n) && !matches!(v, Av::Undef)) {
                        return fail(format!("join of {n} with another kind"));
                    }
                    out.push(self.join_n(&col, hs)?);
                }
                Ok(Av::Struct(n.clone(), Rc::new(out)))
            }
            Av::Tuple(items) | Av::Tup(items) | Av::Arr(items) => {
                let len = items.len();
                let mut out = Vec::new();
                for k in 0..len {
                    let mut col = Vec::new();
                    for v in vals {
                        match v {
                            Av::Tuple(it) | Av::Tup(it) | Av::Arr(it) if it.len() == len => col.push(it[k].clone()),
                            Av::Undef => col.push(Av::Undef),
                            _ => return fail("join of sequences of different shapes"),
                        }
                    }
                    out.push(self.join_n(&col, hs)?);
                }
                Ok(match d0 {
                    Av::Tuple(_) => Av::Tuple(Rc::new(out)),
                    Av::Tup(_) => Av::Tup(Rc::new(out)),
                    _ => Av::Arr(Rc::new(out)),
                })
            }
            Av::Enum(n, _, _) | Av::DEnum(n, _, _) => self.join_enum(n.clone(), vals, hs),
            Av::Int(..) | Av::Bool(_) | Av::F64(_) | Av::F32(_) | Av::D(_) => self.join_scalar(vals, hs),
            Av::Unit | Av::St | Av::Cfg => Ok(d0.clone()),
            _ => fail(format!("join of {d0:?}")),
        }
    }

    fn join_enum(&mut self, n: Rc<str>, vals: &[Av], hs: &[Hole]) -> R<Av> {
        // Same known variant everywhere: join the payloads.
        let mut var: Option<u32> = None;
        let mut same = true;
        for v in vals {
            match v {
                Av::Enum(m, k, _) if *m == n => {
                    if var.is_some_and(|x| x != *k) {
                        same = false;
                    }
                    var = Some(*k);
                }
                Av::Undef => {}
                _ => same = false,
            }
        }
        if same && let Some(k) = var {
            let len = vals
                .iter()
                .find_map(|v| match v {
                    Av::Enum(_, _, p) => Some(p.len()),
                    _ => None,
                })
                .unwrap_or(0);
            let mut out = Vec::new();
            for i in 0..len {
                let col: Vec<Av> = vals
                    .iter()
                    .map(|v| match v {
                        Av::Enum(_, _, p) => p[i].clone(),
                        _ => Av::Undef,
                    })
                    .collect();
                out.push(self.join_n(&col, hs)?);
            }
            return Ok(Av::Enum(n, k, Rc::new(out)));
        }
        // Different variants: a run-time discriminant.
        let nvars = self.enum_arity(&n)?;
        let mut vset: Vec<i128> = Vec::new();
        let mut discs = Vec::new();
        for v in vals {
            match v {
                Av::Enum(_, k, _) => {
                    discs.push(Av::Int(*k as i128, IT::U32));
                    if !vset.contains(&(*k as i128)) {
                        vset.push(*k as i128);
                    }
                }
                Av::DEnum(_, d, _) => {
                    match &d.vset {
                        Some(vs) => {
                            for x in vs.iter() {
                                if !vset.contains(x) {
                                    vset.push(*x);
                                }
                            }
                        }
                        None => {
                            for x in 0..nvars as i128 {
                                if !vset.contains(&x) {
                                    vset.push(x);
                                }
                            }
                        }
                    }
                    discs.push(Av::D((**d).clone()));
                }
                _ => discs.push(Av::Undef),
            }
        }
        vset.sort();
        let disc = match self.join_scalar(&discs, hs)? {
            Av::D(mut d) => {
                d.vset = Some(Rc::new(vset));
                d
            }
            other => return fail(format!("enum discriminant join {other:?}")),
        };
        let mut payloads = Vec::new();
        for k in 0..nvars {
            let width = self.variant_width(&n, k)?;
            let mut out = Vec::new();
            for i in 0..width {
                let col: Vec<Av> = vals
                    .iter()
                    .map(|v| match v {
                        Av::Enum(_, kk, p) if *kk == k => p[i].clone(),
                        Av::DEnum(_, _, ps) => ps[k as usize].get(i).cloned().unwrap_or(Av::Undef),
                        _ => Av::Undef,
                    })
                    .collect();
                out.push(self.join_n(&col, hs)?);
            }
            payloads.push(out);
        }
        Ok(Av::DEnum(n, Box::new(disc), Rc::new(payloads)))
    }

    fn enum_arity(&self, n: &str) -> R<u32> {
        match n {
            "Option" | "Result" => Ok(2),
            _ => self.ty.variants(n).map(|v| v.len() as u32).ok_or_else(|| Fail(format!("enum {n}"))),
        }
    }

    fn variant_width(&self, n: &str, k: u32) -> R<usize> {
        match n {
            "Option" => Ok(if k == 1 { 1 } else { 0 }),
            "Result" => Ok(1),
            _ => self
                .ty
                .variants(n)
                .and_then(|v| v.get(k as usize))
                .map(|(_, t)| t.len())
                .ok_or_else(|| Fail(format!("variant {n}#{k}"))),
        }
    }

    fn join_scalar(&mut self, vals: &[Av], hs: &[Hole]) -> R<Av> {
        // The kind of the joined value.
        let mut kind: Option<Kind> = None;
        let mut lo = i128::MAX;
        let mut hi = i128::MIN;
        let mut vset: Option<Vec<i128>> = Some(Vec::new());
        for v in vals {
            let k = match v {
                Av::Undef => continue,
                Av::Int(x, it) => {
                    lo = lo.min(*x);
                    hi = hi.max(*x);
                    if let Some(s) = &mut vset
                        && !s.contains(x)
                    {
                        s.push(*x);
                    }
                    Kind::Int(*it, Rep::I32)
                }
                Av::Bool(b) => {
                    lo = lo.min(*b as i128);
                    hi = hi.max(*b as i128);
                    Kind::Bool
                }
                Av::F64(_) => Kind::F64,
                Av::F32(_) => Kind::F32,
                Av::D(d) => {
                    lo = lo.min(d.lo);
                    hi = hi.max(d.hi);
                    match (&mut vset, &d.vset) {
                        (Some(s), Some(dv)) => {
                            for x in dv.iter() {
                                if !s.contains(x) {
                                    s.push(*x);
                                }
                            }
                        }
                        _ => vset = None,
                    }
                    d.k
                }
                _ => return fail(format!("scalar join with {v:?}")),
            };
            kind = Some(match (kind, k) {
                (None, k) => k,
                (Some(Kind::Int(a, _)), Kind::Int(b, _)) => Kind::Int(if a == IT::Lit { b } else { a }, Rep::I32),
                (Some(a), b) if std::mem::discriminant(&a) == std::mem::discriminant(&b) => a,
                (Some(a), b) => return fail(format!("join of {a:?} and {b:?}")),
            });
        }
        let kind = match kind.unwrap() {
            Kind::Int(IT::Lit, _) => Kind::Int(IT::I128, IT::I128.rep((lo, hi))),
            Kind::Int(it, _) => Kind::Int(it, it.rep((lo, hi))),
            k => k,
        };
        if let Kind::Int(_, Rep::Wide) = kind {
            let l = self.local(W::I64);
            let h = self.local(W::I64);
            let d = Dv {
                l,
                l2: h,
                k: kind,
                lo,
                hi,
                vset: None,
            };
            for (v, hole) in vals.iter().zip(hs) {
                if matches!(v, Av::Undef) {
                    continue;
                }
                let v = v.clone();
                self.in_hole(*hole, |pe| pe.wstore(&v, l, h))?;
            }
            return Ok(Av::D(d));
        }
        let w = kind.w();
        let l = self.local(w);
        let d = Dv {
            l,
            l2: l,
            k: kind,
            lo,
            hi,
            vset: vset.filter(|s| s.len() <= 64).map(|mut s| {
                s.sort();
                Rc::new(s)
            }),
        };
        for (v, h) in vals.iter().zip(hs) {
            if matches!(v, Av::Undef) {
                continue;
            }
            let v = v.clone();
            let d2 = d.clone();
            self.in_hole(*h, |pe| -> R<()> {
                pe.push_as(&v, d2.k)?;
                pe.emit(I::LocalSet(d2.l));
                Ok(())
            })?;
        }
        Ok(Av::D(d))
    }

    // ------------------------------------------------------------ booleans

    pub fn logic_and(&mut self, a: Av, b: Av) -> R<Av> {
        match (&a, &b) {
            (Av::Bool(false), _) | (_, Av::Bool(false)) => Ok(Av::Bool(false)),
            (Av::Bool(true), _) => Ok(b),
            (_, Av::Bool(true)) => Ok(a),
            _ => self.binop("&", a, b),
        }
    }

    pub fn logic_or(&mut self, a: Av, b: Av) -> R<Av> {
        match (&a, &b) {
            (Av::Bool(true), _) | (_, Av::Bool(true)) => Ok(Av::Bool(true)),
            (Av::Bool(false), _) => Ok(b),
            (_, Av::Bool(false)) => Ok(a),
            _ => self.binop("|", a, b),
        }
    }

    fn binary(&mut self, op: &'static str, a: &Expr, b: &Expr, hint: Option<&RTy>) -> R<Flow> {
        if op == "&&" || op == "||" {
            let av = val!(self.expr(a, Some(&RTy::Bool)));
            match (&av, op) {
                (Av::Bool(false), "&&") => return Ok(Flow::V(Av::Bool(false))),
                (Av::Bool(true), "||") => return Ok(Flow::V(Av::Bool(true))),
                (Av::Bool(_), _) => return self.expr(b, Some(&RTy::Bool)),
                _ => {}
            }
            // Evaluate B where it would run; if that is pure, use it
            // unconditionally.
            let outer = std::mem::take(&mut self.code);
            let ms0 = self.ms.clone();
            let rb = self.expr(b, Some(&RTy::Bool));
            let code_b = std::mem::replace(&mut self.code, outer);
            let rb = rb?;
            if let Flow::V(bv) = &rb
                && super::ir::is_pure(&code_b)
                && self.ms_eq(&ms0)
            {
                self.code.extend(code_b);
                let bv = bv.clone();
                return if op == "&&" { self.logic_and(av, bv) } else { self.logic_or(av, bv) }.map(Flow::V);
            }
            // Not pure: a real branch (re-evaluate B inside it).
            self.ms = ms0;
            let b2 = b.clone();
            return if op == "&&" {
                self.fork(&av, move |pe| pe.expr(&b2, Some(&RTy::Bool)), |_| Ok(Flow::V(Av::Bool(false))))
            } else {
                self.fork(&av, |_| Ok(Flow::V(Av::Bool(true))), move |pe| pe.expr(&b2, Some(&RTy::Bool)))
            };
        }
        let is_shift = op == "<<" || op == ">>";
        let av = val!(self.expr(a, if is_shift || is_cmp(op) { None } else { hint }));
        // The right operand of a shift has its own type.
        let bhint = if is_shift { None } else { type_hint_of(&av).or_else(|| if is_cmp(op) { None } else { hint.cloned() }) };
        let bv = val!(self.expr(b, bhint.as_ref()));
        let av = if let (Av::Int(x, IT::Lit), Some(t)) = (&av, type_hint_of(&bv)) {
            if is_shift { av } else { self.coerce_static(Av::Int(*x, IT::Lit), &t) }
        } else {
            av
        };
        self.binop(op, av, bv).map(Flow::V)
    }

    fn ms_eq(&self, m: &MState) -> bool {
        self.ms.pc == m.pc && self.ms.pending == m.pending && self.ms.r == m.r && self.ms.old == m.old
    }
}

fn negate(op: &str) -> &'static str {
    match op {
        "==" => "!=",
        "!=" => "==",
        "<" => ">=",
        ">=" => "<",
        ">" => "<=",
        _ => ">",
    }
}

fn mentions(v: &Av, x: u32) -> bool {
    match v {
        Av::D(d) => d.l == x,
        Av::DEnum(_, d, ps) => d.l == x || ps.iter().any(|p| p.iter().any(|y| mentions(y, x))),
        _ => v.items().is_some_and(|it| it.iter().any(|y| mentions(y, x))),
    }
}

/// Narrow local X's range to [LO, HI] (and away from NOT at a bound) in V.
fn refine_av(v: &mut Av, x: u32, lo: i128, hi: i128, not: Option<i128>) {
    if !mentions(v, x) {
        return;
    }
    match v {
        Av::D(d) => {
            let mut nlo = d.lo.max(lo);
            let mut nhi = d.hi.min(hi);
            if let Some(k) = not {
                if nlo == k {
                    nlo += 1;
                }
                if nhi == k {
                    nhi -= 1;
                }
            }
            if nlo > nhi {
                // An impossible path: keep what was known.
                return;
            }
            let vset = d.vset.as_ref().map(|s| Rc::new(s.iter().copied().filter(|y| *y >= nlo && *y <= nhi && Some(*y) != not).collect::<Vec<_>>()));
            if nlo == nhi
                && let Kind::Int(it, _) = d.k
            {
                *v = Av::Int(nlo, it);
                return;
            }
            d.lo = nlo;
            d.hi = nhi;
            d.vset = vset;
        }
        Av::DEnum(_, _, ps) => {
            for p in Rc::make_mut(ps).iter_mut() {
                for y in p.iter_mut() {
                    refine_av(y, x, lo, hi, not);
                }
            }
        }
        Av::Tuple(it) | Av::Struct(_, it) | Av::Enum(_, _, it) | Av::Arr(it) | Av::Tup(it) => {
            for y in Rc::make_mut(it).iter_mut() {
                refine_av(y, x, lo, hi, not);
            }
        }
        _ => {}
    }
}

fn has_let(e: &Expr) -> bool {
    match e {
        Expr::Let(..) => true,
        Expr::Binary("&&", a, b) => has_let(a) || has_let(b),
        _ => false,
    }
}

fn conjuncts(e: &Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::Binary("&&", a, b) => {
            conjuncts(a, out);
            conjuncts(b, out);
        }
        e => out.push(e.clone()),
    }
}

fn is_cmp(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=")
}

pub fn type_hint_of(v: &Av) -> Option<RTy> {
    match v {
        Av::Int(_, it) if *it != IT::Lit => Some(RTy::Int(*it)),
        Av::D(d) => match d.k {
            Kind::Int(it, _) => Some(RTy::Int(it)),
            Kind::Bool => Some(RTy::Bool),
            Kind::F64 => Some(RTy::F64),
            Kind::F32 => Some(RTy::F32),
        },
        Av::F64(_) => Some(RTy::F64),
        Av::F32(_) => Some(RTy::F32),
        _ => None,
    }
}

pub fn strip_ref(t: &RTy) -> &RTy {
    match t {
        RTy::Ref(x) => strip_ref(x),
        _ => t,
    }
}

/// The module of item PATH (a method's is its type's module).
pub fn module_of(path: &str, method: bool) -> String {
    let mut parts: Vec<&str> = path.split("::").collect();
    parts.pop();
    // An associated function (`MR::new`) lives in its type's module.
    if method || parts.last().is_some_and(|p| p.chars().next().is_some_and(|c| c.is_uppercase())) {
        parts.pop();
    }
    parts.join("::")
}

pub enum PatRes {
    Yes(Vec<(Rc<str>, Av)>),
    No,
    Dyn(Av, Vec<(Rc<str>, Av)>),
}
