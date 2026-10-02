"""Public-format task restoration and physical PC/status-stack semantics."""

from __future__ import annotations

import os
import struct
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_diff as sd
import sharc_harness
import sharc_run as sr
import sharc_trace as st
import sharc_transpile_run as nr
import sharcldr
from sharc_core.state import _copy, _sync_pc_stack, _sync_status_stack
from sharc_disasm import Instruction, get_type

STACK = dict.fromkeys(
    (
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
    ),
    0,
)
RTS = {"cond[4:0]": 31, "x": 0, "j": 0, "lr": 0}


@pytest.mark.parametrize("native", [False, True])
def test_empty_loop_registers_read_all_ones_and_ignore_guest_writes(native):
    machine = Machine(native)
    for code in (102, 103):
        machine.step("17a", immediate(code, 123))
        assert machine.state.uregs[code] == st.Const(0xFFFFFFFF)
    machine.step(
        "5b_move",
        {
            "srcureghigh[4:0]": 25,
            "srcureglow[1:1]": 1,
            "srcureglow[0:0]": 0,
            "dstureg[6:0]": 0,
            "cond[4:0]": 31,
        },
    )
    assert machine.state.uregs[0] == st.Const(0xFFFFFFFF)


def immediate(code: int, value: int) -> dict[str, int]:
    return {"ureg[6:0]": code, "data[31:16]": value >> 16, "data[15:0]": value & 0xFFFF}


def encoded(form: str, fields: dict[str, int]) -> bytes:
    entry = get_type(form)
    assert entry is not None and entry["bits"] == 48
    raw = entry["opcode_value"]
    for name, value in fields.items():
        hi, lo = entry["fields"][name]
        assert 0 <= value < 1 << (hi - lo + 1)
        raw |= value << lo
    return struct.pack("<HHH", raw >> 32, (raw >> 16) & 0xFFFF, raw & 0xFFFF)


class Machine:
    def __init__(self, native: bool, slots: bytes = bytes(12)):
        image = bytes(6) + slots + bytes(512)
        memory = sharcldr.LoadedMemory(
            image,
            [
                {
                    "target_address": sharcldr.SW_ALIAS_BASE,
                    "byte_count": len(image),
                    "fill": False,
                    "payload_offset": 0,
                    "payload_len": len(image),
                }
            ],
        )
        self.state = sr.make_state(memory, 0)
        self.state.stack_model = True
        self.state.bank_model = True
        self.state.follow_loaded_calls = True
        self.state.max_call_depth = 64
        self.state.uregs[114] = st.Const(0)
        self.state.uregs[116] = st.Const(0)
        _sync_pc_stack(self.state)
        _sync_status_stack(self.state)
        self.core = None
        if native:
            library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
            if not Path(library).exists():
                pytest.skip("no native library built")
            self.core = nr.NativeCore(nr.pack_image(memory), library)
            nr.to_native(self.core, self.state)
            self.core.set_option(nr.OPT_RUNTIME_DECODE, True)

    def step(self, form: str, fields: dict[str, int], length: int = 6):
        [self.state] = st._execute(
            self.state, Instruction(0, length, form, fields, kind="confident")
        )
        assert self.state.stopped is None
        if self.core is not None:
            assert self.core.exec_insn(nr.pack_insn(form, length, "confident", fields))
            assert (
                sd.compare_states(sd.export_state(self.state), self.core.export_state())
                == []
            )
        return self.state

    def call(self, delayed: bool = False):
        return self.step(
            "8a_abs",
            {
                "addr[23:16]": 0,
                "addr[15:0]": 0x40,
                "cond[4:0]": 31,
                "b": 1,
                "j": int(delayed),
                "a": 0,
            },
        )


@pytest.mark.parametrize("native", [False, True])
def test_compiler_cjump_and_computed_return_leave_hardware_stack_alone(native):
    machine = Machine(native)
    machine.step("20a", dict(STACK, ppu=1))
    machine.step("17a", immediate(100, 0x01001234))
    depth, top = machine.state.uregs[101], machine.state.uregs[100]
    machine.step("25a_direct", {"addr[23:16]": 0, "addr[15:0]": 0x40})
    machine.step("21a", {})
    machine.step("21a", {})
    assert machine.state.pc_sw == 0x40
    assert machine.state.call_stack == []
    machine.step("17a", immediate(28, 0x8F))  # I12; software target 0x90
    machine.step("17a", immediate(46, 1))  # M14
    machine.step(
        "9b_abs",
        {
            "b": 0,
            "cond[4:0]": 31,
            "pmm": 6,
            "pmi[2:2]": 1,
            "pmi[1:0]": 0,
            "j": 1,
            "a": 0,
            "ci": 0,
        },
    )
    machine.step("21a", {})
    machine.step("21a", {})
    assert machine.state.pc_sw == 0x90
    assert machine.state.uregs[101] == depth and machine.state.uregs[100] == top


