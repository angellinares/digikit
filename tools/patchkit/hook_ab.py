#!/usr/bin/env python3
"""Paired runs of a coupled scenario with and without a DSP patch, and a PCM
comparison of the two renders (spike 0.7).

    uv run python -m tools.patchkit.hook_ab run LABEL --device dn2|dt2 --scenario NAME
        [--patch FILE] [--seconds S] [--dt2-card] [-- EXTRA coupled-run args]
    uv run python -m tools.patchkit.hook_ab compare BASE.wav TEST.wav [--gain G] [--align-onset] [--json]

`run` renders into out/runs/hook/LABEL/ (report.json, audio.wav) through
tools/emulator/coupled.py, so the benchlock and the ready inputs are the usual ones.
`compare` reads two float32 stereo WAVs and prints: identical or not, RMS
ratio in dB, the largest |test - G * base| (for a pure gain G), and the peak
ratio. With --gain it exits 1 unless the error is within one float32 step of
the scaled sample (the hook multiplies in float32 and the converter rounds to
Q31, so a 2**-23 relative error is the floor). --align-onset compares the
common signal length from each onset, and --json prints only the metrics.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

import numpy as np

from tools import python_env

ROOT = Path(__file__).resolve().parents[2]


def read_wav(path: Path) -> np.ndarray:
    """Float32 interleaved samples of a RIFF IEEE-float WAV (format 3)."""
    data = path.read_bytes()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise SystemExit(f"{path}: not a WAV")
    pos = 12
    fmt = None
    while pos + 8 <= len(data):
        tag = data[pos : pos + 4]
        size = int.from_bytes(data[pos + 4 : pos + 8], "little")
        if tag == b"fmt ":
            fmt = int.from_bytes(data[pos + 8 : pos + 10], "little")
        elif tag == b"data":
            if fmt != 3:
                raise SystemExit(f"{path}: expected IEEE float WAV, format {fmt}")
            body = data[pos + 8 : pos + 8 + size]
            return np.frombuffer(body[: len(body) // 4 * 4], dtype="<f4")
        pos += 8 + size + (size & 1)
    raise SystemExit(f"{path}: no data chunk")


def compare(base: np.ndarray, test: np.ndarray, gain: float | None) -> dict:
    n = min(len(base), len(test))
    a, b = base[:n].astype(np.float64), test[:n].astype(np.float64)
    rms_a = math.sqrt(float(np.mean(a * a)))
    rms_b = math.sqrt(float(np.mean(b * b)))
    out = {
        "samples": n,
        "length_equal": len(base) == len(test),
        "identical": bool(np.array_equal(base[:n], test[:n])),
        "rms_base": rms_a,
        "rms_test": rms_b,
        "rms_db": 20 * math.log10(rms_b / rms_a) if rms_a > 0 and rms_b > 0 else None,
        "peak_ratio": float(np.max(np.abs(b)) / np.max(np.abs(a)))
        if np.max(np.abs(a)) > 0
        else None,
        "active_samples": int(np.count_nonzero(a)),
    }
    if gain is not None:
        err = np.abs(b - gain * a)
        out["max_abs_err"] = float(err.max())
        # a Q31 output step is 2**-31; the float path adds one float32 ulp of the sample
        out["max_err_ulps"] = float(
            (err / np.maximum(np.abs(a) * 2.0**-23, 2.0**-31)).max()
        )
    return out


def compare_aligned(ref: np.ndarray, run: np.ndarray, gain: float) -> dict:
    """RUN against REF times GAIN from the note's onset on (the boots reach the note up to a few blocks apart): `hook_ab.compare`'s figures over the
    common length, and the onset shift in samples."""

    def onset(x: np.ndarray, thr: float) -> int:
        nz = np.flatnonzero(np.abs(x) > thr)
        return int(nz[0]) if len(nz) else -1

    # the threshold scales with the gain, so that a quieter run is not found later on the same attack ramp
    a, b = onset(ref, 1e-6), onset(run, 1e-6 * max(abs(gain), 1e-3))
    if a < 0 or b < 0:
        return {"shift": None, "max_err_ulps": 1e9, "rms_db": None}
    n = min(len(ref) - a, len(run) - b)
    r = compare(ref[a : a + n], run[b : b + n], gain)
    r["shift"] = b - a
    return r


def cmd_compare(args: argparse.Namespace) -> int:
    base, test = read_wav(args.base), read_wav(args.test)
    res = (
        compare_aligned(base, test, args.gain)
        if args.align_onset
        else compare(base, test, args.gain)
    )
    ok = True
    if args.gain is not None:
        ok = res.get("length_equal", False) and res["max_err_ulps"] <= 2.0
    if args.json:
        print(json.dumps(res, allow_nan=False))
    else:
        for k, v in res.items():
            print(f"{k:16} {v}")
        if args.gain is not None:
            print("PASS" if ok else "FAIL", f"(gain {args.gain}: error within 2 ulps)")
    return 0 if ok else 1


def cmd_run(args: argparse.Namespace) -> int:
    out_dir = ROOT / "out" / "runs" / "hook" / args.label
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        "uv", "run", "python", "-m", "tools.emulator.coupled", "scenario", args.scenario,
        "--no-build", "--wav", "--device", args.device, "--out", str(out_dir / "report.json"),
    ]  # fmt: skip
    if args.dt2_card:
        cmd.append("--dt2-card")
    if args.seconds is not None:
        cmd += ["--seconds", str(args.seconds)]
    extra = list(args.extra)
    if args.patch:
        extra = ["--dsp-patch", str(Path(args.patch).resolve()), *extra]
    if extra:
        cmd += ["--", *extra]
    print(" ".join(cmd))
    return subprocess.call(cmd, cwd=ROOT, env=python_env())


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("label")
    r.add_argument("--device", choices=["dn2", "dt2"], required=True)
    r.add_argument("--scenario", required=True)
    r.add_argument("--patch")
    r.add_argument("--seconds", type=float)
    r.add_argument("--dt2-card", action="store_true")
    r.add_argument("extra", nargs="*")
    r.set_defaults(fn=cmd_run)
    c = sub.add_parser("compare")
    c.add_argument("base", type=Path)
    c.add_argument("test", type=Path)
    c.add_argument("--gain", type=float)
    c.add_argument(
        "--align-onset",
        action="store_true",
        help="compare from each signal's onset (requires --gain)",
    )
    c.add_argument(
        "--json", action="store_true", help="print only comparison metrics as JSON"
    )
    c.set_defaults(fn=cmd_compare)
    args = p.parse_args(argv)
    if args.cmd == "compare" and args.align_onset and args.gain is None:
        c.error("--align-onset requires --gain")
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
