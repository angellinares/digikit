"""Translation of one core function body into Rust."""

from __future__ import annotations

import re
import types as pytypes
from typing import TYPE_CHECKING, Any

from mypy import nodes as mn

from tools.sharc_dev import sharc_transpile_infer as infer

from .tables import (
    BLK_ERASED_WRITES,
    BLK_REG_BOUNDARY,
    BLK_RF_ATTRS,
    BOUNDARY,
    DYN_CALLS,
    ERASED_CALLS,
    FACT_ATTRS,
    FAST_FLOAT_BINARY,
    FLOAT_FUNCS,
    LOOP_SYNC,
    LOOP_SYNCED_FACT,
    MEM_FAST,
    MEM_READS,
    MEM_WRITES,
    NARROW_FIELDS,
    OBSERVABILITY_PARAMS,
    PURE_BOUNDARY,
    REG_FACTS,
    SETTING_FACTS,
    STATE_ATTRS,
    STATE_ERASED_WRITES,
    TRAP_CALLS,
    rust_ident,
)
from .textpass import _IDENT_RE, _USES_S, _USES_SRF, _prune
from .types import (
    ANY,
    BOOL,
    CFGSET,
    CONCRETE,
    CONFIG_MAP,
    ERASED,
    FIELDS,
    FLOAT,
    FN,
    INSN,
    INT,
    LSTATE,
    MR,
    NONE,
    OPT,
    REC,
    RECORDS,
    REGVIEW,
    SPEC,
    SPECVIEW,
    STACKS,
    STATE,
    STK,
    STR,
    TABLE,
    TUP,
    VAL,
    VI,
    VTUP,
    T,
    TranspileError,
    _has_any,
    _type_of_value,
    from_mypy,
    normalize_union,
)
from .values import (
    _NOCONST,
    PS,
    E,
    FnSig,
    Tag,
    _is_const,
    _merge_item,
    _ps_item,
    _ps_parts,
    _same_static,
    lit_float,
    lit_int,
)

if TYPE_CHECKING:
    from .translator import Translator

_PY_COMPARE = {
    "==": lambda a, b: a == b,
    "!=": lambda a, b: a != b,
    "<": lambda a, b: a < b,
    "<=": lambda a, b: a <= b,
    ">": lambda a, b: a > b,
    ">=": lambda a, b: a >= b,
    "is": lambda a, b: a is b,
    "is not": lambda a, b: a is not b,
    "in": lambda a, b: a in b,
    "not in": lambda a, b: a not in b,
}

_PY_BINOPS = {
    "+": lambda a, b: a + b,
    "-": lambda a, b: a - b,
    "*": lambda a, b: a * b,
    "&": lambda a, b: a & b,
    "|": lambda a, b: a | b,
    "^": lambda a, b: a ^ b,
    "<<": lambda a, b: a << b,
    ">>": lambda a, b: a >> b,
    "//": lambda a, b: a // b,
    "/": lambda a, b: a / b,
    "**": lambda a, b: a**b,
    "%": lambda a, b: a % b,
}


