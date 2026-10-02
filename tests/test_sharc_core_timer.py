"""Public core timer clocks, write priority and timer interrupt vectors."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

from test_sharc_software_interrupts import native_core, runner, word

import sharc_diff as sd
from sharc_core.sequencer import _core_timer_tick
from sharc_core.state import _write_ureg
from sharc_core.values import Const


def timer(enabled=True, handler=None):
    ref = runner(False, handler)
    ref.state.core_timer = enabled
    for code, value in ((116, 0x20), (110, 3), (111, 2), (122, 0), (123, 0)):
        ref.state.uregs[code] = Const(value)
    return ref


def step(ref, core):
    ref.step()
    if core:
        assert core.run(1) == 1, core.halt_reason
        assert sd.compare_states(sd.export_state(ref.state), core.export_state()) == []


@pytest.mark.parametrize("native", [False, True])
def test_countdown_latches_both_priorities_then_reloads(native):
    ref = timer()
    core = native_core(ref) if native else None
    for count in (1, 0, 3, 2, 1, 0):
        step(ref, core)
        assert ref.state.uregs[111] == Const(count)
    assert ref.state.uregs[122] == Const(0x00400800)


@pytest.mark.parametrize("native", [False, True])
def test_default_disabled_timer_is_static(native):
    ref = timer(False)
    core = native_core(ref) if native else None
    step(ref, core)
    assert ref.state.uregs[111] == Const(2)
    assert ref.state.uregs[122] == Const(0)


@pytest.mark.parametrize("native", [False, True])
def test_timer_low_priority_vector_returns_to_interrupted_program(native):
    ref = timer()
    ref.state.uregs[110] = Const(100)
    ref.state.uregs[123] = Const(0x00400000)
    core = native_core(ref) if native else None
    for expected in (0x43, 0x46, 0x90059, 0x9005A, 0x80, 0x46):
        step(ref, core)
        assert ref.state.pc_sw == expected
    assert ref.state.uregs[124] == Const(0)
    assert ref.state.uregs[122] == Const(0x800)  # Masked high source stays pending.


@pytest.mark.parametrize("native", [False, True])
def test_both_priorities_service_high_then_low(native):
    ref = timer()
    ref.state.uregs[110] = Const(100)
    ref.state.uregs[123] = Const(0x00400800)
    core = native_core(ref) if native else None
    for expected in (
        0x43,
        0x46,
        0x9002D,
        0x9002E,
        0x80,
        0x46,
        0x90059,
        0x9005A,
        0x80,
        0x46,
    ):
        step(ref, core)
        assert ref.state.pc_sw == expected
    assert ref.state.uregs[122] == ref.state.uregs[124] == Const(0)


def test_zero_period_expires_each_functional_clock():
    state = timer().state
    state.uregs[110] = state.uregs[111] = Const(0)
    for _ in range(3):
        state.uregs[122] = Const(0)
        assert _core_timer_tick(state)
        assert state.uregs[111] == Const(0)
        assert state.uregs[122] == Const(0x00400800)


def test_guest_timer_register_write_takes_priority_over_clock():
    state = timer().state
    _write_ureg(state, 110, Const(7))
    assert not _core_timer_tick(state)
    assert state.uregs[111] == Const(2)
    _write_ureg(state, 111, Const(1))
    assert not _core_timer_tick(state)
    assert state.uregs[111] == Const(1)
    assert _core_timer_tick(state)


@pytest.mark.parametrize("count", [0, 1])
@pytest.mark.parametrize("native", [False, True])
def test_decoded_period_write_wins_over_expiring_clock(native, count):
    write = word("17a", **{"ureg[6:0]": 110, "data[31:16]": 0, "data[15:0]": 7})
    ref = timer(handler=[write])
    ref.state.pc_sw = 0x80
    ref.state.uregs[111] = Const(count)
    core = native_core(ref) if native else None
    step(ref, core)
    assert ref.state.uregs[110] == Const(7)
    assert ref.state.uregs[111] == Const(count)
    assert ref.state.uregs[122] == Const(0)


def test_failed_timer_event_retains_completed_instruction_without_partial_tick():
    # Read an absent RAM word into IRPTL, making only the subsequent event unknown.
    load = word(
        "14a",
        **{
            "ureg[6:0]": 122,
            "g": 0,
            "d": 0,
            "l": 0,
            "addr[31:16]": 0x2D,
            "addr[15:0]": 0x1000,
        },
    )
    ref = timer(handler=[load])
    ref.state.pc_sw = 0x80
    ref.state.uregs[111] = Const(1)
    core = native_core(ref)
    import sharc_run as sr

    with pytest.raises(sr.Halt, match="timer"):
        ref.step()
    assert core.run(1) == 1
    assert core.halted
    assert ref.instructions == 1
    assert ref.state.pc_sw == 0x83
    assert ref.state.uregs[111] == Const(1)
    assert sd.compare_states(sd.export_state(ref.state), core.export_state()) == []
