"""Opt-in SEC core interface and descriptor-list DMA (State.peripheral_model)."""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_diff as sd
import sharc_transpile_run as nr
import sharcldr
from sharc_core.encoding import UREG_CODES
from sharc_core.memory import _dm_read, _dm_write
from sharc_core.periph import (
    SEC_CCTL,
    SEC_CSID,
    SEC_CSTAT,
    SEC_END,
    SEC_GCTL,
    SEC_SCTL,
    SECI_ID,
    _dma_done,
    _dma_start,
    _sec_line,
    _sec_raise,
)
from sharc_core.state import State
from sharc_core.values import Const
from sharc_periph_host import SPI2_RX_DMA, SPI2_TX_DMA, spi2_exchange, swap16

RAM = 0x28100000
IRPTL = UREG_CODES["IRPTL"]
IMASKP = UREG_CODES["IMASKP"]


def _words(*values: int) -> bytes:
    return b"".join(value.to_bytes(4, "little") for value in values)


def _state(ram: bytes = bytes(0x400)) -> State:
    memory = sharcldr.LoadedMemory(
        ram,
        [
            dict(
                target_address=RAM,
                byte_count=len(ram),
                fill=False,
                payload_offset=0,
                payload_len=len(ram),
            )
        ],
    )
    state = State(pc_sw=0, concrete=memory, assume_nw32=True, peripheral_model=True)
    state.uregs[IRPTL] = Const(0)
    state.uregs[IMASKP] = Const(0)
    state.mmrs[SEC_GCTL] = Const(1)
    state.mmrs[SEC_CCTL] = Const(1)
    for sid in (69, 70):
        state.mmrs[SEC_SCTL + 8 * sid] = Const(5)
    return state


def _sstat(state: State, sid: int) -> int:
    return state.mmrs[SEC_SCTL + 8 * sid + 4].value


def _ring(cfg: int, count: int) -> bytes:
    """Two descriptors at RAM and RAM+0x20 pointing at each other."""
    ram = bytearray(0x400)
    ram[0:0x14] = _words(RAM + 0x20, RAM + 0x100, cfg, count, 2)
    ram[0x20:0x34] = _words(RAM, RAM + 0x200, cfg, count, 2)
    return bytes(ram)


def test_sec_issues_one_source_until_ack_and_end() -> None:
    state = _state()
    _sec_raise(state, 70)
    _sec_raise(state, 69)
    # 70 was issued first; 69 stays pending behind the unacknowledged ID.
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0x8000)
    assert _dm_read(state, SECI_ID, 4) == Const(70)
    assert _sstat(state, 69) & 0x300 == 0x100
    # Vectoring clears the latch; inside the SECI ISR the line cannot relatch.
    state.uregs[IRPTL] = Const(0)
    state.uregs[IMASKP] = Const(0x8000)
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0)
    assert _dm_write(state, SECI_ID, 4, Const(70))
    assert _sstat(state, 70) & 0x300 == 0x200
    # Equal priority: 69 waits for the END of 70, then is issued inside the
    # ISR and latches only after its RTI.
    assert _dm_write(state, SEC_END, 4, Const(70))
    assert _sstat(state, 70) & 0x300 == 0
    assert state.mmrs[SEC_CSID] == Const(69)
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0)
    state.uregs[IMASKP] = Const(0)
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0x8000)


def test_sec_errors_are_recorded() -> None:
    state = _state()
    assert _dm_write(state, SEC_END, 4, Const(70))
    assert _sstat(state, 70) & 0x32 == 0x22
    assert _dm_write(state, SECI_ID, 4, Const(0))
    assert state.mmrs[SEC_CSTAT].value & 0x12 == 0x12
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0)


def test_disabled_sec_keeps_the_source_pending() -> None:
    state = _state()
    state.mmrs[SEC_GCTL] = Const(0)
    _sec_raise(state, 70)
    assert _sstat(state, 70) & 0x300 == 0x100
    _sec_line(state)
    assert state.uregs[IRPTL] == Const(0)