class FnT:
    """Translates one core function."""

    def __init__(
        self,
        tr: Translator,
        fullname: str,
        *,
        static: dict[str, Any] | None = None,
        pe: bool = False,
        rust_name: str | None = None,
        facts: dict[str, Any] | None = None,
    ) -> None:
        self.tr = tr
        self.core = tr.core
        self.fullname = fullname
        self.sig = tr.signature(fullname)
        if rust_name is not None:
            import dataclasses

            self.sig = dataclasses.replace(self.sig, rust_name=rust_name)
        self.fdef = self.core.funcs[fullname]
        self.locals: dict[int, tuple[str, T]] = {}  # id(Var) -> (name, type)
        self.local_order: list[int] = []
        self.used_names: set[str] = set()
        self.tmp = 0
        self.line = self.fdef.line
        # Partial evaluation (block code): parameter values fixed at
        # generation time, and the static value of each local at the
        # current point of the translation.
        self.static = static or {}
        self.pe = pe
        self.env: dict[int, Any] = {}
        # State attributes (FACT_ATTRS) known here, and the ones this
        # variant may change for its caller.
        self.facts: dict[str, Any] = dict(facts or {})
        self.effects: set[str] = set()
        # Locals whose static value has no Rust form (never assigned natively).
        self.unrepr: set[int] = set()
        # Partial evaluation: the static items of each return (None: none),
        # and each return's whole static value (_NOCONST: dynamic).
        self.returns: list[tuple | None] = []
        self.ret_consts: list[Any] = []
        # The facts at each way out (return statements, the end).
        self.exit_facts: list[dict] = []
        # Partial evaluation: whether this reads or writes data memory
        # (with callees).
        self.memreads = False
        self.memwrites = False
        # Block mode: registers read (current file / snapshot) and written.
        self.rregs: set[int] = set()
        self.oregs: set[int] = set()
        self.wregs: set[int] = set()
        # Registers written with a mask that is not all-known (rf_put).
        self.putregs: set[int] = set()

    def where(self, node: Any = None) -> str:
        line = getattr(node, "line", None) or self.line
        return "%s:%s" % (self.fullname, line)

    def fail(self, node: Any, msg: str) -> TranspileError:
        return TranspileError("%s: %s" % (self.where(node), msg))

    def fresh(self, stem: str = "t") -> str:
        self.tmp += 1
        return "__%s%d" % (stem, self.tmp)

    def mtype(self, e: Any) -> T:
        t = self.core.types.get(e)
        if t is None:
            return ANY
        return from_mypy(t, self.where(e))

    def rt(self, t: T) -> str:
        return self.tr.rust_type(t)

    def static_e(self, value: Any, t: T, node: Any) -> E:
        """An E for static VALUE, typed T."""
        if isinstance(value, pytypes.FunctionType):
            name = self.core.live_funcs.get(id(value))
            if name is not None:
                return E(
                    self.tr.fn_const(name),
                    FN,
                    const=value,
                    pts=frozenset({infer.Obj("fn", name)}),
                )
        if isinstance(
            value, (dict, frozenset, set, pytypes.MappingProxyType)
        ) and t not in (
            FIELDS,
            SPECVIEW,
            CONFIG_MAP,
        ):
            return E("%d" % self.tr.table_id(value), TABLE, const=value)
        if t == ANY:
            t = _type_of_value(value)
        try:
            code = self.tr.const_value(value, t, self.where(node))
        except TranspileError:
            code = "__NOREPR__"
        return E(code, t, const=value)

    def py_eval(self, node: Any, t: T, fn: Any, *args: Any) -> E:
        """FN(*ARGS) at generation time, as a static E (or the trap a Python
        exception would be natively)."""
        try:
            value = fn(*args)
        except Exception as exc:  # noqa: BLE001 -- raised at run time too
            return E(self.trap_expr(node, "raise %s" % type(exc).__name__), t)
        return self.static_e(value, t, node)

    def facts_out(self) -> dict:
        """The register facts (REG_FACTS) every way out agrees on."""
        if not self.exit_facts:
            return {}
        out = {k: v for k, v in self.exit_facts[0].items() if k in REG_FACTS.values()}
        for f in self.exit_facts[1:]:
            for k in list(out):
                if k not in f or not _same_static(f[k], out[k]):
                    del out[k]
        return out

    def ret_const(self) -> Any:
        """The static value every return of this variant has, or _NOCONST."""
        if not self.ret_consts:
            return _NOCONST
        v0 = self.ret_consts[0]
        if v0 is _NOCONST:
            return _NOCONST
        for v in self.ret_consts[1:]:
            if v is _NOCONST or not _same_static(v, v0):
                return _NOCONST
        return v0

    def ret_summary(self) -> tuple | None:
        """The items every tuple return of this variant agrees on
        statically. A ``return None`` does not count: the items describe
        the value when it is a tuple (only then can it be unpacked)."""
        rets = [
            r
            for r, c in zip(self.returns, self.ret_consts, strict=True)
            if c is not None
        ]
        if not rets or any(r is None for r in rets):
            return None
        first = rets[0]
        assert first is not None
        n = len(first)
        if any(len(r) != n for r in rets if r is not None):
            return None
        out = []
        for i in range(n):
            vals = [r[i] for r in rets if r is not None]
            out.append(_merge_item(vals))
        if all(p is _NOCONST for p in out):
            return None
        return tuple(out)

    @property
    def blk(self) -> bool:
        return self.pe and self.tr.blk

    def merge_regs(self, path: str) -> None:
        if self.tr.variant_memreads.get(path, True):
            self.memreads = True
        if self.tr.variant_memwrites.get(path, True):
            self.memwrites = True
        self.putregs |= self.tr.variant_putregs.get(path, frozenset(range(128)))
        r, o, w = self.tr.variant_regs.get(path, (frozenset(),) * 3)
        self.rregs |= r
        self.oregs |= o
        self.wregs |= w

    def reg_index(self, idx: E, node: Any) -> int:
        """The constant register index of a block-mode access."""
        if not _is_const(idx) or isinstance(idx.const, Tag):
            raise self.fail(node, "block code: register index not static")
        return int(idx.const)

    def note_read(self, view: E, code: int) -> None:
        """Record a block-mode read of CODE through VIEW."""
        if not (0 <= code < 128):
            return
        if _is_const(view) and isinstance(view.const, Tag):
            vc = view.const.code
            old = "| 1" in vc or "OLD" in vc
            pey = "PEY" in vc
            olds, peys = [old], [pey]
        else:
            olds, peys = [False, True], [False, True]
        for o in olds:
            for p in peys:
                c = code + 80 if p and code < 16 else code
                (self.oregs if o else self.rregs).add(c)

    def reg_written(self, code: int, v: E | None = None) -> None:
        """A block-mode write of register CODE (value V): its fact becomes
        V's static value, or no longer holds."""
        fact = REG_FACTS.get(code)
        if fact is not None:
            self.facts.pop(fact, None)
            self.effects.add(fact)
            if v is not None and _is_const(v) and type(v.const).__name__ == "Const":
                self.facts[fact] = v.const.value

    def rf_read(self, view: E, idx: E, node: Any, fn: str = "rf_get") -> E:
        code = self.reg_index(idx, node)
        fact = REG_FACTS.get(code)
        if fact is not None and fact in self.facts:
            # A register the block's entry guard fixed (MODE1).
            from tools.sharc_dev.sharc_core.values import Const

            return self.static_e(Const(self.facts[fact]), VAL, node)
        self.note_read(view, code)
        return E(
            "%s(rf, %s, %s)" % (fn, view.code, lit_int(code)),
            VAL,
            True,
            _NOCONST,
        )

    def trap_expr(self, node: Any, kind: str) -> str:
        return "return Err(%s)" % self.tr.trap(self.where(node), kind)

    # -- locals --------------------------------------------------------------

    def declare(self, var: mn.Var, t: T | None = None) -> tuple[str, T]:
        key = id(var)
        if key in self.locals:
            return self.locals[key]
        if t is None:
            t = from_mypy(var.type, self.where() + " local " + var.name)
        if t == ANY:
            raise self.fail(None, "local %s has no type" % var.name)
        name = rust_ident(var.name)
        base = name
        n = 1
        while name in self.used_names:
            n += 1
            name = "%s_%d" % (base, n)
        self.used_names.add(name)
        self.locals[key] = (name, t)
        self.local_order.append(key)
        return name, t

    # -- coercion --------------------------------------------------------------

    def can(self, frm: T, to: T) -> bool:
        try:
            self.coerce_code("x", frm, to, None)
            return True
        except TranspileError:
            return False

    def coerce(self, e: E, to: T, node: Any = None) -> str:
        return self.coerce_code(e.code, e.t, to, node)

    def coerce_code(self, code: str, frm: T, to: T, node: Any) -> str:
        if frm == to or to == ANY:
            return code
        if code.startswith("return Err("):
            return code
        k, tk = frm.kind, to.kind
        if frm == ANY:
            raise self.fail(node, "untyped value used as %r" % (to,))
        if frm == NONE:
            if tk == "opt":
                return "None"
            if to == SPECVIEW:
                return "SpecView::NONE"
            if to in (LSTATE, ERASED):
                return "()"
            raise self.fail(node, "None used as %r" % (to,))
        if to == ERASED:
            return "()"
        if frm == ERASED and to == STR:
            return "SYM_DYN"
        if tk == "opt":
            if k == "opt":
                inner = self.coerce_code("__x", frm.args[0], to.args[0], node)
                return "(%s).map(|__x| %s)" % (code, inner)
            return "Some(%s)" % self.coerce_code(code, frm, to.args[0], node)
        if k == "opt":
            return self.coerce_code("(%s).unwrap()" % code, frm.args[0], to, node)
        if tk == "union":
            if frm in to.args:
                return "%s::A%d(%s)" % (
                    self.tr.union_name(to),
                    to.args.index(frm),
                    code,
                )
            if k == "union":
                arms = []
                for i, a in enumerate(frm.args):
                    if self.can(a, to):
                        arms.append(
                            "%s::A%d(__x) => %s"
                            % (
                                self.tr.union_name(frm),
                                i,
                                self.coerce_code("__x", a, to, node),
                            )
                        )
                if not arms:
                    raise self.fail(node, "no member of %r fits %r" % (frm, to))
                if len(arms) < len(frm.args):
                    arms.append("_ => unreachable!()")
                return "(match %s { %s })" % (code, ", ".join(arms))
            for i, a in enumerate(to.args):
                if self.can(frm, a):
                    return "%s::A%d(%s)" % (
                        self.tr.union_name(to),
                        i,
                        self.coerce_code(code, frm, a, node),
                    )
            raise self.fail(node, "%r fits no member of %r" % (frm, to))
        if k == "union":
            if to in frm.args:
                i = frm.args.index(to)
                return "(match %s { %s::A%d(__x) => __x, _ => unreachable!() })" % (
                    code,
                    self.tr.union_name(frm),
                    i,
                )
            arms = []
            for i, a in enumerate(frm.args):
                if self.can(a, to):
                    arms.append(
                        "%s::A%d(__x) => %s"
                        % (
                            self.tr.union_name(frm),
                            i,
                            self.coerce_code("__x", a, to, node),
                        )
                    )
            if not arms:
                raise self.fail(node, "no member of %r fits %r" % (frm, to))
            if len(arms) < len(frm.args):
                arms.append("_ => unreachable!()")
            return "(match %s { %s })" % (code, ", ".join(arms))
        if frm == VAL and to == SPEC:
            return "Spec::V(%s)" % code
        if frm == MR and to == SPEC:
            return "Spec::M(%s)" % code
        if frm == SPEC and to == VAL:
            return "(%s).as_v()" % code
        if frm == SPEC and to == MR:
            return "(%s).as_mr()" % code
        if frm == VAL and to == VI:
            return "VI::V(%s)" % code
        if frm == INT and to == VI:
            return "VI::I(%s)" % code
        if frm == BOOL and to == INT:
            return "((%s) as Int)" % code
        if frm == BOOL and to == VI:
            return "VI::I((%s) as Int)" % code
        if frm == INT and to == FLOAT:
            return "((%s) as f64)" % code
        if frm == BOOL and to == FLOAT:
            return "((%s) as i32 as f64)" % code
        if k == "tup" and tk == "tup" and len(frm.args) == len(to.args):
            v = self.fresh("c")
            parts = [
                self.coerce_code("%s.%d" % (v, i), a, b, node)
                for i, (a, b) in enumerate(zip(frm.args, to.args, strict=True))
            ]
            inner = "(%s,)" % parts[0] if len(parts) == 1 else "(%s)" % ", ".join(parts)
            return "{ let %s = %s; %s }" % (v, code, inner)
        if k == "tup" and tk == "vtup":
            v = self.fresh("c")
            parts = [
                self.coerce_code("%s.%d" % (v, i), a, to.args[0], node)
                for i, a in enumerate(frm.args)
            ]
            return "{ let %s = %s; Tup::from_slice(&[%s]) }" % (
                v,
                code,
                ", ".join(parts),
            )
        if k == "vtup" and tk == "vtup":
            if code == "Tup::from_slice(&[])":
                return "Tup::new()"
            inner = self.coerce_code("__x", frm.args[0], to.args[0], node)
            return "(%s).map(|__x| %s)" % (code, inner)
        if k == "vtup" and tk == "tup":
            v = self.fresh("c")
            parts = [
                self.coerce_code("%s.get(%d)" % (v, i), frm.args[0], b, node)
                for i, b in enumerate(to.args)
            ]
            inner = "(%s,)" % parts[0] if len(parts) == 1 else "(%s)" % ", ".join(parts)
            return "{ let %s = %s; %s }" % (v, code, inner)
        if frm == TABLE and to == SPECVIEW:
            return code
        raise self.fail(node, "cannot convert %r to %r" % (frm, to))

    def cond(self, node: Any) -> str:
        """NODE's truth value, for a test (if, while, not, a conditional
        expression): ``and``/``or`` combine truth values there, whatever
        the operand types."""
        return self.test(node)[0]

    def test(self, node: Any) -> tuple[str, Any]:
        """(Rust truth-value code, static truth value or _NOCONST)."""
        if isinstance(node, mn.OpExpr) and node.op in ("and", "or"):
            lc, lv = self.test(node.left)
            if lv is not _NOCONST:
                if node.op == "and":
                    return self.test(node.right) if lv else ("false", False)
                return ("true", True) if lv else self.test(node.right)
            rc, rv = self.test(node.right)
            if rv is not _NOCONST and (node.op == "and") != bool(rv):
                return (
                    "{ let _ = %s; %s }" % (lc, "true" if rv else "false"),
                    bool(rv),
                )
            return (
                "(%s %s %s)" % (lc, "&&" if node.op == "and" else "||", rc),
                _NOCONST,
            )
        if isinstance(node, mn.UnaryExpr) and node.op == "not":
            c, v = self.test(node.expr)
            if v is not _NOCONST:
                return ("false" if v else "true"), (not v)
            return "!(%s)" % c, _NOCONST
        e = self.expr(node)
        if _is_const(e) and not e.code.startswith("return Err("):
            truth = bool(e.const)
            if e.s and e.code.startswith("{ let _ ="):
                return e.code, truth
            return ("true" if truth else "false"), truth
        return self.truthy(e, node), _NOCONST

    def truthy(self, e: E, node: Any) -> str:
        t = e.t
        if t == BOOL:
            return e.code
        if t == INT:
            return "((%s) != 0)" % e.code
        if t == FLOAT:
            return "((%s) != 0.0)" % e.code
        if t == STR:
            return "((%s) != S_EMPTY)" % e.code
        if t.kind == "opt":
            inner = t.args[0]
            if inner in (VAL, MR, SPEC, INSN, FIELDS) or inner.kind in ("rec", "tup"):
                return "(%s).is_some()" % e.code
            v = self.fresh("o")
            return "{ let %s = %s; match %s { Some(__x) => %s, None => false } }" % (
                v,
                e.code,
                v,
                self.truthy(E("__x", inner), node),
            )
        if t.kind == "vtup":
            return "!(%s).is_empty()" % e.code
        if t.kind == "stk":
            return "(stk_len_%s(s) != 0)" % t.args[0]
        if t in (VAL, MR, SPEC, INSN, FIELDS, REGVIEW, ERASED, TABLE, FN) or t.kind in (
            "rec",
            "tup",
        ):
            return "true"
        if t == SPECVIEW:
            return "(%s).is_some()" % e.code
        if t == NONE:
            return "false"
        raise self.fail(node, "truth value of %r" % (t,))

    # -- expressions ---------------------------------------------------------

    def expr(self, e: Any) -> E:
        kind = type(e).__name__
        method = getattr(self, "e_" + kind, None)
        if method is None:
            raise self.fail(e, "unsupported expression %s" % kind)
        return method(e)

    def e_IntExpr(self, e: mn.IntExpr) -> E:
        t = self.mtype(e)
        if t == FLOAT:
            return E(lit_float(float(e.value)), FLOAT, const=e.value)
        return E(lit_int(e.value), INT, const=e.value)

    def e_FloatExpr(self, e: mn.FloatExpr) -> E:
        return E(lit_float(e.value), FLOAT, const=e.value)

    def e_StrExpr(self, e: mn.StrExpr) -> E:
        return E(self.tr.syms.ident(e.value), STR, const=e.value)

    def e_NameExpr(self, e: mn.NameExpr) -> E:
        node = e.node
        fullname = e.fullname
        if fullname == "builtins.True":
            return E("true", BOOL, const=True)
        if fullname == "builtins.False":
            return E("false", BOOL, const=False)
        if fullname == "builtins.None":
            return E("()", NONE, const=None)
        if isinstance(node, mn.Var) and e.kind == mn.LDEF:
            if e.name in self.sig.state_params:
                return E("()", STATE, const=_NOCONST)
            name, t = self.locals.get(id(node)) or self.declare(node)
            if id(node) in self.env and isinstance(self.env[id(node)], PS):
                ps = self.env[id(node)]
                narrowed = self.mtype(e)
                out = E(name, t, const=_NOCONST, pts=self.core.pts.get(id(e)))
                out.parts = ps.parts
                if narrowed != ANY and narrowed != t and t not in (FN, TABLE, STATE):
                    return E(
                        self.coerce(out, narrowed, e),
                        narrowed,
                        out.s,
                        _NOCONST,
                        out.pts,
                        parts=ps.parts,
                    )
                return out
            if id(node) in self.env:
                value = self.env[id(node)]
                narrowed = self.mtype(e)
                tt = narrowed if narrowed != ANY else t
                if t in (
                    FN,
                    TABLE,
                    STATE,
                    REGVIEW,
                    SPECVIEW,
                    FIELDS,
                    INSN,
                    CONFIG_MAP,
                    CFGSET,
                ):
                    tt = t
                return self.static_e(value, tt, e)
            narrowed = self.mtype(e)
            out = E(name, t, const=_NOCONST, pts=self.core.pts.get(id(e)))
            if narrowed != ANY and narrowed != t and t not in (FN, TABLE, STATE):
                return E(
                    self.coerce(out, narrowed, e), narrowed, out.s, _NOCONST, out.pts
                )
            return out
        if isinstance(node, mn.FuncDef):
            name = node.fullname
            const: Any = _NOCONST
            if self.pe:
                # A module function named directly: its value is static.
                live = self.core.live_value(name)
                if isinstance(live, pytypes.FunctionType):
                    const = live
            return E(
                self.tr.fn_const(name),
                FN,
                const=const,
                pts=frozenset({infer.Obj("fn", name)}),
            )
        if isinstance(node, mn.Var) and e.kind == mn.GDEF:
            if fullname in FLOAT_FUNCS:
                return E(
                    self.tr.fn_const(fullname),
                    FN,
                    const=_NOCONST,
                    pts=frozenset({infer.Obj("builtin", fullname)}),
                )
            if fullname == "sharc_core.state.NO_SPECIAL":
                return E("SpecView::EMPTY", SPECVIEW, const=_NOCONST)
            t = self.mtype(e)
            return self.tr.module_const(fullname, t, self.where(e))
        if isinstance(node, mn.TypeInfo):
            return E(node.fullname, T("class", (node.fullname,)), const=_NOCONST)
        if fullname == "builtins.abs":
            return E(
                self.tr.fn_const("builtins.abs"),
                FN,
                const=_NOCONST,
                pts=frozenset({infer.Obj("builtin", "abs")}),
            )
        raise self.fail(e, "unsupported name %s (%s)" % (e.name, fullname))

    def e_MemberExpr(self, e: mn.MemberExpr) -> E:
        if (
            e.fullname
            and isinstance(e.expr, mn.NameExpr)
            and isinstance(e.expr.node, mn.MypyFile)
        ):
            value = self.core.live_value(e.fullname)
            t = self.mtype(e)
            return E(self.tr.const_value(value, t, self.where(e)), t, const=value)
        base = self.expr(e.expr)
        attr = e.name
        bt = base.t
        if (
            self.pe
            and _is_const(base)
            and not isinstance(base.const, Tag)
            and bt not in (STATE, TABLE, CONFIG_MAP, CFGSET)
            and hasattr(base.const, attr)
        ):
            value = getattr(base.const, attr)
            if not callable(value) or isinstance(value, pytypes.FunctionType):
                t = self.mtype(e)
                if attr == "fields" and type(base.const).__name__ == "Instruction":
                    t = FIELDS
                return self.static_e(value, t, e)
        if bt == STATE:
            if self.pe and attr in (
                "follow_loaded_calls",
                "continue_external_calls",
                "max_call_depth",
                "assume_nw32",
                "explicit_memory_model",
                "data_memory_tainted",
                "dossier_bytes",
                "provisional_forms",
                "provisional_interpretations",
            ):
                value = self.tr.gen_cfg[attr]
                t = STATE_ATTRS[attr][0] if attr in STATE_ATTRS else ANY
                if attr == "provisional_interpretations":
                    return E("()", CONFIG_MAP, const=value)
                if attr == "provisional_forms":
                    return E("()", CFGSET, const=value)
                return self.static_e(value, t, e)
            if self.pe and attr in self.facts:
                t = STATE_ATTRS[attr][0]
                return self.static_e(self.facts[attr], t, e)
            if self.pe and attr == "uregs":
                return E("RegView::CUR", REGVIEW, const=Tag("RegView::CUR"))
            if self.pe and attr == "special":
                return E("SpecView::CUR", SPECVIEW, const=Tag("SpecView::CUR"))
            if self.pe and attr == "concrete":
                return E("()", CONCRETE, const=Tag("()"))
            if attr == "uregs":
                return E("RegView::CUR", REGVIEW, const=_NOCONST)
            if attr == "special":
                return E("SpecView::CUR", SPECVIEW, const=_NOCONST)
            if attr in STACKS:
                return E("()", STK(attr), const=_NOCONST)
            if attr == "trace":
                return E("()", ERASED, const=_NOCONST)
            if attr in STATE_ATTRS:
                t, code, _w = STATE_ATTRS[attr]
                if self.blk and attr in BLK_RF_ATTRS:
                    code = BLK_RF_ATTRS[attr]
                out = E(code, t, s=code.startswith(("s.", "rf.")), const=_NOCONST)
                narrowed = self.mtype(e)
                if (
                    narrowed != t
                    and narrowed != ANY
                    and t not in (CONCRETE, TABLE, CONFIG_MAP, CFGSET)
                ):
                    return E(self.coerce(out, narrowed, e), narrowed, out.s, _NOCONST)
                return out
            raise self.fail(e, "state attribute %s is not modelled natively" % attr)
        if bt == INSN:
            m = {
                "type_name": ("%s.type_name", STR),
                "fields": ("%s.fields", FIELDS),
                "length_bytes": ("%s.length_bytes", OPT(INT)),
                "kind": ("%s.kind", STR),
                "note": ("SYM_DYN", STR),
                "offset": ("%s.offset", INT),
            }
            if attr not in m:
                raise self.fail(e, "Instruction.%s" % attr)
            code, t = m[attr]
            narrowed = self.mtype(e)
            out = E(code % base.code if "%s" in code else code, t, base.s, _NOCONST)
            if narrowed != t and narrowed != ANY:
                return E(self.coerce(out, narrowed, e), narrowed, base.s, _NOCONST)
            return out
        if bt == VAL:
            if attr == "value":
                return E("(%s).val()" % base.code, INT, base.s, _NOCONST)
            if attr == "mask":
                return E("((%s).m as Int)" % base.code, INT, base.s, _NOCONST)
            if attr == "bits":
                return E("((%s).b as Int)" % base.code, INT, base.s, _NOCONST)
            if attr == "reason":
                return E("SYM_DYN", STR, const=_NOCONST)
            raise self.fail(e, "value attribute %s" % attr)
        if bt == MR:
            if attr in ("mask", "bits"):
                return E("(%s).%s" % (base.code, attr), INT, base.s, _NOCONST)
            if attr == "known":
                return E("(%s).known()" % base.code, BOOL, base.s, _NOCONST)
        if bt.kind == "rec":
            flist = self.tr.record_fields(bt.args[0])
            fields = dict(flist)
            if attr in fields and self.pe and base.parts is not None:
                item = base.parts[[f for f, _ in flist].index(attr)]
                if item is not _NOCONST and not isinstance(item, PS):
                    return self.static_e(item, fields[attr], e)
            if attr in fields:
                t = fields[attr]
                code = "(%s).%s" % (base.code, attr)
                if attr in NARROW_FIELDS.get(bt.args[0], ()):
                    code = "(%s as Int)" % code
                out = E(code, t, base.s, _NOCONST)
                if (
                    self.pe
                    and bt.args[0] == "Loop"
                    and attr == "end_sw"
                    and self.tr.loop_ends is not None
                ):
                    out.vset = self.tr.loop_ends
                narrowed = self.mtype(e)
                if narrowed != t and narrowed != ANY and t != FN:
                    return E(self.coerce(out, narrowed, e), narrowed, base.s, _NOCONST)
                return out
            raise self.fail(e, "record %s has no field %s" % (bt.args[0], attr))
        if bt == ANY:
            raise self.fail(e, "attribute %s of an untyped value" % attr)
        # A method reference: e_CallExpr handles it.
        return E(base.code, T("method", (attr, bt)), base.s, _NOCONST, base.pts)

    def e_OpExpr(self, e: mn.OpExpr) -> E:
        op = e.op
        rt = self.mtype(e)
        if op in ("and", "or"):
            left = self.expr(e.left)
            if _is_const(left) and not left.code.startswith("return Err("):
                truth = bool(left.const)
                if (op == "or") == truth:
                    return left
                return self.expr(e.right)
            right = self.expr(e.right)
            s = left.s or right.s
            if left.t == BOOL and right.t == BOOL:
                return E(
                    "(%s %s %s)"
                    % (left.code, "&&" if op == "and" else "||", right.code),
                    BOOL,
                    s,
                    _NOCONST,
                )
            if rt == ANY:
                raise self.fail(e, "untyped boolean operator")
            if left.t == FN or right.t == FN:
                rt = FN
            v = self.fresh("b")
            test = self.truthy(E(v, left.t), e)
            if op == "or":
                # The left value is only taken when it is truthy, so an
                # Optional left is Some there.
                taken = (
                    self.coerce_code("%s.unwrap()" % v, left.t.args[0], rt, e)
                    if left.t.kind == "opt" and rt.kind != "opt"
                    else self.coerce_code(v, left.t, rt, e)
                )
                code = "{ let %s = %s; if %s { %s } else { %s } }" % (
                    v,
                    left.code,
                    test,
                    taken,
                    self.coerce(right, rt, e),
                )
            else:
                code = "{ let %s = %s; if !(%s) { %s } else { %s } }" % (
                    v,
                    left.code,
                    test,
                    self.coerce_code(v, left.t, rt, e),
                    self.coerce(right, rt, e),
                )
            return E(
                code,
                rt,
                s,
                _NOCONST,
                (left.pts or frozenset()) | (right.pts or frozenset()),
            )
        left = self.expr(e.left)
        right = self.expr(e.right)
        s = left.s or right.s
        if (
            _is_const(left)
            and _is_const(right)
            and isinstance(left.const, (int, float, str))
            and isinstance(right.const, (int, float, str, tuple))
            and op in _PY_BINOPS
            and (self.pe or op != "%")
            and not (isinstance(right.const, tuple) and not isinstance(left.const, str))
        ):
            try:
                value = _PY_BINOPS[op](left.const, right.const)
            except Exception:
                value = _NOCONST
            if isinstance(value, (int, float, str)):
                t = rt if rt != ANY else (STR if isinstance(value, str) else INT)
                if isinstance(value, bool):
                    value = int(value)
                return E(self.tr.const_value(value, t, self.where(e)), t, const=value)
        if left.t == STR or right.t == STR:
            if op == "%":
                return E("SYM_DYN", STR, const=_NOCONST)
            if op == "+":
                if _is_const(right) and isinstance(right.const, str) and left.t == STR:
                    fn = self.tr.sym_concat_fn(right.const)
                    return E("%s(%s)" % (fn, left.code), STR, left.s, _NOCONST)
                return E("SYM_DYN", STR, const=_NOCONST)
            raise self.fail(e, "string operator %s" % op)
        if LSTATE in (left.t, right.t):
            if left.code.startswith("return Err(") or right.code.startswith(
                "return Err("
            ):
                return E(
                    left.code if left.code.startswith("return Err(") else right.code,
                    LSTATE,
                )
            raise self.fail(
                e, "list-of-states operator on a path that does not trap first"
            )
        if op == "+" and (
            left.t.kind in ("tup", "vtup") or right.t.kind in ("tup", "vtup")
        ):
            if rt.kind == "vtup":
                return E(
                    "tup_concat(%s, %s)"
                    % (self.coerce(left, rt, e), self.coerce(right, rt, e)),
                    rt,
                    s,
                    _NOCONST,
                )
            raise self.fail(e, "tuple concatenation to %r" % (rt,))
        if rt == BOOL and op in ("&", "|", "^"):
            return E(
                "(%s %s %s)"
                % (self.coerce(left, BOOL, e), op, self.coerce(right, BOOL, e)),
                BOOL,
                s,
                _NOCONST,
            )
        if rt == ANY:
            raise self.fail(e, "untyped arithmetic")
        if op == "/":
            return E(
                "fdiv(%s, %s)?"
                % (self.coerce(left, FLOAT, e), self.coerce(right, FLOAT, e)),
                FLOAT,
                s,
                _NOCONST,
            )
        if rt == FLOAT:
            lc = self.coerce(left, FLOAT, e)
            rc = self.coerce(right, FLOAT, e)
            if op in ("+", "-", "*"):
                # rt.rs fadd/fsub/fmul keep Python's NaN operand order.
                fn = {"+": "fadd", "-": "fsub", "*": "fmul"}[op]
                return E("%s(%s, %s)" % (fn, lc, rc), FLOAT, s, _NOCONST)
            if op == "%":
                return E("py_fmod(%s, %s)" % (lc, rc), FLOAT, s, _NOCONST)
            if op == "**":
                return E("(%s).powf(%s)" % (lc, rc), FLOAT, s, _NOCONST)
            raise self.fail(e, "float operator %s" % op)
        if rt == INT:
            lc = self.coerce(left, INT, e)
            rc = self.coerce(right, INT, e)
            if op in ("+", "-", "*", "&", "|", "^"):
                return E("(%s %s %s)" % (lc, op, rc), INT, s, _NOCONST)
            if op == "<<":
                return E("shl(%s, %s)?" % (lc, rc), INT, s, _NOCONST)
            if op == ">>":
                return E("shr(%s, %s)?" % (lc, rc), INT, s, _NOCONST)
            if op == "//":
                return E("floordiv(%s, %s)?" % (lc, rc), INT, s, _NOCONST)
            if op == "%":
                return E("pymod(%s, %s)?" % (lc, rc), INT, s, _NOCONST)
            if op == "**":
                return E("ipow(%s, %s)?" % (lc, rc), INT, s, _NOCONST)
        raise self.fail(e, "operator %s on %r, %r -> %r" % (op, left.t, right.t, rt))

    def e_UnaryExpr(self, e: mn.UnaryExpr) -> E:
        if e.op == "not":
            code, truth = self.test(e.expr)
            if truth is not _NOCONST:
                return E("false" if truth else "true", BOOL, const=not truth)
            return E("!(%s)" % code, BOOL, False, _NOCONST)
        v = self.expr(e.expr)
        rt = self.mtype(e)
        if (
            _is_const(v)
            and isinstance(v.const, (int, float))
            and not isinstance(v.const, bool)
        ):
            c: Any = v.const
            value = -c if e.op == "-" else (~c if e.op == "~" else c)
            return E(self.tr.const_value(value, rt, self.where(e)), rt, const=value)
        if e.op == "-":
            t = rt if rt in (INT, FLOAT) else INT
            return E("(-%s)" % self.coerce(v, t, e), t, v.s, _NOCONST)
        if e.op == "~":
            return E("(!%s)" % self.coerce(v, INT, e), INT, v.s, _NOCONST)
        if e.op == "+":
            return v
        raise self.fail(e, "unary %s" % e.op)

    def e_ConditionalExpr(self, e: mn.ConditionalExpr) -> E:
        test, tv = self.test(e.cond)
        if tv is not _NOCONST:
            return self.expr(e.if_expr if tv else e.else_expr)
        a = self.expr(e.if_expr)
        b = self.expr(e.else_expr)
        rt = self.mtype(e)
        if a.t == FN or b.t == FN:
            rt = FN
        if a.t == STATE or b.t == STATE:
            rt = STATE
        if rt == ANY:
            raise self.fail(e, "untyped conditional expression")
        code = "(if %s { %s } else { %s })" % (
            test,
            self.coerce(a, rt, e),
            self.coerce(b, rt, e),
        )
        return E(
            code,
            rt,
            True,
            _NOCONST,
            (a.pts or frozenset()) | (b.pts or frozenset()),
        )

    def e_TupleExpr(self, e: mn.TupleExpr) -> E:
        items = [self.expr(i) for i in e.items]
        rt = self.mtype(e)
        if rt.kind == "tup" and len(rt.args) == len(items):
            parts = [self.coerce(i, t, e) for i, t in zip(items, rt.args, strict=True)]
            rt = TUP(
                *[
                    i.t if t in (FN, TABLE) else t
                    for i, t in zip(items, rt.args, strict=True)
                ]
            )
        else:
            rt = TUP(*[i.t for i in items])
            parts = [i.code for i in items]
        code = "(%s,)" % parts[0] if len(parts) == 1 else "(%s)" % ", ".join(parts)
        const: Any = _NOCONST
        ps: tuple | None = None
        if all(_is_const(i) for i in items):
            const = tuple(i.const for i in items)
        elif self.pe:
            ps = tuple(_ps_item(i) for i in items)
            if all(p is _NOCONST for p in ps):
                ps = None
        return E(code, rt, any(i.s for i in items), const, parts=ps)

    def e_ListExpr(self, e: mn.ListExpr) -> E:
        rt = self.mtype(e)
        if rt == LSTATE:
            if len(e.items) != 1:
                raise self.fail(e, "a successor list of %d states" % len(e.items))
            item = self.expr(e.items[0])
            if item.code.startswith("return Err("):
                return E(item.code, LSTATE, item.s, _NOCONST)
            if item.t != STATE:
                raise self.fail(e, "successor list of %r" % (item.t,))
            return E("()", LSTATE, const=_NOCONST)
        if rt.kind != "vtup":
            raise self.fail(e, "list of %r" % (rt,))
        items = [self.expr(i) for i in e.items]
        parts = [self.coerce(i, rt.args[0], e) for i in items]
        return E(
            "Tup::from_slice(&[%s])" % ", ".join(parts),
            rt,
            any(i.s for i in items),
            _NOCONST,
        )

    def e_IndexExpr(self, e: mn.IndexExpr) -> E:
        base = self.expr(e.base)
        rt = self.mtype(e)
        if isinstance(e.index, mn.SliceExpr):
            sl = e.index
            lo = (
                self.coerce(self.expr(sl.begin_index), INT, e)
                if sl.begin_index
                else "0"
            )
            hi = (
                self.coerce(self.expr(sl.end_index), INT, e)
                if sl.end_index
                else "Int::MAX"
            )
            if sl.stride is not None:
                raise self.fail(e, "strided slice")
            if base.t.kind == "vtup":
                return E(
                    "(%s).slice(%s, %s)" % (base.code, lo, hi), base.t, True, _NOCONST
                )
            raise self.fail(e, "slice of %r" % (base.t,))
        if (
            base.t == TABLE
            and _is_const(base)
            and isinstance(e.index, mn.OpExpr)
            and e.index.op == "%"
            and isinstance(e.index.left, mn.StrExpr)
            and not (self.pe and _is_const(self.expr(e.index.right)))
        ):
            return self.format_key_lookup(e, base, e.index, rt)
        idx = self.expr(e.index)
        if (
            self.pe
            and _is_const(base)
            and _is_const(idx)
            and not isinstance(base.const, Tag)
            and not isinstance(idx.const, Tag)
            and not base.code.startswith("return Err(")
            and not idx.code.startswith("return Err(")
        ):
            return self.py_eval(e, rt, lambda b, i: b[i], base.const, idx.const)
        if base.t == TABLE:
            return self.table_lookup(e, base, idx, rt, missing="trap")
        if (
            self.pe
            and base.parts is not None
            and _is_const(idx)
            and isinstance(idx.const, int)
            and -len(base.parts) <= idx.const < len(base.parts)
        ):
            item = base.parts[idx.const]
            if item is not _NOCONST and not isinstance(item, PS):
                t = rt if rt != ANY else _type_of_value(item)
                return self.static_e(item, t, e)
        if base.t == REGVIEW:
            if self.blk:
                return self.rf_read(base, idx, e)
            return E(
                self.call_code("rv_get", [base.code, self.coerce(idx, INT, e)], False),
                VAL,
                True,
                _NOCONST,
            )
        if base.t == FIELDS:
            return E(
                "%s.key(%s)?" % (base.code, self.coerce(idx, STR, e)),
                INT,
                base.s or idx.s,
                _NOCONST,
            )
        if base.t.kind == "stk":
            name = base.t.args[0]
            return E(
                self.call_code("stk_at_%s" % name, [self.coerce(idx, INT, e)], True),
                STACKS[name],
                True,
                _NOCONST,
            )
        if base.t.kind == "tup":
            if _is_const(idx) and isinstance(idx.const, int):
                i = idx.const
                n = len(base.t.args)
                if i < 0:
                    i += n
                if not 0 <= i < n:
                    raise self.fail(e, "tuple index out of range")
                return E("(%s).%d" % (base.code, i), base.t.args[i], base.s, _NOCONST)
            if len(set(base.t.args)) == 1:
                v = self.fresh("i")
                items = ", ".join("%s.%d" % (v, i) for i in range(len(base.t.args)))
                return E(
                    "{ let %s = %s; tup_index(&[%s], %s)? }"
                    % (v, base.code, items, self.coerce(idx, INT, e)),
                    base.t.args[0],
                    base.s or idx.s,
                    _NOCONST,
                )
            raise self.fail(e, "dynamic index into a mixed tuple %r" % (base.t,))
        if base.t.kind == "vtup":
            return E(
                "(%s).at(%s)?" % (base.code, self.coerce(idx, INT, e)),
                base.t.args[0],
                base.s or idx.s,
                _NOCONST,
            )
        raise self.fail(e, "indexing %r" % (base.t,))

    def _tables_of(self, e: Any, base: E) -> list[Any]:
        if _is_const(base):
            return [base.const]
        objs = base.pts or frozenset()
        tables = [self.core.consts[o.key] for o in objs if o.kind == "const"]
        if not tables:
            raise self.fail(e, "table value with no known tables")
        # A stable order (generated code must not depend on set order).
        return sorted(tables, key=_stable_key)

    def table_lookup(
        self, e: Any, base: E, idx: E, rt: T, missing: str, default: E | None = None
    ) -> E:
        """A lookup in a constant table (or a table-valued variable).
        MISSING is "trap" (``t[k]``) or "get" (``t.get(k[, default])``); RT
        is the mypy type of the whole expression."""
        tables = self._tables_of(e, base)
        vt = rt
        if missing == "get" and default is None:
            vt = rt.args[0] if rt.kind == "opt" else rt
        pts_objs: set = set()
        for table in tables:
            values = (
                table.values()
                if isinstance(table, (dict, pytypes.MappingProxyType))
                else table
            )
            for v in values:
                o = infer._obj_of_live(self.core, v)
                if o is not None and o.kind in ("fn", "builtin", "const"):
                    pts_objs.add(o)
        pts = frozenset(pts_objs) if pts_objs else None
        if vt == FN or (vt.kind == "opt" and vt.args[0] == FN):
            vt = FN if vt == FN else vt
        if (
            vt == TABLE
            or vt == ANY
            and all(
                isinstance(v, (dict, pytypes.MappingProxyType))
                for t in tables
                for v in (t.values() if isinstance(t, dict) else [])
            )
        ):
            vt = TABLE
        # A constant key into one table: fold.
        if _is_const(idx) and len(tables) == 1:
            table = tables[0]
            key = idx.const
            if isinstance(table, (dict, pytypes.MappingProxyType)) and not isinstance(
                key, list
            ):
                if key in table:
                    value = table[key]
                    if isinstance(value, (dict, frozenset, pytypes.MappingProxyType)):
                        return E("%d" % self.tr.table_id(value), TABLE, const=value)
                    if isinstance(value, pytypes.FunctionType):
                        name = self.core.live_funcs[id(value)]
                        code = self.tr.fn_const(name)
                        out_t: T = FN
                        if missing == "get" and default is None:
                            code = "Some(%s)" % code
                            out_t = OPT(FN)
                        return E(
                            code,
                            out_t,
                            const=_NOCONST,
                            pts=frozenset({infer.Obj("fn", name)}),
                        )
                    code = self.tr.const_value(value, vt, self.where(e))
                    if missing == "get" and default is None and rt.kind == "opt":
                        code = "Some(%s)" % code
                    return E(code, rt, const=value)
                if missing == "trap":
                    return E(
                        self.trap_expr(e, "KeyError %r" % (key,)), rt, const=_NOCONST
                    )
                if default is not None:
                    return default
                return E("None", rt, const=None)
        fn = self.tr.lookup_fn(tables, vt, idx.t, self.where(e))
        key = idx.code
        if len(tables) > 1:
            call = "%s(%s, %s)" % (fn, base.code, key)
        else:
            call = "%s(%s)" % (fn, key)
        s = base.s or idx.s
        if missing == "trap":
            return E(
                "%s.ok_or(%s)?" % (call, self.tr.trap(self.where(e), "KeyError")),
                vt,
                s,
                _NOCONST,
                pts,
            )
        if default is not None:
            return E(
                "(match %s { Some(__v) => __v, None => %s })"
                % (call, self.coerce(default, vt, e)),
                vt,
                True,
                _NOCONST,
                pts,
            )
        return E(call, OPT(vt), s, _NOCONST, pts)

    def format_key_lookup(self, e: Any, base: E, fmt: mn.OpExpr, rt: T) -> E:
        table = base.const
        assert isinstance(fmt.left, mn.StrExpr)
        pattern = fmt.left.value
        if pattern.count("%d") != 1 or "%" in pattern.replace("%d", ""):
            raise self.fail(e, "key format %r" % pattern)
        arg = self.expr(fmt.right)
        rx = re.compile("^" + re.escape(pattern).replace("%d", "(-?[0-9]+)") + "$")
        arms = []
        for key, value in table.items():
            if isinstance(key, str):
                m = rx.match(key)
                if m:
                    arms.append(
                        "%s => %s"
                        % (
                            lit_int(int(m.group(1))),
                            self.tr.const_value(value, rt, self.where(e)),
                        )
                    )
        arms.append("_ => %s" % self.trap_expr(e, "KeyError (formatted key)"))
        return E(
            "(match %s { %s })" % (self.coerce(arg, INT, e), ", ".join(arms)),
            rt,
            arg.s,
            _NOCONST,
        )

    def e_ComparisonExpr(self, e: mn.ComparisonExpr) -> E:
        parts = []
        s = False
        operands = [self.expr(o) for o in e.operands]
        if self.pe and len(e.operators) == 1 and e.operators[0] in ("==", "!="):
            a, b = operands
            for x, y in ((a, b), (b, a)):
                if (
                    _is_const(x)
                    and isinstance(x.const, int)
                    and y.vset is not None
                    and x.const not in y.vset
                ):
                    value = e.operators[0] == "!="
                    code = "{ let _ = %s; %s }" % (y.code, "true" if value else "false")
                    return E(code, BOOL, True, value)
        if self.pe and all(
            _is_const(o) and not o.code.startswith("return Err(") for o in operands
        ):
            values = [o.const for o in operands]
            if not any(isinstance(v, Tag) for v in values) or all(
                op in ("is", "is not") for op in e.operators
            ):
                result = True
                for i, op in enumerate(e.operators):
                    if not _PY_COMPARE[op](values[i], values[i + 1]):
                        result = False
                        break
                return E("true" if result else "false", BOOL, const=result)
        codes = [o.code for o in operands]
        pre = []
        for i in range(1, len(operands) - 1):
            v = self.fresh("m")
            pre.append("let %s = %s;" % (v, codes[i]))
            codes[i] = v
        for i, op in enumerate(e.operators):
            a, b = operands[i], operands[i + 1]
            a = E(codes[i], a.t, a.s, a.const, a.pts)
            b = E(codes[i + 1], b.t, b.s, b.const, b.pts)
            parts.append(self.compare(e, op, a, b, e.operands[i + 1]))
            s = s or a.s or b.s
        code = " && ".join("(%s)" % p for p in parts)
        if pre:
            code = "{ %s %s }" % (" ".join(pre), code)
        return E("(%s)" % code, BOOL, s, _NOCONST)

    def compare(self, e: Any, op: str, a: E, b: E, bn: Any) -> str:
        if op in ("is", "is not"):
            neg = op == "is not"
            if b.t == NONE or a.t == NONE:
                x = a if b.t == NONE else b
                if x.t.kind == "opt":
                    return "(%s).%s()" % (x.code, "is_some" if neg else "is_none")
                if x.t == SPECVIEW:
                    return "(%s).%s()" % (x.code, "is_some" if neg else "is_none")
                if x.t == NONE:
                    return "false" if neg else "true"
                if x.t == CONCRETE:
                    return "%s(s.cfg.has_concrete)" % ("" if neg else "!")
                return "true" if neg else "false"
            if _is_const(b) and isinstance(b.const, bool):
                lit = "true" if b.const else "false"
                if a.t == OPT(BOOL):
                    return "(%s %s Some(%s))" % (a.code, "!=" if neg else "==", lit)
                if a.t == BOOL:
                    return "(%s %s %s)" % (a.code, "!=" if neg else "==", lit)
            if a.t == STATE and b.t == STATE:
                return "true" if not neg else "false"
            raise self.fail(e, "identity comparison of %r and %r" % (a.t, b.t))
        if op in ("in", "not in"):
            code = self.membership(e, a, b, bn)
            return "!(%s)" % code if op == "not in" else code
        rop = op
        at, bt = a.t, b.t
        if at == bt and at in (INT, FLOAT, BOOL, STR, FN):
            return "(%s %s %s)" % (a.code, rop, b.code)
        if {at, bt} <= {INT, BOOL}:
            return "(%s %s %s)" % (self.coerce(a, INT, e), rop, self.coerce(b, INT, e))
        if {at, bt} <= {INT, FLOAT, BOOL}:
            return "(%s %s %s)" % (
                self.coerce(a, FLOAT, e),
                rop,
                self.coerce(b, FLOAT, e),
            )
        if rop in ("==", "!="):
            if at.kind == "opt" and bt == at.args[0]:
                return "(%s %s Some(%s))" % (a.code, rop, b.code)
            if bt.kind == "opt" and at == bt.args[0]:
                return "(Some(%s) %s %s)" % (a.code, rop, b.code)
            if at == bt:
                return "(%s %s %s)" % (a.code, rop, b.code)
            if at.kind == "opt" and bt.kind == "opt" and self.can(bt, at):
                return "(%s %s %s)" % (a.code, rop, self.coerce(b, at, e))
            if at == OPT(BOOL) and bt == INT:
                return "(%s.map(|__x| __x as Int) %s Some(%s))" % (a.code, rop, b.code)
        raise self.fail(e, "comparison %s of %r and %r" % (op, at, bt))

    def membership(self, e: Any, a: E, b: E, bn: Any) -> str:
        if isinstance(bn, (mn.TupleExpr, mn.ListExpr, mn.SetExpr)):
            v = self.fresh("k")
            items = [self.expr(i) for i in bn.items]
            tests = [self.compare(e, "==", E(v, a.t), item, None) for item in items]
            return "{ let %s = %s; %s }" % (v, a.code, " || ".join(tests) or "false")
        if b.t == TABLE:
            tables = self._tables_of(e, b)
            if len(tables) != 1:
                raise self.fail(e, "membership in a table variable")
            table = tables[0]
            if _is_const(a) and not isinstance(a.const, list):
                return "true" if a.const in table else "false"
            fn = self.tr.contains_fn(table, a.t, self.where(e))
            return "%s(%s)" % (fn, a.code)
        if b.t == CONFIG_MAP:
            return "s.cfg.provisional_interp_contains(%s)" % self.coerce(a, STR, e)
        if b.t == CFGSET:
            return "s.cfg.provisional_form(%s)" % self.coerce(a, STR, e)
        if b.t == SPECVIEW:
            return self.call_code(
                "sv_contains", [b.code, self.coerce(a, STR, e)], False
            )
        if b.t.kind == "tup":
            if len(set(b.t.args)) != 1:
                raise self.fail(e, "membership in a mixed tuple")
            v = self.fresh("k")
            w = self.fresh("tu")
            tests = [
                self.compare(e, "==", E(v, a.t), E("%s.%d" % (w, i), b.t.args[i]), None)
                for i in range(len(b.t.args))
            ]
            return "{ let %s = %s; let %s = %s; %s }" % (
                v,
                a.code,
                w,
                b.code,
                " || ".join(tests),
            )
        if b.t.kind == "vtup":
            return "(%s).contains(%s)" % (b.code, self.coerce(a, b.t.args[0], e))
        if b.t == FIELDS:
            return "%s.contains(%s)" % (b.code, self.coerce(a, STR, e))
        raise self.fail(e, "membership in %r" % (b.t,))

    def e_CastExpr(self, e: Any) -> E:
        return self.expr(e.expr)

    def e_ListComprehension(self, e: Any) -> E:
        raise self.fail(e, "list comprehension outside observability")

    def e_GeneratorExpr(self, e: Any) -> E:
        raise self.fail(e, "generator expression outside observability")

    def e_DictExpr(self, e: Any) -> E:
        raise self.fail(e, "dict built inside a function")

    def e_LambdaExpr(self, e: Any) -> E:
        raise self.fail(e, "lambda")

    # -- calls -------------------------------------------------------------------

    def call_code(
        self, path: str, args: list[str], traps: bool, sargs: tuple = ("s",)
    ) -> str:
        """``path(s, args...)`` with every argument that uses the state
        evaluated first (``s`` is borrowed by the call itself). SARGS are
        the state arguments (block mode adds the register file ``rf``)."""
        pre = []
        names = []
        uses = _USES_SRF if self.blk else _USES_S
        for a in args:
            if uses.search(a):
                v = self.fresh("a")
                pre.append("let %s = %s;" % (v, a))
                names.append(v)
            else:
                names.append(a)
        call = "%s(%s)" % (path, ", ".join(list(sargs) + names))
        if traps:
            call += "?"
        if pre:
            return "{ %s %s }" % (" ".join(pre), call)
        return call

    def bind_args(self, e: mn.CallExpr, sig: FnSig) -> list[E | None]:
        """E per parameter of SIG (None for a state parameter), with
        defaults filled in."""
        names = [p[0] for p in sig.params]
        bound: dict[str, Any] = {}
        for i, (arg, kind, aname) in enumerate(
            zip(e.args, e.arg_kinds, e.arg_names, strict=True)
        ):
            if kind == mn.ARG_POS:
                if i >= len(names):
                    raise self.fail(e, "too many arguments to %s" % sig.fullname)
                bound[names[i]] = arg
            elif kind == mn.ARG_NAMED:
                bound[str(aname)] = arg
            else:
                raise self.fail(e, "*args/**kwargs call")
        out: list[E | None] = []
        for pname, _pt, default in sig.params:
            node = bound.get(pname)
            if node is None:
                if default is None:
                    raise self.fail(
                        e, "missing argument %s to %s" % (pname, sig.fullname)
                    )
                node = default
            if pname in sig.state_params:
                v = self.expr(node)
                out.append(v if v.code.startswith("return Err(") else None)
                continue
            out.append(self.expr(node))
        return out

    def direct_call(self, e: mn.CallExpr, fullname: str, want: T | None = None) -> E:
        sig = self.tr.signature(fullname)
        if fullname.rpartition(".")[2].startswith("_build_"):
            raise self.fail(
                e, "generation-time builder %s called at run time" % fullname
            )
        args = self.bind_args(e, sig)
        return self.call_bound(e, fullname, sig, args)

    def call_bound(self, e: Any, fullname: str, sig: FnSig, args: list[E | None]) -> E:
        """A call of FULLNAME with its arguments already evaluated (one E
        per parameter, None for a state parameter)."""
        if self.blk and fullname == "sharc_core.state._write_ureg":
            destination = next(
                a
                for (name, _type, _default), a in zip(sig.params, args, strict=True)
                if name == "code"
            )
            if (
                destination is None
                or not _is_const(destination)
                or destination.const in (100, 101)
            ):
                raise self.fail(e, "block guest PC stack write requires interpreter")
        codes = []
        traps_first = []
        for (pname, _pt, _d), a in zip(sig.params, args, strict=True):
            if pname in sig.state_params and a is not None:
                traps_first.append(a.code)
            if a is not None and a.code.startswith("return Err("):
                traps_first.append(a.code)
        if traps_first:
            return E(traps_first[0], sig.ret)
        b = BOUNDARY.get(fullname)
        if self.pe:
            static = {}
            for (pname, _pt, _d), a in zip(sig.params, args, strict=True):
                if pname in sig.state_params or a is None:
                    continue
                if _is_const(a) and pname not in OBSERVABILITY_PARAMS:
                    static[pname] = a.const
                elif (
                    self.blk
                    and a.parts is not None
                    and _ps_parts(a) is not None
                    and b is None
                ):
                    static[pname] = PS(a.parts)
            dynamic_params = [
                p
                for p in sig.params
                if p[0] not in sig.state_params and p[0] not in static
            ]
            observability_only = all(
                p[0] in OBSERVABILITY_PARAMS for p in dynamic_params
            )
            view_params = any(
                p[1] in (REGVIEW, SPECVIEW, CONCRETE, CONFIG_MAP, CFGSET)
                for p in sig.params
            ) or any(isinstance(v, (Tag, PS)) for v in static.values())
            pure = (
                not sig.state_params
                and not view_params
                and (
                    b is None
                    or fullname in PURE_BOUNDARY
                    or fullname.startswith("sharc_core.values.")
                    or fullname.startswith("sharc_core.floats.")
                )
            )
            if pure and observability_only and fullname not in TRAP_CALLS:
                fn = self.core.live_value(fullname)
                kwargs = {}
                for pname, _pt, _d in sig.params:
                    kwargs[pname] = static.get(pname, "<dyn>")
                return self.py_eval(e, sig.ret, lambda: fn(**kwargs))
            # Tag-valued views: the value is known, the call has no effect.
            if fullname == "sharc_core.state._snapshot_uregs" and "uregs" in static:
                tag = static["uregs"]
                code = "RegView(%s.0 | 1)" % tag.code
                return E(code, REGVIEW, const=Tag(code))
            if fullname == "sharc_core.state._pey_view" and "values" in static:
                code = "RegView(%s.0 | RegView::PEY)" % static["values"].code
                return E(code, REGVIEW, const=Tag(code))
            if fullname == "sharc_core.state._pey_special" and "special" in static:
                sv = static["special"]
                cur = isinstance(sv, Tag) and sv.code == "SpecView::CUR"
                code = "SpecView::PEY_CUR" if cur else "SpecView::PEY_EMPTY"
                return E(code, SPECVIEW, const=Tag(code))
            fast = self.fast_float_call(e, fullname, sig, args, static)
            if fast is not None:
                return fast
            if b is None and (static or self.facts or self.blk):
                facts = dict(self.facts)
                path = self.tr.variant(fullname, static, facts)
                changed = self.tr.variant_effects.get(path, set(FACT_ATTRS))
                self.effects |= changed
                for attr in changed:
                    self.facts.pop(attr, None)
                for attr, value in self.tr.variant_facts_out.get(path, {}).items():
                    if attr in changed:
                        self.facts[attr] = value
                self.merge_regs(path)
                dyn_codes = []
                for (pname, pt, _d), a in zip(sig.params, args, strict=True):
                    if pname in sig.state_params:
                        continue
                    if pname in static and not isinstance(static[pname], PS):
                        continue
                    assert a is not None
                    dyn_codes.append(self.coerce(a, pt, e))
                sargs = ("s", "rf") if self.blk else ("s",)
                if path in self.tr.variant_inline:
                    return E(self.tr.variant_inline[path], sig.ret, True, _NOCONST)
                rc = self.tr.variant_const.get(path, _NOCONST)
                if rc is not _NOCONST:
                    if isinstance(rc, Tag):
                        return E(rc.code, sig.ret, const=rc)
                    return self.static_e(rc, sig.ret, e)
                return E(
                    self.call_code(path, dyn_codes, True, sargs),
                    sig.ret,
                    True,
                    _NOCONST,
                    parts=self.tr.variant_parts.get(path),
                )
            if self.blk and fullname in BLK_REG_BOUNDARY:
                return self.blk_boundary(e, fullname, sig, args)
        for (pname, pt, _d), a in zip(sig.params, args, strict=True):
            if pname in sig.state_params:
                continue
            assert a is not None
            codes.append(self.coerce(a, pt, e))
        if b is not None:
            if self.pe and fullname in MEM_READS:
                self.memreads = True
            if self.pe and fullname in MEM_WRITES:
                self.memwrites = True
            name = rust_ident(sig.name)
            if self.blk and fullname in MEM_WRITES and self.facts.get("nolog"):
                name += "_nolog"
            if self.blk and fullname in MEM_FAST:
                name += "_b"
            code = self.call_code("bnd::" + name, codes, b.traps)
        else:
            self.tr.called.add(fullname)
            code = self.call_code(
                self.tr.fn_path(fullname, absolute=self.pe), codes, True
            )
            if self.pe:
                changed = self.tr.state_assigns(fullname)
                self.effects |= changed
                for attr in changed:
                    self.facts.pop(attr, None)
        return E(code, sig.ret, True, _NOCONST)

    def fast_float_call(
        self,
        e: Any,
        fullname: str,
        sig: FnSig,
        args: list[E | None],
        static: dict,
    ) -> E | None:
        """Calls of the float helpers that have a hand-written fast path in
        packages/emulator/sharc-rt/src/rt/bnd.rs (bit-identical results and flags; a NaN
        result falls back to the original sequence there)."""
        bound = {
            p[0]: (p[1], a)
            for p, a in zip(sig.params, args, strict=True)
            if p[0] not in sig.state_params and a is not None
        }
        if fullname == "sharc_core.floats._float_binary":
            op = static.get("operation")
            name = (
                self.core.live_funcs.get(id(op))
                if isinstance(op, pytypes.FunctionType)
                else None
            )
            helper = FAST_FLOAT_BINARY.get(name or "")
            if helper is None:
                return None
            codes = [
                self.coerce(bound[n][1], bound[n][0], e) for n in ("left", "right")
            ]
            return E(
                self.call_code("bnd::" + helper, codes, False), sig.ret, True, _NOCONST
            )
        if fullname == "sharc_core.floats._float_multiply":
            codes = [
                self.coerce(bound[n][1], bound[n][0], e) for n in ("left", "right")
            ]
            return E(
                self.call_code("bnd::_float_multiply", codes, False),
                sig.ret,
                True,
                _NOCONST,
            )
        if fullname == "sharc_core.compute_mult._astatx_mult_float":
            codes = [
                self.coerce(bound[n][1], bound[n][0], e)
                for n in ("result", "overflowed", "underflowed", "invalid")
            ]
            return E(
                self.call_code("bnd::_astatx_mult_float", codes, False),
                sig.ret,
                True,
                _NOCONST,
            )
        return None

    def blk_boundary(
        self, e: Any, fullname: str, sig: FnSig, args: list[E | None]
    ) -> E:
        """A register-file boundary function in block mode: its ``_rf``
        form over the block's register file."""
        bound = {
            p[0]: a
            for p, a in zip(sig.params, args, strict=True)
            if p[0] not in sig.state_params
        }
        name = fullname.rpartition(".")[2]
        if name in ("_ureg", "_ureg_raw"):
            view, idx = bound["values"], bound["code"]
            assert view is not None and idx is not None
            return self.rf_read(view, idx, e, "bnd::%s_rf" % name)
        if name == "_load_normal_ureg":
            code_e = bound["code"]
            assert code_e is not None
            code = self.reg_index(code_e, e)
            self.memreads = True
            self.wregs |= {107, 108, 109} if code == 107 else {code}
            self.putregs |= {107, 108, 109, code}
            self.reg_written(code)
            codes = []
            for pname, pt, _d in sig.params:
                if pname in sig.state_params:
                    continue
                a = bound[pname]
                assert a is not None
                codes.append(self.coerce(a, pt, e))
            return E(
                self.call_code("bnd::_load_normal_ureg_rf", codes, True, ("s", "rf")),
                sig.ret,
                True,
                _NOCONST,
            )
        raise self.fail(e, "block code: %s on a dynamic register view" % name)

    def dispatch_call(self, e: mn.CallExpr, callee: E) -> E:
        if self.pe and _is_const(callee) and callee.const is not None:
            fn = callee.const
            name = (
                self.core.live_funcs.get(id(fn))
                if isinstance(fn, pytypes.FunctionType)
                else None
            )
            if name is not None:
                sig = self.tr.signature(name)
                return self.call_bound(e, name, sig, self.bind_args(e, sig))
            if fn is abs and len(e.args) == 1:
                a = self.expr(e.args[0])
                if _is_const(a):
                    return self.py_eval(e, a.t, abs, a.const)
                return E("(%s).abs()" % a.code, a.t, a.s, _NOCONST)
        targets = sorted(
            (o for o in (callee.pts or ()) if o.kind in ("fn", "builtin")),
            key=lambda o: (o.kind, str(o.key)),
        )
        if not targets:
            raise self.fail(e, "call through a function value with no known targets")
        rt = self.mtype(e)
        rets = []
        for o in targets:
            if o.kind == "fn":
                rets.append(self.tr.signature(o.key).ret)
            else:
                rets.append(FLOAT)
        if rt == ANY or _has_any(rt):
            rt = normalize_union(rets)
        # Evaluate the arguments once.
        pre = []
        arg_es: list[tuple[Any, E, str]] = []  # (node, E, temp)
        for arg in e.args:
            a = self.expr(arg)
            v = self.fresh("d")
            if a.t == STATE:
                pre.append("let %s = ();" % v)
            else:
                pre.append("let %s = %s;" % (v, a.code))
            arg_es.append((arg, a, v))
        f = self.fresh("f")
        arms = []
        facts0 = dict(self.facts)
        arm_facts: list[dict] = []
        arm_results: list[E] = []
        for o in targets:
            if o.kind == "builtin":
                name = o.key
                if name == "abs":
                    (anode, a, v) = arg_es[0]
                    body = self.coerce(E("(%s).abs()" % v, a.t), rt, e)
                    arms.append("%s => %s" % (self.tr.fn_const("builtins.abs"), body))
                    continue
                if name in FLOAT_FUNCS:
                    rust, ptypes, ret = FLOAT_FUNCS[name]
                    parts = [
                        self.coerce(E(v, a.t), pt, e)
                        for (anode, a, v), pt in zip(arg_es, ptypes, strict=True)
                    ]
                    body = self.coerce(
                        E("%s(%s)" % (rust, ", ".join(parts)), ret), rt, e
                    )
                    arms.append("%s => %s" % (self.tr.fn_const(name), body))
                    continue
                raise self.fail(e, "builtin target %s" % name)
            sig = self.tr.signature(o.key)
            names = [p[0] for p in sig.params]
            bound: dict[str, E] = {}
            for i, ((_anode, a, v), kind, aname) in enumerate(
                zip(arg_es, e.arg_kinds, e.arg_names, strict=True)
            ):
                pname = names[i] if kind == mn.ARG_POS else str(aname)
                bound[pname] = E(
                    v, a.t, parts=a.parts, const=a.const if self.blk else _NOCONST
                )
            if self.blk and o.key not in BOUNDARY:
                # Specialise each target on the static arguments, as a
                # direct call would.
                args_l: list[E | None] = []
                for pname, _pt, default in sig.params:
                    if pname in sig.state_params:
                        args_l.append(None)
                    elif pname in bound:
                        args_l.append(bound[pname])
                    elif default is not None:
                        args_l.append(self.expr(default))
                    else:
                        raise self.fail(e, "missing argument %s to %s" % (pname, o.key))
                self.facts = dict(facts0)
                res = self.call_bound(e, o.key, sig, args_l)
                arm_facts.append(dict(self.facts))
                arm_results.append(res)
                arms.append(
                    "%s => %s" % (self.tr.fn_const(o.key), self.coerce(res, rt, e))
                )
                continue
            codes = []
            for pname, pt, default in sig.params:
                if pname in sig.state_params:
                    continue
                if pname in bound:
                    codes.append(self.coerce(bound[pname], pt, e))
                elif default is not None:
                    codes.append(self.coerce(self.expr(default), pt, e))
                else:
                    raise self.fail(e, "missing argument %s to %s" % (pname, o.key))
            if o.key in BOUNDARY:
                if o.key in MEM_READS or o.key in MEM_WRITES:
                    self.memreads = True
                if self.blk and o.key in BLK_REG_BOUNDARY:
                    raise self.fail(e, "block code: register boundary through a value")
                call = "bnd::%s(%s)" % (rust_ident(sig.name), ", ".join(["s"] + codes))
                if BOUNDARY[o.key].traps:
                    call += "?"
            else:
                self.tr.called.add(o.key)
                call = "%s(%s)?" % (
                    self.tr.fn_path(o.key, absolute=self.pe),
                    ", ".join(["s"] + codes),
                )
            arms.append(
                "%s => %s"
                % (self.tr.fn_const(o.key), self.coerce(E(call, sig.ret), rt, e))
            )
        arms.append("_ => unreachable!()")
        if arm_facts:
            # Facts after the call: what every target leaves the same.
            joined = dict(arm_facts[0])
            for af in arm_facts[1:]:
                for k in list(joined):
                    if k not in af or not _same_static(af[k], joined[k]):
                        del joined[k]
            self.facts = joined
        if self.blk and len(targets) == 1 and len(arm_results) == 1:
            # One target: no match (and its static results survive).
            res = arm_results[0]
            code = "{ %s %s }" % (" ".join(pre), res.code) if pre else res.code
            return E(code, res.t, True, res.const, res.pts, res.vset, res.parts)
        if self.pe:
            for attr in self.effects:
                self.facts.pop(attr, None)
            for o in targets:
                if o.kind == "fn" and not self.blk:
                    changed = self.tr.state_assigns(o.key)
                    self.effects |= changed
                    for attr in changed:
                        self.facts.pop(attr, None)
        code = "{ let %s = %s; %s match %s { %s } }" % (
            f,
            callee.code,
            " ".join(pre),
            f,
            ", ".join(arms),
        )
        return E(code, rt, True, _NOCONST)

    def e_CallExpr(self, e: mn.CallExpr) -> E:
        callee: Any = e.callee
        ck = type(callee).__name__
        if ck == "NameExpr":
            fullname = callee.fullname
            node = callee.node
            if fullname in TRAP_CALLS:
                # Arguments that trap first still decide the outcome;
                # observability arguments are dropped. A stop names its
                # reason when it is a literal (the caller can tell the end
                # of a call, "return without followed call", apart).
                t = STATE if fullname != "sharc_core.memory._dossier" else ERASED
                kind = TRAP_CALLS[fullname]
                if (
                    kind == "stop"
                    and len(e.args) > 2
                    and isinstance(e.args[2], mn.StrExpr)
                ):
                    kind = "stop: " + e.args[2].value
                return E(self.trap_expr(e, kind), t)
            if fullname in ERASED_CALLS:
                return E("()", NONE)
            if (
                self.blk
                and fullname == LOOP_SYNC
                and self.facts.pop(LOOP_SYNCED_FACT, False)
            ):
                # Block code whose loop-stack register view is known to be in
                # step (tools/sharc_dev/sharc_rsgen.py): the sync would rewrite the
                # same values. The fact covers this one call only.
                return E("()", NONE)
            if fullname in DYN_CALLS:
                t = DYN_CALLS[fullname]
                return E("SYM_DYN" if t == STR else "()", t)
            if fullname.startswith("builtins."):
                return self.builtin(e, fullname[len("builtins.") :])
            if isinstance(node, mn.TypeInfo):
                return self.construct(e, node.fullname)
            if isinstance(node, mn.FuncDef) and fullname in self.core.funcs:
                return self.direct_call(e, fullname)
            if fullname in FLOAT_FUNCS:
                rust, ptypes, ret = FLOAT_FUNCS[fullname]
                args = [self.expr(a) for a in e.args]
                parts = [
                    self.coerce(a, pt, e) for a, pt in zip(args, ptypes, strict=True)
                ]
                return E(
                    "%s(%s)" % (rust, ", ".join(parts)),
                    ret,
                    any(_USES_S.search(p) for p in parts),
                    _NOCONST,
                )
            if isinstance(node, mn.Var):
                f = self.expr(callee)
                if f.t == FN:
                    return self.dispatch_call(e, f)
            raise self.fail(e, "call of %s" % fullname)
        if ck == "MemberExpr":
            if callee.fullname and callee.fullname in FLOAT_FUNCS:
                rust, ptypes, ret = FLOAT_FUNCS[callee.fullname]
                args = [self.expr(a) for a in e.args]
                parts = [
                    self.coerce(a, pt, e) for a, pt in zip(args, ptypes, strict=True)
                ]
                return E("%s(%s)" % (rust, ", ".join(parts)), ret)
            return self.method(e, callee)
        f = self.expr(callee)
        if f.t == FN:
            return self.dispatch_call(e, f)
        raise self.fail(e, "call of a %s" % ck)

    def construct(self, e: mn.CallExpr, cls: str) -> E:
        args = [self.expr(a) for a in e.args]
        if (
            self.pe
            and cls
            in RECORDS.keys()
            | {
                "sharc_core.values.Const",
                "sharc_core.values.Unknown",
                "sharc_core.state.MR",
            }
            and all(_is_const(a) and not isinstance(a.const, Tag) for a in args)
            and not any(a.code.startswith("return Err(") for a in args)
        ):
            live = self.core.live_value(cls)
            kwargs: dict[str, Any] = {}
            pos = []
            for a, kind, aname in zip(args, e.arg_kinds, e.arg_names, strict=True):
                if kind == mn.ARG_POS:
                    pos.append(a.const)
                else:
                    kwargs[str(aname)] = a.const
            if cls.endswith((".Const", ".Unknown")):
                ct = VAL
            elif cls.endswith(".MR"):
                ct = MR
            else:
                ct = REC(RECORDS[cls])
            return self.py_eval(e, ct, lambda: live(*pos, **kwargs))
        if cls == "sharc_core.values.Const":
            return E(
                "V::c(%s)" % self.coerce(args[0], INT, e), VAL, args[0].s, _NOCONST
            )
        if cls == "sharc_core.values.Unknown":
            return E("V::UNK", VAL, const=_NOCONST)
        if cls == "sharc_core.state.MR":
            return E(
                "MR::new(%s, %s)"
                % (self.coerce(args[0], INT, e), self.coerce(args[1], INT, e)),
                MR,
                any(a.s for a in args),
                _NOCONST,
            )
        if cls in (
            "builtins.ValueError",
            "builtins.KeyError",
            "sharc_core.memory.UnmodeledMMR",
        ):
            return E("()", ERASED)
        if cls in ("sharc_core.values.Affine", "sharc_core.values.PartialConst"):
            # Symbolic values; the concrete driver never builds them.
            return E(self.trap_expr(e, "symbolic value"), VAL)
        rec = RECORDS.get(cls)
        if rec is None:
            raise self.fail(e, "constructor %s" % cls)
        fields = self.tr.record_fields(rec)
        live = self.core.live_value(cls)
        defaults = self.tr.record_defaults(live)
        bound: dict[str, E] = {}
        for i, (a, kind, aname) in enumerate(
            zip(args, e.arg_kinds, e.arg_names, strict=True)
        ):
            if kind == mn.ARG_POS:
                bound[fields[i][0]] = a
            elif kind == mn.ARG_NAMED:
                bound[str(aname)] = a
            else:
                raise self.fail(e, "*args in a record constructor")
        parts = []
        narrow = NARROW_FIELDS.get(rec, {})
        for fname, ft in fields:
            if fname in bound and fname in narrow:
                parts.append(
                    "%s: (%s) as %s"
                    % (fname, self.coerce(bound[fname], ft, e), narrow[fname])
                )
            elif fname in bound:
                parts.append("%s: %s" % (fname, self.coerce(bound[fname], ft, e)))
            elif fname in defaults:
                parts.append(
                    "%s: %s"
                    % (fname, self.tr.const_value(defaults[fname], ft, self.where(e)))
                )
            else:
                raise self.fail(e, "missing field %s of %s" % (fname, rec))
        code = "%s { %s }" % (rec, ", ".join(parts))
        if any(_USES_S.search(p) for p in parts):
            pre = []
            new_parts = []
            for p in parts:
                fname, _, value = p.partition(": ")
                if _USES_S.search(value):
                    v = self.fresh("r")
                    pre.append("let %s = %s;" % (v, value))
                    new_parts.append("%s: %s" % (fname, v))
                else:
                    new_parts.append(p)
            code = "{ %s %s { %s } }" % (" ".join(pre), rec, ", ".join(new_parts))
        out = E(code, REC(rec), True, _NOCONST)
        if self.pe:
            items = []
            for fname, _ft in fields:
                if fname in bound:
                    items.append(_ps_item(bound[fname]))
                elif fname in defaults:
                    items.append(defaults[fname])
                else:
                    items.append(_NOCONST)
            if any(i is not _NOCONST for i in items):
                out.parts = tuple(items)
        return out

    def builtin(self, e: mn.CallExpr, name: str) -> E:
        args = e.args
        if name == "isinstance":
            return self.isinstance_(e)
        if self.pe and name in (
            "len",
            "bool",
            "int",
            "float",
            "abs",
            "min",
            "max",
            "tuple",
            "list",
        ):
            vals = [self.expr(a) for a in args]
            if all(_is_const(v) and not isinstance(v.const, Tag) for v in vals) and all(
                k == mn.ARG_POS for k in e.arg_kinds
            ):
                import builtins as _b

                return self.py_eval(
                    e, self.mtype(e), getattr(_b, name), *[v.const for v in vals]
                )
        if name == "len":
            a = self.expr(args[0])
            if a.t.kind == "vtup":
                return E("((%s).len() as Int)" % a.code, INT, a.s, _NOCONST)
            if a.t.kind == "stk":
                return E("stk_len_%s(s)" % a.t.args[0], INT, True, _NOCONST)
            if a.t.kind == "tup":
                return E(lit_int(len(a.t.args)), INT, const=len(a.t.args))
            raise self.fail(e, "len of %r" % (a.t,))
        if name == "bool":
            return E(self.cond(args[0]), BOOL, True, _NOCONST)
        if name == "int":
            a = self.expr(args[0])
            if a.t in (INT, BOOL):
                return E(self.coerce(a, INT, e), INT, a.s, _NOCONST)
            if a.t == FLOAT:
                return E("py_int_of_float(%s)?" % a.code, INT, a.s, _NOCONST)
            raise self.fail(e, "int of %r" % (a.t,))
        if name == "float":
            a = self.expr(args[0])
            return E(self.coerce(a, FLOAT, e), FLOAT, a.s, _NOCONST)
        if name == "abs":
            a = self.expr(args[0])
            if a.t == BOOL:
                a = E(self.coerce(a, INT, e), INT, a.s)
            return E("(%s).abs()" % a.code, a.t, a.s, _NOCONST)
        if name in ("min", "max"):
            if len(args) != 2:
                raise self.fail(e, "%s with %d arguments" % (name, len(args)))
            rt = self.mtype(e)
            a, b = self.expr(args[0]), self.expr(args[1])
            fn = "py_%s_%s" % (name, "f" if rt == FLOAT else "i")
            return E(
                "%s(%s, %s)" % (fn, self.coerce(a, rt, e), self.coerce(b, rt, e)),
                rt,
                a.s or b.s,
                _NOCONST,
            )
        if name in ("tuple", "list"):
            a = self.expr(args[0])
            rt = self.mtype(e)
            return E(self.coerce(a, rt, e), rt, a.s, _NOCONST)
        if name in ("str", "hex", "repr"):
            return E("SYM_DYN", STR)
        raise self.fail(e, "builtin %s" % name)

    def isinstance_(self, e: mn.CallExpr) -> E:
        target = e.args[1]
        classes: list[str] = []
        items = target.items if isinstance(target, mn.TupleExpr) else [target]
        for c in items:
            if isinstance(c, (mn.NameExpr, mn.MemberExpr)) and isinstance(
                c.node, mn.TypeInfo
            ):
                classes.append(c.node.fullname)
            else:
                raise self.fail(e, "isinstance against %s" % type(c).__name__)
        a = self.expr(e.args[0])
        if self.pe and _is_const(a) and not isinstance(a.const, Tag):
            live = tuple(
                self.core.live_value(c)
                if not c.startswith("builtins.")
                else getattr(__import__("builtins"), c.split(".")[1])
                for c in classes
            )
            return self.py_eval(e, BOOL, isinstance, a.const, live)
        # The declared type of the tested value, not its narrowed type.
        tests = [self.instance_test(e, a, c) for c in classes]
        return E("(%s)" % " || ".join(tests), BOOL, a.s, _NOCONST)

    def instance_test(self, e: Any, a: E, cls: str) -> str:
        t = a.t
        if t.kind == "opt":
            v = self.fresh("o")
            inner = self.instance_test(e, E(v, t.args[0]), cls)
            return "match %s { Some(%s) => %s, None => false }" % (a.code, v, inner)
        if cls == "sharc_core.values.Const":
            if t in (VAL, SPEC, VI):
                return "(%s).is_c()" % a.code
            if t in (MR, INT):
                return "false"
        if cls in ("sharc_core.values.Unknown",):
            if t in (VAL, SPEC):
                return "(%s).is_unknown()" % a.code
            if t == MR:
                return "false"
        if cls == "sharc_core.values.PartialConst":
            if t in (VAL, SPEC):
                return "(%s).is_partial()" % a.code
            if t == MR:
                return "false"
        if cls == "sharc_core.values.Affine":
            return "false"
        if cls == "sharc_core.state.MR":
            if t == SPEC:
                return "(%s).is_mr()" % a.code
            if t == MR:
                return "true"
            if t == VAL:
                return "false"
        py = {
            "builtins.int": INT,
            "builtins.str": STR,
            "builtins.bool": BOOL,
            "builtins.float": FLOAT,
        }
        if t.kind == "union":
            members = []
            for i, m in enumerate(t.args):
                if (
                    cls in py
                    and (m == py[cls] or (cls == "builtins.int" and m == BOOL))
                    or cls == "builtins.tuple"
                    and m.kind in ("tup", "vtup")
                    or cls == "sharc_core.state.MR"
                    and m in (MR,)
                ):
                    members.append(i)
                elif cls == "sharc_core.values.Const" and m in (VAL, SPEC):
                    pass
            if cls in ("sharc_core.values.Const", "sharc_core.state.MR"):
                arms = []
                for i, m in enumerate(t.args):
                    arms.append(
                        "%s::A%d(__x) => %s"
                        % (
                            self.tr.union_name(t),
                            i,
                            self.instance_test(e, E("__x", m), cls),
                        )
                    )
                return "(match %s { %s })" % (a.code, ", ".join(arms))
            if not members:
                return "false"
            return "matches!(%s, %s)" % (
                a.code,
                " | ".join("%s::A%d(_)" % (self.tr.union_name(t), i) for i in members),
            )
        if cls in py:
            return (
                "true"
                if t == py[cls] or (cls == "builtins.int" and t == BOOL)
                else "false"
            )
        if cls == "builtins.tuple":
            return "true" if t.kind in ("tup", "vtup") else "false"
        raise self.fail(e, "isinstance(%r, %s)" % (t, cls))

    def method(self, e: mn.CallExpr, callee: mn.MemberExpr) -> E:
        name = callee.name
        base = self.expr(callee.expr)
        bt = base.t
        args = [self.expr(a) for a in e.args]
        rt = self.mtype(e)
        if (
            self.pe
            and _is_const(base)
            and not isinstance(base.const, Tag)
            and all(_is_const(a) and not isinstance(a.const, Tag) for a in args)
            and name
            in (
                "get",
                "startswith",
                "endswith",
                "lower",
                "upper",
                "signed",
                "bit_length",
                "format",
                "join",
            )
            and all(k == mn.ARG_POS for k in e.arg_kinds)
        ):
            fn = getattr(base.const, name)
            return self.py_eval(e, rt, fn, *[a.const for a in args])
        if bt == ERASED:
            return E("()", NONE)
        if bt == TABLE:
            if name == "get":
                default = args[1] if len(args) > 1 else None
                return self.table_lookup(e, base, args[0], rt, "get", default)
            raise self.fail(e, "table method %s" % name)
        if bt == REGVIEW:
            if name == "get" and self.blk:
                return self.rf_read(base, args[0], e)
            if name == "get":
                return E(
                    self.call_code(
                        "rv_get", [base.code, self.coerce(args[0], INT, e)], False
                    ),
                    VAL,
                    True,
                    _NOCONST,
                )
            raise self.fail(e, "register map method %s" % name)
        if bt == SPECVIEW:
            if name == "get":
                key = self.coerce(args[0], STR, e)
                call = self.call_code("sv_get", [base.code, key], False)
                if len(args) > 1:
                    d = self.coerce(args[1], SPEC, e)
                    code = "(match %s { Some(__v) => __v, None => %s })" % (call, d)
                    return E(
                        self.coerce(E(code, SPEC), rt, e) if rt != ANY else code,
                        rt if rt != ANY else SPEC,
                        True,
                        _NOCONST,
                    )
                return E(call, OPT(SPEC), True, _NOCONST)
            raise self.fail(e, "special map method %s" % name)
        if bt == CONFIG_MAP and name == "get":
            return E(
                "s.cfg.provisional_get(%s)" % self.coerce(args[0], STR, e),
                OPT(STR),
                True,
                _NOCONST,
            )
        if bt == FIELDS:
            if name == "get":
                return E(
                    "%s.get(%s)" % (base.code, self.coerce(args[0], STR, e)),
                    OPT(INT),
                    base.s,
                    _NOCONST,
                )
            raise self.fail(e, "fields method %s" % name)
        if bt.kind == "stk":
            stack = bt.args[0]
            if name == "append":
                return E(
                    self.call_code(
                        "stk_push_%s" % stack,
                        [self.coerce(args[0], STACKS[stack], e)],
                        True,
                    ),
                    NONE,
                    True,
                    _NOCONST,
                )
            if name == "pop" and not args:
                return E("stk_pop_%s(s)?" % stack, STACKS[stack], True, _NOCONST)
            raise self.fail(e, "stack method %s" % name)
        if bt.kind == "vtup":
            if name == "append":
                return E(
                    "%s.push(%s)?" % (base.code, self.coerce(args[0], bt.args[0], e)),
                    NONE,
                    True,
                    _NOCONST,
                )
            raise self.fail(e, "list method %s" % name)
        if bt == STR:
            if name in ("startswith", "endswith"):
                a = args[0]
                if _is_const(a) and isinstance(a.const, (str, tuple)):
                    affixes = a.const if isinstance(a.const, tuple) else (a.const,)
                    if _is_const(base) and isinstance(base.const, str):
                        value = getattr(base.const, name)(affixes)
                        return E("true" if value else "false", BOOL, const=value)
                    fn = self.tr.sym_affix_fn(name, affixes)
                    return E("%s(%s)" % (fn, base.code), BOOL, base.s, _NOCONST)
                raise self.fail(e, "%s with a non-constant argument" % name)
            if name in ("lower", "upper"):
                fn = self.tr.sym_map_fn(name)
                return E("%s(%s)" % (fn, base.code), STR, base.s, _NOCONST)
            if name in ("join", "format"):
                return E("SYM_DYN", STR)
            raise self.fail(e, "string method %s" % name)
        if bt == MR and name == "signed":
            return E("(%s).signed()" % base.code, OPT(INT), base.s, _NOCONST)
        if bt == INT and name == "bit_length":
            return E("bit_length(%s)" % base.code, INT, base.s, _NOCONST)
        raise self.fail(e, "method %s of %r" % (name, bt))

    # -- statements --------------------------------------------------------------

    def block(self, stmts: list[Any], indent: str) -> tuple[list[str], bool]:
        out: list[str] = []
        for st in stmts:
            lines, term = self.stmt(st, indent)
            out.extend(lines)
            if term:
                return out, True
        return out, False

    def stmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        kind = type(st).__name__
        method = getattr(self, "s_" + kind, None)
        if method is None:
            raise self.fail(st, "unsupported statement %s" % kind)
        return method(st, ind)

    def s_Block(self, st: Any, ind: str) -> tuple[list[str], bool]:
        return self.block(st.body, ind)

    def s_PassStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        return [], False

    def s_AssertStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        return [], False

    def s_BreakStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        return [ind + "break;"], True

    def s_ContinueStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        return [ind + "continue;"], True

    def s_RaiseStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        what = "raise"
        if st.expr is not None and isinstance(st.expr, mn.CallExpr):
            c = st.expr.callee
            what = "raise %s" % getattr(c, "name", "?")
        return [ind + self.trap_expr(st, what) + ";"], True

    def s_TryStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        if st.finally_body is not None:
            raise self.fail(st, "try/finally")
        # Every exception traps natively (the Python core then runs the
        # handler itself), so only the body is translated.
        lines, term = self.block(st.body.body, ind)
        if not term and st.else_body is not None:
            more, term = self.block(st.else_body.body, ind)
            lines.extend(more)
        return lines, term

    def s_ExpressionStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        e = st.expr
        if isinstance(e, mn.CallExpr):
            c = e.callee
            if isinstance(c, mn.NameExpr) and c.fullname in ERASED_CALLS:
                return [], False
            if isinstance(c, mn.MemberExpr):
                base_t = self._peek_type(c.expr)
                if base_t == ERASED:
                    return [], False
        if isinstance(e, mn.StrExpr):
            return [], False  # a docstring
        v = self.expr(e)
        if v.code.startswith("return Err("):
            return [ind + v.code + ";"], True
        if v.t in (NONE, LSTATE, STATE, ERASED):
            if v.code in ("()", ""):
                return [], False
            return [ind + v.code + ";"], False
        return [ind + "let _ = %s;" % v.code], False

    def _peek_type(self, e: Any) -> T:
        if isinstance(e, mn.MemberExpr) and e.name == "trace":
            return ERASED
        if isinstance(e, mn.IndexExpr):
            return self._peek_type(e.base)
        if isinstance(e, mn.MemberExpr) and e.name == "overlay":
            return ANY
        return ANY

    def s_ReturnStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        ret = self.sig.ret
        if st.expr is None:
            if self.pe:
                self.exit_facts.append(dict(self.facts))
            return [ind + "return Ok(%s);" % self.none_value(ret, st)], True
        v = self.expr(st.expr)
        if v.code.startswith("return Err("):
            return [ind + v.code + ";"], True
        if self.pe:
            # After the returned expression: a call in it may change facts.
            self.exit_facts.append(dict(self.facts))
            self.returns.append(_ps_parts(v))
            self.ret_consts.append(v.const)
        if ret == LSTATE:
            if v.t != LSTATE:
                raise self.fail(st, "returning %r as successors" % (v.t,))
            if v.code == "()":
                return [ind + "return Ok(());"], True
            return [ind + v.code + ";", ind + "return Ok(());"], True
        return [ind + "return Ok(%s);" % self.coerce(v, ret, st)], True

    def none_value(self, t: T, node: Any) -> str:
        if t in (NONE, LSTATE, ERASED):
            return "()"
        if t.kind == "opt":
            return "None"
        if t == SPECVIEW:
            return "SpecView::NONE"
        raise self.fail(node, "falls off the end of a function returning %r" % (t,))

    def s_IfStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        lines: list[str] = []
        env0 = dict(self.env)
        facts0 = dict(self.facts)
        branches: list[tuple[str | None, list[str], bool, dict]] = []
        branch_facts: list[dict] = []
        taken_else = False
        for cond, body in zip(st.expr, st.body, strict=True):
            self.env = dict(env0)
            self.facts = dict(facts0)
            test, tv = self.test(cond)
            if tv is not _NOCONST and test not in ("true", "false"):
                # Decided statically, but the test has effects of its own.
                if branches:
                    tv = _NOCONST
                else:
                    lines.append(ind + "let _ = %s;" % test)
            if tv is False:
                continue
            self.env = dict(env0)
            self.facts = dict(facts0)
            inner, term = self.block(body.body, ind + "    ")
            if tv is True:
                branches.append((None, inner, term, self.env))
                branch_facts.append(self.facts)
                taken_else = True
                break
            branches.append((test, inner, term, self.env))
            branch_facts.append(self.facts)
        if not taken_else:
            if st.else_body is not None:
                self.env = dict(env0)
                self.facts = dict(facts0)
                inner, term = self.block(st.else_body.body, ind + "    ")
                branches.append((None, inner, term, self.env))
                branch_facts.append(self.facts)
            else:
                branches.append((None, [], False, dict(env0)))
                branch_facts.append(dict(facts0))
        live_facts = [
            f for b, f in zip(branches, branch_facts, strict=True) if not b[2]
        ]
        if live_facts:
            mf = dict(live_facts[0])
            for f in live_facts[1:]:
                for k in list(mf):
                    if k not in f or not _same_static(f[k], mf[k]):
                        del mf[k]
            facts_after = mf
        else:
            facts_after = {}
        # Merge: a local keeps a static value only if every branch that
        # falls through agrees on it.
        live = [b[3] for b in branches if not b[2]]
        if live:
            merged = dict(live[0])
            for env in live[1:]:
                for k in list(merged):
                    if k not in env:
                        del merged[k]
                    elif not _same_static(env[k], merged[k]):
                        m = _merge_item([env[k], merged[k]])
                        if isinstance(m, PS):
                            merged[k] = m
                        else:
                            del merged[k]
            for env in live:
                for k in env:
                    if k not in merged and k in self.unrepr:
                        raise self.fail(
                            st, "a static value with no Rust form merges with another"
                        )
            self.env = merged
        else:
            self.env = {}
        self.facts = facts_after
        all_term = all(b[2] for b in branches)
        if len(branches) == 1 and branches[0][0] is None:
            # Only one path is possible: no `if` at all.
            return lines + [ind + "{"] + branches[0][1] + [ind + "}"], all_term
        first = True
        for btest, inner, _term, _env in branches:
            if btest is None:
                if first:
                    lines.append(ind + "{")
                else:
                    lines.append(ind + "} else {")
            else:
                lines.append(ind + ("if %s {" if first else "} else if %s {") % btest)
            lines.extend(inner)
            first = False
        lines.append(ind + "}")
        return lines, all_term

    def s_WhileStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        if st.else_body is not None:
            raise self.fail(st, "while/else")
        self.facts = {k: v for k, v in self.facts.items() if k in SETTING_FACTS}
        for k in _assigned_vars(st.body):
            self.env.pop(k, None)
        test, tv = self.test(st.expr)
        if tv is False:
            return [], False
        lines = [ind + "while %s {" % test]
        inner, _ = self.block(st.body.body, ind + "    ")
        lines.extend(inner)
        lines.append(ind + "}")
        for k in _assigned_vars(st.body):
            self.env.pop(k, None)
        return lines, False

    def assign(self, target: Any, v: E, ind: str, node: Any) -> list[str]:
        kind = type(target).__name__
        if v.code.startswith("return Err("):
            return [ind + v.code + ";"]
        if kind == "NameExpr":
            var = target.node
            if target.name == "_":
                return []
            if not isinstance(var, mn.Var):
                raise self.fail(node, "assignment to %s" % target.name)
            if target.name in self.sig.state_params:
                return []
            name, t = self.locals.get(id(var)) or self.declare(var)
            if t == STATE:
                return []
            if _is_const(v) and self.pe:
                self.env[id(var)] = v.const
                try:
                    code = self.coerce(v, t, node)
                except TranspileError:
                    code = "__NOREPR__"
                if "__NOREPR__" in code:
                    self.unrepr.add(id(var))
                    return []
                self.unrepr.discard(id(var))
                return [ind + "%s = %s;" % (name, code)]
            self.env.pop(id(var), None)
            ps = _ps_parts(v) if self.pe else None
            if ps is not None:
                self.env[id(var)] = PS(ps)
            return [ind + "%s = %s;" % (name, self.coerce(v, t, node))]
        if kind in ("TupleExpr", "ListExpr"):
            tmp = self.fresh("u")
            lines = [ind + "let %s = %s;" % (tmp, v.code)]
            n = len(target.items)
            vparts = _ps_parts(v) if self.pe else None
            if vparts is not None and len(vparts) != n:
                vparts = None
            for i, item in enumerate(target.items):
                if v.t.kind == "tup":
                    if len(v.t.args) != n:
                        raise self.fail(node, "unpacking %d from %r" % (n, v.t))
                    pv = vparts[i] if vparts is not None else _NOCONST
                    part = E(
                        "%s.%d" % (tmp, i),
                        v.t.args[i],
                        const=_NOCONST if isinstance(pv, PS) else pv,
                        parts=pv.parts if isinstance(pv, PS) else None,
                    )
                elif v.t.kind == "vtup":
                    part = E("%s.at(%d)?" % (tmp, i), v.t.args[0], True)
                elif v.t.kind == "rec":
                    fields = self.tr.record_fields(v.t.args[0])
                    part = E("%s.%s" % (tmp, fields[i][0]), fields[i][1])
                else:
                    raise self.fail(node, "unpacking %r" % (v.t,))
                lines.extend(self.assign(item, part, ind, node))
            return lines
        if kind == "IndexExpr":
            base = target.base
            if isinstance(base, mn.MemberExpr) and base.name == "trace":
                return []
            if (
                isinstance(base, mn.IndexExpr)
                and isinstance(base.base, mn.MemberExpr)
                and base.base.name == "trace"
            ):
                return []
            b = self.expr(base)
            idx = self.expr(target.index)
            if b.t == REGVIEW:
                if b.code != "RegView::CUR":
                    raise self.fail(node, "store into a register snapshot")
                if self.blk:
                    reg = self.reg_index(idx, node)
                    self.wregs.add(reg)
                    self.reg_written(reg, v)
                    vcode = self.coerce(v, VAL, node)
                    # The flag registers are required known at region entry
                    # (tools/sharc_dev/sharc_rsgen.py), so a flag update that would leave
                    # a bit unknown is rf_set's cold exit like any register.
                    fn = "rf_put" if vcode.strip() == "V::UNK" else "rf_set"
                    if fn == "rf_put":
                        self.putregs.add(reg)
                    return [
                        ind
                        + self.call_code(fn, [lit_int(reg), vcode], True, ("rf",))
                        + ";"
                    ]
                return [
                    ind
                    + self.call_code(
                        "s_set_r",
                        [self.coerce(idx, INT, node), self.coerce(v, VAL, node)],
                        True,
                    )
                    + ";"
                ]
            if b.t == SPECVIEW:
                if b.code != "SpecView::CUR":
                    raise self.fail(node, "store into a special-register view")
                return [
                    ind
                    + self.call_code(
                        "s_set_special",
                        [self.coerce(idx, STR, node), self.coerce(v, SPEC, node)],
                        True,
                    )
                    + ";"
                ]
            if b.t.kind == "stk":
                stack = b.t.args[0]
                return [
                    ind
                    + self.call_code(
                        "stk_set_%s" % stack,
                        [
                            self.coerce(idx, INT, node),
                            self.coerce(v, STACKS[stack], node),
                        ],
                        True,
                    )
                    + ";"
                ]
            if b.t.kind == "vtup":
                if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", b.code):
                    raise self.fail(node, "store into a list expression")
                return [
                    ind
                    + "%s.set(%s, %s)?;"
                    % (
                        b.code,
                        self.coerce(idx, INT, node),
                        self.coerce(v, b.t.args[0], node),
                    )
                ]
            raise self.fail(node, "item assignment on %r" % (b.t,))
        if kind == "MemberExpr":
            b = self.expr(target.expr)
            if b.t != STATE:
                raise self.fail(node, "attribute assignment on %r" % (b.t,))
            attr = target.name
            if attr in STATE_ERASED_WRITES:
                return []
            if self.blk and attr in BLK_ERASED_WRITES:
                return []
            if attr in STATE_ATTRS and STATE_ATTRS[attr][2]:
                t, code, _w = STATE_ATTRS[attr]
                if self.blk and attr in BLK_RF_ATTRS:
                    code = BLK_RF_ATTRS[attr]
                value = self.coerce(v, t, node)
                if self.pe and attr in FACT_ATTRS:
                    if _is_const(v) and not isinstance(v.const, Tag):
                        self.facts[attr] = v.const
                        if attr == "pending" and v.const is not None:
                            self.effects.add("pending")
                    else:
                        self.facts.pop(attr, None)
                        self.effects.add(attr)
                    if attr != "pending" or not (_is_const(v) and v.const is None):
                        self.effects.add(attr)
                return [ind + "%s = %s;" % (code, value)]
            raise self.fail(node, "assignment to state.%s" % attr)
        raise self.fail(node, "assignment target %s" % kind)

    def s_AssignmentStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        rv = st.rvalue
        if type(rv).__name__ == "TempNode":
            return [], False  # a bare annotation
        if all(
            isinstance(t, mn.MemberExpr) and t.name in STATE_ERASED_WRITES
            for t in st.lvalues
        ):
            return [], False
        # Tuple-to-tuple assignment evaluates every right-hand item first.
        if (
            len(st.lvalues) == 1
            and isinstance(st.lvalues[0], mn.TupleExpr)
            and isinstance(rv, mn.TupleExpr)
            and len(rv.items) == len(st.lvalues[0].items)
        ):
            items = [self.expr(i) for i in rv.items]
            lines = []
            temps = []
            for it in items:
                if it.code.startswith("return Err("):
                    return [ind + it.code + ";"], True
                if self.pe and _is_const(it):
                    temps.append(it)
                    continue
                tname = self.fresh("p")
                lines.append(ind + "let %s = %s;" % (tname, it.code))
                temps.append(E(tname, it.t, False, _NOCONST, it.pts))
            for target, tmp in zip(st.lvalues[0].items, temps, strict=True):
                lines.extend(self.assign(target, tmp, ind, st))
            return lines, False
        v = self.expr(rv)
        if v.code.startswith("return Err("):
            return [ind + v.code + ";"], True
        lines = []
        if len(st.lvalues) > 1:
            shared = self.fresh("p")
            lines.append(ind + "let %s = %s;" % (shared, v.code))
            v = E(shared, v.t, False, v.const, v.pts)
        for target in st.lvalues:
            lines.extend(self.assign(target, v, ind, st))
        return lines, False

    def s_OperatorAssignmentStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        target = st.lvalue
        op = st.op
        if (
            self.blk
            and isinstance(target, mn.MemberExpr)
            and target.name in BLK_ERASED_WRITES
        ):
            return [], False
        cur = self.expr(target)
        rhs = self.expr(st.rvalue)
        t = cur.t
        if self.pe and isinstance(target, mn.MemberExpr) and target.name in FACT_ATTRS:
            self.effects.add(target.name)
            if not (_is_const(cur) and _is_const(rhs)):
                self.facts.pop(target.name, None)
        if (
            self.pe
            and _is_const(cur)
            and _is_const(rhs)
            and op in _PY_BINOPS
            and isinstance(cur.const, (int, float))
            and isinstance(rhs.const, (int, float))
        ):
            value = _PY_BINOPS[op](cur.const, rhs.const)
            return self.assign(target, self.static_e(value, t, st), ind, st), False
        if t == ERASED:
            return [], False
        if t not in (INT, FLOAT):
            raise self.fail(st, "augmented assignment on %r" % (t,))
        a = cur.code
        b = self.coerce(rhs, t, st)
        if t == FLOAT and op in ("+", "-", "*"):
            value = "%s(%s, %s)" % ({"+": "fadd", "-": "fsub", "*": "fmul"}[op], a, b)
        elif op in ("+", "-", "*", "&", "|", "^"):
            value = "(%s %s %s)" % (a, op, b)
        elif op == "<<":
            value = "shl(%s, %s)?" % (a, b)
        elif op == ">>":
            value = "shr(%s, %s)?" % (a, b)
        elif op == "//":
            value = "floordiv(%s, %s)?" % (a, b)
        elif op == "%":
            value = "pymod(%s, %s)?" % (a, b)
        else:
            raise self.fail(st, "augmented %s" % op)
        return self.assign(target, E(value, t, True), ind, st), False

    def s_ForStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        if st.else_body is not None:
            raise self.fail(st, "for/else")
        if self.pe and not _has_jump(st.body):
            items = self.static_sequence(st.expr)
            if items is not None:
                lines: list[str] = []
                for item in items:
                    lines.extend(self.assign(st.index, item, ind, st))
                    inner, term = self.block(st.body.body, ind)
                    lines.extend(inner)
                    if term:
                        return lines, True
                return lines, False
        for k in _assigned_vars(st):
            self.env.pop(k, None)
        self.facts = {k: v for k, v in self.facts.items() if k in SETTING_FACTS}
        out = self._for_dynamic(st, ind)
        for k in _assigned_vars(st):
            self.env.pop(k, None)
        return out

    def static_sequence(self, it: Any) -> list[E] | None:
        """The items of a static iterable (a tuple, range, zip or
        enumerate of static values) as static E's, or None."""
        if isinstance(it, mn.CallExpr) and isinstance(it.callee, mn.NameExpr):
            fn = it.callee.fullname
            if fn == "builtins.zip":
                mixed = self.static_zip(it)
                if mixed is not None:
                    return mixed
            if fn in ("builtins.range", "builtins.zip", "builtins.enumerate"):
                vals = []
                for a, kind in zip(it.args, it.arg_kinds, strict=True):
                    if kind != mn.ARG_POS:
                        continue
                    v = self.expr(a)
                    if not _is_const(v) or isinstance(v.const, Tag):
                        return None
                    vals.append(v.const)
                import builtins as _b

                seq = list(getattr(_b, fn.split(".")[1])(*vals))
                return [self.static_e(x, _type_of_value(x), it) for x in seq]
        v = self.expr(it)
        if (
            _is_const(v)
            and isinstance(v.const, (tuple, list, str))
            and not isinstance(v.const, Tag)
        ):
            return [self.static_e(x, _type_of_value(x), it) for x in v.const]
        return None

    def static_zip(self, it: mn.CallExpr) -> list[E] | None:
        """zip() of static tuples and dynamic tuples held in locals, when at
        least one is static (it fixes the length): items whose static parts
        are known (a partially static tuple each)."""
        args = [
            a
            for a, kind in zip(it.args, it.arg_kinds, strict=True)
            if kind == mn.ARG_POS
        ]
        es = [self.expr(a) for a in args]
        consts = [e for e in es if _is_const(e) and isinstance(e.const, (tuple, list))]
        if not consts or len(consts) == len(es):
            return None
        n = min(len(e.const) for e in consts)
        for a, e in zip(args, es, strict=True):
            if _is_const(e):
                continue
            if not isinstance(a, mn.NameExpr) or e.t.kind not in ("tup", "vtup"):
                return None
            if e.t.kind == "tup" and len(e.t.args) < n:
                return None
        items = []
        for i in range(n):
            codes, types, parts = [], [], []
            for e in es:
                if _is_const(e):
                    x = e.const[i]
                    se = self.static_e(x, _type_of_value(x), it)
                    codes.append(se.code)
                    types.append(se.t)
                    parts.append(x)
                elif e.t.kind == "tup":
                    codes.append("(%s).%d" % (e.code, i))
                    types.append(e.t.args[i])
                    parts.append(_NOCONST)
                else:
                    codes.append("(%s).at(%d)?" % (e.code, i))
                    types.append(e.t.args[0])
                    parts.append(_NOCONST)
            items.append(
                E(
                    "(%s)" % ", ".join(codes),
                    TUP(*types),
                    True,
                    _NOCONST,
                    parts=tuple(parts),
                )
            )
        return items

    def _for_dynamic(self, st: Any, ind: str) -> tuple[list[str], bool]:
        it = st.expr
        lines: list[str] = []
        i = self.fresh("i")
        elems: list[E]
        header: str
        if (
            isinstance(it, mn.CallExpr)
            and isinstance(it.callee, mn.NameExpr)
            and it.callee.fullname
            in ("builtins.range", "builtins.zip", "builtins.enumerate")
        ):
            fn = it.callee.fullname
            if fn == "builtins.range":
                args = [self.coerce(self.expr(a), INT, st) for a in it.args]
                if len(args) == 1:
                    args = ["0"] + args + ["1"]
                elif len(args) == 2:
                    args = args + ["1"]
                header = "for %s in py_range(%s, %s, %s)? {" % (
                    i,
                    args[0],
                    args[1],
                    args[2],
                )
                elems = [E(i, INT)]
                self._for_body(st, header, elems, ind, lines)
                return lines, False
            if fn == "builtins.enumerate":
                seq = self.expr(it.args[0])
                sv = self.fresh("q")
                lines.append(ind + "let %s = %s;" % (sv, self.seq_code(seq, st)))
                et = self.seq_elem(seq, st)
                header = "for %s in 0..%s.len() {" % (i, sv)
                pair = E("((%s as Int), %s.get(%s))" % (i, sv, i), TUP(INT, et))
                self._for_body(st, header, [pair], ind, lines)
                return lines, False
            # zip (strict= is a check, not data)
            names = []
            types = []
            for a, akind in zip(it.args, it.arg_kinds, strict=True):
                if akind != mn.ARG_POS:
                    continue
                if (
                    isinstance(a, mn.CallExpr)
                    and isinstance(a.callee, mn.NameExpr)
                    and a.callee.fullname == "builtins.range"
                ):
                    rargs = [self.coerce(self.expr(x), INT, st) for x in a.args]
                    if len(rargs) == 1:
                        rargs = ["0"] + rargs + ["1"]
                    elif len(rargs) == 2:
                        rargs = rargs + ["1"]
                    sv = self.fresh("q")
                    lines.append(
                        ind
                        + "let %s = py_range_tup(%s, %s, %s)?;"
                        % (sv, rargs[0], rargs[1], rargs[2])
                    )
                    names.append(sv)
                    types.append(INT)
                    continue
                sq = self.expr(a)
                sv = self.fresh("q")
                lines.append(ind + "let %s = %s;" % (sv, self.seq_code(sq, st)))
                names.append(sv)
                types.append(self.seq_elem(sq, st))
            n = self.fresh("n")
            lines.append(
                ind
                + "let %s = %s;"
                % (
                    n,
                    " .min(".join("%s.len()" % v for v in names)
                    + ")" * (len(names) - 1),
                )
            )
            header = "for %s in 0..%s {" % (i, n)
            tup = E(
                "(%s)" % ", ".join("%s.get(%s)" % (v, i) for v in names), TUP(*types)
            )
            self._for_body(st, header, [tup], ind, lines)
            return lines, False
        seq = self.expr(it)
        sv = self.fresh("q")
        lines.append(ind + "let %s = %s;" % (sv, self.seq_code(seq, st)))
        header = "for %s in 0..%s.len() {" % (i, sv)
        self._for_body(
            st, header, [E("%s.get(%s)" % (sv, i), self.seq_elem(seq, st))], ind, lines
        )
        return lines, False

    def seq_code(self, seq: E, node: Any) -> str:
        t = seq.t
        if t.kind == "vtup":
            return seq.code
        if t.kind == "tup":
            if len(set(t.args)) > 1:
                joined = normalize_union(list(t.args))
                if joined.kind == "union" or joined == ANY:
                    raise self.fail(node, "iterating a mixed tuple %r" % (t,))
                return self.coerce(seq, VTUP(joined), node)
            return self.coerce(seq, VTUP(t.args[0]), node)
        if t.kind == "stk":
            return "stk_tup_%s(s)" % t.args[0]
        if t == TABLE and _is_const(seq):
            raise self.fail(node, "iterating a table")
        raise self.fail(node, "iterating %r" % (t,))

    def seq_elem(self, seq: E, node: Any) -> T:
        t = seq.t
        if t.kind == "vtup":
            return t.args[0]
        if t.kind == "tup":
            return normalize_union(list(t.args)) if len(set(t.args)) > 1 else t.args[0]
        if t.kind == "stk":
            return STACKS[t.args[0]]
        raise self.fail(node, "iterating %r" % (t,))

    def _for_body(
        self, st: Any, header: str, elems: list[E], ind: str, lines: list[str]
    ) -> None:
        lines.append(ind + header)
        lines.extend(self.assign(st.index, elems[0], ind + "    ", st))
        inner, _ = self.block(st.body.body, ind + "    ")
        lines.extend(inner)
        lines.append(ind + "}")

    def s_DelStmt(self, st: Any, ind: str) -> tuple[list[str], bool]:
        raise self.fail(st, "del")

    def s_FuncDef(self, st: Any, ind: str) -> tuple[list[str], bool]:
        raise self.fail(st, "nested function")

    # -- the function ---------------------------------------------------------------

    def run(self, inline: str) -> str:
        sig = self.sig
        params = []
        static_decls = []
        for (pname, pt, _d), arg in zip(sig.params, self.fdef.arguments, strict=True):
            if pname in sig.state_params:
                continue
            name, _t = self.declare(arg.variable, pt)
            if pname in self.static and isinstance(self.static[pname], PS):
                self.env[id(arg.variable)] = self.static[pname]
                params.append("mut %s: %s" % (name, self.rt(pt)))
                continue
            if pname in self.static:
                value = self.static[pname]
                self.env[id(arg.variable)] = value
                try:
                    code = self.coerce(self.static_e(value, pt, None), pt, None)
                except TranspileError:
                    code = "__NOREPR__"
                if "__NOREPR__" in code:
                    code = "Default::default()"
                static_decls.append(
                    "    let mut %s: %s = %s;" % (name, self.rt(pt), code)
                )
                continue
            params.append("mut %s: %s" % (name, self.rt(pt)))
        n_params = len(self.local_order)
        body, term = self.block(self.fdef.body.body, "    ")
        if not term and self.pe:
            self.exit_facts.append(dict(self.facts))
        lines = []
        lines.append("#[%s]" % inline)
        lines.append(
            "pub fn %s(%s) -> R<%s> {"
            % (
                sig.rust_name,
                ", ".join(
                    ["s: &mut St"] + (["rf: &mut Rf"] if self.blk else []) + params
                ),
                self.rt(sig.ret),
            )
        )
        used = set(_IDENT_RE.findall("\n".join(body)))
        for decl in static_decls:
            if decl.split("let mut ", 1)[1].split(":", 1)[0] in used:
                lines.append(decl)
        for key in self.local_order[n_params:]:
            name, t = self.locals[key]
            if t == STATE or (self.pe and name not in used):
                continue
            lines.append(
                "    let mut %s: %s = Default::default();" % (name, self.rt(t))
            )
        lines.extend(body)
        if not term:
            if (
                sig.ret in (NONE, LSTATE, ERASED)
                or sig.ret.kind == "opt"
                or sig.ret == SPECVIEW
            ):
                lines.append("    #[allow(unreachable_code)]")
                lines.append("    return Ok(%s);" % self.none_value(sig.ret, None))
            else:
                lines.append("    #[allow(unreachable_code)]")
                lines.append("    unreachable!()")
        lines.append("}")
        if self.pe:
            lines = _prune(lines)
        text = "\n".join(lines)
        if "__NOREPR__" in text:
            bad = [ln.strip() for ln in text.split("\n") if "__NOREPR__" in ln][:2]
            raise TranspileError(
                "%s: a static value with no Rust form is used at run time: %s"
                % (self.fullname, " | ".join(bad))
            )
        return text


