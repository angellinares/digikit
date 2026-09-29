#!/usr/bin/env python3
"""Generate the ColdFire decoder (native/coldfire/src/decode_gen.rs) from the
instruction table tools/cfisa/coldfire.json.

    uv run python tools/cfisa/gen.py            # check the table, write the Rust
    uv run python tools/cfisa/gen.py --check    # exit 1 if the Rust file is stale
    uv run python tools/cfisa/gen.py --stats    # table size and opword coverage

The table holds only encodings taken from the public manuals (page citations in
each entry); nothing here comes from a firmware image, so the generated file
can be committed.

Before writing, the table is checked for ambiguity: every one of the 65536
opwords is matched against every form (fixed bits, field restrictions, EA mode
sets, size maps, deny rules). Where more than one form accepts an opword, all
of them must have a fixed extension word, and every extension word value is
matched too: at most one form may accept each (opword, extension) pair.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
TABLE = ROOT / "tools" / "cfisa" / "coldfire.json"
OUT = ROOT / "native" / "coldfire" / "src" / "decode_gen.rs"

MODES = [
    "dn",
    "an",
    "ind",
    "post",
    "pre",
    "disp",
    "idx",
    "absw",
    "absl",
    "pcdisp",
    "pcidx",
    "imm",
]
MAX_WORDS = 3  # CFPRM p.35: an instruction is 16, 32 or 48 bits
SIZES = {"": "None", "b": "B", "w": "W", "l": "L", "s": "S", "d": "D"}
FIXED_OPS = {
    "sr": "Sr",
    "ccr": "Ccr",
    "usp": "Usp",
    "macsr": "Macsr",
    "mask": "Mask",
    "accext01": "AccExt01",
    "accext23": "AccExt23",
    "fpcr": "Fpcr",
    "fpsr": "Fpsr",
    "fpiar": "Fpiar",
}


def ea_mode(mode: int, reg: int) -> str | None:
    """The EA mode name for a 3-bit mode and register field (CFPRM p.44 Table 2-3)."""
    if mode < 7:
        return MODES[mode]
    return {0: "absw", 1: "absl", 2: "pcdisp", 3: "pcidx", 4: "imm"}.get(reg)


@dataclass
class Field:
    """A field made of (word, bit, inverted) parts, msb first."""

    name: str
    parts: list[tuple[int, int, bool]]

    def value(self, words: list[int]) -> int:
        v = 0
        for w, b, inv in self.parts:
            v = (v << 1) | (((words[w] >> b) & 1) ^ int(inv))
        return v

    def words(self) -> set[int]:
        return {w for w, _, _ in self.parts}


@dataclass
class Form:
    id: str
    mn: str
    unit: str
    page: str
    priv: bool
    flow: str
    masks: list[int]
    values: list[int]
    fields: dict[str, Field]
    size: Any  # a size letter, "bdisp", or {f, map, else}
    ops: list[str]
    allow: dict[str, list[int]]
    eq: list[list[str]]
    ne: list[list[str]]
    deny: list[dict]
    eas: list[tuple[str, str, str]] = field(
        default_factory=list
    )  # (mode field, reg field, set name)


class TableError(Exception):
    pass


def parse_form(spec: dict) -> Form:
    enc = [e.replace(" ", "") for e in spec["enc"]]
    masks: list[int] = []
    values: list[int] = []
    letters: dict[str, list[tuple[int, int, bool]]] = {}
    for wi, pat in enumerate(enc):
        if len(pat) != 16:
            raise TableError("%s: pattern %r is not 16 bits" % (spec["id"], pat))
        m = v = 0
        for i, ch in enumerate(pat):
            bit = 15 - i
            if ch in "01":
                m |= 1 << bit
                v |= int(ch) << bit
            elif ch != "-":
                letters.setdefault(ch, []).append((wi, bit, False))
        masks.append(m)
        values.append(v)
    fields = {k: Field(k, p) for k, p in letters.items()}
    for name, comp in spec.get("fields", {}).items():
        parts: list[tuple[int, int, bool]] = []
        inv = False
        for ch in comp:
            if ch == "~":
                inv = True
                continue
            if ch not in letters:
                raise TableError(
                    "%s: composite %s uses unknown letter %s" % (spec["id"], name, ch)
                )
            parts.extend((w, b, i ^ inv) for w, b, i in letters[ch])
            inv = False
        fields[name] = Field(name, parts)
    f = Form(
        id=spec["id"],
        mn=spec["mn"],
        unit=spec["unit"],
        page=spec["page"],
        priv=bool(spec.get("priv")),
        flow=spec.get("flow", ""),
        masks=masks,
        values=values,
        fields=fields,
        size=spec["size"],
        ops=spec["ops"],
        allow={k: list(v) for k, v in spec.get("allow", {}).items()},
        eq=spec.get("eq", []),
        ne=spec.get("ne", []),
        deny=spec.get("deny", []),
    )
    for op in f.ops:
        kind, _, arg = op.partition(":")
        if kind == "ea":
            mr, _, setname = arg.partition(":")
            mf, rf = mr.split(",")
            f.eas.append((mf, rf, setname))
        for name in _op_fields(op):
            if name not in fields:
                raise TableError(
                    "%s: operand %s uses unknown field %s" % (f.id, op, name)
                )
    for name in list(f.allow) + [x for pair in f.eq + f.ne for x in pair]:
        if name not in fields:
            raise TableError("%s: constraint on unknown field %s" % (f.id, name))
    if isinstance(f.size, dict) and f.size["f"] not in fields:
        raise TableError("%s: size field %s unknown" % (f.id, f.size["f"]))
    return f


def _op_fields(op: str) -> list[str]:
    kind, _, arg = op.partition(":")
    if kind == "ea":
        mr = arg.partition(":")[0]
        return mr.split(",")
    if kind in FIXED_OPS or kind in ("imm", "immopt", "imm16", "imm16s"):
        return []
    return arg.split(",") if arg else []


def expand_fpu(table: dict) -> list[dict]:
    fa = table["fpu_arith"]
    out = []
    for op in fa["ops"]:
        opm = op["opmode"]
        base = {"unit": "fpu", "page": op["page"], "op": op["id"]}
        if op.get("nodst"):
            ea_ops, rr_ops = ["ea:m,r:%s" % fa["ea_set"]], ["fp:y"]
        else:
            ea_ops, rr_ops = ["ea:m,r:%s" % fa["ea_set"], "fp:x"], ["fp:y", "fp:x"]
        enc_ea = [
            fa["enc_ea"][0],
            fa["enc_ea"][1].replace(" ", "").replace("ooooooo", opm),
        ]
        enc_rr = [
            fa["enc_reg"][0],
            fa["enc_reg"][1].replace(" ", "").replace("ooooooo", opm),
        ]
        fmts = {k: v for k, v in fa["formats"].items()}
        out.append(
            dict(
                base,
                id=op["id"] + "_ea",
                mn=op["id"],
                enc=enc_ea,
                allow={"s": [int(k) for k in fmts]},
                size={"f": "s", "map": fmts},
                ops=ea_ops,
                deny=[{"ops": {"0": ["dn"]}, "sizes": ["d"]}],
            )
        )
        out.append(
            dict(
                base, id=op["id"] + "_rr", mn=op["id"], enc=enc_rr, size="d", ops=rr_ops
            )
        )
    return out


def load(path: Path = TABLE) -> tuple[dict, list[Form]]:
    table = json.loads(path.read_text())
    specs = table["forms"] + expand_fpu(table)
    forms = [parse_form(s) for s in specs]
    ids = [f.id for f in forms]
    dup = {i for i in ids if ids.count(i) > 1}
    if dup:
        raise TableError("duplicate form ids: %s" % sorted(dup))
    for f in forms:
        for _, _, s in f.eas:
            if s not in table["ea_sets"]:
                raise TableError("%s: unknown EA set %s" % (f.id, s))
    return table, forms


# ---------------------------------------------------------------------------
# matching, shared by the ambiguity check and the census statistics


def size_of(f: Form, words: list[int]) -> str | None:
    """The size letter, '' for unsized, None if the size field value is illegal."""
    s = f.size
    if isinstance(s, str):
        return "" if s == "bdisp" else s
    v = str(f.fields[s["f"]].value(words))
    if v in s["map"]:
        return s["map"][v]
    return s.get("else")


def _known(f: Form, names, nwords: int) -> bool:
    return all(max(f.fields[n].words()) < nwords for n in names)


def matches(f: Form, sets: dict, words: list[int]) -> bool:
    """Does form f accept words (a prefix: opword, or opword + fixed extension)?
    Constraints on words not yet given are skipped."""
    n = len(words)
    for i in range(min(n, len(f.masks))):
        if words[i] & f.masks[i] != f.values[i]:
            return False
    for name, vals in f.allow.items():
        if _known(f, [name], n) and f.fields[name].value(words) not in vals:
            return False
    for a, b in f.eq:
        if _known(f, [a, b], n) and f.fields[a].value(words) != f.fields[b].value(
            words
        ):
            return False
    for a, b in f.ne:
        if _known(f, [a, b], n) and f.fields[a].value(words) == f.fields[b].value(
            words
        ):
            return False
    size = None
    if isinstance(f.size, dict) and _known(f, [f.size["f"]], n):
        size = size_of(f, words)
        if size is None:
            return False
    elif isinstance(f.size, str):
        size = size_of(f, words)
    modes: list[str | None] = []
    for mf, rf, setname in f.eas:
        if not _known(f, [mf, rf], n):
            modes.append(None)
            continue
        m = ea_mode(f.fields[mf].value(words), f.fields[rf].value(words))
        if m is None or m not in sets[setname]:
            return False
        modes.append(m)
    ea_idx = [i for i, op in enumerate(f.ops) if op.startswith("ea:")]
    for rule in f.deny:
        hit = True
        for k, ms in rule["ops"].items():
            m = modes[ea_idx.index(int(k))]
            if m is None or m not in ms:
                hit = False
        if "sizes" in rule and (size is None or size not in rule["sizes"]):
            hit = False
        if hit:
            return False
    return True


def check_ambiguity(forms: list[Form], sets: dict) -> dict:
    """-> {opword: [form ids]} after checking every opword and, for ambiguous
    opwords, every extension word. Raises TableError on a real overlap."""
    by_nib: dict[int, list[Form]] = {}
    for f in forms:
        for nib in range(16):
            if (nib << 12) & f.masks[0] == f.values[0] & 0xF000:
                by_nib.setdefault(nib, []).append(f)
    accept: dict[int, list[str]] = {}
    groups: dict[tuple[str, ...], int] = {}
    for w0 in range(0x10000):
        cands = [f for f in by_nib.get(w0 >> 12, []) if matches(f, sets, [w0])]
        if not cands:
            continue
        accept[w0] = [f.id for f in cands]
        if len(cands) > 1:
            key = tuple(f.id for f in cands)
            groups.setdefault(key, w0)
    fmap = {f.id: f for f in forms}
    for key, w0 in groups.items():
        cs = [fmap[i] for i in key]
        bad = [f.id for f in cs if len(f.masks) < 2]
        if bad:
            raise TableError(
                "opword %04x: %s overlap and %s have no fixed extension word"
                % (w0, list(key), bad)
            )
        for w1 in range(0x10000):
            hit = [f.id for f in cs if matches(f, sets, [w0, w1])]
            if len(hit) > 1:
                raise TableError("opword %04x ext %04x matches %s" % (w0, w1, hit))
    return accept


# ---------------------------------------------------------------------------
# Rust emission


def camel(s: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in s.split("_"))


def rust_field(fld: Field) -> str:
    """A Rust expression for a field over the local words w0..w2."""
    parts = fld.parts
    # group runs of consecutive bits in the same word with the same inversion
    runs: list[list] = []
    for w, b, inv in parts:
        if runs and runs[-1][0] == w and runs[-1][2] == b + 1 and runs[-1][3] == inv:
            runs[-1][2] = b
        else:
            runs.append([w, b, b, inv])  # word, hi, lo, inverted
    expr, shift_total = [], 0
    for w, hi, lo, inv in reversed(runs):
        width = hi - lo + 1
        src = "w%d" % w if lo == 0 else "(w%d >> %d)" % (w, lo)
        e = "%s & 0x%x" % (src, (1 << width) - 1)
        if inv:
            e = "(%s) ^ 0x%x" % (e, (1 << width) - 1)
        if shift_total:
            e = "(%s) << %d" % (e, shift_total)
        expr.append(e)
        shift_total += width
    if len(expr) == 1:
        return expr[0]
    return " | ".join("(%s)" % e for e in reversed(expr))


def match_expr(var: str, mask: int, value: int, negate: bool = False) -> str:
    """A Rust test of `var` against fixed bits; '' when no bit is fixed."""
    if mask == 0:
        return ""
    op = "!=" if negate else "=="
    if mask == 0xFFFF:
        return "%s %s 0x%04x" % (var, op, value)
    return "%s & 0x%04x %s 0x%04x" % (var, mask, op, value)


def ea_set_mask(sets: dict, name: str) -> int:
    return sum(1 << MODES.index(m) for m in sets[name])


def emit_form(f: Form, table: dict) -> str:
    sets = table["ea_sets"]
    nfixed = len(f.masks)
    lines = []
    lines.append("/// %s (%s, %s)" % (f.id, f.unit, f.page))
    lines.append(
        "#[allow(unused_variables, unused_mut, non_snake_case, clippy::identity_op)]"
    )
    lines.append("fn f_%s(c: &mut Cur) -> Option<Insn> {" % f.id)
    lines.append("    let w0 = c.w[0] as u32;")
    for i in range(nfixed):
        if i:
            lines.append("    let w%d = c.next()? as u32;" % i)
        cond = match_expr("w%d" % i, f.masks[i], f.values[i], negate=True)
        if cond:
            lines.append("    if %s {" % cond)
            lines.append("        return None;")
            lines.append("    }")
    used = set()
    for op in f.ops:
        used.update(_op_fields(op))
    used.update(f.allow)
    for a, b in f.eq + f.ne:
        used.update((a, b))
    if isinstance(f.size, dict):
        used.add(f.size["f"])
    cond_field = f.mn.split(":")[1].rstrip("}") if "{" in f.mn else None
    if cond_field:
        used.add(cond_field)
    for name in sorted(used):
        lines.append("    let f_%s = %s;" % (name, rust_field(f.fields[name])))
    for name, vals in f.allow.items():
        cond = " || ".join("f_%s == %d" % (name, v) for v in vals)
        lines.append("    if !(%s) {" % cond)
        lines.append("        return None;")
        lines.append("    }")
    for a, b in f.eq:
        lines.append("    if f_%s != f_%s {" % (a, b))
        lines.append("        return None;")
        lines.append("    }")
    for a, b in f.ne:
        lines.append("    if f_%s == f_%s {" % (a, b))
        lines.append("        return None;")
        lines.append("    }")
    # size
    s = f.size
    if isinstance(s, str):
        if s == "bdisp":
            lines.append("    let size = Size::None;")
        else:
            lines.append("    let size = Size::%s;" % SIZES[s])
    else:
        arms = [
            "        %s => Size::%s," % (k, SIZES[v])
            for k, v in sorted(s["map"].items())
        ]
        other = "Size::%s" % SIZES[s["else"]] if "else" in s else "return None"
        lines.append("    let size = match f_%s {" % s["f"])
        lines.extend(arms)
        lines.append("        _ => %s," % other)
        lines.append("    };")
    # EA mode checks before any extension word is consumed
    ea_names = []
    for k, (mf, rf, setname) in enumerate(f.eas):
        lines.append("    let m%d = ea_mode(f_%s, f_%s)?;" % (k, mf, rf))
        lines.append(
            "    if (1u16 << m%d as u16) & 0x%03x == 0 {"
            % (k, ea_set_mask(sets, setname))
        )
        lines.append("        return None;")
        lines.append("    }")
        ea_names.append("m%d" % k)
    ea_idx = [i for i, op in enumerate(f.ops) if op.startswith("ea:")]
    for rule in f.deny:
        conds = []
        for k, ms in rule["ops"].items():
            mask = sum(1 << MODES.index(m) for m in ms)
            conds.append(
                "(1u16 << m%d as u16) & 0x%03x != 0" % (ea_idx.index(int(k)), mask)
            )
        if "sizes" in rule:
            conds.append(
                "(%s)"
                % " || ".join("size == Size::%s" % SIZES[z] for z in rule["sizes"])
            )
        lines.append("    if %s {" % " && ".join(conds))
        lines.append("        return None;")
        lines.append("    }")
    # operands in extension-word order
    lines.append("    let mut i = Insn::new(Form::%s, size);" % camel(f.id))
    k_ea = 0
    for op in f.ops:
        kind, _, arg = op.partition(":")
        if kind == "ea":
            mf, rf = arg.partition(":")[0].split(",")
            lines.append(
                "    i.push(Operand::Ea(c.ea(m%d, f_%s, size)?));" % (k_ea, rf)
            )
            k_ea += 1
        elif kind == "d":
            lines.append("    i.push(Operand::Ea(Ea::Dn(f_%s as u8)));" % arg)
        elif kind == "a":
            lines.append("    i.push(Operand::Ea(Ea::An(f_%s as u8)));" % arg)
        elif kind == "ind":
            lines.append("    i.push(Operand::Ea(Ea::Ind(f_%s as u8)));" % arg)
        elif kind == "r":
            lines.append("    i.push(Operand::Ea(Ea::rn(f_%s as u8)));" % arg)
        elif kind == "imm":
            lines.append("    i.push(Operand::Imm(c.imm(size)?));")
        elif kind == "immopt":
            lines.append("    if size != Size::None {")
            lines.append("        i.push(Operand::Imm(c.imm(size)?));")
            lines.append("    }")
        elif kind == "imm16":
            lines.append("    i.push(Operand::Imm(c.next()? as u32));")
        elif kind == "imm16s":
            lines.append("    i.push(Operand::Imm(c.next()? as i16 as i32 as u32));")
        elif kind == "quick":
            lines.append(
                "    i.push(Operand::Imm(if f_%s == 0 { 8 } else { f_%s }));"
                % (arg, arg)
            )
        elif kind == "moveq":
            lines.append(
                "    i.push(Operand::Imm(f_%s as u8 as i8 as i32 as u32));" % arg
            )
        elif kind == "mov3q":
            lines.append(
                "    i.push(Operand::Imm(if f_%s == 0 { u32::MAX } else { f_%s }));"
                % (arg, arg)
            )
        elif kind in ("bitnum", "vector"):
            lines.append("    i.push(Operand::Imm(f_%s));" % arg)
        elif kind == "bdisp":
            lines.append("    let (t, sz) = c.bdisp(f_%s)?;" % arg)
            lines.append("    i.size = sz;")
            lines.append("    i.push(Operand::Target(t));")
        elif kind == "fbdisp":
            lines.append("    i.push(Operand::Target(c.fbdisp(f_%s)?));" % arg)
        elif kind == "list":
            lines.append("    i.push(Operand::RegList(f_%s as u16));" % arg)
        elif kind == "fplist":
            lines.append("    i.push(Operand::FpList(f_%s as u8));" % arg)
        elif kind == "ctrl":
            lines.append("    i.push(Operand::Ctrl(f_%s as u16));" % arg)
        elif kind == "cache":
            lines.append("    i.push(Operand::Cache(f_%s as u8));" % arg)
        elif kind == "acc":
            lines.append("    i.push(Operand::Acc(f_%s as u8));" % arg)
        elif kind == "macr":
            r, u = arg.split(",")
            lines.append(
                "    i.push(Operand::MacReg { r: f_%s as u8, upper: f_%s != 0 });"
                % (r, u)
            )
        elif kind == "sf":
            lines.append("    i.push(Operand::Scale(f_%s as u8));" % arg)
        elif kind == "maskflag":
            lines.append("    i.push(Operand::MaskFlag(f_%s != 0));" % arg)
        elif kind == "fp":
            lines.append("    i.push(Operand::Fp(f_%s as u8));" % arg)
        elif kind in FIXED_OPS:
            lines.append("    i.push(Operand::%s);" % FIXED_OPS[kind])
        else:
            raise TableError("%s: unknown operand kind %s" % (f.id, op))
    if cond_field:
        lines.append("    i.cond = f_%s as u8;" % cond_field)
    lines.append("    i.len = c.len();")
    lines.append("    Some(i)")
    lines.append("}")
    return "\n".join(lines)


def emit(table: dict, forms: list[Form]) -> str:
    out = []
    out.append(
        "// @generated by tools/cfisa/gen.py from tools/cfisa/coldfire.json. Do not edit."
    )
    out.append(
        "// Encodings from the public ColdFire manuals (page citations per form); no firmware."
    )
    out.append("")
    out.append("use crate::decode::{ea_mode, Cur, Ea, Insn, Operand, Size};")
    out.append("")
    out.append("/// One variant per instruction form of the table.")
    out.append("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]")
    out.append("#[repr(u16)]")
    out.append("pub enum Form {")
    for f in forms:
        out.append("    %s," % camel(f.id))
    out.append("}")
    out.append("")
    out.append("pub const FORM_COUNT: usize = %d;" % len(forms))
    out.append("")
    out.append("/// Every form, in table order.")
    out.append("pub const FORMS: [Form; FORM_COUNT] = [")
    for f in forms:
        out.append("    Form::%s," % camel(f.id))
    out.append("];")
    out.append("")

    def strtab(name, vals):
        out.append("pub const %s: [&str; %d] = [" % (name, len(vals)))
        for v in vals:
            out.append("    %s," % json.dumps(v))
        out.append("];")
        out.append("")

    strtab("FORM_IDS", [f.id for f in forms])
    strtab("MNEMONICS", [f.mn.split("{")[0] for f in forms])
    strtab("UNITS", [f.unit for f in forms])
    strtab("PAGES", [f.page for f in forms])
    strtab("FLOWS", [f.flow for f in forms])
    strtab("CONDITIONS", table["conditions"])
    strtab("FP_CONDITIONS", table["fp_conditions"])
    out.append(
        "/// 0 = no condition, 1 = integer (CONDITIONS), 2 = floating point (FP_CONDITIONS)."
    )
    out.append("pub const COND_KIND: [u8; FORM_COUNT] = [")
    for f in forms:
        out.append("    %d," % (1 if "{cc:" in f.mn else 2 if "{fcc:" in f.mn else 0))
    out.append("];")
    out.append("")
    out.append("pub const PRIVILEGED: [bool; FORM_COUNT] = [")
    for f in forms:
        out.append("    %s," % ("true" if f.priv else "false"))
    out.append("];")
    out.append("")
    ctrl = {
        int(k, 16): v
        for k, v in table["control_registers"].items()
        if not k.startswith("_")
    }
    out.append("/// MOVEC control register names (CFPRM p.250, RM p.90).")
    out.append("pub fn control_register_name(rc: u16) -> Option<&'static str> {")
    out.append("    Some(match rc {")
    for k in sorted(ctrl):
        out.append("        0x%03x => %s," % (k, json.dumps(ctrl[k])))
    out.append("        _ => return None,")
    out.append("    })")
    out.append("}")
    out.append("")
    # dispatch by top nibble
    out.append(
        "/// Decode the instruction whose words are in `c`; None for an illegal encoding."
    )
    out.append("pub(crate) fn dispatch(c: &mut Cur) -> Option<Insn> {")
    out.append("    let w0 = c.w[0];")
    out.append("    match w0 >> 12 {")
    for nib in range(16):
        fs = [f for f in forms if (nib << 12) & f.masks[0] == f.values[0] & 0xF000]
        if not fs:
            continue
        # most specific first; the ambiguity check makes the order irrelevant
        # except for extension-word discrimination, which each f_ handles
        fs.sort(key=lambda f: -bin(f.masks[0]).count("1"))
        out.append("        0x%x => {" % nib)
        for f in fs:
            out.append(
                "            if %s {" % match_expr("w0", f.masks[0], f.values[0])
            )
            out.append("                c.reset();")
            out.append("                if let Some(i) = f_%s(c) {" % f.id)
            out.append("                    return Some(i);")
            out.append("                }")
            out.append("            }")
        out.append("            None")
        out.append("        }")
    out.append("        _ => None,")
    out.append("    }")
    out.append("}")
    out.append("")
    for f in forms:
        out.append(emit_form(f, table))
        out.append("")
    return "\n".join(out)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--check", action="store_true", help="exit 1 if the generated file is stale"
    )
    ap.add_argument(
        "--stats", action="store_true", help="print table size and opword coverage"
    )
    ap.add_argument(
        "--no-ambiguity",
        action="store_true",
        help="skip the 64K ambiguity check (faster)",
    )
    args = ap.parse_args(argv)
    table, forms = load()
    if not args.no_ambiguity:
        accept = check_ambiguity(forms, table["ea_sets"])
    text = emit(table, forms)
    if args.stats:
        units: dict[str, int] = {}
        for f in forms:
            units[f.unit] = units.get(f.unit, 0) + 1
        print(
            "forms: %d  %s"
            % (len(forms), " ".join("%s=%d" % kv for kv in sorted(units.items())))
        )
        if not args.no_ambiguity:
            print("legal opwords (at least one form): %d of 65536" % len(accept))
    if args.check:
        current = OUT.read_text() if OUT.exists() else ""
        if current != text:
            print(
                "stale: %s (run tools/cfisa/gen.py)" % OUT.relative_to(ROOT),
                file=sys.stderr,
            )
            return 1
        return 0
    OUT.write_text(text)
    print("wrote %s (%d forms)" % (OUT.relative_to(ROOT), len(forms)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
