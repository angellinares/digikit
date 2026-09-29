"""Tests for tools/cf_names.py: the hand-curated named-ColdFire-address
table for DT2 1.16. No firmware data needed -- this is a pure data/lookup
module, so these always run."""

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import cf_names  # noqa: E402

_VALID_MARKS = {"[V]", "[D]", "[O]", "[C]", "unmarked"}


def test_every_entry_has_a_well_formed_tuple():
    assert len(cf_names.ADDRS) > 0
    for name, entry in cf_names.ADDRS.items():
        assert isinstance(name, str) and name
        addr, mark, desc, source = entry
        assert isinstance(addr, int) and 0 <= addr <= 0xFFFFFFFF
        assert mark in _VALID_MARKS
        assert isinstance(desc, str) and desc
        assert isinstance(source, str) and source.endswith(".md")


def test_name_by_addr_reverse_index_is_consistent():
    for _name, (addr, *_rest) in cf_names.ADDRS.items():
        assert cf_names.NAME_BY_ADDR.get(addr) is not None


def test_describe_known_and_unknown():
    name, (addr, mark, desc, source) = next(iter(cf_names.ADDRS.items()))
    text = cf_names.describe(addr)
    assert text is not None
    assert name in text and mark in text and source in text
    assert cf_names.describe(0xDEADBEEF) is None
