"""Interned runtime symbols, the boundary-call table and the fixed fact and call tables."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from typing import Any

from .types import (
    BOOL,
    CONCRETE,
    CONFIG_MAP,
    ERASED,
    FIELDS,
    FLOAT,
    INT,
    OPT,
    REC,
    STR,
    VAL,
    T,
)

# ---------------------------------------------------------------------------
# Strings
# ---------------------------------------------------------------------------


# Strings the runtime (packages/emulator/sharc-rt/src/rt.rs RT_SYMS) knows by number:
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
    "operand",
    "operand[6:0]",
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
            "// Interned strings of the SHARC+ core (tools/sharc_dev/sharc_transpile.py).",
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
    "_bank_codes",
    "_bank_complete",
    "_bank_hold_request",
    "_pc_stack_complete",
]:
    BOUNDARY["sharc_core.state." + _n] = Bnd()
BOUNDARY["sharc_core.state._bank_request"] = Bnd(traps=True)
BOUNDARY["sharc_core.state._pc_stack_request"] = Bnd(traps=True)
# Hand-written twins in packages/emulator/sharc-rt/src/rt/periph.rs, reached only from the
# memory boundary and the host.
for _n in [
    "_mmr",
    "_set",
    "_sstat",
    "_sec_raise",
    "_sec_arbitrate",
    "_sec_ack",
    "_sec_end",
    "_sec_line",
    "_signed32",
    "_ram_word",
    "_dma_fetch",
    "_dma_start",
    "_dma_irq",
    "_sport_running",
    "_dma_done",
    "_periph_read",
    "_periph_write",
]:
    BOUNDARY["sharc_core.periph." + _n] = Bnd()
# Hand-written twins in packages/emulator/sharc-rt/src/rt/iir.rs, reached only from periph.
for _n in [
    "_f32",
    "_bits",
    "_r",
    "_mmr",
    "_set",
    "_word_read",
    "_word_write",
    "_step",
    "_load_tcb",
    "_coefficients",
    "_save_state",
    "_window",
    "_channel_done",
    "_run",
    "_iir_read",
    "_iir_write",
]:
    BOUNDARY["sharc_core.iir." + _n] = Bnd()
# Hand-written twins in packages/emulator/sharc-rt/src/rt/fir.rs, reached only from periph.
for _n in [
    "_mmr",
    "_coefficients",
    "_window",
    "_channel",
    "_channel_done",
    "_run",
    "_fir_read",
    "_fir_write",
]:
    BOUNDARY["sharc_core.fir." + _n] = Bnd()
for _n in ["_concrete_address", "_byte_present"]:
    BOUNDARY["sharc_core.memory." + _n] = Bnd()
# The address table: one out-of-line range walk in the runtime instead of a
# copy at every use (packages/emulator/sharc-rt/src/addressing.rs).
BOUNDARY["sharc_core.addressing.normal_word_to_byte"] = Bnd()
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
    "_float32",
    "_float32_result",
]:
    BOUNDARY["sharc_core.floats." + _n] = Bnd()
# The multifunction multiply: one native body, a fast path for the usual
# operands and the original sequence for the rest (bnd.rs).
BOUNDARY["sharc_core.compute_multi._float_multiply_parts"] = Bnd()

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
# pc_sw): the block's entry guard compares them (tools/sharc_dev/sharc_rsgen.py), so
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
# Block code: fact "loop_synced" says LADDR/CURLCNTR already hold what
# _execute's leading _sync_empty_loop_registers would write (the previous
# instruction of the block synced them and changed neither them nor the loop
# stack), so that first call is left out (tools/sharc_dev/sharc_rsgen.py plan_body).
LOOP_SYNC = "sharc_core.state._sync_empty_loop_registers"
LOOP_SYNCED_FACT = "loop_synced"

# The flag registers (ASTATX, ASTATY, STKYX, STKYY) were PartialConst in the
# early cores (the CACC bits, and STKY once a status restore brought in an
# unknown value), so block code kept their known-bit masks at run time. In the
# coupled DN2 runs they are fully known at every region entry (measured on
# trig1-taps, kb-chord16 and FX scenarios, docs/design/aot-mask-free.md
# section 7), so block code now requires them known at entry like any other
# register it reads, and a flag update that would leave a bit unknown is
# rf_set's cold exit. PARTIAL_MASKS and tools/sharc_dev/sharc_rsgen.py REQUIRE_MASKS
# remain for an image whose flags are known to carry a usual partial mask.
PARTIAL_REGS = frozenset({118, 119, 120, 121})
PARTIAL_MASKS = {118: 0x00FFFFFF, 119: 0xFFFFFFFF, 120: 0xFFFFFFFF, 121: 0xFFFFFFFF}

# floats._float_binary called with one of these operations (and
# floats._float_multiply) is replaced by a hand-written fast path in
# packages/emulator/sharc-rt/src/rt/bnd.rs (Transpiler.fast_float_call).
FAST_FLOAT_BINARY = {
    "sharc_core.floats._f_add": "_float_binary_add",
    "sharc_core.floats._f_sub": "_float_binary_sub",
}

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
    "stack_model": (BOOL, "s.cfg.stack_model", False),
    "pc_stack_pending": (INT, "s.pc_stack_pending", False),
    "loop_depth": (INT, "s.loop_depth", True),
    "bank_model": (BOOL, "s.cfg.bank_model", False),
    "core_timer": (BOOL, "s.cfg.core_timer", False),
    "timer_written": (BOOL, "s.timer_written", True),
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
    "provisional_interpreted",
    "provisional_used",
    "trace",
}


# The run configuration block code is generated for (State fields that do
# not change during a run; sharc_harness._make_runner's). Block code checks
# it at entry and leaves anything else to the one-instruction interpreter.
GEN_CFG_DEFAULT: dict[str, Any] = {
    "follow_loaded_calls": True,
    "continue_external_calls": False,
    "max_call_depth": 64,
    "assume_nw32": True,
    "explicit_memory_model": True,
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
