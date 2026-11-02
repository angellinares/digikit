#!/usr/bin/env python3
r"""Closed-loop hardware debugging: press keys on a real Digitakt II / Digitone II through the digikit remote module, record its USB audio with ffmpeg (avfoundation) and analyse each hit (pitch, length, level).

    uv run --with python-rtmidi python -m tools.device.devprobe --list-audio
    uv run --with python-rtmidi python -m tools.device.devprobe --steps 'trig1*10@500'
    uv run --with python-rtmidi python -m tools.device.devprobe --device dn2 --steps 'turn:B:-3,wait:250,trig1*4@400,shot:end'
    uv run --with python-rtmidi python -m tools.device.devprobe --no-remote --tail 10      # capture only, play by hand
    uv run python -m tools.device.devprobe --analyse capture.wav                           # any WAV, no device

Steps (comma separated): `LABEL[*COUNT][@MS]` presses a key by label (trig1, play, stop, ...), `key:CODE[*COUNT][@MS]`
a key by code, `turn:ENC:DELTA` (ENC A..H, LEVEL or 1..9), `wait:MS`, `shot[:NAME]` a screenshot.
Results go to --out (default out/runs/devprobe/<time>): capture.wav, events.json, hits.json, screenshots.
Event times are seconds since the capture file appeared; hit times are seconds in the WAV.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from tools.emulator import audiocheck

REPO = Path(__file__).resolve().parents[2]
LEDS = {"dt2": "digitakt-ii-leds.toml", "dn2": "digitone-ii-leds.toml"}
PORTS = {"dt2": "Digitakt", "dn2": "Digitone"}

# A run is "consistent" when the hits agree: f0 within 3 %, length within 15 %,
# peak level within 2 dB.
F0_SPREAD_MAX = 1.03
DURATION_SPREAD_MAX = 1.15
PEAK_RANGE_DB_MAX = 2.0

MAX_HIT_S = 2.0  # longest segment per hit
F0_WINDOW_S = 0.2  # pitch is measured in the first 200 ms
DECAY_DB = 30.0  # the length ends 30 dB below the hit's envelope peak
KEY_WINDOW_S = 1.0  # a key press this close before a hit is its cause


@dataclass
class Press:
    code: int | None
    label: str
    count: int = 1
    gap_ms: int = 500


@dataclass
class Turn:
    encoder: int
    delta: int


@dataclass
class Wait:
    ms: int


@dataclass
class Shot:
    name: str | None = None


Step = Press | Turn | Wait | Shot

ENCODERS = {c: i + 1 for i, c in enumerate("ABCDEFGH")} | {"LEVEL": 9}


def _norm(label: str) -> str:
    return re.sub(r"\s+", "", label).lower()


def load_key_map(device: str) -> dict[str, int]:
    """Normalised key label (lower case, no spaces) -> key code."""
    path = REPO / "devices" / LEDS[device]
    data = tomllib.loads(path.read_text())
    return {_norm(str(k["label"])): int(k["code"]) for k in data.get("key", [])}


def _int(text: str, what: str) -> int:
    try:
        return int(text, 0)
    except ValueError:
        raise ValueError(f"bad {what}: {text!r}") from None


def _repeat(text: str) -> tuple[str, int, int]:
    """Split `NAME[*COUNT][@MS]` into (NAME, COUNT, MS)."""
    m = re.fullmatch(r"([^*@]+)(?:\*(\w+))?(?:@(\w+))?", text)
    if not m:
        raise ValueError(f"bad step: {text!r}")
    count = _int(m.group(2), "count") if m.group(2) else 1
    gap = _int(m.group(3), "gap") if m.group(3) else 500
    if count < 1 or gap < 0:
        raise ValueError(f"bad count or gap in {text!r}")
    return m.group(1), count, gap


def parse_steps(text: str) -> list[Step]:
    steps: list[Step] = []
    for raw in text.split(","):
        item = raw.strip()
        if not item:
            raise ValueError(f"empty step in {text!r}")
        head, _, rest = item.partition(":")
        kind = head.lower()
        if kind == "key" and rest:
            name, count, gap = _repeat(rest)
            code = _int(name, "key code")
            steps.append(Press(code, f"key:{code}", count, gap))
        elif kind == "turn":
            parts = rest.split(":")
            if len(parts) != 2:
                raise ValueError(f"turn needs ENC:DELTA: {item!r}")
            enc = parts[0].upper()
            encoder = ENCODERS[enc] if enc in ENCODERS else _int(parts[0], "encoder")
            if not 1 <= encoder <= 9:
                raise ValueError(f"encoder out of range 1..9: {item!r}")
            steps.append(Turn(encoder, _int(parts[1], "delta")))
        elif kind == "wait":
            ms = _int(rest, "wait") if rest else -1
            if ms < 0:
                raise ValueError(f"wait needs MS: {item!r}")
            steps.append(Wait(ms))
        elif kind == "shot":
            steps.append(Shot(rest or None))
        elif ":" in item:
            raise ValueError(f"unknown step: {item!r}")
        else:
            name, count, gap = _repeat(item)
            steps.append(Press(None, _norm(name), count, gap))
    return steps


def parse_audio_devices(stderr: str) -> list[tuple[int, str]]:
    """The (index, name) of the avfoundation audio devices in ffmpeg's listing."""
    out: list[tuple[int, str]] = []
    in_audio = False
    for line in stderr.splitlines():
        if "AVFoundation audio devices" in line:
            in_audio = True
            continue
        if "AVFoundation video devices" in line:
            in_audio = False
            continue
        m = re.search(r"\[(\d+)\]\s+(.+?)\s*$", line)
        if in_audio and m:
            out.append((int(m.group(1)), m.group(2)))
    return out


