"""Small, conservative delay-aware tracer for SHARC+ main-program code.

This intentionally follows exact short-word PCs, rather than discovering
functions or linearly sweeping unrelated bytes.  It is a first semantic slice:
unhandled forms stop a state instead of pretending to understand them.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import struct
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Union

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
from sharc_disasm import Instruction, decode_loaded_at, disassemble
from sharcimm import name_address
from sharcldr import SW_ALIAS_BASE, LoadedMemory, sw_to_byte

UREG_NAMES = tuple(
    [f"R{i}" for i in range(16)]
    + [f"I{i}" for i in range(16)]
    + [f"M{i}" for i in range(16)]
    + [f"L{i}" for i in range(16)]
    + [f"B{i}" for i in range(16)]
    + [f"S{i}" for i in range(16)]
    + [
        "FADDR",
        "DADDR",
        "UREG_RESERVED_62",
        "PC",
        "PCSTK",
        "PCSTKP",
        "LADDR",
        "CURLCNTR",
        "LCNTR",
        "EMUCLK",
        "EMUCLK2",
        "PX",
        "PX1",
        "PX2",
        "TPERIOD",
        "TCOUNT",
        "USTAT1",
        "USTAT2",
        "MODE1",
        "MMASK",
        "MODE2",
        "FLAGS",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
        "IRPTL",
        "IMASK",
        "IMASKP",
        "MODE1STK",
        "USTAT3",
        "USTAT4",
    ]
)
UREG_CODES = {name: code for code, name in enumerate(UREG_NAMES)}

# Public SHARC+ register tables document these reset values.  Keep this list
# deliberately bounded to core state used by startup rather than treating
# every absent UREG as zero.
CORE_UREG_RESET_VALUES = {
    name: 0
    for name in (
        "MODE1",
        "MMASK",
        "MODE1STK",
        "MODE2",
        "PCSTK",
        "PCSTKP",
        "LADDR",
        "LCNTR",
        "CURLCNTR",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
        "IRPTL",
        "IMASK",
        "IMASKP",
    )
}
CORE_MMR_RESET_VALUES = {
    0x30024: 0,  # CMMR_SYSCTL
    0x31400: 0,  # SHBTB_CFG
    0x31401: 0,  # SHBTB_LOCK_START
    0x31402: 0,  # SHBTB_LOCK_END
    0x3E000: 0,  # SHL1C_CFG
    0x3E002: 0,  # SHL1C_CFG2
}

# ADSP-2156x L1 block 3 aliases.  The normal-word window is the one used by
# the reset path's PM(...)=PX table read; the loader records the same physical
# storage through the short-word/system-byte view.
L1_BLOCK3_NW_BASE = 0x000E0000
L1_BLOCK3_NW_LIMIT = 0x000E8000
L1_BLOCK3_SW_BASE = 0x001C0000


@dataclass(frozen=True)
class Const:
    value: int

    def __post_init__(self):
        object.__setattr__(self, "value", self.value & 0xFFFFFFFF)


@dataclass(frozen=True)
class Affine:
    """A canonical 32-bit affine expression, constant plus named terms."""

    constant: int
    terms: tuple[tuple[str, int], ...]

    def __post_init__(self):
        coefficients: dict[str, int] = {}
        for name, coefficient in self.terms:
            if not _SYMBOL_RE.fullmatch(name):
                raise ValueError("invalid symbol name: " + repr(name))
            coefficients[name] = (coefficients.get(name, 0) + coefficient) & 0xFFFFFFFF
        object.__setattr__(self, "constant", self.constant & 0xFFFFFFFF)
        object.__setattr__(
            self,
            "terms",
            tuple(
                sorted(
                    (name, coefficient)
                    for name, coefficient in coefficients.items()
                    if coefficient
                )
            ),
        )


@dataclass(frozen=True)
class Unknown:
    reason: str


@dataclass(frozen=True)
class PartialConst:
    """A 32-bit value known only at some bit positions.

    Used for ASTATX/ASTATY: different instruction classes each define a
    disjoint group of bits (ALU flags, shifter flags, multiplier flags, BTF,
    CACC), so full 32-bit knowledge is rare in practice, but bit-level
    knowledge is common and is all the condition predicates ever need (each
    reads at most a handful of specific bits). ``mask`` has a 1 at every
    known bit position; ``bits`` holds the known value at those positions and
    is canonicalized to 0 elsewhere so two PartialConst values with the same
    knowledge compare and hash equal regardless of what an unknown position
    happened to hold before.
    """

    mask: int
    bits: int

    def __post_init__(self):
        object.__setattr__(self, "mask", self.mask & 0xFFFFFFFF)
        object.__setattr__(self, "bits", self.bits & self.mask)


Value = Union[Const, Affine, Unknown, PartialConst]
_SYMBOL_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

# ASTATX/ASTATY bit positions (SHARC+ PRM ch.4 REGF_ASTATX/REGF_ASTATY).
AZ_BIT, AV_BIT, AN_BIT, AC_BIT, AS_BIT, AI_BIT = 0, 1, 2, 3, 4, 5
MN_BIT, MV_BIT, MU_BIT, MI_BIT = 6, 7, 8, 9
AF_BIT = 10
SV_BIT, SZ_BIT, SS_BIT = 11, 12, 13
BTF_BIT = 18
ALUSAT_BIT = 13  # MODE1.ALUSAT
TRUNCATE_BIT = 15  # MODE1.TRUNCATE (PRM Table 28-19, p.28-63): rounding mode
# select for FIX -- 0 rounds to nearest (ties to even), 1 truncates toward
# zero. TRUNC always truncates and does not consult this bit (PRM p.24-.. /
# PGR p.11-37: "The trunc operation always truncates toward 0. The TRUNCATE
# bit does not influence operation of the trunc instruction.").

# Bits every fixed-point ALU op (add/sub/inc/dec/pass/not/and/or/xor/compare)
# defines: AZ/AV/AN/AC/AS/AI, plus AF which ch.19's intro says every
# fixed-point ALU op clears (PRM p.439).
ALU_FLAGS_MASK = (
    (1 << AZ_BIT)
    | (1 << AV_BIT)
    | (1 << AN_BIT)
    | (1 << AC_BIT)
    | (1 << AS_BIT)
    | (1 << AI_BIT)
    | (1 << AF_BIT)
)
# Multiplier-result flags (MN/MV/MU/MI); the tracer does not model the
# multiplier result format, so these are always left unknown except for the
# MR data-move, which the PRM (p.493) documents as clearing all four.
MULT_FLAGS_MASK = (1 << MN_BIT) | (1 << MV_BIT) | (1 << MU_BIT) | (1 << MI_BIT)

# IF-condition codes (PGR Table 10-4) that read a single ASTATX bit,
# optionally complemented.
SIMPLE_COND_BITS = {
    0x03: (AC_BIT, False),
    0x13: (AC_BIT, True),
    0x04: (AV_BIT, False),
    0x14: (AV_BIT, True),
    0x05: (MV_BIT, False),
    0x15: (MV_BIT, True),
    0x06: (MN_BIT, False),
    0x16: (MN_BIT, True),
    0x07: (SV_BIT, False),
    0x17: (SV_BIT, True),
    0x08: (SZ_BIT, False),
    0x18: (SZ_BIT, True),
    0x0D: (BTF_BIT, False),
    0x1D: (BTF_BIT, True),
}


def _affine(constant: int, terms: tuple[tuple[str, int], ...]) -> Const | Affine:
    """Build a canonical affine value, collapsing a constant expression."""
    value = Affine(constant, terms)
    return Const(value.constant) if not value.terms else value


def symbol(name: str) -> Affine:
    """Return the named symbolic value NAME."""
    if not _SYMBOL_RE.fullmatch(name):
        raise ValueError("invalid symbol name: " + repr(name))
    return Affine(0, ((name, 1),))


@dataclass(frozen=True)
class Pending:
    # A None target marks the delay slots of a conditional transfer not taken.
    target: Optional[int]
    call: bool = False
    slots: int = 2
    return_from_call: bool = False
    return_sw: Optional[int] = None


# Pending.return_sw placeholder for a delayed call: the return address is the
# PC after the second delay slot, known only once both slots have executed.
AFTER_DELAY_SLOTS = -1


@dataclass(frozen=True)
class Loop:
    start_sw: int
    end_sw: int
    remaining: int
    mode: int


@dataclass
class State:
    pc_sw: int
    uregs: Dict[int, Value] = field(default_factory=dict)
    trace: List[dict] = field(default_factory=list)
    pending: Optional[Pending] = None
    steps: int = 0
    stopped: Optional[str] = None
    # Concrete mode is deliberately loader-only.  OVERLAY is per path, so a
    # conditional fork cannot mutate another path or the immutable boot image.
    concrete: Optional[LoadedMemory] = None
    overlay: Dict[int, int] = field(default_factory=dict)
    base_sw: Optional[int] = None
    follow_loaded_calls: bool = False
    continue_external_calls: bool = False
    dossier_bytes: int = 0
    max_call_depth: int = 0
    call_stack: List[int] = field(default_factory=list)
    skip_provisional_entries: bool = False
    at_loaded_entry: bool = False
    assume_nw32: bool = False
    loops: List[Loop] = field(default_factory=list)
    status_stack: List[tuple[Value, Value, Value]] = field(default_factory=list)
    core_reset_state: bool = False
    mmrs: Dict[int, Value] = field(default_factory=dict)
    data_memory_tainted: bool = False
    special: Dict[str, Value] = field(default_factory=dict)
    # Forms this run may execute although the table marks them unconfirmed,
    # and the ones it actually did. A state that used any is calibration.
    provisional_forms: tuple[str, ...] = ()
    provisional_used: tuple[str, ...] = ()


def _signed(value: int, bits: int) -> int:
    return value - (1 << bits) if value & (1 << (bits - 1)) else value


def _field(f: Mapping[str, int], stem: str) -> int:
    for key, value in f.items():
        if key == stem or key.startswith(stem + "["):
            return value
    raise KeyError(stem)


def _wide(f: Mapping[str, int], stem: str) -> int:
    return (_field(f, stem + "[31:16]") << 16) | _field(f, stem + "[15:0]")


def _signed32(value: int) -> int:
    return _signed(value & 0xFFFFFFFF, 32)


def _render(value: Value | int) -> str:
    if isinstance(value, Const):
        return _render(value.value)
    if isinstance(value, Affine):
        parts: list[tuple[int, str]] = []
        for name, coefficient in value.terms:
            coefficient = _signed32(coefficient)
            magnitude = (
                name if abs(coefficient) == 1 else "%d*%s" % (abs(coefficient), name)
            )
            parts.append((coefficient, magnitude))
        constant = _signed32(value.constant)
        if constant:
            parts.append((constant, hex(abs(constant))))
        if not parts:
            return "0x0"
        first_sign, first = parts[0]
        rendered = ("-" if first_sign < 0 else "") + first
        for sign, magnitude in parts[1:]:
            rendered += (" - " if sign < 0 else " + ") + magnitude
        return rendered
    if isinstance(value, PartialConst):
        return "partial(known=%#010x, bits=%#010x)" % (value.mask, value.bits)
    if isinstance(value, Unknown):
        return value.reason
    return ("-" if value < 0 else "") + hex(abs(value))


def _json_value(value: Value | int) -> int | dict:
    """Render tracer values without leaking internal dataclasses into CLI JSON."""
    if isinstance(value, Const):
        return value.value
    if isinstance(value, Affine):
        return {
            "affine": {
                "constant": value.constant,
                "terms": [list(term) for term in value.terms],
            }
        }
    if isinstance(value, PartialConst):
        return {"partial": {"known_mask": value.mask, "known_bits": value.bits}}
    if isinstance(value, Unknown):
        return {"unknown": value.reason}
    return value


def _event(state: State, insn: Instruction, action: str, **extra) -> None:
    for key in ("address", "value", "concrete_value"):
        if key in extra:
            extra[key] = _json_value(extra[key])
    state.trace.append(
        {"pc_sw": state.pc_sw, "form": insn.type_name, "action": action, **extra}
    )


def _stop(state: State, insn: Optional[Instruction], reason: str) -> State:
    form = insn.type_name if insn else None
    state.trace.append(
        {"pc_sw": state.pc_sw, "form": form, "action": "stop", "reason": reason}
    )
    state.stopped = reason
    return state


def _copy(state: State) -> State:
    return State(
        state.pc_sw,
        dict(state.uregs),
        [dict(event) for event in state.trace],
        state.pending,
        state.steps,
        state.stopped,
        state.concrete,
        dict(state.overlay),
        state.base_sw,
        state.follow_loaded_calls,
        state.continue_external_calls,
        state.dossier_bytes,
        state.max_call_depth,
        list(state.call_stack),
        state.skip_provisional_entries,
        state.at_loaded_entry,
        state.assume_nw32,
        list(state.loops),
        list(state.status_stack),
        state.core_reset_state,
        dict(state.mmrs),
        state.data_memory_tainted,
        dict(state.special),
        state.provisional_forms,
        state.provisional_used,
    )


def _concrete_address(value: Value | int) -> Optional[int]:
    return (
        value.value
        if isinstance(value, Const)
        else (value if isinstance(value, int) else None)
    )


def _canonical_dm_address(
    state: State, address: int, width: int, *, for_write: bool = False
) -> Optional[int]:
    """Resolve a DSP DM address to the loader's byte-address alias.

    Application code uses unaliased DM pointers such as ``0x26968c`` whereas
    the boot stream is keyed at ``SW_ALIAS_BASE + 0x26968c``.  Keep an already
    mapped direct address (notably external memory and MMRs) unchanged; only
    retry an unmapped low address through the alias.
    """
    concrete = state.concrete
    if concrete is None:
        return None

    def mapped(base: int) -> bool:
        return all(
            here in state.overlay or concrete.read(here, 1) is not None
            for here in range(base, base + width)
        )

    if mapped(address):
        return address
    if 0 <= address < SW_ALIAS_BASE:
        alias = SW_ALIAS_BASE + address
        if for_write or mapped(alias):
            return alias
    # Runtime RAM and MMR destinations need not have loader initializer bytes.
    # A concrete write creates those bytes in this path's overlay.
    return address if for_write else None


def _dm_read(
    state: State, address: Value | int, width: int, signed: bool = False
) -> Optional[Const]:
    """Read little-endian loader-backed DM bytes plus this path's overlay."""
    concrete = _concrete_address(address)
    if state.concrete is None or concrete is None or width not in (1, 2, 4, 8):
        return None
    fixed_width_mmr = (
        concrete in CORE_MMR_RESET_VALUES or name_address(concrete) is not None
    )
    if width == 4 and fixed_width_mmr and concrete in state.mmrs:
        value = state.mmrs[concrete]
        return value if isinstance(value, Const) else None
    if width == 4 and fixed_width_mmr and state.data_memory_tainted:
        return None
    if (
        width == 4
        and not state.assume_nw32
        and not fixed_width_mmr
        and not 0x30000000 <= concrete < 0x40000000
    ):
        # Internal normal-word width depends on runtime IMDWx state.  Reading
        # four loader bytes as one word is opt-in until that state is known.
        return None
    concrete = _canonical_dm_address(state, concrete, width)
    if concrete is None:
        return None
    if state.data_memory_tainted and not all(
        here in state.overlay for here in range(concrete, concrete + width)
    ):
        return None
    backing = state.concrete
    assert backing is not None
    raw = bytearray()
    for here in range(concrete, concrete + width):
        if here in state.overlay:
            raw.append(state.overlay[here])
        else:
            byte = backing.read(here, 1)
            assert byte is not None
            raw.append(byte[0])
    value = int.from_bytes(raw, "little", signed=signed)
    # A long word needs a register pair, which this tracer intentionally does
    # not model.  Do not truncate it into a false 32-bit value.
    return Const(value) if width <= 4 else None


def _read_px48(state: State, address: Value | int) -> Optional[tuple[Const, Const]]:
    """Read a loader-backed 48-bit normal word into the PX1/PX2 halves.

    A combined-PX DM or PM transfer without ``LW`` is 48 bits.  L1 block 3's
    normal-word alias packs those words in three 16-bit columns, while loader
    records use the short-word/system-byte view.  Each 48-bit word therefore
    consumes six loader bytes.  The three parcels are individually little-
    endian, but retain their architectural high-to-low order.
    """
    concrete = _concrete_address(address)
    if (
        state.concrete is None
        or concrete is None
        or not L1_BLOCK3_NW_BASE <= concrete < L1_BLOCK3_NW_LIMIT
    ):
        return None
    offset = concrete - L1_BLOCK3_NW_BASE
    byte_address = sw_to_byte(L1_BLOCK3_SW_BASE) + 6 * offset
    raw = state.concrete.read(byte_address, 6)
    if raw is None:
        return None
    high, middle, low = (
        int.from_bytes(raw[start : start + 2], "little") for start in (0, 2, 4)
    )
    px2 = Const((high << 16) | middle)
    px1 = Const(low << 16)
    return px1, px2


def _load_normal_ureg(
    state: State, space: str, address: Value | int, code: int
) -> Optional[Const | dict[str, int]]:
    """Load one normal-word UREG value, including combined-PX DM/PM reads."""
    if code == UREG_CODES["PX"]:
        halves = _read_px48(state, address)
        if halves is not None:
            px1, px2 = halves
            state.uregs[UREG_CODES["PX"]] = Unknown(
                "combined PX represented by PX1/PX2"
            )
            state.uregs[UREG_CODES["PX1"]] = px1
            state.uregs[UREG_CODES["PX2"]] = px2
            return {"PX1": px1.value, "PX2": px2.value}
        state.uregs[UREG_CODES["PX1"]] = Unknown("memory-address " + _render(address))
        state.uregs[UREG_CODES["PX2"]] = Unknown("memory-address " + _render(address))
    elif space == "DM":
        loaded = _dm_read(state, address, 4)
        state.uregs[code] = loaded or Unknown("memory-address " + _render(address))
        return loaded
    state.uregs[code] = Unknown("memory-address " + _render(address))
    return None


def _dm_write(state: State, address: Value | int, width: int, value: Value) -> bool:
    concrete = _concrete_address(address)
    if (
        state.concrete is None
        or concrete is None
        or not isinstance(value, Const)
        or width not in (1, 2, 4)
    ):
        return False
    fixed_width_mmr = (
        concrete in CORE_MMR_RESET_VALUES or name_address(concrete) is not None
    )
    if width == 4 and fixed_width_mmr:
        state.mmrs[concrete] = value
        return True
    if (
        width == 4
        and not state.assume_nw32
        and not fixed_width_mmr
        and not 0x30000000 <= concrete < 0x40000000
    ):
        return False
    concrete = _canonical_dm_address(state, concrete, width, for_write=True)
    if concrete is None:
        return False
    raw = (value.value & 0xFFFFFFFF).to_bytes(4, "little")[:width]
    state.overlay.update(zip(range(concrete, concrete + width), raw))
    return True


def _dossier(state: State, target: int, return_sw: int) -> dict:
    registers = {
        UREG_NAMES[k]: _json_value(v)
        for k, v in state.uregs.items()
        if isinstance(v, Const)
    }
    objects = []
    if state.concrete is not None and state.dossier_bytes:
        seen = set()
        for name, value in registers.items():
            if not isinstance(value, int) or value in seen:
                continue
            raw = bytearray()
            for offset in range(state.dossier_bytes):
                b = _dm_read(state, value + offset, 1)
                if b is None:
                    break
                raw.append(b.value)
            if raw:
                seen.add(value)
                objects.append(
                    {
                        "register": name,
                        "address": value,
                        "bytes": list(raw),
                        "words_le": [
                            int.from_bytes(raw[i : i + 4], "little")
                            for i in range(0, len(raw) - 3, 4)
                        ],
                    }
                )
    return {
        "target_sw": target,
        "return_sw": return_sw,
        "registers": registers,
        "objects": objects,
    }


def _ureg_raw(values: Mapping[int, Value], code: int) -> Value:
    """Read UREG CODE exactly as stored, including a PartialConst for
    ASTATX/ASTATY. Only the flag/predicate code that understands
    PartialConst (see the ``_astatx_*`` helpers, ``_apply_compute``,
    ``_predicate``, the Type18a BTF writers, and the status-stack push) may
    call this. Everything else — arithmetic, addressing, memory, UREG
    moves, dossiers — must use ``_ureg``, which never lets a PartialConst
    escape into generic code that only understands Const/Affine/Unknown
    (``_terms``/``_add``/``_negate``/``_multiply``/``_bitwise`` would
    otherwise crash or silently misbehave on one).
    """
    return values.get(code, Unknown("uninitialized " + UREG_NAMES[code]))


def _ureg(values: Mapping[int, Value], code: int) -> Value:
    """Read UREG CODE as a value any generic consumer can handle.

    A PartialConst (only ever stored at ASTATX/ASTATY) never escapes this
    function: a fully-known one becomes Const, a partially-known one
    becomes Unknown. This is what makes "R0 = ASTATX" (a Type5 UREG move),
    an ASTATX value used as a compute operand or DM address, or a status
    register read by a dossier all safe by construction, without each of
    those call sites needing to know about PartialConst.
    """
    value = _ureg_raw(values, code)
    if isinstance(value, PartialConst):
        return Const(value.bits) if value.mask == 0xFFFFFFFF else Unknown(
            "partially known ASTATx"
        )
    return value


def _terms(value: Const | Affine) -> tuple[int, tuple[tuple[str, int], ...]]:
    return (
        (value.value, ()) if isinstance(value, Const) else (value.constant, value.terms)
    )


# A symbol name this module recognises as denoting a value some caller has
# already bounded to a known numeric range: either an entry-time seed in
# the convention tools/sharcwriters.py's seed_sets()/ENTRY_SEED_NAMES uses
# ("I6e", "B7e", ...: one or more uppercase letters, one or more digits,
# then "e"), or one of this module's own CIRC_SYMBOL_PREFIX-tagged symbols
# (below). This module does not itself know the numeric bound -- that is
# the caller's fact to state (tools/sharcwriters.py's STACK_SYMBOLS /
# CIRC_WRAP_SLACK) -- it only recognises the *shape* of a name a caller is
# likely to have bounded, so it knows when re-deriving a fresh symbol
# through a circular MODIFY is meaningful rather than fabricating a bound
# for an arbitrary, unrelated value that merely happens to be a bare named
# term (e.g. a loop-count symbol).
_BOUNDED_SYMBOL_RE = re.compile(r"^[A-Z]+\d+e$")
CIRC_SYMBOL_PREFIX = "circ_"


