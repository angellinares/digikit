#!/usr/bin/env python3
"""A profile-guided `digi-emud` for the patchkit proofs (same profile as the launcher's PGO build).

    uv run python -m tools.patchkit.pk_emud_pgo build [--device dt2|dn2]    # builds if stale, prints the binary path
    uv run python -m tools.patchkit.pk_emud_pgo path  [--device dt2|dn2]    # the path, no build (exit 1 when missing)
    uv run python -m tools.patchkit.pk_emud_pgo bench [--device dt2] [--pairs 3] [--seconds 12]

The device's PGO profile (`tools/emulator/native_pgo.py train [--device dt2]`, out/native/pgo/<device>) is applied to the
windowless daemon the proofs drive. It builds into its own target dir (`<gen key>/pgo-emud-<device>` under
tools/emulator/native_target.py, never the launcher's `pgo-use`) and holds benchlock shared like every build. A profile that is
a little stale is fine (LLVM drops the data of functions whose code changed; see native_pgo.is_emulation_package).
The harness (tools/patchkit/pk_harness.py `daemon_binary`) uses this binary when it exists; DIGI_PK_PGO=0 turns that off.

`bench` runs one paired measurement on the same workload (restart, a held note, SECONDS of emulated audio) with the
plain and the PGO daemon alternating, under the exclusive benchlock, and prints wall seconds per pair and the median
ratio.
"""

from __future__ import annotations

import argparse
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools.devtools import benchlock  # noqa: E402
from tools.emulator import (  # noqa: E402
    digi_env,  # noqa: E402
    native_pgo,  # noqa: E402
    native_target,  # noqa: E402
)

FIRMWARE = {"dt2": "1.17", "dn2": "1.12"}  # the harness' default firmware per device


def generated_core(device: str) -> Path:
    """The generated core the device's ready DSP image is registered with (what the plain daemon build uses)."""
    from tools.patchkit import pk_harness as H

    syx, ready = H.firmware_inputs(H.Cell(device, FIRMWARE[device]))
    return digi_env.generated_dir(image=ready / "digi-audio-dn2-image.bin")


def target_dir(device: str) -> Path:
    return Path(native_target.target_dir(generated_core(device), f"pgo-emud-{device}"))


def binary_path(device: str) -> Path:
    return target_dir(device) / digi_env.TRIPLE / "release" / "digi-emud"


def build(device: str) -> Path:
    gen = generated_core(device)
    profile = native_pgo.current_profile(device, gen)
    if profile is None:
        raise SystemExit(
            f"no PGO profile for {device}: `uv run python -m tools.emulator.native_pgo train --device {device}`"
        )
    digi_env.check_generated(gen)
    env = native_pgo.base_env(gen)
    env["RUSTFLAGS"] = native_pgo.profile_use_flags(profile)
    env["RUSTDOC"] = str(Path(env["RUSTC"]).with_name("rustdoc"))
    command = [
        "rustup", "run", digi_env.TOOLCHAIN,
        "cargo", "build", "--release", "--locked",
        "--manifest-path", "packages/emulator/control/Cargo.toml",
        "--bin", "digi-emud",
        "--features", "coupled-audio",
        "--target", digi_env.TRIPLE,
        "--target-dir", str(target_dir(device)),
    ]  # fmt: skip
    started = time.monotonic()
    with benchlock.hold("shared", f"pk_emud_pgo build {device}"):
        done = subprocess.run(command, cwd=ROOT, env=env, stdout=sys.stderr)
    if done.returncode:
        raise SystemExit(done.returncode)
    print(f"built in {time.monotonic() - started:.0f} s", file=sys.stderr)
    binary = binary_path(device)
    if not binary.is_file():
        raise SystemExit(f"no binary at {binary}")
    return binary


def plain_binary(device: str) -> Path:
    from tools.patchkit import pk_harness as H

    syx, _ = H.firmware_inputs(H.Cell(device, FIRMWARE[device]))
    return H.daemon_binary(device, syx, pgo=False)


def workload(binary: Path, device: str, seconds: float) -> float:
    """Wall seconds of: spawn, restart, hold TRIG 1, SECONDS of emulated time (one process, exclusive benchlock held
    by the caller)."""
    from tools.patchkit import pk_harness as H

    cell = H.Cell(device, FIRMWARE[device])
    os.environ[f"DIGI_EMUD_{device.upper()}"] = str(binary)
    s = H.Session(cell, H.Goldens(), log=None, run_name=f"pgo-bench-{device}")
    with s:
        s.open()
        s.reset()
        s.run_ms(500)
        started = time.monotonic()
        s.do_hold("TRIG 1", 2000)
        s.run_ms(seconds * 1000 - 2000)
        s.health("bench")
        return time.monotonic() - started


def bench(device: str, pairs: int, seconds: float) -> int:
    pgo = binary_path(device)
    if not pgo.is_file():
        raise SystemExit(f"{pgo} is missing: build it first")
    plain = plain_binary(device)
    rows: list[tuple[float, float]] = []
    with benchlock.hold("exclusive", f"pk_emud_pgo bench {device}"):
        for i in range(pairs):
            order = (
                (plain, pgo) if i % 2 == 0 else (pgo, plain)
            )  # alternate: drift hits both
            got = {b: workload(b, device, seconds) for b in order}
            rows.append((got[plain], got[pgo]))
            print(
                f"pair {i + 1}: plain {rows[-1][0]:.2f} s, pgo {rows[-1][1]:.2f} s",
                flush=True,
            )
    ratio = statistics.median(a / b for a, b in rows)
    print(
        f"{device}: {seconds:g} emulated s; median plain/pgo = {ratio:.3f} ({(ratio - 1) * 100:+.0f} % faster)"
    )
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cmd", choices=("build", "path", "bench"))
    ap.add_argument("--device", default="dt2", choices=("dt2", "dn2"))
    ap.add_argument("--pairs", type=int, default=3)
    ap.add_argument("--seconds", type=float, default=12.0)
    a = ap.parse_args()
    os.chdir(ROOT)
    if a.cmd == "build":
        print(build(a.device))
        return 0
    if a.cmd == "path":
        p = binary_path(a.device)
        print(p)
        return 0 if p.is_file() else 1
    return bench(a.device, a.pairs, a.seconds)


if __name__ == "__main__":
    sys.exit(main())
