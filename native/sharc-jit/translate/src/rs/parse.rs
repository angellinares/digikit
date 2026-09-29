//! A recursive-descent parser for the Rust subset of the transpiled core
//! (tools/sharc_transpile.py: core_g.rs, tables.rs, syms.rs) and of the
//! runtime it calls (native/sharc/src/rt.rs, rt/bnd.rs). Items it does not
//! need (impl blocks of generic types, macros, uses other than globs) are
//! skipped; a function whose body falls outside the subset is dropped and
//! named in `Program::skipped`, so a later use of it is a clear error.

use super::ast::*;
use super::lex::{Tok, lex};
use std::collections::HashMap;
use std::rc::Rc;

/// Every parsed item by full path, plus the glob imports of each module.
#[derive(Default)]
pub struct Program {
    pub items: HashMap<String, Item>,
    pub globs: HashMap<String, Vec<String>>,
    pub skipped: Vec<(String, String)>,
}

pub struct Parser<'a> {
    t: Vec<Tok>,
    pos: &'a [usize],
    i: usize,
    no_struct: bool,
}

type PR<T> = Result<T, String>;

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.t[self.i]
    }
    fn peek_at(&self, k: usize) -> &Tok {
        self.t.get(self.i + k).unwrap_or(&Tok::Eof)
    }
    fn next(&mut self) -> Tok {
        let t = self.t[self.i].clone();
        if self.i + 1 < self.t.len() {
            self.i += 1;
        }
        t
    }
    fn err<T>(&self, msg: &str) -> PR<T> {
        Err(format!("{msg} at byte {} (token {:?})", self.pos[self.i], self.peek()))
    }
    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::P(x) if *x == p)
    }
    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if x == k)
    }
    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.next();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect_p(&mut self, p: &str) -> PR<()> {
        if self.eat_p(p) {
            Ok(())
        } else {
            self.err(&format!("expected {p:?}"))
        }
    }
    /// A closing `>` of generics, splitting `>>`/`>=`/`>>=`.
    fn expect_gt(&mut self) -> PR<()> {
        match self.peek().clone() {
            Tok::P(">") => {
                self.next();
                Ok(())
            }
            Tok::P(">>") => {
                self.t_replace(Tok::P(">"));
                Ok(())
            }
            Tok::P(">=") => {
                self.t_replace(Tok::P("="));
                Ok(())
            }
            Tok::P(">>=") => {
                self.t_replace(Tok::P(">="));
                Ok(())
            }
            _ => self.err("expected '>'"),
        }
    }
    fn t_replace(&mut self, tok: Tok) {
        self.t[self.i] = tok;
    }
    fn ident(&mut self) -> PR<String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            t => Err(format!("expected identifier, got {t:?} at byte {}", self.pos[self.i])),
        }
    }

    // ---------------------------------------------------------------- items

    fn skip_attrs(&mut self) -> PR<()> {
        while self.is_p("#") {
            self.next();
            self.eat_p("!");
            self.skip_group()?;
        }
        Ok(())
    }

    /// Skip one bracketed group starting at the current token.
    fn skip_group(&mut self) -> PR<()> {
        let open = match self.peek() {
            Tok::P(p) if matches!(*p, "(" | "[" | "{") => *p,
            _ => return self.err("expected a group"),
        };
        let close = match open {
            "(" => ")",
            "[" => "]",
            _ => "}",
        };
        let _ = close;
        let mut depth = 0i32;
        loop {
            match self.next() {
                Tok::P(p) if matches!(p, "(" | "[" | "{") => depth += 1,
                Tok::P(p) if matches!(p, ")" | "]" | "}") => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Tok::Eof => return self.err("unterminated group"),
                _ => {}
            }
        }
    }

    /// Skip to the end of the current item (a `;` at depth 0 or a
    /// top-level `{...}` group).
    fn skip_item(&mut self) -> PR<()> {
        loop {
            match self.peek() {
                Tok::P(";") => {
                    self.next();
                    return Ok(());
                }
                Tok::P("{") => {
                    self.skip_group()?;
                    self.eat_p(";");
                    return Ok(());
                }
                Tok::P("(") | Tok::P("[") => self.skip_group()?,
                Tok::Eof => return Ok(()),
                _ => {
                    self.next();
                }
            }
        }
    }

    fn items(&mut self, prog: &mut Program, module: &str, end_brace: bool) -> PR<()> {
        loop {
            self.skip_attrs()?;
            if end_brace && self.eat_p("}") {
                return Ok(());
            }
            if matches!(self.peek(), Tok::Eof) {
                return Ok(());
            }
            self.eat_kw("pub");
            if self.is_p("(") {
                self.skip_group()?; // pub(crate)
            }
            let kw = match self.peek() {
                Tok::Ident(s) => s.clone(),
                _ => {
                    self.skip_item()?;
                    continue;
                }
            };
            match kw.as_str() {
                "mod" => {
                    self.next();
                    let name = self.ident()?;
                    let sub = join(module, &name);
                    if self.eat_p(";") {
                        continue;
                    }
                    self.expect_p("{")?;
                    // A nested module sees its parent's items only by
                    // explicit paths (super::).
                    self.items(prog, &sub, true)?;
                }
                "use" => {
                    self.next();
                    let mut path = String::new();
                    let mut glob = false;
                    loop {
                        match self.next() {
                            Tok::P(";") => break,
                            Tok::P("*") => glob = true,
                            Tok::Ident(s) => path.push_str(&s),
                            Tok::P("::") => path.push_str("::"),
                            Tok::P("{") => {
                                // use a::{b, c}; not needed
                                self.i -= 1;
                                self.skip_group()?;
                            }
                            _ => {}
                        }
                    }
                    if glob {
                        let p = path.trim_end_matches("::");
                        let resolved = resolve_mod(module, p);
                        prog.globs.entry(module.to_string()).or_default().push(resolved);
                    }
                }
                "fn" | "unsafe" | "const" if kw != "const" || self.const_is_fn() => {
                    if kw != "fn" {
                        self.next();
                        if self.is_kw("extern") {
                            self.skip_item()?;
                            continue;
                        }
                    }
                    let start = self.i;
                    match self.fn_def(module, None) {
                        Ok(Some(f)) => {
                            prog.items.insert(f.path.clone(), Item::Fn(f));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            self.i = start;
                            self.next();
                            let name = self.ident().unwrap_or_default();
                            prog.skipped.push((join(module, &name), e));
                            self.i = start;
                            self.skip_item()?;
                        }
                    }
                }
                "const" | "static" => {
                    self.next();
                    self.eat_kw("mut");
                    let name = self.ident()?;
                    self.expect_p(":")?;
                    let ty = self.ty()?;
                    self.expect_p("=")?;
                    let start = self.i;
                    match self.expr() {
                        Ok(e) => {
                            self.expect_p(";")?;
                            prog.items.insert(join(module, &name), Item::Const(name, ty, e));
                        }
                        Err(e) => {
                            prog.skipped.push((join(module, &name), e));
                            self.i = start;
                            self.skip_item()?;
                        }
                    }
                }
                "struct" => {
                    self.next();
                    let name = self.ident()?;
                    if self.is_p("<") {
                        self.skip_item()?;
                        continue;
                    }
                    let mut fields = Vec::new();
                    let tuple = self.is_p("(");
                    if self.eat_p("(") {
                        let mut k = 0;
                        while !self.eat_p(")") {
                            self.skip_attrs()?;
                            self.eat_kw("pub");
                            let ty = self.ty()?;
                            fields.push((k.to_string(), ty));
                            k += 1;
                            self.eat_p(",");
                        }
                        self.expect_p(";")?;
                    } else if self.eat_p("{") {
                        while !self.eat_p("}") {
                            self.skip_attrs()?;
                            self.eat_kw("pub");
                            let f = self.ident()?;
                            self.expect_p(":")?;
                            let ty = self.ty()?;
                            fields.push((f, ty));
                            self.eat_p(",");
                        }
                    } else {
                        self.expect_p(";")?;
                    }
                    prog.items.insert(join(module, &name), Item::Struct(name, fields, tuple));
                }
                "enum" => {
                    self.next();
                    let name = self.ident()?;
                    if self.is_p("<") {
                        self.skip_item()?;
                        continue;
                    }
                    self.expect_p("{")?;
                    let mut vars = Vec::new();
                    while !self.eat_p("}") {
                        self.skip_attrs()?;
                        let v = self.ident()?;
                        let mut tys = Vec::new();
                        if self.eat_p("(") {
                            while !self.eat_p(")") {
                                tys.push(self.ty()?);
                                self.eat_p(",");
                            }
                        } else if self.is_p("{") {
                            self.skip_group()?;
                        }
                        vars.push((v, tys));
                        self.eat_p(",");
                    }
                    prog.items.insert(join(module, &name), Item::Enum(name, vars));
                }
                "type" => {
                    self.next();
                    let name = self.ident()?;
                    if self.is_p("<") {
                        self.skip_item()?;
                        continue;
                    }
                    self.expect_p("=")?;
                    let ty = self.ty()?;
                    self.expect_p(";")?;
                    prog.items.insert(join(module, &name), Item::TypeAlias(name, ty));
                }
                "impl" => {
                    self.next();
                    if self.is_p("<") {
                        self.skip_item()?;
                        continue;
                    }
                    let first = self.ty()?;
                    let target = if self.eat_kw("for") { self.ty()? } else { first.clone() };
                    let trait_impl = target != first;
                    let tname = match &target {
                        Ty::Path(n, a) if a.is_empty() => n.clone(),
                        _ => {
                            self.skip_item()?;
                            continue;
                        }
                    };
                    // Default impls: kept as "Type::default".
                    if trait_impl && first.name() != "Default" {
                        self.skip_item()?;
                        continue;
                    }
                    self.expect_p("{")?;
                    let owner = join(module, &tname);
                    loop {
                        self.skip_attrs()?;
                        if self.eat_p("}") {
                            break;
                        }
                        self.eat_kw("pub");
                        if self.is_kw("const") && !self.const_is_fn() {
                            self.next();
                            let name = self.ident()?;
                            self.expect_p(":")?;
                            let ty = self.ty()?;
                            self.expect_p("=")?;
                            let e = self.expr()?;
                            self.expect_p(";")?;
                            prog.items.insert(join(&owner, &name), Item::Const(name, ty, e));
                            continue;
                        }
                        self.eat_kw("const");
                        if !self.is_kw("fn") {
                            self.skip_item()?;
                            continue;
                        }
                        let start = self.i;
                        match self.fn_def(module, Some(&owner)) {
                            Ok(Some(f)) => {
                                prog.items.insert(f.path.clone(), Item::Fn(f));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                self.i = start;
                                self.next();
                                let name = self.ident().unwrap_or_default();
                                prog.skipped.push((join(&owner, &name), e));
                                self.i = start;
                                self.skip_item()?;
                            }
                        }
                    }
                }
                _ => {
                    self.skip_item()?;
                }
            }
        }
    }

    fn const_is_fn(&self) -> bool {
        matches!(self.peek_at(1), Tok::Ident(x) if x == "fn")
    }

    fn fn_def(&mut self, module: &str, owner: Option<&str>) -> PR<Option<FnDef>> {
        if !self.eat_kw("fn") {
            return self.err("expected fn");
        }
        let name = self.ident()?;
        if self.is_p("<") {
            return Err("generic function".into());
        }
        self.expect_p("(")?;
        let mut params = Vec::new();
        let mut has_self = false;
        while !self.eat_p(")") {
            // self receivers: self, &self, &mut self, mut self
            let save = self.i;
            self.eat_p("&");
            self.eat_kw("mut");
            if self.eat_kw("self") {
                has_self = true;
                self.eat_p(",");
                continue;
            }
            self.i = save;
            let pat = self.pattern()?;
            self.expect_p(":")?;
            let ty = self.ty()?;
            params.push(Param { pat, ty });
            self.eat_p(",");
        }
        let ret = if self.eat_p("->") { self.ty()? } else { Ty::unit() };
        if self.is_kw("where") {
            return Err("where clause".into());
        }
        if self.eat_p(";") {
            return Ok(None);
        }
        let body = self.block()?;
        let path = match owner {
            Some(o) => join(o, &name),
            None => join(module, &name),
        };
        Ok(Some(FnDef {
            path,
            params,
            ret,
            body: Rc::new(body),
            has_self,
        }))
    }

    // ---------------------------------------------------------------- types

    pub fn ty(&mut self) -> PR<Ty> {
        if self.eat_p("&") {
            if let Tok::Lifetime(_) = self.peek() {
                self.next();
            }
            let m = self.eat_kw("mut");
            return Ok(Ty::Ref(m, Box::new(self.ty()?)));
        }
        if self.eat_p("(") {
            let mut v = Vec::new();
            while !self.eat_p(")") {
                v.push(self.ty()?);
                self.eat_p(",");
            }
            return Ok(Ty::Tuple(v));
        }
        if self.eat_p("[") {
            let inner = self.ty()?;
            if self.eat_p(";") {
                let n = self.expr()?;
                self.expect_p("]")?;
                return Ok(Ty::Array(Box::new(inner), Some(Box::new(n))));
            }
            self.expect_p("]")?;
            return Ok(Ty::Slice(Box::new(inner)));
        }
        if self.eat_kw("impl") || self.eat_kw("dyn") {
            // impl Fn(..) -> ..
            self.ident()?;
            if self.is_p("(") {
                self.skip_group()?;
            }
            if self.eat_p("->") {
                self.ty()?;
            }
            return Ok(Ty::Fn);
        }
        if self.is_kw("fn") {
            self.next();
            self.skip_group()?;
            if self.eat_p("->") {
                self.ty()?;
            }
            return Ok(Ty::Fn);
        }
        if self.eat_p("_") {
            return Ok(Ty::Infer);
        }
        let mut path = self.ident()?;
        while self.eat_p("::") {
            path.push_str("::");
            path.push_str(&self.ident()?);
        }
        let mut args = Vec::new();
        if self.eat_p("<") {
            loop {
                if matches!(self.peek(), Tok::P(">") | Tok::P(">>") | Tok::P(">=") | Tok::P(">>=")) {
                    self.expect_gt()?;
                    break;
                }
                if let Tok::Lifetime(_) = self.peek() {
                    self.next();
                } else if let Tok::Int(..) = self.peek() {
                    // const generic argument
                    self.next();
                } else {
                    args.push(self.ty()?);
                }
                self.eat_p(",");
            }
        }
        Ok(Ty::Path(path, args))
    }

    // ------------------------------------------------------------- patterns

    fn pattern(&mut self) -> PR<Pat> {
        let first = self.pattern1()?;
        if self.is_p("|") {
            let mut v = vec![first];
            while self.eat_p("|") {
                v.push(self.pattern1()?);
            }
            return Ok(Pat::Or(v));
        }
        Ok(first)
    }

    fn pattern1(&mut self) -> PR<Pat> {
        if self.eat_p("(") {
            let mut v = Vec::new();
            while !self.eat_p(")") {
                v.push(self.pattern()?);
                self.eat_p(",");
            }
            return Ok(Pat::Tuple(v));
        }
        if self.eat_p("&") {
            return self.pattern1();
        }
        match self.peek().clone() {
            Tok::Int(..) | Tok::P("-") | Tok::Str(_) | Tok::Char(_) => {
                let lo = self.unary()?;
                if self.eat_p("..=") {
                    let hi = self.unary()?;
                    return Ok(Pat::Range(Box::new(lo), Box::new(hi), true));
                }
                if self.eat_p("..") {
                    let hi = self.unary()?;
                    return Ok(Pat::Range(Box::new(lo), Box::new(hi), false));
                }
                Ok(Pat::Lit(Box::new(lo)))
            }
            Tok::Ident(s) if s == "true" || s == "false" => {
                self.next();
                Ok(Pat::Lit(Box::new(Expr::Bool(s == "true"))))
            }
            Tok::Ident(s) if s == "_" => {
                self.next();
                Ok(Pat::Wild)
            }
            Tok::P("_") => {
                self.next();
                Ok(Pat::Wild)
            }
            Tok::Ident(_) => {
                self.eat_kw("ref");
                self.eat_kw("mut");
                let mut path = self.ident()?;
                let mut qualified = false;
                while self.eat_p("::") {
                    qualified = true;
                    path.push_str("::");
                    path.push_str(&self.ident()?);
                }
                if self.eat_p("(") {
                    let mut v = Vec::new();
                    while !self.eat_p(")") {
                        v.push(self.pattern()?);
                        self.eat_p(",");
                    }
                    return Ok(Pat::TupleStruct(path, v));
                }
                if !self.no_struct && self.is_p("{") && qualified {
                    self.next();
                    let mut v = Vec::new();
                    while !self.eat_p("}") {
                        let f = self.ident()?;
                        let p = if self.eat_p(":") { self.pattern()? } else { Pat::Bind(f.clone()) };
                        v.push((f, p));
                        self.eat_p(",");
                    }
                    return Ok(Pat::Struct(path, v));
                }
                // A single lower-case identifier binds; anything else names
                // a constant or unit variant.
                if !qualified && is_binding(&path) {
                    if self.eat_p("@") {
                        return self.pattern1();
                    }
                    return Ok(Pat::Bind(path));
                }
                Ok(Pat::Path(path))
            }
            _ => self.err("bad pattern"),
        }
    }

    // ----------------------------------------------------------- statements

    pub fn block(&mut self) -> PR<Block> {
        self.expect_p("{")?;
        let save = self.no_struct;
        self.no_struct = false;
        let mut b = Block::default();
        loop {
            self.skip_attrs()?;
            if self.eat_p("}") {
                break;
            }
            if self.eat_p(";") {
                continue;
            }
            if self.is_kw("let") {
                self.next();
                let pat = self.pattern()?;
                let ty = if self.eat_p(":") { Some(self.ty()?) } else { None };
                let init = if self.eat_p("=") { Some(self.expr()?) } else { None };
                if self.eat_kw("else") {
                    let els = self.block()?;
                    self.expect_p(";")?;
                    let Some(init) = init else {
                        return self.err("let-else without a value");
                    };
                    b.stmts.push(Stmt::LetElse(pat, init, Rc::new(els)));
                    continue;
                }
                self.expect_p(";")?;
                b.stmts.push(Stmt::Let(pat, ty, init));
                continue;
            }
            if self.is_kw("fn") || self.is_kw("const") && self.const_item_here() {
                return Err("nested item".into());
            }
            let e = self.expr_stmt()?;
            if self.eat_p(";") {
                b.stmts.push(Stmt::Semi(e));
            } else if self.is_p("}") {
                b.tail = Some(e);
            } else if block_like(&e) {
                b.stmts.push(Stmt::Expr(e));
            } else {
                return self.err("expected ';'");
            }
        }
        self.no_struct = save;
        // A trailing block-like statement is the block's value.
        if b.tail.is_none()
            && let Some(Stmt::Expr(e)) = b.stmts.last()
            && matches!(e, Expr::If(..) | Expr::Match(..) | Expr::Block(..))
        {
            let e = e.clone();
            b.stmts.pop();
            b.tail = Some(e);
        }
        Ok(b)
    }

    fn const_item_here(&self) -> bool {
        matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::P(":"))
    }

    /// An expression statement: block-like expressions end the statement
    /// without a `;` (they are not the left operand of a binary operator).
    fn expr_stmt(&mut self) -> PR<Expr> {
        if self.is_kw("if")
            || self.is_kw("match")
            || self.is_kw("for")
            || self.is_kw("loop")
            || self.is_kw("while")
            || self.is_p("{")
        {
            let e = self.primary()?;
            if self.is_p(".") || self.is_p("?") {
                // (block).method() — rare; continue as a postfix chain.
                let e = self.postfix_from(e)?;
                return self.binary_rest(e, 0);
            }
            return Ok(e);
        }
        self.expr()
    }

    // ---------------------------------------------------------- expressions

    pub fn expr(&mut self) -> PR<Expr> {
        let lhs = self.range_expr()?;
        for op in ["=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="] {
            if self.is_p(op) {
                self.next();
                let rhs = self.expr()?;
                let o: &'static str = match op {
                    "=" => "",
                    "+=" => "+",
                    "-=" => "-",
                    "*=" => "*",
                    "/=" => "/",
                    "%=" => "%",
                    "&=" => "&",
                    "|=" => "|",
                    "^=" => "^",
                    "<<=" => "<<",
                    _ => ">>",
                };
                return Ok(Expr::Assign(o, Box::new(lhs), Box::new(rhs)));
            }
        }
        Ok(lhs)
    }

    fn range_expr(&mut self) -> PR<Expr> {
        if self.is_p("..") || self.is_p("..=") {
            let incl = self.is_p("..=");
            self.next();
            let hi = if self.starts_expr() { Some(Box::new(self.binary(0)?)) } else { None };
            return Ok(Expr::Range(None, hi, incl));
        }
        let lo = self.binary(0)?;
        if self.is_p("..") || self.is_p("..=") {
            let incl = self.is_p("..=");
            self.next();
            let hi = if self.starts_expr() { Some(Box::new(self.binary(0)?)) } else { None };
            return Ok(Expr::Range(Some(Box::new(lo)), hi, incl));
        }
        Ok(lo)
    }

    fn starts_expr(&self) -> bool {
        !matches!(
            self.peek(),
            Tok::P(")") | Tok::P("]") | Tok::P("}") | Tok::P(",") | Tok::P(";") | Tok::Eof
        ) && !(self.no_struct && self.is_p("{"))
    }

    fn binary(&mut self, min: u8) -> PR<Expr> {
        let lhs = self.unary()?;
        self.binary_rest(lhs, min)
    }

    fn binary_rest(&mut self, mut lhs: Expr, min: u8) -> PR<Expr> {
        loop {
            let (op, prec): (&'static str, u8) = match self.peek() {
                Tok::P("||") => ("||", 1),
                Tok::P("&&") => ("&&", 2),
                Tok::P("==") => ("==", 3),
                Tok::P("!=") => ("!=", 3),
                Tok::P("<") => ("<", 3),
                Tok::P(">") => (">", 3),
                Tok::P("<=") => ("<=", 3),
                Tok::P(">=") => (">=", 3),
                Tok::P("|") => ("|", 4),
                Tok::P("^") => ("^", 5),
                Tok::P("&") => ("&", 6),
                Tok::P("<<") => ("<<", 7),
                Tok::P(">>") => (">>", 7),
                Tok::P("+") => ("+", 8),
                Tok::P("-") => ("-", 8),
                Tok::P("*") => ("*", 9),
                Tok::P("/") => ("/", 9),
                Tok::P("%") => ("%", 9),
                Tok::Ident(s) if s == "as" => ("as", 10),
                _ => return Ok(lhs),
            };
            if prec < min {
                return Ok(lhs);
            }
            self.next();
            if op == "as" {
                let ty = self.ty()?;
                lhs = Expr::Cast(Box::new(lhs), ty);
                continue;
            }
            let mut rhs = self.unary()?;
            // Higher-precedence operators bind the right operand first.
            loop {
                let next_prec = match self.peek() {
                    Tok::P("||") => 1,
                    Tok::P("&&") => 2,
                    Tok::P("==") | Tok::P("!=") | Tok::P("<") | Tok::P(">") | Tok::P("<=")
                    | Tok::P(">=") => 3,
                    Tok::P("|") => 4,
                    Tok::P("^") => 5,
                    Tok::P("&") => 6,
                    Tok::P("<<") | Tok::P(">>") => 7,
                    Tok::P("+") | Tok::P("-") => 8,
                    Tok::P("*") | Tok::P("/") | Tok::P("%") => 9,
                    Tok::Ident(s) if s == "as" => 10,
                    _ => 0,
                };
                if next_prec > prec {
                    rhs = self.binary_rest(rhs, next_prec)?;
                } else {
                    break;
                }
            }
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn unary(&mut self) -> PR<Expr> {
        if self.eat_p("!") {
            return Ok(Expr::Unary("!", Box::new(self.unary()?)));
        }
        if self.eat_p("-") {
            return Ok(Expr::Unary("-", Box::new(self.unary()?)));
        }
        if self.eat_p("*") {
            return Ok(Expr::Deref(Box::new(self.unary()?)));
        }
        if self.eat_p("&") {
            let m = self.eat_kw("mut");
            return Ok(Expr::Ref(m, Box::new(self.unary()?)));
        }
        if self.eat_p("&&") {
            let m = self.eat_kw("mut");
            let inner = Expr::Ref(m, Box::new(self.unary()?));
            return Ok(Expr::Ref(false, Box::new(inner)));
        }
        let p = self.primary()?;
        self.postfix_from(p)
    }

    fn postfix_from(&mut self, mut e: Expr) -> PR<Expr> {
        loop {
            if self.eat_p("?") {
                e = Expr::Try(Box::new(e));
            } else if self.is_p(".") {
                self.next();
                match self.next() {
                    Tok::Ident(name) => {
                        if self.is_p("::") {
                            // turbofish: .method::<T>(...)
                            self.next();
                            self.expect_p("<")?;
                            while !matches!(self.peek(), Tok::P(">") | Tok::P(">>")) {
                                self.ty()?;
                                self.eat_p(",");
                            }
                            self.expect_gt()?;
                        }
                        if self.is_p("(") {
                            let args = self.args()?;
                            e = Expr::Method(Box::new(e), name, args);
                        } else {
                            e = Expr::Field(Box::new(e), name);
                        }
                    }
                    Tok::Int(v, _) => e = Expr::Field(Box::new(e), v.to_string()),
                    Tok::Float(s, _) => {
                        // `.0.1` lexes as the float "0.1".
                        for part in s.split('.') {
                            e = Expr::Field(Box::new(e), part.to_string());
                        }
                    }
                    t => return Err(format!("bad field {t:?}")),
                }
            } else if self.is_p("(") {
                let args = self.args()?;
                e = Expr::Call(Box::new(e), args);
            } else if self.is_p("[") {
                self.next();
                let save = self.no_struct;
                self.no_struct = false;
                let i = self.expr()?;
                self.no_struct = save;
                self.expect_p("]")?;
                e = Expr::Index(Box::new(e), Box::new(i));
            } else {
                return Ok(e);
            }
        }
    }

    fn args(&mut self) -> PR<Vec<Expr>> {
        self.expect_p("(")?;
        let save = self.no_struct;
        self.no_struct = false;
        let mut v = Vec::new();
        while !self.eat_p(")") {
            v.push(self.expr()?);
            self.eat_p(",");
        }
        self.no_struct = save;
        Ok(v)
    }

    fn cond(&mut self) -> PR<Expr> {
        let save = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = save;
        e
    }

    fn primary(&mut self) -> PR<Expr> {
        match self.peek().clone() {
            Tok::Int(v, s) => {
                self.next();
                Ok(Expr::Int(v, s))
            }
            Tok::Float(t, s) => {
                self.next();
                let v: f64 = t.parse().map_err(|_| format!("bad float {t}"))?;
                Ok(Expr::Float(v, s))
            }
            Tok::Str(s) => {
                self.next();
                Ok(Expr::Str(s))
            }
            Tok::Char(c) => {
                self.next();
                Ok(Expr::Char(c))
            }
            Tok::Lifetime(l) => {
                // 'label: loop { ... }
                self.next();
                self.expect_p(":")?;
                if self.eat_kw("loop") {
                    let b = self.block()?;
                    return Ok(Expr::Loop(Rc::new(b), Some(l)));
                }
                self.err("labelled non-loop")
            }
            Tok::P("(") => {
                self.next();
                let save = self.no_struct;
                self.no_struct = false;
                let mut v = Vec::new();
                let mut trailing = false;
                while !self.eat_p(")") {
                    v.push(self.expr()?);
                    trailing = self.eat_p(",");
                }
                self.no_struct = save;
                if v.len() == 1 && !trailing {
                    return Ok(v.pop().unwrap());
                }
                Ok(Expr::Tuple(v))
            }
            Tok::P("[") => {
                self.next();
                let save = self.no_struct;
                self.no_struct = false;
                let mut v = Vec::new();
                if self.eat_p("]") {
                    self.no_struct = save;
                    return Ok(Expr::Array(v));
                }
                let first = self.expr()?;
                if self.eat_p(";") {
                    let n = self.expr()?;
                    self.expect_p("]")?;
                    self.no_struct = save;
                    return Ok(Expr::Repeat(Box::new(first), Box::new(n)));
                }
                v.push(first);
                self.eat_p(",");
                while !self.eat_p("]") {
                    v.push(self.expr()?);
                    self.eat_p(",");
                }
                self.no_struct = save;
                Ok(Expr::Array(v))
            }
            Tok::P("{") => Ok(Expr::Block(Rc::new(self.block()?))),
            Tok::P("|") | Tok::P("||") => {
                let mut params = Vec::new();
                if !self.eat_p("||") {
                    self.next();
                    while !self.eat_p("|") {
                        let p = self.pattern1()?;
                        let ty = if self.eat_p(":") { self.ty()? } else { Ty::Infer };
                        params.push((p, ty));
                        self.eat_p(",");
                    }
                }
                if self.eat_p("->") {
                    self.ty()?;
                    let b = self.block()?;
                    return Ok(Expr::Closure(params, Box::new(Expr::Block(Rc::new(b)))));
                }
                let body = self.expr()?;
                Ok(Expr::Closure(params, Box::new(body)))
            }
            Tok::Ident(k) => self.primary_ident(&k),
            _ => self.err("bad expression"),
        }
    }

    fn primary_ident(&mut self, k: &str) -> PR<Expr> {
        match k {
            "true" | "false" => {
                self.next();
                Ok(Expr::Bool(k == "true"))
            }
            "if" => {
                self.next();
                let c = self.cond()?;
                let then = self.block()?;
                let els = if self.eat_kw("else") {
                    if self.is_kw("if") {
                        Some(Box::new(self.primary()?))
                    } else {
                        Some(Box::new(Expr::Block(Rc::new(self.block()?))))
                    }
                } else {
                    None
                };
                Ok(Expr::If(Box::new(c), Rc::new(then), els))
            }
            "match" => {
                self.next();
                let scrut = self.cond()?;
                self.expect_p("{")?;
                let save = self.no_struct;
                self.no_struct = false;
                let mut arms = Vec::new();
                while !self.eat_p("}") {
                    let pat = self.pattern()?;
                    let guard = if self.eat_kw("if") { Some(self.expr()?) } else { None };
                    self.expect_p("=>")?;
                    let body = self.expr_stmt()?;
                    self.eat_p(",");
                    arms.push(Arm { pat, guard, body });
                }
                self.no_struct = save;
                Ok(Expr::Match(Box::new(scrut), arms))
            }
            "loop" => {
                self.next();
                Ok(Expr::Loop(Rc::new(self.block()?), None))
            }
            "while" => {
                self.next();
                if self.is_kw("let") {
                    return self.err("while let");
                }
                let c = self.cond()?;
                Ok(Expr::While(Box::new(c), Rc::new(self.block()?)))
            }
            "for" => {
                self.next();
                let pat = self.pattern()?;
                if !self.eat_kw("in") {
                    return self.err("expected in");
                }
                let it = self.cond()?;
                Ok(Expr::For(pat, Box::new(it), Rc::new(self.block()?)))
            }
            "break" => {
                self.next();
                if let Tok::Lifetime(_) = self.peek() {
                    self.next();
                }
                let v = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                Ok(Expr::Break(v))
            }
            "continue" => {
                self.next();
                if let Tok::Lifetime(_) = self.peek() {
                    self.next();
                }
                Ok(Expr::Continue)
            }
            "return" => {
                self.next();
                let v = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                Ok(Expr::Return(v))
            }
            "unsafe" => {
                self.next();
                Ok(Expr::Block(Rc::new(self.block()?)))
            }
            "let" => {
                // `if let PAT = EXPR` (and `&& let` chains): the scrutinee
                // binds tighter than `&&`.
                self.next();
                let pat = self.pattern()?;
                self.expect_p("=")?;
                let e = self.binary(3)?;
                Ok(Expr::Let(pat, Box::new(e)))
            }
            _ => {
                let mut path = self.ident()?;
                loop {
                    if self.is_p("::") {
                        self.next();
                        if self.is_p("<") {
                            // Type::<T>::f
                            self.next();
                            while !matches!(self.peek(), Tok::P(">") | Tok::P(">>")) {
                                self.ty()?;
                                self.eat_p(",");
                            }
                            self.expect_gt()?;
                            continue;
                        }
                        path.push_str("::");
                        path.push_str(&self.ident()?);
                    } else if self.is_p("<") && path.chars().next().is_some_and(|c| c.is_uppercase()) && self.generic_path_ahead() {
                        // Tup<V>::new — generic args on a type path
                        self.next();
                        while !matches!(self.peek(), Tok::P(">") | Tok::P(">>")) {
                            self.ty()?;
                            self.eat_p(",");
                        }
                        self.expect_gt()?;
                    } else {
                        break;
                    }
                }
                if self.is_p("!") {
                    self.next();
                    return self.macro_call(&path);
                }
                if !self.no_struct && self.is_p("{") && path.chars().next().is_some_and(|c| c.is_uppercase()) {
                    return self.struct_lit(path);
                }
                Ok(Expr::Path(path))
            }
        }
    }

    fn generic_path_ahead(&self) -> bool {
        // `Name<...>::`: look for a matching '>' followed by '::'.
        let mut depth = 0;
        let mut k = 0;
        loop {
            match self.peek_at(k) {
                Tok::P("<") => depth += 1,
                Tok::P(">") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.peek_at(k + 1), Tok::P("::"));
                    }
                }
                Tok::P(">>") => {
                    depth -= 2;
                    if depth <= 0 {
                        return matches!(self.peek_at(k + 1), Tok::P("::"));
                    }
                }
                Tok::P(";") | Tok::P("{") | Tok::P("}") | Tok::Eof => return false,
                _ => {}
            }
            k += 1;
            if k > 64 {
                return false;
            }
        }
    }

    fn struct_lit(&mut self, path: String) -> PR<Expr> {
        self.expect_p("{")?;
        let save = self.no_struct;
        self.no_struct = false;
        let mut fields = Vec::new();
        let mut base = None;
        while !self.eat_p("}") {
            if self.eat_p("..") {
                base = Some(Box::new(self.expr()?));
                continue;
            }
            let f = match self.next() {
                Tok::Ident(s) => s,
                Tok::Int(v, _) => v.to_string(),
                t => return Err(format!("bad struct field {t:?}")),
            };
            let e = if self.eat_p(":") { self.expr()? } else { Expr::Path(f.clone()) };
            fields.push((f, e));
            self.eat_p(",");
        }
        self.no_struct = save;
        Ok(Expr::StructLit(path, fields, base))
    }

    fn macro_call(&mut self, name: &str) -> PR<Expr> {
        match name {
            "matches" => {
                self.expect_p("(")?;
                let save = self.no_struct;
                self.no_struct = false;
                let e = self.expr()?;
                self.expect_p(",")?;
                let p = self.pattern()?;
                let guard = if self.eat_kw("if") { Some(Box::new(self.expr()?)) } else { None };
                self.eat_p(",");
                self.expect_p(")")?;
                self.no_struct = save;
                Ok(Expr::Matches(Box::new(e), p, guard))
            }
            "unreachable" | "panic" | "todo" | "unimplemented" => {
                self.skip_group()?;
                Ok(Expr::Unreachable)
            }
            _ => self.err(&format!("macro {name}!")),
        }
    }
}

