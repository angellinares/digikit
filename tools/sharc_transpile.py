"""Translate tools/sharc_core (the SHARC+ instruction semantics) to Rust.

The translatable subset is tools/sharc_core/SUBSET.md; this is the
translator it anticipates. It walks the syntax tree mypy builds from each
core module (``ast`` underneath, with names resolved and every expression
typed -- see tools/sharc_transpile_infer.py for how the core's
unannotated handlers get their types), and emits one Rust module per core
module. Every module-level function becomes a Rust ``fn`` over the native
runtime in native/sharc/src/rt.rs:

- Python ``int`` -> ``Int`` (i128), ``float`` -> ``f64``, ``bool``,
  ``str`` -> ``Sym`` (an interned string id; strings that only feed
  observability become ``SYM_DYN`` and are never built).
- The value lattice (``Const``/``Unknown``/``PartialConst``) -> ``V``
  (known-bit mask and bits); ``MR`` -> ``MR``; ``Operand | MR`` -> ``Spec``.
  ``Affine`` never exists natively (the concrete driver never builds one).
- ``FlagUpdate`` and the other NamedTuple records -> Rust structs.
- ``State`` -> the implicit ``s: &mut St`` every function takes; register
  maps (``state.uregs``, a ``_snapshot_uregs`` copy, a ``_pey_view``) ->
  ``RegView`` tags read through ``s``.
- Function values (op tables, ``transfer = _transfer if j else ...``) ->
  ``FnId``; a call through one is a ``match`` over the functions the
  points-to analysis says can reach it.
- A fork (``_copy``), a stop (``_stop``), any ``raise``, an unmodelled
  lookup (KeyError), or a boundary the runtime does not model returns
  ``Err(Trap)``: the caller undoes the instruction and lets the Python core
  execute it.
- Observability (``_event``, trace updates, Unknown and stop reasons) is
  erased.

The boundary SUBSET.md declares (values.py, the state.py helpers listed
there, generation-time decode) is not translated; ``BOUNDARY`` below lists
it together with the memory store (memory.py's byte access over the loader
image and write overlay) that the runtime implements by hand.

Anything outside the subset stops the translation with the module, line
and construct.

Usage (writes out/native/gen/core.rs; it holds no firmware, but lives
next to the firmware-derived block code it is compiled with):

    uv run python tools/sharc_transpile.py [--out out/native/gen]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sys
import types as pytypes
from dataclasses import dataclass
from typing import Any

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

from mypy import nodes as mn  # noqa: E402
from mypy import types as mt  # noqa: E402

import sharc_transpile_infer as infer  # noqa: E402

ROOT = os.path.dirname(HERE)


class TranspileError(Exception):
    """A construct outside the translatable subset."""


# ---------------------------------------------------------------------------
# Native types
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class T:
    """A native type. KIND is one of the names below; ARGS are T or str."""

    kind: str
    args: tuple = ()

    def __repr__(self) -> str:
        if not self.args:
            return self.kind
        return "%s(%s)" % (self.kind, ", ".join(repr(a) for a in self.args))


INT = T("int")
FLOAT = T("float")
BOOL = T("bool")
STR = T("str")
NONE = T("none")
VAL = T("v")  # Const | Unknown | PartialConst (| Affine: never native)
MR = T("mr")
SPEC = T("spec")  # Operand | MR
VI = T("vi")  # Value | int (memory addresses)
STATE = T("state")
INSN = T("insn")
FIELDS = T("fields")
REGVIEW = T("regview")
SPECVIEW = T("specview")
LSTATE = T("lstate")  # list[State]: Ok(()) natively
CONCRETE = T("concrete")  # the loader image handle
ERASED = T("erased")  # an observability-only value
FN = T("fn")
TABLE = T("table")
ANY = T("any")
CONFIG_MAP = T("configmap")  # state.provisional_interpretations
CFGSET = T("cfgset")  # state.provisional_forms


def OPT(t: T) -> T:
    return T("opt", (t,))


def TUP(*items: T) -> T:
    return T("tup", tuple(items))


def VTUP(t: T) -> T:
    return T("vtup", (t,))


def REC(name: str) -> T:
    return T("rec", (name,))


def UNION(items: list[T]) -> T:
    uniq: list[T] = []
    for i in items:
        if i not in uniq:
            uniq.append(i)
    uniq.sort(key=repr)
    return T("union", tuple(uniq))


RECORDS = {
    "sharc_core.values.FlagUpdate": "FlagUpdate",
    "sharc_core.compute_mult.MultSpec": "MultSpec",
    "sharc_core.compute_multi.MultifnOperands": "MultifnOperands",
    "sharc_core.state.Pending": "Pending",
    "sharc_core.state.Loop": "Loop",
}
VALUE_CLASSES = {
    "sharc_core.values.Const",
    "sharc_core.values.Unknown",
    "sharc_core.values.Affine",
    "sharc_core.values.PartialConst",
}


def normalize_union(items: list[T]) -> T:
    flat: list[T] = []
    for i in items:
        if i.kind == "union":
            flat.extend(i.args)
        elif i.kind == "opt":
            flat.append(NONE)
            flat.append(i.args[0])
        else:
            flat.append(i)
    if ANY in flat:
        return ANY
    has_none = NONE in flat
    rest = [i for i in flat if i != NONE]
    kinds = set(rest)
    if FIELDS in kinds and VAL in kinds:
        # Const | dict[str, int] | None: _load_normal_ureg's result. The
        # dict (a combined-PX summary) only feeds observability, so natively
        # it is None; the Const is kept.
        kinds.discard(FIELDS)
    if SPECVIEW in kinds:
        # Mapping[str, Operand | MR] | None: the view carries None itself.
        kinds.discard(SPECVIEW)
        if kinds:
            raise TranspileError("special-register view mixed with %s" % kinds)
        return SPECVIEW
    if BOOL in kinds and INT in kinds:
        kinds.discard(BOOL)
    if VAL in kinds and MR in kinds:
        kinds -= {VAL, MR}
        kinds.add(SPEC)
    if SPEC in kinds:
        kinds -= {VAL, MR}
    if VAL in kinds and INT in kinds and len(kinds) == 2:
        kinds = {VI}
    # Tuples of the same length join elementwise.
    tups = [k for k in kinds if k.kind == "tup"]
    if len(tups) > 1 and len({len(t.args) for t in tups}) == 1:
        joined = TUP(
            *[
                normalize_union([t.args[i] for t in tups])
                for i in range(len(tups[0].args))
            ]
        )
        kinds -= set(tups)
        kinds.add(joined)
    vtups = [k for k in kinds if k.kind == "vtup"]
    if len(vtups) > 1:
        joined = VTUP(normalize_union([t.args[0] for t in vtups]))
        kinds -= set(vtups)
        kinds.add(joined)
    if LSTATE in kinds and len(kinds) > 1:
        kinds = {LSTATE}
    if len(kinds) == 0:
        base = NONE
    elif len(kinds) == 1:
        base = next(iter(kinds))
    else:
        base = UNION(sorted(kinds, key=repr))
    if has_none and base != NONE:
        if base.kind == "opt":
            return base
        return OPT(base)
    return base


def from_mypy(t: mt.Type | None, where: str = "") -> T:
    if t is None:
        return ANY
    p = mt.get_proper_type(t)
    if isinstance(p, mt.AnyType):
        return ANY
    if isinstance(p, mt.NoneType):
        return NONE
    if isinstance(p, mt.UninhabitedType):
        return NONE
    if isinstance(p, mt.LiteralType):
        return from_mypy(p.fallback, where)
    if isinstance(p, mt.UnionType):
        return normalize_union([from_mypy(i, where) for i in p.items])
    if isinstance(p, mt.TupleType):
        fb = p.partial_fallback.type.fullname
        if fb in RECORDS:
            return REC(RECORDS[fb])
        return TUP(*[from_mypy(i, where) for i in p.items])
    if isinstance(p, (mt.CallableType, mt.Overloaded)):
        return FN
    if isinstance(p, mt.TypeType):
        return ANY
    if isinstance(p, mt.Instance):
        name = p.type.fullname
        if name == "builtins.int":
            return INT
        if name == "builtins.bool":
            return BOOL
        if name == "builtins.float":
            return FLOAT
        if name == "builtins.str":
            return STR
        if name in VALUE_CLASSES:
            return VAL
        if name == "sharc_core.state.MR":
            return MR
        if name == "sharc_core.state.State":
            return STATE
        if name == "sharc_disasm.Instruction":
            return INSN
        if name == "sharcldr.LoadedMemory":
            return CONCRETE
        if name in RECORDS:
            return REC(RECORDS[name])
        if name in ("builtins.tuple",):
            return VTUP(from_mypy(p.args[0], where))
        if name == "builtins.list":
            item = from_mypy(p.args[0], where)
            if item == STATE:
                return LSTATE
            return VTUP(item)
        if name in (
            "builtins.dict",
            "typing.Mapping",
            "collections.abc.Mapping",
            "types.MappingProxyType",
            "typing.MutableMapping",
        ):
            k = from_mypy(p.args[0], where)
            v = from_mypy(p.args[1], where)
            if k == STR and v == INT:
                return FIELDS
            if k == INT and v in (VAL,):
                return REGVIEW
            if k == STR and v in (SPEC, VAL, MR):
                return SPECVIEW
            if k == STR and v == STR:
                return CONFIG_MAP
            if v == FN or v.kind == "table":
                return TABLE
            return TABLE
        if name in ("builtins.frozenset", "builtins.set"):
            return TABLE
        if name == "builtins.bytearray" or name == "builtins.bytes":
            return ERASED
        if name == "builtins.object":
            return ANY
        if name == "builtins.dict_items":
            return ANY
        if name == "sharc_core.memory.UnmodeledMMR":
            return ERASED
        if name == "builtins.ValueError" or name == "builtins.KeyError":
            return ERASED
        if name == "builtins.function":
            return FN
    raise TranspileError("no native type for %s %s" % (p, where))


# ---------------------------------------------------------------------------
# Strings
# ---------------------------------------------------------------------------


# Strings the runtime (native/sharc/src/rt.rs RT_SYMS) knows by number:
# interned first, in this order, and defined there rather than in syms.rs.
RT_SYMS = (
    "",
    "<dyn>",
    "MRF",
    "MRB",
    "MSF",
    "MSB",
    "BFFWRP",
    "BFF_HI",
    "BFF_LO",
    "unknown",
    "confident",
    "uncertain",
    "nop",
    "DM",
    "PM",
    "21p_undoc16",
)


class Syms:
    """Interned strings. 0 is the empty string, 1 a dynamic
    (observability-only) string that is never built."""

    def __init__(self) -> None:
        self.ids: dict[str, int] = {}
        self.names: list[str] = []
        self.idents: list[str] = []
        for name in RT_SYMS:
            self.intern(name)
        self.idents[0] = "S_EMPTY"
        self.idents[1] = "SYM_DYN"

    def intern(self, s: str) -> int:
        if s in self.ids:
            return self.ids[s]
        n = len(self.names)
        self.ids[s] = n
        self.names.append(s)
        base = re.sub(r"[^A-Za-z0-9]+", "_", s).strip("_").upper()[:40] or "E"
        ident = "S_%s" % base
        if ident in self.idents or not base:
            ident = "S_%s_%d" % (base, n)
        self.idents.append(ident)
        return n

    def ident(self, s: str) -> str:
        return self.idents[self.intern(s)]

    def rust(self) -> str:
        lines = [
            "// Interned strings of the SHARC+ core (tools/sharc_transpile.py).",
            "use crate::rt::Sym;",
        ]
        for n, ident in enumerate(self.idents):
            if n < len(RT_SYMS):
                continue  # defined by the runtime
            lines.append(
                "pub const %s: Sym = %d; // %s" % (ident, n, json.dumps(self.names[n]))
            )
        lines.append("pub static SYM_NAMES: [&str; %d] = [" % len(self.names))
        for name in self.names:
            lines.append("    %s," % json.dumps(name))
        lines.append("];")
        return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# The boundary: functions the native runtime implements (rt.rs, module
# bnd). Each takes ``s`` first, then the Python parameters (native types from
# the mypy signature, or the override here), and returns the native type of
# the Python return, as ``R<...>`` when TRAPS.
# ---------------------------------------------------------------------------


@dataclass
class Bnd:
    traps: bool = False
    params: list[T] | None = None  # override the mypy-derived parameter types
    ret: T | None = None
    drop_params: tuple[str, ...] = ()


BOUNDARY: dict[str, Bnd] = {}
for _n in [
    "_affine",
    "symbol",
    "_signed",
    "_signed32",
    "_terms",
    "_stack_bounded_symbol",
    "_add",
    "_negate",
    "_subtract",
    "_multiply",
    "_multiply_fractional",
    "_aconv_symbol",
    "_aconv",
    "_op_and",
    "_op_or",
    "_op_xor",
    "_op_andnot",
    "_bitwise",
    "_not",
    "_is_unknown",
    "_astatx_known_bit",
    "_astatx_define",
    "_astatx_forget",
    "_apply_flag_update",
    "_flags_define",
    "_flags_forget",
    "_flags_put",
    "_flags_from_pairs",
    "_flags_then",
    "_flags_or",
]:
    BOUNDARY["sharc_core.values." + _n] = Bnd()
BOUNDARY["sharc_core.values.symbol"] = Bnd(traps=True)
BOUNDARY["sharc_core.values._aconv_symbol"] = Bnd(traps=True)
BOUNDARY["sharc_core.values._aconv"] = Bnd(traps=True)
for _n in [
    "_mr_from_signed",
    "_mr_read_word",
    "_mr_write_word",
    "_ureg",
    "_ureg_raw",
    "_snapshot_uregs",
    "_pey_view",
    "_pey_special",
]:
    BOUNDARY["sharc_core.state." + _n] = Bnd()
for _n in ["_concrete_address", "_byte_present"]:
    BOUNDARY["sharc_core.memory." + _n] = Bnd()
for _n in [
    "_canonical_dm_address",
    "dm_write_range",
    "_dm_read",
    "_read_px48",
    "_dm_write",
]:
    BOUNDARY["sharc_core.memory." + _n] = Bnd(traps=True)
BOUNDARY["sharc_core.memory._load_normal_ureg"] = Bnd(traps=True, ret=OPT(VAL))
BOUNDARY["sharc_core.encoding._field"] = Bnd(traps=True)
BOUNDARY["sharc_core.encoding._wide"] = Bnd(traps=True)
BOUNDARY["sharc_core.encoding._split_compute_fields"] = Bnd(ret=FIELDS)
BOUNDARY["sharc_core.sequencer.decode_at"] = Bnd(
    traps=True, params=[CONCRETE, OPT(INT), INT]
)
for _n in [
    "_f32_from_bits",
    "_f64_from_words",
    "_ldexp",
    "_trunc_int",
    "_round_even_int",
    "_float32_bits",
    "_double_pair_bits",
]:
    BOUNDARY["sharc_core.floats." + _n] = Bnd()

# Boundary functions that touch the register file: block mode calls their
# ``_rf`` forms over the block's register file (rt.rs Rf, bnd.rs).
BLK_REG_BOUNDARY = {
    "sharc_core.state._ureg",
    "sharc_core.state._ureg_raw",
    "sharc_core.state._snapshot_uregs",
    "sharc_core.state._pey_view",
    "sharc_core.memory._load_normal_ureg",
}

# Record fields the runtime stores narrower than Int (rt.rs): the values
# are 32-bit machine quantities (PCs, counts), so the conversion is exact.
NARROW_FIELDS: dict[str, dict[str, str]] = {
    "Loop": {"start_sw": "i64", "end_sw": "i64", "remaining": "i64", "mode": "i64"},
}

# Registers block code can know at an instruction's start (a fact, like
# pc_sw): the block's entry guard compares them (tools/sharc_rsgen.py), so
# reads fold until the block writes one. Code -> fact name.
REG_FACTS = {114: "MODE1"}
# Data-memory reads and writes (boundary functions). An instruction whose
# code reads no memory does not log its writes in block code: if it traps
# later, running it again stores the same bytes (fact "nolog").
MEM_READS = {
    "sharc_core.memory._dm_read",
    "sharc_core.memory._read_px48",
    "sharc_core.memory._load_normal_ureg",
}
MEM_WRITES = {"sharc_core.memory._dm_write"}
# Block code calls their _b forms (no run-configuration test: block code
# runs only under the default one).
MEM_FAST = {"sharc_core.memory._dm_read", "sharc_core.memory._dm_write"}
# Facts that are settings, not machine state: kept across loops.
SETTING_FACTS = ("nolog",)

# Registers that are PartialConst in practice (ASTATX, ASTATY: the CACC bits
# are not known): block code keeps their known-bit masks at run time.
PARTIAL_REGS = frozenset({118, 119})
# Their usual known-bit masks (the CACC compare history, ASTATX bits 24-31,
# is never known): block code requires these at entry, so they fold.
PARTIAL_MASKS = {118: 0x00FFFFFF, 119: 0xFFFFFFFF}

# Function values the runtime knows by number (rt.rs FN_OP_*).
RT_FN_IDS = {
    "sharc_core.values._op_and": "FN_OP_AND",
    "sharc_core.values._op_or": "FN_OP_OR",
    "sharc_core.values._op_xor": "FN_OP_XOR",
    "sharc_core.values._op_andnot": "FN_OP_ANDNOT",
}

# Calls that end the instruction on the native side.
TRAP_CALLS = {
    "sharc_core.state._copy": "fork",
    "sharc_core.state._stop": "stop",
    "sharc_core.memory._dossier": "external-call",
}
# Calls erased with their arguments (observability, per-run reports).
ERASED_CALLS = {
    "sharc_core.state._event",
    "sharc_core.state._note_provisional",
}
# Calls whose value is an observability-only string or structure.
DYN_CALLS = {
    "sharc_core.state._render": STR,
    "sharc_core.state._json_value": ERASED,
    "builtins.str": STR,
    "builtins.hex": STR,
    "builtins.repr": STR,
}
# Module-level float aliases (floats.py) and constants.
FLOAT_FUNCS = {
    "sharc_core.floats._isnan": ("f_isnan", [FLOAT], BOOL),
    "sharc_core.floats._isinf": ("f_isinf", [FLOAT], BOOL),
    "sharc_core.floats._isfinite": ("f_isfinite", [FLOAT], BOOL),
    "sharc_core.floats._copysign": ("f_copysign", [FLOAT, FLOAT], FLOAT),
    "sharc_core.floats._sqrt": ("f_sqrt", [FLOAT], FLOAT),
    "math.isnan": ("f_isnan", [FLOAT], BOOL),
    "math.isinf": ("f_isinf", [FLOAT], BOOL),
    "math.copysign": ("f_copysign", [FLOAT, FLOAT], FLOAT),
}
# State attributes: name -> (native type, read code, writable).
STATE_ATTRS: dict[str, tuple[T, str, bool]] = {
    "pc_sw": (INT, "s.pc_sw", True),
    "pending": (OPT(REC("Pending")), "s.pending", True),
    "steps": (INT, "s.steps", True),
    "concrete": (CONCRETE, "()", False),
    "follow_loaded_calls": (BOOL, "s.cfg.follow_loaded_calls", False),
    "continue_external_calls": (BOOL, "s.cfg.continue_external_calls", False),
    "max_call_depth": (INT, "s.cfg.max_call_depth", False),
    "assume_nw32": (BOOL, "s.cfg.assume_nw32", False),
    "explicit_memory_model": (BOOL, "s.cfg.explicit_memory_model", False),
    "approx_recips": (BOOL, "s.cfg.approx_recips", False),
    "data_memory_tainted": (BOOL, "s.cfg.data_memory_tainted", False),
    "dossier_bytes": (INT, "s.cfg.dossier_bytes", False),
    "at_loaded_entry": (BOOL, "s.at_loaded_entry", True),
    "provisional_forms": (T("cfgset"), "()", False),
    "provisional_interpretations": (CONFIG_MAP, "()", False),
}
# State attributes whose writes are erased (reports, not machine state).
# State attributes block code keeps in its register file (rt.rs Rf) from
# entry to exit, like the registers.
BLK_RF_ATTRS = {"pc_sw": "rf.pc", "pending": "rf.pending"}
# State attributes nothing reads back and the canonical state leaves out:
# block code does not keep them (the one-instruction interpreter still does).
BLK_ERASED_WRITES = {"steps", "at_loaded_entry"}
STATE_ERASED_WRITES = {
    "approx_recips_used",
    "provisional_interpreted",
    "provisional_used",
    "trace",
}


# ---------------------------------------------------------------------------
# Expressions
# ---------------------------------------------------------------------------


class _NoConst:
    """E.const when the value is not known at generation time."""

    def __repr__(self) -> str:
        return "<dynamic>"


_NOCONST: Any = _NoConst()


class PS:
    """A partially static tuple (partial evaluation): PARTS holds each
    item's generation-time value, or _NOCONST where it is known only at run
    time. A local holding one keeps its Rust value too."""

    __slots__ = ("parts",)

    def __init__(self, parts: tuple) -> None:
        self.parts = parts

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, PS) or len(other.parts) != len(self.parts):
            return False
        for a, b in zip(self.parts, other.parts, strict=True):
            if (a is _NOCONST) != (b is _NOCONST):
                return False
            if a is not _NOCONST and not _same_static(a, b):
                return False
        return True

    def __hash__(self) -> int:
        return len(self.parts)

    def __repr__(self) -> str:
        return "PS%r" % (self.parts,)


def _ps_parts(e: E) -> tuple | None:
    """The per-item static values of tuple E (None when no item is)."""
    if _is_const(e) and isinstance(e.const, tuple) and not isinstance(e.const, Tag):
        return tuple(e.const)
    if e.parts is not None and any(p is not _NOCONST for p in e.parts):
        return e.parts
    return None


def _ps_item(e: E) -> Any:
    """E as an item of a partially static tuple: its value, a nested PS,
    or _NOCONST."""
    if _is_const(e) and not isinstance(e.const, Tag):
        return e.const
    parts = _ps_parts(e)
    if parts is not None:
        return PS(parts)
    return _NOCONST


def _merge_item(vals: list) -> Any:
    """Join of partially static items: equal statics stay, a None next to
    partially static tuples (an Optional) keeps their parts."""
    ps = [v for v in vals if isinstance(v, PS)]
    if ps:
        if all(isinstance(v, PS) or v is None for v in vals) and all(
            p == ps[0] for p in ps[1:]
        ):
            return ps[0]
        return _NOCONST
    v0 = vals[0]
    if v0 is _NOCONST or isinstance(v0, Tag):
        return _NOCONST
    if any(v is _NOCONST or not _same_static(v, v0) for v in vals[1:]):
        return _NOCONST
    return v0


@dataclass(frozen=True)
class Tag:
    """A static value that exists only natively (a register or special
    register view, the loader image handle): its Rust code."""

    code: str


# The run configuration block code is generated for (State fields that do
# not change during a run; sharc_harness._make_runner's). Block code checks
# it at entry and leaves anything else to the one-instruction interpreter.
GEN_CFG_DEFAULT: dict[str, Any] = {
    "follow_loaded_calls": True,
    "continue_external_calls": False,
    "max_call_depth": 64,
    "assume_nw32": True,
    "explicit_memory_model": True,
    "approx_recips": True,
    "data_memory_tainted": False,
    "dossier_bytes": 0,
    "provisional_forms": (),
    "provisional_interpretations": {},
}

# State attributes block code can know at an instruction's start: the PC
# (the instruction's own address) and whether a delayed transfer is pending.
FACT_ATTRS = ("pc_sw", "pending")

# Parameters that only feed observability (Unknown and stop reasons, event
# text): never a reason to specialize a function.
OBSERVABILITY_PARAMS = {"expression", "label", "rendered", "reason", "note", "text"}

# Boundary functions without state access: evaluated at generation time
# when every argument is static.
PURE_BOUNDARY = {
    "sharc_core.encoding._field",
    "sharc_core.encoding._wide",
    "sharc_core.encoding._split_compute_fields",
    "sharc_core.memory._concrete_address",
    "sharc_core.state._mr_from_signed",
    "sharc_core.state._mr_read_word",
    "sharc_core.state._mr_write_word",
}


@dataclass
class E:
    """A translated expression: Rust CODE of native type T. S is True when
    the code reads or writes the state (so it cannot be an argument next to
    another use of ``s``). CONST is the value when it is known at generation
    time (a literal, a module constant, or partial evaluation), else
    _NOCONST."""

    code: str
    t: T
    s: bool = False
    const: Any = _NOCONST
    pts: frozenset | None = None  # points-to set for FN/TABLE values
    # Values a dynamic int can take (a DO loop's end address: one of the
    # image's DO instructions' ends), or None.
    vset: frozenset | None = None
    # A dynamic tuple's items known at generation time (PS.parts), or None.
    parts: tuple | None = None


def _is_const(e: E) -> bool:
    return e.const is not _NOCONST


def lit_int(v: int) -> str:
    return "(%di128)" % v if v < 0 else "%di128" % v


def lit_float(v: float) -> str:
    if math.isnan(v) or math.isinf(v) or v == 0.0:
        return "f64::from_bits(0x%016x)" % (
            int.from_bytes(__import__("struct").pack(">d", v), "big")
        )
    r = repr(v)
    if "e" not in r and "." not in r:
        r += ".0"
    return "(%s_f64)" % r


@dataclass
class FnSig:
    fullname: str
    module: str
    name: str
    params: list[tuple[str, T, Any]]  # (name, type, default node or None)
    param_kinds: list[Any]
    ret: T
    state_params: set[str]
    rust_name: str


class Translator:
    def __init__(self, core: infer.Core) -> None:
        self.core = core
        self.syms = Syms()
        self.sigs: dict[str, FnSig] = {}
        self.fn_ids: dict[str, int] = {}
        self.fn_names: list[str] = []
        self.unions: dict[T, str] = {}
        self.union_order: list[T] = []
        self.consts_needed: dict[str, set[str]] = {}
        self.const_items: dict[str, dict[str, str]] = {}  # module -> name -> code
        self.lookup_fns: dict[str, str] = {}  # key -> rust fn text
        self.trap_sites: list[str] = []
        self.sym_fns: dict[str, tuple[str, Any]] = {}
        self.errors: list[str] = []
        self.table_ids: dict[int, int] = {}
        self.table_objs: list[Any] = []
        self.lookup_keys: set = set()
        self.affix_ids: dict[tuple, int] = {}
        self.called: set[str] = set()
        self.trap_ids: dict[str, int] = {}
        # Partial evaluation (block code): specialized variants by key and
        # by text, their Rust text per module, the fact attributes each may
        # change, the Rust names of static instructions, the run
        # configuration block code assumes, and the image's DO loop ends.
        self.variants: dict[Any, str] = {}
        self.variant_by_text: dict[str, str] = {}
        self.variant_texts: list[tuple[int, str]] = []
        self.variant_busy: set = set()
        self.variant_effects: dict[str, set[str]] = {}
        self.variant_modules = 16
        self.static_names: dict[int, str] = {}
        self.gen_cfg: dict[str, Any] = dict(GEN_CFG_DEFAULT)
        self.loop_ends: frozenset | None = None
        self.block_failures: list[tuple[int, str]] = []
        self._assigns: dict[str, set[str]] | None = None
        # Block mode (tools/sharc_rsgen.py): variants keep the register file
        # in the block's local ``rf`` (rt.rs Rf) and take it after ``s``;
        # every register index must then be a generation-time constant.
        # ``variant_regs[path]`` is what a variant reads from the current
        # file, reads from the instruction-start snapshot, and writes.
        self.blk = False
        self.variant_regs: dict[str, tuple[frozenset, frozenset, frozenset]] = {}
        self.variant_putregs: dict[str, frozenset] = {}
        # The items every return of a variant has static (PS.parts), or None.
        self.variant_parts: dict[str, tuple | None] = {}
        self.variant_pure: dict[str, bool] = {}
        # Variants too small to call: the statement they amount to.
        self.variant_inline: dict[str, str] = {}
        self.variant_logs: dict[str, bool] = {}
        # Register facts known after a variant returns (a MODE1 it set).
        self.variant_facts_out: dict[str, dict] = {}
        # Whether a variant (with its callees) reads data memory.
        self.variant_memreads: dict[str, bool] = {}
        self.variant_memwrites: dict[str, bool] = {}
        self.variant_const: dict[str, Any] = {}
        self._intern_decode_names()

    # -- setup -------------------------------------------------------------

    def _intern_decode_names(self) -> None:
        import sharc_disasm

        for t in sharc_disasm.TYPES:
            self.syms.intern(t["name"])
            for f in t["fields"]:
                label = f if isinstance(f, str) else f["label"]
                self.syms.intern(label)
                self.syms.intern(label.split("[")[0])
        for s in (
            "unknown",
            "confident",
            "uncertain",
            "compute[22:16]",
            "compute[15:0]",
        ):
            self.syms.intern(s)

    def fn_id(self, fullname: str) -> int:
        if fullname not in self.fn_ids:
            self.fn_ids[fullname] = len(self.fn_names)
            self.fn_names.append(fullname)
        return self.fn_ids[fullname]

    def fn_const(self, fullname: str) -> str:
        if fullname in RT_FN_IDS:
            return RT_FN_IDS[fullname]
        self.fn_id(fullname)
        return "FN_" + re.sub(r"[^A-Za-z0-9]", "_", fullname.replace("sharc_core.", ""))

    def trap(self, where: str, kind: str) -> str:
        site = "%s: %s" % (where, kind)
        if site not in self.trap_ids:
            self.trap_ids[site] = len(self.trap_sites)
            self.trap_sites.append(site)
        return "Trap(%d)" % self.trap_ids[site]

    def union_name(self, t: T) -> str:
        if t not in self.unions:
            self.unions[t] = "U%d" % len(self.unions)
            self.union_order.append(t)
            for a in t.args:
                self.rust_type(a)
        return self.unions[t]

    def rust_type(self, t: T) -> str:
        k = t.kind
        simple = {
            "int": "Int",
            "float": "f64",
            "bool": "bool",
            "str": "Sym",
            "none": "()",
            "v": "V",
            "mr": "MR",
            "spec": "Spec",
            "vi": "VI",
            "insn": "&'static Insn",
            "fields": "&'static Fields",
            "regview": "RegView",
            "specview": "SpecView",
            "lstate": "()",
            "concrete": "()",
            "erased": "()",
            "fn": "FnId",
            "table": "TblId",
            "configmap": "()",
            "cfgset": "()",
            "state": "()",
        }
        if k in simple:
            return simple[k]
        if k == "opt":
            return "Option<%s>" % self.rust_type(t.args[0])
        if k == "tup":
            if len(t.args) == 1:
                return "(%s,)" % self.rust_type(t.args[0])
            return "(%s)" % ", ".join(self.rust_type(a) for a in t.args)
        if k == "vtup":
            return "Tup<%s>" % self.rust_type(t.args[0])
        if k == "rec":
            return t.args[0]
        if k == "union":
            return self.union_name(t)
        raise TranspileError("no Rust type for %r" % (t,))

    # -- signatures ---------------------------------------------------------

    def signature(self, fullname: str) -> FnSig:
        if fullname in self.sigs:
            return self.sigs[fullname]
        fdef = self.core.funcs[fullname]
        module, _, name = fullname.rpartition(".")
        ftype = fdef.type
        if not isinstance(ftype, mt.CallableType):
            raise TranspileError("%s has no signature" % fullname)
        params = []
        kinds = []
        state_params: set[str] = set()
        for arg, at in zip(fdef.arguments, ftype.arg_types, strict=True):
            pt = from_mypy(at, fullname + "(" + arg.variable.name + ")")
            if pt == STATE:
                state_params.add(arg.variable.name)
            params.append((arg.variable.name, pt, arg.initializer))
            kinds.append(arg.kind)
        ret = from_mypy(ftype.ret_type, fullname + " return")
        b = BOUNDARY.get(fullname)
        if b is not None:
            if b.params is not None:
                params = [
                    (p[0], bt, p[2])
                    for p, bt in zip(
                        [p for p in params if p[1] != STATE], b.params, strict=True
                    )
                ]
                state_params = set()
            if b.ret is not None:
                ret = b.ret
        sig = FnSig(
            fullname=fullname,
            module=module,
            name=name,
            params=params,
            param_kinds=kinds,
            ret=ret,
            state_params=state_params,
            rust_name=rust_ident(name),
        )
        self.sigs[fullname] = sig
        return sig

    def fn_path(self, fullname: str, absolute: bool = False) -> str:
        module, _, name = fullname.rpartition(".")
        if fullname in BOUNDARY:
            return "bnd::%s" % rust_ident(name)
        if absolute:
            return "crate::generated::core_i::%s::%s" % (
                module.split(".")[-1],
                rust_ident(name),
            )
        return "super::%s::%s" % (module.split(".")[-1], rust_ident(name))

    # -- specialized variants (block code) ------------------------------------

    def static_key(self, value: Any) -> Any:
        """A hashable key for static VALUE (instructions by their decode)."""
        cls = type(value).__name__
        if isinstance(value, PS):
            return (
                "ps",
                tuple(
                    "<dyn>" if p is _NOCONST else self.static_key(p)
                    for p in value.parts
                ),
            )
        if cls == "Instruction":
            return (
                "insn",
                value.type_name,
                value.length_bytes,
                value.kind,
                tuple(value.fields.items()),
            )
        if isinstance(value, dict):
            return ("dict", tuple((k, self.static_key(v)) for k, v in value.items()))
        if isinstance(value, pytypes.MappingProxyType):
            return ("dict", id(value))
        if isinstance(value, (list, tuple)) and cls in ("list", "tuple"):
            return (cls, tuple(self.static_key(v) for v in value))
        if isinstance(value, (pytypes.FunctionType, pytypes.BuiltinFunctionType)):
            return ("fn", getattr(value, "__module__", ""), value.__qualname__)
        try:
            hash(value)
        except TypeError:
            return ("id", id(value))
        return (cls, value)

    def variant(
        self, fullname: str, static: dict[str, Any], facts: dict[str, Any] | None = None
    ) -> str:
        """Rust path of FULLNAME specialized on STATIC (parameter -> value)
        and on FACTS (state attribute -> value at entry), generated on first
        use. ``variant_effects[path]`` is the set of fact attributes it may
        change."""
        facts = facts or {}
        key = (
            fullname,
            tuple(sorted((k, self.static_key(v)) for k, v in static.items())),
            tuple(sorted((k, self.static_key(v)) for k, v in facts.items())),
        )
        found = self.variants.get(key)
        if found is not None:
            return found
        if key in self.variant_busy:
            raise TranspileError("recursive specialization of %s" % fullname)
        self.variant_busy.add(key)
        try:
            fnt = FnT(
                self,
                fullname,
                static=static,
                pe=True,
                rust_name="__VARIANT__",
                facts=facts,
            )
            text = fnt.run("inline(always)")
        finally:
            self.variant_busy.discard(key)
        # Instructions that differ only in what a function does not read
        # get the same code: share it.
        summary = fnt.ret_summary()
        # What callers learn from a variant besides its code (returned
        # statics, facts it changes or leaves known) is part of its identity.
        facts_out = fnt.facts_out()
        digest = hashlib.sha1(
            "\n//".join(
                [
                    text,
                    repr(self.static_key(PS(summary))) if summary else "",
                    repr(sorted(fnt.effects)),
                    repr(sorted(facts_out.items())),
                    repr((fnt.memreads, fnt.memwrites)),
                ]
            ).encode()
        ).hexdigest()
        path = self.variant_by_text.get(digest)
        if path is None and self.blk:
            pc_only = _only_sets_pc(text)
            if pc_only is not None:
                # Callers use the statement itself (variant_inline).
                path = "@inline%d" % len(self.variant_inline)
                self.variant_inline[path] = pc_only
                self.variant_by_text[digest] = path
        if path is None and self.blk:
            # A variant that only calls another with its own parameters is
            # that one: callers call it directly.
            target = _forwards_to(text)
            if target is not None:
                path = target
                self.variant_by_text[digest] = path
        if path is None:
            index = len(self.variant_by_text)
            module = index % self.variant_modules
            name = "%s__v%d" % (rust_ident(fullname.rpartition(".")[2]), index)
            path = "crate::generated::image::spec_%02d::%s" % (module, name)
            self.variant_by_text[digest] = path
            self.variant_texts.append((module, text.replace("__VARIANT__", name)))
        self.variant_effects[path] = set(fnt.effects)
        self.variant_facts_out[path] = facts_out
        self.variant_memreads[path] = fnt.memreads
        self.variant_memwrites[path] = fnt.memwrites
        self.variant_parts[path] = summary
        # A variant without effects or traps whose every return has the same
        # static value folds to that value at its call sites.
        body = text.split("\n", 2)[-1]
        pure = not _EFFECT_RE.search(body) and all(
            self.variant_pure.get(m, False) for m in _VARIANT_CALL_RE.findall(body)
        )
        self.variant_pure[path] = pure
        # Whether it (or a callee) records anything in the undo log.
        self.variant_logs[path] = bool(_LOGS_RE.search(body)) or any(
            self.variant_logs.get(m, True) for m in _VARIANT_CALL_RE.findall(body)
        )
        rc = fnt.ret_const()
        self.variant_const[path] = rc if pure else _NOCONST
        self.variant_putregs[path] = frozenset(fnt.putregs)
        self.variant_regs[path] = (
            frozenset(fnt.rregs),
            frozenset(fnt.oregs),
            frozenset(fnt.wregs),
        )
        self.variants[key] = path
        return path

    def state_assigns(self, fullname: str) -> set[str]:
        """FACT_ATTRS FULLNAME (with everything it calls) may assign."""
        if self._assigns is None:
            self._assigns = _state_assigns(self.core)
        return self._assigns.get(fullname, set(FACT_ATTRS))

    # -- constants ------------------------------------------------------------

    def const_value(self, value: Any, t: T, where: str) -> str:
        """Rust code for live Python VALUE at native type T."""
        if isinstance(value, Tag):
            return value.code
        if id(value) in self.static_names:
            return self.static_names[id(value)]
        if type(value).__name__ == "Instruction" or (
            isinstance(value, dict) and t in (FIELDS, ANY)
        ):
            return "__NOREPR__"
        if t.kind == "opt":
            if value is None:
                return "None"
            return "Some(%s)" % self.const_value(value, t.args[0], where)
        if t == SPECVIEW:
            if isinstance(value, pytypes.MappingProxyType) and not value:
                return "SpecView::EMPTY"
            if value is None:
                return "SpecView::NONE"
        if t.kind == "union":
            for i, a in enumerate(t.args):
                if self._value_fits(value, a):
                    return "%s::A%d(%s)" % (
                        self.union_name(t),
                        i,
                        self.const_value(value, a, where),
                    )
            raise TranspileError("%s: %r fits no member of %r" % (where, value, t))
        if isinstance(value, bool):
            if t == INT:
                return lit_int(int(value))
            return "true" if value else "false"
        if isinstance(value, int):
            if t == FLOAT:
                return lit_float(float(value))
            if t == BOOL:
                return "true" if value else "false"
            if t == VI:
                return "VI::I(%s)" % lit_int(value)
            return lit_int(value)
        if isinstance(value, float):
            return lit_float(value)
        if isinstance(value, str):
            return self.syms.ident(value)
        if value is None:
            if t == NONE:
                return "()"
            raise TranspileError("%s: None at %r" % (where, t))
        cls = type(value).__name__
        if cls == "Const":
            code = "V::c(%s)" % lit_int(value.value)
            if t == SPEC:
                return "Spec::V(%s)" % code
            return code
        if cls == "Unknown":
            return "Spec::V(V::UNK)" if t == SPEC else "V::UNK"
        if cls == "MR":
            code = "MR::new(%s, %s)" % (lit_int(value.mask), lit_int(value.bits))
            return "Spec::M(%s)" % code if t == SPEC else code
        if isinstance(value, pytypes.FunctionType):
            name = self.core.live_funcs.get(id(value))
            if name is None:
                raise TranspileError(
                    "%s: function %r outside the core" % (where, value)
                )
            return self.fn_const(name)
        if isinstance(value, pytypes.BuiltinFunctionType) or value is abs:
            return self.fn_const("builtins." + value.__name__)
        if t.kind == "rec":
            rec = t.args[0]
            fields = self.record_fields(rec)
            parts = []
            for fname, ftype in fields:
                code = self.const_value(getattr(value, fname), ftype, where)
                if fname in NARROW_FIELDS.get(rec, ()):
                    code = "(%s) as %s" % (code, NARROW_FIELDS[rec][fname])
                parts.append("%s: %s" % (fname, code))
            return "%s { %s }" % (rec, ", ".join(parts))
        if isinstance(value, (tuple, list)):
            if t.kind == "tup":
                if len(value) != len(t.args):
                    raise TranspileError("%s: tuple length" % where)
                inner = ", ".join(
                    self.const_value(v, a, where)
                    for v, a in zip(value, t.args, strict=True)
                )
                return "(%s,)" % inner if len(value) == 1 else "(%s)" % inner
            if t.kind == "vtup":
                inner = ", ".join(self.const_value(v, t.args[0], where) for v in value)
                return "Tup::from_slice(&[%s])" % inner
        if (
            isinstance(value, (dict, frozenset, set, pytypes.MappingProxyType))
            and t == TABLE
        ):
            return "%d" % self.table_id(value)
        raise TranspileError("%s: no constant for %r as %r" % (where, value, t))

    def _value_fits(self, value: Any, t: T) -> bool:
        if isinstance(value, bool):
            return t in (BOOL, INT)
        if isinstance(value, int):
            return t in (INT, VI)
        if isinstance(value, str):
            return t == STR
        if isinstance(value, tuple):
            if t.kind == "vtup":
                return all(self._value_fits(v, t.args[0]) for v in value)
            if t.kind == "tup":
                return len(value) == len(t.args) and all(
                    self._value_fits(v, a) for v, a in zip(value, t.args, strict=True)
                )
            return False
        cls = type(value).__name__
        if cls in ("Const", "Unknown"):
            return t in (VAL, SPEC)
        if cls == "MR":
            return t in (MR, SPEC)
        return False

    def table_id(self, obj: Any) -> int:
        if id(obj) not in self.table_ids:
            self.table_ids[id(obj)] = len(self.table_objs)
            self.table_objs.append(obj)
        return self.table_ids[id(obj)]

    def record_fields(self, rec: str) -> list[tuple[str, T]]:
        full = {v: k for k, v in RECORDS.items()}[rec]
        module, _, cls = full.rpartition(".")
        info = self.core.trees[module].names[cls].node
        assert isinstance(info, mn.TypeInfo)
        out = []
        if info.tuple_type is not None:
            names = [
                n
                for n in info.names
                if not n.startswith("_")
                and n in info.metadata.get("namedtuple", {}).get("fields", info.names)
            ]
            tt = info.tuple_type
            fieldnames = [n for n in _namedtuple_fields(info)]
            for fname, ft in zip(fieldnames, tt.items, strict=True):
                out.append((fname, from_mypy(ft, full + "." + fname)))
            del names
            return out
        # dataclass
        for name in _dataclass_fields(info):
            var = info.names[name].node
            assert isinstance(var, mn.Var)
            out.append((name, from_mypy(var.type, full + "." + name)))
        return out

    def lookup_fn(self, tables: list, vt: T, kt: T, where: str) -> str:
        if len(tables) == 1:
            return self._lookup_one(tables[0], vt, kt, where)
        ids = sorted(self.table_id(t) for t in tables)
        name = "tbl_multi_%s_get" % "_".join(str(i) for i in ids)
        key = ("multi", tuple(ids), vt, kt)
        if key not in self.lookup_keys:
            arms = []
            for t in tables:
                one = self._lookup_one(t, vt, kt, where)
                arms.append("%d => %s(k)" % (self.table_id(t), one))
            arms.append("_ => None")
            self.lookup_fns[name] = (
                "#[inline(always)]\npub fn %s(t: TblId, k: %s) -> Option<%s> {\n    match t { %s }\n}"
                % (name, self.rust_type(kt), self.rust_type(vt), ", ".join(arms))
            )
            self.lookup_keys.add(key)
        return name

    def _lookup_one(self, table: Any, vt: T, kt: T, where: str) -> str:
        tid = self.table_id(table)
        name = "tbl_%d_get_%s" % (tid, _type_tag(vt))
        if name in self.lookup_fns:
            return name
        arms = []
        for key, value in table.items():
            kcode = self._pattern(key, kt, where)
            if kcode is None:
                continue
            if isinstance(value, (dict, frozenset, pytypes.MappingProxyType)):
                vcode = "%d" % self.table_id(value)
            else:
                vcode = self.const_value(value, vt, where)
            arms.append("%s => Some(%s)" % (kcode, vcode))
        arms.append("_ => None")
        self.lookup_fns[name] = (
            "#[inline(always)]\npub fn %s(k: %s) -> Option<%s> {\n    match k {\n        %s\n    }\n}"
            % (name, self.rust_type(kt), self.rust_type(vt), ",\n        ".join(arms))
        )
        return name

    def _pattern(self, key: Any, kt: T, where: str) -> str | None:
        if isinstance(key, bool):
            if kt == BOOL:
                return "true" if key else "false"
            return lit_int(int(key))
        if isinstance(key, int):
            if kt not in (INT, BOOL):
                return None
            return lit_int(key)
        if isinstance(key, str):
            if kt != STR:
                return None
            return self.syms.ident(key)
        if isinstance(key, tuple):
            if kt.kind != "tup" or len(kt.args) != len(key):
                return None
            parts = [
                self._pattern(k, t, where) for k, t in zip(key, kt.args, strict=True)
            ]
            if any(p is None for p in parts):
                return None
            return "(%s)" % ", ".join(parts)  # type: ignore[arg-type]
        raise TranspileError("%s: table key %r" % (where, key))

    def contains_fn(self, table: Any, kt: T, where: str) -> str:
        tid = self.table_id(table)
        name = "tbl_%d_contains_%s" % (tid, _type_tag(kt))
        if name not in self.lookup_fns:
            keys = (
                list(table.keys())
                if isinstance(table, (dict, pytypes.MappingProxyType))
                else sorted(table, key=repr)
            )
            pats = [
                p for p in (self._pattern(k, kt, where) for k in keys) if p is not None
            ]
            body = "matches!(k, %s)" % " | ".join(pats) if pats else "false"
            self.lookup_fns[name] = (
                "#[inline(always)]\npub fn %s(k: %s) -> bool {\n    %s\n}"
                % (name, self.rust_type(kt), body)
            )
        return name

    def sym_concat_fn(self, suffix: str) -> str:
        self.syms.intern(suffix)
        name = "sym_concat_%d" % self.syms.intern(suffix)
        self.sym_fns[name] = ("concat", suffix)
        return name

    def sym_affix_fn(self, which: str, affixes: tuple) -> str:
        key = (which, tuple(affixes))
        if key not in self.affix_ids:
            self.affix_ids[key] = len(self.affix_ids)
        name = "sym_%s_%d" % (which, self.affix_ids[key])
        self.sym_fns[name] = (which, tuple(affixes))
        return name

    def sym_map_fn(self, which: str) -> str:
        name = "sym_%s" % which
        self.sym_fns[name] = (which, None)
        return name

    def record_defaults(self, cls_obj: Any) -> dict[str, Any]:
        import dataclasses

        if dataclasses.is_dataclass(cls_obj):
            out = {}
            for f in dataclasses.fields(cls_obj):
                if f.default is not dataclasses.MISSING:
                    out[f.name] = f.default
            return out
        return dict(getattr(cls_obj, "_field_defaults", {}))

    def sym_fns_rust(self) -> str:
        # Materialize sym helpers over every interned string.
        out = []
        for name, (which, arg) in sorted(self.sym_fns.items()):
            names = list(self.syms.names)
            if which == "concat":
                arms = []
                for i, s in enumerate(names):
                    if i == 1:
                        continue
                    target = s + arg
                    if target in self.syms.ids:
                        arms.append(
                            "%s => %s" % (self.syms.idents[i], self.syms.ident(target))
                        )
                arms.append("_ => SYM_DYN")
                out.append(
                    "#[inline(always)]\npub fn %s(a: Sym) -> Sym {\n    match a { %s }\n}"
                    % (name, ", ".join(arms))
                )
            elif which in ("startswith", "endswith"):
                hits = [
                    self.syms.idents[i]
                    for i, s in enumerate(names)
                    if i != 1 and getattr(s, which)(arg)
                ]
                body = "matches!(a, %s)" % " | ".join(hits) if hits else "false"
                out.append(
                    "#[inline(always)]\npub fn %s(a: Sym) -> bool {\n    %s\n}"
                    % (name, body)
                )
            elif which in ("lower", "upper"):
                arms = []
                for i, s in enumerate(names):
                    if i == 1:
                        continue
                    t = getattr(s, which)()
                    if t != s and t in self.syms.ids:
                        arms.append(
                            "%s => %s" % (self.syms.idents[i], self.syms.ident(t))
                        )
                arms.append("_ => a")
                out.append(
                    "#[inline(always)]\npub fn %s(a: Sym) -> Sym {\n    match a { %s }\n}"
                    % (name, ", ".join(arms))
                )
        return "\n\n".join(out)

    def module_const(self, fullname: str, t: T, where: str) -> E:
        """A reference to module-level constant FULLNAME."""
        value = self.core.live_value(fullname)
        if isinstance(value, (dict, frozenset, set, pytypes.MappingProxyType)):
            if t == SPECVIEW:
                return E(self.const_value(value, t, where), t, const=value)
            return E("%d" % self.table_id(value), TABLE, const=value)
        if isinstance(value, pytypes.FunctionType):
            name = self.core.live_funcs[id(value)]
            return E(
                self.fn_const(name),
                FN,
                const=value,
                pts=frozenset({infer.Obj("fn", name)}),
            )
        return E(self.const_value(value, t, where), t, const=value)


def _namedtuple_fields(info: mn.TypeInfo) -> list[str]:
    tt = info.tuple_type
    assert tt is not None
    # mypy stores the field names in the NamedTuple's own 'namedtuple' plugin
    # data; fall back to the class body's annotated names in order.
    names = []
    for stmt in info.defn.defs.body:
        if (
            isinstance(stmt, mn.AssignmentStmt)
            and isinstance(stmt.lvalues[0], mn.NameExpr)
            and (stmt.type is not None or getattr(stmt, "new_syntax", False))
        ):
            names.append(stmt.lvalues[0].name)
    if len(names) != len(tt.items):
        raise TranspileError("cannot read the fields of %s" % info.fullname)
    return names


def _dataclass_fields(info: mn.TypeInfo) -> list[str]:
    names = []
    for stmt in info.defn.defs.body:
        if isinstance(stmt, mn.AssignmentStmt) and isinstance(
            stmt.lvalues[0], mn.NameExpr
        ):
            names.append(stmt.lvalues[0].name)
    return names


RUST_KEYWORDS = set(
    [
        "as",
        "break",
        "const",
        "continue",
        "crate",
        "else",
        "enum",
        "extern",
        "false",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "match",
        "mod",
        "move",
        "mut",
        "pub",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "type",
        "unsafe",
        "use",
        "where",
        "while",
        "async",
        "await",
        "dyn",
        "abstract",
        "become",
        "box",
        "do",
        "final",
        "macro",
        "override",
        "priv",
        "typeof",
        "unsized",
        "virtual",
        "yield",
        "try",
        "gen",
        "s",
    ]
)


def rust_ident(name: str) -> str:
    if name in RUST_KEYWORDS:
        return name + "_"
    return name


# ---------------------------------------------------------------------------
# Function translation
# ---------------------------------------------------------------------------

STACKS = {
    "loops": REC("Loop"),
    "call_stack": INT,
    "status_stack": TUP(VAL, VAL, VAL),
}


def STK(name: str) -> T:
    return T("stk", (name,))


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
            from sharc_core.values import Const

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
                "approx_recips",
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
                    fn = (
                        "rf_put"
                        if vcode.strip() == "V::UNK" or reg in PARTIAL_REGS
                        else "rf_set"
                    )
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


_IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_DECL_RE = re.compile(r"^\s*let mut (\w+): .* = Default::default\(\);$")
_ASSIGN_RE = re.compile(r"^\s*(?:let )?(\w+) = (.*);$")
# A call in an expression (anything but the constructors below) may have
# an effect: such an assignment stays even when its target is dead.
_CALL_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_:]*)\s*\(")
_PURE_CALLS = {
    "Some",
    "V::c",
    "Spec::V",
    "Spec::M",
    "RegView",
    "SpecView",
    "Trap",
    "stk_len_loops",
    "stk_len_call_stack",
    "stk_len_status_stack",
}


def _pure_rhs(rhs: str) -> bool:
    if "?" in rhs or "return" in rhs or "{" in rhs:
        return False
    for m in _CALL_RE.finditer(rhs):
        name = m.group(1)
        if name in _PURE_CALLS or re.fullmatch(r"U\d+::A\d+", name):
            continue
        return False
    return True


_SIG_RE = re.compile(r"^pub fn __VARIANT__\((.*)\) -> (.*) \{$")
_FWD_RE = re.compile(
    r"^\s*(?:return Ok\()?(crate::generated::image::spec_\d+::\w+)\((.*)\)\?(\))?;$"
)


def _only_sets_pc(text: str) -> str | None:
    """The statement a parameterless unit variant TEXT amounts to, when all
    it does is set the block's PC to a constant."""
    lines = [ln.strip() for ln in text.split("\n") if ln.strip() not in ("{", "}", "")]
    if len(lines) != 4 or not lines[0].startswith("#["):
        return None
    if not re.match(
        r"^pub fn __VARIANT__\(s: &mut St, rf: &mut Rf\) -> R<\(\)> \{$", lines[1]
    ):
        return None
    m = re.match(r"^(?:\{ )?rf\.pc = (\(?-?\d+i128\)?);(?: \};)?$", lines[2])
    if m is None or lines[3] != "return Ok(());":
        return None
    return "{ rf.pc = %s; }" % m.group(1)


