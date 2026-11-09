"""tools/coldfire/fw.py and tools/coldfire/atlas.py: pure helpers always, database queries when
the derived databases exist (out/cfdb, out/sharcdb, out/sections)."""

import os
import subprocess
import sys

import pytest
from support.paths import REPO_ROOT

ROOT = str(REPO_ROOT)

from tools import python_env  # noqa: E402
from tools.coldfire import (  # noqa: E402
    atlas,  # noqa: E402
    fw,  # noqa: E402
)

CF_DB = os.path.join(ROOT, "out", "cfdb", "dt2-1.17-emac.sqlite")
SHARC_DB = os.path.join(ROOT, "out", "sharcdb", "dt2-1.17.sqlite")
SECTION = os.path.join(ROOT, "out", "sections", "dt2-1.17", "section_3_MAIN_OS.bin")
need_cf = pytest.mark.skipif(not os.path.exists(CF_DB), reason="no cfdb for dt2-1.17")
need_sharc = pytest.mark.skipif(
    not os.path.exists(SHARC_DB), reason="no sharcdb for dt2-1.17"
)


def test_capped_adds_tail():
    assert fw.capped(["a", "b", "c"], 2) == "a\nb\n... 1 more (--max N, 0 = all)"
    assert fw.capped(["a", "b"], 0) == "a\nb"
    assert fw.capped(["a"], 5) == "a"


def test_parse_addr_and_cpu_choice():
    assert fw.parse_addr("0x401be1be") == 0x401BE1BE
    assert fw.parse_addr("FUN_401be1be") == 0x401BE1BE
    assert fw.parse_addr("401be1be") == 0x401BE1BE
    assert fw.parse_addr("PatternGridView") is None
    assert fw.want_sharc("0xb80105", None)
    assert not fw.want_sharc("0x401be1be", None)
    assert fw.want_sharc("0x401be1be", "sharc")


def test_tri_query():
    assert fw.tri_query("pattern step") == '"pattern" AND "step"'
    assert fw.tri_query("wrong ui") == '"wrong ui"'
    with pytest.raises(fw.FwError):
        fw.tri_query("  ")


def test_role_rules():
    assert atlas.role_of("SourcePageView") == "page"
    assert atlas.role_of("TempoMenuView") == "menu"
    assert atlas.role_of("ProgressWindow") == "window"
    assert atlas.role_of("Zzz") == "other"


def test_dossier_call_setup_and_anchors():
    text = "\n".join(
        [
            "; FUN_40003c02 @ 0x40003c02, 54 bytes",
            "FUN_40003c02:",
            "40003c02  4878 0007                 pea (0x7).w",
            "40003c06  41f9 4096 56a0            movea.l (DAT_409656a0).l,A0",
            "40003c0c  2f08                      move.l A0,-(SP)",
            "40003c12  4eb9 4000 5a56            jsr FUN_40005a56.l",
            "40003c18  588f                      addq.l 0x4,SP",
        ]
    )
    insns = fw.parse_disasm(text)
    assert [a for a, _ in insns][:2] == [0x40003C02, 0x40003C06]
    lines = fw.call_setup(insns, 0x40003C12, 2)
    assert len(lines) == 3
    assert lines[0].strip().startswith("40003c06")  # only the last 2 before the call
    lines = fw.call_setup(insns, 0x40003C12, 8)
    assert "hidden return ptr" in lines[1] and "hidden" not in lines[2]
    assert lines[-1].startswith("  > 40003c12")
    assert fw.hidden_return_ptr("lea (0x4,A1),A0")
    assert not fw.hidden_return_ptr("move.l A0,-(SP)")
    refs = [
        {
            "cpu": "cf",
            "name": "cf.x.fn.a",
            "mark": "D",
            "at": {"dt2-1.17": [{"addr": "0x40001004"}]},
        },
        {
            "cpu": "cf",
            "name": "cf.x.fn.b",
            "at": {"dt2-1.17": [{"addr": "0x40002000"}]},
        },
    ]
    assert fw.anchors_for(refs, "dt2-1.17", 0x40001000, 0x40001010) == [
        "cf.x.fn.a [D]+0x4"
    ]


def run(*argv: str) -> str:
    return fw.run_line(fw.Session(), list(argv), "dt2-1.17", 60)


