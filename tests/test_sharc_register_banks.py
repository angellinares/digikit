"""MODE1's opt-in alternate UREG bank timing and wire-state contract."""

from __future__ import annotations

import os
import struct
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_diff as sd
import sharc_trace as st
import sharc_transpile_run as nr
from sharc_core.encoding import UREG_CODES
from sharc_core.forms_system import _type_20a
from sharc_core.state import State, _bank_complete, _copy, _write_ureg
from sharc_core.values import Const, Unknown


def _state() -> State:
    state = State(pc_sw=0, bank_model=True)
    for code in range(96):
        state.uregs[code] = Const(code)
        state.bank_alt[code] = Const(1000 + code)
    return state


def _mode(state: State, bits: int) -> None:
    _write_ureg(state, UREG_CODES["MODE1"], Const(bits))


def test_groups_are_independent_and_swap_both_pe_halves() -> None:
    state = _state()
    # SRRFL, SRRFH, and all four DAG quarters in one request.
    _mode(state, 0x4F8)
    _bank_complete(state)
    _bank_complete(state)
    for code in (*range(16), *range(80, 96), *range(16, 80)):
        assert state.uregs[code] == Const(1000 + code)
    assert state.bank_alt[0] == Const(0)
    assert state.bank_alt[80] == Const(80)


@pytest.mark.parametrize(
    "selector,selected",
    [
        (10, (*range(0, 8), *range(80, 88))),
        (7, (*range(8, 16), *range(88, 96))),
        (4, (*range(16, 20), *range(32, 36), *range(48, 52), *range(64, 68))),
        (3, (*range(20, 24), *range(36, 40), *range(52, 56), *range(68, 72))),
        (6, (*range(24, 28), *range(40, 44), *range(56, 60), *range(72, 76))),
        (5, (*range(28, 32), *range(44, 48), *range(60, 64), *range(76, 80))),
    ],
)
def test_each_selector_preserves_every_other_group(selector, selected) -> None:
    state = _state()
    _mode(state, 1 << selector)
    _bank_complete(state)
    _bank_complete(state)
    for code in range(96):
        assert state.uregs[code] == Const(code + (1000 if code in selected else 0))


def test_mode1_has_one_following_instruction_latency_and_back_to_back_writes() -> None:
    state = _state()
    _mode(state, 1 << 10)
    _bank_complete(state)  # MODE1 instruction itself
    assert state.uregs[0] == Const(0)
    _mode(state, 1 << 4)
    _bank_complete(state)  # following instruction: first request takes effect
    assert state.uregs[0] == Const(1000)
    assert state.uregs[16] == Const(16)
    _bank_complete(state)  # second request now takes effect
    assert state.uregs[0] == Const(0)
    assert state.uregs[16] == Const(1016)


def test_unknown_mode1_is_fail_closed_and_forks_do_not_share_shadow_bank() -> None:
    state = _state()
    fork = _copy(state)
    fork.bank_alt[0] = Const(77)
    assert state.bank_alt[0] == Const(1000)
    try:
        _write_ureg(state, UREG_CODES["MODE1"], Unknown("unknown MODE1"))
    except ValueError:
        pass
    else:
        raise AssertionError("unknown MODE1 write must not select a bank")


def test_push_and_pop_status_latch_mode1_bank_selection() -> None:
    state = _state()
    state.uregs[UREG_CODES["MODE1"]] = Const(1 << 10)
    state.uregs[UREG_CODES["MMASK"]] = Const(1 << 10)
    # Type20a only consumes these fields before advancing a two-byte insn.
    from sharc_disasm import Instruction

    insn = Instruction(
        0,
        2,
        "20a",
        {
            key: 0
            for key in (
                "lpu",
                "spu",
                "ppu",
                "lpo",
                "spo",
                "ppo",
                "fc",
                "llii",
                "lldwb",
                "lldi",
                "llpwb",
                "llpi",
            )
        },
    )
    push = dict(insn.fields, spu=1)
    _type_20a(state, insn, push, "20a")
    assert state.bank_pending_mask == 0  # PUSH STS masked SRRFL.
    _bank_complete(state)
    _type_20a(state, insn, dict(insn.fields, spo=1), "20a")
    assert state.bank_pending_mask == 1 << 10  # POP STS restored it.


