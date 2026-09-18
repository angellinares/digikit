"""Small, conservative delay-aware tracer for SHARC+ main-program code.

This intentionally follows exact short-word PCs, rather than discovering
functions or linearly sweeping unrelated bytes.  It is a first semantic slice:
unhandled forms stop a state instead of pretending to understand them.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Union

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
from sharc_disasm import Instruction, decode_loaded_at, disassemble
from sharcldr import LoadedMemory

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


Value = Union[Const, Affine, Unknown]
_SYMBOL_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


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


@dataclass
class State:
    pc_sw: int
    uregs: Dict[int, Value] = field(default_factory=dict)
    trace: List[dict] = field(default_factory=list)
    pending: Optional[Pending] = None
    steps: int = 0
    stopped: Optional[str] = None


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
    if isinstance(value, Unknown):
        return {"unknown": value.reason}
    return value


def _event(state: State, insn: Instruction, action: str, **extra) -> None:
    for key in ("address", "value"):
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
    )


def _ureg(values: Mapping[int, Value], code: int) -> Value:
    return values.get(code, Unknown("uninitialized " + UREG_NAMES[code]))


def _terms(value: Const | Affine) -> tuple[int, tuple[tuple[str, int], ...]]:
    return (
        (value.value, ()) if isinstance(value, Const) else (value.constant, value.terms)
    )


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


def _bitwise(left: Value, right: Value, expression: str, operation) -> Value:
    if isinstance(left, Const) and isinstance(right, Const):
        return Const(operation(left.value, right.value))
    return Unknown(expression)


def _not(value: Value, expression: str) -> Value:
    return Const(~value.value) if isinstance(value, Const) else Unknown(expression)


def _compute(
    f: Mapping[str, int], short: bool, values: Mapping[int, Value]
) -> Optional[tuple[int, Value, str]]:
    """Decode the small public-table subset, reading every operand from VALUES."""
    field = (
        _field(f, "compute")
        if short
        else ((_field(f, "compute[22:16]") << 16) | _field(f, "compute[15:0]"))
    )
    if not short and field == 0:
        return None
    if short:
        opcode, rn, rx = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
        left, right = _ureg(values, rn), _ureg(values, rx)
        operations = {
            0: ("add", lambda: _add(left, right, "R%d + R%d" % (rn, rx))),
            1: ("subtract", lambda: _subtract(left, right, "R%d - R%d" % (rn, rx))),
            2: ("pass", lambda: right),
            4: (
                "not",
                lambda: _not(right, "not R%d" % rx),
            ),
            5: ("increment", lambda: _add(right, Const(1), "R%d + 1" % rx)),
            6: ("decrement", lambda: _add(right, Const(-1), "R%d - 1" % rx)),
            7: (
                "multiply",
                lambda: _multiply(left, right, "R%d * R%d" % (rn, rx)),
            ),
            0xC: (
                "and",
                lambda: _bitwise(
                    left, right, "R%d and R%d" % (rn, rx), lambda a, b: a & b
                ),
            ),
            0xD: (
                "or",
                lambda: _bitwise(
                    left, right, "R%d or R%d" % (rn, rx), lambda a, b: a | b
                ),
            ),
            0xE: (
                "xor",
                lambda: _bitwise(
                    left, right, "R%d xor R%d" % (rn, rx), lambda a, b: a ^ b
                ),
            ),
        }
        if opcode == 3:
            return rn, left, "compare"
        if opcode not in operations:
            raise ValueError("unsupported short compute opcode %#x" % opcode)
        operation, calculate = operations[opcode]
        return rn, calculate(), operation
    cu, opcode = (field >> 20) & 3, (field >> 12) & 0xFF
    rn, rx, ry = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
    left, right = _ureg(values, rx), _ureg(values, ry)
    if cu == 0 and opcode == 0x02:
        value = _subtract(left, right, "R%d - R%d" % (rx, ry))
        return rn, value, "subtract"
    # PRM Table 18-5: ALUOP 00001010 is signed comp(RX, RY). It updates
    # status only, so the tracer records the comparison without writing RN.
    if cu == 0 and opcode == 0x0A:
        return rn, left, "compare"
    if cu == 0 and opcode == 0x21:
        return rn, left, "pass"
    if cu == 0 and opcode == 0x29:
        return rn, _add(left, Const(1), "R%d + 1" % rx), "increment"
    # PRM Table 18-5 and p. 19-9: ALUOP 00101010 is RN = RX - 1.
    if cu == 0 and opcode == 0x2A:
        return rn, _add(left, Const(-1), "R%d - 1" % rx), "decrement"
    if cu == 1 and opcode == 0x70:
        value = _multiply(left, right, "R%d * R%d" % (rx, ry))
        return rn, value, "multiply"
    # PRM Table 18-9 and pp. 24-5--24-6: SHIFTOP 11001100 is
    # btst RX by RY. It changes status flags only and has no RN result.
    if cu == 2 and opcode == 0xCC:
        return rn, left, "bit-test"
    raise ValueError("unsupported full compute cu=%#x opcode=%#x" % (cu, opcode))


def _apply_compute(
    state: State, insn: Instruction, result: tuple[int, Value, str]
) -> None:
    rn, value, operation = result
    if operation in ("compare", "bit-test"):
        _event(state, insn, "compute", operation=operation, status_only=True)
    else:
        _event(state, insn, "compute", operation=operation, result_register="R%d" % rn)
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
        state.pc_sw = next_pc
        return [state]
    p = state.pending
    if p.slots == 1:
        if p.call:
            _stop(state, insn, "external-call")
            state.trace[-1]["return_sw"] = next_pc
            state.trace[-1]["target_sw"] = p.target
            return [state]
        state.pending = None
        state.pc_sw = next_pc if p.target is None else p.target
        return [state]
    state.pending = Pending(p.target, p.call, p.slots - 1)
    state.pc_sw = next_pc
    return [state]


def _predicate(cond: int) -> Optional[bool]:
    # TRUE is documented; no flag model exists, so every other predicate is unknown.
    return True if cond == 0x1F else None


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
    if cond is True:
        state.pc_sw, state.pending = fall, Pending(target, call)
        return [state]
    taken, not_taken = _copy(state), _copy(state)
    taken.pc_sw, taken.pending = fall, Pending(target, call)
    not_taken.pc_sw, not_taken.pending = fall, Pending(None)
    not_taken.trace[-1]["action"] = "branch-not-taken"
    return [taken, not_taken]


def _execute(state: State, insn: Instruction) -> List[State]:
    if insn.kind != "confident" or insn.length_bytes is None:
        return [_stop(state, insn, "uncertain or undecodable form: " + insn.note)]
    f, name = insn.fields, insn.type_name
    if state.pending and name in ("25a_direct", "25a_pcrel", "8a_abs", "8a_rel"):
        return [_stop(state, insn, "nested delayed transfer")]
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
    if name in ("5a_move", "5b_move"):
        if _field(f, "cond") != 0x1F:
            return [_stop(state, insn, "unsupported predicate")]
        old = dict(state.uregs)
        compute = None
        if name == "5a_move":
            try:
                compute = _compute(f, False, old)
            except ValueError as error:
                return [_stop(state, insn, str(error))]
        src = (
            _field(f, "srcureghigh") << 2
            | _field(f, "srcureglow[1:1]") << 1
            | _field(f, "srcureglow[0:0]")
        )
        dst = _field(f, "dstureg")
        copied = _ureg(old, src)
        # The Type 5a data move and compute both consume the pre-instruction file.
        state.uregs[dst] = copied
        if compute is not None:
            _apply_compute(state, insn, compute)
        _event(
            state,
            insn,
            "ureg-copy",
            source=UREG_NAMES[src],
            destination=UREG_NAMES[dst],
        )
        return _advance(state, insn)
    if name == "2c":
        try:
            compute = _compute(f, True, dict(state.uregs))
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute is None:
            return [_stop(state, insn, "empty short compute")]
        _apply_compute(state, insn, compute)
        return _advance(state, insn)
    if name == "2a":
        # Type 2a conditionally executes a full compute.  Decode against the
        # pre-instruction register file before either predicate assumption mutates it.
        try:
            compute = _compute(f, False, dict(state.uregs))
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute is None:
            return [_stop(state, insn, "empty full compute")]
        cond = _field(f, "cond")
        predicate = _predicate(cond)
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
            compute = _compute(f, False, old)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        index = _field(f, "i") + (8 if _field(f, "g") else 0)
        offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
        iv = _ureg(old, 16 + index)
        space = "PM" if _field(f, "g") else "DM"
        if _field(f, "u"):
            address, next_i = iv, _add(iv, Const(offset), "I%d + %d" % (index, offset))
        else:
            address, next_i = _add(iv, Const(offset), "I%d + %d" % (index, offset)), iv
        code = _field(f, "dreg")
        if _field(f, "d"):
            _event(
                state,
                insn,
                "store",
                space=space,
                dreg="R%d" % code,
                value=_ureg(old, code),
                address=address,
                expression=_render(address),
            )
        else:
            state.uregs[code] = Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                space=space,
                dreg="R%d" % code,
                address=address,
                expression=_render(address),
            )
        state.uregs[16 + index] = next_i
        if compute is not None:
            _apply_compute(state, insn, compute)
        return _advance(state, insn)
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
            address = (
                iv if post_modify else _add(iv, mv, f"I{index} + M{modifier}")
            )
            if store:
                _event(
                    executed,
                    insn,
                    "store",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    value=_ureg(old, ureg),
                    address=address,
                    expression=_render(address),
                    addressing_mode=addressing_mode,
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            else:
                executed.uregs[ureg] = Unknown("memory-address " + _render(address))
                _event(
                    executed,
                    insn,
                    "load",
                    space=space,
                    ureg=UREG_NAMES[ureg],
                    address=address,
                    expression=_render(address),
                    addressing_mode=addressing_mode,
                    access_width=access_width,
                    condition=cond,
                    predicate_assumption=True,
                )
            if post_modify:
                executed.uregs[16 + index] = _add(
                    iv, mv, "I%d + M%d" % (index, modifier)
                )

        predicate = _predicate(cond)
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
        address = iv
        state.uregs[16 + index] = _add(iv, mv, "I%d + M%d" % (index, modifier))
        code = _field(f, "dreg")
        if _field(f, "d"):
            _event(
                state,
                insn,
                "store",
                space="DM",
                dreg="R%d" % code,
                value=_ureg(old, code),
                address=address,
                expression=_render(address),
            )
        else:
            state.uregs[code] = Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                space="DM",
                dreg="R%d" % code,
                address=address,
                expression=_render(address),
            )
        return _advance(state, insn)
    if name == "16a":
        if _field(f, "by") or _field(f, "sl"):
            return [_stop(state, insn, "unsupported Type16a by/sl")]
        index, modifier = (
            _field(f, "i") + (8 if _field(f, "g") else 0),
            _field(f, "m") + (8 if _field(f, "g") else 0),
        )
        old = dict(state.uregs)
        iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
        address = iv
        _event(
            state,
            insn,
            "store",
            space="PM" if _field(f, "g") else "DM",
            address=address,
            expression=_render(address),
            value=_wide(f, "data"),
            by=_field(f, "by"),
            sl=_field(f, "sl"),
        )
        state.uregs[16 + index] = _add(iv, mv, "I%d + M%d" % (index, modifier))
        return _advance(state, insn)
    if name == "15b":
        index = _field(f, "i") + (8 if _field(f, "g") else 0)
        offset = _signed(_field(f, "data[6:0]"), 7)
        iv = state.uregs.get(16 + index, Unknown("uninitialized I%d" % index))
        address = _add(iv, Const(offset), "I%d + %d" % (index, offset))
        code = _field(f, "ureg")
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
            )
        else:
            state.uregs[code] = Unknown("memory-address " + _render(address))
            _event(
                state,
                insn,
                "load",
                ureg=UREG_NAMES[code],
                address=address,
                expression=_render(address),
                long_word=bool(_field(f, "l")),
            )
        return _advance(state, insn)
    if name == "19a":
        dst, src = (
            _field(f, "idis") + (8 if _field(f, "g") else 0),
            _field(f, "is") + (8 if _field(f, "g") else 0),
        )
        v = state.uregs.get(16 + src, Unknown("uninitialized I%d" % src))
        delta = _signed(_wide(f, "data"), 32)
        state.uregs[16 + dst] = _add(v, Const(delta), "I%d + %d" % (src, delta))
        _event(
            state,
            insn,
            "i-add",
            source="I%d" % src,
            destination="I%d" % dst,
            offset=delta,
        )
        return _advance(state, insn)
    if name in ("25a_direct", "25a_pcrel", "8a_abs", "8a_rel"):
        stem = "addr" if name.endswith("direct") or name.endswith("abs") else "reladdr"
        raw = (_field(f, stem + "[23:16]") << 16) | _field(f, stem + "[15:0]")
        target = (
            raw if name.endswith(("direct", "abs")) else state.pc_sw + _signed(raw, 24)
        )
        call = name.startswith("25a") or bool(_field(f, "b"))
        cond = True if name.startswith("25a") else _predicate(_field(f, "cond"))
        return _transfer(state, insn, target, call, cond)
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


def trace(
    data: bytes | LoadedMemory,
    base_sw: Optional[int],
    start: int,
    sets: Optional[Mapping[Union[str, int], int | Value | str]] = None,
    max_steps: int = 100,
    max_states: int = 32,
) -> List[State]:
    uregs: Dict[int, Value] = {}
    for key, value in (sets or {}).items():
        uregs[_seed_code(key)] = _seed_value(value)
    active, done = [State(start, uregs)], []
    while active:
        state = active.pop(0)
        if state.steps >= max_steps:
            done.append(_stop(state, None, "max-steps"))
            continue
        out = _execute(state, decode_at(data, base_sw, state.pc_sw))
        for child in out:
            if child.stopped:
                done.append(child)
            elif len(active) + len(done) >= max_states:
                done.append(_stop(child, None, "max-states"))
            else:
                active.append(child)
    return done


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source")
    p.add_argument("--blob", action="store_true")
    p.add_argument("--base-sw", type=lambda x: int(x, 0))
    p.add_argument("--start", required=True, type=lambda x: int(x, 0))
    p.add_argument("--set", dest="sets", action="append", default=[])
    p.add_argument("--max-steps", type=int, default=100)
    p.add_argument("--max-states", type=int, default=32)
    p.add_argument("--json", action="store_true")
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
    states = trace(source, a.base_sw, a.start, values, a.max_steps, a.max_states)
    result = [
        {"stopped": s.stopped, "steps": s.steps, "trace": s.trace} for s in states
    ]
    if a.json:
        print(json.dumps(result, indent=2))
    else:
        for state in result:
            for event in state["trace"]:
                print(event)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