fn block_like(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(..) | Expr::Match(..) | Expr::Block(..) | Expr::Loop(..) | Expr::While(..) | Expr::For(..)
    )
}

fn is_binding(s: &str) -> bool {
    let c = s.chars().next().unwrap_or('A');
    (c.is_lowercase() || c == '_') && s != "None" && !s.starts_with("S_")
}

pub fn join(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{a}::{b}") }
}

/// The module a `use` path names, from inside MODULE.
fn resolve_mod(module: &str, p: &str) -> String {
    let mut parts: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    for (k, seg) in p.split("::").enumerate() {
        match seg {
            "super" => {
                parts.pop();
            }
            "self" => {}
            "crate" if k == 0 => {
                parts.clear();
            }
            "generated" if out.is_empty() && parts.is_empty() => {}
            s => out.push(s),
        }
    }
    let mut all: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    all.extend(out.iter().map(|s| s.to_string()));
    // The generated core is parsed as module "core" (core_g) with the
    // tables and syms as top-level modules.
    let s = all.join("::");
    match s.as_str() {
        "core_g" | "core_i" => "core".to_string(),
        _ => s,
    }
}

/// Parse SRC as the contents of MODULE into PROG.
pub fn parse_into(prog: &mut Program, module: &str, src: &str) -> Result<(), String> {
    let lexed = lex(src)?;
    let mut p = Parser {
        t: lexed.toks,
        pos: &lexed.pos,
        i: 0,
        no_struct: false,
    };
    p.items(prog, module, false)
}
