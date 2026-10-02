"""Public synthetic L1 ISA vectors and opt-in software interrupt semantics."""

from __future__ import annotations

import os
import struct
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_diff as sd
import sharc_run as sr
import sharc_transpile_run as nr
import sharcldr
from sharc_core.sequencer import _software_interrupt_candidate
from sharc_core.state import (
    _bank_complete,
    _sync_pc_stack,
    _sync_status_stack,
    _write_ureg,
)
from sharc_core.values import Const
from sharc_disasm import decode_isa48, get_type


def word(form: str, **fields: int) -> int:
    entry = get_type(form)
    assert entry is not None and entry["bits"] == 48
    raw = entry["opcode_value"]
    for name, value in fields.items():
        hi, lo = entry["fields"][name]
        assert 0 <= value < 1 << (hi - lo + 1)
        raw |= value << lo
    return raw


def memory(handler: list[int] | None = None):
    nop = word("21a")
    ivt = bytearray(nop.to_bytes(6, "little") * 128)
    jump = word(
        "8a_abs",
        **{
            "addr[23:16]": 0,
            "addr[15:0]": 0x80,
            "cond[4:0]": 31,
            "b": 0,
            "j": 1,
            "a": 0,
        },
    )
    for level in (11, 22, *range(28, 32)):
        offset = level * 24
        ivt[offset : offset + 6] = jump.to_bytes(6, "little")
    rti = word(
        "11a",
        **{
            "x": 1,
            "cond[4:0]": 31,
            "j": 0,
            "lr": 0,
            "e": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        },
    )
    program = bytearray(512)
    words = [(0x40, nop)] + [
        (0x80 + 3 * index, raw) for index, raw in enumerate(handler or [rti])
    ]
    for pc, raw in words:
        program[pc * 2 : pc * 2 + 6] = struct.pack(
            "<HHH", raw >> 32, (raw >> 16) & 0xFFFF, raw & 0xFFFF
        )
    return sharcldr.LoadedMemory(
        bytes(ivt + program),
        [
            dict(
                target_address=0x28240000,
                byte_count=len(ivt),
                fill=False,
                payload_offset=0,
                payload_len=len(ivt),
            ),
            dict(
                target_address=sharcldr.SW_ALIAS_BASE,
                byte_count=len(program),
                fill=False,
                payload_offset=len(ivt),
                payload_len=len(program),
            ),
        ],
    )


def runner(enabled=True, handler=None):
    ref = sr.Runner(memory(handler), 0x40)
    state = ref.state
    state.software_interrupts = enabled
    state.stack_model = state.bank_model = True
    for code, value in (
        (114, 0x1000),
        (115, 0x1000),
        (122, 0x80000000),
        (123, 0x80000000),
        (124, 0),
        (112, 0x123),
        (113, 0x456),
    ):
        state.uregs[code] = Const(value)
    state.mmrs[0x30024] = Const(4)
    _sync_pc_stack(state)
    _sync_status_stack(state)
    return ref


def native_core(ref):
    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("no native library built")
    core = nr.NativeCore(nr.pack_image(ref.state.concrete), library)
    nr.to_native(core, ref.state)
    core.set_option(nr.OPT_RUNTIME_DECODE, 1)
    return core


@pytest.mark.parametrize("native", [False, True])
def test_isa_vector_delay_slots_and_rti(native):
    ref = runner()
    core = native_core(ref) if native else None
    for expected in (0x9007D, 0x9007E, 0x80, 0x40):
        ref.step()
        assert ref.state.pc_sw == expected
        if core:
            assert core.run(1) == 1, core.halt_reason
            assert (
                sd.compare_states(sd.export_state(ref.state), core.export_state()) == []
            )
        if expected != 0x40:
            assert ref.state.pc_stack == [0x01000040]
            assert len(ref.state.status_stack) == 1
            assert ref.state.uregs[114] == Const(0)
            assert ref.state.uregs[122] == Const(0)
            assert ref.state.uregs[124] == Const(0x80000000)
    assert ref.state.pc_stack == ref.state.status_stack == []
    assert ref.state.uregs[114] == Const(0x1000)
    assert ref.state.uregs[112] == Const(0x123)
    assert ref.state.uregs[113] == Const(0x456)
    assert ref.state.uregs[124] == Const(0)


@pytest.mark.parametrize("native", [False, True])
def test_default_disabled_leaves_eligible_irq_pending(native):
    ref = runner(False)
    core = native_core(ref) if native else None
    ref.step()
    assert ref.state.pc_sw == 0x43
    assert ref.state.uregs[122] == Const(0x80000000)
    assert ref.state.pc_stack == []
    if core:
        assert core.run(1) == 1
        assert sd.compare_states(sd.export_state(ref.state), core.export_state()) == []


