"""Type5b SIMD predicates must select each register-file transfer separately."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import sharc_trace as st
from sharc_disasm import Instruction


def move(state, src, dst):
    fields = {
        "cond[4:0]": 0,
        "srcureghigh[4:0]": src >> 2,
        "srcureglow[1:1]": (src >> 1) & 1,
        "srcureglow[0:0]": src & 1,
        "dstureg[6:0]": dst,
    }
    [result] = st._execute(
        state, Instruction(0, 4, "5b_move", fields, kind="confident")
    )
    return result


def state(x, y):
    return st.State(
        0,
        {
            114: st.Const(1 << 21),
            118: st.Const(x),
            119: st.Const(y),
            0: st.Const(123),
            80: st.Const(456),
            1: st.Const(111),
            81: st.Const(222),
            37: st.Const(789),
        },
    )


@pytest.mark.parametrize("x,y", [(0, 0), (0, 1), (1, 0), (1, 1)])
def test_paired_move_uses_independent_conditions_and_sources(x, y):
    result = move(state(x, y), 0, 1)
    assert result.stopped is None
    assert result.pc_sw == 2
    assert result.uregs[1] == st.Const(123 if x else 111)
    assert result.uregs[81] == st.Const(456 if y else 222)
    assert result.uregs[118] == st.Const(x)
    assert result.uregs[119] == st.Const(y)


def test_shared_source_broadcasts_only_to_selected_destination():
    result = move(state(0, 1), 37, 1)
    assert result.uregs[1] == st.Const(111)
    assert result.uregs[81] == st.Const(789)


@pytest.mark.parametrize("x", [0, 1])
def test_px_broadcast_uses_upper_word_for_both_processing_elements(x):
    initial = state(x, 1)
    initial.uregs[107] = st.Unknown("combined PX")
    initial.uregs[108] = st.Const(0x11223344)
    initial.uregs[109] = st.Const(0x55667788)
    result = move(initial, 107, 1)
    assert result.uregs[1] == st.Const(0x55667788 if x else 111)
    assert result.uregs[81] == st.Const(0x55667788)


def test_shared_destination_ignores_unknown_pey_condition():
    initial = state(1, 0)
    initial.uregs[119] = st.Unknown("unused condition")
    result = move(initial, 0, 37)
    assert result.stopped is None
    assert result.uregs[37] == st.Const(123)


def test_unknown_relevant_condition_stops_without_writing():
    initial = state(1, 0)
    initial.uregs[119] = st.Unknown("unknown condition")
    result = move(initial, 0, 1)
    assert result.stopped == "unknown conditional SIMD Type5b predicate"
    assert result.pc_sw == 0
    assert result.uregs[1] == st.Const(111)
    assert result.uregs[81] == st.Const(222)


@pytest.mark.parametrize("x,y", [(0, 0), (0, 1), (1, 0), (1, 1)])
def test_conditional_compute_uses_preinstruction_predicates_for_each_pe(x, y):
    initial = state(x, y)
    # R1 = R0 + R1 / S1 = S0 + S1; updating AZx must not change
    # the already selected PEy operation.
    [result] = st._execute(
        initial,
        Instruction(
            0,
            6,
            "2a",
            {"cond[4:0]": 0, "compute[22:16]": 0, "compute[15:0]": 0x1101},
            kind="confident",
        ),
    )
    assert result.stopped is None
    assert result.uregs[1] == st.Const(234 if x else 111)
    assert result.uregs[81] == st.Const(678 if y else 222)
    if not x:
        assert result.uregs[118] == st.Const(x)
    if not y:
        assert result.uregs[119] == st.Const(y)


def test_native_conditional_move_and_compute_match_reference():
    import os

    import sharc_diff as sd
    import sharc_transpile_run as nr

    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("no native library built")
    core = nr.NativeCore(nr.pack_image(None), library)
    for x, y in [(0, 0), (0, 1), (1, 0), (1, 1)]:
        for form, fields, length in [
            (
                "5b_move",
                {
                    "cond[4:0]": 0,
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 1,
                },
                4,
            ),
            (
                "2a",
                {"cond[4:0]": 0, "compute[22:16]": 0, "compute[15:0]": 0x1101},
                6,
            ),
        ]:
            initial = state(x, y)
            nr.to_native(core, initial)
            [reference] = st._execute(
                initial, Instruction(0, length, form, fields, kind="confident")
            )
            assert core.exec_insn(nr.pack_insn(form, length, "confident", fields))
            assert (
                sd.compare_states(sd.export_state(reference), core.export_state()) == []
            )


def test_unknown_compute_predicate_stops_before_either_pe_changes():
    initial = state(1, 0)
    initial.uregs[119] = st.Unknown("unknown PEy flags")
    before = dict(initial.uregs)
    [result] = st._execute(
        initial,
        Instruction(
            0,
            6,
            "2a",
            {"cond[4:0]": 0, "compute[22:16]": 0, "compute[15:0]": 0x1101},
            kind="confident",
        ),
    )
    assert result.stopped is not None
    assert result.pc_sw == 0
    assert result.uregs == before
