"""The frequency shifter on the concrete SHARC+ core behind core/fx (tests/support/fx_rig.py), held to the float32 model of freqshift_ref.py.

The routine and its model do the same float32 operations in the same order: the output of the routine (`rig.scratch()`, the effect's block before the adapter's mix) is the model's output
to the last bit. Tones through the routine show what the effect does: a 1 kHz sine moves by the shift, the image is 40 dB down. A long run (PyPy through uv when it can start, as the
conformance suite does) shows that the oscillator does not drift. The device page/knob proof is modules/fx/freqshift/tests/freqshift.device.test.ts.
Skips when Selache is not built."""

import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import freqshift_ref as R
import numpy as np
import pytest
from freqshift_tone import FS, amplitude, sine_blocks
from support.fx_rig import NFAULT, ONE, P0, FxRig, Row, f2w
from support.paths import REPO_ROOT

from tools import python_env
from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = Path(__file__).resolve().parents[1]
SRC = MODULE / "src/dsp/freqshift.s"
NFAULT_WORD = 17


def rig_for(v: float = 0.0, mix: int = ONE) -> FxRig:
    return FxRig(
        [Row("_fs_process", "", desc="_fs_desc", params=(f2w(v),), mix=mix)],
        sources=(SRC,),
        record_stores=True,
    )


def mixed_input(k: int, rng: np.random.Generator) -> np.ndarray:
    """Block k: a sine and noise, a different mix on each channel."""
    n = np.arange(k * 32, k * 32 + 32)
    s = 0.4 * np.sin(2 * np.pi * 440 * n / FS)
    left = s + rng.uniform(-0.3, 0.3, 32)
    right = 0.7 * s + rng.uniform(-0.5, 0.5, 32)
    return np.concatenate([left, right]).astype(np.float32)


@pytest.mark.parametrize("v", [0.0, 1.0, 20.0, 40.0, -40.0, 63.0, -64.0, 2e-6])
def test_the_routine_matches_the_float32_model(v):
    rig, model, rng = rig_for(v), R.Shifter(), np.random.default_rng(3)
    worst = 0.0
    for k in range(36):
        x = mixed_input(k, rng)
        b = rig.block([float(s) for s in x], ctx="friendly")
        assert b.returned and not b.ctx_bad and not b.regs_bad, (k, b.halt)
        got = np.array(rig.scratch(), dtype=np.float32)
        worst = max(worst, float(np.abs(got - model.block(x, v)).max()))
    # the bound is the last bit of a float32 near 1.0 (it is 0 where the SHARC+ FLOAT and TRUNC agree with numpy's)
    assert worst <= 2e-7, worst