def _forwards_to(text: str) -> str | None:
    """The variant TEXT calls, when all it does is call one with its own
    parameters in order and return that result (or unit)."""
    lines = [ln for ln in text.split("\n") if ln.strip() not in ("{", "}", "")]
    if len(lines) < 3 or not lines[0].startswith("#["):
        return None
    sig = _SIG_RE.match(lines[1])
    if sig is None:
        return None
    params = [p.strip() for p in sig.group(1).split(",") if p.strip()]
    names = []
    for p in params:
        name = p.split(":", 1)[0].replace("mut ", "").strip()
        names.append(name)
    body = lines[2:-1] if lines[-1].strip() == "}" else lines[2:]
    if len(body) == 1:
        m = _FWD_RE.match(body[0])
        if (
            m is None
            or not body[0].strip().startswith("return Ok(")
            or m.group(3) is None
        ):
            return None
    elif len(body) == 2 and body[1].strip() == "return Ok(());":
        m = _FWD_RE.match(body[0])
        if m is None or body[0].strip().startswith("return") or m.group(3):
            return None
        if sig.group(2).strip() != "R<()>":
            return None
    else:
        return None
    args = [a.strip() for a in m.group(2).split(",")]
    if args != names:
        return None
    return m.group(1)


def _prune(lines: list[str]) -> list[str]:
    """Partial-evaluation clean-up of one variant's text: drop assignments
    to locals nothing reads (their values were folded where used) when the
    assigned expression has no effect, their declarations, and empty
    blocks. It only removes code; rustc would drop the same, but the
    generated source is much smaller."""
    header = lines[:2]
    body = lines[2:]
    while True:
        text = "\n".join(body)
        counts: dict[str, int] = {}
        for name in _IDENT_RE.findall(text):
            counts[name] = counts.get(name, 0) + 1
        dead_lines = set()
        dead_names = set()
        defs: dict[str, list[int]] = {}
        for i, line in enumerate(body):
            m = _DECL_RE.match(line)
            if m:
                defs.setdefault(m.group(1), []).append(i)
                continue
            m = _ASSIGN_RE.match(line)
            if m and _pure_rhs(m.group(2)):
                defs.setdefault(m.group(1), []).append(i)
        for name, idxs in defs.items():
            # Every occurrence is one of these definitions: nothing reads it.
            own = 0
            for i in idxs:
                own += len(
                    re.findall(r"\b%s\b" % re.escape(name), body[i].split("=", 1)[0])
                )
            if counts.get(name, 0) == own:
                dead_names.add(name)
                dead_lines.update(idxs)
        for i, line in enumerate(body):
            m = re.match(r"^\s*let _ = (.*);$", line)
            if m and _pure_rhs(m.group(1).replace("{", "").replace("}", "")):
                dead_lines.add(i)
        new = [line for i, line in enumerate(body) if i not in dead_lines]
        # Empty blocks.
        out: list[str] = []
        for line in new:
            st = line.strip()
            if st == "}" and out:
                prev = out[-1].strip()
                if prev == "{":
                    out.pop()
                    continue
                if prev == "} else {":
                    out[-1] = out[-1].replace("} else {", "}")
                    continue
            out.append(line)
        if out == body:
            break
        body = out
    return header + body