@need_cf
def test_fn_and_callers():
    out = run("fn", "0x401be1be")
    assert out.startswith("401be1be FUN_401be1be")
    assert "callees:" in out
    assert "error" not in run("callers", "0x40111924")


@need_cf
def test_unknown_function_is_one_line_error():
    assert run("fn", "NoSuchThing_zzz").startswith("error: no function matches")


@need_cf
def test_class_and_str():
    out = run("class", "SourcePageView")
    assert "vtable" in out and "SourcePageView::vfunc_4" in out
    assert "WRONG UI CARD" in run("str", "WRONG UI")


@need_cf
def test_search_and_sql_and_cap():
    out = run("search", "pattern", "--max", "8")
    assert "names:" in out and "more" in out
    assert run("sql", "select count(*) from functions").splitlines()[1].isdigit()
    assert run("sql", "delete from functions").startswith("error: sql is read-only")


@need_cf
def test_path_and_port():
    assert "hops" in run("path", "0x400caff0", "0x40001928")
    assert "401be1be" in run(
        "port", "0x401bdee2", "--from", "dt2-1.16", "--to", "dt2-1.17"
    )


@need_cf
def test_batch_blocks(tmp_path):
    q = tmp_path / "q.txt"
    q.write_text("# c\nfn 0x401be1be --max 3\nclass Pattern --max 2\nbogus\n")
    out = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.coldfire.fw",
            "batch",
            str(q),
            "-i",
            "dt2-1.17",
        ],
        env=python_env(),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    heads = [ln for ln in out.splitlines() if ln.startswith("## ")]
    assert heads == ["## fn 0x401be1be --max 3", "## class Pattern --max 2", "## bogus"]
    assert "error: bad arguments" in out


@need_sharc
def test_sharc_fn_and_callers():
    assert run("fn", "0xb80105").startswith("b80105 FUN_b80105")
    assert "call sites" in run("callers", "0x1c0ccf")
    assert run("decomp", "0xb80105").startswith("error: no decompiler")


@need_cf
@pytest.mark.skipif(
    not os.path.exists(SECTION), reason="no MAIN OS section for dt2-1.17"
)
def test_atlas_build_and_query(tmp_path):
    counts = atlas.build("dt2-1.17", str(tmp_path))
    assert (
        counts["machines"] >= 5 and counts["params"] > 200 and counts["classes"] > 100
    )
    import sqlite3

    con = sqlite3.connect(str(tmp_path / "dt2-1.17.sqlite"))
    names = {r[0] for r in con.execute("select name1 from machines")}
    assert "SAMPLE" in names
    assert (
        con.execute("select count(*) from glossary where term='machine'").fetchone()[0]
        > 5
    )


@pytest.fixture
def store_db(tmp_path, monkeypatch):
    """Synthetic recorded facts: no firmware bytes or database builder required."""
    import hashlib
    import sqlite3

    from tools.sharc_dev.sharcdb.schema import DB_VERSION, SCHEMA

    path = tmp_path / "dn2-1.12.sqlite"
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    sha = hashlib.sha256(b"synthetic image").hexdigest()
    db.executemany(
        "INSERT INTO meta VALUES ('dn2-1.12',?,?)",
        [
            ("image", "dn2-1.12"),
            ("cpu", "sharc"),
            ("db_version", str(DB_VERSION)),
            ("image_sha256", sha),
            ("blob_path", "/removed/original/blob"),
        ],
    )
    for start in (0x10, 0x20, 0x30):
        db.execute(
            "INSERT INTO functions(image,entry_sw,end_sw,name) VALUES ('dn2-1.12',?,?,?)",
            (start, start + 0x10, f"fn_{start:x}"),
        )
    edges = [
        (0x11, 0x23, "call", 0x10, 0x20),
        (0x21, 0x10, "call", 0x20, 0x10),
        (0x22, 0x33, "jump", 0x20, 0x30),
        (0x24, None, "indirect", 0x20, None),
        (0x25, 0x99, "call", 0x20, None),
    ]
    db.executemany(
        "INSERT INTO edges(image,from_sw,to_sw,kind,from_function,to_function) VALUES ('dn2-1.12',?,?,?,?,?)",
        edges,
    )
    monkeypatch.setattr(fw, "SHARC_DIR", str(tmp_path))

    def store(sw, space="DM", base="I0", width="word", form="i,m-mod", owner=0x10):
        db.execute(
            "INSERT OR IGNORE INTO insn(image,sw,form,fields,confidence,function_sw,raw) VALUES ('dn2-1.12',?,?,'{}','recorded',?,'synthetic')",
            (sw, "1a" if form == "dual" else "3a", owner),
        )
        db.execute(
            "INSERT INTO mem_access(image,sw,space,direction,base_reg,modifier,u,form,width) VALUES ('dn2-1.12',?,?,'store',?,'M0',1,?,?)",
            (sw, space, base, form, width),
        )

    def pointer(sw, base="I0", address=0x100, width="word"):
        db.execute(
            "INSERT INTO ptr VALUES ('dn2-1.12',?,?,256,?,'store',?)",
            (sw, base, address, width),
        )

    store(0x12)
    pointer(0x12)
    store(0x26, base="I6", owner=0x20)
    store(0x31, owner=0x30)
    db.commit()
    return db, path, sha, store, pointer