def _stack_bounded_symbol(value: Value) -> Optional[tuple[str, int]]:
    """-> (name, signed constant offset), if `value` is exactly one named
    symbol with coefficient 1 (any constant offset) whose name matches
    _BOUNDED_SYMBOL_RE or starts with CIRC_SYMBOL_PREFIX -- otherwise None.
    A second term, or a coefficient other than 1, means the value's range
    is no longer provably tied to the symbol's own bound (e.g. a scaled or
    summed expression), so the caller falls back to Unknown rather than
    guess."""
    if isinstance(value, Affine) and len(value.terms) == 1:
        name, coefficient = value.terms[0]
        if coefficient == 1 and (
            _BOUNDED_SYMBOL_RE.match(name) or name.startswith(CIRC_SYMBOL_PREFIX)
        ):
            return name, _signed(value.constant, 32)
    return None


def _add(left: Value, right: Value, expression: str) -> Value:
    if isinstance(left, Unknown) or isinstance(right, Unknown):
        return Unknown(expression)
    constant, terms = _terms(left)
    other_constant, other_terms = _terms(right)
    return _affine(constant + other_constant, terms + other_terms)


def _negate(value: Value, expression: str) -> Value:
    if isinstance(value, Unknown):
        return Unknown(expression)
    constant, terms = _terms(value)
    return _affine(
        -constant, tuple((name, -coefficient) for name, coefficient in terms)
    )


def _subtract(left: Value, right: Value, expression: str) -> Value:
    return _add(left, _negate(right, expression), expression)


def _multiply(left: Value, right: Value, expression: str) -> Value:
    if isinstance(left, Unknown) or isinstance(right, Unknown):
        return Unknown(expression)
    if isinstance(left, Const) and isinstance(right, Const):
        return Const(left.value * right.value)
    if isinstance(left, Const):
        constant, terms = _terms(right)
        return _affine(
            left.value * constant,
            tuple((name, left.value * coefficient) for name, coefficient in terms),
        )
    if isinstance(right, Const):
        constant, terms = _terms(left)
        return _affine(
            right.value * constant,
            tuple((name, right.value * coefficient) for name, coefficient in terms),
        )
    return Unknown(expression + " (non-affine multiplication)")


def _aconv_symbol(value: Affine, direction: str, source_code: int, pc_sw: int) -> Affine:
    """Return an opaque, stable symbolic result for map-dependent ACONV.

    B2W is not affine when the source's low two bits are unknown.  The PRM's
    address-map/ILAD exception also prevents treating a symbolic source as an
    unconditional shift.  Retaining a source-derived opaque symbol lets the
    bounded writer tracer continue without asserting a false linear relation.
    """
    pieces = [direction, str(source_code), "%x" % pc_sw, "%x" % value.constant]
    pieces.extend("%s_%x" % (name, coefficient) for name, coefficient in value.terms)
    return symbol("aconv_" + "_".join(pieces))


