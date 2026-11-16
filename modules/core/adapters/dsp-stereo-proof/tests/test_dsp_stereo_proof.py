"""Packaging of the private DN2 1.12 stereo proof observers: `patchkit apply` retargets only the five engine calls of the stock
image. The sound checks of the observers are in the `*.rig.test.ts` files of this folder (SHARC-only rig)."""

import hashlib
import json

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import patchkit
from tools.sharc_dev.sharc_disasm import decode_loaded_at
from tools.sharc_dev.sharcldr import LoadedMemory

IMAGE = REPO_ROOT / "out/sections/dn2-1.12/section_7_BLOB.bin"


@pytest.fixture(scope="module", autouse=True)
def prebuilt_tools():
    patch = pytest.MonkeyPatch()
    patch.setattr(patchkit, "_BUILT", patchkit.binary(build=False))
    yield
    patch.undo()


def test_packaging_retargets_only_the_five_engine_calls(tmp_path):
    from tools.patchkit.port_resolve import sections_of

    stock = REPO_ROOT / "firmware/Digitone_II_OS1.12.syx"
    if not IMAGE.exists() or not stock.exists():
        pytest.skip("original DN2 1.12 firmware and its section 7 required")
    assert (
        hashlib.sha256(IMAGE.read_bytes()).hexdigest()
        == "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
    )
    output, plan_path = tmp_path / "proof.syx", tmp_path / "plan.json"
    result = patchkit.run(
        [
            "apply",
            "--firmware",
            str(stock),
            "--modules",
            "core/dsp-stereo-proof",
            "-o",
            str(output),
            "--plan",
            str(plan_path),
        ],
        capture=True,
    )
    assert result.returncode == 0, result.stderr
    plan = json.loads(plan_path.read_text())
    assert [name.split("@")[0] for name in plan["modules"]] == ["core/dsp-stereo-proof"]
    original = LoadedMemory.from_stream(IMAGE.read_bytes())
    patched = LoadedMemory.from_stream(sections_of(output, (7,))[7])
    expected = bytearray(original.read_sw(0x1C8EF1, (0x1C9B84 - 0x1C8EF1) * 2))
    for site, name in [
        (0x1C94FD, "_sp_render"),
        (0x1C96CB, "_sp_reset"),
        (0x1C959F, "_uy_render"),
        (0x1C9B7A, "_uy_render"),
        (0x1C96AA, "_uy_reset"),
    ]:
        symbols = {
            value for key, value in plan["symbols"].items() if key.endswith(":" + name)
        }
        assert len(symbols) == 1
        insn = decode_loaded_at(patched, site)
        assert insn.type_name == "25a_direct"
        assert (
            (insn.fields["addr[23:16]"] << 16) | insn.fields["addr[15:0]"]
        ) == symbols.pop()
        start = (site - 0x1C8EF1) * 2
        expected[start : start + 6] = patched.read_sw(site, 6)
        assert patched.read_sw(
            site + 3, 12 if site == 0x1C96AA else 8
        ) == original.read_sw(site + 3, 12 if site == 0x1C96AA else 8)
    assert patched.read_sw(0x1C8EF1, len(expected)) == bytes(expected)
    for site in [
        0xB84D92,
        0xB84DA9,
        0xB834C8,
        0xB834CF,
        0x1C6EE9,
        0x1C7926,
        0xB836F6,
        0xB84876,
        0xB8488E,
    ]:
        assert patched.read_sw(site, 6) == original.read_sw(site, 6)
    for lo, hi in [
        (0xB80C39, 0xB80C6E),
        (0xB80B93, 0xB80BA5),
        (0xB81368, 0xB813E8),
        (0xB8135A, 0xB81368),
    ]:
        assert patched.read_sw(lo, (hi - lo) * 2) == original.read_sw(lo, (hi - lo) * 2)
