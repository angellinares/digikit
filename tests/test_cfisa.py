"""The ColdFire instruction table (tools/cfisa/coldfire.json), its generated
decoder (native/coldfire/src/decode_gen.rs) and the decoder's agreement with
Ghidra, SLEIGH and Unicorn on the firmware images (tools/cfisa/oracle.py)."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools" / "cfisa"))

import gen  # noqa: E402
import oracle  # noqa: E402


def test_generated_decoder_is_current():
    """The table is unambiguous over all 65536 opwords and the committed
    Rust matches what the generator writes from it."""
    assert gen.main(["--check"]) == 0


def test_every_form_cites_a_manual_page():
    _, forms = gen.load()
    for f in forms:
        assert f.page.startswith(("CFPRM p.", "RM p.")), f.id


def test_sleigh_normalisation():
    assert oracle.norm_sleigh("divsl.l", "D5,D1:D1", "divs_l", 0) == ("divs.l", "d5,d1")
    assert oracle.norm_sleigh("divsl.l", "D5,D2:D1", "rems_l", 0) == (
        "rems.l",
        "d5,d2,d1",
    )
    assert oracle.norm_sleigh("mvz.b:", "(0x1,A1), D0", "mvz", 0) == (
        "mvz.b",
        "(0x1,a1),d0",
    )
    assert oracle.norm_sleigh("jsr", "0x100.l", "jsr", 0) == ("jsr", "(0x100).l")
    assert oracle.norm_sleigh("jsr", "0x1000", "jsr", 0x2000) == ("jsr", "(-0x1002,pc)")


@pytest.mark.skipif(shutil.which("cargo") is None, reason="needs cargo")
def test_crate():
    subprocess.run(
        ["cargo", "test", "--release", "--quiet"],
        cwd=ROOT / "native" / "coldfire",
        check=True,
    )


@pytest.mark.slow
@pytest.mark.parametrize("image", oracle.DEFAULT_IMAGES)
def test_decoder_agrees_with_oracles_on_firmware(image):
    path, dump = oracle.image_paths(image)
    if not path.exists() or not (dump / "disasm").is_dir():
        pytest.skip("needs out/sections/%s and its Ghidra dump" % image)
    oracle.load_flows()
    oracle.load_ctrl_names()
    r = oracle.check_image(image, use_unicorn=True, show=5)
    assert r["bad_counts"] == {}, r["bad"]
    assert r["stats"]["agree"] == r["stats"]["total"]
