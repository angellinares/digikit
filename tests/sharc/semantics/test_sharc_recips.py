"""RECIPS / RSQRTS (SHARC+ PRM pp.19-16..19-18): the seed tables and the special cases.

The core reproduces the 6694 items of the probe capture on a Digitakt II (OS 1.17) exactly: result, ASTAT[13:0] and STKY[9:0]
(`tests/fixtures/sharc/recips-rsqrts-dt2-1.17.json`, written by `tools/sharc_dev/recip_tables.py`). The other tests pin special cases
and the 8-bit accuracy of the seeds.
"""

from __future__ import annotations

import struct
from pathlib import Path

from support.paths import REPO_ROOT

from tools.sharc_dev import recip_tables
from tools.sharc_dev.sharc_core.compute_alu import _alu_recip_seed_impl
from tools.sharc_dev.sharc_core.floats import Const

FIXTURE = Path(REPO_ROOT) / recip_tables.FIXTURE


def f2b(x: float) -> int:
    return struct.unpack("<I", struct.pack("<f", x))[0]


def b2f(b: int) -> float:
    return struct.unpack("<f", struct.pack("<I", b))[0]


def seed(name: str, bits: int) -> int:
    _rn, value, _op, _updates = _alu_recip_seed_impl(0, 0, Const(bits), name)
    assert isinstance(value, Const), value
    return value.value & 0xFFFFFFFF


def test_core_reproduces_the_device_capture():
    items = recip_tables.load_items(FIXTURE)
    assert len(items) == 6694
    assert recip_tables.check(items) == []


def test_special_cases():
    assert seed("recips", 0x00000000) == 0x7F800000  # +0 -> +inf
    assert seed("recips", 0x80000000) == 0xFF800000  # -0 -> -inf
    assert seed("recips", 0x7FC00000) == 0xFFFFFFFF  # NaN
    assert seed("recips", f2b(2.0**126)) == 0  # exponent above 125
    assert seed("recips", f2b(-(2.0**126))) == 0x80000000
    assert seed("rsqrts", 0x00000000) == 0x7F800000
    assert seed("rsqrts", 0x7F800000) == 0  # +inf -> +0
    assert seed("rsqrts", f2b(-4.0)) == 0xFFFFFFFF  # negative


def test_seeds_are_8_bit_accurate():
    for x in (1.0, 1.5, 3.0, 48000.0, 1.00065, 1e-3, 123456.0, -7.25):
        s = seed("recips", f2b(x))
        assert s & 0x7FFF == 0
        assert abs(b2f(s) * x - 1.0) < 2.0**-7, x
    for x in (1.0, 2.0, 48000.0, 0.01, 1e5):
        s = seed("rsqrts", f2b(x))
        assert s & 0x7FFF == 0
        assert abs(b2f(s) * x**0.5 - 1.0) < 2.0**-7, x
