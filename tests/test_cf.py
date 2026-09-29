"""Tests for tools/cf.py, the tools/cfdb.py query layer (mirrors
tests/test_sharc_symbols.py's convention: skip, don't fail, when the
firmware-derived database this repo never commits is absent)."""

import os
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import cf  # noqa: E402
import cf_names  # noqa: E402

IMAGE_NAME = "dt2-1.16-emac"
DUMP_DIR = os.path.join(ROOT, "out", "ghidra", IMAGE_NAME)


def _dump_present():
    return os.path.exists(os.path.join(DUMP_DIR, "manifest.json"))


@pytest.fixture(scope="module")
def img(tmp_path_factory):
    if not _dump_present():
        pytest.skip("out/ghidra/%s missing (manifest.json)" % IMAGE_NAME)
    db_dir = tmp_path_factory.mktemp("cfdb")
    image = cf.load(IMAGE_NAME, ghidra_dir=DUMP_DIR, db_dir=str(db_dir))
    yield image
    image.close()


def test_func_by_address_and_by_name_agree(img):
    addr, mark, desc, source = cf_names.ADDRS["bgworker_ctor"]
    by_addr = img.func(addr)
    by_cf_name = img.func("bgworker_ctor")
    by_ghidra_name = img.func(by_addr["name"])
    assert by_addr is not None
    assert by_addr["entry"] == by_cf_name["entry"] == by_ghidra_name["entry"]
    assert by_addr["cf_name"] == "bgworker_ctor"


def test_func_unknown_returns_none(img):
    assert img.func("not_a_real_symbol_name") is None
    assert img.func(0x12345678) is None


def test_callers_include_jump_edges_not_just_call(img):
    """tools/sharcdb.py's own module docstring documents a real JUMP-into-
    function edge that a CALL-only caller scan missed for weeks; callers()
    here must not repeat that mistake -- it should return edges of every
    kind by default, only filtering when `kinds` is passed explicitly."""
    addr, *_ = cf_names.ADDRS["bgworkerbase_ctor"]
    all_kinds = img.callers(addr)
    call_only = img.callers(addr, kinds=("call",))
    assert len(all_kinds) >= len(call_only)
    assert all(c["kind"] == "call" for c in call_only)


def test_reach_and_paths_agree_on_a_known_call(img):
    ctor_addr, *_ = cf_names.ADDRS["bgworker_ctor"]
    base_addr, *_ = cf_names.ADDRS["bgworkerbase_ctor"]
    reach = img.reach(ctor_addr, max_depth=1)
    assert "0x%x" % base_addr in reach
    paths = img.paths(ctor_addr, base_addr, max_depth=2)
    assert ["0x%x" % ctor_addr, "0x%x" % base_addr] in paths


def test_refs_to_returns_data_and_code_buckets(img):
    addr, *_ = cf_names.ADDRS["live_track_base_ptr"]
    refs = img.refs_to(addr)
    assert "data" in refs and "code" in refs
    assert len(refs["data"]) > 0


def test_sql_escape_hatch(img):
    rows = img.sql("SELECT count(*) FROM functions WHERE image=?", img.name)
    assert rows[0][0] > 0