def _aconv(value: Value, w2b: bool, source_code: int, pc_sw: int) -> Value:
    """Apply the PRM-likely ACONV arithmetic without inventing ILAD behavior."""
    if isinstance(value, Const):
        return Const(value.value << 2 if w2b else value.value >> 2)
    if not isinstance(value, Affine):
        return Unknown("ACONV source is not symbolic")
    if w2b:
        return _multiply(value, Const(4), "ACONV W2B")
    if value.constant % 4 == 0 and all(coefficient % 4 == 0 for _, coefficient in value.terms):
        return _affine(
            value.constant // 4,
            tuple((name, coefficient // 4) for name, coefficient in value.terms),
        )
    return _aconv_symbol(value, "b2w", source_code, pc_sw)


def _access_modifier_scale(access_width: str, assume_nw32: bool) -> int:
    """Return SHARC+ byte-space scaled-address arithmetic width."""
    if access_width.startswith("short-word"):
        return 2
    if access_width == "long-word":
        return 8
    if access_width == "normal-word" and assume_nw32:
        return 4
    return 1


def _bitwise(left: Value, right: Value, expression: str, operation) -> Value:
    if isinstance(left, Const) and isinstance(right, Const):
        return Const(operation(left.value, right.value))
    return Unknown(expression)


def _sync_pc_stack(state: State) -> None:
    """Mirror the tracer's architectural PC stack into its public registers."""
    state.uregs[UREG_CODES["PCSTKP"]] = Const(len(state.call_stack))
    state.uregs[UREG_CODES["PCSTK"]] = (
        Const(state.call_stack[-1]) if state.call_stack else Const(0x7FFFFFFF)
    )
    stkyx_code = UREG_CODES["STKYX"]
    state.uregs[stkyx_code] = _bitwise(
        _ureg(state.uregs, stkyx_code),
        Const(1 << 22),
        "PC stack empty" if not state.call_stack else "PC stack nonempty",
        (lambda value, mask: value | mask)
        if not state.call_stack
        else (lambda value, mask: value & ~mask),
    )


def _shift_immediate(
    f: Mapping[str, int], values: Mapping[int, Value]
) -> tuple[int, Value, str, "Callable[[Value], Value]"]:
    """Execute the documented ShiftImm subset seen on qualifying paths."""
    field = (_field(f, "shiftimm[22:16]") << 16) | _field(f, "shiftimm[15:0]")
    opcode = (field >> 16) & 0x3F
    data8 = (field >> 8) & 0xFF
    rn, rx = (field >> 4) & 0xF, field & 0xF
    source = _ureg(values, rx)
    if opcode in (0x00, 0x01, 0x08, 0x09):
        amount = _signed(data8, 8)
        base = opcode & 0x01
        name = "lshift" if base == 0x00 else "ashift"
        if amount == 0:
            shifted = source
        elif not isinstance(source, Const):
            shifted = Unknown("%s R%d by %d" % (name, rx, amount))
        elif amount >= 32:
            shifted = Const(0)
        elif amount <= -32:
            shifted = (
                Const(0xFFFFFFFF)
                if base == 0x01 and source.value & 0x80000000
                else Const(0)
            )
        elif amount > 0:
            shifted = Const(source.value << amount)
        elif base == 0x01:
            shifted = Const(_signed32(source.value) >> -amount)
        else:
            shifted = Const(source.value >> -amount)
        if opcode in (0x08, 0x09):
            # PRM Table 17-9, shiftimm 001000/001001 (p. 17-10): RN = RN or
            # (l/a)shift RX by DATA8.
            value = _bitwise(
                _ureg(values, rn),
                shifted,
                "R%d or %s R%d by %d" % (rn, name, rx, amount),
                lambda a, b: a | b,
            )
            operation = (
                "logical-shift-or-immediate"
                if base == 0x00
                else "arithmetic-shift-or-immediate"
            )
        else:
            value = shifted
            operation = (
                "logical-shift-immediate"
                if base == 0x00
                else "arithmetic-shift-immediate"
            )
        # SZ is defined from the shifted value before any OR (PRM pp.509-510:
        # "Set if the shifted result is zero"); SS is cleared for every one
        # of these forms except OR-ashift (opcode 0x09), whose entry omits
        # the SS line.
        ss_mode = "forget" if opcode == 0x09 else "clear"
        return rn, value, operation, _astatx_shift(amount, shifted, ss_mode)
    if opcode == 0x10:
        position = data8 & 0x3F
        length = (_field(f, "dataex[3:0]") << 2) | (data8 >> 6)
        if length == 0:
            value = Const(0)
        elif not isinstance(source, Const):
            value = Unknown("fext R%d by %d:%d" % (rx, position, length))
        else:
            value = Const((source.value >> position) & ((1 << min(length, 32)) - 1))
        return rn, value, "field-extract-immediate", _astatx_fext(position + length, value)
    if opcode == 0x12:
        # PRM Table 17-9 p.17-10/17-11 (out/refs/sharc-plus-prm/all.txt
        # lines 22781-22785): shiftimm 010010 is
        # "RREG = fext RREG by BIT6:LEN6 (se)" -- the sign-extending twin
        # of the already-implemented opcode 0x10. Field layout (position,
        # length) is identical to 0x10; NOTE at PRM p.3-17 (all.txt
        # line 3486) says the (SE) option "sign extends the left bits" of
        # the extracted field, i.e. bits above the extracted field take the
        # value of the field's own sign (MSB) instead of being cleared.
        # CALIBRATION NOTE: added in a scratch copy of sharc_trace.py for
        # this task only; not part of the tracked tool.
        position = data8 & 0x3F
        length = (_field(f, "dataex[3:0]") << 2) | (data8 >> 6)
        if length == 0:
            value = Const(0)
        elif not isinstance(source, Const):
            value = Unknown("fext R%d by %d:%d (se)" % (rx, position, length))
        else:
            value = Const(_signed(source.value >> position, min(length, 32)) & 0xFFFFFFFF)
        return rn, value, "field-extract-immediate-se", _astatx_fext(position + length, value)
    if opcode in (0x30, 0x31):
        position = data8
        if position > 31:
            value = source
        else:
            calculate = (
                (lambda a, b: a | b) if opcode == 0x30 else (lambda a, b: a & ~b)
            )
            name = "bset" if opcode == 0x30 else "bclr"
            value = _bitwise(
                source,
                Const(1 << position),
                "%s R%d by %d" % (name, rx, position),
                calculate,
            )
        operation = "bit-set-immediate" if opcode == 0x30 else "bit-clear-immediate"
        return rn, value, operation, _astatx_bit_field(position, value)
    if opcode == 0x32:
        position = data8
        if position > 31:
            value = source
        else:
            value = _bitwise(
                source,
                Const(1 << position),
                "btgl R%d by %d" % (rx, position),
                lambda a, b: a ^ b,
            )
        return rn, value, "bit-toggle-immediate", _astatx_bit_field(position, value)
    if opcode == 0x33:
        # PRM Table 17-9: ShiftImm 110011 is btst RX by DATA8, the immediate
        # form of the 11001100 register operation. It updates status only, so
        # RN keeps its value.
        return rn, source, "bit-test", _astatx_btst(source, Const(data8))
    raise ValueError("unsupported ShiftImm opcode %#x" % opcode)


def _not(value: Value, expression: str) -> Value:
    return Const(~value.value) if isinstance(value, Const) else Unknown(expression)


def _compare_flags(left: Value, right: Value, signed: bool, label: str) -> Value:
    """Return AZ (bit 0), AN (bit 2) and the new CACC MSB (bit 31) of a compare.

    PRM comp/compu (pp. 18-5, 18-6): AZ when RX equals RY, AN when RX is
    smaller, and the CACC MSB when RX is greater.
    """
    if not isinstance(left, Const) or not isinstance(right, Const):
        return Unknown(label)
    x, y = left.value & 0xFFFFFFFF, right.value & 0xFFFFFFFF
    if signed:
        x, y = _signed32(x), _signed32(y)
    return Const(
        (0x1 if x == y else 0)
        | (0x4 if x < y else 0)
        | (0x80000000 if x > y else 0)
    )


# ---------------------------------------------------------------------------
# Floating-point compute support.
#
# The register file (R0-R15/F0-F15) is a flat 32-bit store either way; a
# float ALU/multiplier op just reinterprets the same bits as IEEE-754 single
# precision (PRM p.3-4: "floating-point instructions operate on 32-bit ...
# operands"). The SHARC+ ALU/multiplier additionally support an optional
# 40-bit extended-precision float format for *intermediate* results (PRM
# p.3-37, active when MODE1.RND32=0, the reset default: "eight additional
# LSBs of mantissa"), but that only matters to values forwarded between
# back-to-back compute ops without ever reaching the register file; this
# tracer has no pipeline/forwarding model and every UREG it tracks is a
# plain 32-bit value, so every float result here is computed and stored at
# IEEE-754 single precision -- the same width a real RN/FN write-back uses
# regardless of the extended-precision mode. Denormal-flush-to-zero, which
# several PGR entries below document for their inputs/outputs, is also not
# modeled (struct's round-trip preserves denormals exactly); this only
# matters for subnormal magnitudes, which real audio sample/parameter data
# essentially never produces.
# ---------------------------------------------------------------------------


def _float32(value: Value) -> Optional[float]:
    """Reinterpret VALUE's 32-bit pattern as IEEE-754 single precision.

    Returns None when VALUE isn't a fully known Const: an Affine (symbolic
    address arithmetic) or Unknown source is not float data and must not be
    silently coerced into one.
    """
    if not isinstance(value, Const):
        return None
    return struct.unpack("<f", struct.pack("<I", value.value))[0]


def _float32_bits(value: float) -> tuple[int, bool]:
    """Round VALUE to IEEE-754 single precision; return (bits, overflowed).

    struct raises OverflowError for a finite double outside the float32
    range. SHARC+ float overflow rounds to signed infinity in the default
    round-to-nearest mode (PRM Table 3-3 / PGR p.11-24 AV description:
    "post-rounded result overflows ... returns +-infinity"); reproduce that
    by hand when struct refuses, since struct has no float32-infinity
    fallback of its own.
    """
    try:
        return struct.unpack("<I", struct.pack("<f", value))[0], False
    except OverflowError:
        sign = 0x80000000 if math.copysign(1.0, value) < 0 else 0
        return (0x7F800000 | sign), True


_FLOAT_ALL_ONES = Const(0xFFFFFFFF)


def _float_binary(
    left: Value, right: Value, expression: str, operation
) -> tuple[Value, Optional[bool], Optional[bool]]:
    """Evaluate a float ALU/multiplier binary OPERATION; return (result,
    overflowed, invalid).

    Several PGR float-ALU entries (e.g. p.11-24 Fx+Fy, p.11-46 MIN, p.11-48
    CLIP) document "A NAN input returns an all 1s result" -- a fixed
    sentinel pattern, not whatever IEEE NaN OPERATION would naturally
    produce -- so an explicit NaN *input* is special-cased before OPERATION
    ever runs. A NaN produced by OPERATION itself from two non-NaN inputs
    (e.g. +infinity + -infinity, PGR p.11-24's "opposite-signed infinities"
    AI case) is not overridden: it keeps the ordinary computed NaN pattern.
    Either input not being a known Const makes the whole result unknown.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(expression), None, None
    if math.isnan(a) or math.isnan(b):
        return _FLOAT_ALL_ONES, False, True
    raw = operation(a, b)
    bits, overflowed = _float32_bits(raw)
    return Const(bits), overflowed, math.isnan(raw)


def _float_unary(
    value: Value, expression: str, operation
) -> tuple[Value, Optional[bool], Optional[bool]]:
    """Unary counterpart of ``_float_binary`` (see its docstring)."""
    a = _float32(value)
    if a is None:
        return Unknown(expression), None, None
    if math.isnan(a):
        return _FLOAT_ALL_ONES, False, True
    raw = operation(a)
    bits, overflowed = _float32_bits(raw)
    return Const(bits), overflowed, math.isnan(raw)


def _float_min(a: float, b: float) -> float:
    """PGR p.11-46: smaller operand; min(+0, -0) is documented as -0."""
    if a == 0.0 and b == 0.0:
        return -0.0
    return a if a < b else b


def _float_max(a: float, b: float) -> float:
    """PGR p.11-47: larger operand; max(+0, -0) is documented as +0."""
    if a == 0.0 and b == 0.0:
        return 0.0
    return a if a > b else b


def _float_clip(a: float, b: float) -> float:
    """PGR p.11-48 / PRM p.3-6 CLIP: FX if |FX| < |FY|, else +-|FY| with
    FX's sign (copysign handles the FX=+-0 boundary the same as the PGR
    text's "if Fx is positive")."""
    return a if abs(a) < abs(b) else math.copysign(abs(b), a)


def _float_mantissa(
    value: Value, expression: str
) -> tuple[Value, Optional[bool], Optional[bool], Optional[bool]]:
    """RN = mant FX (PRM Table 18-5 opcode 0xAD, p.427; PGR p.11-34/11-35).

    Extracts the hidden bit plus the 23-bit fraction, left-justified as an
    unsigned-magnitude 1.31 fixed-point word (bit31 the hidden bit, bits
    30-8 the fraction, bits 7-0 zero-filled); the 24 significant bits
    always fit exactly, so no rounding is performed (PGR: "no rounding is
    performed because all results are inherently exact"). Denormal and
    zero inputs flush to a zero mantissa (PGR: "Denormal inputs are
    flushed to +-zero"). A NAN *or an infinity* input returns the fixed
    all-1s sentinel (PGR: "A NAN or an infinity input returns an all 1s
    result") -- unlike the arithmetic float ALU ops above, which only
    override NAN, MANT also overrides infinity, since it is not an
    ordinary IEEE operation. Returns (result, overflow=is-infinity,
    negative=input-sign, invalid=is-NAN); AN is always cleared for this
    op (PRM Table 3-3) and is not returned here.
    """
    if not isinstance(value, Const):
        return Unknown(expression), None, None, None
    bits = value.value
    sign = bool(bits & 0x80000000)
    exponent = (bits >> 23) & 0xFF
    fraction = bits & 0x7FFFFF
    if exponent == 0xFF:
        is_nan = fraction != 0
        return _FLOAT_ALL_ONES, not is_nan, sign, is_nan
    if exponent == 0:
        return Const(0), False, sign, False
    return Const((0x800000 | fraction) << 8), False, sign, False


def _float_scalb(
    value: Value, scale: Value, expression: str
) -> tuple[Value, Optional[bool], Optional[bool]]:
    """FN = scalb FX by RY (PRM Table 18-5 opcode 0xBD, p.427; PGR p.11-33).

    Adds the two's-complement fixed-point integer RY to FX's exponent
    (i.e. FX * 2**RY). Overflow rounds to +-infinity (round-to-nearest,
    the only rounding mode this tracer models, matching every other float
    op here); a result whose magnitude underflows below the smallest
    float32 normal (2**-126) flushes to +-zero rather than becoming a
    subnormal (PGR: "Denormal returns +-zero" -- an explicit override of
    struct's ordinary IEEE denormal rounding, the same kind of override
    ``_float_to_fixed_trunc`` already applies for its own corner cases). A
    NAN input returns the same all-1s sentinel ``_float_binary`` uses;
    zero and infinity inputs pass through unchanged (``math.ldexp``
    preserves both, matching the PRM, which documents no special case for
    them). Returns (result, overflow, invalid).
    """
    a = _float32(value)
    if a is None or not isinstance(scale, Const):
        return Unknown(expression), None, None
    if math.isnan(a):
        return _FLOAT_ALL_ONES, False, True
    shift = _signed32(scale.value)
    try:
        scaled = math.ldexp(a, shift)
    except OverflowError:
        scaled = math.copysign(math.inf, a)
    if scaled != 0.0 and not math.isinf(scaled) and abs(scaled) < 2.0**-126:
        return Const(0x80000000 if scaled < 0 else 0), False, False
    bits, overflowed = _float32_bits(scaled)
    return Const(bits), overflowed, False


def _fixed_to_float(value: Value, expression: str) -> tuple[Value, Optional[bool]]:
    """FN = float RX (PRM Table 18-5 opcode 0xCA, p.427; PGR p.11-39 "without
    scaling factor"): numeric int32->float32 conversion, not a bit
    reinterpretation. PGR documents AV and AI both fixed 0 for the
    no-scaling form actually used here (RN=FLOAT RX BY RY, which also takes
    a scale factor, is not implemented). Returns (result, invalid) where
    invalid is always False when computable, matching that fixed AI=0.
    """
    if not isinstance(value, Const):
        return Unknown(expression), None
    bits, _ = _float32_bits(float(_signed32(value.value)))
    return Const(bits), False


def _float_to_fixed(
    value: Value, mode1: Value, always_truncate: bool, expression: str
) -> tuple[Value, Optional[bool], Optional[bool]]:
    """RN = FIX FX / RN = TRUNC FX (PRM Table 18-5 opcodes 0xC9/0xCD, p.427;
    PGR p.11-36..11-38), and their scaled BY RY siblings 0xD9/0xDD (the
    caller pre-scales VALUE via ``_scale_fixed_input`` -- PGR Table 3-3,
    p.3-11 marks the BY RY forms with the identical AZ/AV/AN/AC/AS/AI
    columns as the unscaled ones, so no separate flag rule is needed here).

    TRUNC always truncates toward zero, ignoring MODE1 (ALWAYS_TRUNCATE=True
    -- PGR p.11-37: "The trunc operation always truncates toward 0. The
    TRUNCATE bit does not influence operation of the trunc instruction.").
    FIX instead rounds to nearest, ties to even (Python's ``round()`` on a
    float already implements this) when MODE1.TRUNCATE=0, or truncates
    toward zero when MODE1.TRUNCATE=1 (PGR p.11-37 / PRM p.24-..); the whole
    result is Unknown when TRUNCATE itself is unknown, rather than guessing
    which rounding applied.

    A result within int32 range needs no saturation. Out-of-range
    magnitudes and NAN/+-infinity inputs are governed by MODE1.ALUSAT (PGR:
    "In saturation mode ... positive overflows and +infinity return
    0x7FFFFFFF, and negative overflows and -infinity return 0x80000000");
    when ALUSAT is known clear the PGR instead documents a "floating-point
    all 1s" *Rn* pattern for that corner, an architecturally odd case this
    tracer does not attempt to reproduce bit-for-bit, so it reports Unknown
    there and whenever MODE1 itself isn't known, rather than guessing.
    Returns (result, overflow, invalid).
    """
    a = _float32(value)
    if a is None:
        return Unknown(expression), None, None
    saturating = _astatx_known_bit(mode1, ALUSAT_BIT)
    if math.isnan(a) or math.isinf(a):
        if saturating:
            return (
                Const(0x7FFFFFFF if (math.isnan(a) or a > 0) else 0x80000000),
                True,
                True,
            )
        if saturating is False:
            return Unknown(expression + " (unsaturated NAN/infinity fix)"), True, True
        return Unknown(expression), None, True
    if always_truncate:
        rounded = math.trunc(a)
    else:
        truncate_mode = _astatx_known_bit(mode1, TRUNCATE_BIT)
        if truncate_mode is None:
            return Unknown(expression), None, False
        rounded = math.trunc(a) if truncate_mode else int(round(a))
    if -(1 << 31) <= rounded <= (1 << 31) - 1:
        return Const(rounded & 0xFFFFFFFF), False, False
    if saturating:
        return Const(0x7FFFFFFF if rounded > 0 else 0x80000000), True, False
    if saturating is False:
        return Unknown(expression + " (unsaturated fix overflow)"), True, False
    return Unknown(expression), None, False


def _float_to_fixed_trunc(
    value: Value, mode1: Value, expression: str
) -> tuple[Value, Optional[bool], Optional[bool]]:
    """RN = TRUNC FX: ``_float_to_fixed`` with ALWAYS_TRUNCATE=True. Kept as
    a named wrapper since opcode 0xCD's call site predates the shared
    FIX/TRUNC helper and reads more clearly with its own name."""
    return _float_to_fixed(value, mode1, True, expression)


def _scale_fixed_input(value: Value, scale: Value, expression: str) -> Value:
    """Fx * 2**Ry (exponent add), the shared first step of RN = FIX/TRUNC FX
    BY RY (PGR p.11-37: "the fixed-point two's-complement integer in Ry is
    added to the exponent of the floating-point operand in Fx before the
    conversion"). Reuses ``_float_scalb``'s ldexp/overflow/denormal-flush
    rule -- the same exponent-add primitive FN=SCALB uses -- and discards
    its own (overflow, invalid) pair, since the caller's FIX/TRUNC applies
    its own overflow/NAN handling to the scaled value afterward.
    """
    scaled, _overflow, _invalid = _float_scalb(value, scale, expression)
    return scaled


def _fixed_to_float_scaled(
    value: Value, scale: Value, expression: str
) -> tuple[Value, Optional[bool]]:
    """FN = FLOAT RX BY RY (PRM p.19-.. ; PGR p.11-39 "with scaling factor"):
    numeric int32->float32 conversion as ``_fixed_to_float``, then the
    fixed-point two's-complement integer in RY is added to the result's
    exponent (PGR: "the fixed-point two's-complement integer in Ry is added
    to the exponent of the floating-point result"). Overflow (unbiased
    exponent > 127) returns +-infinity; underflow (unbiased exponent <
    -126) flushes to +-zero (PGR: "Overflow generates a return of
    +-infinity ...; underflow generates a return of +-zero"). Unlike the
    unscaled form -- where an int32 input can never land in the subnormal
    range, so AV is architecturally fixed 0 (PGR Table 3-3) -- AV here is
    genuinely data-dependent, so this returns (result, overflow) rather
    than the unscaled helper's implicit always-False.
    """
    if not isinstance(value, Const) or not isinstance(scale, Const):
        return Unknown(expression), None
    unscaled = float(_signed32(value.value))
    shift = _signed32(scale.value)
    try:
        scaled = math.ldexp(unscaled, shift)
    except OverflowError:
        scaled = math.copysign(math.inf, unscaled) if unscaled != 0.0 else 0.0
    if scaled != 0.0 and not math.isinf(scaled) and abs(scaled) < 2.0**-126:
        return Const(0x80000000 if scaled < 0 else 0), False
    bits, overflowed = _float32_bits(scaled)
    return Const(bits), overflowed


def _float_copysign(
    left: Value, right: Value, expression: str
) -> tuple[Value, Optional[bool]]:
    """FN = FX copysign FY (PRM p.19-19, opcode 1110 0000; PGR p.11-45,
    Table 12-4 opcode 1110 0000): copies FY's sign bit onto FX's exponent
    and mantissa unchanged. A denormal FX input flushes to zero before the
    sign copy (PRM/PGR: "A denormal input is flushed to +-zero"). A NAN
    input -- either operand -- returns the fixed all-1s sentinel
    (``_float_binary``'s convention). Returns (result, invalid); AC/AS/AV
    are architecturally fixed for this op (PRM Table 3-3 / PGR Table 3-3)
    and are not returned here.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(expression), None
    if math.isnan(a) or math.isnan(b):
        return _FLOAT_ALL_ONES, True
    magnitude = abs(a)
    if 0.0 < magnitude < 2.0**-126:
        magnitude = 0.0
    negative = math.copysign(1.0, b) < 0
    result = -magnitude if negative else magnitude
    bits, _overflowed = _float32_bits(result)
    return Const(bits), False


def _compare_flags_float(
    left: Value, right: Value, label: str
) -> tuple[Value, Optional[bool]]:
    """comp(FX, FY) (PRM Table 18-5 opcode 0x8A, p.426; PGR p.11-29).

    Same bit-0 (AZ)/bit-2 (AN)/bit-31 (new CACC MSB) value encoding
    ``_compare_flags`` uses for the fixed-point comp/compu, consumed by
    ``_astatx_compare``'s CACC shift-register logic. An unordered (NAN)
    compare sets none of those bits (PGR doesn't document AZ/AN/CACC firing
    on an unordered compare) and instead reports the invalid flag, which
    the caller applies on top via ``_astatx_compare``'s AI override.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(label), None
    if math.isnan(a) or math.isnan(b):
        return Const(0), True
    return (
        Const(
            (0x1 if a == b else 0)
            | (0x4 if a < b else 0)
            | (0x80000000 if a > b else 0)
        ),
        False,
    )


def _bits_to_updates(mask: int, bits: Optional[int]) -> Dict[int, Optional[bool]]:
    """Expand an optional MASK-shaped flag bit pattern into an
    ``_astatx_apply_bits()`` updates dict; BITS=None forgets every bit in
    MASK."""
    updates: Dict[int, Optional[bool]] = {}
    bit = 0
    while (1 << bit) <= mask:
        if mask & (1 << bit):
            updates[bit] = None if bits is None else bool(bits & (1 << bit))
        bit += 1
    return updates


def _or_updates(
    a: Dict[int, Optional[bool]], b: Dict[int, Optional[bool]]
) -> Dict[int, Optional[bool]]:
    """Kleene-OR two ASTATX update dicts bit by bit (PRM p.3-21/3-22:
    "Multifunction Computations ... in the dual add/subtract computation,
    the ALU flags from the two operations are ORed together"). True beats
    anything; a bit present in only one dict keeps that dict's own value."""
    merged = dict(a)
    for bit, b_value in b.items():
        a_value = merged.get(bit, False)
        if a_value is True or b_value is True:
            merged[bit] = True
        elif a_value is None or b_value is None:
            merged[bit] = None
        else:
            merged[bit] = False
    return merged


def _alu_arith_updates(a: Value, b: Value, subtract: bool) -> Dict[int, Optional[bool]]:
    """Dict-returning counterpart of ``_astatx_alu_arith`` (PRM pp.439-440,
    446-447), for callers -- the fixed-point dual add/subtract -- that need
    to OR two such results together before applying either to ASTATX."""
    if isinstance(a, Const) and isinstance(b, Const):
        return _bits_to_updates(ALU_FLAGS_MASK, _arith_flag_bits(a, b, subtract))
    return _bits_to_updates(ALU_FLAGS_MASK, None)


def _float_alu_updates(
    result: Value,
    *,
    av: Optional[bool] = False,
    an_zero: bool = False,
    as_source: Optional[Value] = None,
    ai: Optional[bool] = None,
) -> Dict[int, Optional[bool]]:
    """ASTATX update dict shared by the float ALU ops (PRM Table 3-3,
    pp.3-8/3-9; per-op PGR pages cited at each call site).

    AC is always 0 and AF is always 1 for a float ALU result. AZ/AN come
    from RESULT's bit pattern (both +0.0 and -0.0 count as AZ) unless the
    op's AN column is architecturally fixed to 0 (the abs-family:
    AN_ZERO=True). AS is 0 unless AS_SOURCE is given (FN=abs FX carries the
    *input*'s sign, PGR p.11-31). AV/AI are per-op data: pass the
    (overflowed, invalid) pair ``_float_binary``/``_float_unary`` computed,
    or an explicit fixed value for an op the table/PGR documents as always
    0 (e.g. FN=float RX's AV and AI).
    """
    updates: Dict[int, Optional[bool]] = {
        AC_BIT: False,
        AF_BIT: True,
        AV_BIT: av,
        AI_BIT: ai,
        AS_BIT: False if as_source is None else _astatx_known_bit(as_source, 31),
    }
    if isinstance(result, Const):
        bits = result.value
        updates[AZ_BIT] = (bits & 0x7FFFFFFF) == 0
        updates[AN_BIT] = False if an_zero else bool(bits & 0x80000000)
    else:
        updates[AZ_BIT] = None
        updates[AN_BIT] = False if an_zero else None
    return updates


def _astatx_from_updates(updates: Dict[int, Optional[bool]]) -> "Callable[[Value], Value]":
    """Wrap a pre-built updates dict as an ASTATX updater function, matching
    the ``Callable[[Value], Value]`` contract every other compute-table
    branch returns."""
    return lambda astatx: _astatx_apply_bits(astatx, updates)


def _astatx_compare_float(value: Value, invalid: Optional[bool]) -> "Callable[[Value], Value]":
    """Float comp (PRM Table 3-3 AI='*'; PGR p.11-29 spells it out: "Set if
    either of the input operands is a NAN"). Identical to
    ``_astatx_compare``'s AC/AV/AS-clear, AZ/AN/CACC-from-VALUE and
    CACC-shift behaviour (which needs the *old* ASTATX, so it is reused
    rather than duplicated); only AI and AF differ from the fixed-point
    comp/compu version, which the PRM documents as always 0/0 rather than
    float compare's AI=data-dependent, AF=1.
    """

    def update(astatx: Value) -> Value:
        base = _astatx_compare(value)(astatx)
        return _astatx_apply_bits(base, {AI_BIT: invalid, AF_BIT: True})

    return update


def _compute(
    f: Mapping[str, int],
    short: bool,
    values: Mapping[int, Value],
    special: Optional[Mapping[str, Value]] = None,
) -> Optional[tuple[int | str, Value, str, "Callable[[Value], Value]"]]:
    """Decode the small public-table subset, reading every operand from VALUES.

    The 4th element of a non-None result is an ASTATX updater: a function
    from the old ASTATX Value to the new one, computed here (where the
    operands are in scope) and applied by ``_apply_compute``.
    """
    field = (
        _field(f, "compute")
        if short
        else ((_field(f, "compute[22:16]") << 16) | _field(f, "compute[15:0]"))
    )
    if not short and field == 0:
        return None
    # PRM Table 18-29: fixed bits 22:17=100000 select an MR data move.
    # The target-guided SPORT setup path uses the register-to-MR direction.
    if not short and field >> 17 == 0b100000:
        direction = (field >> 16) & 1
        opcode = (field >> 12) & 0xF
        rn = (field >> 8) & 0xF
        if direction != 1 or opcode != 0:
            raise ValueError("unsupported MR data move %#x" % field)
        return "MR0F", _ureg(values, rn), "mr-data-move", _astatx_mult_clear
    # PRM multiplier compute table: MRF = MRF + RX * RY (MOD1).  Preserve
    # the accumulator separately from the UREG file so later MR transfers do
    # not masquerade as architectural UREGs.
    if not short and ((field >> 20) & 3) == 1 and ((field >> 12) & 0xFF) == 0xB4:
        rx, ry = (field >> 4) & 0xF, field & 0xF
        accumulator = (special or {}).get("MRF", Unknown("uninitialized MRF"))
        product = _multiply(
            _ureg(values, rx), _ureg(values, ry), "R%d * R%d" % (rx, ry)
        )
        return (
            "MRF",
            _add(accumulator, product, "MRF + R%d * R%d" % (rx, ry)),
            "multiply-accumulate",
            _astatx_mult_forget,
        )
    if not short and ((field >> 20) & 3) == 1 and ((field >> 12) & 0xFF) == 0xB0:
        rn, rx, ry = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
        accumulator = (special or {}).get("MRF", Unknown("uninitialized MRF"))
        product = _multiply(
            _ureg(values, rx), _ureg(values, ry), "R%d * R%d" % (rx, ry)
        )
        return (
            rn,
            _add(accumulator, product, "MRF + R%d * R%d" % (rx, ry)),
            "multiply-add-mrf",
            _astatx_mult_forget,
        )
    if short:
        opcode, rn, rx = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
        left, right = _ureg(values, rn), _ureg(values, rx)
        # Each entry is (name, calculate, astatx_kind): astatx_kind is None
        # for the value-only logical rule (pass/not/and/or/xor), an
        # (a, b, subtract) triple for the arithmetic-flags rule, or "mult"
        # to forget the (unmodelled) multiplier flags.
        operations = {
            0: ("add", lambda: _add(left, right, "R%d + R%d" % (rn, rx)), (left, right, False)),
            1: (
                "subtract",
                lambda: _subtract(left, right, "R%d - R%d" % (rn, rx)),
                (left, right, True),
            ),
            2: ("pass", lambda: right, None),
            4: (
                "not",
                lambda: _not(right, "not R%d" % rx),
                None,
            ),
            5: ("increment", lambda: _add(right, Const(1), "R%d + 1" % rx), (right, Const(1), False)),
            6: ("decrement", lambda: _add(right, Const(-1), "R%d - 1" % rx), (right, Const(1), True)),
            7: (
                "multiply",
                lambda: _multiply(left, right, "R%d * R%d" % (rn, rx)),
                "mult",
            ),
            0xC: (
                "and",
                lambda: _bitwise(
                    left, right, "R%d and R%d" % (rn, rx), lambda a, b: a & b
                ),
                None,
            ),
            0xD: (
                "or",
                lambda: _bitwise(
                    left, right, "R%d or R%d" % (rn, rx), lambda a, b: a | b
                ),
                None,
            ),
            0xE: (
                "xor",
                lambda: _bitwise(
                    left, right, "R%d xor R%d" % (rn, rx), lambda a, b: a ^ b
                ),
                None,
            ),
        }
        if opcode == 3:
            # PRM ShortCompute table (p. 17-3): 0011 is the signed comp(RN, RX).
            value = _compare_flags(left, right, True, "comp R%d, R%d" % (rn, rx))
            return rn, value, "compare", _astatx_compare(value)
        # PRM Table 18-2 (p.423-425)/PGR "Short Compute Opcodes"
        # (pgr.txt:23108-23120): 1000-1011 and 1111 are the float
        # ShortCompute rows -- the same ops as the full-compute float table
        # above, just the compact 16-bit Type 2c encoding where RN doubles
        # as both the Y input and the result (Table 18-22: "RN = RN op RX").
        if opcode == 0x8:
            value, overflow, invalid = _float_binary(
                left, right, "F%d + F%d" % (rn, rx), lambda a, b: a + b
            )
            return rn, value, "float-add", _astatx_from_updates(
                _float_alu_updates(value, av=overflow, ai=invalid)
            )
        if opcode == 0x9:
            value, overflow, invalid = _float_binary(
                left, right, "F%d - F%d" % (rn, rx), lambda a, b: a - b
            )
            return rn, value, "float-subtract", _astatx_from_updates(
                _float_alu_updates(value, av=overflow, ai=invalid)
            )
        if opcode == 0xA:
            # FN = float RX: unlike the other short float rows, RN is not
            # read as an input here (only RX is converted); RN is purely the
            # destination.
            value, invalid = _fixed_to_float(right, "float R%d" % rx)
            return rn, value, "float-convert", _astatx_from_updates(
                _float_alu_updates(value, av=False, ai=invalid)
            )
        if opcode == 0xB:
            label = "comp F%d, F%d" % (rn, rx)
            value, invalid = _compare_flags_float(left, right, label)
            return rn, value, "float-compare", _astatx_compare_float(value, invalid)
        if opcode == 0xF:
            a, b = _float32(left), _float32(right)
            if a is None or b is None:
                value = Unknown("F%d * F%d" % (rn, rx))
            else:
                bits, _overflowed = _float32_bits(a * b)
                value = Const(bits)
            return rn, value, "float-multiply", _astatx_mult_forget
        if opcode not in operations:
            raise ValueError("unsupported short compute opcode %#x" % opcode)
        operation, calculate, astatx_kind = operations[opcode]
        value = calculate()
        if astatx_kind is None:
            astatx_update = _astatx_alu_logical(value)
        elif astatx_kind == "mult":
            astatx_update = _astatx_mult_forget
        else:
            a, b, subtract = astatx_kind
            astatx_update = _astatx_alu_arith(a, b, subtract)
        return rn, value, operation, astatx_update
    # PRM Table 18-1/Figure 18-1 (p.423): bit22 is MF, the multifunction
    # selector. A multifunction op's register sub-fields (PRM Table
    # 18-15..18-19, p.434-435) do not line up with the SINGLEFN rn/rx/ry
    # layout computed below, so it is decoded separately and always
    # returns/raises before falling through to that layout.
    mf = (field >> 22) & 1
    if mf:
        category = (field >> 16) & 0x3F
        rm, ra = (field >> 12) & 0xF, (field >> 8) & 0xF
        # PRM Table 18-16/18-17 (p.434): the four multifunction INPUT
        # operands are 2-bit fields, each selecting within a fixed quad --
        # Fxm in F0-3, Fym in F4-7, Fxa in F8-11, Fya in F12-15.
        rxm_reg = (field >> 6) & 0x3
        rym_reg = 4 + ((field >> 4) & 0x3)
        rxa_reg = 8 + ((field >> 2) & 0x3)
        rya_reg = 12 + (field & 0x3)
        fxm, fym = _ureg(values, rxm_reg), _ureg(values, rym_reg)
        fxa, fya = _ureg(values, rxa_reg), _ureg(values, rya_reg)
        # PGR Table 12-12 (pgr.txt:23129-23198), opcode[21:16] 011000/011001:
        # FM = FXM*FYM, FA = FXA+-FYA -- the only MUL/ALU multifunction rows
        # this firmware's audio code uses. Flags follow the single-function
        # rule for each half (PRM p.3-21/3-22: multifunction "handle[s]
        # flags in the same way as the single function computations" except
        # for dual add/subtract), so the ALU half reuses
        # ``_float_alu_updates`` and the multiplier half stays forgotten via
        # ``_astatx_mult_forget`` exactly as the plain float multiply below.
        if category in (0x18, 0x19, 0x1A, 0x1E, 0x1F):
            # The multiplier half uses ordinary IEEE NaN propagation like
            # the plain float multiply below, not the ALU's NaN-input
            # all-1s quirk, so it is computed directly rather than through
            # ``_float_binary``. Shared by every multifunction category
            # below: only the ALU half's operation differs.
            fm_a, fm_b = _float32(fxm), _float32(fym)
            if fm_a is None or fm_b is None:
                fm_value: Value = Unknown("F%d * F%d" % (rxm_reg, rym_reg))
            else:
                fm_bits, _fm_overflowed = _float32_bits(fm_a * fm_b)
                fm_value = Const(fm_bits)

            def with_mult(fa_updates, operation):
                def astatx_update(astatx: Value, updates=fa_updates) -> Value:
                    return _astatx_mult_forget(_astatx_apply_bits(astatx, updates))

                return (rm, ra), (fm_value, fa_value), operation, astatx_update

            if category in (0x18, 0x19):
                subtract = category == 0x19
                fa_op = (lambda a, b: a - b) if subtract else (lambda a, b: a + b)
                fa_value, fa_overflow, fa_invalid = _float_binary(
                    fxa,
                    fya,
                    "F%d %s F%d" % (rxa_reg, "-" if subtract else "+", rya_reg),
                    fa_op,
                )
                return with_mult(
                    _float_alu_updates(fa_value, av=fa_overflow, ai=fa_invalid),
                    "float-mulalu-subtract" if subtract else "float-mulalu-add",
                )
            # PGR Table 12-12 opcode 011010 (p.587): FM = FXM*FYM,
            # FA = FLOAT RXA by RYA -- the ALU half is the scaled
            # fixed->float convert (0xDA above), reading RXA/RYA as fixed
            # rather than float registers (same physical register file
            # slots, ``fxa``/``fya`` above are just the raw bit patterns).
            if category == 0x1A:
                fa_value, fa_overflow = _fixed_to_float_scaled(
                    fxa, fya, "float R%d by R%d" % (rxa_reg, rya_reg)
                )
                return with_mult(
                    _float_alu_updates(fa_value, av=fa_overflow, ai=False),
                    "float-mulalu-convert",
                )
            # PGR Table 12-12 opcodes 011110/011111 (p.588): FM = FXM*FYM,
            # FA = MAX/MIN(FXA, FYA) -- the ALU half is the same
            # single-function float min/max as 0xE1/0xE2 above (AV fixed 0
            # there too).
            if category in (0x1E, 0x1F):
                minimum = category == 0x1F
                name = "min" if minimum else "max"
                combine = _float_min if minimum else _float_max
                fa_value, fa_overflow, fa_invalid = _float_binary(
                    fxa, fya, "%s(F%d, F%d)" % (name, rxa_reg, rya_reg), combine
                )
                return with_mult(
                    _float_alu_updates(fa_value, av=False, ai=fa_invalid),
                    "float-mul" + name,
                )
        raise ValueError(
            "unsupported multifunction category=%#04x rm=%d ra=%d" % (category, rm, ra)
        )
    cu, opcode = (field >> 20) & 3, (field >> 12) & 0xFF
    rn, rx, ry = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
    left, right = _ureg(values, rx), _ureg(values, ry)
    # PRM Table 18-10 (p.433) / Table 18-13 (p.434): Dual Add/Subtract is a
    # single-function ALU op (mf=0, not multifunction) whose opcode top
    # nibble (bits 19:16, i.e. this OPCODE's top nibble) is 0111 (fixed) or
    # 1111 (float); the low nibble (bits 15:12) is not part of the opcode at
    # all -- it is RS, a second 4-bit result register alongside
    # RA=RN/FN at bits 11:8 (already read above as RN).
    if cu == 0 and (opcode >> 4) in (0x7, 0xF):
        float_form = (opcode >> 4) == 0xF
        rs = opcode & 0xF
        if float_form:
            add_value, add_overflow, add_invalid = _float_binary(
                left, right, "F%d + F%d" % (rx, ry), lambda a, b: a + b
            )
            sub_value, sub_overflow, sub_invalid = _float_binary(
                left, right, "F%d - F%d" % (rx, ry), lambda a, b: a - b
            )
            updates = _or_updates(
                _float_alu_updates(add_value, av=add_overflow, ai=add_invalid),
                _float_alu_updates(sub_value, av=sub_overflow, ai=sub_invalid),
            )
            operation = "float-dual-add-subtract"
        else:
            add_value = _add(left, right, "R%d + R%d" % (rx, ry))
            sub_value = _subtract(left, right, "R%d - R%d" % (rx, ry))
            updates = _or_updates(
                _alu_arith_updates(left, right, False),
                _alu_arith_updates(left, right, True),
            )
            operation = "dual-add-subtract"
        return (rn, rs), (add_value, sub_value), operation, _astatx_from_updates(updates)
    # PRM Table 17-5: ALUOP 00000001/00000010 are add/subtract.
    if cu == 0 and opcode == 0x01:
        value = _add(left, right, "R%d + R%d" % (rx, ry))
        return rn, value, "add", _astatx_alu_arith(left, right, False)
    if cu == 0 and opcode == 0x02:
        value = _subtract(left, right, "R%d - R%d" % (rx, ry))
        return rn, value, "subtract", _astatx_alu_arith(left, right, True)
    # PRM p.438-439 / PGR Table 12-3 (p.573): ALUOP 00000101 is
    # RN = RX + RY + ci (add with carry) and 00000110 is RN = RX - RY + ci -
    # 1 (subtract with borrow), both using ASTATX's AC bit as an explicit
    # carry-in read before this instruction's own flags are written. The
    # identity RX - RY + ci - 1 = RX + ~RY + ci (see ``_arith_flag_bits_ci``)
    # lets both share one value formula: add RY (or its one's complement for
    # subtract) plus a constant offset of ci (add) or ci-1 (subtract).
    if cu == 0 and opcode in (0x05, 0x06):
        subtract = opcode == 0x06
        astatx = _ureg_raw(values, UREG_CODES["ASTATX"])
        carry_in = _astatx_known_bit(astatx, AC_BIT)
        label = "R%d %s R%d + ci%s" % (rx, "-" if subtract else "+", ry, " - 1" if subtract else "")
        if carry_in is None:
            value = Unknown(label)
        else:
            base = _subtract(left, right, label) if subtract else _add(left, right, label)
            offset = (1 if carry_in else 0) - (1 if subtract else 0)
            value = _add(base, Const(offset), label)
        operation = "subtract-with-borrow" if subtract else "add-with-carry"
        return rn, value, operation, _astatx_alu_arith_ci(left, right, subtract, carry_in)
    # PRM Table 18-5: ALUOP 00001010 is signed comp(RX, RY) and 00001011 is
    # unsigned compu(RX, RY). Both update status only, so the tracer records
    # the comparison without writing RN; the value carries the new flags.
    if cu == 0 and opcode in (0x0A, 0x0B):
        signed = opcode == 0x0A
        label = "%s R%d, R%d" % ("comp" if signed else "compu", rx, ry)
        value = _compare_flags(left, right, signed, label)
        return rn, value, "compare", _astatx_compare(value)
    if cu == 0 and opcode == 0x21:
        return rn, left, "pass", _astatx_alu_logical(left)
    # PRM Table 18-5 and p. 19-10: ALUOP 00100010 is RN = -RX, the two's
    # complement, with the same flags as 0 - RX.
    if cu == 0 and opcode == 0x22:
        value = _subtract(Const(0), left, "-R%d" % rx)
        return rn, value, "negate", _astatx_alu_arith(Const(0), left, True)
    if cu == 0 and opcode == 0x29:
        value = _add(left, Const(1), "R%d + 1" % rx)
        return rn, value, "increment", _astatx_alu_arith(left, Const(1), False)
    # PRM Table 18-5 and p. 19-9: ALUOP 00101010 is RN = RX - 1.
    if cu == 0 and opcode == 0x2A:
        value = _add(left, Const(-1), "R%d - 1" % rx)
        return rn, value, "decrement", _astatx_alu_arith(left, Const(1), True)
    # PRM Table 18-5: ALUOP 01000000..01000010 are the integer logical
    # operations AND, OR, and XOR.
    if cu == 0 and opcode in (0x40, 0x41, 0x42):
        name, operation = {
            0x40: ("and", lambda a, b: a & b),
            0x41: ("or", lambda a, b: a | b),
            0x42: ("xor", lambda a, b: a ^ b),
        }[opcode]
        value = _bitwise(
            left,
            right,
            "R%d %s R%d" % (rx, name, ry),
            operation,
        )
        return rn, value, name, _astatx_alu_logical(value)
    # PRM Table 18-5 (p.425-427), float rows; per-op flags cited at each
    # branch (PRM Table 3-3, pp.3-8/3-9, cross-checked against the classic
    # PGR's per-instruction pages, which spell out AZ/AN/AV/AI exactly where
    # the SHARC+ PRM only marks a column "*"/data-dependent).
    if cu == 0 and opcode == 0x81:
        value, overflow, invalid = _float_binary(
            left, right, "F%d + F%d" % (rx, ry), lambda a, b: a + b
        )
        return rn, value, "float-add", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    if cu == 0 and opcode == 0x82:
        value, overflow, invalid = _float_binary(
            left, right, "F%d - F%d" % (rx, ry), lambda a, b: a - b
        )
        return rn, value, "float-subtract", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PGR p.11-29: comp(Fx, Fy).
    if cu == 0 and opcode == 0x8A:
        label = "comp F%d, F%d" % (rx, ry)
        value, invalid = _compare_flags_float(left, right, label)
        return rn, value, "float-compare", _astatx_compare_float(value, invalid)
    # PGR p.11-32: Fn = pass Fx.
    if cu == 0 and opcode == 0xA1:
        value, overflow, invalid = _float_unary(left, "pass F%d" % rx, lambda a: a)
        return rn, value, "float-pass", _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PGR p.11-30: Fn = -Fx.
    if cu == 0 and opcode == 0xA2:
        value, overflow, invalid = _float_unary(left, "-F%d" % rx, lambda a: -a)
        return rn, value, "float-negate", _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PGR p.11-34/11-35: Rn = mant Fx. Bespoke flag dict, not
    # ``_float_alu_updates``: the result is an unsigned-magnitude fixed
    # word (no sign bit of its own to derive AZ/AN from), AN is
    # architecturally fixed 0 (PRM Table 3-3), and AS/AV/AI come from the
    # *input*'s sign/infinity/NAN rather than the result.
    if cu == 0 and opcode == 0xAD:
        value, overflow, negative, invalid = _float_mantissa(left, "mant F%d" % rx)
        updates = {
            AC_BIT: False,
            AF_BIT: True,
            AN_BIT: False,
            AV_BIT: overflow,
            AS_BIT: negative,
            AI_BIT: invalid,
            AZ_BIT: (value.value == 0) if isinstance(value, Const) else None,
        }
        return rn, value, "mant", _astatx_from_updates(updates)
    # PGR p.11-31: Fn = abs Fx. AN fixed 0; AS carries the *input*'s sign.
    if cu == 0 and opcode == 0xB0:
        value, overflow, invalid = _float_unary(left, "abs F%d" % rx, abs)
        return rn, value, "float-abs", _astatx_from_updates(
            _float_alu_updates(value, av=False, an_zero=True, as_source=left, ai=invalid)
        )
    # PGR p.11-33: Fn = scalb Fx by Ry. Unlike abs/pass/etc., AN here
    # follows the *result*'s sign (PRM Table 3-3 marks AN '*', not 0), so
    # this reuses ``_float_alu_updates``'s default (as_source=None,
    # an_zero=False) rather than the abs-style override.
    if cu == 0 and opcode == 0xBD:
        value, overflow, invalid = _float_scalb(
            left, right, "scalb F%d by R%d" % (rx, ry)
        )
        return rn, value, "float-scalb", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PGR p.11-20/11-21: Rn = min/max(Rx, Ry) -- fixed-point, not the
    # float min/max at 0xE1/0xE2 below. AV/AC/AS/AI/AF are all fixed 0
    # (PRM Table 3-2, "AF Flag = 0"); only AZ/AN follow the chosen
    # operand, the same rule ``_astatx_alu_logical`` already implements
    # for pass/not/and/or/xor.
    if cu == 0 and opcode in (0x61, 0x62):
        name = "min" if opcode == 0x61 else "max"
        if isinstance(left, Const) and isinstance(right, Const):
            a, b = _signed32(left.value), _signed32(right.value)
            pick_left = (a <= b) if name == "min" else (a >= b)
            value = left if pick_left else right
        else:
            value = Unknown("%s(R%d, R%d)" % (name, rx, ry))
        return rn, value, name, _astatx_alu_logical(value)
    # PRM p.19-19, opcode 1110 0000 / PGR Table 12-4 p.574, same opcode: Fn =
    # Fx copysign Fy.
    if cu == 0 and opcode == 0xE0:
        value, invalid = _float_copysign(left, right, "F%d copysign F%d" % (rx, ry))
        return rn, value, "float-copysign", _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PGR p.11-46/11-47: Fn = min/max(Fx, Fy).
    if cu == 0 and opcode in (0xE1, 0xE2):
        name = "min" if opcode == 0xE1 else "max"
        combine = _float_min if opcode == 0xE1 else _float_max
        value, overflow, invalid = _float_binary(
            left, right, "%s(F%d, F%d)" % (name, rx, ry), combine
        )
        return rn, value, "float-" + name, _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PGR p.11-48 / PRM p.3-6: Fn = clip Fx by Fy.
    if cu == 0 and opcode == 0xE3:
        value, overflow, invalid = _float_binary(
            left, right, "clip F%d by F%d" % (rx, ry), _float_clip
        )
        return rn, value, "float-clip", _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PRM p.427/PGR p.11-39 "without scaling factor": Fn = float Rx.
    if cu == 0 and opcode == 0xCA:
        value, invalid = _fixed_to_float(left, "float R%d" % rx)
        return rn, value, "float-convert", _astatx_from_updates(
            _float_alu_updates(value, av=False, ai=invalid)
        )
    # PGR p.11-39 "with scaling factor" / PRM p.19-.. : Fn = float Rx by Ry.
    # AV is data-dependent here (unlike the unscaled form above, where an
    # int32 input can never overflow float32 range), per PGR Table 3-3.
    if cu == 0 and opcode == 0xDA:
        value, overflow = _fixed_to_float_scaled(
            left, right, "float R%d by R%d" % (rx, ry)
        )
        return rn, value, "float-convert-scaled", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=False)
        )
    # PRM p.24-.. / PGR p.11-36..11-38, opcode 1100 1001: Rn = fix Fx
    # (rounds to nearest or truncates per MODE1.TRUNCATE; see
    # ``_float_to_fixed``).
    if cu == 0 and opcode == 0xC9:
        mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
        value, overflow, invalid = _float_to_fixed(left, mode1, False, "fix F%d" % rx)
        return rn, value, "fix", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PRM p.427/PGR p.11-37: Rn = trunc Fx.
    if cu == 0 and opcode == 0xCD:
        mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
        value, overflow, invalid = _float_to_fixed_trunc(left, mode1, "trunc F%d" % rx)
        return rn, value, "trunc", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PGR p.11-36..11-38, opcode 1101 1001: Rn = fix Fx by Ry -- Ry's
    # exponent-add (``_scale_fixed_input``) applied before the same fix
    # conversion as 0xC9; PGR Table 3-3 gives it the identical flag columns
    # as the unscaled form.
    if cu == 0 and opcode == 0xD9:
        mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
        scaled = _scale_fixed_input(left, right, "F%d * 2**R%d" % (rx, ry))
        value, overflow, invalid = _float_to_fixed(
            scaled, mode1, False, "fix F%d by R%d" % (rx, ry)
        )
        return rn, value, "fix-scaled", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PGR p.11-36..11-38, opcode 1101 1101: Rn = trunc Fx by Ry -- Ry's
    # exponent-add applied before the same truncation as 0xCD; PGR Table 3-3
    # again gives it the unscaled form's flag columns.
    if cu == 0 and opcode == 0xDD:
        mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
        scaled = _scale_fixed_input(left, right, "F%d * 2**R%d" % (rx, ry))
        value, overflow, invalid = _float_to_fixed(
            scaled, mode1, True, "trunc F%d by R%d" % (rx, ry)
        )
        return rn, value, "trunc-scaled", _astatx_from_updates(
            _float_alu_updates(value, av=overflow, ai=invalid)
        )
    # PRM p.427 / PGR p.11-44/11-45: Fn = recips/rsqrts Fx -- iterative
    # reciprocal/reciprocal-sqrt seed instructions. The seed mantissa comes
    # from an ROM lookup table the public manuals do not print, so this
    # tracer decodes the instruction (unblocking whatever reads its flags or
    # continues past it) without claiming a numeric seed value it cannot
    # verify; AV/AI are genuinely data-dependent here and left unknown too.
    if cu == 0 and opcode in (0xC4, 0xC5):
        name = "recips" if opcode == 0xC4 else "rsqrts"
        label = "%s F%d (iterative seed, not numerically modeled)" % (name, rx)
        return rn, Unknown(label), "float-" + name + "-seed", _astatx_from_updates(
            _float_alu_updates(Unknown(label), av=None, ai=None)
        )
    # PRM Table 17-7: MULOP 0000 F00x writes a saturated MRF value to RN.
    # The tracer does not model the full-width multiplier accumulator or MOD2
    # format bits, so preserve the documented data dependency conservatively.
    if cu == 1 and opcode == 0x00:
        return rn, Unknown("saturated MRF (unmodeled MOD2)"), "saturate-mrf", _astatx_mult_forget
    if cu == 1 and opcode == 0x70:
        value = _multiply(left, right, "R%d * R%d" % (rx, ry))
        return rn, value, "multiply", _astatx_mult_forget
    # PRM Table 18-7 (p.428-429): MULOP 00110000 is Fn = Fx * Fy. Flags are
    # the multiplier's MN/MV/MU/MI (PGR p.11-57), the same unmodeled-result
    # group the fixed-point multiply above forgets via
    # ``_astatx_mult_forget``; unlike the ALU's NaN-input quirk, the PGR
    # text for this op does not document an all-1s override, so ordinary
    # IEEE NaN propagation applies.
    if cu == 1 and opcode == 0x30:
        a, b = _float32(left), _float32(right)
        if a is None or b is None:
            value = Unknown("F%d * F%d" % (rx, ry))
        else:
            bits, _overflowed = _float32_bits(a * b)
            value = Const(bits)
        return rn, value, "float-multiply", _astatx_mult_forget
    # Shifter opcode 1011 0000: absent from both public sources' shifter
    # tables (PRM Table 17-9, p.17-10/17-11, and PGR Table 12-11, p.580-581,
    # transcribed in full -- neither lists any 0xA0-0xBF row). Seen at
    # `sw 0x1cd002`. Rather than guess an operation from an undocumented
    # opcode, decode it (so the walk does not desync) and leave both the
    # result and its flags Unknown, per this file's existing rule for gaps
    # the manuals do not cover.
    if cu == 2 and opcode == 0xB0:
        label = "shift opcode 0xb0 R%d, R%d (undocumented; no public source)" % (rx, ry)
        return rn, Unknown(label), "shift-undocumented-b0", lambda astatx: _astatx_forget(
            astatx, ALU_FLAGS_MASK
        )
    # PRM Table 17-9: SHIFTOP 00000000 is RN = LSHIFT RX by RY. The signed
    # low byte of RY selects a left (positive) or logical right (negative)
    # shift; magnitudes of 32 or more produce zero.
    if cu == 2 and opcode == 0x00:
        amount: Optional[int] = None
        if not isinstance(right, Const):
            value = Unknown("lshift R%d by R%d" % (rx, ry))
        else:
            amount = _signed(right.value & 0xFF, 8)
            if amount == 0:
                value = left
            elif not isinstance(left, Const):
                value = Unknown("lshift R%d by %d" % (rx, amount))
            elif amount >= 32 or amount <= -32:
                value = Const(0)
            elif amount > 0:
                value = Const(left.value << amount)
            else:
                value = Const(left.value >> -amount)
        return rn, value, "logical-shift", _astatx_shift(amount, value, "clear")
    # PRM Table 17-9 p.17-10 (out/refs/sharc-plus-prm/all.txt line 22765):
    # SHIFTOP 00000100 is RN = ASHIFT RX by RY -- the register-operand twin
    # of the already-implemented ShiftImm opcode 0x01 (arithmetic shift by
    # an 8-bit immediate). Same signed-low-byte amount, same 32-magnitude
    # saturation, differing only in that a right shift (negative amount) is
    # sign-extending, mirroring _shift_immediate's opcode==0x01 branch
    # exactly.
    # CALIBRATION NOTE: added in a scratch copy of sharc_trace.py for this
    # task only; not part of the tracked tool.
    if cu == 2 and opcode == 0x04:
        amount = None
        if not isinstance(right, Const):
            value = Unknown("ashift R%d by R%d" % (rx, ry))
        else:
            amount = _signed(right.value & 0xFF, 8)
            if amount == 0:
                value = left
            elif not isinstance(left, Const):
                value = Unknown("ashift R%d by %d" % (rx, amount))
            elif amount >= 32:
                value = Const(0)
            elif amount <= -32:
                value = (
                    Const(0xFFFFFFFF)
                    if left.value & 0x80000000
                    else Const(0)
                )
            elif amount > 0:
                value = Const(left.value << amount)
            else:
                value = Const(_signed32(left.value) >> -amount)
        return rn, value, "arithmetic-shift", _astatx_shift(amount, value, "clear")
    # PRM Table 17-9: SHIFTOP 10001000 is RN = leftz RX.
    if cu == 2 and opcode == 0x88:
        value = (
            Const(32 if left.value == 0 else 32 - left.value.bit_length())
            if isinstance(left, Const)
            else Unknown("leftz R%d" % rx)
        )
        return rn, value, "leftz", _astatx_leftz(left, value)
    # PRM Table 18-9: SHIFTOP 11000000/11000100 are variable bit set/clear.
    if cu == 2 and opcode in (0xC0, 0xC4):
        name = "bset" if opcode == 0xC0 else "bclr"
        if not isinstance(right, Const):
            value = Unknown("%s R%d by R%d" % (name, rx, ry))
        elif right.value > 31:
            value = left
        else:
            calculate = (
                (lambda a, b: a | b) if opcode == 0xC0 else (lambda a, b: a & ~b)
            )
            value = _bitwise(
                left,
                Const(1 << right.value),
                "%s R%d by R%d" % (name, rx, ry),
                calculate,
            )
        return (
            rn,
            value,
            "bit-set" if opcode == 0xC0 else "bit-clear",
            _astatx_bit_field(right, value),
        )
    # PRM Table 17-9 and p. 23-5: SHIFTOP 11001000 is
    # RN = btgl RX by RY.  Positions outside the 32-bit field leave RX
    # unchanged.
    if cu == 2 and opcode == 0xC8:
        if not isinstance(right, Const):
            value = Unknown("btgl R%d by R%d" % (rx, ry))
        elif right.value > 31:
            value = left
        else:
            value = _bitwise(
                left,
                Const(1 << right.value),
                "btgl R%d by R%d" % (rx, ry),
                lambda a, b: a ^ b,
            )
        return rn, value, "bit-toggle", _astatx_bit_field(right, value)
    # PRM Table 18-9 and pp. 24-5--24-6: SHIFTOP 11001100 is
    # btst RX by RY. It changes status flags only and has no RN result.
    if cu == 2 and opcode == 0xCC:
        return rn, left, "bit-test", _astatx_btst(left, right)
    raise ValueError("unsupported full compute cu=%#x opcode=%#x" % (cu, opcode))


