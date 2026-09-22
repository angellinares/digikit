import importlib.util
import struct
import sys
from pathlib import Path

import pytest

# tools/sharcemu.py does its own sys.path juggling (sibling imports of
# sharc_disasm/sharcinv, then a guard that strips tools/ before any pyghidra
# import) so tools/ must be importable when the module is loaded this way,
# same as tests/test_ghidraq.py loading tools/ghidraq.py.
_TOOLS = Path(__file__).parents[1] / "tools"
if str(_TOOLS) not in sys.path:
    sys.path.insert(0, str(_TOOLS))

spec = importlib.util.spec_from_file_location("sharcemu", _TOOLS / "sharcemu.py")
assert spec is not None and spec.loader is not None
sharcemu = importlib.util.module_from_spec(spec)
# sharcemu.py uses `from __future__ import annotations`, so its dataclasses
# resolve field types lazily by looking up their own module in sys.modules;
# it must be registered there before exec_module runs the class bodies.
sys.modules["sharcemu"] = sharcemu
spec.loader.exec_module(sharcemu)

from sharc_disasm import Instruction  # noqa: E402


def frame_bytes(frame48):
    """Real 48-bit SHARC+ VISA encoding -> the 6-byte little-endian-word
    buffer tools/sharc_disasm.py expects (words MSB-first, each word
    little-endian on the wire)."""
    w0 = (frame48 >> 32) & 0xFFFF
    w1 = (frame48 >> 16) & 0xFFFF
    w2 = frame48 & 0xFFFF
    return struct.pack("<HHH", w0, w1, w2)


# --- coordinate / CLI-value parsing -----------------------------------------


def test_parse_coordinate_sw_and_displayed():
    assert sharcemu.parse_coordinate("sw:0x1c18a6") == (0x38314C, 0x1C18A6)
    assert sharcemu.parse_coordinate("0x38314c") == (0x38314C, 0x1C18A6)
    assert sharcemu.parse_coordinate("0x3") == (0x3, None)  # odd: not a whole short word
    assert sharcemu.parse_coordinate("100") == (100, 50)


def test_parse_coordinate_rejects_bad_tokens():
    for bad in ("sw:nope", "-1", "sw:-1"):
        with pytest.raises(ValueError):
            sharcemu.parse_coordinate(bad)


def test_parse_watch_defaults_and_explicit_length():
    assert sharcemu.parse_watch("0x252658") == (0x252658, 4)
    assert sharcemu.parse_watch("0x252658:8") == (0x252658, 8)
    assert sharcemu.parse_watch("sw:0x1c1928") == (0x383250, 4)
    with pytest.raises(ValueError):
        sharcemu.parse_watch("0x252658:0")


def test_parse_set_and_parse_poke():
    assert sharcemu.parse_set("R2=0x1234") == ("R2", 0x1234)
    assert sharcemu.parse_set("R2=42") == ("R2", 42)
    assert sharcemu.parse_poke("0x252658=0xdeadbeef") == (0x252658, 0xDEADBEEF)
    assert sharcemu.parse_poke("sw:0x10=5") == (0x20, 5)
    with pytest.raises(ValueError):
        sharcemu.parse_set("R2")
    with pytest.raises(ValueError):
        sharcemu.parse_set("R2=nope")


# --- exemption classification: real encodings for the two exempt forms ------


def test_type21a_nop_is_exempt():
    exemption = sharcemu.classify_pcode_exemption(frame_bytes(0x000000000000), "nop")
    assert exemption.exempt is True
    assert exemption.decoder_type_name == "21a"


def test_type9a_abs_register_indirect_call_b1_is_exempt():
    data = frame_bytes(0x088000000000)  # b=1: register-indirect call
    exemption = sharcemu.classify_pcode_exemption(data, "callJ")
    assert exemption.exempt is True
    assert exemption.decoder_type_name == "9a_abs"
    assert "b=1" in exemption.note


def test_type9a_abs_absolute_call_b0_is_not_exempt():
    data = frame_bytes(0x080000000000)  # b=0: absolute-address call
    exemption = sharcemu.classify_pcode_exemption(data, "callJ")
    assert exemption.exempt is False
    assert exemption.decoder_type_name == "9a_abs"
    assert "b=0" in exemption.note


def test_type9b_abs_follows_the_same_b_rule():
    exempt = sharcemu.classify_pcode_exemption(frame_bytes(0x0800003F0000 | (1 << 39)), "callJ")
    not_exempt = sharcemu.classify_pcode_exemption(frame_bytes(0x0800003F0000), "callJ")
    assert (exempt.exempt, exempt.decoder_type_name) == (True, "9b_abs")
    assert (not_exempt.exempt, not_exempt.decoder_type_name) == (False, "9b_abs")


# --- exemption classification: edge cases via a monkeypatched decoder -------


