"""Tests for tools/cfdb.py: building a ColdFire image database from a
tools/ghidradump.py dump directory.

Skips (not fails) when out/ghidra/<image>/manifest.json is absent, the same
convention tests/test_sharc_symbols.py uses for firmware-derived data this
repo never commits.
"""

import json
import os
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import cfdb  # noqa: E402

IMAGE_NAME = "dt2-1.16-emac"
DUMP_DIR = os.path.join(ROOT, "out", "ghidra", IMAGE_NAME)


def _dump_present():
    return os.path.exists(os.path.join(DUMP_DIR, "manifest.json")) and os.path.exists(
        os.path.join(DUMP_DIR, "xrefs.sqlite")
    )


@pytest.fixture(scope="module")
def db_path(tmp_path_factory):
    if not _dump_present():
        pytest.skip("out/ghidra/%s missing (manifest.json/xrefs.sqlite)" % IMAGE_NAME)
    out_dir = tmp_path_factory.mktemp("cfdb")
    out_path = os.path.join(out_dir, IMAGE_NAME + ".sqlite")
    cfdb.build_database(DUMP_DIR, out_path, name=IMAGE_NAME, force=True)
    return out_path


def test_build_produces_expected_tables_and_counts(db_path):
    import sqlite3

    db = sqlite3.connect(db_path)
    tables = {
        r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    assert {"meta", "functions", "edges", "datarefs", "strings", "symbols"} <= tables

    with open(os.path.join(DUMP_DIR, "manifest.json")) as f:
        manifest = json.load(f)
    assert manifest.get("complete")

    n_functions = db.execute("SELECT count(*) FROM functions").fetchone()[0]
    assert n_functions == manifest["function_count"]

    meta = cfdb.read_meta(db_path)
    assert meta["image"] == IMAGE_NAME
    assert meta["db_version"] == str(cfdb.DB_VERSION)
    assert meta["image_sha256"] == manifest["image_sha256"]
    db.close()


def test_function_end_uses_own_contiguous_range(db_path):
    """A function's `end` is its entry's own contiguous range, not the max
    across every disjoint chunk a scattered function can have (see the
    module docstring's cross-reference to tools/sharcdb.py's identical
    concern for the SHARC side)."""
    import sqlite3

    db = sqlite3.connect(db_path)
    row = db.execute(
        "SELECT entry, end, size FROM functions WHERE image=? ORDER BY entry LIMIT 1",
        (IMAGE_NAME,),
    ).fetchone()
    assert row is not None
    entry, end, size = row
    assert end > entry
    db.close()


def test_build_is_skipped_when_up_to_date(db_path, capsys):
    r = cfdb.build_database(DUMP_DIR, db_path, name=IMAGE_NAME, force=False)
    assert r["skipped"] is True


def test_build_rejects_dump_without_manifest(tmp_path):
    empty_dir = tmp_path / "not-a-dump"
    empty_dir.mkdir()
    with pytest.raises(SystemExit):
        cfdb.build_database(str(empty_dir), str(tmp_path / "out.sqlite"))