def _astatx_known_bit(value: Value, bit: int) -> Optional[bool]:
    """Return ASTATX/ASTATY bit BIT if known, else None."""
    if isinstance(value, Const):
        return bool(value.value & (1 << bit))
    if isinstance(value, PartialConst):
        if value.mask & (1 << bit):
            return bool(value.bits & (1 << bit))
        return None
    return None


def _astatx_define(old: Value, mask: int, bits: int) -> Value:
    """Return OLD with MASK's bits set definitively to BITS (masked to MASK);
    bits outside MASK keep whatever knowledge OLD already carried."""
    mask &= 0xFFFFFFFF
    bits &= mask
    if isinstance(old, Const):
        return Const((old.value & ~mask) | bits)
    if isinstance(old, PartialConst):
        new_mask = old.mask | mask
        new_bits = (old.bits & ~mask) | bits
        return Const(new_bits) if new_mask == 0xFFFFFFFF else PartialConst(new_mask, new_bits)
    # Unknown (or a stray non-ASTATX Value type): only MASK becomes known.
    if not mask:
        return old
    return Const(bits) if mask == 0xFFFFFFFF else PartialConst(mask, bits)


def _astatx_forget(old: Value, mask: int) -> Value:
    """Return OLD with MASK's bits downgraded to unknown; other bits keep
    whatever knowledge OLD already carried."""
    mask &= 0xFFFFFFFF
    if isinstance(old, Const):
        new_mask = 0xFFFFFFFF & ~mask
        new_bits = old.value & new_mask
    elif isinstance(old, PartialConst):
        new_mask = old.mask & ~mask
        new_bits = old.bits & new_mask
    else:
        return old
    return Unknown("astatx bits forgotten") if new_mask == 0 else PartialConst(new_mask, new_bits)


def _astatx_apply_bits(old: Value, updates: Mapping[int, Optional[bool]]) -> Value:
    """Apply per-bit updates to an ASTATX-like value: True/False defines that
    bit, None forgets it (downgrades to unknown). Bits not mentioned in
    UPDATES are left exactly as OLD had them."""
    define_mask = define_bits = forget_mask = 0
    for bit, known in updates.items():
        if known is None:
            forget_mask |= 1 << bit
        else:
            define_mask |= 1 << bit
            if known:
                define_bits |= 1 << bit
    result = old
    if define_mask:
        result = _astatx_define(result, define_mask, define_bits)
    if forget_mask:
        result = _astatx_forget(result, forget_mask)
    return result


def _alu_result_bits(value: Const) -> int:
    """AN/AZ for a pass/not/and/or/xor result (PRM pp.449-452); AC/AV/AS/AI
    are always 0 for these."""
    bits = 0
    if value.value & 0x80000000:
        bits |= 1 << AN_BIT
    if value.value == 0:
        bits |= 1 << AZ_BIT
    return bits


def _arith_flag_bits(a: Const, b: Const, subtract: bool) -> int:
    """AC/AV/AN/AZ for add/subtract/increment/decrement (PRM pp.439-440,
    446-447); AS/AI are always 0.

    AC is the carry out of the MSB adder stage; AV is the XOR of the carries
    into and out of the MSB adder stage (the standard two's-complement
    signed-overflow test). Subtraction is modelled the way the ALU does it:
    add the one's complement of B with a forced carry-in of 1 (so decrement,
    RX - 1, is add(RX, 1, subtract=True), matching the PRM wording exactly).
    """
    A = a.value & 0xFFFFFFFF
    if subtract:
        b_eff, carry_in = (~b.value) & 0xFFFFFFFF, 1
    else:
        b_eff, carry_in = b.value & 0xFFFFFFFF, 0
    low31 = (A & 0x7FFFFFFF) + (b_eff & 0x7FFFFFFF) + carry_in
    carry_into_msb = (low31 >> 31) & 1
    full = A + b_eff + carry_in
    carry_out = (full >> 32) & 1
    result = full & 0xFFFFFFFF
    bits = 0
    if carry_out:
        bits |= 1 << AC_BIT
    if carry_into_msb ^ carry_out:
        bits |= 1 << AV_BIT
    if result & 0x80000000:
        bits |= 1 << AN_BIT
    if result == 0:
        bits |= 1 << AZ_BIT
    return bits


