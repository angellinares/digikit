#!/usr/bin/env python
# fmt: off
"""Ghidra p-code emulator harness for the SHARC+ program.

    uv run python tools/sharcemu.py dt2-1.16_SHARC --start sw:0x1c18ed --steps 200 \
        --watch 0x252658 --watch 0x254d9c --set R2=0x1234 \
        --project ~/ghidra-projects/sharc-batch-dt2-116 \
        --project-name sharc-batch-dt2-116 --json

Runs EmulatorHelper (Ghidra's p-code emulator) forward from --start, one
instruction at a time, and fails loudly instead of silently no-opping:
`tools/sharcspec/ghidra/gen_sleigh.py` emits an empty body for any SHARC+
VISA form it has not given semantics to, so an instruction with zero p-code
ops executes as a silent no-op unless we catch it first. Before every step
this tool reads the instruction's raw bytes and independently classifies
them with the repo's own decoder (tools/sharc_disasm.py, tools/sharcinv.py's
merge_fields), never Ghidra's mnemonic, so the two decoders' disagreements
stay visible. Only Type21a (an architecturally fixed NOP) and
Type9a_abs/Type9b_abs with merged field b==1 (register-indirect call, no
static target to jump to) are legitimately empty; anything else with zero
p-code is a `no-semantics` fault.

A conditional jump's compute-condition is an unimplemented CALLOTHER
pcodeop in this language, so EmulatorHelper.step() returns false there; that
surfaces as an `emulator-error` fault carrying Ghidra's own message.

`--watch ADDR[:LEN]` (repeatable; LEN in bytes, default 4 -- one DM word)
reports every write that touches that range: step index, PC, address, and
the old/new bytes. `--set REG=VALUE` seeds a register's initial value
in a fresh emulator, before --start's PC is set. `--poke ADDR=VALUE` seeds a
32-bit DM word (little-endian, matching this language's SHARC_VISA:LE:32
id) before execution starts, for cases where the register you want to seed
gets reloaded from memory before reaching the code you actually want to
watch (see the positive-control run in the task write-up).

Read-only against the Ghidra project throughout: EmulatorHelper's registers
and memory are its own live state, never the Program; nothing here opens a
transaction or saves. Follows tools/ghidraq.py's PyGhidra startup pattern,
including the sys.path guard below (tools/ghidra/ is a plain directory that
shadows the real `ghidra` Java-bridge package once tools/ lands on
sys.path, which crashes `import pyghidra` with a RecursionError -- see
CLAUDE.md).
"""

from __future__ import annotations

import argparse
import json
import os
import struct
import sys
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

# Sibling imports (sharc_disasm, sharcinv) need tools/ on sys.path; do them
# now, before the guard below strips tools/ for pyghidra's sake. Python
# auto-adds this file's own directory to sys.path when run as a script, but
# not when loaded by path (e.g. tests/test_sharcemu.py), so make it explicit.
_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

from sharc_disasm import Instruction, disassemble  # noqa: E402
from sharcinv import merge_fields  # noqa: E402
import sharc_trace as _sharc_trace  # noqa: E402  (PypcodeConcreteBackend's CALLOTHER bridge)

# tools/ghidraq.py's guard, copied exactly: tools/ghidra/ is a plain
# directory of Java scripts that shadows the real `ghidra` Java-bridge
# namespace PyGhidra needs, once tools/ lands on sys.path -- which it does
# when this is run as `python tools/sharcemu.py`. Strip it before anything
# imports pyghidra (done lazily, inside main(), well after this point).
sys.path[:] = [p for p in sys.path if os.path.abspath(p or '.') != _here]

DEFAULT_PROJECT = os.path.expanduser('~/ghidra-projects/sharc-batch-dt2-116')
DEFAULT_PROJECT_NAME = 'sharc-batch-dt2-116'
DEFAULT_GHIDRA = '/opt/homebrew/Cellar/ghidra/12.1.3/libexec'

# Forms whose generated SLEIGH is correctly empty (docs/findings/05, final
# section): Type21a is an architectural NOP with no fields at all; Type9a_abs
# and Type9b_abs with merged field b==1 are register-indirect calls, which
# have no static jump target to encode as p-code. b==0 (absolute-address
# call) is expected to carry real semantics and is NOT exempt.
EXEMPT_NOP_FORM = '21a'
EXEMPT_INDIRECT_CALL_FORMS = ('9a_abs', '9b_abs')

WATCH_DEFAULT_LENGTH = 4  # bytes; one DM word in this LE:32 language
POKE_WORD_BYTES = 4


# --- coordinate / CLI-value parsing (no JVM needed) -------------------------