def test_stores_roots_cycles_and_interior_targets(store_db):
    db, path, sha, _, _ = store_db
    before = path.read_bytes()
    img = fw.Sharc("dn2-1.12")
    report = fw.cmd_stores(img, ["000012"], sha, None)
    assert report["roots"] == [
        {"requested_pc": 0x12, "containing_function": 0x10, "is_function_entry": False}
    ]
    assert [r["entry_sw"] for r in report["reachable_functions"]] == [0x10, 0x20]
    edge = report["edges"][0]
    assert edge["to_sw"] == 0x23 and edge["to_function"] == 0x20
    assert edge["target_containing_functions"] == [0x20]
    assert not edge["target_is_recorded_entry"]
    assert {e["not_followed_reason"] for e in report["edges"]} >= {
        "unresolved_indirect_target",
        "non_call_cross_function_edge",
        "unresolved_or_mismatched_call_target",
    }
    assert report["counts"]["reachable_store_operands"] == 2
    assert report["counts"]["other_store_operands"] == 1
    assert report["counts"]["stack_candidate_operands"] == 1
    assert report["stores"][0]["resolution"] == "static_first_arrival_estimate"
    assert (
        report["provenance"]["image_verification"] == "metadata_only_not_bytes_verified"
    )
    assert report == fw.cmd_stores(img, ["000012"], sha, None)
    assert not img.db.in_transaction
    with pytest.raises(Exception, match="readonly"):
        img.db.execute("DELETE FROM ptr")
    assert path.read_bytes() == before
    assert not list(path.parent.glob("*.notes.sqlite"))


def test_stores_dual_operands_not_deduplicated_and_raw_widths(store_db):
    db, _, _, store, pointer = store_db
    store(0x14, form="dual", width=None)
    store(0x14, space="PM", base="I8", form="dual", width=None)
    pointer(0x14, width=None)
    pointer(0x14, base="I8", address=0x200, width=None)
    store(0x15, width="short")
    db.commit()
    report = fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], None, None)
    dual = [r for r in report["stores"] if r["mem_access"]["sw"] == 0x14]
    assert len(dual) == 2 and {r["mem_access"]["space"] for r in dual} == {"DM", "PM"}
    assert [r["address_estimate"] for r in dual] == [0x100, 0x200]
    assert report["counts"]["dual_operands"] == 2
    assert report["counts"]["pm_operands"] == 1
    assert report["counts"]["reachable_store_operands"] == 5
    assert report["counts"]["reachable_store_sites"] == 4
    assert (
        next(r for r in report["stores"] if r["mem_access"]["sw"] == 0x15)[
            "mem_access"
        ]["width"]
        == "short"
    )
    assert "footprint" in report["policy"] and not any("bytes" in r for r in dual)


