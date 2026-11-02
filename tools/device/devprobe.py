#!/usr/bin/env python3
r"""Closed-loop hardware debugging: press keys on a real Digitakt II / Digitone II through the digikit remote module, record its USB audio with PortAudio (sounddevice) and analyse each hit (pitch, length, level).

    uv run --with python-rtmidi --with sounddevice python -m tools.device.devprobe --list-audio
    uv run --with python-rtmidi --with sounddevice python -m tools.device.devprobe --steps 'trig1*10@500'
    uv run --with python-rtmidi --with sounddevice python -m tools.device.devprobe --device dn2 --steps 'turn:B:-3,wait:250,trig1*4@400,shot:end'
    uv run --with python-rtmidi --with sounddevice python -m tools.device.devprobe --no-remote --tail 10      # capture only, play by hand
    uv run python -m tools.device.devprobe --analyse capture.wav                           # any WAV, no device

Steps (comma separated): `LABEL[*COUNT][@MS]` presses a key by label (trig1, play, stop, ...), `key:CODE[*COUNT][@MS]`
a key by code, `turn:ENC:DELTA` (ENC A..H, LEVEL or 1..9), `wait:MS`, `shot[:NAME]` a screenshot.
Results go to --out (default out/runs/devprobe/<time>): capture.wav, events.json, hits.json, screenshots.
Event times are seconds since the capture stream started; hit times are seconds in the WAV.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import struct
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
RATE = 48000

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


def input_devices() -> list[tuple[int, str, int, float]]:
    """(index, name, input channels, default rate) of the PortAudio inputs."""
    import sounddevice  # lazy: only capture needs it

    return [
        (
            i,
            str(d["name"]),
            int(d["max_input_channels"]),
            float(d["default_samplerate"]),
        )
        for i, d in enumerate(sounddevice.query_devices())
        if d["max_input_channels"] > 0
    ]


def format_input(dev: tuple[int, str, int, float]) -> str:
    i, name, ch, rate = dev
    return f"{i} {name} ({ch} ch, {rate:.0f} Hz)"


def pick_input(
    devices: list[tuple[int, str, int, float]], want: str
) -> tuple[int, str, int, float]:
    """The first input whose name contains `want` (case-insensitive)."""
    match = next((d for d in devices if want.lower() in d[1].lower()), None)
    if match is None:
        have = "; ".join(format_input(d) for d in devices) or "none"
        raise RuntimeError(f"no audio input matching {want!r}; found: {have}")
    return match


def write_wav_f32(path: Path, data: Any, rate: int) -> None:
    """Write (frames, channels) float samples as a 32-bit float WAV."""
    x = np.asarray(data, dtype="<f4")
    x = x.reshape(len(x), -1)
    ch = x.shape[1]
    payload = x.tobytes()
    header = b"RIFF" + struct.pack("<I", 36 + len(payload)) + b"WAVE"
    fmt = struct.pack("<IHHIIHH", 16, 3, ch, rate, rate * ch * 4, ch * 4, 32)
    header += b"fmt " + fmt
    header += b"data" + struct.pack("<I", len(payload))
    path.write_bytes(header + payload)


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


def run_device(args: argparse.Namespace, out: Path, steps: list[Step]) -> int:
    import sounddevice

    from tools.device.dkscreen import png_of, rtmidi_screenshot
    from tools.device.dkupload import RtMidiTransport, Uploader, UploadError

    keymap = load_key_map(args.device)
    # Resolve key labels before touching the device.
    for s in steps:
        if isinstance(s, Press) and s.code is None and s.label not in keymap:
            raise ValueError(
                f"unknown key {s.label!r}; known: {', '.join(sorted(keymap))}"
            )
    match = pick_input(input_devices(), args.audio or PORTS[args.device])
    out.mkdir(parents=True, exist_ok=True)

    t = RtMidiTransport(args.port or PORTS[args.device])
    up = Uploader(t)
    remote_used = False
    stream: Any = None
    chunks: list[Any] = []
    overflows = 0
    t0 = t_stop = time.monotonic()
    events: list[dict[str, Any]] = []

    def cb(indata: Any, frames: int, tinfo: Any, status: Any) -> None:
        nonlocal overflows
        chunks.append(indata.copy())
        if status.input_overflow:
            overflows += 1

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
        stream = sounddevice.InputStream(
            device=match[0],
            channels=2,
            samplerate=RATE,
            dtype="float32",
            callback=cb,
        )
        stream.start()
        t0 = time.monotonic()
        time.sleep(0.3)

        def now() -> float:
            return time.monotonic() - t0

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
        if stream is not None:
            t_stop = time.monotonic()
            stream.stop()
            stream.close()
        if remote_used:
            try:
                up.remote_release()
            except Exception as e:  # keep closing the port
                print(f"warning: remote release failed: {e}", file=sys.stderr)
        t.close()

    data = np.concatenate(chunks) if chunks else np.zeros((0, 2), dtype="float32")
    write_wav_f32(out / "capture.wav", data, RATE)
    captured = len(data) / RATE
    expected = t_stop - t0
    print(
        f"captured {captured:.2f} s, expected {expected:.2f} s, overflows {overflows}"
    )
    if overflows > 0 or abs(captured - expected) > 0.01 * expected:
        print("warning: capture length differs from wall time or overflowed")

    (out / "events.json").write_text(
        json.dumps(
            {
                "device": args.device,
                "port": args.port or PORTS[args.device],
                "audio": match[1],
                "steps": args.steps,
                "captured_s": captured,
                "expected_s": expected,
                "overflows": overflows,
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
            for d in input_devices():
                print(format_input(d))
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