@pytest.mark.parametrize("native", [False, True])
def test_delayed_call_reserves_entry_before_explicit_push_in_first_slot(native):
    machine = Machine(native, encoded("20a", dict(STACK, ppu=1)) + bytes(6))
    machine.call(delayed=True)
    assert machine.state.pc_stack == [0x01000009]
    machine.step("20a", dict(STACK, ppu=1))
    assert len(machine.state.pc_stack) == 2
    machine.step("21a", {})
    machine.step("20a", dict(STACK, ppo=1))
    machine.step("11c", RTS, 2)
    assert machine.state.pc_sw == 9 and machine.state.pc_stack == []


@pytest.mark.parametrize("native", [False, True])
def test_guest_pcstk_write_in_call_delay_slot_changes_actual_rts_target(native):
    machine = Machine(native, encoded("17a", immediate(100, 0x01000090)) + bytes(6))
    machine.call(delayed=True)
    machine.step("17a", immediate(100, 0x01000090))
    machine.step("21a", {})
    machine.step("11c", RTS, 2)
    assert machine.state.pc_sw == 0x90
    assert machine.state.pc_stack == [] and machine.state.call_stack == []


@pytest.mark.parametrize("native", [False, True])
def test_delayed_call_counts_mixed_width_slots_and_rts_pops_before_its_slots(native):
    machine = Machine(native, b"\x01\x00" + bytes(6))  # public 16-bit NOP + 48-bit NOP
    machine.call(delayed=True)
    assert machine.state.pc_stack == [0x01000007]
    machine.step("21c", {}, 2)
    machine.step("21a", {})
    machine.step("11c", dict(RTS, j=1), 2)
    assert machine.state.pc_stack == [] and machine.state.uregs[101] == st.Const(0)
    machine.step("21a", {})
    machine.step("21a", {})
    assert machine.state.pc_sw == 7


@pytest.mark.parametrize("native", [False, True])
def test_do_loop_entries_and_termination_do_not_change_followed_call_depth(native):
    machine = Machine(native)
    machine.call()
    followed = list(machine.state.call_stack)
    machine.step(
        "12a_imm",
        {
            "data[15:8]": 0,
            "data[7:0]": 2,
            "mode": 0,
            "reladdr[22:16]": 0,
            "reladdr[15:0]": 3,
        },
    )
    assert machine.state.pc_stack == [0x01000003, 0x43]
    machine.step("21c", {}, 2)
    assert machine.state.pc_sw == 0x43
    machine.step("21c", {}, 2)
    assert machine.state.pc_stack == [0x01000003]
    assert machine.state.call_stack == followed
    machine.step("11c", RTS, 2)
    assert machine.state.pc_sw == 3


@pytest.mark.parametrize("native", [False, True])
def test_nested_mode1stk_reads_track_top_status_entry(native):
    machine = Machine(native)
    machine.step("20a", dict(STACK, spu=1))
    machine.step("17a", immediate(125, 0x1000))
    machine.step("20a", dict(STACK, spu=1))
    machine.step("17a", immediate(125, 0x1008))
    assert machine.state.uregs[125] == st.Const(0x1008)
    machine.step("20a", dict(STACK, spo=1))
    assert machine.state.uregs[125] == st.Const(0x1000)
    machine.step("20a", dict(STACK, spo=1))
    assert machine.state.uregs[114] == st.Const(0x1000)


def test_empty_stack_write_and_unknown_pointer_fail_closed():
    machine = Machine(False)
    machine.step("17a", immediate(100, 0x90))
    assert machine.state.uregs[100] == st.Const(0x7FFFFFFF)
    [state] = st._execute(machine.state, Instruction(0, 6, "17a", immediate(101, 1)))
    assert state.stopped == "guest PCSTKP growth is not modeled"


@pytest.mark.parametrize("native", [False, True])
def test_task_restore_preserves_task_entry_and_restores_mode1stk_value(native):
    machine = Machine(native)
    machine.step("20a", dict(STACK, ppu=1))
    machine.step("17a", immediate(100, 0x01000090))  # saved task's continuation
    machine.call()
    helper_return = machine.state.call_stack[-1]
    machine.step("20a", dict(STACK, spu=1))
    machine.step("17a", immediate(125, 0x1010))  # restored MODE1STK: IRQ + DAG1 bank
    assert machine.state.status_stack[-1][2] == st.Const(0x1010)
    assert machine.state.uregs[114] == st.Const(0)
    machine.step("11c", dict(RTS, j=1), 2)
    machine.step("20a", dict(STACK, spo=1))  # first RTS delay slot
    assert machine.state.uregs[114] == st.Const(0x1010)
    machine.step("21a", {})
    assert machine.state.pc_sw == helper_return
    assert machine.state.pc_stack == [0x01000090]
    assert machine.state.bank_active_mask == 0x10
    machine.step("11c", RTS, 2)
    assert machine.state.pc_sw == 0x90 and machine.state.pc_stack == []