def list_audio(ffmpeg: str) -> list[tuple[int, str]]:
    r = subprocess.run(
        [ffmpeg, "-hide_banner", "-f", "avfoundation", "-list_devices", "true"]
        + ["-i", ""],
        capture_output=True,
        text=True,
        check=False,
    )
    return parse_audio_devices(r.stderr)


def _db(v: float) -> float:
    return 20 * math.log10(max(v, 1e-12))


def _spread(values: list[float]) -> float | None:
    vals = [v for v in values if v > 0 and math.isfinite(v)]
    return max(vals) / min(vals) if vals else None


def analyse(
    x: Any, rate: int, events: list[dict[str, Any]] | None = None
) -> dict[str, Any]:
    """Per-hit measurements and a summary; `events` are dicts with kind and t."""
    x = np.atleast_2d(x.T).T if x.ndim == 1 else x
    starts = audiocheck.onsets(x, rate)
    keys = [e for e in (events or []) if e.get("kind") == "key"]
    hits: list[dict[str, Any]] = []
    for i, t in enumerate(starts):
        end = min(
            starts[i + 1] if i + 1 < len(starts) else t + MAX_HIT_S, t + MAX_HIT_S
        )
        seg = x[int(t * rate) : int(end * rate)]
        if len(seg) < 64:
            continue
        et, edb = audiocheck.envelope(seg, rate)
        dur = len(seg) / rate
        if len(edb):
            k = int(np.argmax(edb))
            below = np.flatnonzero(edb[k:] < edb[k] - DECAY_DB)
            if len(below):
                dur = float(et[k + int(below[0])])
        head = seg[: int(F0_WINDOW_S * rate)]
        f0 = math.nan
        if len(head) >= 64:
            freqs, spec = audiocheck.spectrum(head, rate)
            pk = audiocheck.peaks(spec, freqs, n=1)
            if pk:
                f0 = pk[0][0]
        st = audiocheck.segment_stats(x, rate, [t], end - t)[0]
        prior = [e["t"] for e in keys if t - KEY_WINDOW_S <= e["t"] <= t + 0.01]
        key_t = max(prior) if prior else None
        hits.append(
            {
                "t": t,
                "peak_dbfs": _db(float(np.max(np.abs(seg)))),
                "rms_db": st["rms_db"],
                "duration_ms": dur * 1000,
                "f0_hz": f0,
                "centroid_hz": st["centroid_hz"],
                "key_t": key_t,
                "latency_ms": None if key_t is None else (t - key_t) * 1000,
            }
        )
    peaks_db = [h["peak_dbfs"] for h in hits]
    f0_spread = _spread([h["f0_hz"] for h in hits])
    dur_spread = _spread([h["duration_ms"] for h in hits])
    peak_range = (max(peaks_db) - min(peaks_db)) if peaks_db else None
    consistent = (
        len(hits) > 0
        and f0_spread is not None
        and dur_spread is not None
        and peak_range is not None
        and f0_spread <= F0_SPREAD_MAX
        and dur_spread <= DURATION_SPREAD_MAX
        and peak_range <= PEAK_RANGE_DB_MAX
    )
    return {
        "summary": {
            "hits": len(hits),
            "key_presses": len(keys),
            "f0_spread": f0_spread,
            "duration_spread": dur_spread,
            "peak_range_db": peak_range,
            "consistent": consistent,
        },
        "hits": hits,
    }


