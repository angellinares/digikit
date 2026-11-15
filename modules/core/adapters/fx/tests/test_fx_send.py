"""core/fx, the send route (finding 62): the firmware-image checks of the send hook and the anchors on the Digitakt II 1.17 image
(skipped when it is not in the repository). The send hook and the master hook on the SHARC core run on the rig
(modules/core/adapters/fx/tests/fx-send.rig.test.ts)."""

import json
import os
import subprocess
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit as front_end

ROOT = REPO_ROOT

# ---- patchkit: the hook on the Digitakt II -------------------------------------------------------------------------------
DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
needs_image = pytest.mark.skipif(
    not DT2_117.exists(), reason="stock firmware image not present"
)


@pytest.fixture(scope="module")
def patchkit():
    exe = front_end.binary()

    def run(*args):
        r = subprocess.run(
            [str(exe), "--root", str(ROOT), *map(str, args)],
            capture_output=True,
            text=True,
            env={**os.environ, "PATCHKIT_MODULES": str(ROOT / "out" / "modules")},
        )
        assert r.returncode == 0, f"patchkit {args}: {r.stderr}{r.stdout}"
        return r

    return run


@needs_image
def test_the_send_hook_takes_the_first_instruction_of_the_master_function(
    patchkit, tmp_path: Path
):
    out = tmp_path / "fx.syx"
    plan_path = tmp_path / "fx.plan.json"
    patchkit(
        "apply",
        "--firmware",
        DT2_117,
        "--modules",
        "core/fx",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    (hook,) = [h for h in plan["hooks"] if h["anchor"] == "dsp.fx.fn.send"]
    assert (
        hook["cpu"] == "sharc" and hook["displaced"] == "run" and hook["site_len"] == 6
    )
    assert hook["tail"] is not None
    assert (
        hook["ret"] == 0x1C207E
    )  # the second instruction of FUN_1c207b (the first is 3 parcels)
    assert any(k.endswith(":_fxs_entry") for k in plan["symbols"])


@needs_image
def test_the_anchors_of_the_send_route_hold_the_track_bus_and_the_return_bus():
    port = (ROOT / "devices/ports/dt2-1.17.toml").read_text()
    for name, value in (
        ("dsp.fx.value.send.track_bus", 0x252DF8),
        ("dsp.fx.value.send.return_bus", 0x254778),
    ):
        i = port.index(f'name = "{name}"')
        block = port[i : i + 400]
        assert f"addr = {value:#x}" in block, name
