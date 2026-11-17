"""digikit/freqshift: the assertions that no SHARC-rig check (tests/rig/freqshift.toml) replaces yet: the manifest and the committed knob, and the
60 s oscillator run (90,000 blocks, slow). Kept until T1g phase 5 ports it (docs/plans/ledgers/t1g-retirement.md "Kept until phase 5").
The model-only tests are in test_freqshift_ref.py. The device page/knob proof is fx-bipolar.device.test.ts of core/fxpages. The run skips when Selache is not built."""

import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import numpy as np
import pytest
from support.paths import REPO_ROOT

from tools import python_env
from tools.patchkit import pk_toolchains

MODULE = Path(__file__).resolve().parents[1]


def _python() -> list[str]:
    if shutil.which("uv"):
        probe = subprocess.run(
            [
                "uv",
                "run",
                "--no-project",
                "--python",
                "pypy3.11",
                "python",
                "-c",
                "pass",
            ],
            capture_output=True,
            cwd=REPO_ROOT,
        )
        if probe.returncode == 0:
            return ["uv", "run", "--no-project", "--python", "pypy3.11", "python"]
    return [sys.executable]


def long_run(blocks: int, timeout: int) -> dict:
    env = python_env()
    env["PYTHONPATH"] = os.pathsep.join([env["PYTHONPATH"], str(REPO_ROOT / "tests")])
    r = subprocess.run(
        [
            *_python(),
            str(Path(__file__).with_name("freqshift_long.py")),
            "--blocks",
            str(blocks),
        ],
        capture_output=True,
        text=True,
        cwd=REPO_ROOT,
        env=env,
        timeout=timeout,
    )
    assert r.returncode == 0, r.stderr[-2000:]
    return json.loads(r.stdout.strip().splitlines()[-1])


def check_long(rep: dict) -> None:
    # the oscillator is an integer: the phase is the closed form after any number of blocks
    assert rep["phase_word"] == rep["phase_want"], rep
    wins = rep["windows"]
    amps = [w["amplitude"] for w in wins]
    assert max(amps) - min(amps) < 1e-4, amps
    assert abs(amps[0] - 0.5) < 2e-3, amps[0]
    # the tone's phase at its exact frequency does not move from the first window to the last: no drift of the frequency
    # (1e-3 rad over the run is, in 60 s, 3e-6 Hz)
    phases = np.unwrap([w["phase"] for w in wins])
    assert np.abs(phases - phases[0]).max() < 1e-3, phases.tolist()
    assert rep["peak"] < 0.8


# kept until T1g phase 5 ports it
@pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)
@pytest.mark.slow
def test_the_oscillator_does_not_drift_in_a_run_of_a_minute():
    """90,000 blocks (60 s of audio, about 10 minutes on PyPy)."""
    check_long(long_run(90000, 3600))


# kept until T1g phase 5 ports it
def test_the_module_declares_one_effect_by_builder_calls():
    """The page, the knob, the route and the MIX default are builder calls in init: the device test reads the rows the module commits."""
    man = tomllib.loads((MODULE / "module.toml").read_text())
    assert len(man["module"]["summary"]) < 120
    assert [(f["id"], f["dsp"]) for f in man["fx"]] == [("fshift", "fs")]


def test_the_committed_shift_knob_is_bipolar():
    """`module:check` writes build/gen/gallery.json from the records `init` committed: the SHIFT knob carries the bipolar display of the record (DK_P_BIPOLAR) and -64 .. +63."""
    consts = {
        c["name"]: c["value"]
        for c in tomllib.loads((REPO_ROOT / "sdk/sdk.toml").read_text())["machine"][
            "consts"
        ]
    }
    gallery = json.loads((MODULE / "build/gen/gallery.json").read_text())
    (knob,) = gallery["effects"][0]["knobs"]
    assert (
        knob["short"],
        knob["display"],
        knob["min"],
        knob["max"],
        knob["default"],
    ) == (
        "SHIFT",
        consts["DK_P_BIPOLAR"],
        -64,
        63,
        0,
    )
