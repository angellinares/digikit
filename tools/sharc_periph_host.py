"""Host side of the opt-in SHARC peripheral model (sharc_core/periph.py).

spi2_exchange() is one full-duplex SPI2 slave frame on a Python State: the
master's bytes land in the RX DMA work unit and the TX work unit's bytes go
back, then both channels complete (SEC sources 69 and 70). Each 16-bit SPI
word is MSB first on the wire (SPI_CTL.LSBF=0) and little-endian in memory
(16-bit MSIZE), so every 16-bit unit is byte-swapped in both directions.
"""

from __future__ import annotations

from sharc_core.periph import _dma_done, _dma_start, _mmr, _ram_word
from sharc_core.state import State

SPI2_TX_DMA = 0x3102D200
SPI2_RX_DMA = 0x3102D280
SID_SPI2_TXDMA = 69
SID_SPI2_RXDMA = 70


def swap16(data: bytes) -> bytes:
    out = bytearray(len(data))
    out[0::2] = data[1::2]
    out[1::2] = data[0::2]
    return bytes(out)


def spi2_exchange(state: State, frame: bytes) -> bytes:
    """Deliver FRAME (wire order) and return the DSP's reply (wire order)."""
    rx = _dma_start(state, SPI2_RX_DMA)
    tx = _dma_start(state, SPI2_TX_DMA)
    for base in (SPI2_RX_DMA, SPI2_TX_DMA):
        if _mmr(state, base + 0x10) != 2:
            raise ValueError("SPI2 DMA X modify is not 2 bytes")
        if 2 * _mmr(state, base + 0x0C) != len(frame):
            raise ValueError("SPI2 frame size does not match the work unit")
    if len(frame) % 4:
        raise ValueError("SPI2 frame is not a whole number of words")
    reply = bytearray()
    for offset in range(0, len(frame), 4):
        reply += _ram_word(state, tx + offset).to_bytes(4, "little")
    for offset, byte in enumerate(swap16(frame)):
        state.overlay[rx + offset] = byte
    _dma_done(state, SPI2_TX_DMA, SID_SPI2_TXDMA)
    _dma_done(state, SPI2_RX_DMA, SID_SPI2_RXDMA)
    return swap16(bytes(reply))
