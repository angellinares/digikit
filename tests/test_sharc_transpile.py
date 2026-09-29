"""The SHARC+ core's Rust translation (tools/sharc_transpile.py) and the
native core built from it (native/sharc, tools/sharc_transpile_run.py).

The translation itself needs no firmware: it reads tools/sharc_core. The
native checks need the library built with the generated code
(``SHARC_GEN_DIR``; see tools/sharc_rsgen.py) and are skipped without it;
the frame check also needs the firmware and a capture.
"""

import os
import shutil
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import sharc_transpile as tp  # noqa: E402
import sharc_transpile_infer as infer  # noqa: E402
import sharc_transpile_run as run  # noqa: E402

LIB = run.DEFAULT_LIB
CAPTURE = os.path.join(
    ROOT, "out", "captures", "drive3", "dt2-1.16-drive3-trig1-emac.dt2cap"
)


@pytest.fixture(scope="module")
def translated(tmp_path_factory):
    core = infer.load(str(tmp_path_factory.mktemp("work")))
    return core, tp.translate(core)


def test_the_whole_core_translates(translated):
    core, out = translated
    assert out.report["functions_not_translated"] == {}
    assert out.report["functions_translated"] > 250
    # Every unannotated compute handler got its parameter types from its
    # callers.
    assert any("compute_alu" in name for name in core.annotated)
    for name in ("core_i.rs", "core_g.rs", "tables.rs", "syms.rs"):
        assert out.files[name]
    assert "pub fn _execute(" in out.files["core_g.rs"]
    assert tp.core_hash() in out.files["tables.rs"]


def test_translation_is_deterministic(translated, tmp_path):
    _core, out = translated
    again = tp.translate(infer.load(str(tmp_path)))
    assert again.files == out.files


def test_lattice_types():
    assert tp.normalize_union([tp.VAL, tp.NONE]) == tp.OPT(tp.VAL)
    assert tp.normalize_union([tp.VAL, tp.MR]) == tp.SPEC
    assert tp.normalize_union([tp.INT, tp.BOOL]) == tp.INT
    assert tp.normalize_union([tp.VAL, tp.INT]) == tp.VI
    u = tp.normalize_union([tp.INT, tp.STR])
    assert u.kind == "union" and set(u.args) == {tp.INT, tp.STR}


def test_a_construct_outside_the_subset_is_rejected(tmp_path):
    core_dir = tmp_path / "sharc_core"
    shutil.copytree(infer.CORE_DIR, core_dir)
    with open(core_dir / "compute_multi.py", "a") as fh:
        fh.write("\n\ndef _outside(x: int) -> int:\n    return (lambda y: y + 1)(x)\n")
    core = infer.load(str(tmp_path / "work"), core_dir=str(core_dir))
    out = tp.translate(core)
    why = out.report["functions_not_translated"]["sharc_core.compute_multi._outside"]
    assert "lambda" in why
    with pytest.raises(tp.TranspileError, match="lambda"):
        tp.translate(core, strict=True)


needs_lib = pytest.mark.skipif(
    not os.path.exists(LIB), reason="native core not built (native/sharc)"
)


@needs_lib
def test_native_compute_corpus_matches():
    summary = run.run_corpus(seed=7, cases_per_op=4, verbose=False)
    assert summary["python_errors_or_forks"] == 0
    assert summary["diverged"] == 0, summary["examples"]
    assert summary["native_traps"] == 0, summary["examples"]
    assert summary["match"] == summary["cases"]


@needs_lib
@pytest.mark.slow
@pytest.mark.skipif(not os.path.exists(CAPTURE), reason="no drive3 capture")
def test_native_frames_match_the_python_replay(tmp_path):
    out = run.run_frames(
        "dt2-1.16",
        CAPTURE,
        range(0, 3),
        snapshot=str(tmp_path / "start.snap"),
        verbose=False,
    )
    rows = out["frames"]
    assert [r["frame"] for r in rows] == [0, 1, 2]
    assert all(r["diff"] == [] for r in rows), rows


def test_block_mode_clean_up_helpers():
    # Dead constant assignments and their declarations go; a call stays.
    lines = [
        "#[inline(always)]",
        "pub fn f(s: &mut St) -> R<()> {",
        "    let mut a: Int = Default::default();",
        "    let mut b: Int = Default::default();",
        "    a = 5i128;",
        "    b = g(s)?;",
        "    {",
        "    }",
        "    return Ok(());",
        "}",
    ]
    out = tp._prune(lines)
    assert "    a = 5i128;" not in out and "    b = g(s)?;" in out
    assert not any(ln.strip() == "{" for ln in out)
    # A variant that only forwards to another is that one.
    fwd = "\n".join(
        [
            "#[inline(always)]",
            "pub fn __VARIANT__(s: &mut St, rf: &mut Rf, mut x: V) -> R<()> {",
            "    crate::generated::image::spec_01::g__v3(s, rf, x)?;",
            "    return Ok(());",
            "}",
        ]
    )
    assert tp._forwards_to(fwd) == "crate::generated::image::spec_01::g__v3"
    assert tp._forwards_to(fwd.replace("rf, x)?", "rf, 1i128)?")) is None
    pc = "\n".join(
        [
            "#[inline(always)]",
            "pub fn __VARIANT__(s: &mut St, rf: &mut Rf) -> R<()> {",
            "    rf.pc = 1881198i128;",
            "    return Ok(());",
            "}",
        ]
    )
    assert tp._only_sets_pc(pc) == "{ rf.pc = 1881198i128; }"


def test_partially_static_tuples_merge():
    a = tp.PS((1, tp._NOCONST))
    assert a == tp.PS((1, tp._NOCONST)) and a != tp.PS((2, tp._NOCONST))
    # An Optional tuple: None next to the tuple keeps its static items.
    assert tp._merge_item([a, None]) == a
    assert tp._merge_item([1, 1]) == 1
    assert tp._merge_item([1, 2]) is tp._NOCONST


IMAGE_DB = os.path.join(ROOT, "out", "sharcdb", "dt2-1.16.sqlite")


@pytest.mark.skipif(not os.path.exists(IMAGE_DB), reason="no program database")
def test_block_mode_keeps_registers_static(translated):
    """Block mode (tools/sharc_rsgen.py): an instruction of a hot DO loop
    specialises with every register index static, reading and writing the
    block's register file only."""
    import sharc
    import sharc_rsgen as rg

    _core, out = translated
    tr = out.translator
    tr.blk = True
    img = sharc.load("dt2-1.16")
    (block,) = rg.load_blocks(IMAGE_DB, "dt2-1.16", [0x1CB463], img._mem())
    for pc, insn in block.insns:
        tr.static_names[id(insn)] = "(&I_%X)" % pc
        tr.static_names[id(insn.fields)] = "(&F_%X)" % pc
    body = rg.plan_body(block, tr, 0, 0)
    assert body is not None and body.stopped is None
    assert len(body.steps) == len(block.insns)
    assert body.read_any and body.writes
    for _m, text in tr.variant_texts:
        assert "s.r[" not in text and "s_set_r" not in text
