"""Synthetic product-map regressions; no firmware or running state."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import sharc_run as sr
import sharc_trace as trace
import sharcldr
from sharc_core import addressing as a
from sharc_core import memory as m
from sharc_core.encoding import UREG_CODES
from sharc_core.state import Unknown
from sharc_core.values import Const, _aconv
from sharc_disasm import Instruction


@pytest.mark.parametrize(
    "word,byte",
    [
        (0x90000, 0x28240000),
        (0x9BFFF, 0x2826FFFC),
        (0xB0000, 0x282C0000),
        (0xC0000, 0x28300000),
        (0xE0000, 0x28380000),
        (0xE7FFF, 0x2839FFFC),
        (0x04000000, 0x60000000),
        (0x07FFFFFF, 0x6FFFFFFC),
        (0x08000000, 0x20000000),
        (0x08045FFF, 0x20117FFC),
        (0x0A090000, 0x28240000),
        (0x0A0E7FFF, 0x2839FFFC),
        (0x10000000, 0x80000000),
        (0x17FFFFFF, 0x9FFFFFFC),
    ],
)
def test_product_map_endpoints(word, byte):
    assert a.normal_word_to_byte(word) == byte
    arch_byte = a.normal_word_to_architectural_byte(word)
    assert a.byte_to_normal_word(arch_byte) == word
    assert _aconv(Const(word), True, 0, 0) == Const(arch_byte)
    assert _aconv(Const(arch_byte), False, 0, 0) == Const(word)


def test_access_context_and_alias_coherence():
    state = sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)
    # Distinct words expose a wrong stride even when loader DDR is zero-filled.
    assert m._dm_write(state, 0x80000000, 4, Const(0x44332211))
    assert m._dm_write(state, 0x80000004, 4, Const(0x88776655))
    assert m._dm_write(state, 0x10000000, 4, Const(0xDEADBEEF))
    assert m._dm_read(state, 0x10000000, 4, normal_word=True) == Const(0x44332211)
    assert m._dm_read(state, 0x10000001, 4, normal_word=True) == Const(0x88776655)
    assert m._dm_read(state, 0x10000000, 4) == Const(0xDEADBEEF)
    assert m._dm_read(state, 0x10000000, 2) == Const(0xBEEF)
    assert m._dm_read(state, 0x10000000, 1) == Const(0xEF)
    assert m._dm_write(state, 0x10000001, 4, Const(0x12345678), normal_word=True)
    assert m._dm_read(state, 0x80000004, 4) == Const(0x12345678)
    assert m._access_modifier_scale("normal-word", True, Const(0x10000000)) == 1
    assert m._access_modifier_scale("long-word", True, Const(0x10000000)) == 1
    assert m._access_modifier_scale("normal-word", True, Const(0x80000000)) == 4
    assert m._modify_scale(None, True, Const(0x10000000)) == 1
    assert m._modify_scale("normal-word", True, Const(0x10000000)) == 1


def test_translation_does_not_supply_missing_memory_or_wrap_addresses():
    state = sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)
    assert m._dm_read(state, 0x10000000, 4, normal_word=True) is None
    for address in (
        -1,
        0x9C000,
        0xBC000,
        0xC8000,
        0xE8000,
        0x08046000,
        0x18000000,
        0x100000000,
    ):
        assert a.normal_word_to_byte(address) is None


def test_watchpoint_selects_word_units_explicitly():
    state = sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)
    word = sr.Watchpoint(0x10000000, 0x10000002, normal_word=True)
    resolved = sr._canonicalize_watchpoint(state, word)
    assert [(w.start, w.end) for w in resolved] == [(0x80000000, 0x80000008)]
    byte = sr.Watchpoint(0x10000000, 0x10000002)
    assert all(w.start != 0x80000000 for w in sr._canonicalize_watchpoint(state, byte))
    with pytest.raises(ValueError):
        sr._canonicalize_watchpoint(
            state, sr.Watchpoint(0x17FFFFFF, 0x18000001, normal_word=True)
        )


@pytest.mark.parametrize("overflow,taken", [(0, False), (1 << 11, True)])
def test_scalar_conditional_type4a_gates_transfer_and_post_modify(overflow, taken):
    state = sr.make_state(
        sharcldr.LoadedMemory.from_stream(b""),
        0,
        regs={"I3": 0x10000000, "R9": 99, "ASTATX": overflow},
    )
    m._dm_write(state, 0x80000000, 4, Const(123))
    fields = {
        "i[2:0]": 3,
        "g": 0,
        "d": 0,
        "u": 1,
        "cond[4:0]": 7,
        "data[5:5]": 0,
        "data[4:0]": 3,
        "dreg[3:0]": 9,
        "compute[22:16]": 0,
        "compute[15:0]": 0,
    }
    [result] = trace._execute(state, Instruction(0, 6, "4a", fields, kind="confident"))
    assert result.stopped is None
    assert result.pc_sw == 3
    assert result.uregs[UREG_CODES["R9"]] == Const(123 if taken else 99)
    assert result.uregs[UREG_CODES["I3"]] == Const(0x10000003 if taken else 0x10000000)


def test_false_type4a_does_not_evaluate_unsupported_compute():
    state = sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)
    # The absent transfer/compute fields must never be consulted on a NOP.
    result = trace._execute(
        state, Instruction(0, 6, "4a", {"cond": 7}, kind="confident")
    )
    assert len(result) == 1 and result[0].stopped is None
    assert result[0].pc_sw == 3


def test_unknown_type4a_predicate_preserves_both_paths():
    state = sr.make_state(
        sharcldr.LoadedMemory.from_stream(b""), 0, regs={"I3": 0x10000000, "R9": 99}
    )
    state.uregs[UREG_CODES["ASTATX"]] = Unknown("unknown overflow")
    m._dm_write(state, 0x80000000, 4, Const(123))
    fields = {
        "i": 3,
        "g": 0,
        "d": 0,
        "u": 1,
        "cond": 7,
        "data[5:5]": 0,
        "data[4:0]": 3,
        "dreg": 9,
        "compute[22:16]": 0,
        "compute[15:0]": 0,
    }
    result = trace._execute(state, Instruction(0, 6, "4a", fields, kind="confident"))
    assert len(result) == 2
    assert {r.uregs[UREG_CODES["R9"]].value for r in result} == {99, 123}
    assert state.uregs[UREG_CODES["R9"]] == Const(99)


def test_cache_flush_preserves_coherent_data_and_rejects_stack_combination():
    state = sr.make_state(sharcldr.LoadedMemory.from_stream(b""), 0)
    m._dm_write(state, 0x80000000, 4, Const(123))
    fields = {
        "lpu": 0,
        "lpo": 0,
        "spu": 0,
        "spo": 0,
        "ppu": 0,
        "ppo": 0,
        "fc": 0,
        "llii": 0,
        "lldwb": 1,
        "lldi": 1,
        "llpwb": 1,
        "llpi": 1,
    }
    registers = state.uregs.copy()
    [result] = trace._execute(state, Instruction(0, 6, "20a", fields, kind="confident"))
    assert result.stopped is None and result.pc_sw == 3
    assert result.uregs == registers
    assert m._dm_read(result, 0x10000000, 4, normal_word=True) == Const(123)
    [invalid] = trace._execute(
        state, Instruction(0, 6, "20a", dict(fields, spu=1), kind="confident")
    )
    assert invalid.stopped == "invalid Type20a combined L1 cache operation"