@pytest.mark.parametrize("control", ["bank_model", "stack_model", "sysctl"])
@pytest.mark.parametrize("native", [False, True])
def test_invalid_entry_prerequisite_does_not_mutate_state(control, native):
    ref = runner()
    if control == "sysctl":
        ref.state.mmrs[0x30024] = Const(8)
    else:
        setattr(ref.state, control, False)
    before = sd.export_state(ref.state)
    if native:
        core = native_core(ref)
        assert core.run(1) == 0
        assert core.halted
        assert sd.compare_states(before, core.export_state()) == []
    else:
        with pytest.raises(sr.Halt, match="interrupt"):
            ref.step()
        assert sd.compare_states(before, sd.export_state(ref.state)) == []


def test_priority_nesting_and_active_latch_write():
    state = runner().state
    state.uregs[122] = state.uregs[123] = Const(0xF0000000)
    assert _software_interrupt_candidate(state) == 0x10000000
    state.uregs[124] = Const(0x40000000)
    assert _software_interrupt_candidate(state) == 0
    state.uregs[114] = Const(0x1800)
    assert _software_interrupt_candidate(state) == 0x10000000
    _write_ureg(state, 122, Const(0xF0000000))
    assert state.uregs[122] == Const(0xB0000000)


def test_isa_encoding_is_little_endian_whole_word():
    raw = word(
        "8a_abs",
        **{
            "addr[23:16]": 0x12,
            "addr[15:0]": 0x3456,
            "cond[4:0]": 31,
            "b": 0,
            "j": 1,
            "a": 0,
        },
    )
    insn = decode_isa48(raw.to_bytes(6, "little"))
    assert insn.type_name == "8a_abs"
    assert insn.fields["addr[23:16]"] == 0x12
    assert insn.fields["addr[15:0]"] == 0x3456
    assert insn.fields["j"] == 1


@pytest.mark.parametrize("native", [False, True])
def test_decoded_latch_write_cannot_relatch_active_interrupt(native):
    write = word("17a", **{"ureg[6:0]": 122, "data[31:16]": 0xC000, "data[15:0]": 0})
    rti = word(
        "11a",
        **{
            "x": 1,
            "cond[4:0]": 31,
            "j": 0,
            "lr": 0,
            "e": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        },
    )
    ref = runner(handler=[write, rti])
    core = native_core(ref) if native else None
    for _ in range(5):
        ref.step()
        if core:
            assert core.run(1) == 1, core.halt_reason
            assert (
                sd.compare_states(sd.export_state(ref.state), core.export_state()) == []
            )
    assert ref.state.pc_sw == 0x40
    assert ref.state.uregs[122] == Const(0x40000000)
    assert ref.state.uregs[124] == Const(0)


@pytest.mark.parametrize("native", [False, True])
def test_entry_settles_banks_and_rti_restores_with_following_instruction(native):
    ref = runner()
    ref.state.uregs[0] = Const(11)
    ref.state.bank_alt[0] = Const(22)
    _write_ureg(ref.state, 114, Const(0x1400))
    _bank_complete(ref.state)
    _bank_complete(ref.state)
    ref.state.uregs[115] = Const(0x1400)
    assert ref.state.uregs[0] == Const(22)
    core = native_core(ref) if native else None
    for index in range(5):
        ref.step()
        if core:
            assert core.run(1) == 1, core.halt_reason
            assert (
                sd.compare_states(sd.export_state(ref.state), core.export_state()) == []
            )
        assert ref.state.uregs[0] == Const(22 if index == 4 else 11)
    assert ref.state.bank_active_mask == 0x400


def test_vector_fetch_failure_preserves_successful_entry():
    ref = runner()
    # Public format: an unpopulated IVT has no readable backing bytes.
    ref.state.concrete = sharcldr.LoadedMemory(
        bytes(512),
        [
            dict(
                target_address=sharcldr.SW_ALIAS_BASE,
                byte_count=512,
                fill=False,
                payload_offset=0,
                payload_len=512,
            )
        ],
    )
    core = native_core(ref)
    assert core.run(1) == 0
    assert core.halted
    state = core.export_state()
    assert state["pc_sw"] == 0x9007C
    assert state["pc_stack"] == [0x01000040]
    assert len(state["status_stack"]) == 1
    assert state["uregs"][122]["value"] == 0
    assert state["uregs"][124]["value"] == 0x80000000