def test_any_other_empty_form_is_not_exempt(monkeypatch):
    fake = Instruction(offset=0, length_bytes=6, type_name="2a_short",
                        fields={}, raw=0, kind="confident", note="")
    monkeypatch.setattr(sharcemu, "disassemble", lambda data, count=1: iter([fake]))
    exemption = sharcemu.classify_pcode_exemption(b"\x00" * 6, "some_mnemonic")
    assert exemption.exempt is False
    assert exemption.decoder_type_name == "2a_short"
    assert "not a known-exempt form" in exemption.note


def test_unknown_decode_is_not_exempt(monkeypatch):
    fake = Instruction(offset=0, length_bytes=None, type_name="unknown",
                        fields={}, raw=0, kind="unknown", note="no form matches")
    monkeypatch.setattr(sharcemu, "disassemble", lambda data, count=1: iter([fake]))
    exemption = sharcemu.classify_pcode_exemption(b"\x00" * 6, "mystery")
    assert exemption.exempt is False
    assert exemption.decoder_type_name == "unknown"


def test_uncertain_match_of_an_exempt_form_is_not_exempt(monkeypatch):
    # An uncertain "21a" match must NOT be silently trusted as the confirmed
    # NOP -- only a confident match exempts an instruction.
    fake = Instruction(offset=0, length_bytes=6, type_name="21a", fields={},
                        raw=0, kind="uncertain", note="source: some weaker table")
    monkeypatch.setattr(sharcemu, "disassemble", lambda data, count=1: iter([fake]))
    exemption = sharcemu.classify_pcode_exemption(b"\x00" * 6, "nop")
    assert exemption.exempt is False
    assert "not confident enough" in exemption.note


def test_decoder_produces_no_instruction(monkeypatch):
    monkeypatch.setattr(sharcemu, "disassemble", lambda data, count=1: iter([]))
    exemption = sharcemu.classify_pcode_exemption(b"", "whatever")
    assert exemption.exempt is False
    assert exemption.decoder_type_name is None


# --- run_emulation: fault/skip loop logic, driven by a fake backend --------