def _astatx_alu_logical(value: Value) -> "Callable[[Value], Value]":
    """pass/not/and/or/xor: AC/AV/AS/AI/AF cleared; AN/AZ from VALUE."""

    def update(astatx: Value) -> Value:
        if isinstance(value, Const):
            return _astatx_define(astatx, ALU_FLAGS_MASK, _alu_result_bits(value))
        return _astatx_forget(astatx, ALU_FLAGS_MASK)

    return update


def _astatx_alu_arith(a: Value, b: Value, subtract: bool) -> "Callable[[Value], Value]":
    """add/subtract/increment/decrement: AC/AV/AN/AZ from A and B; AS/AI/AF
    cleared."""

    def update(astatx: Value) -> Value:
        if isinstance(a, Const) and isinstance(b, Const):
            return _astatx_define(astatx, ALU_FLAGS_MASK, _arith_flag_bits(a, b, subtract))
        return _astatx_forget(astatx, ALU_FLAGS_MASK)

    return update


def _arith_flag_bits_ci(a: Const, b: Const, subtract: bool, carry_in: bool) -> int:
    """AC/AV/AN/AZ for RN = RX+RY+ci / RN = RX-RY+ci-1 (PRM p.438-439; PGR
    Table 12-3 opcodes 0000 0101/0000 0110, p.573); AS/AI are always 0,
    matching plain add/subtract (PRM: "AS Cleared", "AI Cleared" for both).

    Same two's-complement adder model as ``_arith_flag_bits``, with the
    ASTATX AC bit supplied as an explicit carry-in. The identity RX - RY +
    ci - 1 = RX + ~RY + ci (two's complement: ~RY = -RY-1) means the
    subtract-with-borrow row needs no forced +1 of its own -- CI itself
    supplies the carry that ordinary subtract hard-codes to 1 -- so this
    reuses the exact same b_eff = ~B one's-complement substitution as
    ``_arith_flag_bits``, just with CI standing in for the fixed carry_in.
    """
    A = a.value & 0xFFFFFFFF
    b_eff = (~b.value if subtract else b.value) & 0xFFFFFFFF
    ci = 1 if carry_in else 0
    low31 = (A & 0x7FFFFFFF) + (b_eff & 0x7FFFFFFF) + ci
    carry_into_msb = (low31 >> 31) & 1
    full = A + b_eff + ci
    carry_out = (full >> 32) & 1
    result = full & 0xFFFFFFFF
    bits = 0
    if carry_out:
        bits |= 1 << AC_BIT
    if carry_into_msb ^ carry_out:
        bits |= 1 << AV_BIT
    if result & 0x80000000:
        bits |= 1 << AN_BIT
    if result == 0:
        bits |= 1 << AZ_BIT
    return bits


def _astatx_alu_arith_ci(
    a: Value, b: Value, subtract: bool, carry_in: Optional[bool]
) -> "Callable[[Value], Value]":
    """add-with-carry/subtract-with-borrow: AC/AV/AN/AZ from A, B and the
    ASTATX AC carry-in; AS/AI/AF cleared, same as ``_astatx_alu_arith``.
    Forgets the flags (rather than defining them) whenever the carry-in
    itself is unknown, not just when A or B is."""

    def update(astatx: Value) -> Value:
        if isinstance(a, Const) and isinstance(b, Const) and carry_in is not None:
            return _astatx_define(
                astatx, ALU_FLAGS_MASK, _arith_flag_bits_ci(a, b, subtract, carry_in)
            )
        return _astatx_forget(astatx, ALU_FLAGS_MASK)

    return update


def _astatx_compare(value: Value) -> "Callable[[Value], Value]":
    """PRM comp/compu: AC/AV/AS/AI/AF clear; AZ/AN from VALUE (bits 0, 2);
    CACC (bits 31:24) is an 8-bit shift register, newest bit (VALUE bit 31)
    entering at bit 31. The shift needs the old CACC bits, so it is only
    computed exactly when the old ASTATX is fully known; otherwise CACC
    becomes unknown while the other newly defined bits do not.
    """

    def update(astatx: Value) -> Value:
        if not isinstance(value, Const):
            return _astatx_forget(_astatx_forget(astatx, ALU_FLAGS_MASK), 0xFF000000)
        new_low = value.value & ((1 << AZ_BIT) | (1 << AN_BIT))
        if isinstance(astatx, Const):
            old = astatx.value
            cacc = (old >> 1) & 0x7F000000
            preserve = 0x00FFFFC0 & ~(1 << AF_BIT)  # bits 6-23 minus AF
            return Const((old & preserve) | cacc | new_low | (value.value & 0x80000000))
        result = _astatx_define(astatx, ALU_FLAGS_MASK, new_low)
        return _astatx_forget(result, 0xFF000000)

    return update


def _astatx_mult_forget(astatx: Value) -> Value:
    """multiply/multiply-add-mrf/saturate-mrf/multiply-accumulate: the
    tracer does not model the multiplier result format, so MN/MV/MU/MI are
    always unknown."""
    return _astatx_forget(astatx, MULT_FLAGS_MASK)


def _astatx_mult_clear(astatx: Value) -> Value:
    """mr-data-move: PRM p.493 documents MU/MN/MI/MV all cleared."""
    return _astatx_define(astatx, MULT_FLAGS_MASK, 0)


def _astatx_bit_field(position: Value | int, result: Value) -> "Callable[[Value], Value]":
    """bset/bclr/btgl reg and immediate (PRM pp.511-513): SS cleared; SZ =
    output == 0; SV = bit position > 31."""
    pos = position.value if isinstance(position, Const) else (
        position if isinstance(position, int) else None
    )

    def update(astatx: Value) -> Value:
        updates: Dict[int, Optional[bool]] = {SS_BIT: False}
        if pos is None:
            updates[SV_BIT] = None
            updates[SZ_BIT] = None
        else:
            updates[SV_BIT] = pos > 31
            updates[SZ_BIT] = (result.value == 0) if isinstance(result, Const) else None
        return _astatx_apply_bits(astatx, updates)

    return update


def _astatx_fext(span: int, result: Value) -> "Callable[[Value], Value]":
    """fext immediate (PRM pp.518-519): SS cleared; SZ = output == 0; SV =
    len6 + bit6 > 32. SPAN is len6+bit6, always known from the immediate."""

    def update(astatx: Value) -> Value:
        updates: Dict[int, Optional[bool]] = {
            SS_BIT: False,
            SV_BIT: span > 32,
            SZ_BIT: (result.value == 0) if isinstance(result, Const) else None,
        }
        return _astatx_apply_bits(astatx, updates)

    return update


def _astatx_leftz(source: Value, result: Value) -> "Callable[[Value], Value]":
    """leftz (PRM p.521): SS cleared; SZ = MSB of RX is 1; SV = result == 32."""

    def update(astatx: Value) -> Value:
        updates: Dict[int, Optional[bool]] = {SS_BIT: False}
        updates[SZ_BIT] = (
            bool(source.value & 0x80000000) if isinstance(source, Const) else None
        )
        updates[SV_BIT] = (result.value == 32) if isinstance(result, Const) else None
        return _astatx_apply_bits(astatx, updates)

    return update


def _astatx_btst(source: Value, position: Value) -> "Callable[[Value], Value]":
    """btst reg (PRM p.513): SS cleared; SZ set if the tested bit is 0 or the
    position is out of range, cleared if the tested bit is 1; SV = position >
    31. BTF is unaffected."""

    def update(astatx: Value) -> Value:
        updates: Dict[int, Optional[bool]] = {SS_BIT: False}
        if not isinstance(position, Const):
            updates[SV_BIT] = None
            updates[SZ_BIT] = None
        else:
            pos = position.value
            out_of_range = pos > 31
            updates[SV_BIT] = out_of_range
            if out_of_range:
                updates[SZ_BIT] = True
            elif isinstance(source, Const):
                updates[SZ_BIT] = not bool(source.value & (1 << pos))
            else:
                updates[SZ_BIT] = None
        return _astatx_apply_bits(astatx, updates)

    return update


def _astatx_shift(
    amount: Optional[int], shifted: Value, ss_mode: str
) -> "Callable[[Value], Value]":
    """lshift/ashift reg and immediate, OR-lshift/OR-ashift immediate (PRM
    pp.508-510): SZ = the shifted value (before any OR) is zero; SV = the
    shift amount is a left shift (> 0).

    SS is cleared for every one of these forms except OR-ashift, whose PRM
    entry omits an SS line entirely (unlike its OR-lshift sibling, which
    repeats "SS Cleared"); pass ss_mode="forget" there so SS becomes unknown
    instead of guessed, without touching any other already-known bit.
    """

    def update(astatx: Value) -> Value:
        updates: Dict[int, Optional[bool]] = {
            SS_BIT: False if ss_mode == "clear" else None,
            SV_BIT: None if amount is None else amount > 0,
            SZ_BIT: (shifted.value == 0) if isinstance(shifted, Const) else None,
        }
        return _astatx_apply_bits(astatx, updates)

    return update


def _apply_compute(
    state: State,
    insn: Instruction,
    result: tuple[int | str, Value, str, "Callable[[Value], Value]"],
) -> None:
    rn, value, operation, astatx_update = result
    astatx_code = UREG_CODES["ASTATX"]
    state.uregs[astatx_code] = astatx_update(_ureg_raw(state.uregs, astatx_code))
    if isinstance(rn, str):
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register=rn,
            value=value,
        )
        state.special["MRF"] = value
        return
    if isinstance(rn, tuple):
        # Dual-result compute (dual add/subtract, MUL/ALU multifunction):
        # two destination registers sharing one ASTATX update, already
        # combined by the caller (PRM p.3-21/3-22).
        names = ["R%d" % reg for reg in rn]
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register=names,
            value=[_json_value(v) for v in value],
        )
        for reg, val in zip(rn, value):
            state.uregs[reg] = val
        return
    if operation in ("compare", "bit-test", "float-compare"):
        _event(state, insn, "compute", operation=operation, status_only=True)
    else:
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register="R%d" % rn,
            value=value,
        )
        state.uregs[rn] = value


def decode_at(
    data: bytes | LoadedMemory, base_sw: Optional[int], pc_sw: int
) -> Instruction:
    """Decode exactly at PC_SW from a flat image or loader-backed memory."""
    if isinstance(data, LoadedMemory):
        return decode_loaded_at(data, pc_sw)
    if base_sw is None:
        raise ValueError("base_sw is required for flat image decoding")
    offset = (pc_sw - base_sw) * 2
    if offset < 0 or offset >= len(data):
        return Instruction(
            offset, None, "unknown", kind="unknown", note="PC outside image"
        )
    return next(disassemble(data, start_offset=offset, count=1))


def _advance(state: State, insn: Instruction) -> List[State]:
    state.steps += 1
    if insn.length_bytes is None:
        raise ValueError("cannot advance an instruction without a decoded length")
    next_pc = state.pc_sw + insn.length_bytes // 2
    if state.pending is None:
        if state.loops and state.pc_sw == state.loops[-1].end_sw:
            loop = state.loops[-1]
            remaining = loop.remaining - 1
            state.uregs[UREG_CODES["CURLCNTR"]] = Const(max(remaining, 0))
            if remaining > 0:
                _event(
                    state,
                    insn,
                    "loop-back",
                    target_sw=loop.start_sw,
                    remaining=remaining,
                    mode=loop.mode,
                )
                state.loops[-1] = Loop(loop.start_sw, loop.end_sw, remaining, loop.mode)
                state.pc_sw = loop.start_sw
                return [state]
            _event(state, insn, "loop-exit", remaining=0, mode=loop.mode)
            state.loops.pop()
            if not state.call_stack or state.call_stack[-1] != loop.start_sw:
                return [_stop(state, insn, "loop PC-stack mismatch")]
            state.call_stack.pop()
            _sync_pc_stack(state)
            state.uregs[UREG_CODES["CURLCNTR"]] = (
                Const(state.loops[-1].remaining) if state.loops else Const(0xFFFFFFFF)
            )
            if not state.loops:
                stkyx_code = UREG_CODES["STKYX"]
                state.uregs[stkyx_code] = _bitwise(
                    _ureg(state.uregs, stkyx_code),
                    Const(1 << 26),
                    "loop stacks empty",
                    lambda a, b: a | b,
                )
        state.pc_sw = next_pc
        return [state]
    p = state.pending
    if p.slots == 1:
        if p.return_from_call:
            if not state.call_stack:
                return [_stop(state, insn, "return without followed call")]
            if state.loops and state.call_stack[-1] == state.loops[-1].start_sw:
                return [_stop(state, insn, "return reached loop PC-stack entry")]
            state.pc_sw = state.call_stack.pop()
            _sync_pc_stack(state)
            state.pending = None
            _event(state, insn, "loaded-call-return", return_sw=state.pc_sw)
            return [state]
        if p.call:
            if p.target is None:
                return [_stop(state, insn, "call without target")]
            target = p.target
            return_sw = next_pc if p.return_sw == AFTER_DELAY_SLOTS else p.return_sw
            loaded = False
            if (
                state.follow_loaded_calls
                and state.concrete is not None
                and target is not None
                and target >= 0
            ):
                decoded = decode_at(state.concrete, None, target)
                loaded = decoded.kind != "unknown"
            if loaded:
                followed_depth = len(state.call_stack) - len(state.loops)
                if followed_depth >= state.max_call_depth:
                    return [_stop(state, insn, "max-call-depth")]
                if return_sw is None:
                    return [_stop(state, insn, "call without architectural return")]
                state.call_stack.append(return_sw)
                _sync_pc_stack(state)
                state.pending = None
                state.pc_sw = target
                state.at_loaded_entry = True
                _event(
                    state,
                    insn,
                    "loaded-call-enter",
                    target_sw=target,
                    return_sw=return_sw,
                )
                return [state]
            if return_sw is None:
                return [_stop(state, insn, "call without architectural return")]
            dossier = _dossier(state, target, return_sw)
            if not state.continue_external_calls:
                _stop(state, insn, "external-call")
                # The default endpoint remains the historical stop event; its
                # dossier explicitly labels the otherwise opaque boundary.
                state.trace[-1].update(dossier)
                state.trace[-1]["opaque_external_call"] = True
                return [state]
            _event(state, insn, "opaque-external-call", **dossier)
            # Conservative ABI boundary: results can be clobbered; memory and
            # pointer arguments are deliberately untouched.
            for code in range(16):
                state.uregs[code] = Unknown("opaque-external-call result")
            state.special["MRF"] = Unknown("opaque-external-call result")
            state.pending = None
            state.pc_sw = return_sw
            _event(
                state,
                insn,
                "external-call-continue",
                clobbered=["R%d" % n for n in range(16)] + ["MRF"],
            )
            return [state]
        state.pending = None
        state.pc_sw = next_pc if p.target is None else p.target
        return [state]
    state.pending = Pending(
        p.target, p.call, p.slots - 1, p.return_from_call, p.return_sw
    )
    state.pc_sw = next_pc
    return [state]


def _lt_ge_le_gt(state: State, cond: int) -> Optional[bool]:
    """PGR Table 4-37 (p.4-93) / PRM p.4-53:

    X = (NOT AF AND (AN XOR (AV AND NOT ALUSAT))) OR (AF AND AN) OR AZ
    LE iff X, GT iff NOT X.
    Y = (NOT AF AND (AN XOR (AV AND NOT ALUSAT))) OR (AF AND AN AND NOT AZ)
    LT iff Y, GE iff NOT Y.

    (At AF=0 this is X = Y OR AZ, i.e. LE = LT OR EQ, matching intuition.)
    ALUSAT is only read when it would actually change the answer (AF=0 and
    AV=1); this lets a comparison that clearly did not overflow resolve
    without needing MODE1 to be known.
    """
    astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
    af = _astatx_known_bit(astatx, AF_BIT)
    an = _astatx_known_bit(astatx, AN_BIT)
    az = _astatx_known_bit(astatx, AZ_BIT)
    if af is None or an is None or az is None:
        return None
    if af:
        x = an or az
        y = an and not az
    else:
        av = _astatx_known_bit(astatx, AV_BIT)
        if av is None:
            return None
        if not av:
            term = an  # AN xor (AV and not ALUSAT), with AV=0
        else:
            mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
            if not isinstance(mode1, Const):
                return None
            alusat = bool(mode1.value & (1 << ALUSAT_BIT))
            term = an != (not alusat)  # AN xor (True and not ALUSAT)
        x = term or az
        y = term
    if cond in (0x02, 0x12):  # LE / GT
        return x if cond == 0x02 else not x
    return y if cond == 0x01 else not y  # LT / GE


def _predicate(state: State, cond: int) -> Optional[bool]:
    if cond == 0x1F:
        return True
    if cond in (0x00, 0x10):
        # Conditional branches in SIMD mode combine the PEx/PEy conditions.
        # The tracer does not yet model the companion PASS, so only consume
        # AZ when execution is concretely SISD.
        mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
        astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
        equal = _astatx_known_bit(astatx, AZ_BIT)
        if not isinstance(mode1, Const) or mode1.value & (1 << 21) or equal is None:
            return None
        return equal if cond == 0x00 else not equal
    if cond in (0x01, 0x02, 0x11, 0x12):
        return _lt_ge_le_gt(state, cond)
    if cond in SIMPLE_COND_BITS:
        bit, negate = SIMPLE_COND_BITS[cond]
        astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
        known = _astatx_known_bit(astatx, bit)
        if known is None:
            return None
        return (not known) if negate else known
    return None


def _check_return_target(state: State) -> Optional[str]:
    """The firmware returns through JUMP (M14, I12) (DB). When both registers
    are known, the jump target must equal the recorded return address."""
    index = _ureg(state.uregs, UREG_CODES["I12"])
    modifier = _ureg(state.uregs, UREG_CODES["M14"])
    if not isinstance(index, Const) or not isinstance(modifier, Const):
        return None
    target = (index.value + modifier.value) & 0xFFFFFF
    if target != state.call_stack[-1]:
        return "return target %#x differs from recorded return %#x" % (
            target,
            state.call_stack[-1],
        )
    return None


def _transfer(
    state: State, insn: Instruction, target: int, call: bool, cond: Optional[bool]
) -> List[State]:
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot transfer from an instruction without a decoded length")
    fall = state.pc_sw + insn.length_bytes // 2
    _event(state, insn, "call" if call else "branch", target_sw=target, predicate=cond)
    state.steps += 1
    if cond is False:
        state.pc_sw = fall
        return [state]
    # A delayed CALL returns to the instruction after its second delay slot.
    # The firmware's CJUMP idiom stores that address - 1 in the second slot, so
    # the short-word offset depends on the slot widths (7 after a 16-bit push,
    # 9 after a 48-bit one). Resolve it when the slots complete.
    return_sw = AFTER_DELAY_SLOTS if call else None
    if cond is True:
        state.pc_sw, state.pending = fall, Pending(target, call, return_sw=return_sw)
        return [state]
    taken, not_taken = _copy(state), _copy(state)
    taken.pc_sw, taken.pending = fall, Pending(target, call, return_sw=return_sw)
    not_taken.pc_sw, not_taken.pending = fall, Pending(None)
    not_taken.trace[-1]["action"] = "branch-not-taken"
    return [taken, not_taken]


def _immediate_transfer(
    state: State, insn: Instruction, target: int, call: bool, cond: Optional[bool]
) -> List[State]:
    """Execute a Type 8 transfer without the instruction's DB modifier."""
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot transfer from an instruction without a decoded length")
    fall = state.pc_sw + insn.length_bytes // 2
    _event(state, insn, "call" if call else "branch", target_sw=target, predicate=cond)
    if cond is False:
        return _advance(state, insn)
    if cond is True:
        state.pending = Pending(target, call, slots=1, return_sw=fall if call else None)
        return _advance(state, insn)
    taken, not_taken = _copy(state), _copy(state)
    taken.pending = Pending(target, call, slots=1, return_sw=fall if call else None)
    not_taken.trace[-1]["action"] = "branch-not-taken"
    return _advance(taken, insn) + _advance(not_taken, insn)


def _return_transfer(
    state: State, insn: Instruction, predicate: Optional[bool], delayed: bool
) -> List[State]:
    """Execute a documented RTS against the tracer's followed-call stack."""
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot return from an instruction without a decoded length")
    length_bytes = insn.length_bytes
    _event(state, insn, "return", predicate=predicate, delayed=delayed)
    if predicate is False:
        state.trace[-1]["action"] = "return-not-taken"
        return _advance(state, insn)

    def take_return(taken: State) -> List[State]:
        if not taken.call_stack:
            return [_stop(taken, insn, "return without followed call")]
        if taken.loops and taken.call_stack[-1] == taken.loops[-1].start_sw:
            return [_stop(taken, insn, "return reached loop PC-stack entry")]
        if delayed:
            taken.steps += 1
            taken.pc_sw += length_bytes // 2
            taken.pending = Pending(None, slots=2, return_from_call=True)
        else:
            taken.steps += 1
            taken.pc_sw = taken.call_stack.pop()
            _sync_pc_stack(taken)
            _event(taken, insn, "loaded-call-return", return_sw=taken.pc_sw)
        return [taken]

    if predicate is True:
        return take_return(state)
    taken, not_taken = _copy(state), _copy(state)
    not_taken.trace[-1]["action"] = "return-not-taken"
    return take_return(taken) + _advance(not_taken, insn)


def _start_counted_loop(state: State, insn: Instruction, count: int) -> List[State]:
    if count == 0:
        return [_stop(state, insn, "unsupported zero-count Type12a loop")]
    reladdr = (_field(insn.fields, "reladdr[22:16]") << 16) | _field(
        insn.fields, "reladdr[15:0]"
    )
    if insn.length_bytes is None:
        raise ValueError("cannot start a loop from an instruction without a length")
    end_sw = state.pc_sw + _signed(reladdr, 23)
    start_sw = state.pc_sw + insn.length_bytes // 2
    mode = _field(insn.fields, "mode")
    state.uregs[UREG_CODES["LCNTR"]] = Const(count)
    state.uregs[UREG_CODES["CURLCNTR"]] = Const(count)
    stkyx_code = UREG_CODES["STKYX"]
    state.uregs[stkyx_code] = _bitwise(
        _ureg(state.uregs, stkyx_code),
        Const(1 << 26),
        "loop stacks nonempty",
        lambda a, b: a & ~b,
    )
    state.loops.append(Loop(start_sw, end_sw, count, mode))
    state.call_stack.append(start_sw)
    _sync_pc_stack(state)
    _event(
        state,
        insn,
        "loop-setup",
        start_sw=start_sw,
        end_sw=end_sw,
        count=count,
        mode=mode,
    )
    return _advance(state, insn)


