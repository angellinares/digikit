"""Tests for tools/snapread.py: reading an emu snapshot without restoring a
Unicorn machine.

Skips (not fails) when no .snap file is present under snapshots/ -- those
are firmware-derived and this repo never commits them (CLAUDE.md's Rules).
"""

import glob
import os
import struct
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from snapread import PAGE, Snapshot  # noqa: E402

_CANDIDATES = ["snapshots/boot60M.snap", "snapshots/postintro.snap"]


def _find_snapshot():
    for rel in _CANDIDATES:
        path = os.path.join(ROOT, rel)
        if os.path.exists(path):
            return path
    found = glob.glob(os.path.join(ROOT, "snapshots", "**", "*.snap"), recursive=True)
    return found[0] if found else None


@pytest.fixture(scope="module")
def snap_path():
    path = _find_snapshot()
    if path is None:
        pytest.skip("no snapshots/*.snap present")
    return path


def test_regs_are_ints_and_readable(snap_path):
    s = Snapshot(snap_path)
    regs = s.regs
    assert set(regs) == {
        "d0",
        "d1",
        "d2",
        "d3",
        "d4",
        "d5",
        "d6",
        "d7",
        "a0",
        "a1",
        "a2",
        "a3",
        "a4",
        "a5",
        "a6",
        "a7",
        "pc",
        "sr",
    }
    assert all(isinstance(v, int) for v in regs.values())
    # regs is a fresh copy each time, not a live view into the blob
    assert s.regs is not regs
    regs["pc"] = 0
    assert s.regs["pc"] == s._blob["regs"]["pc"]


def test_read_matches_u32_u16_u8(snap_path):
    s = Snapshot(snap_path)
    base = s.mapped_bases[0]
    data = s.read(base, 8)
    assert data is not None and len(data) == 8
    assert s.u32(base) == struct.unpack(">I", data[:4])[0]
    assert s.u16(base) == struct.unpack(">H", data[:2])[0]
    assert s.u8(base) == data[0]


def test_read_across_page_boundary(snap_path):
    """A read spanning two mapped pages must not silently truncate (the
    scratch Mem.rd this promotes did: `pg[a-base:a-base+n]` past one page's
    end just returns fewer bytes)."""
    s = Snapshot(snap_path)
    bases = s.mapped_bases
    # find two contiguous mapped pages
    contiguous = [b for b in bases if (b + PAGE) in s._mapped]
    if not contiguous:
        pytest.skip("no two contiguous mapped pages in this snapshot")
    base = contiguous[0]
    data = s.read(base + PAGE - 4, 8)
    assert data is not None
    assert len(data) == 8


def test_unmapped_read_returns_none(snap_path):
    s = Snapshot(snap_path)
    mapped = s._mapped
    addr = 0x12340000
    while addr & ~(PAGE - 1) in mapped:
        addr += PAGE
    assert s.read(addr, 4) is None
    assert s.u32(addr) is None


def test_tasks_returns_list_of_dicts_or_empty(snap_path):
    s = Snapshot(snap_path)
    tasks = s.tasks()
    assert isinstance(tasks, list)
    for t in tasks:
        assert {"call_site", "tcb", "entry", "prio", "stack", "stacksize"} <= t.keys()
    if len(tasks) > 1:
        prios = [t["prio"] for t in tasks if t["prio"] is not None]
        assert prios == sorted(prios)


def test_name_for_known_cf_names_address():
    import cf_names
    from snapread import name_for

    some_name, (addr, *_rest) = next(iter(cf_names.ADDRS.items()))
    assert name_for(addr) == some_name
    assert name_for(0xFFFFFFFF) is None