def _state_assigns(core: infer.Core) -> dict[str, set[str]]:
    """Per core function, the FACT_ATTRS it or its callees assign."""
    own: dict[str, set[str]] = {}
    calls: dict[str, set[str]] = {}
    for name, fdef in core.funcs.items():
        mine: set[str] = set()
        callees: set[str] = set()
        for n in infer.walk(fdef.body):
            kind = type(n).__name__
            targets = []
            if kind == "AssignmentStmt":
                targets = list(n.lvalues)
            elif kind == "OperatorAssignmentStmt":
                targets = [n.lvalue]
            for t in targets:
                items = t.items if isinstance(t, (mn.TupleExpr, mn.ListExpr)) else [t]
                for item in items:
                    if isinstance(item, mn.MemberExpr) and item.name in FACT_ATTRS:
                        mine.add(item.name)
            if kind == "CallExpr":
                c = n.callee
                if isinstance(c, mn.NameExpr) and isinstance(c.node, mn.FuncDef):
                    callees.add(c.node.fullname)
                for o in core.dispatch.get(id(n), ()):
                    if o.kind == "fn":
                        callees.add(o.key)
        own[name] = mine
        calls[name] = callees
    result = {k: set(v) for k, v in own.items()}
    changed = True
    while changed:
        changed = False
        for name in result:
            for c in calls[name]:
                extra = result.get(c, set()) - result[name]
                if extra:
                    result[name] |= extra
                    changed = True
    return result