def _execute(state: State, insn: Instruction) -> List[State]:
    if insn.kind != "confident" or insn.length_bytes is None:
        if insn.length_bytes is None or insn.type_name not in state.provisional_forms:
            return [_stop(state, insn, "uncertain or undecodable form: " + insn.note)]
        if insn.type_name not in state.provisional_used:
            state.provisional_used = tuple(
                sorted(set(state.provisional_used) | {insn.type_name})
            )
    state.at_loaded_entry = False
    f, name = insn.fields, insn.type_name
    if name in ("21a", "21c"):
        return _advance(state, insn)
    if name == "6b_shiftimm":
        if _field(f, "cond") != 0x1F:
            return [_stop(state, insn, "unsupported Type6b predicate")]
        try:
            result = _shift_immediate(f, dict(state.uregs))
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        _apply_compute(state, insn, result)
        return _advance(state, insn)
    if name == "6a_mem":
        # PRM Type 6a performs a ShiftImm and a normal-word memory transfer
        # in parallel, then post-modifies the selected I register by M.
        if _field(f, "cond") != 0x1F:
            return [_stop(state, insn, "unsupported Type6a predicate")]
        old = dict(state.uregs)
        try:
            result = _shift_immediate(f, old)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        bank = 8 if _field(f, "g") else 0
        index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
        iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
        scale = _access_modifier_scale("normal-word", state.assume_nw32)
        scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
        space = "PM" if bank else "DM"
        dreg = _field(f, "dreg")
        if _field(f, "d"):
            value = _ureg(old, dreg)
            _event(
                state,
                insn,
                "store",
                space=space,
                dreg="R%d" % dreg,
                value=value,
                address=iv,
                expression=_render(iv),
                concrete_write=_dm_write(state, iv, 4, value)
                if space == "DM"
                else False,
                addressing_mode="post-modify",
                access_width="normal-word",
            )
        else:
            loaded = _load_normal_ureg(state, space, iv, dreg)
            _event(
                state,
                insn,
                "load",
                space=space,
                dreg="R%d" % dreg,
                address=iv,
                expression=_render(iv),
                concrete_value=loaded,
                addressing_mode="post-modify",
                access_width="normal-word",
            )
        state.uregs[16 + index] = _add(
            iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
        )
        _apply_compute(state, insn, result)
        return _advance(state, insn)
    if name == "18a":
        bop = _field(f, "bop")
        sreg = _field(f, "sreg")
        if bop in (4, 5):
            operation = "bit-test" if bop == 4 else "xor-test"
            code = UREG_CODES["USTAT1"] + sreg
            mask = _wide(f, "data")
            source = _ureg(state.uregs, code)
            if isinstance(source, Const):
                result = (
                    (source.value & mask) == mask if bop == 4 else source.value == mask
                )
            else:
                result = None
            mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
            simd = bool(mode1.value & (1 << 21)) if isinstance(mode1, Const) else None
            astatx_code = UREG_CODES["ASTATX"]
            astatx = _ureg_raw(state.uregs, astatx_code)
            if result is None:
                state.uregs[astatx_code] = _astatx_forget(astatx, 1 << BTF_BIT)
            else:
                state.uregs[astatx_code] = _astatx_define(
                    astatx, 1 << BTF_BIT, (1 << BTF_BIT) if result else 0
                )
            # In SIMD mode the complementary STKY/ASTAT pair is evaluated
            # independently.  Preserve that uncertainty unless both MODE1
            # and the complementary source are concrete.
            if sreg in (6, 7, 8, 9) and simd is not False:
                complement = {6: 7, 7: 6, 8: 9, 9: 8}[sreg]
                complement_source = _ureg(
                    state.uregs, UREG_CODES["USTAT1"] + complement
                )
                if simd is True and isinstance(complement_source, Const):
                    complement_result = (
                        (complement_source.value & mask) == mask
                        if bop == 4
                        else complement_source.value == mask
                    )
                else:
                    complement_result = None
                astaty_code = UREG_CODES["ASTATY"]
                astaty = _ureg_raw(state.uregs, astaty_code)
                if complement_result is None:
                    state.uregs[astaty_code] = _astatx_forget(astaty, 1 << BTF_BIT)
                else:
                    state.uregs[astaty_code] = _astatx_define(
                        astaty, 1 << BTF_BIT, (1 << BTF_BIT) if complement_result else 0
                    )
            _event(
                state,
                insn,
                "system-bit-test",
                register=UREG_NAMES[code],
                operation=operation,
                mask=mask,
                result=result,
                simd=simd,
            )
            return _advance(state, insn)
        operations = {
            0: ("set", lambda a, b: a | b),
            1: ("clear", lambda a, b: a & ~b),
            2: ("toggle", lambda a, b: a ^ b),
        }
        if bop not in operations:
            return [_stop(state, insn, "unsupported Type18a BOP %#x" % bop)]
        # ASTATx/y and STKYx/y have implicit complementary-register behavior
        # in SIMD mode.  Stop rather than invent MODE1/PE state for those
        # register pairs; the other SYSREG selections have no companion.
        if sreg in (6, 7, 8, 9):
            return [
                _stop(
                    state,
                    insn,
                    "unsupported Type18a SIMD-sensitive system register",
                )
            ]
        code = UREG_CODES["USTAT1"] + sreg
        mask = _wide(f, "data")
        previous = _ureg(state.uregs, code)
        operation, calculate = operations[bop]
        value = _bitwise(
            previous,
            Const(mask),
            "%s %s %#x" % (operation, UREG_NAMES[code], mask),
            calculate,
        )
        state.uregs[code] = value
        _event(
            state,
            insn,
            "system-bit-op",
            register=UREG_NAMES[code],
            operation=operation,
            mask=mask,
            previous=_json_value(previous),
            value=value,
        )
        return _advance(state, insn)
    if name == "20a":
        push_fields = ("lpu", "spu", "ppu")
        pop_fields = ("lpo", "spo", "ppo")
        if any(_field(f, field) for field in push_fields) and any(
            _field(f, field) for field in pop_fields
        ):
            return [_stop(state, insn, "invalid Type20a mixed push and pop")]
        unsupported = [
            field
            for field in (
                "lpu",
                "ppu",
                "llii",
                "lldwb",
                "lldi",
                "llpwb",
                "llpi",
            )
            if _field(f, field)
        ]
        if unsupported:
            return [
                _stop(
                    state,
                    insn,
                    "unsupported Type20a operations: " + ", ".join(unsupported),
                )
            ]
        push_status = bool(_field(f, "spu"))
        pop_status = bool(_field(f, "spo"))
        pop_loop = bool(_field(f, "lpo"))
        pop_pc = bool(_field(f, "ppo"))
        flush_cache = bool(_field(f, "fc"))
        astatx_code = UREG_CODES["ASTATX"]
        astaty_code = UREG_CODES["ASTATY"]
        mode1_code = UREG_CODES["MODE1"]
        stkyx_code = UREG_CODES["STKYX"]
        if push_status:
            # PUSH STS saves the exact ASTATX/ASTATY register, including any
            # partial knowledge, not a value moved to a general register: use
            # _ureg_raw so a PartialConst round-trips through POP STS intact.
            state.status_stack.append(
                (
                    _ureg_raw(state.uregs, astatx_code),
                    _ureg_raw(state.uregs, astaty_code),
                    _ureg(state.uregs, mode1_code),
                )
            )
            state.uregs[mode1_code] = _bitwise(
                _ureg(state.uregs, mode1_code),
                _ureg(state.uregs, UREG_CODES["MMASK"]),
                "MODE1 masked by PUSH STS",
                lambda mode1, mmask: mode1 & ~mmask,
            )
            state.uregs[stkyx_code] = _bitwise(
                _ureg(state.uregs, stkyx_code),
                Const(1 << 24),
                "status stack nonempty",
                lambda value, mask: value & ~mask,
            )
        if pop_status:
            if state.status_stack:
                astatx, astaty, mode1 = state.status_stack.pop()
                state.uregs[astatx_code] = astatx
                state.uregs[astaty_code] = astaty
                state.uregs[mode1_code] = mode1
            if not state.status_stack:
                state.uregs[stkyx_code] = _bitwise(
                    _ureg(state.uregs, stkyx_code),
                    Const(1 << 24),
                    "status stack empty",
                    lambda value, mask: value | mask,
                )
        if pop_loop:
            if state.loops:
                state.loops.pop()
            state.uregs[UREG_CODES["CURLCNTR"]] = (
                Const(state.loops[-1].remaining) if state.loops else Const(0xFFFFFFFF)
            )
            if not state.loops:
                state.uregs[stkyx_code] = _bitwise(
                    _ureg(state.uregs, stkyx_code),
                    Const(1 << 26),
                    "loop stacks empty",
                    lambda value, mask: value | mask,
                )
        if pop_pc:
            if state.call_stack:
                state.call_stack.pop()
            _sync_pc_stack(state)
        _event(
            state,
            insn,
            "stack-control",
            push_status=push_status,
            pop_status=pop_status,
            pop_loop=pop_loop,
            pop_pc=pop_pc,
            flush_cache=flush_cache,
            status_depth=len(state.status_stack),
        )
        return _advance(state, insn)
    if name == "12a_imm":
        count = (_field(f, "data[15:8]") << 8) | _field(f, "data[7:0]")
        return _start_counted_loop(state, insn, count)
    if name == "12a_ureg":
        count = _ureg(state.uregs, _field(f, "ureg"))
        if not isinstance(count, Const):
            return [_stop(state, insn, "nonconcrete Type12a UREG loop count")]
        return _start_counted_loop(state, insn, count.value)
    if state.pending and name in ("25a_direct", "25a_pcrel", "8a_abs", "8a_rel"):
        return [_stop(state, insn, "nested delayed transfer")]
    if name == "11c":
        if _field(f, "x"):
            return [_stop(state, insn, "unsupported Type11c RTI")]
        if _field(f, "lr"):
            return [_stop(state, insn, "unsupported Type11c loop reentry")]
        return _return_transfer(
            state,
            insn,
            _predicate(state, _field(f, "cond")),
            bool(_field(f, "j")),
        )
    if name == "9a_abs":
        # PRM Type 9a (pp. 14-5, 14-8): JUMP/CALL (Md, Ic) with an optional
        # compute. I pre-modified by M gives the target; I is unchanged.
        if state.pending:
            return [_stop(state, insn, "nested delayed transfer")]
        if _field(f, "a") or _field(f, "ci"):
            return [_stop(state, insn, "unsupported Type9a control modifier")]
        pmi = (_field(f, "pmi[2:2]") << 2) | _field(f, "pmi[1:0]")
        pmm = _field(f, "pmm")
        cond = _field(f, "cond")
        compute_when_taken = not bool(_field(f, "e"))

        def apply_compute(executed: State) -> Optional[str]:
            try:
                compute = _compute(f, False, dict(executed.uregs), executed.special)
            except ValueError as error:
                return str(error)
            if compute is not None:
                _apply_compute(executed, insn, compute)
            return None

        if (
            _field(f, "b") == 0
            and cond == 0x1F
            and pmi == 4
            and pmm == 6
            and _field(f, "j") == 1
        ):
            # The verified I12/M14 (DB) return idiom of 9b_abs, plus the compute.
            if not state.call_stack:
                return [_stop(state, insn, "return without followed call")]
            mismatch = _check_return_target(state)
            if mismatch:
                return [_stop(state, insn, mismatch)]
            if compute_when_taken:
                error = apply_compute(state)
                if error:
                    return [_stop(state, insn, error)]
            _event(state, insn, "return-branch", index="I12", modifier="M14")
            state.steps += 1
            state.pc_sw = state.pc_sw + insn.length_bytes // 2
            state.pending = Pending(None, slots=2, return_from_call=True)
            return [state]
        # Type 9 indirect branches use DAG2: Ic is I8-I15 and Md is M8-M15.
        i_value = _ureg(state.uregs, UREG_CODES["I%d" % (8 + pmi)])
        m_value = _ureg(state.uregs, UREG_CODES["M%d" % (8 + pmm)])
        if not isinstance(i_value, Const) or not isinstance(m_value, Const):
            return [
                _stop(
                    state,
                    insn,
                    "unknown 9a_abs indirect target through I%d/M%d"
                    % (8 + pmi, 8 + pmm),
                )
            ]
        target = (i_value.value + m_value.value) & 0xFFFFFF
        predicate = _predicate(state, cond)
        call = bool(_field(f, "b"))
        transfer = _transfer if _field(f, "j") else _immediate_transfer
        if predicate is not None:
            if predicate == compute_when_taken:
                error = apply_compute(state)
                if error:
                    return [_stop(state, insn, error)]
            return transfer(state, insn, target, call, predicate)
        taken, not_taken = _copy(state), _copy(state)
        compute_state = taken if compute_when_taken else not_taken
        error = apply_compute(compute_state)
        if error:
            return [_stop(compute_state, insn, error)]
        _event(
            taken, insn, "predicate-assumption", condition=cond, predicate_assumption=True
        )
        _event(
            not_taken, insn, "predicate-assumption", condition=cond, predicate_assumption=False
        )
        return transfer(taken, insn, target, call, True) + transfer(
            not_taken, insn, target, call, False
        )
    # The verified compiler return is a TRUE 9b_abs jump through I12/M14,
    # with two delay slots, one of which is the confident 25c_rframe form.
    # Do not treat rframe alone, its provisional 48-bit sibling, or another
    # register-indirect jump as a return.
    if name == "9b_abs":
        pmi = (_field(f, "pmi[2:2]") << 2) | _field(f, "pmi[1:0]")
        pmm = _field(f, "pmm")
        if (
            _field(f, "b") == 0
            and _field(f, "cond") == 0x1F
            and pmi == 4
            and pmm == 6
            and _field(f, "j") == 1
        ):
            if not state.call_stack:
                return [_stop(state, insn, "return without followed call")]
            mismatch = _check_return_target(state)
            if mismatch:
                return [_stop(state, insn, mismatch)]
            _event(state, insn, "return-branch", index="I12", modifier="M14")
            state.steps += 1
            state.pc_sw = state.pc_sw + insn.length_bytes // 2
            state.pending = Pending(None, slots=2, return_from_call=True)
            return [state]
        # Any other Type 9b JUMP/CALL (Md, Ic): DAG2 I(8+pmi) + M(8+pmm).
        if state.pending:
            return [_stop(state, insn, "nested delayed transfer")]
        if _field(f, "a") or _field(f, "ci"):
            return [_stop(state, insn, "unsupported Type9b control modifier")]
        i_value = _ureg(state.uregs, UREG_CODES["I%d" % (8 + pmi)])
        m_value = _ureg(state.uregs, UREG_CODES["M%d" % (8 + pmm)])
        if not isinstance(i_value, Const) or not isinstance(m_value, Const):
            return [
                _stop(
                    state,
                    insn,
                    "unknown 9b_abs indirect target through I%d/M%d"
                    % (8 + pmi, 8 + pmm),
                )
            ]
        target = (i_value.value + m_value.value) & 0xFFFFFF
        transfer = _transfer if _field(f, "j") else _immediate_transfer
        return transfer(
            state,
            insn,
            target,
            bool(_field(f, "b")),
            _predicate(state, _field(f, "cond")),
        )
    if name == "25c_rframe":
        if state.pending and state.pending.return_from_call:
            frame = _ureg(state.uregs, UREG_CODES["I6"])
            state.uregs[UREG_CODES["I7"]] = frame
            if isinstance(frame, Const):
                restored = _dm_read(state, frame.value, 4)
                if restored is None:
                    state.uregs[UREG_CODES["I6"]] = Unknown(
                        "RFRAME load from unavailable memory"
                    )
                else:
                    state.uregs[UREG_CODES["I6"]] = restored
            else:
                state.uregs[UREG_CODES["I6"]] = Unknown(
                    "RFRAME load through nonconcrete I6"
                )
            _event(
                state,
                insn,
                "rframe",
                frame=_json_value(frame),
                restored_i6=_json_value(_ureg(state.uregs, UREG_CODES["I6"])),
            )
            return _advance(state, insn)
        return [_stop(state, insn, "rframe outside verified return delay slots")]
    if name in ("17a", "17b"):
        value = (
            _wide(f, "data") if name == "17a" else _signed(_field(f, "data[15:0]"), 16)
        )
        code = _field(f, "ureg")
        state.uregs[code] = Const(value)
        _event(
            state, insn, "ureg-write", ureg=UREG_NAMES[code], value=value & 0xFFFFFFFF
        )
        return _advance(state, insn)
    if name == "7a":
        # Type 7a is MODIFY: the manual guarantees an index-register update in
        # parallel with its optional compute.  The table now carries the M
        # register selector at bits 29-27, the same field Type7b uses.
        cond = _field(f, "cond")
        if cond not in (0x1F, 0x17):
            return [_stop(state, insn, "unsupported Type7a predicate")]
        bank = 8 if _field(f, "g") else 0
        source_low = _field(f, "is[2:2]") << 2 | _field(f, "is[1:0]")
        destination_low = source_low ^ _field(f, "idis")
        source, destination = source_low + bank, destination_low + bank
        modifier = _field(f, "m") + bank
        conditional = cond == 0x17
        if conditional:
            if _field(f, "compute[22:16]") or _field(f, "compute[15:0]"):
                return [_stop(state, insn, "unsupported Type7a conditional compute")]
            length = _ureg(state.uregs, UREG_CODES["L%d" % source])
            if not isinstance(length, Const) or length.value != 0:
                return [_stop(state, insn, "unsupported Type7a circular modify")]
            predicate = _predicate(state, cond)
            mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
            if predicate is False and isinstance(mode1, Const) and not (
                mode1.value & (1 << 21)
            ):
                _event(
                    state,
                    insn,
                    "i-modify-skipped",
                    source="I%d" % source,
                    destination="I%d" % destination,
                    predicate="PEx false, SISD",
                )
                return _advance(state, insn)
            if predicate is not True:
                state.uregs[16 + destination] = Unknown(
                    "conditional Type7a modify outcome"
                )
                _event(
                    state,
                    insn,
                    "i-modify-uncertain",
                    source="I%d" % source,
                    destination="I%d" % destination,
                    predicate="PEx unknown" if predicate is None else "PEx false, SIMD unknown",
                    scale_assumption="assume_nw32" if state.assume_nw32 else "unscaled normal-word",
                )
                return _advance(state, insn)
        try:
            compute = _compute(f, False, dict(state.uregs), state.special)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        index_value = _ureg(state.uregs, 16 + source)
        modifier_value = _ureg(state.uregs, 32 + modifier)
        scale = _access_modifier_scale("normal-word", state.assume_nw32)
        scaled_modifier = _multiply(
            modifier_value, Const(scale), "M%d * %d" % (modifier, scale)
        )
        state.uregs[16 + destination] = _add(
            index_value,
            scaled_modifier,
            "I%d + M%d * %d" % (source, modifier, scale),
        )
        _event(
            state,
            insn,
            "i-modify",
            source="I%d" % source,
            destination="I%d" % destination,
            modifier="M%d" % modifier,
            **({"scale_assumption": "assume_nw32"} if conditional and state.assume_nw32 else {}),
        )
        if compute is not None:
            _apply_compute(state, insn, compute)
        return _advance(state, insn)
    if name == "7d":
        # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm), ACONV
        # (Type 7d), Figure 13-21 p.352 and its Encode Table (same page):
        # this handler is deliberately only the pure ACONV row: cond=11111
        # and an empty compute (Table 13-22, p.350). decode_table.json pins
        # those fields into the mask/value, so cond/compute are not free here.
        # The PRM also describes conditional/compute-parallel Type7d rows;
        # they are outside this bounded decoder contract and must not silently
        # enter this handler as pure ACONV. g selects
        # DAG1/DAG2 (add 8, as for Type7a/Type19a); breg selects the I or B
        # register class; toby selects W2B (1) vs B2W (0); the destination
        # register is the source XOR idis, the same trick as Type7a/Type19a.
        #
        # Table 6-4 "Switch Address Instruction Semantics" (same PRM p.200,
        # printed 6-16; identical table in out/refs/sc58x-2158x-prm) hedges
        # the shift:
        #   "Id = B2W(Is) ... Base addr in byte-addressed space: Convert
        #   byte pointer to word pointer. Likely semantics Id <- Is >> 2.
        #   Exact semantics depend on address map and must work correctly
        #   for all addresses in both internal and external memory. In case
        #   of byte addresses not having word space equivalent Is will be
        #   retained as is i.e. Id = Is and illegal address space (ILAD)
        #   interrupt is generated."
        #   "Id = W2B(Is) ... Likely semantics Id <- Is << 2 ... [same ILAD
        #   hedge]." (Bd/Bs rows mirror Id/Is.)
        # This decoder does not model the address map or the ILAD trap, so
        # it only applies the documented "likely" shift, tags the event
        # semantics="prm-likely", and stops when the source is not concrete
        # rather than guess whether the trap fires.
        bank = 8 if _field(f, "g") else 0
        source_low = _field(f, "is[2:2]") << 2 | _field(f, "is[1:0]")
        destination_low = source_low ^ _field(f, "idis")
        source, destination = source_low + bank, destination_low + bank
        breg = bool(_field(f, "breg"))
        reg_class = "B" if breg else "I"
        base_code = UREG_CODES["B0"] if breg else UREG_CODES["I0"]
        src_code, dst_code = base_code + source, base_code + destination
        value = _ureg(state.uregs, src_code)
        w2b = bool(_field(f, "toby"))
        direction = "w2b" if w2b else "b2w"
        if isinstance(value, Unknown) or isinstance(value, PartialConst):
            return [
                _stop(
                    state,
                    insn,
                    "Type7d %s(%s%d) source is not concrete"
                    % (direction.upper(), reg_class, source),
                )
            ]
        result = _aconv(value, w2b, src_code, state.pc_sw)
        state.uregs[dst_code] = result
        _event(
            state,
            insn,
            "aconv",
            direction=direction,
            source="%s%d" % (reg_class, source),
            destination="%s%d" % (reg_class, destination),
            value=_json_value(result),
            semantics="prm-likely",
        )
        return _advance(state, insn)
    if name == "3a":
        # PRM Type 3a is a conditional compute plus one normal-word DM/PM
        # transfer. Table 13-1's syntax row is "IF cond compute, DM(Ia,Mb)
        # = Ureg" -- cond gates the *whole* instruction, not just the
        # compute half (PRM p.7924: a false condition "generate[s] NOPs
        # on the processing element"), so a resolved-false predicate skips
        # both the transfer and the compute, and an unresolved predicate
        # forks into executed/skipped states exactly like every other
        # conditional form here (2a, 5a_move, 9a_abs). Long-word pairs
        # remain deliberately unsupported.
        if _field(f, "l"):
            return [_stop(state, insn, "unsupported Type3a long-word access")]
        cond = _field(f, "cond")
        old = dict(state.uregs)
        compute_fields = dict(f)
        compute_field = _field(f, "compute")
        compute_fields["compute[22:16]"] = compute_field >> 16
        compute_fields["compute[15:0]"] = compute_field & 0xFFFF
        try:
            compute = _compute(compute_fields, False, old, state.special)
        except ValueError as error:
            return [_stop(state, insn, str(error))]

        def run_transfer(target: State) -> None:
            bank = 8 if _field(f, "g") else 0
            index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
            post_modify = bool(_field(f, "u"))
            space = "PM" if bank else "DM"
            iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
            scale = _access_modifier_scale("normal-word", target.assume_nw32)
            scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
            modified = _add(iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale))
            address = iv if post_modify else modified
            ureg = _field(f, "ureg")
            if _field(f, "d"):
                value = _ureg(old, ureg)
                _event(
                    target,
                    insn,
                    "store",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    value=value,
                    address=address,
                    expression=_render(address),
                    concrete_write=_dm_write(target, address, 4, value)
                    if space == "DM"
                    else False,
                    addressing_mode="post-modify" if post_modify else "pre-modify",
                    access_width="normal-word",
                )
            else:
                loaded = _load_normal_ureg(target, space, address, ureg)
                _event(
                    target,
                    insn,
                    "load",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    address=address,
                    expression=_render(address),
                    concrete_value=loaded,
                    addressing_mode="post-modify" if post_modify else "pre-modify",
                    access_width="normal-word",
                )
            if post_modify:
                target.uregs[16 + index] = modified
            if compute is not None:
                _apply_compute(target, insn, compute)

        predicate = _predicate(state, cond)
        if predicate is True:
            run_transfer(state)
            state.trace[-1].update(condition=cond, predicate_assumption=True)
            return _advance(state, insn)
        if predicate is False:
            _event(
                state, insn, "type3a-skipped", condition=cond, predicate_assumption=False
            )
            return _advance(state, insn)
        executed, skipped = _copy(state), _copy(state)
        run_transfer(executed)
        executed.trace[-1].update(condition=cond, predicate_assumption=True)
        _event(
            skipped, insn, "type3a-skipped", condition=cond, predicate_assumption=False
        )
        return _advance(executed, insn) + _advance(skipped, insn)
    if name == "14a":
        if _field(f, "l"):
            code = _field(f, "ureg")
            if _field(f, "g"):
                return [_stop(state, insn, "unsupported Type14a PM long-word access")]
            if code & 1 or code + 1 >= len(UREG_NAMES):
                return [_stop(state, insn, "unsupported Type14a odd UREG pair")]
            address = _wide(f, "addr")
            rendered = _render(Const(address))
            pair = (code, code + 1)
            if _field(f, "d"):
                values = tuple(_ureg(state.uregs, item) for item in pair)
                writes = tuple(
                    _dm_write(state, address + 4 * offset, 4, value)
                    for offset, value in enumerate(values)
                )
                concrete_write = all(writes)
                _event(
                    state,
                    insn,
                    "store",
                    space="DM",
                    ureg_pair=[UREG_NAMES[item] for item in pair],
                    values=[_json_value(value) for value in values],
                    address=address,
                    expression=rendered,
                    access_width="long-word",
                    concrete_write=concrete_write,
                    simd_companion_possible=False,
                )
            else:
                values = tuple(
                    _dm_read(state, address + 4 * offset, 4)
                    for offset in range(2)
                )
                for item, value, offset in zip(pair, values, range(2)):
                    state.uregs[item] = value or Unknown(
                        "memory-address " + _render(Const(address + 4 * offset))
                    )
                _event(
                    state,
                    insn,
                    "load",
                    space="DM",
                    ureg_pair=[UREG_NAMES[item] for item in pair],
                    address=address,
                    expression=rendered,
                    concrete_values=[
                        _json_value(value)
                        if value is not None
                        else {"unknown": "unavailable memory"}
                        for value in values
                    ],
                    access_width="long-word",
                    simd_companion_possible=False,
                )
            return _advance(state, insn)
        address = _wide(f, "addr")
        rendered = _render(Const(address))
        code = _field(f, "ureg")
        space = "PM" if _field(f, "g") else "DM"
        if _field(f, "d"):
            _event(
                state,
                insn,
                "store",
                space=space,
                ureg=UREG_NAMES[code],
                value=_ureg(state.uregs, code),
                address=address,
                expression=rendered,
                simd_companion_possible=True,
                **(
                    {
                        "concrete_write": _dm_write(
                            state, address, 4, _ureg(state.uregs, code)
                        )
                    }
                    if space == "DM" and state.concrete is not None
                    else {}
                ),
            )
        else:
            loaded = _load_normal_ureg(state, space, address, code)
            _event(
                state,
                insn,
                "load",
                space=space,
                ureg=UREG_NAMES[code],
                address=address,
                expression=rendered,
                concrete_value=loaded,
                simd_companion_possible=True,
            )
        return _advance(state, insn)
    if name == "14d":
        # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm)
        # pp.384-387, Figure 15-2 ("Type14d Instruction Opcode"): a direct-
        # address DM <-> R-register-file move, an "extension (exclusive
        # access) to 14a instruction". The w/ex/d/l opcode table (p.384-385)
        # lists only EX/LWEX rows (Dreg = dm(addr32) EX/LWEX and the mirror
        # store) at w=1,ex=1; every w=0 row is BH/BHEX (store, l selects
        # byte/short) or BHSE/BHSEEX (load, l selects byte/short and x
        # selects zero- vs sign-extend, p.386 BHSE/BHSEEX Encode Tables).
        # BWSE/SWSE are load-only per the Description on p.386. This
        # decoder does not model exclusive-access monitors, so it stops on
        # ex=1 (EX/BHEX/BHSEEX/LWEX) and on the undocumented w=1,ex=0
        # combination the opcode table has no row for.
        if _field(f, "ex"):
            return [_stop(state, insn, "unsupported Type14d exclusive access")]
        if _field(f, "w"):
            return [
                _stop(state, insn, "undocumented Type14d encoding (w=1, ex=0)")
            ]
        store = bool(_field(f, "d"))
        l_bit, x_bit = _field(f, "l"), _field(f, "x")
        if store:
            if x_bit:
                return [
                    _stop(
                        state,
                        insn,
                        "undocumented Type14d store encoding (x=1)",
                    )
                ]
            access_width, width, signed = (
                ("byte", 1, False),
                ("short-word", 2, False),
            )[l_bit]
        else:
            access_width, width, signed = {
                (0, 0): ("byte", 1, False),
                (1, 0): ("short-word", 2, False),
                (0, 1): ("byte-sign-extended", 1, True),
                (1, 1): ("short-word-sign-extended", 2, True),
            }[(l_bit, x_bit)]
        address = _wide(f, "addr")
        rendered = _render(Const(address))
        code = _field(f, "dreg")
        if store:
            value = _ureg(state.uregs, code)
            _event(
                state,
                insn,
                "store",
                space="DM",
                dreg="R%d" % code,
                value=value,
                address=address,
                expression=rendered,
                access_width=access_width,
                concrete_write=_dm_write(state, address, width, value),
            )
        else:
            loaded = _dm_read(state, address, width, signed)
            state.uregs[code] = loaded or Unknown("memory-address " + rendered)
            _event(
                state,
                insn,
                "load",
                space="DM",
                dreg="R%d" % code,
                address=address,
                expression=rendered,
                concrete_value=loaded,
                access_width=access_width,
            )
        return _advance(state, insn)
    if name in ("5a_move", "5b_move"):
        cond = _field(f, "cond")
        old = dict(state.uregs)
        compute = None
        if name == "5a_move":
            try:
                compute = _compute(f, False, old, state.special)
            except ValueError as error:
                return [_stop(state, insn, str(error))]
        src = (
            _field(f, "srcureghigh") << 2
            | _field(f, "srcureglow[1:1]") << 1
            | _field(f, "srcureglow[0:0]")
        )
        dst = _field(f, "dstureg")
        copied = _ureg(old, src)
        predicate = _predicate(state, cond)
        if predicate is False:
            _event(
                state,
                insn,
                "ureg-copy-skipped",
                source=UREG_NAMES[src],
                destination=UREG_NAMES[dst],
                condition=cond,
                predicate_assumption=False,
            )
            return _advance(state, insn)
        executed = state if predicate is True else _copy(state)
        # The Type 5a data move and compute both consume the pre-instruction file.
        executed.uregs[dst] = copied
        if compute is not None:
            _apply_compute(executed, insn, compute)
        _event(
            executed,
            insn,
            "ureg-copy",
            source=UREG_NAMES[src],
            destination=UREG_NAMES[dst],
            condition=cond,
            predicate_assumption=True,
        )
        if predicate is True:
            return _advance(executed, insn)
        skipped = _copy(state)
        _event(
            skipped,
            insn,
            "ureg-copy-skipped",
            source=UREG_NAMES[src],
            destination=UREG_NAMES[dst],
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(executed, insn) + _advance(skipped, insn)
    if name == "2c":
        try:
            compute = _compute(f, True, dict(state.uregs))
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute is None:
            return [_stop(state, insn, "empty short compute")]
        _apply_compute(state, insn, compute)
        return _advance(state, insn)
    if name in ("2a_short", "2b"):
        # Both are 32-bit unconditional full-compute forms with no
        # condition field (Type2b: PRM prefix 0xc0, decode_table.json
        # "prm figure (overrides PGR; firmware-confirmed)"): always execute.
        try:
            compute = _compute(f, False, dict(state.uregs), state.special)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute is None:
            return [_stop(state, insn, "empty full compute")]
        _apply_compute(state, insn, compute)
        return _advance(state, insn)
    if name == "2a":
        # Type 2a conditionally executes a full compute.  Decode against the
        # pre-instruction register file before either predicate assumption mutates it.
        try:
            compute = _compute(f, False, dict(state.uregs), state.special)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute is None:
            return [_stop(state, insn, "empty full compute")]
        cond = _field(f, "cond")
        predicate = _predicate(state, cond)
        if predicate is True:
            _apply_compute(state, insn, compute)
            state.trace[-1].update(condition=cond, predicate_assumption=True)
            return _advance(state, insn)
        if predicate is False:
            _event(
                state,
                insn,
                "compute-skipped",
                condition=cond,
                predicate_assumption=False,
            )
            return _advance(state, insn)
        executed, skipped = _copy(state), _copy(state)
        _apply_compute(executed, insn, compute)
        executed.trace[-1].update(condition=cond, predicate_assumption=True)
        _event(
            skipped,
            insn,
            "compute-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(executed, insn) + _advance(skipped, insn)
    if name == "4a":
        if _field(f, "cond") != 0x1F:
            return [_stop(state, insn, "unsupported predicate")]
        old = dict(state.uregs)
        try:
            compute = _compute(f, False, old, state.special)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        index = _field(f, "i") + (8 if _field(f, "g") else 0)
        offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
        # The immediate modifier is in normal-word address units.  Only turn
        # it into a byte displacement when the caller has explicitly fixed
        # internal normal words at 32 bits.
        if state.assume_nw32:
            offset *= 4
        iv = _ureg(old, 16 + index)
        space = "PM" if _field(f, "g") else "DM"
        if _field(f, "u"):
            address, next_i = iv, _add(iv, Const(offset), "I%d + %d" % (index, offset))
        else:
            address, next_i = _add(iv, Const(offset), "I%d + %d" % (index, offset)), iv
        code = _field(f, "dreg")
        if _field(f, "d"):
            value = _ureg(old, code)
            _event(
                state,
                insn,
                "store",
                space=space,
                dreg="R%d" % code,
                value=value,
                address=address,
                expression=_render(address),
                concrete_write=_dm_write(state, address, 4, value)
                if space == "DM"
                else False,
            )
        else:
            loaded = _dm_read(state, address, 4) if space == "DM" else None
            state.uregs[code] = loaded or Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                space=space,
                dreg="R%d" % code,
                address=address,
                expression=_render(address),
                concrete_value=loaded,
            )
        state.uregs[16 + index] = next_i
        if compute is not None:
            _apply_compute(state, insn, compute)
        return _advance(state, insn)
    if name == "4b":
        # SHARC+ Core Programming Reference rev. 1.4, pp. 13-29--13-32:
        # conditional DM/PM transfer with a signed six-bit immediate modifier.
        width_fields = (_field(f, "l"), _field(f, "x"), _field(f, "w"))
        widths = {
            (1, 1, 1): ("normal-word", 4, False),
            (0, 0, 0): ("byte", 1, False),
            (1, 0, 0): ("short-word", 2, False),
            (0, 1, 0): ("byte-sign-extended", 1, True),
            (1, 1, 0): ("short-word-sign-extended", 2, True),
        }
        access_spec = widths.get(width_fields)
        if access_spec is None:
            return [_stop(state, insn, "unsupported Type4b access width")]
        access_width, width, signed = access_spec
        store = bool(_field(f, "d"))
        if store and signed:
            return [_stop(state, insn, "unsupported Type4b sign-extended store")]
        bank = 8 if _field(f, "g") else 0
        index = _field(f, "i") + bank
        offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
        offset *= _access_modifier_scale(access_width, state.assume_nw32)
        post_modify = bool(_field(f, "u"))
        space = "PM" if bank else "DM"
        code = _field(f, "dreg")
        cond = _field(f, "cond")

        def access_memory(executed: State) -> None:
            old = dict(executed.uregs)
            iv = _ureg(old, 16 + index)
            address = (
                iv
                if post_modify
                else _add(iv, Const(offset), "I%d + %d" % (index, offset))
            )
            if store:
                value = _ureg(old, code)
                _event(
                    executed,
                    insn,
                    "store",
                    space=space,
                    dreg="R%d" % code,
                    value=value,
                    address=address,
                    expression=_render(address),
                    concrete_write=_dm_write(executed, address, width, value)
                    if space == "DM"
                    else False,
                    addressing_mode="post-modify" if post_modify else "pre-modify",
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            else:
                loaded = (
                    _dm_read(executed, address, width, signed)
                    if space == "DM"
                    else None
                )
                executed.uregs[code] = loaded or Unknown(
                    "memory-address " + _render(address)
                )
                _event(
                    executed,
                    insn,
                    "load",
                    space=space,
                    dreg="R%d" % code,
                    address=address,
                    expression=_render(address),
                    concrete_value=loaded,
                    addressing_mode="post-modify" if post_modify else "pre-modify",
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            if post_modify:
                executed.uregs[16 + index] = _add(
                    iv, Const(offset), "I%d + %d" % (index, offset)
                )

        predicate = _predicate(state, cond)
        if predicate is True:
            access_memory(state)
            return _advance(state, insn)
        if predicate is False:
            _event(
                state,
                insn,
                "memory-access-skipped",
                condition=cond,
                predicate_assumption=False,
            )
            return _advance(state, insn)
        executed, skipped = _copy(state), _copy(state)
        access_memory(executed)
        _event(
            skipped,
            insn,
            "memory-access-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(executed, insn) + _advance(skipped, insn)
    if name == "3b":
        # SHARC+ Core Programming Reference rev. 1.4, pp. 13-16--13-19.
        # Validate and decode the complete access before making a predicate
        # assumption, so unsupported forms stop rather than creating paths.
        width_fields = (_field(f, "l"), _field(f, "x"), _field(f, "w"))
        widths = {
            (0, 1, 1): "normal-word",
            (0, 0, 0): "byte",
            (0, 1, 0): "byte-sign-extended",
            (1, 0, 0): "short-word",
            (1, 1, 0): "short-word-sign-extended",
            (1, 1, 1): "long-word",
        }
        access_width = widths.get(width_fields)
        if access_width is None:
            return [_stop(state, insn, "unsupported Type3b access width")]
        store = bool(_field(f, "d"))
        if store and access_width.endswith("sign-extended"):
            return [_stop(state, insn, "unsupported Type3b sign-extended store")]
        bank = 8 if _field(f, "g") else 0
        index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
        post_modify = bool(_field(f, "u"))
        addressing_mode = "post-modify" if post_modify else "pre-modify"
        space = "PM" if bank else "DM"
        ureg = _field(f, "ureg")
        cond = _field(f, "cond")

        def access(executed: State) -> None:
            old = dict(executed.uregs)
            iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
            widths = {
                "normal-word": 4,
                "byte": 1,
                "byte-sign-extended": 1,
                "short-word": 2,
                "short-word-sign-extended": 2,
                "long-word": 8,
            }
            width = widths[access_width]
            scale = _access_modifier_scale(access_width, executed.assume_nw32)
            scaled_mv = _multiply(mv, Const(scale), f"M{modifier} * {scale}")
            address = (
                iv
                if post_modify
                else _add(iv, scaled_mv, f"I{index} + M{modifier} * {scale}")
            )
            if store:
                value = _ureg(old, ureg)
                _event(
                    executed,
                    insn,
                    "store",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    value=value,
                    address=address,
                    expression=_render(address),
                    concrete_write=_dm_write(executed, address, width, value)
                    if space == "DM"
                    else False,
                    addressing_mode=addressing_mode,
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            else:
                if access_width == "normal-word":
                    loaded: Optional[Const | dict[str, int]] = _load_normal_ureg(
                        executed, space, address, ureg
                    )
                else:
                    scalar_loaded = (
                        _dm_read(
                            executed,
                            address,
                            width,
                            access_width.endswith("sign-extended"),
                        )
                        if space == "DM"
                        else None
                    )
                    executed.uregs[ureg] = scalar_loaded or Unknown(
                        "memory-address " + _render(address)
                    )
                    loaded = scalar_loaded
                _event(
                    executed,
                    insn,
                    "load",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    address=address,
                    expression=_render(address),
                    concrete_value=loaded,
                    addressing_mode=addressing_mode,
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            if post_modify:
                executed.uregs[16 + index] = _add(
                    iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
                )

        predicate = _predicate(state, cond)
        if predicate is True:
            access(state)
            return _advance(state, insn)
        executed, skipped = _copy(state), _copy(state)
        access(executed)
        _event(
            skipped,
            insn,
            "memory-access-skipped",
            space=space,
            ureg=UREG_NAMES[ureg],
            addressing_mode=addressing_mode,
            access_width=access_width,
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(executed, insn) + _advance(skipped, insn)
    if name == "3c":
        index, modifier = _field(f, "dmi"), _field(f, "dmm")
        old = dict(state.uregs)
        iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
        scale = _access_modifier_scale("normal-word", state.assume_nw32)
        scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
        address = iv
        state.uregs[16 + index] = _add(
            iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
        )
        code = _field(f, "dreg")
        if _field(f, "d"):
            value = _ureg(old, code)
            _event(
                state,
                insn,
                "store",
                space="DM",
                dreg="R%d" % code,
                value=value,
                address=address,
                expression=_render(address),
                concrete_write=_dm_write(state, address, 4, value),
            )
        else:
            loaded = _dm_read(state, address, 4)
            state.uregs[code] = loaded or Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                space="DM",
                dreg="R%d" % code,
                address=address,
                expression=_render(address),
                concrete_value=loaded,
            )
        return _advance(state, insn)
    if name in ("16a", "16b"):
        if name == "16a" and (_field(f, "by") or _field(f, "sl")):
            return [_stop(state, insn, "unsupported Type16a by/sl")]
        index, modifier = (
            _field(f, "i") + (8 if _field(f, "g") else 0),
            _field(f, "m") + (8 if _field(f, "g") else 0),
        )
        old = dict(state.uregs)
        iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
        scale = _access_modifier_scale(
            "normal-word", state.assume_nw32 and not bool(_field(f, "g"))
        )
        scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
        address = iv
        value = Const(
            _wide(f, "data") if name == "16a" else _signed(_field(f, "data[15:0]"), 16)
        )
        _event(
            state,
            insn,
            "store",
            space="PM" if _field(f, "g") else "DM",
            address=address,
            expression=_render(address),
            value=value,
            by=_field(f, "by") if name == "16a" else 0,
            sl=_field(f, "sl") if name == "16a" else 0,
            concrete_write=_dm_write(state, address, 4, value)
            if not _field(f, "g")
            else False,
        )
        state.uregs[16 + index] = _add(
            iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
        )
        return _advance(state, insn)
    if name == "15b":
        index = _field(f, "i") + (8 if _field(f, "g") else 0)
        offset = _signed(_field(f, "data[6:0]"), 7)
        # Type 15b's immediate modifier follows the selected memory width.
        # The opt-in 32-bit normal-word interpretation therefore makes an
        # unqualified (non-LW) displacement four bytes wide.
        if state.assume_nw32 and not _field(f, "l"):
            offset *= 4
        iv = state.uregs.get(16 + index, Unknown("uninitialized I%d" % index))
        address = _add(iv, Const(offset), "I%d + %d" % (index, offset))
        code = _field(f, "ureg")
        width = 8 if _field(f, "l") else 4
        if _field(f, "d"):
            value = state.uregs.get(code, Unknown("uninitialized " + UREG_NAMES[code]))
            _event(
                state,
                insn,
                "store",
                ureg=UREG_NAMES[code],
                address=address,
                expression=_render(address),
                long_word=bool(_field(f, "l")),
                concrete_write=_dm_write(state, address, width, value),
            )
        else:
            loaded = _dm_read(state, address, width)
            state.uregs[code] = loaded or Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                ureg=UREG_NAMES[code],
                address=address,
                expression=_render(address),
                long_word=bool(_field(f, "l")),
                concrete_value=loaded,
            )
        return _advance(state, insn)
    if name == "15a":
        # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm)
        # pp.387-390, Figure 15-3 p.390 ("Type15a Instruction Opcode"):
        # DM(<data32>,Ia) = Ureg / Ureg = DM(<data32>,Ia), and the PM/Ic
        # form when g=1 (opcode table p.387: g=0 -> dm/I1REG(DAG1), g=1 ->
        # pm/I2REG(DAG2)). p.388 Description: "The I register is pre-
        # modified with an immediate value specified in the instruction.
        # The I register is not updated" -- pre-modify without writeback,
        # unlike Type19a's post-modify MODIFY. The optional (lw) "forces
        # register pair access" (p.389), modelled the same way as Type14a's
        # own (lw) register-pair form, with no SIMD companion.
        bank = 8 if _field(f, "g") else 0
        index = _field(f, "i[2:0]") + bank
        addr = _wide(f, "addr")
        iv = _ureg(state.uregs, 16 + index)
        address = _add(iv, Const(addr), "I%d + %d" % (index, addr))
        rendered = _render(address)
        space = "PM" if bank else "DM"
        if _field(f, "l"):
            code = _field(f, "ureg")
            if code & 1 or code + 1 >= len(UREG_NAMES):
                return [_stop(state, insn, "unsupported Type15a odd UREG pair")]
            pair = (code, code + 1)
            offsets = tuple(
                _add(address, Const(4 * offset), "%s + %d" % (rendered, 4 * offset))
                for offset in range(2)
            )
            if _field(f, "d"):
                values = tuple(_ureg(state.uregs, item) for item in pair)
                writes = tuple(
                    _dm_write(state, offset_address, 4, value)
                    if space == "DM"
                    else False
                    for offset_address, value in zip(offsets, values)
                )
                concrete_write = all(writes)
                _event(
                    state,
                    insn,
                    "store",
                    space=space,
                    ureg_pair=[UREG_NAMES[item] for item in pair],
                    values=[_json_value(value) for value in values],
                    address=address,
                    expression=rendered,
                    access_width="long-word",
                    concrete_write=concrete_write,
                    simd_companion_possible=False,
                )
            else:
                values = tuple(
                    _dm_read(state, offset_address, 4) if space == "DM" else None
                    for offset_address in offsets
                )
                for item, value in zip(pair, values):
                    state.uregs[item] = value or Unknown(
                        "memory-address " + rendered
                    )
                _event(
                    state,
                    insn,
                    "load",
                    space=space,
                    ureg_pair=[UREG_NAMES[item] for item in pair],
                    address=address,
                    expression=rendered,
                    concrete_values=[
                        _json_value(value)
                        if value is not None
                        else {"unknown": "unavailable memory"}
                        for value in values
                    ],
                    access_width="long-word",
                    simd_companion_possible=False,
                )
            return _advance(state, insn)
        code = _field(f, "ureg")
        if _field(f, "d"):
            value = _ureg(state.uregs, code)
            _event(
                state,
                insn,
                "store",
                space=space,
                ureg=UREG_NAMES[code],
                value=value,
                address=address,
                expression=rendered,
                simd_companion_possible=True,
                concrete_write=_dm_write(state, address, 4, value)
                if space == "DM"
                else False,
            )
        else:
            loaded = _load_normal_ureg(state, space, address, code)
            _event(
                state,
                insn,
                "load",
                space=space,
                ureg=UREG_NAMES[code],
                address=address,
                expression=rendered,
                concrete_value=loaded,
                simd_companion_possible=True,
            )
        return _advance(state, insn)
    if name in ("19a", "19a_scaled"):
        bank = 8 if _field(f, "g") else 0
        src_low = _field(f, "is")
        # PGR Type 19 encodes the destination as Id XOR Is, not as a direct
        # register number (Table 17-2 and Figure 17-2).
        dst_low = src_low ^ _field(f, "idis")
        src, dst = src_low + bank, dst_low + bank
        v = state.uregs.get(16 + src, Unknown("uninitialized I%d" % src))
        delta = _signed(_wide(f, "data"), 32)
        scale = 1
        scaled_width = None
        if name == "19a_scaled":
            scaled_width = "normal-word" if _field(f, "w") else "short-word"
            # The opt-in normal-word model represents the loaded program's
            # internal pointers in byte space. Enhanced MODIFY therefore
            # scales NW/SW immediates by four/two bytes respectively.
            if state.assume_nw32:
                scale = 4 if _field(f, "w") else 2
                delta *= scale

        result = _add(v, Const(delta), "I%d + %d" % (src, delta))
        circular = False
        wrapped = False
        if name == "19a_scaled":
            base = _ureg(state.uregs, UREG_CODES["B%d" % src])
            length = _ureg(state.uregs, UREG_CODES["L%d" % src])
            if isinstance(length, Const) and length.value == 0:
                pass
            elif (
                isinstance(v, Const)
                and isinstance(base, Const)
                and isinstance(length, Const)
            ):
                circular = True
                byte_length = length.value * scale
                if byte_length <= abs(delta):
                    result = Unknown("circular modifier is not smaller than L%d" % src)
                else:
                    candidate = (v.value + delta) & 0xFFFFFFFF
                    lower, upper = base.value, base.value + byte_length
                    if candidate < lower:
                        candidate += byte_length
                        wrapped = True
                    elif candidate >= upper:
                        candidate -= byte_length
                        wrapped = True
                    result = Const(candidate)
            else:
                bounded = _stack_bounded_symbol(v)
                byte_length = length.value * scale if isinstance(length, Const) else None
                if (
                    bounded is not None
                    and byte_length is not None
                    and abs(bounded[1]) < byte_length
                ):
                    # v has no proof yet of its own concrete value, but it
                    # is a symbol some caller has already bounded (an
                    # entry-time seed, or an earlier circular-MODIFY-
                    # derived symbol -- recursively, ultimately grounded in
                    # an entry-time seed), offset by less than one buffer
                    # length. PRM p.6-23: "If the index pointer falls
                    # outside the buffer, the DAG subtracts or adds the
                    # buffer length to the index value, wrapping the index
                    # pointer back within the start and end boundaries of
                    # the buffer" -- one +-byte_length correction. So the
                    # true (concrete) result is v + delta, corrected by at
                    # most one +-byte_length: within one buffer length of
                    # wherever v's own bound places it -- never a claim
                    # that the result equals v, another modify site's
                    # result, or the same site's own value on a different
                    # visit. A FRESH symbol (never v's own name) is minted
                    # so two circular-MODIFY results are never treated as
                    # equal or made to cancel by the Affine algebra; this
                    # module does not itself know or state the numeric
                    # bound -- that is the caller's fact (tools/
                    # sharcwriters.py's STACK_SYMBOLS / CIRC_WRAP_SLACK).
                    fresh = "%s%d_%x" % (CIRC_SYMBOL_PREFIX, src, state.pc_sw)
                    result = Affine(constant=0, terms=((fresh, 1),))
                    circular = True
                else:
                    result = Unknown("scaled circular modify I%d" % src)
        state.uregs[16 + dst] = result
        _event(
            state,
            insn,
            "i-add",
            source="I%d" % src,
            destination="I%d" % dst,
            offset=delta,
            scaled_width=scaled_width,
            circular=circular,
            wrapped=wrapped,
        )
        return _advance(state, insn)
    if name == "9a_rel":
        if state.pending:
            return [_stop(state, insn, "nested delayed transfer")]
        if _field(f, "a") or _field(f, "ci") or not _field(f, "j"):
            return [_stop(state, insn, "unsupported Type9a control modifier")]
        relative = (_field(f, "reladdr[5:5]") << 5) | _field(f, "reladdr[4:0]")
        target = (state.pc_sw + _signed(relative, 6)) & 0xFFFFFF
        predicate = _predicate(state, _field(f, "cond"))

        def apply_compute(executed: State) -> Optional[str]:
            try:
                compute = _compute(f, False, dict(executed.uregs), executed.special)
            except ValueError as error:
                return str(error)
            if compute is not None:
                _apply_compute(executed, insn, compute)
            return None

        compute_when_taken = not bool(_field(f, "e"))
        if predicate is not None:
            if predicate == compute_when_taken:
                error = apply_compute(state)
                if error:
                    return [_stop(state, insn, error)]
            return _transfer(state, insn, target, bool(_field(f, "b")), predicate)

        taken, not_taken = _copy(state), _copy(state)
        compute_state = taken if compute_when_taken else not_taken
        error = apply_compute(compute_state)
        if error:
            return [_stop(compute_state, insn, error)]
        _event(
            taken,
            insn,
            "predicate-assumption",
            condition=_field(f, "cond"),
            predicate_assumption=True,
        )
        _event(
            not_taken,
            insn,
            "predicate-assumption",
            condition=_field(f, "cond"),
            predicate_assumption=False,
        )
        return _transfer(taken, insn, target, bool(_field(f, "b")), True) + _transfer(
            not_taken, insn, target, bool(_field(f, "b")), False
        )
    if name in ("25a_direct", "25a_pcrel", "8a_abs", "8a_rel"):
        stem = "addr" if name.endswith("direct") or name.endswith("abs") else "reladdr"
        raw = (_field(f, stem + "[23:16]") << 16) | _field(f, stem + "[15:0]")
        # The sequencer generates 24-bit short-word instruction addresses;
        # reduce a signed PC-relative sum to that architectural width.
        target = (
            raw
            if name.endswith(("direct", "abs"))
            else (state.pc_sw + _signed(raw, 24)) & 0xFFFFFF
        )
        call = name.startswith("25a") or bool(_field(f, "b"))
        if name.startswith("25a"):
            previous_i6 = _ureg(state.uregs, UREG_CODES["I6"])
            new_i6 = _ureg(state.uregs, UREG_CODES["I7"])
            state.uregs[UREG_CODES["R2"]] = previous_i6
            state.uregs[UREG_CODES["I6"]] = new_i6
            _event(
                state,
                insn,
                "cjump-frame",
                saved_i6=_json_value(previous_i6),
                frame=_json_value(new_i6),
            )
        cond = True if name.startswith("25a") else _predicate(state, _field(f, "cond"))
        delayed = name.startswith("25a") or bool(_field(f, "j"))
        transfer = _transfer if delayed else _immediate_transfer
        return transfer(state, insn, target, call, cond)
    return [_stop(state, insn, "unsupported form " + str(name))]


def _seed_value(value: int | Value | str) -> Value:
    if isinstance(value, (Const, Affine, Unknown)):
        return value
    if isinstance(value, int):
        return Const(value)
    if isinstance(value, str) and value.startswith("@"):
        return symbol(value[1:])
    raise ValueError("seed value must be an integer, Value, or @symbol")


def _seed_code(key: str | int) -> int:
    if isinstance(key, str):
        try:
            return UREG_CODES[key.upper()]
        except KeyError as error:
            raise ValueError("unknown UREG: " + key) from error
    if not 0 <= key < len(UREG_NAMES):
        raise ValueError("UREG code out of range: %d" % key)
    return key


def _dedupe_key(state: State) -> tuple:
    """Everything that decides a state's future; history (trace, steps) and the
    run-wide settings shared by every state are left out."""
    return (
        state.pc_sw,
        state.pending,
        tuple(state.call_stack),
        tuple(state.loops),
        tuple(sorted(state.uregs.items())),
        tuple(sorted(state.special.items())),
        tuple(sorted(state.overlay.items())),
        tuple(sorted(state.mmrs.items())),
        tuple(state.status_stack),
        state.data_memory_tainted,
        state.at_loaded_entry,
    )


def trace(
    data: bytes | LoadedMemory,
    base_sw: Optional[int],
    start: int,
    sets: Optional[Mapping[Union[str, int], int | Value | str]] = None,
    max_steps: int = 100,
    max_states: int = 32,
    *,
    concrete_memory: bool = False,
    follow_loaded_calls: bool = False,
    continue_external_calls: bool = False,
    dossier_bytes: int = 0,
    max_call_depth: int = 8,
    skip_provisional_entries: bool = False,
    assume_nw32: bool = False,
    core_reset_state: bool = False,
    breakpoints: Sequence[int] = (),
    provisional_forms: Sequence[str] = (),
) -> List[State]:
    uregs: Dict[int, Value] = (
        {
            UREG_CODES[name]: Const(value)
            for name, value in CORE_UREG_RESET_VALUES.items()
        }
        if core_reset_state
        else {}
    )
    for key, value in (sets or {}).items():
        uregs[_seed_code(key)] = _seed_value(value)
    if concrete_memory and not isinstance(data, LoadedMemory):
        raise ValueError("concrete memory requires LoadedMemory")
    if not 0 <= max_steps <= 100_000:
        raise ValueError("max_steps must be between 0 and 100000")
    if not 1 <= max_states <= 1_024:
        raise ValueError("max_states must be between 1 and 1024")
    if dossier_bytes < 0 or dossier_bytes > 256:
        raise ValueError("dossier_bytes must be between 0 and 256")
    if max_call_depth < 1 or max_call_depth > 32:
        raise ValueError("max_call_depth must be between 1 and 32")
    if any(not isinstance(pc, int) or not 0 <= pc <= 0xFFFFFF for pc in breakpoints):
        raise ValueError("breakpoints must be 24-bit short-word addresses")
    breakpoint_set = frozenset(breakpoints)
    concrete = data if isinstance(data, LoadedMemory) and concrete_memory else None
    mmrs: Dict[int, Value] = (
        {address: Const(value) for address, value in CORE_MMR_RESET_VALUES.items()}
        if core_reset_state
        else {}
    )
    start_state = State(
        start,
        uregs,
        concrete=concrete,
        base_sw=base_sw,
        follow_loaded_calls=follow_loaded_calls,
        continue_external_calls=continue_external_calls,
        dossier_bytes=dossier_bytes,
        max_call_depth=max_call_depth,
        skip_provisional_entries=skip_provisional_entries,
        at_loaded_entry=skip_provisional_entries,
        assume_nw32=assume_nw32,
        core_reset_state=core_reset_state,
        mmrs=mmrs,
        provisional_forms=tuple(provisional_forms),
    )
    # FIFO of distinct live states. Paths that reconverge on an identical state
    # behave identically from there, so only one is kept.
    active: Dict[tuple, State] = {_dedupe_key(start_state): start_state}
    done: List[State] = []
    while active:
        state = active.pop(next(iter(active)))
        if state.pc_sw in breakpoint_set:
            done.append(
                _stop(state, decode_at(data, base_sw, state.pc_sw), "breakpoint")
            )
            continue
        if state.steps >= max_steps:
            done.append(_stop(state, None, "max-steps"))
            continue
        out = _execute(state, decode_at(data, base_sw, state.pc_sw))
        for child in out:
            if child.stopped:
                done.append(child)
                continue
            key = _dedupe_key(child)
            existing = active.get(key)
            if existing is not None:
                # Keep the copy that has used less of --max-steps.
                if child.steps < existing.steps:
                    active[key] = child
            elif len(active) + len(done) >= max_states:
                done.append(_stop(child, None, "max-states"))
            else:
                active[key] = child
    return done


def _register_snapshot(state: State) -> dict[str, Any]:
    return {
        UREG_NAMES[code]: _json_value(value)
        for code, value in sorted(state.uregs.items())
    }


def _watched_dm_snapshot(state: State, addresses: Sequence[int]) -> dict[str, Any]:
    snapshot: dict[str, Any] = {}
    for address in addresses:
        value = _dm_read(state, address, 4)
        snapshot[f"{address:#x}"] = (
            _json_value(value) if value is not None else {"unavailable": True}
        )
    return snapshot


def summarize(
    states: Sequence[State], start_sw: int, watch_dm: Sequence[int] = ()
) -> dict:
    """Return a bounded machine-readable runtime-probe summary."""
    summaries = []
    for state in states:
        peripheral_accesses = []
        loop_setups = []
        for event in state.trace:
            if event.get("action") == "loop-setup":
                loop_setups.append(
                    {
                        key: event[key]
                        for key in (
                            "pc_sw",
                            "start_sw",
                            "end_sw",
                            "count",
                            "mode",
                        )
                    }
                )
            if event.get("action") not in ("load", "store"):
                continue
            address = event.get("address")
            if not isinstance(address, int):
                continue
            peripheral = name_address(address)
            if peripheral is None:
                continue
            access = {
                "pc_sw": event["pc_sw"],
                "action": event["action"],
                "address": address,
                "peripheral": peripheral,
            }
            for key in ("value", "concrete_value", "access_width"):
                if key in event:
                    access[key] = event[key]
            peripheral_accesses.append(access)
        stop_event = state.trace[-1] if state.trace else {}
        summary = {
            "stopped": state.stopped,
            "stop_pc_sw": stop_event.get("pc_sw", state.pc_sw),
            "stop_form": stop_event.get("form"),
            "steps": state.steps,
            "events": len(state.trace),
            "loaded_calls": sum(
                event.get("action") == "loaded-call-enter" for event in state.trace
            ),
            "opaque_calls": sum(
                event.get("action") == "opaque-external-call"
                for event in state.trace
            ),
            "loop_setups": loop_setups,
            "peripheral_accesses": peripheral_accesses,
            "last_events": state.trace[-5:],
        }
        if state.stopped == "breakpoint":
            summary["registers"] = _register_snapshot(state)
            summary["watched_dm"] = _watched_dm_snapshot(state, watch_dm)
        if state.provisional_used:
            summary["provisional_forms_used"] = list(state.provisional_used)
        summaries.append(summary)
    return {"start_sw": start_sw, "states": summaries}


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source")
    p.add_argument("--blob", action="store_true")
    p.add_argument("--base-sw", type=lambda x: int(x, 0))
    p.add_argument("--start", required=True, type=lambda x: int(x, 0))
    p.add_argument("--set", dest="sets", action="append", default=[])
    p.add_argument("--max-steps", type=int, default=100)
    p.add_argument("--max-states", type=int, default=32)
    p.add_argument(
        "--break-pc",
        action="append",
        default=[],
        type=lambda x: int(x, 0),
        help="stop before executing this short-word PC (repeatable)",
    )
    p.add_argument(
        "--watch-dm",
        action="append",
        default=[],
        type=lambda x: int(x, 0),
        help="include this 32-bit DM value in breakpoint snapshots (repeatable)",
    )
    p.add_argument(
        "--concrete-memory",
        action="store_true",
        help="read loader-backed DM bytes and keep a per-path write overlay",
    )
    p.add_argument("--follow-loaded-calls", action="store_true")
    p.add_argument(
        "--continue-external-calls",
        action="store_true",
        help="record dossier, clobber result registers, then continue",
    )
    p.add_argument("--dossier-bytes", type=int, default=0)
    p.add_argument("--max-call-depth", type=int, default=8)
    p.add_argument(
        "--skip-provisional-entries",
        action="store_true",
        help=(
            "legacy artifact-replay option; currently no-op because the former provisional "
            "Type19 entry is now documented"
        ),
    )
    p.add_argument(
        "--assume-32bit-normal-words",
        action="store_true",
        help="opt in to four-byte internal normal-word DM accesses (runtime IMDWx is otherwise unknown)",
    )
    p.add_argument(
        "--core-reset-state",
        action="store_true",
        help="seed only documented core-register and core-MMR reset values",
    )
    p.add_argument(
        "--allow-provisional-form",
        action="append",
        default=[],
        metavar="NAME",
        help="execute this form (e.g. 14d) although the table marks it "
        "unconfirmed; any run that uses one is calibration, not qualification",
    )
    output = p.add_mutually_exclusive_group()
    output.add_argument("--json", action="store_true")
    output.add_argument(
        "--summary",
        action="store_true",
        help="print compact stop, loop and named-peripheral details",
    )
    p.add_argument(
        "--trace-json",
        metavar="PATH",
        help="also write the full JSON trace to PATH",
    )
    a = p.parse_args(argv)
    values = {}
    for item in a.sets:
        try:
            name, value = item.split("=", 1)
            values[name] = value if value.startswith("@") else int(value, 0)
            _seed_code(name)
            _seed_value(values[name])
        except ValueError:
            p.error("--set must be NAME=VALUE or NAME=@symbol")
    if a.concrete_memory and not a.blob:
        p.error("--concrete-memory requires --blob")
    if (a.follow_loaded_calls or a.continue_external_calls) and not a.concrete_memory:
        p.error("call following/continuation requires --concrete-memory")
    if a.skip_provisional_entries and not a.follow_loaded_calls:
        p.error("--skip-provisional-entries requires --follow-loaded-calls")
    if a.assume_32bit_normal_words and not a.concrete_memory:
        p.error("--assume-32bit-normal-words requires --concrete-memory")
    if not 0 <= a.max_steps <= 100_000:
        p.error("--max-steps must be between 0 and 100000")
    if not 1 <= a.max_states <= 1_024:
        p.error("--max-states must be between 1 and 1024")
    if not 0 <= a.dossier_bytes <= 256:
        p.error("--dossier-bytes must be between 0 and 256")
    if not 1 <= a.max_call_depth <= 32:
        p.error("--max-call-depth must be between 1 and 32")
    if any(not 0 <= pc <= 0xFFFFFF for pc in a.break_pc):
        p.error("--break-pc must be a 24-bit short-word address")
    if any(not 0 <= address <= 0xFFFFFFFF for address in a.watch_dm):
        p.error("--watch-dm must be a 32-bit address")
    if a.blob and a.base_sw is not None:
        p.error("--base-sw is ambiguous with --blob")
    if not a.blob and a.base_sw is None:
        p.error("--base-sw is required unless --blob is used")
    try:
        with open(a.source, "rb") as fh:
            source = fh.read()
    except OSError as error:
        p.error(str(error))
    if a.blob:
        try:
            source = LoadedMemory.from_stream(source)
        except (TypeError, ValueError) as error:
            p.error("invalid loader stream: " + str(error))
        if not source.ranges():
            p.error("loader stream has no loaded ranges")
        if not source.blocks or "FINAL" not in source.blocks[-1].get("flags", ()):
            p.error("loader stream ended before a final marker")
    states = trace(
        source,
        a.base_sw,
        a.start,
        values,
        a.max_steps,
        a.max_states,
        concrete_memory=a.concrete_memory,
        follow_loaded_calls=a.follow_loaded_calls,
        continue_external_calls=a.continue_external_calls,
        dossier_bytes=a.dossier_bytes,
        max_call_depth=a.max_call_depth,
        skip_provisional_entries=a.skip_provisional_entries,
        assume_nw32=a.assume_32bit_normal_words,
        core_reset_state=a.core_reset_state,
        breakpoints=a.break_pc,
        provisional_forms=tuple(a.allow_provisional_form),
    )
    result = [
        {
            "stopped": s.stopped,
            "steps": s.steps,
            "assumptions": (
                (["32-bit internal normal words"] if s.assume_nw32 else [])
                + (["documented core/MMR reset values"] if s.core_reset_state else [])
            ),
            "trace": s.trace,
            "registers": _register_snapshot(s),
            "watched_dm": _watched_dm_snapshot(s, a.watch_dm),
            **(
                {"provisional_forms_used": list(s.provisional_used)}
                if s.provisional_used
                else {}
            ),
        }
        for s in states
    ]
    if a.trace_json:
        try:
            with open(a.trace_json, "w") as fh:
                json.dump(result, fh, indent=2)
                fh.write("\n")
        except OSError as error:
            p.error("cannot write trace JSON: " + str(error))
    if a.summary:
        print(
            json.dumps(
                summarize(states, a.start, a.watch_dm), separators=(",", ":")
            )
        )
    elif a.json:
        print(json.dumps(result, indent=2))
    else:
        for state in result:
            for event in state["trace"]:
                print(event)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