def _fmt(v: float | None, spec: str) -> str:
    return "-" if v is None or not math.isfinite(v) else format(v, spec)


def format_table(result: dict[str, Any]) -> str:
    lines = ["#   t(s)  lat(ms)  f0(Hz)  dur(ms)  peak(dBFS)  centroid(Hz)"]
    for i, h in enumerate(result["hits"]):
        lines.append(
            f"{i:<3} {_fmt(h['t'], '.3f'):>6} {_fmt(h['latency_ms'], '.0f'):>8} "
            f"{_fmt(h['f0_hz'], '.1f'):>7} {_fmt(h['duration_ms'], '.0f'):>8} "
            f"{_fmt(h['peak_dbfs'], '.1f'):>11} {_fmt(h['centroid_hz'], '.0f'):>13}"
        )
    s = result["summary"]
    lines.append(
        f"hits {s['hits']}, key presses {s['key_presses']}, "
        f"f0 spread {_fmt(s['f0_spread'], '.3f')}, "
        f"duration spread {_fmt(s['duration_spread'], '.3f')}, "
        f"peak range {_fmt(s['peak_range_db'], '.1f')} dB, "
        f"consistent {s['consistent']}"
    )
    return "\n".join(lines)


def analyse_file(wav: Path, out: Path, events_path: Path | None) -> int:
    rate, x = audiocheck.load(wav)
    events: list[dict[str, Any]] | None = None
    if events_path is not None and events_path.exists():
        events = json.loads(events_path.read_text()).get("events")
    result = analyse(x, rate, events)
    out.mkdir(parents=True, exist_ok=True)
    (out / "hits.json").write_text(json.dumps(result, indent=2))
    print(format_table(result))
    print(out / "hits.json")
    return 0


def start_capture(ffmpeg: str, index: int, wav: Path) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "avfoundation"]
        + ["-i", f":{index}", "-ac", "2", "-ar", "48000", "-c:a", "pcm_s24le"]
        + ["-y", str(wav)],
        stdin=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def stop_capture(proc: subprocess.Popen[bytes]) -> None:
    try:
        if proc.stdin:
            proc.stdin.write(b"q")
            proc.stdin.flush()
            proc.stdin.close()
    except OSError:
        pass
    try:
        proc.wait(5)
        return
    except subprocess.TimeoutExpired:
        proc.send_signal(signal.SIGINT)
    try:
        proc.wait(3)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def wait_for_file(wav: Path, proc: subprocess.Popen[bytes]) -> None:
    end = time.monotonic() + 3.0
    while time.monotonic() < end:
        if proc.poll() is not None:
            err = proc.stderr.read().decode(errors="replace") if proc.stderr else ""
            raise RuntimeError(f"ffmpeg stopped: {err.strip()}")
        if wav.exists() and wav.stat().st_size > 44:
            return
        time.sleep(0.05)
    raise RuntimeError("ffmpeg wrote no audio within 3 s")


