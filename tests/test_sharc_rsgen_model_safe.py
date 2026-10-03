"""Which instructions block code may run with the native core's models on.

tools/sharc_rsgen.py classifies each instruction (model_unsafe_reason): block
code does not do what the engine does between instructions (bank and PC-stack
completion, the core timer, interrupt entry, EMUCLK, peripheral stepping), so
a block may only hold instructions that cannot make those matter. The tests
pin the classification on decoded fields, with no firmware.
"""

from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_rsgen as rg
from sharc_core.encoding import UREG_CODES


def _insn(type_name: str, **fields: int) -> SimpleNamespace:
    return SimpleNamespace(type_name=type_name, fields=dict(fields))


def _reason(type_name: str, **fields: int) -> str | None:
    return rg.model_unsafe_reason(_insn(type_name, **fields), 0x1000, frozenset())


def test_compute_and_dag_forms_are_model_safe() -> None:
    for form in ("2a", "2c", "1a", "7a", "19a"):
        assert _reason(form) is None, form


def test_register_moves_that_name_model_state_are_not() -> None:
    codes = UREG_CODES
    # 14a: DM(I,M) <-> ureg; the ureg field names the register.
    assert _reason("14a", **{"ureg[6:0]": codes["R3"]}) is None
    for name in ("MODE1", "MODE2", "IRPTL", "IMASK", "IMASKP", "TCOUNT", "PCSTKP"):
        assert _reason("14a", **{"ureg[6:0]": codes[name]}) == "register " + name
    assert _reason("17a", **{"ureg[6:0]": codes["EMUCLK"]}) == "register EMUCLK"
    # 5a_move splits the source register over three fields.
    src = codes["MODE1"]
    fields = {
        "dstureg[6:0]": codes["R0"],
        "srcureghigh[4:0]": src >> 2,
        "srcureglow[1:1]": (src >> 1) & 1,
        "srcureglow[0:0]": src & 1,
    }
    assert _reason("5a_move", **fields) == "register MODE1"
    fields["dstureg[6:0]"] = codes["PCSTK"]
    fields["srcureghigh[4:0]"] = 0
    fields["srcureglow[1:1]"] = 0
    fields["srcureglow[0:0]"] = 0
    assert _reason("5a_move", **fields) == "register PCSTK"


def test_jumps_calls_and_returns_are_safe_aborts_and_ci_are_not() -> None:
    # A call's or return's stack pushes and pops are the core's own steps;
    # only a PCSTKP write asks the engine for a PC-stack completion.
    plain = {"b": 0, "a": 0, "ci": 0, "j": 1, "cond": 0, "pmm": 0}
    assert _reason("9a_abs", **plain) is None
    assert _reason("8a_rel", **plain) is None
    assert _reason("9a_abs", **{**plain, "b": 1}) is None  # call
    assert _reason("8a_rel", **{**plain, "b": 1}) is None  # call
    assert _reason("9a_abs", **{**plain, "a": 1}) is not None  # loop abort
    assert _reason("9a_abs", **{**plain, "ci": 1}) is not None
    ret = {**plain, "cond": 0x1F, "pmm": 6}
    assert _reason("9b_abs", **ret) is None  # the (DB) return idiom
    assert _reason("9a_abs", **{**ret, "j": 0}) is None  # not delayed


def test_rts_cjump_rframe_and_nops_are_safe_rti_is_not() -> None:
    for form in ("11a", "11c"):
        assert _reason(form, x=0, j=1, lr=0) is None, form  # RTS
        assert _reason(form, x=1, j=1, lr=0) == "RTI", form
    for form in ("25a_direct", "25a_pcrel", "25c_rframe", "21a", "21c"):
        assert _reason(form) is None, form


def test_stack_interrupt_and_system_forms_are_not() -> None:
    for form in ("18a", "20a", "22c", "26a"):
        assert _reason(form, x=0, j=0, lr=0) is not None, form
    assert (_reason("21p_undoc16") or "").startswith("form")


def test_do_loops_are_safe_and_their_last_instruction_too() -> None:
    assert _reason("12a_imm") is None
    loop_end = frozenset({0x1000})
    insn = _insn("2a")
    assert rg.model_unsafe_reason(insn, 0x1000, loop_end) is None
    assert rg.LOOP_FORMS == ("12a_imm", "12a_ureg")


def test_ureg_codes_reads_every_register_field() -> None:
    codes = rg.ureg_codes(
        {"ureg[6:0]": 5, "dstureg[6:0]": 9, "dreg": 3, "x": 1},
    )
    assert codes == {5, 9}
