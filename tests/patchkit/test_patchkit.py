"""patchkit (packages/patchkit): the codec, the linker and the oracles, against the Python reference tools.

Needs the stock images in the repository root and a built `patchkit` binary (built here with cargo, offline).
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools import python_env  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import port_resolve  # noqa: E402

DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
DN2_112 = config.stock("Digitone_II_OS1.12.syx")

pytestmark = pytest.mark.skipif(
    not all(p.exists() for p in (DT2_117, DN2_112)),
    reason="stock firmware images not present",
)


@pytest.fixture(scope="module")
def patchkit(tmp_path_factory):
    exe = (
        front_end.binary()
    )  # one build under a lock, then an immutable copy (tools/devtools/stablebin.py)

    def run(*args, check=True):
        r = subprocess.run(
            [str(exe), "--root", str(ROOT), *map(str, args)],
            capture_output=True,
            text=True,
            env={**os.environ, "PATCHKIT_MODULES": str(ROOT / "out" / "modules")},
        )
        if check and r.returncode:
            raise AssertionError(f"patchkit {args}: {r.stderr}{r.stdout}")
        return r

    return run


def apply(patchkit, tmp_path, fw, mods, *extra):
    out = tmp_path / (Path(fw).stem + "-" + mods.replace("/", "_") + ".syx")
    plan = out.with_suffix(".plan.json")
    patchkit(
        "apply", "--firmware", fw, "--modules", mods, "-o", out, "--plan", plan, *extra
    )
    return out, json.loads(plan.read_text())


def test_aplib_packer_equals_python_on_small_sections(patchkit):
    from tools.patchkit import pk_aplib_eq

    rust = pk_aplib_eq.rust_bin()
    assert rust.exists()
    r = subprocess.run(
        [sys.executable, "-m", "tools.patchkit.pk_aplib_eq", "--only", "2,7,8", "--rust", str(rust),
         str(config.stock("Digitakt_II_OS1.17.syx")), str(config.stock("Digitone_II_OS1.12.syx"))],
        cwd=ROOT,
        env=python_env(),
        capture_output=True,
        text=True,
    )  # fmt: skip
    assert r.returncode == 0, r.stdout + r.stderr


def test_dn2_master_half_equals_dsp_patch_py(patchkit, tmp_path):
    subprocess.run(
        [sys.executable, "-m", "tools.patchkit.pk_oracle_dn2_half"],
        cwd=ROOT,
        env=python_env(),
        check=True,
        capture_output=True,
    )
    out, _ = apply(patchkit, tmp_path, DN2_112, "oracle/dn2-master-half", "--no-record")
    ref = tmp_path / "ref.syx"
    subprocess.run(
        [sys.executable, "-m", "tools.patchkit.dsp_patch", "syx", str(DN2_112),
         str(ROOT / "tools" / "patchkit" / "dsp_patches" / "dn2-1.12-master-half.patch"), str(ref)],
        env=python_env(),
        check=True,
        capture_output=True,
    )  # fmt: skip
    a, b = port_resolve.sections_of(out), port_resolve.sections_of(ref)
    assert a == b, {k: a[k] == b[k] for k in a}


@pytest.mark.parametrize("fw", [DT2_117, DN2_112])
def test_core_fx_applies_and_verifies(patchkit, tmp_path, fw):
    from tools.firmware import roundtrip

    out, plan = apply(patchkit, tmp_path, fw, "core/fx")
    v = patchkit("verify", out, "--base", fw)
    assert "record: base" in v.stdout
    assert roundtrip.check_authenticity(out.read_bytes(), str(out))
    # sections 2, 4 and 8 are the stock bytes
    secs, stock = port_resolve.sections_of(out), port_resolve.sections_of(fw)
    assert all(secs[k] == stock[k] for k in (2, 3, 4, 5, 8))
    # a patched image is never patched again
    again = patchkit("apply", "--firmware", out, "--modules", "core/fx", check=False)
    assert again.returncode != 0 and "no port" in again.stderr