# What makes a variant body impure: state or register-file access, a trap
# or `?`, a boundary call. Calls of other variants are checked one by one.
_EFFECT_RE = re.compile(
    r"(?<![A-Za-z0-9_])(?:s|rf)(?![A-Za-z0-9_])|\?|Err\(|bnd::|stk_"
)
_VARIANT_CALL_RE = re.compile(r"crate::generated::image::spec_\d+::\w+")
# Runtime calls that write the undo log (rt.rs St::log).
_LOGS_RE = re.compile(r"stk_(?:push|pop|set)_|s_set_special|bnd::_dm_write\(")
_USES_S = re.compile(r"(?<![A-Za-z0-9_:.])s(?![A-Za-z0-9_])")
_USES_SRF = re.compile(r"(?<![A-Za-z0-9_:.])(?:s|rf)(?![A-Za-z0-9_])")


def _type_of_value(value: Any) -> T:
    """The native type of a static Python value."""
    if isinstance(value, bool):
        return BOOL
    if isinstance(value, int):
        return INT
    if isinstance(value, float):
        return FLOAT
    if isinstance(value, str):
        return STR
    if value is None:
        return NONE
    cls = type(value).__name__
    if cls in ("Const", "Unknown", "PartialConst"):
        return VAL
    if cls == "MR":
        return MR
    for full, rec in RECORDS.items():
        if full.endswith("." + cls):
            return REC(rec)
    if cls == "Instruction":
        return INSN
    if isinstance(value, tuple):
        return TUP(*[_type_of_value(v) for v in value])
    if isinstance(value, pytypes.FunctionType):
        return FN
    return ANY


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