def run_device(args: argparse.Namespace, out: Path, steps: list[Step]) -> int:
    from tools.device.dkscreen import png_of, rtmidi_screenshot
    from tools.device.dkupload import RtMidiTransport, Uploader, UploadError

    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None:
        raise RuntimeError("ffmpeg not found in PATH")
    keymap = load_key_map(args.device)
    # Resolve key labels before touching the device.
    for s in steps:
        if isinstance(s, Press) and s.code is None and s.label not in keymap:
            raise ValueError(
                f"unknown key {s.label!r}; known: {', '.join(sorted(keymap))}"
            )
    audio = list_audio(ffmpeg)
    want = (args.audio or PORTS[args.device]).lower()
    match = next((d for d in audio if want in d[1].lower()), None)
    if match is None:
        have = ", ".join(f"[{i}] {n}" for i, n in audio) or "none"
        raise RuntimeError(f"no audio device matching {want!r}; found: {have}")
    out.mkdir(parents=True, exist_ok=True)

    t = RtMidiTransport(args.port or PORTS[args.device])
    up = Uploader(t)
    remote_used = False
    proc: subprocess.Popen[bytes] | None = None
    events: list[dict[str, Any]] = []
    try:
        if not args.no_remote:
            try:
                up.remote_state()
            except UploadError as e:
                if e.status == -60:
                    raise RuntimeError(
                        "digikit/remote is not loaded: tick it in DIGIKIT > MODULES"
                    ) from None
                raise
            remote_used = True
        wav = out / "capture.wav"
        t0 = time.monotonic()
        proc = start_capture(ffmpeg, match[0], wav)
        wait_for_file(wav, proc)
        t_file = time.monotonic()
        capture_started = t_file - t0
        time.sleep(0.3)

        def now() -> float:
            return time.monotonic() - t_file

        seq = 0
        if args.no_remote:
            time.sleep(args.tail)
        else:
            for s in steps:
                if isinstance(s, Press):
                    code = s.code if s.code is not None else keymap[s.label]
                    for n in range(s.count):
                        events.append(
                            {"t": now(), "kind": "key", "label": s.label, "code": code}
                        )
                        up.remote_key(code, True)
                        time.sleep(args.hold / 1000)
                        up.remote_key(code, False)
                        if n + 1 < s.count:
                            time.sleep(s.gap_ms / 1000)
                elif isinstance(s, Turn):
                    events.append(
                        {"t": now(), "kind": "turn", "encoder": s.encoder,
                         "delta": s.delta}
                    )  # fmt: skip
                    up.remote_turn(s.encoder, s.delta)
                elif isinstance(s, Wait):
                    time.sleep(s.ms / 1000)
                else:
                    seq += 1
                    name = s.name or f"shot-{seq}"
                    events.append({"t": now(), "kind": "shot", "name": name})
                    png = png_of(rtmidi_screenshot(t.mi, t.mo, seq=seq))
                    (out / f"{name}.png").write_bytes(png)
                    print(out / f"{name}.png", flush=True)
            time.sleep(args.tail)
    finally:
        if proc is not None:
            stop_capture(proc)
        if remote_used:
            try:
                up.remote_release()
            except Exception as e:  # keep closing the port
                print(f"warning: remote release failed: {e}", file=sys.stderr)
        t.close()

    (out / "events.json").write_text(
        json.dumps(
            {
                "device": args.device,
                "port": args.port or PORTS[args.device],
                "audio": match[1],
                "steps": args.steps,
                "capture_started": capture_started,
                "events": events,
            },
            indent=2,
        )
    )
    return analyse_file(out / "capture.wav", out, out / "events.json")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--device", choices=sorted(LEDS), default="dt2")
    ap.add_argument("--port", help="MIDI port substring (default by device)")
    ap.add_argument("--audio", help="audio device name substring (default by device)")
    ap.add_argument("--steps", default="trig1*10@500")
    ap.add_argument("--hold", type=int, default=100, help="key hold in ms")
    ap.add_argument("--tail", type=float, help="seconds of capture after the steps")
    ap.add_argument("--out", type=Path)
    ap.add_argument("--list-audio", action="store_true")
    ap.add_argument("--analyse", type=Path, metavar="WAV")
    ap.add_argument("--no-remote", action="store_true", help="capture only")
    args = ap.parse_args(argv)
    if args.tail is None:
        args.tail = 10.0 if args.no_remote else 2.0
    try:
        if args.list_audio:
            ffmpeg = shutil.which("ffmpeg")
            if ffmpeg is None:
                raise RuntimeError("ffmpeg not found in PATH")
            for i, n in list_audio(ffmpeg):
                print(f"[{i}] {n}")
            return 0
        if args.analyse:
            events = args.analyse.with_name("events.json")
            return analyse_file(args.analyse, args.out or args.analyse.parent, events)
        steps = parse_steps(args.steps)
        out = args.out or (
            REPO / "out" / "runs" / "devprobe" / time.strftime("%Y%m%d-%H%M%S")
        )
        return run_device(args, out, steps)
    except (ValueError, RuntimeError, OSError, TimeoutError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except Exception as e:
        if type(e).__name__ == "UploadError":
            print(f"error: {e}", file=sys.stderr)
            return 1
        raise


if __name__ == "__main__":
    sys.exit(main())