def test_v2_bank_wire_round_trip_and_v1_defaults_to_disabled_unknown_shadow() -> None:
    state = _state()
    state.bank_active_mask = 1 << 10
    state.bank_pending_mask = 1 << 4
    state.bank_requested_mask = 1 << 3
    state.bank_alt[7] = Unknown("inactive")
    fields = sd.export_state(state)
    restored = sd.unpack_state(sd.pack_state(fields))
    assert restored["bank_model"] is True
    assert restored["bank_active_mask"] == 1 << 10
    assert restored["bank_pending_mask"] == 1 << 4
    assert restored["bank_requested_mask"] == 1 << 3
    assert restored["bank_alt"][7]["kind"] == 0

    v1 = bytearray(sd.pack_state(fields))
    v1[4:8] = struct.pack("<I", 1)
    prefix = len(v1) - (1 + 12 + 96 * 9 + 11)
    restored_v1 = sd.unpack_state(bytes(v1[:prefix]))
    assert restored_v1["bank_model"] is False
    assert restored_v1["bank_pending_mask"] == -1
    assert restored_v1["bank_alt"][0]["kind"] == 0


def test_native_generated_forms_match_reference_for_banks_and_delay_slots() -> None:
    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("no native library built")
    from sharc_disasm import Instruction

    reference = _state()
    reference.uregs[UREG_CODES["MODE1"]] = Const(0)
    core = nr.NativeCore(nr.pack_image(None), library)
    nr.to_native(core, reference)

    def step(form: str, length: int, fields: dict[str, int]) -> None:
        nonlocal reference
        states = st._execute(
            reference, Instruction(0, length, form, fields, kind="confident")
        )
        assert len(states) == 1
        reference = states[0]
        assert reference.stopped is None
        assert core.exec_insn(nr.pack_insn(form, length, "confident", fields))
        assert sd.compare_states(sd.export_state(reference), core.export_state()) == []

    def move(source: int, destination: int) -> tuple[str, int, dict[str, int]]:
        return (
            "5b_move",
            4,
            {
                "cond[4:0]": 31,
                "srcureghigh[4:0]": source >> 2,
                "srcureglow[1:1]": (source >> 1) & 1,
                "srcureglow[0:0]": source & 1,
                "dstureg[6:0]": destination,
            },
        )

    def mode(bits: int) -> tuple[str, int, dict[str, int]]:
        return (
            "17a",
            6,
            {"ureg[6:0]": 114, "data[31:16]": bits >> 16, "data[15:0]": bits & 0xFFFF},
        )

    # Back-to-back MODE1 writes: the second instruction executes with the
    # old view, then switches the low RF; its successor switches to high RF.
    for form, length, fields in (mode(1 << 10), mode(1 << 7), move(0, 1)):
        step(form, length, fields)
    assert reference.bank_active_mask == 1 << 7

    # A delayed branch and its two explicitly executed slots. The first slot
    # requests SRRFL; the second slot still sees the old bank and completes
    # the delayed switch. The following move reads the alternate R0.
    step(
        "8a_abs",
        6,
        {"addr[23:16]": 0, "addr[15:0]": 0x40, "b": 0, "j": 1, "cond[4:0]": 31},
    )
    for form, length, fields in (mode(1 << 10), move(0, 1), move(0, 1)):
        step(form, length, fields)
    assert reference.uregs[1] == Const(1000)
    # The system-bit form is another guest MODE1 writer. Each selection
    # must be latched, rather than changing MODE1 while leaving banks stale.
    for operation in (0, 1, 2):
        step(
            "18a",
            6,
            {"sreg": 2, "bop": operation, "data[31:16]": 0, "data[15:0]": 1 << 7},
        )
        step(*move(0, 1))
        assert reference.bank_active_mask == reference.uregs[114].value & 0x4F8


def test_state_comparison_detects_latent_bank_divergence() -> None:
    initial = sd.export_state(_state())
    changed = dict(initial, bank_pending_mask=1 << 4)
    assert sd.compare_states(initial, changed) == ["bank_pending_mask: -1 != 16"]
    changed = dict(initial, bank_alt=dict(initial["bank_alt"]))
    changed["bank_alt"][7] = {"kind": 0, "value": 0, "mask": 0}
    assert len(sd.compare_states(initial, changed)) == 1
    assert sd.compare_states(initial, changed)[0].startswith("bank_alt[R7]:")


def test_diagnostic_call_and_render_clones_own_their_inactive_registers() -> None:
    import sharc_harness
    import sharc_run

    initial = _state()
    for clone in (
        sharc_run.fresh_call_state(initial, 0x40),
        sharc_harness._clone_state(initial),
    ):
        assert clone.bank_model
        clone.bank_alt[0] = Const(77)
        assert initial.bank_alt[0] == Const(1000)