def _same_static(a: Any, b: Any) -> bool:
    if a is b:
        return True
    try:
        return type(a) is type(b) and bool(a == b)
    except Exception:  # noqa: BLE001
        return False


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


def _contains(t: T, x: T) -> bool:
    if t == x:
        return True
    return any(isinstance(a, T) and _contains(a, x) for a in t.args)


def _has_any(t: T) -> bool:
    if t == ANY:
        return True
    return any(isinstance(a, T) and _has_any(a) for a in t.args)


# ---------------------------------------------------------------------------
# Translator: generated tables and output
# ---------------------------------------------------------------------------


def _stable_key(table: Any) -> str:
    """A deterministic sort key for a constant table."""
    if isinstance(table, (dict, pytypes.MappingProxyType)):
        return "d%06d%s" % (len(table), repr(list(table.items())[:4]))
    return "s%06d%s" % (len(table), repr(sorted(table, key=repr)[:4]))


def _type_tag(t: T) -> str:
    return hashlib.sha1(repr(t).encode()).hexdigest()[:8]


# Functions never translated: the boundary (hand-written runtime),
# generation-time builders, and the calls TRAP_CALLS/ERASED_CALLS/DYN_CALLS
# replace.
def _skipped(fullname: str) -> bool:
    name = fullname.rpartition(".")[2]
    return (
        fullname in BOUNDARY
        or fullname.startswith("sharc_core.values.")
        or name.startswith("_build_")
        or fullname in TRAP_CALLS
        or fullname in ERASED_CALLS
        or fullname in DYN_CALLS
        or fullname
        in (
            "sharc_core.state._render",
            "sharc_core.state._json_value",
            "sharc_core.state._copy",
            "sharc_core.memory._dossier",
        )
    )


