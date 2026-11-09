#!/usr/bin/env python3
"""Offline frequency-shifter PCM and float32-model oracles."""

from __future__ import annotations

import importlib.util
from pathlib import Path
from typing import Any

import numpy as np

from tools.patchkit import modroots
from tools.patchkit.hook_ab import compare_aligned as scaled
from tools.patchkit.pk_digikit_fxsend import aligned

FS = 48000.0
# Largest onset offset between two captures, in interleaved samples (128 audio frames, as in fx-live.ts). The note
# starts later when the blocks before it cost more DSP instructions, so the offset is capture timing, not audio;
# the bit-identical check after alignment is the real assertion. Gate runs under load gave 192 (96 frames).
MAX_ONSET_SHIFT = 256
AMOUNT = 40  # the send amount exercised by the acceptance captures


def channels(wav: Path) -> np.ndarray:
    """The left channel of a float WAV (the capture is stereo)."""
    data = wav.read_bytes()
    pos, nch = 12, 2
    while pos + 8 <= len(data):
        tag, size = (
            data[pos : pos + 4],
            int.from_bytes(data[pos + 4 : pos + 8], "little"),
        )
        if tag == b"fmt ":
            nch = int.from_bytes(data[pos + 10 : pos + 12], "little")
        elif tag == b"data":
            x = np.frombuffer(data[pos + 8 : pos + 8 + size // 4 * 4], dtype="<f4")
            return x.reshape(-1, nch)[:, 0].astype(np.float64)
        pos += 8 + size + (size & 1)
    raise ValueError(f"{wav}: no data chunk")


def amplitude(y: np.ndarray, f: float) -> float:
    """The amplitude of the component of y at exactly f Hz (a Hann-windowed single-bin projection)."""
    n = np.arange(len(y))
    w = np.hanning(len(y))
    return float(2 * abs(np.sum(w * y * np.exp(-2j * np.pi * f * n / FS))) / np.sum(w))


def find_f0(y: np.ndarray) -> float:
    """The frequency of the strongest component, refined by the projection (the window is 0.7 s: 1.4 Hz bins)."""
    spec = np.abs(np.fft.rfft(y * np.hanning(len(y)), 8 * len(y)))
    f = float(np.argmax(spec[100:])) + 100
    f0 = f * FS / (8 * len(y))
    best = f0
    for step in (0.2, 0.02, 0.002):
        grid = best + step * np.arange(-10, 11)
        best = float(grid[int(np.argmax([amplitude(y, g) for g in grid]))])
    return best


def model_out(x: np.ndarray, v: float, model: Any) -> np.ndarray:
    """The float32 model of the routine (`Shifter` of the module's model) on the mono signal X (both channels the same), SHIFT word V, from silent state; the left channel."""
    sh = model.Shifter()
    out = []
    for b in range(0, len(x) // 32 * 32, 32):
        blk = np.concatenate([x[b : b + 32], x[b : b + 32]]).astype(np.float32)
        out.append(sh.block(blk, v)[:32])
    return np.concatenate(out).astype(np.float64)


def load_model(module: str) -> Any:
    """The reference model `freqshift_ref.py` of the module folder, loaded by path."""
    path = modroots.module_dir(module) / "tests" / "freqshift_ref.py"
    spec = importlib.util.spec_from_file_location("freqshift_ref", path)
    assert spec and spec.loader, path
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def analyze_pcm(
    check: Any,
    pcm: dict[str, np.ndarray],
    raw_pcm: dict[str, np.ndarray],
    exp: dict[str, float],
    mixes: dict[str, float],
    model: Any,
) -> None:
    """Retained master/send spectral, exact PCM and independent float-model oracles."""
    # the sound: the sine of the dry run, then the effect's runs
    win = slice(int(0.3 * FS), int(1.0 * FS))
    y0 = pcm["dry"][win]
    f0 = find_f0(y0)
    a0 = amplitude(y0, f0)
    check(
        "dry: the sine is a clean tone (f0 found, amplitude above 0.01)",
        a0 > 0.01 and 20 < f0 < 8000,
        f"f0 {f0:.3f} Hz, amplitude {a0:.4f}",
    )
    rd = scaled(raw_pcm["dry"], raw_pcm["b"], 1.0)
    check(
        "shift B: with MIX 0 the PCM is the dry PCM bit for bit from the onset on",
        bool(rd["identical"]) and abs(rd["shift"]) <= MAX_ONSET_SHIFT,
        f"onset shift {rd['shift']}, max err {rd.get('max_abs_err')}",
    )
    ya = pcm["a"][win]
    aa = amplitude(ya, f0)
    rms = float(np.sqrt(np.mean(ya**2)) / np.sqrt(np.mean(y0**2)))
    check(
        "shift A: the defaults (MIX 127, SHIFT 0) keep the sine's frequency and level (an allpass pair: amplitude within 0.5 %, RMS within 0.5 %)",
        abs(aa / a0 - 1) < 0.005 and abs(rms - 1) < 0.005,
        f"f0 {f0:.3f}: amplitude ratio {aa / a0:.5f}, rms ratio {rms:.5f}",
    )
    for name, key in (("c", "up"), ("d", "down"), ("e", "e")):
        v = exp[key]
        inc = model.target_inc(v)
        sh = inc * FS / 2**32
        y = pcm[name][win]
        wanted, image, still = (
            amplitude(y, f0 + sh),
            amplitude(y, abs(f0 - sh)),
            amplitude(y, f0),
        )
        db = 20 * np.log10(wanted / max(image, 1e-12))
        check(
            f"shift {name.upper()}: SHIFT {v:+.3f} moves the sine from {f0:.2f} Hz by {sh:+.3f} Hz (the routine's mapping: inc {inc}); level within 0.5 %, the image at {abs(f0 - sh):.2f} Hz is {db:.1f} dB down (at least 40), nothing is left at f0",
            abs(wanted / a0 - 1) < 0.005 and db > 40.0 and still / wanted < 0.01,
            f"wanted {wanted:.4f} ({wanted / a0:.4f} of dry), image {image:.5f}, at f0 {still:.5f}, rejection {db:.1f} dB",
        )
    g = (AMOUNT / 127) ** 2
    v = exp["send"]
    inc = model.target_inc(v)
    sh = inc * FS / 2**32
    d_al, _ = aligned(pcm["dry"], pcm["send"])
    # the model: the shifter on the track's bus (the dry sound times the send gain), from silent state at 0.2 s (the filters settle in far less)
    t0, t1, t2 = int(0.2 * FS), int(0.3 * FS), int(1.0 * FS)
    mo = model_out(g * d_al[t0:t2], v, model)[t1 - t0 :]
    a_mod, i_mod = amplitude(mo, f0 + sh), amplitude(mo, abs(f0 - sh))
    figures = {}
    for tag in ("send", "half"):
        d, o = aligned(pcm["dry"], pcm[tag])
        added = (o - d)[win]
        mix = mixes[tag]
        wanted, image, still = (
            amplitude(added, f0 + sh),
            amplitude(added, abs(f0 - sh)),
            amplitude(added, f0),
        )
        db = 20 * np.log10(wanted / max(image, 1e-12))
        figures[tag] = wanted
        check(
            f"send {tag}: the FX return is the shifted track: amount {AMOUNT} (gain {g:.5f}) x MIX {mix:.4f} x the dry amplitude {a0:.4f}, at {f0 + sh:.2f} Hz (f0 {f0:.2f} + {sh:+.3f}); level within 0.5 %, the image at {abs(f0 - sh):.2f} Hz at least 40 dB down, nothing left at f0",
            abs(wanted / (g * mix * a0) - 1) < 0.005
            and db > 40.0
            and still / wanted < 0.01,
            f"wanted {wanted:.6f} ({wanted / (g * mix * a0):.5f} of g x mix x dry), image {image:.7f}, at f0 {still:.7f}, rejection {db:.1f} dB",
        )
        check(
            f"send {tag}: the float32 model (`Shifter` of the module's model on the dry sound times {g:.5f}) agrees: level of the shifted tone within 0.5 % of the model's x MIX, image level within 25 % of the model's",
            abs(wanted / (a_mod * mix) - 1) < 0.005
            and abs(image / (i_mod * mix) - 1) < 0.25,
            f"model {a_mod:.6f} (measured / model x mix {wanted / (a_mod * mix):.5f}), image {i_mod:.7f} (measured / model x mix {image / (i_mod * mix):.3f})",
        )
    r = figures["half"] / figures["send"]
    want = mixes["half"] / mixes["send"]
    check(
        f"send: MIX at {mixes['half']:.4f} halves the return: the added level is {want:.5f} of MIX 127's",
        abs(r / want - 1) < 0.005,
        f"{r:.5f} against {want:.5f}",
    )
    check(
        "send: the PCM differs from the dry PCM (the return is audible)",
        not bool(scaled(raw_pcm["dry"], raw_pcm["send"], 1.0)["identical"]),
        "",
    )
    ro = scaled(raw_pcm["dry"], raw_pcm["other"], 1.0)
    check(
        "other: the amount on track 2 only (silent) adds nothing to track 1: the PCM is the dry PCM bit for bit from the onset on, the shifter running on an empty bus",
        bool(ro["identical"]) and abs(ro["shift"]) <= MAX_ONSET_SHIFT,
        f"onset shift {ro['shift']}, max err {ro.get('max_abs_err')}",
    )


def pcm_report(request: dict[str, Any]) -> dict[str, Any]:
    """Analyze completed TS captures with the retained runner's assertions."""
    from tools.patchkit.hook_ab import read_wav

    tags = ("dry", "a", "b", "c", "d", "e", "send", "half", "other")
    raw_pcm = {}
    pcm = {}
    for tag in tags:
        path = Path(request["pcm"][tag])
        raw_pcm[tag] = read_wav(path)
        pcm[tag] = channels(path)
        if len(pcm[tag]) < int(FS) or not np.isfinite(raw_pcm[tag]).all():
            raise ValueError(f"{path}: need at least one second of finite PCM")
    exp = {key: float(request["shift"][key]) for key in ("up", "down", "e", "send")}
    mixes = {key: float(request["mix"][key]) for key in ("send", "half")}
    if not all(np.isfinite(v) for v in (*exp.values(), *mixes.values())):
        raise ValueError("SHIFT and MIX must be finite")
    if not all(0 < v <= 1 for v in mixes.values()):
        raise ValueError("send MIX must be above zero and at most one")
    control = pcm["dry"][int(0.3 * FS) : int(FS)]
    if amplitude(control, find_f0(control)) <= 0:
        raise ValueError("dry control must have nonzero tone amplitude")
    checks = []

    def check(label: str, passed: bool, detail: str = "") -> None:
        checks.append({"label": label, "pass": bool(passed), "detail": detail})

    analyze_pcm(check, pcm, raw_pcm, exp, mixes, load_model(request["module"]))
    return {"pass": all(item["pass"] for item in checks), "checks": checks}


def main(argv: list[str] | None = None) -> int:
    """JSON adapter for completed frequency-shifter captures; never starts a device."""
    import argparse
    import json
    import sys

    parser = argparse.ArgumentParser(description=main.__doc__)
    parser.add_argument("request", type=Path)
    args = parser.parse_args(argv)
    try:
        result = pcm_report(json.loads(args.request.read_text()))
    except (ValueError, KeyError, OSError) as exc:
        print(str(exc), file=sys.stderr)
        return 1
    print(json.dumps(result, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
