"""Compact SHL1C range registers use independent words, including native image metadata."""

import os
import struct
from pathlib import Path

import pytest

from tools.sharc_dev import sharc_transpile_run as nr
from tools.sharc_dev import sharcldr
from tools.sharc_dev.sharc_core.memory import UnmodeledMMR, _dm_read, _dm_write
from tools.sharc_dev.sharc_core.state import State
from tools.sharc_dev.sharc_core.values import Const


def test_adjacent_cache_range_words_do_not_overlap_ram_bytes():
    state = State(
        pc_sw=0,
        concrete=sharcldr.LoadedMemory(b"", []),
        assume_nw32=True,
    )
    assert _dm_read(state, 0x3E018, 4) is None  # Naming adds no reset value.
    for offset, address in enumerate(range(0x3E010, 0x3E020)):
        assert _dm_write(state, address, 4, Const(0x12345600 + offset))
    for offset, address in enumerate(range(0x3E010, 0x3E020)):
        assert _dm_read(state, address, 4) == Const(0x12345600 + offset)
    assert not state.overlay  # These stores went to word MMRs, not RAM aliases.


def test_packed_names_enable_adjacent_cache_words_in_existing_native_engine():
    library = os.environ.get("SHARC_NATIVE_LIB", nr.DEFAULT_LIB)
    if not Path(library).is_file():
        pytest.skip("no native library built")
    core = nr.NativeCore(nr.pack_image(None), library)
    with pytest.raises(UnmodeledMMR):
        core.peek(0x3E018)  # No reset value is synthesized by the packed name.
    assert core.poke(0x3E002, struct.pack("<I", 0x300), width=4) == 1
    for offset, address in enumerate(range(0x3E010, 0x3E020)):
        assert core.poke(address, struct.pack("<I", 0x87654300 + offset), width=4) == 1
    for offset, address in enumerate(range(0x3E010, 0x3E020)):
        assert core.peek(address) == 0x87654300 + offset
    assert core.peek(0x3E002) == 0x300  # Range writes leave unaligned CFG2 intact.