@dataclass
class Output:
    files: dict[str, str]
    report: dict


def translate(core: infer.Core, *, strict: bool = False) -> Output:
    tr = Translator(core)
    modules: dict[str, list[tuple[str, str]]] = {}  # module -> [(fullname, template)]
    failures: dict[str, str] = {}
    translated: list[str] = []
    for fullname in sorted(core.funcs):
        if _skipped(fullname):
            continue
        module = fullname.rpartition(".")[0]
        try:
            text = FnT(tr, fullname).run("{INLINE}")
            translated.append(fullname)
        except Exception as exc:  # noqa: BLE001 -- reported per function
            if strict:
                raise
            if not isinstance(exc, TranspileError):
                import traceback

                tb = traceback.extract_tb(exc.__traceback__)[-1]
                exc = TranspileError(
                    "%s: internal %s: %s (at %s:%d)"
                    % (
                        fullname,
                        type(exc).__name__,
                        exc,
                        os.path.basename(tb.filename),
                        tb.lineno or 0,
                    )
                )
            failures[fullname] = str(exc)
            sig = tr.signature(fullname)
            params = [
                "_%s: %s" % (rust_ident(p[0]), tr.rust_type(p[1]))
                for p in sig.params
                if p[0] not in sig.state_params
            ]
            text = (
                "// NOT TRANSLATED: %s\n#[{INLINE}]\npub fn %s(%s) -> R<%s> {\n    Err(%s)\n}"
                % (
                    str(exc).replace("\n", " "),
                    sig.rust_name,
                    ", ".join(["_s: &mut St"] + params),
                    tr.rust_type(sig.ret),
                    tr.trap(fullname, "not translated"),
                )
            )
        modules.setdefault(module, []).append((fullname, text))

    header = (
        "// Generated by tools/sharc_transpile.py from tools/sharc_core. Do not edit.\n"
    )
    uses = "    use crate::rt::*;\n    use crate::generated::syms::*;\n    use crate::generated::tables::*;\n"

    def core_file(inline: str) -> str:
        parts = []
        for module in sorted(modules):
            short = module.split(".")[-1] if module != infer.PACKAGE else "package"
            body = "\n\n".join(
                text.replace("{INLINE}", inline) for _f, text in modules[module]
            )
            body = "\n".join(("    " + ln) if ln else ln for ln in body.split("\n"))
            parts.append("pub mod %s {\n%s\n%s\n}" % (short, uses, body))
        return "\n\n".join(parts) + "\n"

    core_i = core_file("inline(always)")
    core_g = core_file("inline")

    tables = render_tables(tr)

    report = {
        "functions_translated": len(translated),
        "functions_not_translated": failures,
        "trap_sites": len(tr.trap_sites),
        "syms": len(tr.syms.names),
        "unions": len(tr.unions),
        "fn_ids": len(tr.fn_names),
        "annotated": {k: v for k, v in core.annotated.items()},
        "core_sha256": core_hash(),
        "generator_version": GENERATOR_VERSION,
    }
    files = {
        "core_i.rs": header + core_i,
        "core_g.rs": header + core_g,
        "tables.rs": tables,
        "syms.rs": tr.syms.rust(),
    }
    out = Output(files, report)
    out.translator = tr  # type: ignore[attr-defined]
    return out