def parse_coordinate(token: str) -> Tuple[int, Optional[int]]:
    """Parse an ADDR token: 'sw:0x1c18a6' (SHARC short-word address) or a
    plain displayed word address, either in hex (0x...) or decimal.

    -> (displayed, sw): sw is the short-word address for an `sw:` token, or
    for a plain even displayed value (displayed // 2); None for an odd
    displayed value, which is not a whole short word.
    """
    text = token.strip()
    is_sw = text.startswith('sw:')
    number = text[3:] if is_sw else text
    try:
        value = int(number, 0)
    except ValueError:
        raise ValueError('invalid coordinate %r' % token)
    if value < 0:
        raise ValueError('coordinate must be non-negative: %r' % token)
    displayed = value * 2 if is_sw else value
    sw = value if is_sw else (displayed // 2 if displayed % 2 == 0 else None)
    return displayed, sw


def parse_watch(token: str) -> Tuple[int, int]:
    """--watch ADDR[:LEN] -> (displayed_address, length_bytes).

    ADDR may itself be an `sw:...` token, so the optional `:LEN` suffix is
    split off first rather than on the first ':' in the whole token.
    """
    is_sw = token.startswith('sw:')
    body = token[3:] if is_sw else token
    addr_text, sep, length_text = body.partition(':')
    displayed, _ = parse_coordinate(('sw:' + addr_text) if is_sw else addr_text)
    length = int(length_text, 0) if sep else WATCH_DEFAULT_LENGTH
    if length <= 0:
        raise ValueError('watch length must be positive: %r' % token)
    return displayed, length


def _parse_assignment(token: str, what: str) -> Tuple[str, int]:
    name, sep, value_text = token.partition('=')
    if not sep or not name:
        raise ValueError('%s wants NAME=VALUE, got %r' % (what, token))
    try:
        value = int(value_text, 0)
    except ValueError:
        raise ValueError('invalid value in %s %r' % (what, token))
    return name, value


def parse_set(token: str) -> Tuple[str, int]:
    """--set REG=VALUE -> (register_name, value)."""
    return _parse_assignment(token, '--set')


def parse_poke(token: str) -> Tuple[int, int]:
    """--poke ADDR=VALUE -> (displayed_address, value)."""
    addr_text, value = _parse_assignment(token, '--poke')
    displayed, _ = parse_coordinate(addr_text)
    return displayed, value


# --- independent-decoder exemption classification ---------------------------

@dataclass
class Exemption:
    exempt: bool
    decoder_type_name: Optional[str]
    note: str


def classify_pcode_exemption(insn_bytes: bytes, ghidra_mnemonic: Optional[str]) -> Exemption:
    """Classify a zero-p-code instruction's raw bytes with the repo's own
    SHARC+ decoder, independent of Ghidra's mnemonic. Only a *confident*
    Type21a, or a confident Type9a_abs/Type9b_abs with merged field b==1,
    are legitimately empty; everything else -- including a decode our own
    tables cannot confirm -- is reported as not exempt, so an unconfirmed
    match never silently passes as a known-safe no-op.
    """
    try:
        instr: Instruction = next(disassemble(insn_bytes, count=1))
    except StopIteration:
        return Exemption(False, None,
                          'repo decoder produced no instruction from %d byte(s)' % len(insn_bytes))
    if instr.type_name in (None, 'unknown'):
        return Exemption(False, instr.type_name,
                          'repo decoder could not classify these bytes: %s' % instr.note)
    if instr.kind != 'confident':
        return Exemption(False, instr.type_name,
                          'repo decoder matched Type%s but only as %r (source: %s), not confident enough to exempt'
                          % (instr.type_name, instr.kind, instr.note))
    if instr.type_name == EXEMPT_NOP_FORM:
        return Exemption(True, instr.type_name, 'Type21a is an architectural NOP with no fields')
    if instr.type_name in EXEMPT_INDIRECT_CALL_FORMS:
        merged = merge_fields(instr.fields)
        b = merged.get('b')
        if b == 1:
            return Exemption(True, instr.type_name,
                              'Type%s with merged field b=1 is a register-indirect call, correctly empty'
                              % instr.type_name)
        return Exemption(False, instr.type_name,
                          'Type%s with merged field b=%r is an absolute-address call and is expected to have semantics'
                          % (instr.type_name, b))
    return Exemption(False, instr.type_name,
                      'Type%s has empty p-code (Ghidra mnemonic %r) and is not a known-exempt form'
                      % (instr.type_name, ghidra_mnemonic))


# --- run result data model ---------------------------------------------------

@dataclass
class InstrInfo:
    mnemonic: Optional[str]
    length_bytes: int
    pcode_op_count: int
    raw_bytes: bytes


@dataclass
class WatchHit:
    address: int
    before: bytes
    after: bytes


@dataclass
class StepOutcome:
    ok: bool
    message: Optional[str]
    writes: List[WatchHit] = field(default_factory=list)


@dataclass
class Fault:
    index: int
    kind: str  # 'no-semantics' | 'emulator-error'
    pc_sw: int
    pc_displayed: int
    bytes_hex: str
    ghidra_mnemonic: Optional[str]
    decoder_type_name: Optional[str]
    message: str

    def to_dict(self) -> dict:
        return {
            'index': self.index, 'kind': self.kind,
            'pc_sw': '0x%x' % self.pc_sw, 'pc_displayed': '0x%x' % self.pc_displayed,
            'bytes': self.bytes_hex, 'ghidra_mnemonic': self.ghidra_mnemonic,
            'decoder_type_name': self.decoder_type_name, 'message': self.message,
        }


@dataclass
class Write:
    step: int
    pc_sw: int
    pc_displayed: int
    address: int
    before_hex: str
    after_hex: str

    def to_dict(self) -> dict:
        return {
            'step': self.step, 'pc_sw': '0x%x' % self.pc_sw, 'pc_displayed': '0x%x' % self.pc_displayed,
            'address': '0x%x' % self.address, 'before': self.before_hex, 'after': self.after_hex,
        }


@dataclass
class RunOutcome:
    steps_executed: int
    stop_reason: str
    faults: List[Fault]
    writes: List[Write]


# --- backend-agnostic driver (testable without a JVM) ------------------------

class Backend:
    """Duck-typed interface run_emulation() needs; see GhidraBackend for the
    live implementation and tests/test_sharcemu.py for a fake one.

    set_pc_sw(sw), get_pc_sw() -> int
    instruction_at(displayed) -> InstrInfo | None   (None: PC left mapped code)
    step_and_watch() -> StepOutcome
    read_register(name) -> int
    write_register(name, value)
    write_memory(displayed, data: bytes)
    write_tracking_mechanism: str
    """


def run_emulation(backend, start_sw: int, max_steps: int, skip_faults: int = 0,
                   pokes: Sequence[Tuple[int, int]] = (), sets: Sequence[Tuple[str, int]] = ()) -> RunOutcome:
    """Drive `backend` from `start_sw`, applying `pokes`/`sets` first.

    Before every step, an instruction with zero p-code ops is checked against
    classify_pcode_exemption(); a non-exempt one is a `no-semantics` fault. A
    step() that fails (returns false or raises) is an `emulator-error` fault.
    Either fault stops the run unless fewer than `skip_faults` faults have
    been recorded yet, in which case PC is advanced past the instruction and
    the run continues. PC leaving Ghidra's mapped/decoded code is a stop, not
    a fault (there is nothing at that address to classify or execute).
    """
    for addr, value in pokes:
        backend.write_memory(addr, struct.pack('<I', value & 0xFFFFFFFF))
    for name, value in sets:
        backend.write_register(name, value)

    backend.set_pc_sw(start_sw)

    faults: List[Fault] = []
    writes: List[Write] = []
    steps_executed = 0
    stop_reason: Optional[str] = None

    while steps_executed < max_steps:
        pc_sw = backend.get_pc_sw()
        pc_displayed = pc_sw * 2
        info = backend.instruction_at(pc_displayed)
        if info is None:
            stop_reason = 'pc-left-program'
            break

        if info.pcode_op_count == 0:
            exemption = classify_pcode_exemption(info.raw_bytes, info.mnemonic)
            if not exemption.exempt:
                faults.append(Fault(len(faults), 'no-semantics', pc_sw, pc_displayed,
                                     info.raw_bytes.hex(), info.mnemonic,
                                     exemption.decoder_type_name, exemption.note))
                if len(faults) > skip_faults:
                    stop_reason = 'no-semantics'
                    break
                backend.set_pc_sw(pc_sw + max(info.length_bytes, 2) // 2)
                continue

        outcome = backend.step_and_watch()
        if not outcome.ok:
            faults.append(Fault(len(faults), 'emulator-error', pc_sw, pc_displayed,
                                 info.raw_bytes.hex(), info.mnemonic, None,
                                 outcome.message or 'step() returned false'))
            if len(faults) > skip_faults:
                stop_reason = 'emulator-error'
                break
            backend.set_pc_sw(pc_sw + max(info.length_bytes, 2) // 2)
            continue

        steps_executed += 1
        for hit in outcome.writes:
            writes.append(Write(steps_executed, pc_sw, pc_displayed, hit.address,
                                 hit.before.hex(), hit.after.hex()))

    if stop_reason is None:
        stop_reason = 'max-steps'
    return RunOutcome(steps_executed, stop_reason, faults, writes)


# --- Ghidra-free concrete p-code interpreter ---------------------------------
#
# GhidraBackend (below) drives Ghidra's own EmulatorHelper over a live
# pyghidra project. This second backend needs neither: it lifts each
# instruction directly with pypcode.Context.translate() against the in-tree
# generated SHARC_VISA language and interprets the p-code itself -- a small
# generic VM for arithmetic/logic/branch ops, plus a CALLOTHER bridge for
# the three opaque pcodeops the generator emits instead of real semantics
# (tools/sharcspec/ghidra/gen_sleigh.py):
#
#   condition(code)  PGR Table 10-4 predicate; reimplemented here as a
#                     concrete lookup against a plain-int ASTATX register
#                     (tools/sharc_trace.py's _predicate()/_lt_ge_le_gt(),
#                     minus its Value/Const/Unknown symbolic machinery,
#                     which this always-concrete backend does not need).
#   compute(raw)      the 23-bit parallel-compute field. SLEIGH gives it NO
#                     effect ("compute models no effect at all" --
#                     gen_sleigh.py's compute_marker_lines docstring): the
#                     generated language only ever gets an instruction's
#                     ADDRESSING/control-flow right, never its ALU result.
#                     Bridged here to tools/sharc_trace.py's own _compute(),
#                     which already decodes this exact field against public
#                     PRM/PGR tables, instead of re-deriving ALU semantics a
#                     second time.
#   circular(index)   post-modify index-register update when L[i] != 0; not
#                     modelled (matches the generator's own disclaimer).
#                     This backend requires L[i] == 0 for every post-modify
#                     access it executes and raises Fault otherwise, rather
#                     than silently computing a wrong wrapped address.
#
# Coverage note (docs/findings/06, "semantics coverage"): a non-empty pypcode
# lift means an instruction's addressing/control-flow is modelled -- NOT
# that its arithmetic effect is, because `compute` is exactly such an opaque
# marker. This backend is the first thing that actually *executes* SHARC+
# ALU results through pypcode-lifted code, by piping that one marker to
# already-validated Python semantics instead of pretending SLEIGH has them.
#
# The DM/PM `ram` space is wordsize=2 (tools/sharcspec/ghidra/gen_sleigh.py
# dm_byte_addr_to_ram_unit(): "Ghidra always scales a LOAD/STORE offset by
# the space's wordsize"), so a LOAD/STORE's address operand is in that
# space's own units, not raw bytes; recovered here by multiplying back by 2,
# matching what a real p-code executor does internally.

# CALLOTHER user-op indices, assigned by SLEIGH in the `define pcodeop ...;`
# declaration order in gen_sleigh.py (condition, compute, circular) --
# confirmed empirically against a live translate() (see tests), not merely
# assumed.
PYCODE_USEROP_CONDITION = 0
PYCODE_USEROP_COMPUTE = 1
PYCODE_USEROP_CIRCULAR = 2

# ASTATX bit positions (PRM ch.4), duplicated from tools/sharc_trace.py as
# plain ints since this interpreter's ASTATX is a concrete 32-bit register,
# not a sharc_trace.py Value.
PYCODE_AZ_BIT, PYCODE_AN_BIT, PYCODE_AV_BIT, PYCODE_AF_BIT = 0, 2, 1, 6


def _pypcode_register_offsets():
    """-> {ureg code: register-space byte offset}, replicating
    gen_sleigh.py's `define register offset=... [...]` emission order so
    ureg codes resolve to the exact same offsets the generated language
    uses. One literal table (instead of re-deriving it) so a change to that
    emission is a visible diff here too, not a silent mismatch."""
    st = _sharc_trace
    offsets = {}

    def block(names, base):
        for i, name in enumerate(names):
            offsets[st.UREG_CODES[name]] = base + 4 * i

    block(['R%d' % i for i in range(16)], 0x000)
    block(['I%d' % i for i in range(16)], 0x080)
    block(['M%d' % i for i in range(16)], 0x0C0)
    block(['L%d' % i for i in range(16)], 0x100)
    block(['B%d' % i for i in range(16)], 0x140)
    offsets[st.UREG_CODES['PC']] = 0x180
    block(['S%d' % i for i in range(16)], 0x1C0)
    ureg_registers = list(st.UREG_NAMES)
    for i, name in enumerate(ureg_registers[96:99] + ureg_registers[100:112]):
        offsets[st.UREG_CODES[name]] = 0x200 + 4 * i
    for i, name in enumerate(ureg_registers[112:]):
        offsets[st.UREG_CODES[name]] = 0x240 + 4 * i
    return offsets


class PypcodeFault(Exception):
    """A pypcode-native step could not be executed: no semantics (empty
    lift), or an opcode/CALLOTHER this draft interpreter does not model."""


class PypcodeConcreteBackend:
    """Concrete p-code interpreter over a flat SHARC+ image, no Ghidra.

    `code`: bytes of the region to execute; `code_base`: the DISPLAYED
    (byte) address code[0] sits at (the same convention as --start below,
    and as tools/sharcpcode.py's `2 * sw`).
    """

    def __init__(self, code, code_base):
        import pypcode  # local: keep this module importable without pypcode
        self._pypcode = pypcode
        arch = pypcode.Arch('SHARC_VISA', os.path.join(
            os.path.dirname(os.path.abspath(__file__)),
            'sharcspec/ghidra/SHARC_VISA/data/languages/sharc_visa.ldefs'))
        self.ctx = pypcode.Context(arch.languages[0])
        self.code = code
        self.code_base = code_base
        self.mem = {}    # ram-space byte address -> byte
        self.regs = {}   # register-space offset -> 4-byte value
        self.pc = code_base
        self.trace = []
        self.max_bytes = 32
        self._offsets = _pypcode_register_offsets()

    def poke(self, address, value, width=4):
        for i in range(width):
            self.mem[address + i] = (value >> (8 * i)) & 0xFF

    def peek(self, address, width):
        out = 0
        for i in range(width):
            b = self.mem.get(address + i)
            if b is None:
                off = (address + i) - self.code_base
                b = self.code[off] if 0 <= off < len(self.code) else 0
            out |= b << (8 * i)
        return out

    def set_ureg(self, name, value):
        st = _sharc_trace
        self.regs[self._offsets[st.UREG_CODES[name]]] = value & 0xFFFFFFFF

    def get_ureg(self, name):
        st = _sharc_trace
        return self.regs.get(self._offsets[st.UREG_CODES[name]], 0)

    def _space_name(self, space):
        return str(space.name)

    def read_vn(self, vn, unique):
        name = self._space_name(vn.space)
        if name == 'const':
            return vn.offset
        if name == 'unique':
            return unique.get(vn.offset, 0) & ((1 << (8 * vn.size)) - 1)
        if name == 'register':
            return self.regs.get(vn.offset & ~3, 0) & ((1 << (8 * vn.size)) - 1)
        if name == 'ram':
            return self.peek(vn.offset, vn.size)
        raise PypcodeFault('unhandled read space %r' % name)

    def write_vn(self, vn, value, unique):
        name = self._space_name(vn.space)
        value &= (1 << (8 * vn.size)) - 1
        if name == 'unique':
            unique[vn.offset] = value
        elif name == 'register':
            self.regs[vn.offset & ~3] = value
        elif name == 'ram':
            self.poke(vn.offset, value, vn.size)
        else:
            raise PypcodeFault('unhandled write space %r' % name)

    def _predicate(self, code):
        astatx = self.get_ureg('ASTATX')

        def bit(n):
            return bool((astatx >> n) & 1)

        if code == 0x1F:
            return True
        if code in (0x00, 0x10):
            return bit(PYCODE_AZ_BIT) if code == 0x00 else not bit(PYCODE_AZ_BIT)
        if code in (0x01, 0x02, 0x11, 0x12):
            af, an, az = bit(PYCODE_AF_BIT), bit(PYCODE_AN_BIT), bit(PYCODE_AZ_BIT)
            if af:
                x, y = (an or az), (an and not az)
            else:
                av = bit(PYCODE_AV_BIT)
                term = an if not av else not an  # ALUSAT (MODE1) not tracked here
                x, y = (term or az), term
            if code in (0x02, 0x12):
                return x if code == 0x02 else not x
            return y if code == 0x01 else not y
        raise PypcodeFault('condition() code %#x not modelled by this draft' % code)

    def _compute_bridge(self, raw):
        st = _sharc_trace
        f = {'compute[22:16]': (raw >> 16) & 0x7F, 'compute[15:0]': raw & 0xFFFF}
        values = {code: st.Const(self.regs.get(off, 0)) for code, off in self._offsets.items()}
        result = st._compute(f, short=False, values=values)
        if result is None:
            return
        rn, value, operation, astatx_update = result
        old = st.Const(self.get_ureg('ASTATX'))
        new_astatx = astatx_update(old)
        if isinstance(new_astatx, st.Const):
            self.set_ureg('ASTATX', new_astatx.value)
        if isinstance(rn, tuple):
            for reg, val in zip(rn, value):
                if isinstance(val, st.Const):
                    self.regs[self._offsets[reg]] = val.value & 0xFFFFFFFF
        elif isinstance(rn, int) and operation not in ('compare', 'bit-test', 'float-compare'):
            if isinstance(value, st.Const):
                self.regs[self._offsets[rn]] = value.value & 0xFFFFFFFF
        self.trace.append({'action': 'compute', 'operation': operation})

    def lift(self, pc):
        off = pc - self.code_base
        chunk = self.code[off: off + self.max_bytes]
        tr = self.ctx.translate(chunk, base_address=pc, max_instructions=1)
        length, ops = None, []
        for op in tr.ops:
            if op.opcode == self._pypcode.OpCode.IMARK:
                if length is None:
                    length = op.inputs[0].size
            else:
                ops.append(op)
        if length is None:
            raise PypcodeFault('pypcode did not decode anything at %#x' % pc)
        return length, ops

    def step(self):
        """Execute one instruction. Returns False on RETURN (stop)."""
        length, ops = self.lift(self.pc)
        if not ops:
            raise PypcodeFault('no-semantics at %#x (empty p-code)' % self.pc)
        unique = {}
        i = 0
        next_pc = self.pc + length
        while i < len(ops):
            op = ops[i]
            name = op.opcode.name
            ins = op.inputs
            if name == 'COPY':
                self.write_vn(op.output, self.read_vn(ins[0], unique), unique)
            elif name in ('INT_ADD', 'INT_SUB', 'INT_MULT', 'INT_AND', 'INT_OR', 'INT_XOR',
                          'INT_LEFT', 'INT_RIGHT', 'INT_SRIGHT'):
                a, b = self.read_vn(ins[0], unique), self.read_vn(ins[1], unique)
                mask = (1 << (8 * op.output.size)) - 1
                r = {'INT_ADD': a + b, 'INT_SUB': a - b, 'INT_MULT': a * b,
                     'INT_AND': a & b, 'INT_OR': a | b, 'INT_XOR': a ^ b,
                     'INT_LEFT': a << b, 'INT_RIGHT': a >> b, 'INT_SRIGHT': a >> b}[name]
                self.write_vn(op.output, r & mask, unique)
            elif name in ('INT_EQUAL', 'INT_NOTEQUAL', 'INT_LESS', 'INT_LESSEQUAL',
                          'INT_SLESS', 'INT_SLESSEQUAL'):
                a, b = self.read_vn(ins[0], unique), self.read_vn(ins[1], unique)
                r = {'INT_EQUAL': a == b, 'INT_NOTEQUAL': a != b,
                     'INT_LESS': a < b, 'INT_LESSEQUAL': a <= b,
                     'INT_SLESS': a < b, 'INT_SLESSEQUAL': a <= b}[name]
                self.write_vn(op.output, int(r), unique)
            elif name in ('BOOL_AND', 'BOOL_OR', 'BOOL_XOR'):
                a, b = self.read_vn(ins[0], unique), self.read_vn(ins[1], unique)
                r = {'BOOL_AND': a and b, 'BOOL_OR': a or b, 'BOOL_XOR': bool(a) != bool(b)}[name]
                self.write_vn(op.output, int(bool(r)), unique)
            elif name == 'BOOL_NEGATE':
                self.write_vn(op.output, int(not self.read_vn(ins[0], unique)), unique)
            elif name == 'INT_NEGATE':
                self.write_vn(op.output, (~self.read_vn(ins[0], unique)) & ((1 << (8 * op.output.size)) - 1), unique)
            elif name == 'INT_2COMP':
                self.write_vn(op.output, (-self.read_vn(ins[0], unique)) & ((1 << (8 * op.output.size)) - 1), unique)
            elif name in ('INT_ZEXT', 'INT_SEXT'):
                v = self.read_vn(ins[0], unique)
                if name == 'INT_SEXT':
                    bits = 8 * ins[0].size
                    if v & (1 << (bits - 1)):
                        v -= 1 << bits
                self.write_vn(op.output, v & ((1 << (8 * op.output.size)) - 1), unique)
            elif name == 'SUBPIECE':
                v = self.read_vn(ins[0], unique)
                shift = 8 * ins[1].offset if self._space_name(ins[1].space) == 'const' else 0
                self.write_vn(op.output, (v >> shift) & ((1 << (8 * op.output.size)) - 1), unique)
            elif name == 'PIECE':
                hi, lo = self.read_vn(ins[0], unique), self.read_vn(ins[1], unique)
                self.write_vn(op.output, (hi << (8 * ins[1].size)) | lo, unique)
            elif name == 'LOAD':
                unit = self.read_vn(ins[1], unique)
                byte_addr = unit * 2
                v = self.peek(byte_addr, op.output.size)
                self.write_vn(op.output, v, unique)
                self.trace.append({'action': 'load', 'address': byte_addr, 'value': v})
            elif name == 'STORE':
                unit = self.read_vn(ins[1], unique)
                byte_addr = unit * 2
                value = self.read_vn(ins[2], unique)
                self.poke(byte_addr, value, ins[2].size)
                self.trace.append({'action': 'store', 'address': byte_addr, 'value': value})
            elif name == 'CALLOTHER':
                index = ins[0].offset
                if index == PYCODE_USEROP_CONDITION:
                    code = self.read_vn(ins[1], unique)
                    self.write_vn(op.output, int(self._predicate(code)), unique)
                elif index == PYCODE_USEROP_COMPUTE:
                    self._compute_bridge(self.read_vn(ins[1], unique))
                elif index == PYCODE_USEROP_CIRCULAR:
                    raise PypcodeFault('circular() at %#x: post-modify with L[i]!=0 is not modelled' % self.pc)
                else:
                    raise PypcodeFault('unknown CALLOTHER index %d at %#x' % (index, self.pc))
            elif name == 'CBRANCH':
                taken = bool(self.read_vn(ins[1], unique))
                target = ins[0]
                if taken:
                    if self._space_name(target.space) == 'const':
                        i += target.offset
                        continue
                    next_pc = target.offset
                    break
            elif name == 'BRANCH':
                target = ins[0]
                if self._space_name(target.space) == 'const':
                    i += target.offset
                    continue
                next_pc = target.offset
                break
            elif name == 'CALL':
                self.trace.append({'action': 'call', 'target': ins[0].offset})
                next_pc = ins[0].offset
                break
            elif name == 'RETURN':
                self.trace.append({'action': 'return'})
                return False
            elif name == 'BRANCHIND':
                raise PypcodeFault('BRANCHIND at %#x not modelled by this draft' % self.pc)
            else:
                raise PypcodeFault('unhandled opcode %s at %#x' % (name, self.pc))
            i += 1
        self.pc = next_pc
        return True


# --- live Ghidra backend ------------------------------------------------------

class GhidraBackend:
    """Wraps EmulatorHelper for run_emulation(). Prefers
    EmulatorHelper.enableMemoryWriteTracking()/getTrackedMemoryWriteSet()
    (present in Ghidra 12.1.3, confirmed by reflection on EmulatorHelper
    before writing this class) to detect writes to watched addresses by
    address touched, not by value change, so a write that happens to store
    the same value it already held is still reported. Falls back to
    before/after readMemory diffing -- which CANNOT see a same-value write
    -- only if that API is absent.
    """

    def __init__(self, program, emu, monitor, watches: Sequence[Tuple[int, int]]):
        self.program = program
        self.emu = emu
        self.monitor = monitor
        self.listing = program.getListing()
        self.static_mem = program.getMemory()
        self.space = program.getAddressFactory().getDefaultAddressSpace()
        self.pc_reg = emu.getPCRegister()
        self.watches = list(watches)

        self._tracking = hasattr(emu, 'enableMemoryWriteTracking') and hasattr(emu, 'getTrackedMemoryWriteSet')
        self._prev_written = None
        if self._tracking:
            from ghidra.program.model.address import AddressSet  # pyright: ignore[reportMissingImports]
            self._AddressSet = AddressSet
            emu.enableMemoryWriteTracking(True)
            self._prev_written = AddressSet()
            self.write_tracking_mechanism = (
                'EmulatorHelper.enableMemoryWriteTracking/getTrackedMemoryWriteSet '
                '(address-touched; catches a write that stores the same value)')
        else:
            self.write_tracking_mechanism = (
                'before/after EmulatorHelper.readMemory diff '
                '(enableMemoryWriteTracking unavailable; a same-value write is NOT detected)')

        self._baseline: Dict[Tuple[int, int], bytes] = {}
        for addr, length in self.watches:
            self._baseline[(addr, length)] = self.read_memory(addr, length)

    def _addr(self, displayed):
        return self.space.getAddress(displayed)

    def set_pc_sw(self, sw):
        self.emu.writeRegister(self.pc_reg, sw)

    def get_pc_sw(self) -> int:
        # readRegister returns a java.math.BigInteger; JPype does not make it
        # directly int()-able, but its str() is the plain decimal value.
        return int(str(self.emu.readRegister(self.pc_reg)))

    def instruction_at(self, displayed: int) -> Optional[InstrInfo]:
        instr = self.listing.getInstructionAt(self._addr(displayed))
        if instr is None:
            return None
        length = int(instr.getLength())
        mnemonic = str(instr.getMnemonicString())
        raw_ops = instr.getPcode()
        pcode_op_count = 0 if raw_ops is None else len(raw_ops)
        raw_bytes = self._read_static(displayed, max(length, 2))
        return InstrInfo(mnemonic, length, pcode_op_count, raw_bytes)

    def _read_static(self, displayed: int, n: int) -> bytes:
        from jpype import JArray, JByte  # pyright: ignore[reportMissingImports]
        buf = JArray(JByte)(n)
        got = self.static_mem.getBytes(self._addr(displayed), buf)
        return bytes(bytearray(x & 0xFF for x in buf[:got]))

    def read_memory(self, displayed: int, length: int) -> bytes:
        jb = self.emu.readMemory(self._addr(displayed), length)
        return bytes(bytearray(x & 0xFF for x in jb))

    def write_memory(self, displayed: int, data: bytes) -> None:
        from jpype import JArray, JByte  # pyright: ignore[reportMissingImports]
        jb = JArray(JByte)(len(data))
        for i, b in enumerate(data):
            jb[i] = JByte(b - 256 if b > 127 else b)
        self.emu.writeMemory(self._addr(displayed), jb)

    def read_register(self, name: str) -> int:
        return int(str(self.emu.readRegister(name)))

    def write_register(self, name: str, value: int) -> None:
        self.emu.writeRegister(name, value)

    def step_and_watch(self) -> StepOutcome:
        try:
            ok = bool(self.emu.step(self.monitor))
            message = None if ok else str(self.emu.getLastError())
        except Exception as exc:
            ok, message = False, '%s: %s' % (type(exc).__name__, exc)

        hits: List[WatchHit] = []
        if ok:
            if self._tracking:
                # getTrackedMemoryWriteSet() returns a reference to
                # EmulatorHelper's own live, mutating AddressSet -- not a
                # snapshot. Storing that reference directly as _prev_written
                # would alias it, making every later subtract() a set
                # against itself (always empty) once the live set has grown
                # further. Copy it into an independent AddressSet instead.
                current = self.emu.getTrackedMemoryWriteSet()
                new_written = current.subtract(self._prev_written)
                self._prev_written = self._AddressSet(current)
                for addr, length in self.watches:
                    lo, hi = self._addr(addr), self._addr(addr + length - 1)
                    if new_written.intersects(lo, hi):
                        before = self._baseline[(addr, length)]
                        after = self.read_memory(addr, length)
                        hits.append(WatchHit(addr, before, after))
                        self._baseline[(addr, length)] = after
            else:
                for addr, length in self.watches:
                    before = self._baseline[(addr, length)]
                    after = self.read_memory(addr, length)
                    if after != before:
                        hits.append(WatchHit(addr, before, after))
                        self._baseline[(addr, length)] = after
        return StepOutcome(ok, message, hits)


# --- CLI ----------------------------------------------------------------------

def _report_dict(start_sw: int, steps_requested: int, skip_faults: int,
                  outcome: RunOutcome, watches: Sequence[Tuple[int, int]],
                  sets: Sequence[Tuple[str, int]], final_registers: Dict[str, int],
                  write_tracking_mechanism: str) -> dict:
    return {
        'start': {'sw': '0x%x' % start_sw, 'displayed': '0x%x' % (start_sw * 2)},
        'steps_requested': steps_requested, 'steps_executed': outcome.steps_executed,
        'skip_faults': skip_faults, 'stop_reason': outcome.stop_reason,
        'watches': ['0x%x:%d' % (a, n) for a, n in watches],
        'set': ['%s=0x%x' % (n, v) for n, v in sets],
        'write_tracking_mechanism': write_tracking_mechanism,
        'faults': [f.to_dict() for f in outcome.faults],
        'watched_writes': [w.to_dict() for w in outcome.writes],
        'final_registers': {name: '0x%x' % value for name, value in final_registers.items()},
    }


def _print_text(report: dict) -> None:
    print('start: sw=%s displayed=%s' % (report['start']['sw'], report['start']['displayed']))
    print('steps: requested=%d executed=%d skip_faults=%d' %
          (report['steps_requested'], report['steps_executed'], report['skip_faults']))
    print('stop reason: %s' % report['stop_reason'])
    print('write tracking: %s' % report['write_tracking_mechanism'])
    print('watches: %s' % (', '.join(report['watches']) or '(none)'))
    print('\nfaults (%d):' % len(report['faults']))
    for f in report['faults']:
        print('  #%d %-14s pc_sw=%s pc=%s bytes=%s ghidra=%s decoder=%s' %
              (f['index'], f['kind'], f['pc_sw'], f['pc_displayed'], f['bytes'],
               f['ghidra_mnemonic'], f['decoder_type_name']))
        print('      %s' % f['message'])
    print('\nwatched writes (%d):' % len(report['watched_writes']))
    for w in report['watched_writes']:
        print('  step=%-5d pc_sw=%s addr=%s  %s -> %s' %
              (w['step'], w['pc_sw'], w['address'], w['before'], w['after']))
    print('\nfinal registers:')
    for name, value in report['final_registers'].items():
        print('  %s = %s' % (name, value))


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('program', help='program path inside the project, e.g. dt2-1.16_SHARC')
    ap.add_argument('--start', required=True, help='sw:LOGICAL or a displayed word address')
    ap.add_argument('--steps', required=True, type=int, help='maximum instructions to execute')
    ap.add_argument('--watch', action='append', default=[], metavar='ADDR[:LEN]',
                     help='report writes touching this data range (repeatable)')
    ap.add_argument('--set', action='append', default=[], dest='sets', metavar='REG=VALUE',
                     help='seed a register before --start runs (repeatable)')
    ap.add_argument('--poke', action='append', default=[], metavar='ADDR=VALUE',
                     help='seed a 32-bit little-endian DM word before --start runs (repeatable)')
    ap.add_argument('--skip-faults', type=int, default=0, metavar='N',
                     help='record up to N faults and continue past them instead of stopping (default: 0)')
    ap.add_argument('--project', default=DEFAULT_PROJECT)
    ap.add_argument('--project-name', default=DEFAULT_PROJECT_NAME)
    ap.add_argument('--json', action='store_true')
    args = ap.parse_args(argv)

    if args.steps <= 0:
        ap.error('--steps must be positive')
    if args.skip_faults < 0:
        ap.error('--skip-faults must be non-negative')

    try:
        start_displayed, start_sw = parse_coordinate(args.start)
        if start_sw is None:
            ap.error('--start %r is not a whole short-word address' % args.start)
        watches = [parse_watch(w) for w in args.watch]
        sets = [parse_set(s) for s in args.sets]
        pokes = [parse_poke(p) for p in args.poke]
    except ValueError as exc:
        ap.error(str(exc))
        return 2  # pragma: no cover (ap.error exits)

    os.environ.setdefault('GHIDRA_INSTALL_DIR', DEFAULT_GHIDRA)
    import pyghidra  # pyright: ignore[reportMissingImports]
    pyghidra.start(verbose=False)

    program_path = args.program if args.program.startswith('/') else '/' + args.program
    project = pyghidra.open_project(args.project, args.project_name, create=False)
    try:
        with pyghidra.program_context(project, program_path) as program:
            from ghidra.app.emulator import EmulatorHelper  # pyright: ignore[reportMissingImports]
            from ghidra.util.task import TaskMonitor  # pyright: ignore[reportMissingImports]

            emu = EmulatorHelper(program)
            try:
                backend = GhidraBackend(program, emu, TaskMonitor.DUMMY, watches)
                outcome = run_emulation(backend, start_sw, args.steps, args.skip_faults, pokes, sets)
                final_registers = {name: backend.read_register(name) for name, _ in sets}
                write_tracking_mechanism = backend.write_tracking_mechanism
            finally:
                emu.dispose()
    finally:
        project.close()

    report = _report_dict(start_sw, args.steps, args.skip_faults, outcome, watches, sets,
                           final_registers, write_tracking_mechanism)
    if args.json:
        json.dump(report, sys.stdout, indent=2)
        print()
    else:
        _print_text(report)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
