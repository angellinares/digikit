"""Architectural PC-stack writes, with synthetic instructions only."""

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import sharc_diff as sd
import sharc_run as sr
import sharc_trace as st
import sharc_transpile_run as nr
import sharcldr
from sharc_disasm import Instruction

PUSH = {
    k: 0
    for k in (
        "lpu",
        "lpo",
        "spu",
        "spo",
        "ppu",
        "ppo",
        "fc",
        "llii",
        "lldwb",
        "lldi",
        "llpwb",
        "llpi",
    )
}
PUSH["ppu"] = 1
RTS = {"cond[4:0]": 31, "x": 0, "j": 0, "lr": 0}


def execute(state, form, fields, length=6):
    instruction = Instruction(0, length, form, fields, kind="confident")
    [result] = st._execute(state, instruction)
    return result


def fresh():
    return sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)


def test_reserved_entry_is_unknown_and_cannot_return():
    state = execute(fresh(), "20a", PUSH)
    assert isinstance(state.uregs[100], st.Unknown)
    assert state.uregs[101] == st.Const(1)
    state = execute(state, "11c", RTS, 2)
    assert state.stopped == "return through unwritten PC stack entry"
    assert len(state.call_stack) == 1


def test_guest_restore_preserves_control_bits_and_returns_to_24bit_address():
    state = execute(fresh(), "20a", PUSH)
    state = execute(
        state, "17a", {"ureg[6:0]": 100, "data[31:16]": 0x301, "data[15:0]": 0x2345}
    )
    assert state.call_stack == [0x03012345]
    assert state.uregs[100] == st.Const(0x03012345)
    state = execute(state, "11c", RTS, 2)
    assert state.pc_sw == 0x12345
    assert state.call_stack == []
    assert state.uregs[100] == st.Const(0x7FFFFFFF)


def test_empty_pcstk_write_has_no_effect_and_pointer_write_stops():
    state = fresh()
    before = state.uregs[100]
    state = execute(state, "17a", {"ureg[6:0]": 100, "data[31:16]": 1, "data[15:0]": 2})
    assert state.uregs[100] == before and not state.call_stack
    state = execute(state, "17b", {"ureg[6:0]": 101, "data[15:0]": 2}, 4)
    assert state.stopped == "guest PCSTKP write is not modeled"


def test_memory_restore_changes_occupied_stack_entry():
    state = execute(fresh(), "20a", PUSH)
    st._dm_write(state, 0x80000000, 4, st.Const(0x01012345))
    state = execute(
        state,
        "14a",
        {
            "addr[31:16]": 0x8000,
            "addr[15:0]": 0,
            "ureg[6:0]": 100,
            "g": 0,
            "l": 0,
            "d": 0,
        },
    )
    assert state.call_stack == [0x01012345]
    assert state.uregs[100] == st.Const(0x01012345)


LIB = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)


@pytest.mark.skipif(not Path(LIB).exists(), reason="no native library built")
def test_native_push_memory_restore_and_return_match_reference():
    state = fresh()
    st._dm_write(state, 0x80000000, 4, st.Const(0x01012345))
    core = nr.NativeCore(nr.pack_image(state.concrete), LIB)
    nr.to_native(core, state)
    for form, length, fields in (
        ("20a", 6, PUSH),
        (
            "14a",
            6,
            {
                "addr[31:16]": 0x8000,
                "addr[15:0]": 0,
                "ureg[6:0]": 100,
                "g": 0,
                "l": 0,
                "d": 0,
            },
        ),
        ("11c", 2, RTS),
    ):
        state = execute(state, form, fields, length)
        assert core.exec_insn(nr.pack_insn(form, length, "confident", fields))
        assert sd.compare_states(sd.export_state(state), core.export_state()) == []


def test_pm_stack_load_reads_the_same_ram_as_dm():
    state = fresh()
    st._dm_write(state, 0x80000000, 4, st.Const(0x12345678))
    state.uregs[28] = st.Const(0x80000000)
    state = execute(
        state,
        "15b",
        {"i[2:0]": 4, "g": 1, "d": 0, "l": 0, "ureg[6:0]": 23, "data[6:0]": 0},
        4,
    )
    assert state.uregs[23] == st.Const(0x12345678)


def test_px_transfer_to_dreg_uses_upper_word_not_stale_combined_value():
    state = fresh()
    state.uregs[107] = st.Unknown("combined PX")
    state.uregs[108] = st.Const(0x11223344)
    state.uregs[109] = st.Const(0x55667788)
    state = execute(
        state,
        "5b_move",
        {
            "srcureghigh[4:0]": 26,
            "srcureglow[1:1]": 1,
            "srcureglow[0:0]": 1,
            "dstureg[6:0]": 12,
            "cond[4:0]": 31,
        },
        4,
    )
    assert state.uregs[12] == st.Const(0x55667788)
