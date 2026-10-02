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
    DAI1_GBL_INT_EN,
    DAI1_GBL_SP_EN,
    SEC_CCTL,
    SEC_CSID,
    SEC_CSTAT,
    SEC_END,
    SEC_GCTL,
    SEC_SCTL,
    SECI_ID,
    SPORT4A_CTL,
    SPORT4B_CTL,
    _dma_done,
    _dma_start,
    _sec_line,
    _sec_raise,
)
from sharc_core.state import State
from sharc_core.values import Const
from sharc_periph_host import (
    SPI2_RX_DMA,
    SPI2_TX_DMA,
    SPORT4A_DMA,
    SPORT4B_DMA,
    spi2_exchange,
    sport_block,
    swap16,
)

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
    return state.mmrs.get(SEC_SCTL + 8 * sid + 4, Const(0)).value


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


def _sport_state(running: bool = True) -> State:
    """SPORT4A/B rings as DN2 sets them up, DAI1 group 0 interrupt on."""
    ram = bytearray(0x400)
    ram[0:0x14] = _words(RAM, RAM + 0x100, 0x144225, 4, 4)
    ram[0x40:0x54] = _words(RAM + 0x40, RAM + 0x200, 0x144227, 4, 4)
    ram[0x100:0x110] = _words(0xDEADBEEF, 1, 0x80000000, 0x12345678)
    state = _state(bytes(ram))
    state.mmrs[SEC_SCTL + 8 * 191] = Const(5)
    state.mmrs[DAI1_GBL_INT_EN] = Const(0x10003)
    state.mmrs[SPORT4A_DMA] = Const(RAM)
    state.mmrs[SPORT4B_DMA] = Const(RAM + 0x40)
    state.mmrs[SPORT4A_DMA + 0x08] = Const(0x44225)
    state.mmrs[SPORT4B_DMA + 0x08] = Const(0x44227)
    state.mmrs[DAI1_GBL_SP_EN] = Const(0x5F if running else 0x5E)
    state.mmrs[SPORT4A_CTL] = Const(0x111F3)
    state.mmrs[SPORT4B_CTL] = Const(0x111F3)
    return state


def test_sport_block_waits_for_the_enables() -> None:
    for patch in (DAI1_GBL_SP_EN, SPORT4A_CTL, SPORT4B_CTL):
        state = _sport_state()
        state.mmrs[patch] = Const(state.mmrs[patch].value & ~1)
        assert sport_block(state) is None
        assert not state.overlay
        assert _sstat(state, 191) == 0


def test_sport_block_returns_tx_words_and_raises_the_group_source() -> None:
    state = _sport_state()
    with pytest.raises(ValueError):
        sport_block(state, bytes(4))
    block = bytes(range(16))
    out = sport_block(state, block)
    assert out == _words(0xDEADBEEF, 1, 0x80000000, 0x12345678)
    landed = bytes(state.overlay[RAM + 0x200 + k] for k in range(16))
    assert landed == block
    for base in (SPORT4A_DMA, SPORT4B_DMA):
        assert state.mmrs[base + 0x30].value & 0x701 == 0x201
    # The group source fires once both IRQDONE bits are set; the channel
    # sources 53 and 55 stay quiet.
    assert _sstat(state, 191) & 0x300 == 0x100
    assert state.mmrs[SEC_CSID] == Const(191)
    assert SEC_SCTL + 8 * 53 + 4 not in state.mmrs
    # DMA_STAT W1C clears the group condition; the next block fires again.
    assert _dm_write(state, SEC_END, 4, Const(191)) is True
    for base in (SPORT4A_DMA, SPORT4B_DMA):
        assert _dm_write(state, base + 0x30, 4, Const(1))
    assert sport_block(state) is not None
    assert state.mmrs[SEC_CSID] == Const(191)
    zeros = bytes(state.overlay[RAM + 0x200 + k] for k in range(16))
    assert zeros == bytes(16)


def test_sport_group_waits_for_every_member() -> None:
    state = _sport_state()
    _dma_start(state, SPORT4A_DMA)
    _dma_done(state, SPORT4A_DMA, 53)
    assert _sstat(state, 191) == 0
    # Without the group enable a channel raises its own source.
    state.mmrs[DAI1_GBL_INT_EN] = Const(0)
    state.mmrs[SEC_SCTL + 8 * 55] = Const(5)
    _dma_start(state, SPORT4B_DMA)
    _dma_done(state, SPORT4B_DMA, 55)
    assert _sstat(state, 191) == 0
    assert _sstat(state, 55) & 0x300 == 0x100


def test_native_sport_block_matches_reference() -> None:
    state = _sport_state()
    core = _native(state)
    nr.to_native(core, state)
    block = bytes(range(16, 32))
    assert core.sport_block(block) == sport_block(state, block)
    assert sd.compare_states(sd.export_state(state), core.export_state()) == []
    core2 = _native(_sport_state(False))
    assert core2.sport_block() is None
    with pytest.raises(ValueError):
        core.sport_block(bytes(4))