def _assigned_vars(node: Any) -> set[int]:
    """id() of every Var a statement (tree) assigns."""
    out: set[int] = set()
    for n in infer.walk(node):
        kind = type(n).__name__
        targets = []
        if kind == "AssignmentStmt":
            targets = list(n.lvalues)
        elif kind == "OperatorAssignmentStmt":
            targets = [n.lvalue]
        elif kind == "ForStmt":
            targets = [n.index]
        while targets:
            t = targets.pop()
            if isinstance(t, (mn.TupleExpr, mn.ListExpr)):
                targets.extend(t.items)
            elif isinstance(t, mn.NameExpr) and isinstance(t.node, mn.Var):
                out.add(id(t.node))
    return out


def _has_jump(node: Any) -> bool:
    """A break or continue that leaves NODE's own loop body."""
    for n in infer.walk(node):
        if type(n).__name__ in ("BreakStmt", "ContinueStmt"):
            return True
    return False


# ---------------------------------------------------------------------------
# Translator: generated tables and output
# ---------------------------------------------------------------------------


def _stable_key(table: Any) -> str:
    """A deterministic sort key for a constant table."""
    if isinstance(table, (dict, pytypes.MappingProxyType)):
        return "d%06d%s" % (len(table), repr(list(table.items())[:4]))
    return "s%06d%s" % (len(table), repr(sorted(table, key=repr)[:4]))
