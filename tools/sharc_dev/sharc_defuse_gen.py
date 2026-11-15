"""Generate the register def/use table of dksharc (packages/dksharc/src/defuse_table.rs).

    uv run python -m tools.sharc_dev.sharc_defuse_gen            # write the table and the golden vectors
    uv run python -m tools.sharc_dev.sharc_defuse_gen --check    # fail when the checked-in files are stale

The instruction semantics stay in tools/sharc_dev/sharc_core, which this tool only reads (it never edits it, so CORE_FINGERPRINT does
not change; --check prints it). The table has three parts:

1. Compute fields (ALU, multiplier, shifter, multifunction, short compute, MR data moves, shift immediate). Each (kind, opcode) is run
   through sharc_core's `_compute` / `_shift_immediate` with distinct register values. Writes are the destinations the core returns,
   reads are the registers whose value changes the result or flags (checked by perturbing each one over several operand sets). Core
   reads of system registers (MODE1, ASTATX, the MR accumulators) are taken as traced: they may over-approximate. An opcode the core does
   not model is marked `modelled: false` and reported as uncertain, never guessed.
2. Universal registers: what a write to each of the 128 codes changes (PX also writes PX1 and PX2), the long-word pair mates and the SIMD
   companions, read from the core's own tables.
3. Per instruction form, a small effect program over the decoded fields (which fields name which registers). The form layout is
   written here; every program is checked against the core: over many field assignments the registers the core changes must equal the
   registers the program writes (the program may read more than the core traces, never less). A form the core cannot run is listed as
   `unverified` and a form it does not model as `uncertain`.

The golden vectors (tests/fixtures/defuse-golden.json) are encoded instructions with the effects this tool computes; the Rust crate's
test decodes each one and must report the same sets.
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

from tools.emulator.digi_env import core_fingerprint
from tools.sharc_dev.sharc_core import compute as C
from tools.sharc_dev.sharc_core import state as S
from tools.sharc_dev.sharc_core.compute_shift import _shift_immediate
from tools.sharc_dev.sharc_core.encoding import (
    UREG_CODES,
    UREG_NAMES,
    _split_compute_fields,
)
from tools.sharc_dev.sharc_core.forms import FORMS, _execute
from tools.sharc_dev.sharc_core.state import State, _mr_from_signed
from tools.sharc_dev.sharc_core.values import Const
from tools.sharc_dev.sharc_disasm import Instruction, disassemble

ROOT = Path(__file__).resolve().parents[2]
OUT_RS = ROOT / "packages/dksharc/src/defuse_table.rs"
OUT_GOLDEN = ROOT / "packages/dksharc/tests/fixtures/defuse-golden.json"
DECODE_TABLE = ROOT / "tools/sharc_dev/sharcspec/decode_table.json"

NUREG = 128  # universal register codes 0..127; special registers follow
SPECIAL_NAMES = ["MRF", "MRB", "MSF", "MSB", "BFFWRP", "BFF_HI", "BFF_LO"]
SPECIAL_CODE = {n: NUREG + i for i, n in enumerate(SPECIAL_NAMES)}
ASTATX = UREG_CODES["ASTATX"]
STKYX = UREG_CODES["STKYX"]
MODE1 = UREG_CODES["MODE1"]

# Compute roles: the register fields of a compute word. Bit k of the role mask in the table.
ROLES = ["rn", "rx", "ry", "rs", "rm", "ra", "rxm", "rym", "rxa", "rya"]
ROLE_BIT = {r: 1 << i for i, r in enumerate(ROLES)}

STICKY_OPS = (
    C._MULT_MOS_OPERATIONS
    | C._MULT_FLOAT_STICKY_OPERATIONS
    | C._ALU_FLOAT_STICKY_OPERATIONS
    | C._ALU_FIXED_STICKY_OPERATIONS
)


# --- tracing containers ---------------------------------------------------------------------------------------------------------------


class Traced(dict):
    """A dict that records which keys were read."""

    def __init__(self, data):
        super().__init__(data)
        self.reads = set()

    def get(self, key, default=None):
        self.reads.add(key)
        return super().get(key, default)

    def __getitem__(self, key):
        self.reads.add(key)
        return super().__getitem__(key)


def _values(kind: int, rng: random.Random) -> dict:
    """A register file: R/F codes by operand set `kind`, system registers fixed."""
    v = {}
    for c in range(NUREG):
        v[c] = Const(0x1000 + c * 0x111)
    for c in range(16):
        if kind == 0:
            v[c] = Const(rng.getrandbits(32))
        elif kind == 1:
            v[c] = Const(0x7FFFFFFF)
        elif kind == 2:
            v[c] = Const(0x80000000)
        else:
            v[c] = Const(0x3F800000 if c % 2 else 0x40000000)
    v[MODE1] = Const(0)
    v[ASTATX] = Const(0)
    v[STKYX] = Const(0)
    return v


def _special(rng: random.Random) -> dict:
    sp = {}
    for n in ("MRF", "MRB", "MSF", "MSB"):
        sp[n] = _mr_from_signed(rng.getrandbits(79))
    for n in ("BFFWRP", "BFF_HI", "BFF_LO"):
        sp[n] = Const(rng.getrandbits(32))
    return sp


# Operations the core applies as a flag update only, with no register write (compute._apply_compute).
STATUS_ONLY = frozenset({"compare", "bit-test", "float-compare"})

PERTURB = [0, 1, 0xFFFFFFFF, 0x7FFFFFFF, 0x80000000, 0x3F800000, 0xC0000000, 0x12345678]


def _norm_dest(dest):
    regs, specials = set(), set()
    for d in dest if isinstance(dest, tuple) else (dest,):
        if isinstance(d, str):
            specials.add(d)
        else:
            regs.add(d)
    return regs, specials


class Probe:
    """One compute-like function `fn(values, special)` -> core result tuple, probed for its register effects."""

    def __init__(self, fn, roles: dict[str, int]):
        self.fn = fn
        self.roles = roles  # role -> register number

    def run(self):
        """-> dict(modelled, writes roles, reads roles, ...) or None when the core does not model it."""
        rng = random.Random(7)
        vals = Traced(_values(0, rng))
        sp = Traced(_special(rng))
        try:
            res = self.fn(vals, sp)
        except (ValueError, KeyError):
            return None
        if res is None:
            return None
        dest, _value, op, flags = res
        regs, specials = _norm_dest(dest)
        if op in STATUS_ONLY:
            regs = (
                set()
            )  # the core applies a compare as a flag update only (_apply_compute)
        role_regs = set(self.roles.values())
        by_reg: dict[int, list[str]] = {}
        for role, reg in self.roles.items():
            by_reg.setdefault(reg, []).append(role)
        unexplained = [r for r in regs if r not in role_regs]
        # Roles written; a register shared by two roles cannot happen (the probe uses distinct registers).
        w_roles = {by_reg[r][0] for r in regs if r in by_reg}
        # Reads: role registers by perturbation, system registers and specials as traced.
        sys_reads = sorted(
            c
            for c in vals.reads
            if isinstance(c, int) and c not in role_regs and c >= 16
        )
        data_reads_traced = sorted(
            c
            for c in vals.reads
            if isinstance(c, int) and c < 16 and c not in role_regs
        )
        spec_reads = sorted(s for s in sp.reads if isinstance(s, str))
        dep: set[int] = set()
        cands = [c for c in vals.reads if isinstance(c, int) and c in role_regs]
        for kind in range(4):
            r2 = random.Random(100 + kind)
            base_v = _values(kind, r2)
            base_s = _special(r2)
            try:
                base = self.fn(dict(base_v), dict(base_s))
            except (ValueError, KeyError):
                continue
            for c in cands:
                if c in dep:
                    continue
                for pv in PERTURB:
                    v2 = dict(base_v)
                    v2[c] = Const(pv)
                    try:
                        other = self.fn(v2, dict(base_s))
                    except (ValueError, KeyError):
                        continue
                    if other != base:
                        dep.add(c)
                        break
        r_roles = set()
        for c in dep:
            r_roles.add(by_reg[c][0])
        astat = 0
        fl = flags
        if fl is not None:
            astat = (fl.define_mask | fl.forget_mask) & 0xFFFFFFFF
        sys_writes = []
        if astat:
            sys_writes.append(ASTATX)
        if op in STICKY_OPS:
            sys_writes.append(STKYX)
        sys_writes += [SPECIAL_CODE[s] for s in sorted(specials) if s in SPECIAL_CODE]
        unknown_specials = sorted(s for s in specials if s not in SPECIAL_CODE)
        return {
            "w": sorted(w_roles, key=ROLES.index),
            "r": sorted(r_roles, key=ROLES.index),
            "sys_w": sorted(set(sys_writes)),
            "sys_r": sorted(
                set(
                    sys_reads
                    + [SPECIAL_CODE[s] for s in spec_reads if s in SPECIAL_CODE]
                )
            ),
            "astat": astat,
            "unexplained": unexplained,
            "extra_data_reads": data_reads_traced,
            "unknown_specials": unknown_specials,
            "op": op,
        }


# Distinct register sets for the probe, chosen per entry so that no role collides.
def _pick(sets, taken):
    for s in sets:
        if not (set(s) & taken):
            return s
    raise AssertionError("no free register set")


def _split(field: int) -> dict:
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def probe_single(cu: int, opcode: int):
    rs = opcode & 0xF if cu == 0 and (opcode >> 4) in (0x7, 0xF) else None
    sets = [(1, 2, 3), (5, 6, 7), (9, 10, 11), (13, 14, 15)]
    rn, rx, ry = _pick(sets, {rs} if rs is not None else set())
    roles = {"rn": rn, "rx": rx, "ry": ry}
    if rs is not None:
        roles["rs"] = rs
    field = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
    f = _split(field)
    return Probe(lambda v, s: C._compute(f, False, v, s), roles)


def probe_multifn(category: int):
    """The four input quads are picked by 2-bit fields (F0-3, F4-7, F8-11, F12-15); the probe fills every field with q so that the
    registers of the quads, Rm, Ra and Rs (the low nibble of a dual add/subtract's category) are all distinct."""
    rs = category & 0xF if category >> 4 == 3 else None
    for q in range(4):
        quad = {q, 4 + q, 8 + q, 12 + q}
        if rs is None or rs not in quad:
            break
    pool = [r for r in range(16) if r not in quad and r != rs]
    rm, ra = pool[0], pool[1]
    roles = {"rm": rm, "ra": ra, "rxm": q, "rym": 4 + q, "rxa": 8 + q, "rya": 12 + q}
    if rs is not None:
        roles["rs"] = rs
    field = (
        (1 << 22)
        | (category << 16)
        | (rm << 12)
        | (ra << 8)
        | (q << 6)
        | (q << 4)
        | (q << 2)
        | q
    )
    f = _split(field)
    return Probe(lambda v, s: C._compute(f, False, v, s), roles)


def probe_short(opcode: int):
    rn, rx = 1, 2
    f = {"compute": (opcode << 8) | (rn << 4) | rx}
    return Probe(lambda v, s: C._compute(f, True, v, s), {"rn": rn, "rx": rx})


def probe_mrmove(direction: int, opcode: int):
    rn = 1
    field = (0b100000 << 17) | (direction << 16) | (opcode << 12) | (rn << 8)
    f = _split(field)
    return Probe(lambda v, s: C._compute(f, False, v, s), {"rn": rn})


def probe_shiftimm(opcode: int, data8: int, dataex: int = 0):
    rn, rx = 1, 2
    field = (opcode << 16) | (data8 << 8) | (rn << 4) | rx
    f = {
        "shiftimm[22:16]": field >> 16,
        "shiftimm[15:0]": field & 0xFFFF,
        "dataex[3:0]": dataex,
    }
    return Probe(lambda v, s: _shift_immediate(f, v, s), {"rn": rn, "rx": rx})


def _merge(results):
    """Union the probes of one entry (shift immediate over several data bytes)."""
    got = [r for r in results if r is not None]
    if not got:
        return None
    out = dict(got[0])
    for r in got[1:]:
        for k in (
            "w",
            "r",
            "sys_w",
            "sys_r",
            "extra_data_reads",
            "unexplained",
            "unknown_specials",
        ):
            out[k] = sorted(set(out[k]) | set(r[k]), key=lambda x: (str(type(x)), x))
        out["astat"] |= r["astat"]
    out["w"] = sorted(out["w"], key=ROLES.index)
    out["r"] = sorted(out["r"], key=ROLES.index)
    return out


def compute_tables():
    single, multifn, short, mrmove, shiftimm = {}, {}, {}, {}, {}
    for cu in range(4):
        for op in range(256):
            single[(cu, op)] = probe_single(cu, op).run()
    for cat in range(64):
        multifn[cat] = probe_multifn(cat).run()
    for op in range(16):
        short[op] = probe_short(op).run()
    for d in range(2):
        for op in range(16):
            mrmove[(d, op)] = probe_mrmove(d, op).run()
    for op in range(64):
        shiftimm[op] = _merge(
            [
                probe_shiftimm(op, d, x).run()
                for d in (0x00, 0x01, 0x05, 0x3F, 0x80, 0xFF)
                for x in (0, 5)
            ]
        )
    return single, multifn, short, mrmove, shiftimm


# --- universal registers --------------------------------------------------------------------------------------------------------------


def ureg_tables():
    """-> (extra writes per code, lw mate per code, lw load fills mate, simd companion per code)."""
    extra = {}
    for code in range(NUREG):
        st = State(
            pc_sw=0x1000,
            uregs={c: Const(0x1000 + c * 7) for c in range(NUREG)},
            record_events=False,
        )
        base = dict(st.uregs)
        ins = Instruction(
            0,
            6,
            "17a",
            {"ureg[6:0]": code, "data[31:16]": 0x1234, "data[15:0]": 0x5678},
            0,
        )
        out = _execute(st, ins)[0]
        changed = {c for c in range(NUREG) if out.uregs.get(c) != base.get(c)}
        extra[code] = sorted(changed - {code})
    mate = {c: S._lw_pair_mate(c) for c in range(NUREG)}
    loads = {c: S._lw_pair_loads_mate(c) for c in range(NUREG)}
    comp = {c: S._cureg_code(c) for c in range(NUREG)}
    return extra, mate, loads, comp


# --- form effect programs -------------------------------------------------------------------------------------------------------------
#
# Loc: ("fixed", code) | ("r", field) | ("ureg", field) | ("ureg_src",) | ("dag", file, field, bank) | ("dag_dst", "is", "idis", bank)
#      | ("sys_sreg",)      file is "I", "M", "L" or "B"; bank is 0, 8 or "g" (the field named g picks 8 when set)
# Eff: ("rd", loc) | ("wr", loc) | ("move", dirfield, loc, lwfield|None) | ("compute", kind, stem) | ("if", field, [values], [effs])
#      | ("bitop",) | ("astat", mask)


def dag_i(f, bank="g"):
    return ("dag", "I", f, bank)


def dag_m(f, bank="g"):
    return ("dag", "M", f, bank)


def dag_l(f, bank="g"):
    return ("dag", "L", f, bank)


def dag_b(f, bank="g"):
    return ("dag", "B", f, bank)


def fixed(name):
    return ("fixed", UREG_CODES[name])


COMPUTE = ("compute", "full", "compute")


def _post_modify(f, bank="g", mfield=None):
    """A post-modify of I[f] by M[m]: I, M, L and B are read, I is written. Pre-modify (u = 0) reads I and M and writes nothing."""
    return [
        ("rd", dag_i(f, bank)),
        ("rd", dag_m(mfield, bank)) if mfield else None,
        (
            "if",
            "u",
            [1],
            [("rd", dag_l(f, bank)), ("rd", dag_b(f, bank)), ("wr", dag_i(f, bank))],
        ),
    ]


def _clean(effs):
    return [e for e in effs if e is not None]


def _mem_ureg(lw: bool):
    return ("move", "d", ("ureg", "ureg"), "l" if lw else None)


def _dag_dst(file, bank="g"):
    return ("dag_dst", file, "is", "idis", bank)


def _dag_src(file, bank="g"):
    return ("dag_dst_src", file, "is", bank)


def _modify_dag(file="I"):
    """Type 7 and 19: `Id = f(Is)` with the destination `is ^ idis` in the same file and DAG (the circular state of Is is read)."""
    effs = [("rd", _dag_src(file))]
    if file == "I":
        effs += [("rd", _dag_src("L")), ("rd", _dag_src("B"))]
    effs.append(("wr", _dag_dst(file)))
    return effs


_STACK = ("stack",)
_CALL = ("if", "b", [1], [_STACK])
_LOOP_WRITES = [("wr", fixed("LCNTR")), ("wr", fixed("CURLCNTR")), _STACK]

FORM_PROGRAMS = {
    "1a": _clean(
        [
            ("move", "dmd", ("r", "dmdreg"), None),
            ("move", "pmd", ("r", "pmdreg"), None),
            ("rd", dag_i("dmi", 0)),
            ("rd", dag_m("dmm", 0)),
            ("rd", dag_l("dmi", 0)),
            ("rd", dag_b("dmi", 0)),
            ("wr", dag_i("dmi", 0)),
            ("rd", dag_i("pmi", 8)),
            ("rd", dag_m("pmm", 8)),
            ("rd", dag_l("pmi", 8)),
            ("rd", dag_b("pmi", 8)),
            ("wr", dag_i("pmi", 8)),
            COMPUTE,
        ]
    ),
    "2a": [COMPUTE],
    "2a_short": [COMPUTE],
    "2b": [COMPUTE],
    "2c": [("compute", "short", "compute")],
    "3a": _clean([_mem_ureg(True), *_post_modify("i", "g", "m"), COMPUTE]),
    "3b": _clean([_mem_ureg(False), *_post_modify("i", "g", "m")]),
    "3d": _clean([_mem_ureg(False), *_post_modify("i", "g", "m")]),
    "3c": _clean(
        [
            ("move", "d", ("r", "dreg"), None),
            ("rd", dag_i("dmi", 0)),
            ("rd", dag_m("dmm", 0)),
            ("rd", dag_l("dmi", 0)),
            ("rd", dag_b("dmi", 0)),
            ("wr", dag_i("dmi", 0)),
        ]
    ),
    "4a": _clean(
        [
            ("move", "d", ("r", "dreg"), None),
            ("rd", dag_i("i")),
            (
                "if",
                "u",
                [1],
                [("rd", dag_l("i")), ("rd", dag_b("i")), ("wr", dag_i("i"))],
            ),
            COMPUTE,
        ]
    ),
    "4b": _clean(
        [
            ("move", "d", ("r", "dreg"), None),
            ("rd", dag_i("i")),
            (
                "if",
                "u",
                [1],
                [("rd", dag_l("i")), ("rd", dag_b("i")), ("wr", dag_i("i"))],
            ),
        ]
    ),
    "4d": _clean(
        [
            ("move", "d", ("r", "dreg"), None),
            ("rd", dag_i("i")),
            (
                "if",
                "u",
                [1],
                [("rd", dag_l("i")), ("rd", dag_b("i")), ("wr", dag_i("i"))],
            ),
        ]
    ),
    "5a_move": [("rd", ("ureg_src",)), ("wr", ("ureg", "dstureg")), COMPUTE],
    "5b_move": [("rd", ("ureg_src",)), ("wr", ("ureg", "dstureg"))],
    "5a_swap": [
        ("rd", ("r", "cdreg")),
        ("rd", ("r", "dreg")),
        ("wr", ("r", "cdreg")),
        ("wr", ("r", "dreg")),
        COMPUTE,
    ],
    "5b_swap": [
        ("rd", ("r", "cdreg")),
        ("rd", ("r", "dreg")),
        ("wr", ("r", "cdreg")),
        ("wr", ("r", "dreg")),
    ],
    "6a_mem": _clean(
        [
            ("move", "d", ("r", "dreg"), None),
            ("rd", dag_i("i")),
            ("rd", dag_m("m")),
            ("rd", dag_l("i")),
            ("rd", dag_b("i")),
            ("wr", dag_i("i")),
            ("compute", "shiftimm", "shiftimm"),
        ]
    ),
    "6a_nomem": [("compute", "shiftimm", "shiftimm")],
    "6b_shiftimm": [("compute", "shiftimm", "shiftimm")],
    "7a": [*_modify_dag(), ("rd", dag_m("m")), COMPUTE],
    "7b": [*_modify_dag(), ("rd", dag_m("m"))],
    # Type 7d (ACONV): the I or the B register class by breg; B2W and W2B only move the value through a shift.
    "7d": [
        ("if", "breg", [0], _modify_dag("I")),
        ("if", "breg", [1], _modify_dag("B")),
    ],
    # Jumps and calls: no general register effect; a call pushes the PC stack, JUMP and CALL through DAG2 read I and M.
    "8a_abs": [_CALL],
    "8a_rel": [_CALL],
    "9a_abs": [("rd", dag_i("pmi", 8)), ("rd", dag_m("pmm", 8)), _CALL, COMPUTE],
    "9a_rel": [_CALL, COMPUTE],
    "9b_abs": [("rd", dag_i("pmi", 8)), ("rd", dag_m("pmm", 8)), _CALL],
    "9b_rel": [_CALL],
    "11a": [_STACK, COMPUTE],
    "11c": [_STACK],
    # DO loops push the PC and loop stacks and set the loop counter.
    "12a_imm": list(_LOOP_WRITES),
    "12a_ureg": [("rd", ("ureg", "ureg")), *_LOOP_WRITES],
    "13a": [_STACK],
    "14a": [_mem_ureg(True)],
    "14d": [("move", "d", ("r", "dreg"), None)],
    "15a": _clean([_mem_ureg(True), ("rd", dag_i("i"))]),
    "15b": _clean([_mem_ureg(True), ("rd", dag_i("i"))]),
    "16a": [
        ("rd", dag_i("i")),
        ("rd", dag_m("m")),
        ("rd", dag_l("i")),
        ("rd", dag_b("i")),
        ("wr", dag_i("i")),
    ],
    "16b": [
        ("rd", dag_i("i")),
        ("rd", dag_m("m")),
        ("rd", dag_l("i")),
        ("rd", dag_b("i")),
        ("wr", dag_i("i")),
    ],
    "17a": [("wr", ("ureg", "ureg"))],
    "17b": [("wr", ("ureg", "ureg"))],
    "18a": [("bitop",)],
    "19a": _modify_dag(),
    "19a_scaled": _modify_dag(),
    "19a_bitrev": _modify_dag(),
    # PUSH and POP of the status, loop and PC stacks. PUSH STS saves ASTATX, ASTATY and MODE1 and clears the MMASK bits of MODE1;
    # POP STS restores all three.
    "20a": [
        (
            "if",
            "spu",
            [1],
            [
                ("rd", fixed("ASTATX")),
                ("rd", fixed("ASTATY")),
                ("rd", fixed("MODE1")),
                ("rd", fixed("MMASK")),
                ("wr", fixed("MODE1")),
                ("wr", fixed("STKYX")),
                ("sts", "push"),
                _STACK,
            ],
        ),
        (
            "if",
            "spo",
            [1],
            [
                ("wr", fixed("ASTATX")),
                ("wr", fixed("ASTATY")),
                ("wr", fixed("MODE1")),
                ("wr", fixed("STKYX")),
                ("sts", "pop"),
                _STACK,
            ],
        ),
        ("if", "lpu", [1], [_STACK]),
        ("if", "lpo", [1], [_STACK]),
        ("if", "ppu", [1], [_STACK]),
        ("if", "ppo", [1], [_STACK]),
    ],
    "21a": [],
    "21c": [],
    "22a": [],
    "22c": [],
    # CJUMP: R2 = I6, I6 = I7, then a call. RFRAME: I7 = I6, I6 = DM(0, I6).
    "25a_direct": [
        ("rd", fixed("I6")),
        ("rd", fixed("I7")),
        ("wr", fixed("R2")),
        ("wr", fixed("I6")),
        _STACK,
    ],
    "25a_pcrel": [
        ("rd", fixed("I6")),
        ("rd", fixed("I7")),
        ("wr", fixed("R2")),
        ("wr", fixed("I6")),
        _STACK,
    ],
    "25c_rframe": [("rd", fixed("I6")), ("wr", fixed("I7")), ("wr", fixed("I6"))],
}
# Forms with no register effect program because the core or the ISA table leaves their meaning open: reported as uncertain.
UNCERTAIN_FORMS = [
    "10a_abs",
    "10a_rel",
    "8p_undoc48",
    "21p_undoc16",
    "22p_undoc48",
    "26a",
    "26p_undoc48",
    "25a_rframe",
]
# Forms whose core handler is missing: verified through a sibling that has the same register fields.
ALIAS_FOR_VERIFY = {
    "1b": "1a",
    "5b_move": "5a_move",
    "5b_swap": "5a_swap",
    "6a_nomem": "6b_shiftimm",
    "9a_rel": "9a_abs",
    "4b": "4a",
    "4d": "4a",
    "3d": "3b",
}
FORM_PROGRAMS["1b"] = _clean([e for e in FORM_PROGRAMS["1a"] if e != COMPUTE])

# Bit operation (Type 18a): sreg selects the system register (USTAT1 + sreg); bop 0 set, 1 clear, 2 toggle read and write it, 4 and 5 are tests
# (read; BTF of ASTATX). The mask of a MODE1 operation is the `data` field.
BIT_TEST = (4, 5)
BIT_MODIFY = (0, 1, 2)


# --- evaluation of a program (the Python twin of dksharc's defuse.rs; used for the verification and the golden vectors) ------------------


def field(f, stem):
    """The field `stem`, or its pieces `stem[hi:lo]` joined (the same rule as dksharc's Fields::get)."""
    if stem in f:
        return f[stem]
    found, v = False, 0
    for k, val in f.items():
        if k.startswith(stem + "["):
            lo = int(k[len(stem) + 1 : -1].split(":")[-1])
            found = True
            v |= val << lo
    return v if found else None


DAG_BASE = {"I": 16, "M": 32, "L": 48, "B": 64}


def bank_of(f, bank):
    if bank == "g":
        return 8 if field(f, "g") else 0
    return bank


def loc_codes(f, loc):
    """The universal register codes a location names (before pair and extra effects)."""
    kind = loc[0]
    if kind == "fixed":
        return [loc[1]]
    if kind == "r":
        return [field(f, loc[1])]
    if kind == "ureg":
        return [field(f, loc[1])]
    if kind == "ureg_src":
        return [(field(f, "srcureghigh") << 2) | (field(f, "srcureglow") & 3)]
    if kind == "dag":
        return [DAG_BASE[loc[1]] + bank_of(f, loc[3]) + field(f, loc[2])]
    if kind == "dag_dst":
        src, dis = field(f, loc[2]), field(f, loc[3])
        return [DAG_BASE[loc[1]] + bank_of(f, loc[4]) + (src ^ dis)]
    if kind == "dag_dst_src":
        return [DAG_BASE[loc[1]] + bank_of(f, loc[3]) + field(f, loc[2])]
    raise AssertionError(loc)


class Table:
    """The tables the evaluation needs (the generator fills it; the checks use it)."""

    def __init__(
        self, single, multifn, short, mrmove, shiftimm, extra, mate, loads, comp
    ):
        self.single, self.multifn, self.short, self.mrmove, self.shiftimm = (
            single,
            multifn,
            short,
            mrmove,
            shiftimm,
        )
        self.extra, self.mate, self.loads, self.comp = extra, mate, loads, comp


def role_regs(kind, word):
    if kind == "short":
        return {"rn": (word >> 4) & 15, "rx": word & 15}
    if kind == "shiftimm":
        return {"rn": (word >> 4) & 15, "rx": word & 15}
    d = {
        "rn": (word >> 8) & 15,
        "rx": (word >> 4) & 15,
        "ry": word & 15,
        "rs": (word >> 12) & 15,
    }
    return d


def compute_entry(t: Table, kind: str, word: int):
    """The table entry the dispatch of sharc_core's `_compute` picks for a compute word, with the role -> register map."""
    if kind == "short":
        return t.short[(word >> 8) & 15], {"rn": (word >> 4) & 15, "rx": word & 15}
    if kind == "shiftimm":
        return t.shiftimm[(word >> 16) & 0x3F], {
            "rn": (word >> 4) & 15,
            "rx": word & 15,
        }
    if word >> 17 == 0b100000:
        return t.mrmove[((word >> 16) & 1, (word >> 12) & 15)], {"rn": (word >> 8) & 15}
    cu, opcode = (word >> 20) & 3, (word >> 12) & 0xFF
    regs = {"rn": (word >> 8) & 15, "rx": (word >> 4) & 15, "ry": word & 15}
    if cu == 1 and opcode in (0xB4, 0xB0):
        return t.single[(1, opcode)], regs
    if (word >> 22) & 1:
        cat = (word >> 16) & 0x3F
        regs = {
            "rm": (word >> 12) & 15,
            "ra": (word >> 8) & 15,
            "rxm": (word >> 6) & 3,
            "rym": 4 + ((word >> 4) & 3),
            "rxa": 8 + ((word >> 2) & 3),
            "rya": 12 + (word & 3),
            "rs": cat & 15,
        }
        return t.multifn[cat], regs
    if cu == 0 and (opcode >> 4) in (0x7, 0xF):
        regs["rs"] = opcode & 15
    return t.single[(cu, opcode)], regs


def evaluate(t: Table, form: str, f: dict):
    """-> dict(reads, writes, modelled, astat, mode1) for a decoded form with fields f (condition not considered)."""
    reads, writes = set(), set()
    out = {"astat": 0, "mode1": None, "uncertain": [], "stack": False}

    def rd(c):
        reads.add(c)

    def wr(c):
        writes.add(c)

    def ureg_read(code, lw=False):
        rd(code)
        if lw and t.mate[code] is not None:
            rd(t.mate[code])

    def ureg_write(code, lw=False):
        wr(code)
        for e in t.extra[code]:
            wr(e)
        if lw and t.mate[code] is not None and t.loads[code]:
            wr(t.mate[code])

    def run(effs):
        for e in effs:
            k = e[0]
            if k == "rd":
                if e[1][0] == "ureg":
                    ureg_read(field(f, e[1][1]))
                else:
                    for c in loc_codes(f, e[1]):
                        rd(c)
            elif k == "wr":
                if e[1][0] == "ureg":
                    ureg_write(field(f, e[1][1]))
                else:
                    for c in loc_codes(f, e[1]):
                        wr(c)
            elif k == "move":
                store = field(f, e[1]) == 1
                lw = e[3] is not None and field(f, e[3]) == 1
                loc = e[2]
                code = loc_codes(f, loc)[0]
                if store:
                    ureg_read(code, lw) if loc[0] == "ureg" else rd(code)
                else:
                    ureg_write(code, lw) if loc[0] == "ureg" else wr(code)
            elif k == "compute":
                word = field(f, e[2]) or 0
                if word == 0 and e[1] == "full":
                    continue  # a zero compute field is the compute NOP (the core returns None)
                ent, regs = compute_entry(t, e[1], word)
                if ent is None:
                    out["uncertain"].append(
                        "compute %s %#x not modelled" % (e[1], word)
                    )
                    continue
                for role in ent["r"]:
                    rd(regs[role])
                for role in ent["w"]:
                    wr(regs[role])
                for c in ent["sys_r"]:
                    rd(c)
                for c in ent["sys_w"]:
                    wr(c)
                out["astat"] |= ent["astat"]
            elif k == "if":
                if field(f, e[1]) in e[2]:
                    run(e[3])
            elif k == "stack":
                out["stack"] = True
            elif k == "sts":
                out["mode1"] = "push_sts" if e[1] == "push" else "pop_sts"
            elif k == "bitop":
                sreg, bop = field(f, "sreg"), field(f, "bop")
                code = UREG_CODES["USTAT1"] + sreg
                if bop in BIT_TEST:
                    rd(code)
                    wr(ASTATX)
                    out["astat"] |= 1 << 18
                elif bop in BIT_MODIFY:
                    rd(code)
                    ureg_write(code)
                    if code == MODE1:
                        out["mode1"] = ("set", "clear", "toggle")[bop]
                else:
                    out["uncertain"].append("bit operation %d" % bop)
            else:
                raise AssertionError(e)

    run(FORM_PROGRAMS[form])
    out["reads"], out["writes"] = reads, writes
    return out


# --- verification against the core ----------------------------------------------------------------------------------------------------


def _form_specs():
    return {f["name"]: f for f in json.loads(DECODE_TABLE.read_text())["forms"]}


def core_name(table_name: str) -> str:
    """The sharc_core form key of a decode_table name (`Type3a` -> `3a`, `Type5a (swap)` -> `5a_swap`)."""
    n = table_name.replace("Type", "")
    if " (" in n:
        base, kind = n.split(" (")
        kind = kind.rstrip(")")
        return (
            base
            + "_"
            + {"move": "move", "swap": "swap", "mem": "mem", "nomem": "nomem"}[kind]
        )
    return n.lower()


def random_fields(spec, rng, nocompute=True, pin=None):
    f = {}
    for fd in spec["fields"]:
        w = fd["hi"] - fd["lo"] + 1
        f[fd["label"]] = rng.randrange(1 << w)
    for k in list(f):
        if k.startswith("cond"):
            f[k] = 0x1F
        if nocompute and ("compute" in k or "shiftimm" in k):
            f[k] = 0
    if pin:
        f.update(pin)
    return f


def core_effects(name: str, fields: dict, width: int):
    """Run the core on a fresh state; -> (writes, stopped) with the universal register codes the instruction changed."""
    st = State(
        pc_sw=0x1000,
        uregs={c: Const(0x1000 + c * 7) for c in range(NUREG)},
        record_events=False,
    )
    base = dict(st.uregs)
    traced = Traced(st.uregs)
    st.uregs = traced
    fl = dict(fields)
    # An alias sibling may need fields the form does not have: a compute of zero (the NOP) and the always condition.
    if name in ("1a", "4a", "5a_move", "5a_swap", "9a_abs") and not any(
        k.startswith("compute") for k in fl
    ):
        fl.update({"compute[22:16]": 0, "compute[15:0]": 0})
    if name == "6b_shiftimm" and not any(k.startswith("shiftimm") for k in fl):
        fl.update({"shiftimm[22:16]": 0, "shiftimm[15:0]": 0, "dataex[3:0]": 0})
    if (
        "cond[4:0]" not in fl
        and "cond" not in fl
        and name in ("4a", "5a_move", "5a_swap", "9a_abs", "6b_shiftimm")
    ):
        fl["cond[4:0]"] = 0x1F
    if "compute" in fl:
        fl = _split_compute_fields(fl)
    out = _execute(st, Instruction(0, width // 8, name, fl, 0))
    reads = set(traced.reads)
    res = []
    for o in out:
        w = {c for c in range(NUREG) if o.uregs.get(c) != base.get(c)}
        res.append((w, o.stopped))
    return res, reads


def verify(t: Table, rng, samples=400):
    """Compare the programs with the core. -> (report lines, failures)."""
    specs = _form_specs()
    lines, fails = [], []
    for tname, spec in specs.items():
        name = core_name(tname)
        prog = name if name in FORM_PROGRAMS else None
        run_as = name
        note = ""
        if name not in FORMS:
            if name in ALIAS_FOR_VERIFY:
                run_as = ALIAS_FOR_VERIFY[name]
                note = "verified through " + run_as
            else:
                run_as = None
        if name in UNCERTAIN_FORMS:
            lines.append(
                (
                    name,
                    "uncertain",
                    "no program: the core or the ISA table does not define it",
                )
            )
            continue
        if prog is None:
            lines.append((name, "MISSING", "no program"))
            fails.append(name)
            continue
        if run_as is None or run_as not in FORMS:
            lines.append((name, "unverified", "the core has no handler"))
            continue
        ran = stopped = bad = 0
        extra_reads = 0
        extra_names = set()
        first = None
        for _ in range(samples):
            fl = random_fields(spec, rng)
            if name == "18a":
                fl["bop[2:0]"] = rng.choice([0, 1, 2, 4, 5])
            res, creads = core_effects(run_as, fl, spec["width"])
            ev = evaluate(t, name, fl)
            for cw, stop in res:
                if stop:
                    stopped += 1
                    continue
                ran += 1
                # A register the core changed that the program does not write is a failure. A program write the core did not show is
                # one only for DAG and system registers: a data register may be written with an equal value, and the stack and loop
                # registers and the flag registers are not plain values in the core.
                # PX, PX1 and PX2 are one 48-bit register; the stack and loop registers and the sticky register are synchronised by the
                # core as a side effect of any instruction (the program reports stack use as a flag, not as register writes).
                invisible = {
                    UREG_CODES[n]
                    for n in (
                        "PCSTK",
                        "PCSTKP",
                        "CURLCNTR",
                        "LADDR",
                        "STKYX",
                        "MODE1STK",
                    )
                }
                px = {
                    UREG_CODES["PX1"]: UREG_CODES["PX"],
                    UREG_CODES["PX2"]: UREG_CODES["PX"],
                }
                cw1 = {px.get(c, c) for c in cw} - invisible
                pw1 = {px.get(c, c) for c in ev["writes"]} - invisible
                miss = cw1 - pw1
                over = {
                    c
                    for c in pw1 - cw1
                    if c >= 32 and c != UREG_CODES["LCNTR"] and c != ASTATX
                }
                if name == "20a":
                    over = set()  # POP STS restores values from the status stack, which is empty in this probe
                if (
                    name.startswith("5")
                    and name.endswith("_move")
                    and loc_codes(fl, ("ureg_src",)) == [field(fl, "dstureg")]
                ):
                    over = set()  # a move of a register onto itself changes no value
                if miss or over:
                    bad += 1
                    if first is None:
                        first = (
                            fl,
                            "core",
                            sorted(cw),
                            "program",
                            sorted(ev["writes"]),
                        )
            rd_extra = {c for c in creads if c not in ev["reads"] and c < 96}
            extra_reads += bool(rd_extra)
            extra_names |= {UREG_NAMES[c] for c in rd_extra}
        status = "ok" if not bad else "MISMATCH"
        if bad:
            fails.append(name)
        lines.append(
            (
                name,
                status,
                "%s ran %d stopped %d traced reads outside the program in %d %s%s"
                % (
                    note,
                    ran,
                    stopped,
                    extra_reads,
                    sorted(extra_names),
                    "" if not bad else " first mismatch %s" % (first,),
                ),
            )
        )
    return lines, fails


# --- golden vectors -------------------------------------------------------------------------------------------------------------------


def encode(spec, f):
    """The parcels of the form with field values f (in the order the decoder reads them)."""
    frame = int(spec["value"], 16)
    for fd in spec["fields"]:
        frame |= (f[fd["label"]] & ((1 << (fd["hi"] - fd["lo"] + 1)) - 1)) << fd["lo"]
    w = spec["width"]
    words = [(frame >> 32) & 0xFFFF, (frame >> 16) & 0xFFFF, frame & 0xFFFF]
    return words[: w // 16]


def golden(t: Table, rng, per_form=12, compute_n=300):
    specs = _form_specs()
    cases = []
    for tname, spec in specs.items():
        name = core_name(tname)
        if name not in FORM_PROGRAMS:
            continue
        got = 0
        tries = 0
        while got < per_form and tries < per_form * 40:
            tries += 1
            nocompute = rng.random() < 0.5
            fl = random_fields(spec, rng, nocompute=nocompute)
            if rng.random() < 0.25:
                for k in list(fl):
                    if k.startswith("cond"):
                        fl[k] = rng.randrange(32)
            if name == "18a":
                fl["bop[2:0]"] = rng.choice([0, 1, 2, 4, 5])
            words = encode(spec, fl)
            data = b"".join(w.to_bytes(2, "little") for w in words)
            try:
                ins = next(disassemble(data, count=1))
            except StopIteration:
                continue
            if (
                ins.type_name != name
                or ins.kind != "confident"
                or ins.fields != {k: v for k, v in fl.items()}
                and False
            ):
                continue
            ev = evaluate(t, name, ins.fields)
            cond = field(ins.fields, "cond")
            cases.append(
                {
                    "form": name,
                    "parcels": words,
                    "conditional": cond is not None and cond != 0x1F,
                    "reads": sorted(ev["reads"]),
                    "writes": sorted(ev["writes"]),
                    "astat": ev["astat"],
                    "mode1": ev["mode1"],
                    "stack": ev["stack"],
                    "uncertain": ev["uncertain"],
                }
            )
            got += 1
    return cases


# --- Rust emission --------------------------------------------------------------------------------------------------------------------


def rs_codes(codes):
    return "&[" + ", ".join(str(c) for c in codes) + "]"


def rs_bank(b):
    return {0: "Bank::Zero", 8: "Bank::Eight", "g": "Bank::ByG"}[b]


def rs_loc(loc):
    k = loc[0]
    if k == "fixed":
        return "Loc::Fixed(%d)" % loc[1]
    if k == "r":
        return 'Loc::R("%s")' % loc[1]
    if k == "ureg":
        return 'Loc::Ureg("%s")' % loc[1]
    if k == "ureg_src":
        return "Loc::UregSrc"
    if k == "dag":
        return 'Loc::Dag(DagFile::%s, "%s", %s)' % (loc[1], loc[2], rs_bank(loc[3]))
    if k == "dag_dst":
        return 'Loc::DagDst(DagFile::%s, "%s", "%s", %s)' % (
            loc[1],
            loc[2],
            loc[3],
            rs_bank(loc[4]),
        )
    if k == "dag_dst_src":
        return 'Loc::DagSrc(DagFile::%s, "%s", %s)' % (loc[1], loc[2], rs_bank(loc[3]))
    raise AssertionError(loc)


def rs_eff(e):
    k = e[0]
    if k == "rd":
        return "Eff::Rd(%s)" % rs_loc(e[1])
    if k == "wr":
        return "Eff::Wr(%s)" % rs_loc(e[1])
    if k == "move":
        lw = 'Some("%s")' % e[3] if e[3] else "None"
        return 'Eff::Move { dir: "%s", reg: %s, lw: %s }' % (e[1], rs_loc(e[2]), lw)
    if k == "compute":
        return 'Eff::Compute(ComputeKind::%s, "%s")' % (
            {"full": "Full", "short": "Short", "shiftimm": "ShiftImm"}[e[1]],
            e[2],
        )
    if k == "if":
        return 'Eff::If { field: "%s", any: &[%s], then: &[%s] }' % (
            e[1],
            ", ".join(str(v) for v in e[2]),
            ", ".join(rs_eff(x) for x in e[3]),
        )
    if k == "bitop":
        return "Eff::BitOp"
    if k == "stack":
        return "Eff::Stack"
    if k == "sts":
        return "Eff::Sts(%s)" % ("true" if e[1] == "push" else "false")
    raise AssertionError(e)


def rs_comp_entry(ent):
    if ent is None:
        return "CompEff::UNMODELLED"
    rmask = sum(ROLE_BIT[r] for r in ent["r"])
    wmask = sum(ROLE_BIT[r] for r in ent["w"])
    return (
        "CompEff { modelled: true, reads: %#x, writes: %#x, sys_reads: %s, sys_writes: %s, astat: %#x }"
        % (
            rmask,
            wmask,
            rs_codes(ent["sys_r"]),
            rs_codes(ent["sys_w"]),
            ent["astat"],
        )
    )


def emit(t: Table, fingerprint: str):
    o = []
    w = o.append
    w(
        "// @generated by tools/sharc_dev/sharc_defuse_gen.py from tools/sharc_dev/sharc_core (core fingerprint %s). Do not edit."
        % fingerprint[:16]
    )
    w(
        "// Regenerate: uv run python -m tools.sharc_dev.sharc_defuse_gen; `--check` fails when this file is stale."
    )
    w("")
    w("use crate::defuse::{Bank, CompEff, ComputeKind, DagFile, Eff, Loc};")
    w("")
    w(
        "/// Universal register names by code (0..127); the special registers follow at `NUREG`."
    )
    w("pub const NUREG: usize = %d;" % NUREG)
    w(
        "pub const UREG_NAMES: [&str; %d] = [%s];"
        % (NUREG, ", ".join('"%s"' % n for n in UREG_NAMES))
    )
    w(
        "pub const SPECIAL_NAMES: [&str; %d] = [%s];"
        % (len(SPECIAL_NAMES), ", ".join('"%s"' % n for n in SPECIAL_NAMES))
    )
    w("pub const ASTATX: u8 = %d;" % ASTATX)
    w("pub const MODE1: u8 = %d;" % MODE1)
    w(
        "pub const ROLES: [&str; %d] = [%s];"
        % (len(ROLES), ", ".join('"%s"' % r for r in ROLES))
    )
    w("")
    w(
        "/// Registers a write to each universal register code also changes (PX writes PX1 and PX2)."
    )
    w(
        "pub const UREG_EXTRA_WRITES: [&[u8]; %d] = [%s];"
        % (NUREG, ", ".join(rs_codes(t.extra[c]) for c in range(NUREG)))
    )
    w("/// The register a long word access pairs with (255: none).")
    w(
        "pub const LW_MATE: [u8; %d] = [%s];"
        % (
            NUREG,
            ", ".join(
                str(t.mate[c] if t.mate[c] is not None else 255) for c in range(NUREG)
            ),
        )
    )
    w("/// Whether a long word load into the register also fills its mate.")
    w(
        "pub const LW_LOADS_MATE: [bool; %d] = [%s];"
        % (NUREG, ", ".join("true" if t.loads[c] else "false" for c in range(NUREG)))
    )
    w(
        "/// The SIMD companion of a universal register (255: none): R and S, USTAT, ASTAT, STKY and PX pairs."
    )
    w(
        "pub const SIMD_COMPANION: [u8; %d] = [%s];"
        % (
            NUREG,
            ", ".join(
                str(t.comp[c] if t.comp[c] is not None else 255) for c in range(NUREG)
            ),
        )
    )
    w("")
    w("/// Single function compute, index `cu * 256 + opcode`.")
    w("pub static COMPUTE_SINGLE: [CompEff; 1024] = [")
    for cu in range(4):
        for op in range(256):
            w("    %s," % rs_comp_entry(t.single[(cu, op)]))
    w("];")
    w("/// Multifunction compute by category (bits 21:16).")
    w("pub static COMPUTE_MULTIFN: [CompEff; 64] = [")
    for cat in range(64):
        w("    %s," % rs_comp_entry(t.multifn[cat]))
    w("];")
    w("/// Short compute (Type 2c) by opcode.")
    w("pub static COMPUTE_SHORT: [CompEff; 16] = [")
    for op in range(16):
        w("    %s," % rs_comp_entry(t.short[op]))
    w("];")
    w("/// MR data moves, index `direction * 16 + opcode`.")
    w("pub static COMPUTE_MRMOVE: [CompEff; 32] = [")
    for d in range(2):
        for op in range(16):
            w("    %s," % rs_comp_entry(t.mrmove[(d, op)]))
    w("];")
    w("/// Shift immediate by opcode (bits 21:16).")
    w("pub static COMPUTE_SHIFTIMM: [CompEff; 64] = [")
    for op in range(64):
        w("    %s," % rs_comp_entry(t.shiftimm[op]))
    w("];")
    w("")
    w("/// Effect program of each form, by the form name of the ISA table.")
    w("pub static FORM_PROGRAMS: &[(&str, &[Eff])] = &[")
    for name in sorted(FORM_PROGRAMS):
        w(
            '    ("%s", &[%s]),'
            % (name, ", ".join(rs_eff(e) for e in FORM_PROGRAMS[name]))
        )
    w("];")
    w(
        "/// Forms the core or the ISA table leaves open: reported as uncertain, never guessed."
    )
    w(
        "pub const UNCERTAIN_FORMS: &[&str] = &[%s];"
        % ", ".join('"%s"' % n for n in UNCERTAIN_FORMS)
    )
    w("")
    return "\n".join(o)


def build_table():
    single, multifn, short, mrmove, shiftimm = compute_tables()
    extra, mate, loads, comp = ureg_tables()
    return Table(single, multifn, short, mrmove, shiftimm, extra, mate, loads, comp)


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--check",
        action="store_true",
        help="fail when the checked-in table or golden file differs",
    )
    ap.add_argument(
        "--report", action="store_true", help="print the verification report only"
    )
    args = ap.parse_args(argv)
    fp = core_fingerprint()
    t = build_table()
    lines, fails = verify(t, random.Random(2024))
    for name, status, detail in lines:
        if args.report or status not in ("ok",):
            print("%-14s %-10s %s" % (name, status, detail))
    if fails:
        print("verification failed for: %s" % ", ".join(fails), file=sys.stderr)
        return 1
    if args.report:
        return 0
    rs = emit(t, fp)
    cases = golden(t, random.Random(99))
    gold = '{"core_fingerprint": "%s", "cases": [\n%s\n]}\n' % (
        fp[:16],
        ",\n".join(json.dumps(c, sort_keys=True, separators=(",", ":")) for c in cases),
    )
    if args.check:
        stale = [
            p
            for p, s in ((OUT_RS, rs), (OUT_GOLDEN, gold))
            if not p.exists() or p.read_text() != s
        ]
        if stale:
            print(
                "stale: " + ", ".join(str(p.relative_to(ROOT)) for p in stale),
                file=sys.stderr,
            )
            return 1
        print("up to date (core fingerprint %s)" % fp[:16])
        return 0
    OUT_GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    OUT_RS.write_text(rs)
    OUT_GOLDEN.write_text(gold)
    print(
        "wrote %s and %s (core fingerprint %s)"
        % (OUT_RS.relative_to(ROOT), OUT_GOLDEN.relative_to(ROOT), fp[:16])
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