def test_stores_ambiguous_pointer_and_bank_mismatch_stay_unknown(store_db):
    db, _, _, store, pointer = store_db
    pointer(0x12, address=0x300)
    store(0x13)
    pointer(0x13, base="I8")
    store(0x14, form="dual")
    store(0x14, space="PM", form="dual")
    pointer(0x14)
    db.commit()
    rows = fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], None, None)["stores"]
    for row in rows[:4]:
        assert row["resolution"] == "unknown" and row["address_estimate"] is None
    assert "ambiguous_pointer_to_operand_matching" in rows[0]["unknown_reasons"]
    assert (
        "pointer_base_or_width_mismatch_possible_bank_metadata_gap"
        in rows[1]["unknown_reasons"]
    )
    assert len(rows[0]["ptr_site_evidence"]) == 2
    assert rows[2]["compatible_ptr_rowids"] == rows[3]["compatible_ptr_rowids"]


def test_stores_blob_verification_and_cli_json_is_uncapped(store_db, tmp_path):
    _, _, sha, _, _ = store_db
    blob = tmp_path / "input.bin"
    blob.write_bytes(b"synthetic image")
    parser = fw.build_parser()
    args = parser.parse_args(
        [
            "stores",
            "000010",
            "-i",
            "dn2-1.12",
            "--max",
            "1",
            "--expected-image-sha256",
            sha,
            "--image-blob",
            str(blob),
        ]
    )
    import json

    report = json.loads(fw.run(fw.Session(), args))
    assert report["provenance"]["verified_image_sha256"] == sha
    assert report["provenance"]["meta"]["blob_path"] == "/removed/original/blob"
    assert report["provenance"]["selected_blob_path"] == str(blob)
    blob.write_bytes(b"changed")
    with pytest.raises(fw.FwError, match="image bytes differ"):
        fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], sha, str(blob))
    with pytest.raises(fw.FwError, match="cannot read"):
        fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], sha, str(blob) + ".missing")
    with pytest.raises(fw.FwError, match="expected image"):
        fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], "0" * 64, None)


@pytest.mark.parametrize(
    "change, error",
    [
        ("DELETE FROM meta WHERE key='image_sha256'", "missing per-image"),
        ("UPDATE meta SET value='0' WHERE key='db_version'", "stale/mismatched"),
        ("DROP TABLE ptr", "missing/stale schema"),
        ("DROP TABLE meta", "missing/stale schema"),
        ("UPDATE meta SET value='cf' WHERE key='cpu'", "stale/mismatched"),
    ],
)
def test_stores_fails_closed_without_rebuilding(store_db, change, error):
    db, path, _, _, _ = store_db
    db.execute(change)
    db.commit()
    before = path.read_bytes()
    with pytest.raises(fw.FwError, match=error):
        fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], None, None)
    assert path.read_bytes() == before


def test_stores_missing_database_does_not_create_it(tmp_path, monkeypatch):
    monkeypatch.setattr(fw, "SHARC_DIR", str(tmp_path))
    with pytest.raises(fw.FwError, match="missing database"):
        fw.Sharc("dn2-1.12")
    assert list(tmp_path.iterdir()) == []


def test_stores_missing_source_function_edge_remains_visible(store_db):
    db, _, _, _, _ = store_db
    db.execute(
        "INSERT INTO edges(image,from_sw,to_sw,kind) VALUES ('dn2-1.12', 22, 48, 'call')"
    )
    db.commit()
    report = fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], None, None)
    edge = next(e for e in report["edges"] if e["from_sw"] == 22)
    assert edge["source_containing_functions"] == [0x10]
    assert not edge["followed_by_policy"]
    assert (
        edge["not_followed_reason"] == "source_function_metadata_missing_or_mismatched"
    )
    assert 0x30 not in [f["entry_sw"] for f in report["reachable_functions"]]


def test_stores_entry_identity_is_independent_of_edge_normalization(store_db):
    db, _, _, _, _ = store_db
    db.executemany(
        "INSERT INTO edges(image,from_sw,to_sw,kind,from_function,to_function) VALUES ('dn2-1.12',?,?,'call',16,?)",
        [(0x16, 0x30, None), (0x17, 0x99, 0x99)],
    )
    db.commit()
    report = fw.cmd_stores(fw.Sharc("dn2-1.12"), ["000010"], None, None)
    known, missing = [e for e in report["edges"] if e["from_sw"] in (0x16, 0x17)]
    assert known["target_is_recorded_entry"] is True
    assert known["to_function"] is None
    assert missing["target_is_recorded_entry"] is False
    assert missing["to_sw"] == missing["to_function"] == 0x99
    assert not known["followed_by_policy"] and not missing["followed_by_policy"]