@pytest.mark.parametrize("native", [False, True])
def test_pcstkp_truncates_after_following_instruction_preserving_oldest(native):
    machine = Machine(native)
    for entry in (0x01000020, 0x01000030, 0x01000040):
        machine.step("20a", dict(STACK, ppu=1))
        machine.step("17a", immediate(100, entry))
    machine.step("17a", immediate(101, 1))
    assert len(machine.state.pc_stack) == 3
    machine.step("21a", {})
    assert machine.state.pc_stack == [0x01000020]
    assert machine.state.uregs[101] == st.Const(1)


def test_canonical_v4_and_old_versions_do_not_invent_physical_stacks():
    machine = Machine(False)
    machine.step("20a", dict(STACK, ppu=1))
    machine.step("17a", immediate(100, 0x01000090))
    machine.step("17a", immediate(101, 0))
    fields = sd.export_state(machine.state)
    blob = sd.pack_state(fields)
    restored = sd.unpack_state(blob)
    assert restored["pc_stack"] == [0x01000090] and restored["stack_model"]
    assert restored["pc_stack_pending"] == 0
    for version, suffix in ((3, 109), (2, 109 + 15), (1, 109 + 15 + 877)):
        old = bytearray(blob[:-suffix])
        old[4:8] = struct.pack("<I", version)
        restored = sd.unpack_state(old)
        assert restored["loop_depth"] == 0
        if version < 3:
            assert restored["pc_stack"] == [] and not restored["stack_model"]
    changed = dict(fields, pc_stack=[0x01000091])
    assert sd.compare_states(fields, changed)[0].startswith("pc_stack:")


def test_forks_and_render_clones_own_physical_stack():
    machine = Machine(False)
    machine.step("20a", dict(STACK, ppu=1))
    for clone in (_copy(machine.state), sharc_harness._clone_state(machine.state)):
        clone.pc_stack[-1] = 0x90
    assert machine.state.pc_stack == [0xFFFFFFFF]


@pytest.mark.parametrize("native", [False, True])
def test_push_loop_reserves_and_preserves_popped_contents(native):
    machine = Machine(native)
    machine.step("20a", {**STACK, "lpu": 1})
    assert machine.state.loop_depth == 1
    assert machine.state.loops == []
    assert isinstance(machine.state.uregs[102], st.Unknown)
    machine.step("17a", immediate(103, 123))
    machine.step("17a", immediate(102, 0xFFFFFFFF))
    machine.step("20a", {**STACK, "lpo": 1})
    assert machine.state.loop_depth == 0
    assert machine.state.uregs[103] == st.Const(0xFFFFFFFF)
    machine.step("20a", {**STACK, "lpu": 1})
    assert machine.state.uregs[103] == st.Const(123)
    assert machine.state.uregs[102] == st.Const(0xFFFFFFFF)
    assert machine.state.loops == []
    fields = sd.unpack_state(sd.pack_state(sd.export_state(machine.state)))
    restored = sd.import_state(fields)
    assert restored.loop_depth == 1
    assert sd.export_state(restored)["loop_slots"] == fields["loop_slots"]
    copied = _copy(restored)
    copied.loop_slots[0] = (st.Const(0), st.Const(0))
    assert restored.loop_slots[0] == (st.Const(0xFFFFFFFF), st.Const(123))


def test_packed_loop_restore_stops_before_inventing_control_flow():
    machine = Machine(False)
    machine.step("20a", {**STACK, "lpu": 1})
    before = list(machine.state.loop_slots)
    [result] = st._execute(
        machine.state,
        Instruction(0, 6, "17a", immediate(102, 0x1234), kind="confident"),
    )
    assert result.stopped and "packed loop restoration" in result.stopped
    assert machine.state.loop_slots == before
    assert machine.state.loops == []


def test_import_keyed_memory_ranges_round_trips_loaded_bytes():
    state = Machine(False).state
    address = sharcldr.SW_ALIAS_BASE
    state.overlay.update({address: 10, address + 1: 20})
    fields = sd.unpack_state(
        sd.pack_state(sd.export_state(state, memory_ranges=[(address, 2)]))
    )
    assert sd.import_state(fields, data=state.concrete).overlay == state.overlay


def test_native_failed_delay_slot_rolls_back_loop_reservation():
    machine = Machine(True)
    assert machine.core is not None
    machine.state.pending = st.Pending(None, slots=1, return_from_call=True)
    nr.to_native(machine.core, machine.state)
    before = machine.core.export_state()
    assert not machine.core.exec_insn(
        nr.pack_insn("20a", 6, "confident", {**STACK, "lpu": 1})
    )
    after = machine.core.export_state()
    assert after["loop_depth"] == before["loop_depth"] == 0
    assert after["loop_slots"] == before["loop_slots"]
    assert after["pending"] == before["pending"]
