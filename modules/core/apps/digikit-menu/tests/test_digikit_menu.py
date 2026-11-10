"""core/digikit-menu (finding 28): the committed objects, the anchors on every port, the hook site, the proof tool's parsers.

The emulator proof is tools/coldfire/digikit_menu_proof.py (minutes); these are the quick checks."""

import json
import subprocess

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.coldfire import arena_safety, digikit_menu_proof  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import pk_toolchains  # noqa: E402

MODULE = ROOT / "modules/core/apps/digikit-menu"
IMAGES = {
    "dt2-1.17": "Digitakt_II_OS1.17.syx",
    "dn2-1.12": "Digitone_II_OS1.12.syx",
}


def test_committed_objects_are_current_and_plain_coldfire():
    obj = json.loads((MODULE / "build" / "digikit-menu.dko.json").read_text())
    assert {r["type"] for r in obj["relocs"]} == {"cf.32"}
    if pk_toolchains.m68k_bin("gcc") is None:
        pytest.skip("m68k-elf toolchain not installed")
    front_end.build_module(MODULE, check=True)


def patchkit(*args):
    exe = (
        front_end.binary()
    )  # one build under a lock, then an immutable copy (tools/devtools/stablebin.py)
    r = subprocess.run(
        [str(exe), "--root", str(ROOT), *map(str, args)],
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr + r.stdout


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_hook_displaces_the_selection_restore(image, tmp_path):
    """On every image the hook sits before `movea.l (a2),a0; pea (2).w; move.l d4,-(sp)` of the SETTINGS rebuild and the
    two displaced instructions run in the tail."""
    fw = config.stock(IMAGES[image])
    if not fw.exists():
        pytest.skip(f"{IMAGES[image]} missing")
    out, plan_path = tmp_path / "x.syx", tmp_path / "plan.json"
    patchkit(
        "apply",
        "--firmware",
        fw,
        "--modules",
        "core/digikit-menu",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    hooks = [
        h
        for h in json.loads(plan_path.read_text())["hooks"]
        if h["module"].split("@")[0] == "core/ui"
    ]
    assert len(hooks) == 1 and hooks[0]["anchor"] == "cf.menu.site.settings_list.tail"
    assert [i.split(None, 2)[2] for i in hooks[0]["displaced_insns"]] == [
        "movea.l (a2),a0",
        "pea.l (0x2).w",
    ]


def test_plan_helpers(tmp_path):
    plan = tmp_path / "p.json"
    plan.write_text(
        json.dumps(
            {
                "base": {"device": "dt2", "version": "1.17"},
                "regions": {"cf.expansion": [0x1000, 0x100000]},
                "symbols": {
                    "core/digikit-menu@0.1.0:TICK": 0x47E8C100,
                    "core/runtime@0.1.0:DKRT_STATE": 0x47E8C200,
                },
            }
        )
    )
    p = digikit_menu_proof.Plan(plan)
    assert p.addr("TICK") == 0x47E8C100 and p.addr("DKRT_STATE") == 0x47E8C200
    lo, hi = arena_safety.expansion("dt2-1.17")
    assert arena_safety.plan_allow_pc(plan) == "0x%x-0x%x" % (lo, lo + 0x1000)


def test_peek_parsers(tmp_path):
    (tmp_path / "probe.stdout").write_text(
        '{"peek_at":"0x47e8c100","t":5,"value":Some(7)}\n{"peek_at":"0x47e8c100","t":9,"value":None}\n'
        '{"peek":"0x47e8c100","value":Some(16909060)}\n{"peek":"0x47e8c104","value":None}\n'
    )
    assert digikit_menu_proof.peek_ats(tmp_path) == [7, None]
    assert digikit_menu_proof.peek(tmp_path, 0x47E8C100) == 0x01020304
    assert digikit_menu_proof.peek(tmp_path, 0x47E8C104) is None
    assert digikit_menu_proof.peek_bytes(tmp_path, 0x47E8C100, 3) == b"\x01\x02\x03"
