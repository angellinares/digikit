#!/usr/bin/env python3
"""Does a trig on a DT2 card sample play that sample? A spectrum check.

    uv run python -m tools.emulator.dt2_sample_check [--seconds 1.6] [--rebuild]

Builds a card with one 1 kHz mono sine (`test-sine-1khz.wav`, 48 kHz, 1 s), its
ready state (`tools/emulator/ready.py dt2 --card-image`, cached by card), runs the
`dt2-sample-trig` scenario of tests/scenarios/dt2.json headless
(`coupled-run --card-image`, interpreter only: about 80 s per audio second) and
analyses the left channel while the note sounds: the strongest partial, the
share of the power near it, and the two harmonics above it.

The sample is the card's, not a synthesis: a clean playback has one dominant
partial at 1 kHz times the track's pitch ratio and nothing else of note.
Exit status 0 only when the tone is clean (peak share >= 0.9).
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.machinery
import importlib.util
import math
import os
import pathlib
import struct
import subprocess
import sys
import wave

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools import python_env  # noqa: E402
from tools.emulator import digi_env  # noqa: E402


def native_emu():
    loader = importlib.machinery.SourceFileLoader(
        "native_emu", str(ROOT / "tools" / "emulator" / "native_emu.py")
    )
    spec = importlib.util.spec_from_loader("native_emu", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def write_sine(path: pathlib.Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(48000)
        w.writeframes(
            b"".join(
                struct.pack(
                    "<h", int(0.5 * 32767 * math.sin(2 * math.pi * 1000 * i / 48000))
                )
                for i in range(48000)
            )
        )


def read_left(path: pathlib.Path) -> list[float]:
    data = path.read_bytes()
    at = data.find(b"data")
    size = struct.unpack_from("<I", data, at + 4)[0]
    floats = struct.unpack_from("<%df" % (size // 4), data, at + 8)
    return list(floats[0::2])


def analyse(left: list[float], rate: float = 48000.0) -> dict:
    onset = next(i for i, v in enumerate(left) if abs(v) > 1e-4)
    start = onset + 2400  # 50 ms into the note
    n = 2048
    seg = [
        left[start + i] * (0.5 - 0.5 * math.cos(2 * math.pi * i / (n - 1)))
        for i in range(n)
    ]
    power = []
    for k in range(1, n // 2):
        re = sum(seg[i] * math.cos(2 * math.pi * k * i / n) for i in range(n))
        im = sum(seg[i] * math.sin(2 * math.pi * k * i / n) for i in range(n))
        power.append(re * re + im * im)
    peak = max(range(len(power)), key=power.__getitem__)
    near = sum(power[max(0, peak - 3) : peak + 4])
    harmonics = {}
    for h in (2, 3):
        j = peak * h
        if j + 3 < len(power):
            harmonics[h] = 10 * math.log10(
                max(power[j - 3 : j + 4]) / power[peak] + 1e-30
            )
    return {
        "onset_s": onset / rate,
        "peak_hz": (peak + 1) * rate / n,
        "peak_share": near / sum(power),
        "harmonics_db": harmonics,
        "rms": math.sqrt(sum(v * v for v in seg) / n),
    }


def prepare() -> tuple[pathlib.Path, pathlib.Path, pathlib.Path, pathlib.Path]:
    """The card, its ready state and the firmware: (syx, card, ready_dir, work)."""
    work = digi_env.run_dir("dt2-sample-check")
    samples = work / "samples"
    write_sine(samples / "test-sine-1khz.wav")
    syx = digi_env.firmware_for("dt2")
    ne = native_emu()
    pinned = os.environ.get("DIGI_DT2_CARD")
    if pinned:
        # a card image kept from an earlier builder: the oracle values of
        # tests/regression/scenarios/dt2-fx.test.ts are tied to the ready state it boots to
        card = pathlib.Path(pinned).resolve()
    else:
        card = ne.plusdrive_card(samples, syx, digi_env.rust_env(dict(os.environ)))
    ready = subprocess.run(
        [sys.executable, "-m", "tools.emulator.ready", "dt2", "--syx", str(syx),
         "--card-image", str(card)],
        cwd=ROOT, env=python_env(), capture_output=True, text=True, check=True,
    )  # fmt: skip
    ready_dir = next(
        pathlib.Path(line.partition("=")[2])
        for line in ready.stdout.splitlines()
        if line.startswith("ready_dir=")
    )
    return syx, card, ready_dir, work


def card_inputs() -> tuple[dict[str, pathlib.Path], list[str]]:
    """The coupled-run inputs on the card's ready state, and `--card-image CARD`."""
    syx, card, ready, _ = prepare()
    return (
        {
            "firmware": syx,
            "DSP image": ready / "digi-audio-dn2-image.bin",
            "DSP state": ready / "digi-audio-m5.snap.dsp",
            "CF snapshot": ready / "digi-audio-m5.snap",
        },
        ["--card-image", str(card)],
    )


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--seconds", type=float, default=1.6)
    p.add_argument("--rebuild", action="store_true", help="rebuild coupled-run first")
    p.add_argument(
        "--binary", type=pathlib.Path, help="run this coupled-run (e.g. a PGO build)"
    )
    p.add_argument("--wav", type=pathlib.Path, help="output WAV (default trig.wav)")
    p.add_argument(
        "--env", action="append", default=[], metavar="K=V",
        help="environment of the run (default SHARC_FAST_REGIONS=: AOT, no fast tier)",
    )  # fmt: skip
    p.add_argument("--fast", action="store_true", help="the default fast tier too")
    a = p.parse_args(argv)
    syx, card, ready_dir, work = prepare()
    from tools.emulator import coupled

    binary = a.binary or coupled.ensure_binary(
        not a.rebuild, "dt2", ready_dir / "digi-audio-dn2-image.bin", pgo=True
    )
    wav = a.wav or work / "trig.wav"
    env = coupled.run_env({}) if a.fast else {**os.environ, "SHARC_FAST_REGIONS": ""}
    env.update(kv.partition("=")[::2] for kv in a.env)
    subprocess.run(
        [
            str(binary), "--syx", str(syx), "--card-image", str(card),
            "--dsp-image", str(ready_dir / "digi-audio-dn2-image.bin"),
            "--dsp-state", str(ready_dir / "digi-audio-m5.snap.dsp"),
            "--cf-snapshot", str(ready_dir / "digi-audio-m5.snap"),
            "--scenarios", str(ROOT / "tests/scenarios/dt2.json"),
            "--scenario", "dt2-sample-trig", "--seconds", str(a.seconds),
            "--wav", str(wav),
        ],
        cwd=ROOT, env=env, check=False, capture_output=True,
    )  # fmt: skip
    result = analyse(read_left(wav))
    print(result)
    print("wav sha256", hashlib.sha256(wav.read_bytes()).hexdigest()[:16])
    ok = result["peak_share"] >= 0.9
    print("clean tone" if ok else "NOT a clean tone (peak share < 0.9)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
