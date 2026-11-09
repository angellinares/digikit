"""Actual assembled SDK call frames, including native execution and poisoned RAM."""

import os
from pathlib import Path

import pytest

from tools.patchkit import pk_sharc
from tools.sharc_dev import sharc_run as sr
from tools.sharc_dev import sharc_trace as st
from tools.sharc_dev import sharc_transpile_run as nr
from tools.sharc_dev.sharc_core.state import _sync_pc_stack, _sync_status_stack
from tools.sharc_dev.sharcldr import SW_ALIAS_BASE, LoadedMemory

ROOT = Path(__file__).resolve().parents[2]
BASE = 0xB90000
COUNT = 0x08021000
POISON = 0xDEADBEEF


@pytest.fixture(scope="module")
def linked():
    source = "\n".join(
        [
            '#include "dsp/call.inc"',
            ".SECTION/PM seg_pmco;",
            ".GLOBAL _call_test;",
            "_call_test:",
            "DK_CALL(_middle, _after_first);",
            "DK_CALL(_middle, _after_second);",
            "_finished: JUMP _finished;",
            "_middle:",
            "DK_ENTER(1);",
            "DK_SAVE(2, R3);",
            "DK_CALL(_leaf, _after_nested);",
            "DK_RETADDR;",
            "NOP; NOP; NOP;",
            "DK_LEAVE(R3, 2);",
            "_leaf:",
            "DK_ENTER(1);",
            "DK_SAVE(2, R3);",
            f"R0 = DM({COUNT});",
            "R0 = R0 + 1;",
            f"DM({COUNT}) = R0;",
            "DK_RETADDR;",
            "NOP; NOP; NOP;",
            "DK_LEAVE(R3, 2);",
        ]
    )
    return pk_sharc.build(pk_sharc.expand_includes(source, [ROOT / "sdk"]), BASE)[1]


def value(state, name):
    result = state.uregs[st.UREG_CODES[name]]
    assert isinstance(result, st.Const), (name, result)
    return result.value & 0xFFFFFFFF


def word(state, address):
    result = st._dm_read(state, address, 4, normal_word=True)
    assert isinstance(result, st.Const), (address, result)
    return result.value & 0xFFFFFFFF


@pytest.mark.parametrize(
    "stack", [0x002D1600, 0x2009C000], ids=["low-alias", "l2-byte-alias"]
)
def test_repeated_nested_calls_have_real_return_slots_on_python_and_native(
    linked, stack
):
    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("current native library required: set SHARC_NATIVE_LIB")
    memory = LoadedMemory(
        linked.code_boot,
        [
            {
                "target_address": SW_ALIAS_BASE + 2 * BASE,
                "byte_count": len(linked.code_boot),
                "fill": False,
                "payload_offset": 0,
                "payload_len": len(linked.code_boot),
                "index": 0,
            }
        ],
    )
    original_frame = stack + 0x100
    runner = sr.Runner(
        memory,
        linked.symbols["_call_test"],
        regs={
            "I6": original_frame,
            "I7": stack,
            "M5": 0,
            "M6": 1,
            "M7": -1,
            "M14": 1,
        },
        breakpoints=[linked.symbols["_after_first"], linked.symbols["_after_second"]],
    )
    # Architectural stacks force actual indirect return targets. Known poison
    # prevents an unwritten slot passing through the abstract unknown fallback.
    runner.state.stack_model = True
    runner.state.bank_model = True
    runner.state.call_stack = []
    runner.state.pc_stack = []
    _sync_pc_stack(runner.state)
    _sync_status_stack(runner.state)
    for offset in range(0, 128, 4):
        assert st._dm_write(
            runner.state, stack - offset, 4, st.Const(POISON), normal_word=True
        )
    assert st._dm_write(runner.state, COUNT, 4, st.Const(0), normal_word=True)
    native = nr.NativeCore(nr.pack_image(memory), library)
    nr.to_native(native, runner.state)
    native.set_option(nr.OPT_RUNTIME_DECODE, True)
    previous_instructions = 0
    for count, continuation in enumerate(["_after_first", "_after_second"], 1):
        result = runner.run(200)
        assert result.halt.reason == "breakpoint", result.halt
        assert result.final_pc_sw == linked.symbols[continuation]
        executed = result.instructions - previous_instructions
        assert 0 < executed < 200
        previous_instructions = result.instructions
        assert native.run(executed) == executed, native.halt_reason
        assert native.halt_reason is None
        actual = nr.to_python(native, runner.state)
        for state in (runner.state, actual):
            assert state.pc_sw == linked.symbols[continuation]
            assert value(state, "I6") == original_frame
            assert value(state, "I7") == stack
            assert word(state, stack) == original_frame
            assert word(state, stack - 4) == linked.symbols[continuation] - 1
            assert word(state, stack - 12) == stack
            assert word(state, stack - 16) == linked.symbols["_after_nested"] - 1
            assert word(state, COUNT) == count
        runner.breakpoints -= frozenset([linked.symbols[continuation]])


def test_one_argument_call_is_rejected():
    source = pk_sharc.expand_includes(
        "\n".join(
            [
                '#include "dsp/call.inc"',
                ".SECTION/PM seg_pmco;",
                "_old: DK_CALL(_callee);",
                "_callee: RTS;",
            ]
        ),
        [ROOT / "sdk"],
    )
    with pytest.raises(pk_sharc.PkSharcError, match="DK_CALL"):
        pk_sharc.assemble(source)
