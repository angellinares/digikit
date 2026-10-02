"""Bounded SISD conditional Type6a shift-plus-memory and Type6b shift coverage."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import sharc_trace as st
from sharc_disasm import Instruction
from sharcldr import LoadedMemory


def synthetic_fields(*, store=True):
    """A valid synthetic Type6a store and logical-shift pairing."""
    return {
        "cond[4:0]": 0,
        "g": 0,
        "i[2:0]": 0,
        "m[2:0]": 4,
        "d": int(store),
        "dreg[3:0]": 4,
        "dataex[3:0]": 0,
        "shiftimm[22:16]": 0,
        "shiftimm[15:0]": 0x574,
    }


def fresh_dn2_fields():
    # Values are decimal decoded fields from the 0x1c0e18 sighting.
    return {
        "cond[4:0]": 0,
        "g": 0,
        "i[2:0]": 0,
        "m[2:0]": 4,
        "d": 0,
        "dreg[3:0]": 4,
        "dataex[3:0]": 10,
        "shiftimm[22:16]": 16,
        "shiftimm[15:0]": 574,
    }


def state(astat):
    return st.State(
        0,
        {
            st.UREG_CODES["I0"]: st.Const(0x80000000),
            st.UREG_CODES["M4"]: st.Const(1),
            st.UREG_CODES["R4"]: st.Const(0x11223344),
            st.UREG_CODES["R7"]: st.Const(0xA5A5A5A5),
            st.UREG_CODES["MODE1"]: st.Const(0),  # SISD
            st.UREG_CODES["ASTATX"]: astat,
        },
        concrete=LoadedMemory.from_stream(b""),
        assume_nw32=True,
    )


def execute(initial, f):
    [result] = st._execute(initial, Instruction(0, 6, "6a_mem", f, kind="confident"))
    return result


def test_conditional_eq_true_executes_shift_store_and_postmodify_from_old_values():
    # The captured ASTATX partial constant has a known AZ=1.
    result = execute(state(st.PartialConst(0x00FFFFFF, 0x00041801)), synthetic_fields())
    assert result.stopped is None
    assert result.uregs[st.UREG_CODES["R7"]] == st.Const(0x24466880)
    assert result.uregs[st.UREG_CODES["I0"]] == st.Const(0x80000004)
    assert result.overlay == {
        0x80000000: 0x44,
        0x80000001: 0x33,
        0x80000002: 0x22,
        0x80000003: 0x11,
    }


def test_conditional_eq_false_skips_invalid_shift_and_unmapped_memory_without_effects():
    initial = state(st.Const(0))  # known AZ=0
    before = dict(initial.uregs)
    bad = synthetic_fields()
    bad["shiftimm[22:16]"] = 0x7F  # would be rejected if decoded
    del initial.uregs[st.UREG_CODES["I0"]]  # would make a memory access unknown
    result = execute(initial, bad)
    assert result.stopped is None
    assert result.pc_sw == 3
    assert result.uregs == {k: v for k, v in before.items() if k != st.UREG_CODES["I0"]}
    assert result.overlay == {}
    assert result.trace[-1]["action"] == "type6a-skipped"


def test_unknown_eq_predicate_stops_without_shift_dag_or_memory_changes():
    initial = state(st.Unknown("AZ unknown"))
    before = dict(initial.uregs)
    result = execute(initial, synthetic_fields())
    assert result.stopped == "unknown conditional Type6a predicate"
    assert result.pc_sw == 0
    assert result.uregs == before
    assert result.overlay == {}


def test_parallel_store_reads_old_shift_destination_value():
    initial = state(st.PartialConst(0x00FFFFFF, 0x00041801))
    # The approved Type6a body snapshots old UREGs before its shift writes.
    # Make DREG share ShiftImm's R7 destination: memory gets the old R7,
    # while the compute event then replaces R7 with its shift result.
    f = synthetic_fields()
    f["dreg[3:0]"] = 7
    result = execute(initial, f)
    assert result.overlay == {
        0x80000000: 0xA5,
        0x80000001: 0xA5,
        0x80000002: 0xA5,
        0x80000003: 0xA5,
    }
    assert result.uregs[st.UREG_CODES["R7"]] == st.Const(0x24466880)


@pytest.mark.parametrize("mode", [st.Const(1 << 21), st.Unknown("MODE1 unknown")])
def test_conditional_simd_or_unknown_mode_stays_fail_closed(mode):
    initial = state(st.PartialConst(0x00FFFFFF, 0x00041801))
    initial.uregs[st.UREG_CODES["MODE1"]] = mode
    before = dict(initial.uregs)
    result = execute(initial, synthetic_fields())
    assert result.stopped == "unsupported conditional SIMD Type6a"
    assert result.uregs == before
    assert result.overlay == {}


def test_fresh_dn2_decimal_fields_take_the_known_eq_sisd_path():
    result = execute(state(st.PartialConst(0x00FFFFFF, 0x00041801)), fresh_dn2_fields())
    assert result.stopped is None
    assert result.pc_sw == 3
    assert result.uregs[st.UREG_CODES["I0"]] == st.Const(0x80000004)
    assert result.trace[0]["action"] == "load"
    assert result.trace[1]["operation"] == "field-extract-immediate"


@pytest.mark.parametrize("az", [0, 1])
@pytest.mark.parametrize("fields", [synthetic_fields(), fresh_dn2_fields()])
def test_native_conditional_shift_matches_reference(az, fields):
    import os

    import sharc_diff as sd
    import sharc_transpile_run as nr

    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("no native library built")
    core = nr.NativeCore(nr.pack_image(None), library)
    initial = state(st.Const(az))
    nr.to_native(core, initial)
    reference = execute(initial, fields)
    assert core.exec_insn(nr.pack_insn("6a_mem", 6, "confident", fields))
    assert sd.compare_states(sd.export_state(reference), core.export_state()) == []


def execute_6b(initial, f):
    [result] = st._execute(
        initial, Instruction(0, 6, "6b_shiftimm", f, kind="confident")
    )
    return result


def synthetic_6b_fields(cond):
    # Same synthetic logical shift as the Type6a tests, no memory transfer.
    return {
        "cond[4:0]": cond,
        "dataex[3:0]": 0,
        "shiftimm[22:16]": 0,
        "shiftimm[15:0]": 0x574,
    }


def test_conditional_type6b_true_executes_and_false_skips():
    # cond 0x00 is EQ (AZ). Known AZ=1 executes the shift of R4 into R7.
    ran = execute_6b(
        state(st.PartialConst(0x00FFFFFF, 0x00041801)), synthetic_6b_fields(0)
    )
    assert ran.stopped is None
    assert ran.uregs[st.UREG_CODES["R7"]] == st.Const(0x24466880)
    assert ran.trace[-1]["action"] == "compute"
    # Known AZ=0 skips: no register changes, even for an invalid shift opcode.
    initial = state(st.Const(0))
    before = dict(initial.uregs)
    bad = synthetic_6b_fields(0)
    bad["shiftimm[22:16]"] = 0x7F
    skipped = execute_6b(initial, bad)
    assert skipped.stopped is None
    assert skipped.pc_sw == 3
    assert skipped.uregs == before
    assert skipped.trace[-1]["action"] == "type6b-skipped"


def test_conditional_type6b_unknown_or_simd_predicate_stops_unchanged():
    initial = state(st.Unknown("AZ unknown"))
    before = dict(initial.uregs)
    result = execute_6b(initial, synthetic_6b_fields(0))
    assert result.stopped == "unknown conditional Type6b predicate"
    assert result.uregs == before
    simd = state(st.Const(0))
    simd.uregs[st.UREG_CODES["MODE1"]] = st.Const(1 << 21)
    assert execute_6b(simd, synthetic_6b_fields(0)).stopped == (
        "unsupported conditional SIMD Type6b"
    )