class FakeBackend:
    """Duck-types sharcemu.Backend. `instructions` maps sw -> InstrInfo;
    `step_results` maps sw -> StepOutcome for the step executed *from* that
    sw. A successful step advances PC by the instruction's length (in short
    words) unless `jumps` overrides the next sw explicitly, mirroring how a
    real CPU's PC auto-advances (or branches) inside step()."""

    def __init__(self, instructions, step_results=None, jumps=None, default_instruction=None):
        self.instructions = instructions
        self.step_results = step_results or {}
        self.jumps = jumps or {}
        self.default_instruction = default_instruction
        self.pc_sw = None
        self.registers = {}
        self.memory_writes = []
        self.register_writes = []

    def set_pc_sw(self, sw):
        self.pc_sw = sw

    def get_pc_sw(self):
        return self.pc_sw

    def instruction_at(self, displayed):
        return self.instructions.get(displayed // 2, self.default_instruction)

    def step_and_watch(self):
        sw = self.pc_sw
        outcome = self.step_results.get(sw, sharcemu.StepOutcome(True, None, []))
        if outcome.ok:
            info = self.instructions.get(sw, self.default_instruction)
            self.pc_sw = self.jumps.get(sw, sw + max(info.length_bytes, 2) // 2)
        return outcome

    def read_register(self, name):
        return self.registers.get(name, 0)

    def write_register(self, name, value):
        self.registers[name] = value
        self.register_writes.append((name, value))

    def write_memory(self, displayed, data):
        self.memory_writes.append((displayed, data))


def make_info(mnemonic="Type14a", length_bytes=6, pcode_op_count=1, raw_bytes=b"\x00" * 6):
    return sharcemu.InstrInfo(mnemonic, length_bytes, pcode_op_count, raw_bytes)


def test_run_stops_at_max_steps_when_everything_executes_cleanly():
    instrs = {0x100: make_info(), 0x103: make_info(), 0x106: make_info()}
    backend = FakeBackend(instrs)
    outcome = sharcemu.run_emulation(backend, start_sw=0x100, max_steps=3)
    assert outcome.stop_reason == "max-steps"
    assert outcome.steps_executed == 3
    assert outcome.faults == []


def test_run_stops_at_pc_leaving_mapped_code():
    instrs = {0x100: make_info(length_bytes=6)}
    backend = FakeBackend(instrs)
    outcome = sharcemu.run_emulation(backend, start_sw=0x100, max_steps=50)
    assert outcome.stop_reason == "pc-left-program"
    assert outcome.steps_executed == 1
    assert outcome.faults == []


def test_no_semantics_fault_stops_by_default():
    exempt_nop = frame_bytes(0x000000000000)
    instrs = {0x100: make_info(mnemonic="nop", length_bytes=6, pcode_op_count=0, raw_bytes=exempt_nop),
              0x103: make_info(mnemonic="weird", length_bytes=6, pcode_op_count=0, raw_bytes=b"\xff" * 6)}
    backend = FakeBackend(instrs)
    outcome = sharcemu.run_emulation(backend, start_sw=0x100, max_steps=50)
    # step 1 (the exempt NOP) executes fine; step 2 has zero p-code and does
    # not decode as an exempt form -> stop right there, default skip_faults=0.
    assert outcome.steps_executed == 1
    assert outcome.stop_reason == "no-semantics"
    assert len(outcome.faults) == 1
    fault = outcome.faults[0]
    assert fault.kind == "no-semantics"
    assert fault.pc_sw == 0x103
    assert fault.ghidra_mnemonic == "weird"


def test_skip_faults_advances_past_and_continues_then_stops_on_the_next_one():
    bad = make_info(mnemonic="bad", length_bytes=2, pcode_op_count=0, raw_bytes=b"\xff\xff")
    good = make_info(mnemonic="good", length_bytes=2, pcode_op_count=1)
    instrs = {0x10: bad, 0x11: bad, 0x12: good, 0x13: bad}
    backend = FakeBackend(instrs)
    outcome = sharcemu.run_emulation(backend, start_sw=0x10, max_steps=50, skip_faults=2)
    # Two faults are skipped (0x10, 0x11), 0x12 executes, the third fault
    # (0x13) is the one that stops the run.
    assert [f.pc_sw for f in outcome.faults] == [0x10, 0x11, 0x13]
    assert outcome.stop_reason == "no-semantics"
    assert outcome.steps_executed == 1


def test_emulator_error_fault_stops_by_default_and_carries_the_message():
    instrs = {0x10: make_info(mnemonic="jump")}
    step_results = {0x10: sharcemu.StepOutcome(False, "Unimplemented CALLOTHER pcodeop (condition)")}
    backend = FakeBackend(instrs, step_results=step_results)
    outcome = sharcemu.run_emulation(backend, start_sw=0x10, max_steps=50)
    assert outcome.stop_reason == "emulator-error"
    assert outcome.steps_executed == 0
    assert len(outcome.faults) == 1
    assert outcome.faults[0].kind == "emulator-error"
    assert "CALLOTHER" in outcome.faults[0].message


def test_emulator_error_is_skippable_too():
    instrs = {0x10: make_info(length_bytes=2)}
    step_results = {0x10: sharcemu.StepOutcome(False, "boom")}
    # Everywhere but 0x10 is an ordinary, always-mapped instruction so the
    # run can reach max_steps after skipping the one fault at 0x10.
    backend = FakeBackend(instrs, step_results=step_results, default_instruction=make_info(length_bytes=2))
    outcome = sharcemu.run_emulation(backend, start_sw=0x10, max_steps=5, skip_faults=1)
    assert outcome.stop_reason == "max-steps"
    assert outcome.steps_executed == 5
    assert len(outcome.faults) == 1


def test_watched_writes_are_recorded_with_step_and_pc():
    hit = sharcemu.WatchHit(0x254D9C, b"\x00\x00\x00\x00", b"\x12\x34\x00\x00")
    instrs = {0x10: make_info(length_bytes=2), 0x11: make_info(length_bytes=2)}
    step_results = {0x10: sharcemu.StepOutcome(True, None, [hit])}
    backend = FakeBackend(instrs, step_results=step_results)
    outcome = sharcemu.run_emulation(backend, start_sw=0x10, max_steps=2)
    assert len(outcome.writes) == 1
    write = outcome.writes[0]
    assert (write.step, write.pc_sw, write.address) == (1, 0x10, 0x254D9C)
    assert write.before_hex == "00000000"
    assert write.after_hex == "12340000"


def test_pokes_and_sets_are_applied_before_the_first_instruction():
    instrs = {0x10: make_info(length_bytes=2)}
    backend = FakeBackend(instrs)
    sharcemu.run_emulation(backend, start_sw=0x10, max_steps=1,
                            pokes=[(0x252658, 0xDEADBEEF)], sets=[("R2", 0x1234)])
    assert backend.memory_writes == [(0x252658, struct.pack("<I", 0xDEADBEEF))]
    assert backend.register_writes == [("R2", 0x1234)]
    assert backend.pc_sw is not None  # set_pc_sw ran after pokes/sets


def test_zero_pcode_exempt_instruction_still_executes_via_step():
    # An exempt (correctly empty) instruction must still be *stepped*, not
    # merely skipped -- it is a real, semantics-free instruction (a NOP or a
    # register-indirect call), not a decoding failure.
    exempt_nop = frame_bytes(0x000000000000)
    instrs = {0x10: make_info(mnemonic="nop", length_bytes=6, pcode_op_count=0, raw_bytes=exempt_nop)}
    backend = FakeBackend(instrs)
    outcome = sharcemu.run_emulation(backend, start_sw=0x10, max_steps=1)
    assert outcome.faults == []
    assert outcome.steps_executed == 1
