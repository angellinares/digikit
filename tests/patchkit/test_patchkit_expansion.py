"""core/expansion: the reset-path loader and a ColdFire module placed in cf.expansion.

Slow (a ColdFire boot to ready per device, a few seconds each). Skips without the toolchain or the images."""

import json
import struct
import subprocess

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.firmware import config  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import (  # noqa: E402
    pk_cfcall,  # noqa: E402
    pk_toolchains,  # noqa: E402
)

DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
DN2_112 = config.stock("Digitone_II_OS1.12.syx")

pytestmark = pytest.mark.skipif(
    pk_toolchains.m68k_bin("gcc") is None
    or not DT2_117.exists()
    or not DN2_112.exists(),
    reason="m68k toolchain or firmware images missing",
)

REGION = 0x47E90000  # inside cf.expansion of every port (scratch for the unit test, after the boot)


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


def test_loader_copies_segments_and_zeroes_bss():
    """Run modules/core/expansion's loader on the core with stub helpers, over a region filled with 0xA5."""
    base, segs, h1, h2 = REGION, REGION + 0x1000, REGION + 0x2000, REGION + 0x2002
    d1, d2 = REGION + 0x4000, REGION + 0x5000
    linked = front_end.cf_link(
        [ROOT / "modules/core/expansion/src/cf/loader.s"],
        base,
        {"pk_segs": segs, "pk_copy_helper": h1, "pk_clear_helper": h2},
    )
    p1 = bytes(range(1, 17))
    chain = (
        struct.pack(">III", 16, d1, 32)
        + p1
        + struct.pack(">III", 0, d2, 16)
        + struct.pack(">I", 0xFFFFFFFF)
    )
    fill = b"\xa5" * 0x100
    m = pk_cfcall.CfMachine(
        DT2_117, state=ROOT / "out/cache/pk_cfcall/dt2-1.17-ldr.state"
    )
    r = m.call(
        base,
        [],
        pokes={
            base: linked.image,
            segs: chain,
            h1: b"\x4e\x75",
            h2: b"\x4e\x75",
            d1: fill,
            d2: fill,
        },
        reads=[(d1, 0x100), (d2, 0x100)],
    )
    assert r.ok and not r.clobbered
    got1, got2 = r.reads
    assert got1[:16] == p1
    assert got1[16:48] == bytes(32)  # bss zeroed
    assert got1[48:] == b"\xa5" * (0x100 - 48)  # nothing past it touched
    assert got2[:16] == bytes(16) and got2[16:] == b"\xa5" * (0x100 - 16)


@pytest.mark.parametrize("fw,base_sha", [(DT2_117, None), (DN2_112, None)])
def test_patched_image_boots_and_the_module_runs_in_place(fw, base_sha, tmp_path):
    out = tmp_path / (fw.stem + "-demo.syx")
    plan_path = tmp_path / "plan.json"
    patchkit(
        "apply",
        "--firmware",
        fw,
        "--modules",
        "test/cf-demo",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    sym = {
        k.split(":", 1)[1]: v
        for k, v in plan["symbols"].items()
        if k.startswith("test/cf-demo")
    }
    m = pk_cfcall.CfMachine(
        out, state=tmp_path / "boot.state", base_sha256=plan["base"]["syx_sha256"]
    )
    # code and data arrived intact, the function runs, the bss is zero
    r = m.run(
        {},
        [
            {"pc": sym["pk_demo_add"], "args": [2, 3], "reads": [(sym["pk_demo_table"], 32), (sym["pk_demo_bss"], 64)]},
            {"pc": sym["pk_demo_sum"]},
        ],
    )  # fmt: skip
    assert r[0].ok and r[0].d0 == 2 + 3 + 0x1234 and not r[0].clobbered
    table, bss = r[0].reads
    assert struct.unpack(">8I", table) == tuple(range(1, 9))
    assert bss == bytes(64)
    assert r[1].ok and r[1].d0 == 0x1234 + 36


@pytest.mark.parametrize("fw", ["Digitakt_II_OS1.17.syx"])
def test_machines_shims_live_in_the_expansion_region_not_the_cave(fw, tmp_path):
    """The native core executes from DDR (finding 07): core/machines keeps its code in cf.expansion and the flash cave holds
    the expansion loader only (56 bytes), with the machines framework alone."""
    firmware = config.stock(fw)
    if not firmware.exists():
        pytest.skip(f"{fw} missing")
    plan_path = tmp_path / "plan.json"
    patchkit(
        "apply",
        "--firmware",
        firmware,
        "--modules",
        "core/machines",
        "-o",
        tmp_path / "out.syx",
        "--plan",
        plan_path,
    )
    regions = json.loads(plan_path.read_text())["regions"]
    assert regions["cf.flash_cave"][0] <= 56, regions
    assert regions["cf.expansion"][0] > 600, regions