def render_tables(tr: Translator) -> str:
    """tables.rs: function ids, records, unions, lookup tables, trap sites."""
    # tables.rs
    t_lines = [
        "// Generated by tools/sharc_transpile.py. Do not edit.",
        "use crate::rt::*;",
        "use crate::generated::syms::*;",
        "",
    ]
    for i, name in enumerate(tr.fn_names):
        t_lines.append("pub const %s: FnId = %d;" % (tr.fn_const(name), i + FN_ID_BASE))
    t_lines.append("pub static FN_NAMES: [&str; %d] = [" % len(tr.fn_names))
    for name in tr.fn_names:
        t_lines.append("    %s," % json.dumps(name))
    t_lines.append("];")
    for rec in ("MultSpec", "MultifnOperands"):
        fields = tr.record_fields(rec)
        t_lines.append("#[derive(Clone, Copy, Debug, Default, PartialEq)]")
        t_lines.append("pub struct %s {" % rec)
        for fname, ft in fields:
            t_lines.append("    pub %s: %s," % (fname, tr.rust_type(ft)))
        t_lines.append("}")
    # Unions (after every other type is known).
    done = 0
    while done < len(tr.union_order):
        t = tr.union_order[done]
        done += 1
        name = tr.unions[t]
        variants = [tr.rust_type(a) for a in t.args]
        t_lines.append("#[derive(Clone, Copy, Debug, PartialEq)]")
        t_lines.append(
            "pub enum %s { %s }"
            % (name, ", ".join("A%d(%s)" % (i, v) for i, v in enumerate(variants)))
        )
        t_lines.append(
            "impl Default for %s { fn default() -> Self { %s::A0(Default::default()) } }"
            % (name, name)
        )
    t_lines.append("")
    t_lines.append(tr.sym_fns_rust())
    t_lines.append("")
    for name in sorted(tr.lookup_fns):
        t_lines.append(tr.lookup_fns[name])
    t_lines.append("")
    t_lines.append("pub const CORE_SHA256: &str = %s;" % json.dumps(core_hash()))
    t_lines.append("pub const GENERATOR_VERSION: u32 = %d;" % GENERATOR_VERSION)
    t_lines.append("pub static TRAP_SITES: [&str; %d] = [" % len(tr.trap_sites))
    for site in tr.trap_sites:
        t_lines.append("    %s," % json.dumps(site))
    t_lines.append("];")

    return "\n".join(t_lines) + "\n"