def test_the_routine_follows_the_model_while_the_shift_moves():
    rig, model, rng = rig_for(), R.Shifter(), np.random.default_rng(5)
    steps = [0.0, 5.0, -20.0, 40.0, 63.0, -64.0, 0.5, -0.5, 33.0, 0.0]
    worst = 0.0
    for k in range(len(steps) * 5):
        v = steps[k // 5]
        rig.set_row_word(0, P0, f2w(v))
        x = mixed_input(k, rng)
        rig.block([float(s) for s in x], ctx="friendly")
        got = np.array(rig.scratch(), dtype=np.float32)
        worst = max(worst, float(np.abs(got - model.block(x, v)).max()))
    assert worst <= 2e-7, worst


@pytest.mark.parametrize("shift", [100.0, -100.0, 2000.0])
def test_a_tone_moves_by_the_shift_and_the_image_is_40_db_down(shift):
    v = R.v_for_hz(abs(shift)) * (1 if shift > 0 else -1)
    rig, f_in, blocks = rig_for(v), 1000.0, 200
    x = sine_blocks(f_in, blocks)
    wet = []
    for k in range(blocks):
        blk = [float(s) for s in x[k * 32 : k * 32 + 32]]
        rig.block(blk + blk, ctx="friendly")
        wet.extend(rig.scratch()[:32])
    exact = R.target_inc(v) * FS / 2**32
    skip = 40 * 32
    wanted = amplitude(np.array(wet), f_in + exact, skip)
    image = amplitude(np.array(wet), abs(f_in - exact), skip)
    assert abs(wanted - 0.5) < 0.005, wanted
    assert 20 * np.log10(wanted / image) > 40.0, (wanted, image)


def test_shift_zero_keeps_the_magnitude():
    for f_in in (1000.0, 12000.0):
        rig, blocks = rig_for(0.0), 120
        x = sine_blocks(f_in, blocks)
        wet = []
        for k in range(blocks):
            blk = [float(s) for s in x[k * 32 : k * 32 + 32]]
            rig.block(blk + blk, ctx="friendly")
            wet.extend(rig.scratch()[:32])
        a = amplitude(np.array(wet), f_in, 40 * 32)
        assert abs(a / 0.5 - 1.0) < 2e-3, (f_in, a)


def test_mix_is_the_frameworks_zero_is_dry_bit_for_bit_and_one_is_wet():
    rng = np.random.default_rng(9)
    dry, wet = rig_for(40.0, mix=0), rig_for(40.0, mix=ONE)
    for k in range(24):
        x = mixed_input(k, rng)
        a = dry.block([float(s) for s in x], ctx="friendly")
        b = wet.block([float(s) for s in x], ctx="friendly")
        assert a.master == tuple(float(s) for s in x), k
        if k:  # the first block fades the effect in
            assert b.master == tuple(wet.scratch()), k


def test_the_output_stays_below_the_level_bound_and_the_adapter_never_refuses_a_block():
    rng = np.random.default_rng(11)
    for amp, v in ((1.0, 30.0), (8.0, -45.0), (8.0, 0.0)):
        rig = rig_for(v)
        worst = 0.0
        for k in range(150):
            n = np.arange(k * 32, k * 32 + 32)
            x = np.concatenate(
                [
                    amp * np.sign(np.sin(2 * np.pi * 997 * n / FS)),
                    amp * rng.choice([-1.0, 1.0], 32),
                ]
            ).astype(np.float32)
            before = rig.row_word(0, NFAULT_WORD)
            b = rig.block([float(s) for s in x], ctx="friendly")
            assert rig.row_word(0, NFAULT_WORD) == before == 0, (amp, v, k)
            assert b.returned
            worst = max(worst, max(abs(s) for s in rig.scratch()))
        assert worst <= float(np.float32(7.9)), (amp, v, worst)  # the routine's clamp
        if amp == 8.0:
            assert worst > 3.0, (
                amp,
                v,
                worst,
            )  # an input at the level bound does reach the clamp's neighbourhood
    assert NFAULT == NFAULT_WORD


def test_writes_stay_in_the_output_block_and_the_state_and_the_cost_matches_the_description():
    rig = rig_for(25.0)
    desc = rig.desc_words("_fs_desc")
    assert desc[:3] == (0x444B4658, 1, 1) and desc[3] == 664 and desc[5:] == (1, 0)
    costs = []
    for k in range(6):
        b = rig.block([0.25 * np.sin(k + i) for i in range(64)], ctx="friendly")
        assert rig.foreign_stores(desc[6]) == [], k
        costs.append(b.instructions)
    steady = costs[-1]
    # core/fx around one call is about 520 instructions (finding 55); the declared worst case of the effect is above its own cost
    assert 3000 < steady < 4800, costs
    assert steady - 900 <= desc[4] <= 5000, (steady, desc[4])


def test_the_state_is_the_description_state_and_nothing_else():
    """Every word of the state that the routine writes lies in the first state_bytes (the rig reserves a poisoned 4 KiB; the adapter zeroes the declared size only)."""
    rig = rig_for(25.0)
    rig.block([0.1] * 64, ctx="friendly")
    nwords = rig.desc_words("_fs_desc")[3] // 4
    poison = 0x7FC00001
    beyond = [rig.state_word(0, k) for k in range(nwords, 1024)]
    assert all(w == poison for w in beyond)


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


def test_the_oscillator_does_not_drift_in_a_long_run():
    """1,500 blocks (1 s)."""
    check_long(long_run(1500, 600))


@pytest.mark.slow
def test_the_oscillator_does_not_drift_in_a_run_of_a_minute():
    """90,000 blocks (60 s of audio, about 10 minutes on PyPy)."""
    check_long(long_run(90000, 3600))


def test_the_module_declares_the_page_and_the_knob():
    man = tomllib.loads((MODULE / "module.toml").read_text())
    assert len(man["module"]["summary"]) < 120
    assert "fx" not in man  # the effect is declared in src/module.c
    (fx,) = pk_conform.fx_records(man["module"]["id"])
    assert (fx["id"], fx["tab"], fx["title"], fx["mix"]) == (
        "fshift",
        "FSH",
        "Freq Shifter",
        127,
    )
    # a send effect that may move to the master (step 5): MIX is the return level, so its default is 127
    assert (fx["route"], fx["routes"]) == ("send", ["send", "master"])
    (knob,) = fx["knob"]
    assert (knob["short"], knob["name"], knob["display"]) == (
        "SHIFT",
        "Freq Shift",
        "bipolar",
    )
    assert (knob["min"], knob["max"], knob["default"]) == (-64.0, 63.0, 0.0)