def test_descriptor_list_dma_walks_a_ring_and_raises_its_source() -> None:
    state = _state(_ring(0x144117, 8))
    rx = SPI2_RX_DMA
    state.mmrs[rx] = Const(RAM)
    assert _dm_write(state, rx + 0x08, 4, Const(0x44117))
    assert _dma_start(state, rx) == RAM + 0x100
    assert state.mmrs[rx + 0x24] == Const(RAM + 0x14)
    _dma_done(state, rx, 70)
    assert state.mmrs[rx + 0x30].value & 0x701 == 0x201
    assert state.mmrs[rx + 0x04] == Const(RAM + 0x200)
    assert state.mmrs[rx + 0x28] == Const(RAM + 0x14)
    assert state.mmrs[SEC_CSID] == Const(70)
    # DMA_STAT.IRQDONE is write-one-to-clear; RUN is read-only.
    assert _dm_write(state, rx + 0x30, 4, Const(0x701))
    assert state.mmrs[rx + 0x30].value & 0x701 == 0x200
    _dma_done(state, rx, 70)
    assert state.mmrs[rx + 0x04] == Const(RAM + 0x100)


def test_spi2_exchange_swaps_wire_words_both_ways() -> None:
    ram = bytearray(0x400)
    ram[0:0x14] = _words(RAM, RAM + 0x100, 0x144117, 4, 2)
    ram[0x40:0x54] = _words(RAM + 0x40, RAM + 0x200, 0x144115, 4, 2)
    ram[0x200:0x208] = bytes(range(8))
    state = _state(bytes(ram))
    state.mmrs[SPI2_RX_DMA] = Const(RAM)
    state.mmrs[SPI2_TX_DMA] = Const(RAM + 0x40)
    assert _dm_write(state, SPI2_RX_DMA + 0x08, 4, Const(0x44117))
    assert _dm_write(state, SPI2_TX_DMA + 0x08, 4, Const(0x44115))
    reply = spi2_exchange(state, b"\x12\x34\x56\x78\x9a\xbc\xde\xf0")
    assert reply == swap16(bytes(range(8)))
    landed = bytes(state.overlay[RAM + 0x100 + k] for k in range(8))
    assert landed == b"\x34\x12\x78\x56\xbc\x9a\xf0\xde"
    # TX (69) is issued first; RX (70) waits for its END.
    assert state.mmrs[SEC_CSID] == Const(69)
    assert _sstat(state, 70) & 0x300 == 0x100


def test_disabled_model_keeps_plain_mmr_stores() -> None:
    state = _state()
    state.peripheral_model = False
    assert _dm_write(state, SEC_END, 4, Const(70))
    assert state.mmrs[SEC_END] == Const(70)
    assert SEC_SCTL + 8 * 70 + 4 not in state.mmrs


def _native(state: State):
    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).exists():
        pytest.skip("no native library built")
    core = nr.NativeCore(nr.pack_image(state.concrete), library)
    nr.to_native(core, state)
    return core


def test_native_twin_matches_reference_through_a_frame() -> None:
    ram = bytearray(0x400)
    ram[0:0x14] = _words(RAM, RAM + 0x100, 0x144117, 4, 2)
    ram[0x40:0x54] = _words(RAM + 0x40, RAM + 0x200, 0x144115, 4, 2)
    ram[0x200:0x208] = bytes(range(8))
    state = _state(bytes(ram))
    state.mmrs[SPI2_RX_DMA] = Const(RAM)
    state.mmrs[SPI2_TX_DMA] = Const(RAM + 0x40)
    core = _native(state)
    for base, cfg in ((SPI2_RX_DMA, 0x44117), (SPI2_TX_DMA, 0x44115)):
        assert _dm_write(state, base + 0x08, 4, Const(cfg))
        assert core.poke(base + 0x08, cfg.to_bytes(4, "little"), 4) == 1
    frame = b"\x12\x34\x56\x78\x9a\xbc\xde\xf0"
    assert core.spi2_exchange(frame) == spi2_exchange(state, frame)
    assert core.peek(SECI_ID, 4) == 69
    for address, value in ((SECI_ID, 0), (SEC_END, 69), (SECI_ID, 0), (SEC_END, 70)):
        assert _dm_write(state, address, 4, Const(value))
        assert core.poke(address, value.to_bytes(4, "little"), 4) == 1
    assert sd.compare_states(sd.export_state(state), core.export_state()) == []
    assert _sstat(state, 70) == 0