# FnIds below this are the runtime's own (values.py operations).
FN_ID_BASE = 16


# The version of the generated code's meaning: bump it when a change to this
# translator, tools/sharc_transpile_infer.py or tools/sharc_rsgen.py changes
# what generated code does, so native libraries built by an older generator
# are refused (tools/sharc_transpile_run.check_build_info). A native library
# also carries core_hash(), so a tools/sharc_core change needs no bump.
GENERATOR_VERSION = 1


def core_hash() -> str:
    h = hashlib.sha256()
    for m in infer.core_modules():
        with open(infer.module_file(infer.CORE_DIR, m), "rb") as fh:
            h.update(fh.read())
    return h.hexdigest()


def write(out: Output, directory: str) -> None:
    os.makedirs(directory, exist_ok=True)
    for name, text in out.files.items():
        with open(os.path.join(directory, name), "w") as fh:
            fh.write(text)
    with open(os.path.join(directory, "transpile-report.json"), "w") as fh:
        json.dump(out.report, fh, indent=1, sort_keys=True)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--out", default=os.path.join(ROOT, "out", "native", "gen", "tx"))
    p.add_argument(
        "--work", default=os.path.join(ROOT, "out", "native", "transpile-work")
    )
    p.add_argument(
        "--strict",
        action="store_true",
        help="stop at the first construct outside the subset",
    )
    args = p.parse_args(argv)
    core = infer.load(args.work)
    try:
        out = translate(core, strict=args.strict)
    except TranspileError as exc:
        print("sharc_transpile: %s" % exc, file=sys.stderr)
        return 1
    write(out, args.out)
    rep = out.report
    print(
        "translated %d functions, %d not translated, %d trap sites -> %s"
        % (
            rep["functions_translated"],
            len(rep["functions_not_translated"]),
            rep["trap_sites"],
            args.out,
        )
    )
    for _name, why in sorted(rep["functions_not_translated"].items()):
        print("  NOT TRANSLATED %s" % why)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
