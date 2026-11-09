#!/usr/bin/env python3
"""Audition a machine or module: what each voice, each knob and each preset sounds like.

A golden check answers "did this change". It cannot say whether a control moves the sound at all. This
report does, in three sections from one manifest of renders:

    note map   one render per note or voice, so the voices can be heard apart
    sweeps     one parameter from min to max, other parameters at their defaults; a sweep that moves
               nothing a listener could name is flagged DEAD
    presets    the same descriptors per preset and voice; two presets that read the same on every voice
               are flagged as one sound under two names

The measures, the thresholds and the report follow moveforge's scripts/palette.ts and
scripts/lib/descriptors.ts by m-dwyer (3edfa17), written again in Python on numpy.

Rendering stays outside: any renderer (the coupled emulator, a SHARC harness run, recordings from a unit)
writes WAVs and a manifest, and this module reads them.

    uv run python -m tools.emulator.palette MANIFEST.json [--out DIR]

MANIFEST.json:
    {"title": "...", "needs_a_pattern": ["choke"],
     "rows": [{"section": "note", "label": "note 60", "wav": "note-060.wav"},
              {"section": "sweep", "param": "decay", "group": "amp", "label": "decay=0", "wav": "..."},
              {"section": "preset", "preset": "Kit 1", "label": "Kit 1 voice 0", "wav": "..."}]}
WAV paths are relative to the manifest. Writes palette.md and palette.html into DIR (default: beside it).
"""

from __future__ import annotations

import argparse
import html
import json
import math
import os
import pathlib
import sys
from typing import Any

import numpy as np

from tools.emulator.audiocheck import F, load

ONSET = 0.002
FFT_SIZE = 4096


def _mono(x: F) -> F:
    return x.mean(axis=1) if x.ndim == 2 else x


def describe(x: F, rate: int) -> dict[str, Any]:
    """Descriptors of one hit, read from its onset (the first sample above 0.002):
    peak_db, t60_ms (RMS in 5 ms windows to -60 dB of its first 15 ms; None when the render ends first),
    attack_ms (onset to peak), centroid_hz, bright (power above 4 kHz) and flatness (2..14 kHz; 1 is white)
    over the first 250 ms, crest_db, width (side over mid RMS; 0 for mono) and silent."""
    mono = _mono(x)
    hot = np.flatnonzero(np.abs(mono) > ONSET)
    if len(hot) == 0 or not np.isfinite(mono).all():
        return {
            "peak_db": -math.inf,
            "t60_ms": None,
            "attack_ms": 0.0,
            "centroid_hz": 0.0,
            "bright": 0.0,
            "flatness": 0.0,
            "crest_db": 0.0,
            "width": 0.0,
            "silent": True,
        }
    onset = int(hot[0])
    seg = mono[onset:]
    peak_at = int(np.argmax(np.abs(seg)))
    peak = float(abs(seg[peak_at]))
    w = max(
        1, round(0.005 * rate)
    )  # coarse on purpose: a finer window follows the lowest partial's phase
    blocks = len(seg) // w
    env = (
        np.sqrt((seg[: blocks * w].reshape(blocks, w) ** 2).mean(axis=1))
        if blocks
        else np.zeros(0)
    )
    e0 = float(env[:3].max()) if blocks else 0.0
    t60 = None
    if e0 > 0:
        quiet = np.flatnonzero(20 * np.log10(env[3:] / e0 + 1e-12) < -60)
        t60 = float((quiet[0] + 3) * 5) if len(quiet) else None
    n = min(
        round(0.25 * rate), len(seg), FFT_SIZE
    )  # the first 250 ms decide what a hit sounds like
    re = np.zeros(FFT_SIZE)
    re[:n] = seg[:n] * np.hanning(n) if n > 1 else seg[:n]
    p = np.abs(np.fft.rfft(re))[1 : FFT_SIZE // 2] ** 2
    mag = np.sqrt(p)
    hz = np.arange(1, FFT_SIZE // 2) * rate / FFT_SIZE
    band = (hz >= 2000) & (hz < 14000)
    m = mag[band] + 1e-12
    rms_n = min(round(0.25 * rate), len(seg))
    rms = float(np.sqrt(np.mean(seg[:rms_n] ** 2)))
    width = 0.0
    if x.ndim == 2 and x.shape[1] == 2:
        mid, side = (
            mono[onset : onset + rms_n],
            (x[onset : onset + rms_n, 0] - x[onset : onset + rms_n, 1]) / 2,
        )
        mid_p = float((mid**2).sum())
        width = math.sqrt(float((side**2).sum()) / mid_p) if mid_p > 0 else 0.0
    return {
        "peak_db": 20 * math.log10(peak),
        "t60_ms": t60,
        "attack_ms": peak_at / rate * 1000,
        "centroid_hz": float((mag * hz).sum() / mag.sum()) if mag.sum() > 0 else 0.0,
        "bright": float(p[hz >= 4000].sum() / p.sum()) if p.sum() > 0 else 0.0,
        "flatness": float(np.exp(np.log(m).mean()) / m.mean()) if band.any() else 0.0,
        "crest_db": 20 * math.log10(peak / rms) if rms > 0 else 0.0,
        "width": width,
        "silent": False,
    }


def fingerprint(d: dict[str, Any]) -> str:
    """Three words for a table someone skims; the figures stay beside them."""
    if d["silent"]:
        return "silent"
    c, fl, t = d["centroid_hz"], d["flatness"], d["t60_ms"]
    bright = (
        "bright" if c >= 6000 else "mid" if c >= 2500 else "warm" if c >= 800 else "low"
    )
    texture = "noisy" if fl >= 0.45 else "grainy" if fl >= 0.20 else "tonal"
    length = (
        "unending"
        if t is None
        else "long"
        if t >= 1000
        else "medium"
        if t >= 300
        else "short"
        if t >= 90
        else "tight"
    )
    return f"{bright} {texture} {length}"


def spread(values: list[float], ratio: bool) -> float:
    """How far a descriptor travelled: octaves (or doublings) when `ratio`, else max - min."""
    use = [v for v in values if math.isfinite(v) and (not ratio or v > 0)]
    if len(use) < 2:
        return 0.0
    return math.log2(max(use) / min(use)) if ratio else max(use) - min(use)


def travel(ds: list[dict[str, Any]]) -> dict[str, float]:
    return {
        "centroid": spread([d["centroid_hz"] for d in ds], True),
        "length": spread(
            [d["t60_ms"] if d["t60_ms"] is not None else 8000.0 for d in ds], True
        ),
        "flatness": spread([d["flatness"] for d in ds], False),
        "level": spread([d["peak_db"] for d in ds], False),
        "width": spread([d["width"] for d in ds], False),
        "bright": spread([d["bright"] for d in ds], False),
    }


def is_dead(t: dict[str, float]) -> bool:
    """Nothing a listener could name moved: under a third of an octave of centroid, 0.5 doublings of length,
    0.08 of flatness, 1.5 dB of level, 0.05 of width and 0.10 of brightness. Generous on purpose: a false
    DEAD sends someone hunting for a bug that is not there."""
    return (
        t["centroid"] < 0.33
        and t["length"] < 0.5
        and t["flatness"] < 0.08
        and t["level"] < 1.5
        and t["width"] < 0.05
        and t["bright"] < 0.10
    )


def same_voice(a: dict[str, Any], b: dict[str, Any]) -> bool:
    """Two hits a listener would not tell apart; centroid alone is not enough."""
    if a["silent"] or b["silent"]:
        return bool(a["silent"] and b["silent"])
    octaves = (
        abs(math.log2(a["centroid_hz"] / b["centroid_hz"]))
        if a["centroid_hz"] > 0 and b["centroid_hz"] > 0
        else 9
    )
    la, lb = a["t60_ms"] or 8000.0, b["t60_ms"] or 8000.0
    return (
        octaves < 0.33
        and abs(math.log2(la / lb)) < 0.5
        and abs(a["peak_db"] - b["peak_db"]) < 3
        and abs(a["flatness"] - b["flatness"]) < 0.12
        and abs(a["width"] - b["width"]) < 0.15
    )


def bands(
    x: F, rate: int, count: int = 48, lo: float = 60.0, hi: float = 18000.0
) -> list[float]:
    """A log-frequency spectrum of the first 4096 samples from the onset, in dB floored at -60, 0..1."""
    mono = _mono(x)
    hot = np.flatnonzero(np.abs(mono) > ONSET)
    seg = mono[int(hot[0]) if len(hot) else 0 :][:FFT_SIZE]
    re = np.zeros(FFT_SIZE)
    re[: len(seg)] = seg * np.hanning(len(seg)) if len(seg) > 1 else seg
    p = np.abs(np.fft.rfft(re)) ** 2
    hz = np.fft.rfftfreq(FFT_SIZE, 1 / rate)
    out = np.zeros(count)
    keep = (hz >= lo) & (hz <= hi)
    idx = np.minimum(
        count - 1, (np.log2(hz[keep] / lo) / math.log2(hi / lo) * count).astype(int)
    )
    np.add.at(out, idx, p[keep])
    top = max(float(out.max()), 1e-12)
    return [max(0.0, (10 * math.log10(v / top + 1e-12) + 60) / 60) for v in out]


def _fmt_hz(v: float) -> str:
    return "-" if not v > 0 else f"{v / 1000:.1f}k" if v >= 1000 else f"{v:.0f}"


def _fmt_ms(v: float | None) -> str:
    return ">render" if v is None else f"{v / 1000:.2f}s" if v >= 1000 else f"{v:.0f}ms"


def _fmt_db(v: float) -> str:
    return f"{v:.1f}" if math.isfinite(v) else "-inf"


HEADER = f"{'':22} {'peak':>7} {'T60':>8} {'centroid':>8} {'bright':>7} {'flat':>6} {'crest':>6}  reads as"


def _line(label: str, d: dict[str, Any]) -> str:
    return (
        f"{label[:22]:22} {_fmt_db(d['peak_db']):>7} {_fmt_ms(d['t60_ms']):>8} {_fmt_hz(d['centroid_hz']):>8} "
        f"{d['bright'] * 100:>6.0f}% {d['flatness']:>6.2f} {d['crest_db']:>6.0f}  {fingerprint(d)}"
    )


def build(manifest: dict[str, Any], base: pathlib.Path) -> dict[str, Any]:
    """Read every row's WAV and describe it; group the sweeps and presets."""
    rows = []
    for r in manifest["rows"]:
        rate, x = load(base / r["wav"])
        rows.append(r | {"d": describe(x, rate), "bands": bands(x, rate)})
    sweeps: dict[str, list[dict[str, Any]]] = {}
    presets: dict[str, list[dict[str, Any]]] = {}
    for r in rows:
        if r["section"] == "sweep":
            sweeps.setdefault(r["param"], []).append(r)
        elif r["section"] == "preset":
            presets.setdefault(r["preset"], []).append(r)
    pattern = set(manifest.get("needs_a_pattern", []))
    verdicts = {
        k: (
            "needs a pattern"
            if k in pattern
            else "DEAD"
            if is_dead(travel([r["d"] for r in v]))
            else ""
        )
        for k, v in sweeps.items()
    }
    names = list(presets)
    dupes = [
        f"{a} = {b}"
        for i, a in enumerate(names)
        for b in names[i + 1 :]
        if len(presets[a]) == len(presets[b])
        and all(
            same_voice(p["d"], q["d"])
            for p, q in zip(presets[a], presets[b], strict=True)
        )
    ]
    return {
        "title": manifest.get("title", "palette"),
        "notes": [r for r in rows if r["section"] == "note"],
        "sweeps": sweeps,
        "verdicts": verdicts,
        "presets": presets,
        "dupes": dupes,
    }


def markdown(p: dict[str, Any]) -> str:
    out = [
        f"# {p['title']}: sonic palette",
        "",
        "peak dBFS · T60 to -60 dB · centroid Hz · bright = power above 4 kHz ·",
        "flat = spectral flatness 2-14 kHz (1 white, 0 a few tones) · crest = peak/RMS dB",
        "",
    ]
    if p["notes"]:
        out += (
            ["## Note map", "", "```", HEADER]
            + [_line(r["label"], r["d"]) for r in p["notes"]]
            + ["```", ""]
        )
    if p["sweeps"]:
        dead = [k for k, v in p["verdicts"].items() if v == "DEAD"]
        out += [
            "## Knob travel",
            "",
            "Centroid and length in octaves and doublings, flatness absolute, level in dB.",
            "",
        ]
        if dead:
            out += [
                f"**{len(dead)} of {len(p['sweeps'])} controls move nothing audible:** "
                + ", ".join(f"`{k}`" for k in dead),
                "",
            ]
        out += [
            "```",
            f"{'':22} {'centroid':>9} {'length':>8} {'flat':>7} {'level':>7} {'width':>7} {'bright':>7}",
        ]
        for k, v in p["sweeps"].items():
            t = travel([r["d"] for r in v])
            flag = f"  {p['verdicts'][k]}" if p["verdicts"][k] else ""
            out.append(
                f"{k[:22]:22} {t['centroid']:>5.2f} oct {t['length']:>7.2f}x {t['flatness']:>7.2f} "
                f"{t['level']:>4.1f} dB {t['width']:>7.2f} {t['bright']:>7.2f}{flag}"
            )
        out += ["```", ""]
    if p["presets"]:
        out += ["## Presets", "", "```", HEADER]
        out += [_line(r["label"], r["d"]) for v in p["presets"].values() for r in v] + [
            "```",
            "",
        ]
        if p["dupes"]:
            out += [
                f"**{len(p['dupes'])} preset pair(s) read as the same sound:** "
                + ", ".join(p["dupes"]),
                "",
            ]
    return "\n".join(out)


def _svg(r: dict[str, Any]) -> str:
    w, h, b = 180, 34, r["bands"]
    bw = w / len(b)
    hue = (
        0
        if r["d"]["silent"]
        else round(200 - min(1.0, r["d"]["centroid_hz"] / 12000) * 160)
    )
    bars = "".join(
        f'<rect x="{i * bw:.1f}" y="{h - max(0.5, v * h):.1f}" width="{bw * 0.9:.2f}" '
        f'height="{max(0.5, v * h):.1f}"/>'
        for i, v in enumerate(b)
    )
    return f'<svg viewBox="0 0 {w} {h}" width="{w}" height="{h}" fill="hsl({hue} 65% 55%)">{bars}</svg>'


def _table(rows: list[dict[str, Any]], href: Any) -> str:
    e = html.escape
    body = "".join(
        f'<tr><td class="l">{e(r["label"])}</td><td>{_svg(r)}</td><td class="n">{_fmt_db(r["d"]["peak_db"])}</td>'
        f'<td class="n">{_fmt_ms(r["d"]["t60_ms"])}</td><td class="n">{_fmt_hz(r["d"]["centroid_hz"])}</td>'
        f'<td class="n">{r["d"]["bright"] * 100:.0f}%</td><td class="n">{r["d"]["flatness"]:.2f}</td>'
        f'<td class="f">{fingerprint(r["d"])}</td>'
        f'<td><audio preload="none" controls src="{e(href(r["wav"]))}"></audio></td></tr>'
        for r in rows
    )
    return (
        "<table><thead><tr><th></th><th>spectrum</th><th>peak</th><th>T60</th><th>centroid</th><th>bright</th>"
        f"<th>flat</th><th>reads as</th><th>listen</th></tr></thead><tbody>{body}</tbody></table>"
    )


def page(p: dict[str, Any], href: Any) -> str:
    """palette.html: one self-contained page; each row carries its audio and an inline SVG of its spectrum."""
    e = html.escape
    parts = [f"<h1>{e(p['title'])}: sonic palette</h1>"]
    if p["notes"]:
        parts += ["<h2>Note map</h2>", _table(p["notes"], href)]
    if p["sweeps"]:
        parts.append("<h2>Knob travel</h2>")
        for k, v in p["sweeps"].items():
            t = travel([r["d"] for r in v])
            verdict = p["verdicts"][k]
            parts.append(
                f"<details{' class=dead' if verdict == 'DEAD' else ''}><summary><b>{e(k)}</b> "
                f'<span class="k">centroid {t["centroid"]:.2f} oct · length {t["length"]:.2f}x · '
                f"flat {t['flatness']:.2f} · level {t['level']:.1f} dB · width {t['width']:.2f}"
                f"{' · <b>' + e(verdict) + '</b>' if verdict else ''}</span></summary>"
                f"{_table(v, href)}</details>"
            )
    if p["presets"]:
        parts.append("<h2>Presets</h2>")
        if p["dupes"]:
            parts.append(
                f'<p class="warn">Read as the same sound: {e(", ".join(p["dupes"]))}</p>'
            )
        for k, v in p["presets"].items():
            parts.append(
                f"<details><summary><b>{e(k)}</b></summary>{_table(v, href)}</details>"
            )
    style = (
        "body{font:13px/1.45 ui-monospace,Menlo,monospace;margin:24px;max-width:1200px;background:#14150f;"
        "color:#eee}h2{border-bottom:1px solid #3a3d31}.k{color:#a6a696}.warn,details.dead summary{color:#e6b422}"
        "table{border-collapse:collapse;width:100%}td,th{padding:2px 6px;border-top:1px solid #26281f}"
        "th{color:#a6a696;text-align:left}td.n{text-align:right}td.f{color:#9fbf7f}audio{height:26px;width:180px}"
    )
    return (
        f'<!doctype html><meta charset="utf-8"><title>{e(p["title"])} palette</title>'
        f"<style>{style}</style>" + "\n".join(parts)
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("manifest", type=pathlib.Path)
    ap.add_argument("--out", type=pathlib.Path)
    a = ap.parse_args(argv)
    base = a.manifest.resolve().parent
    out = (a.out or base).resolve()
    out.mkdir(parents=True, exist_ok=True)
    p = build(json.loads(a.manifest.read_text()), base)
    (out / "palette.md").write_text(markdown(p) + "\n")
    rel = pathlib.Path(os.path.relpath(base, out))
    (out / "palette.html").write_text(page(p, lambda wav: (rel / wav).as_posix()))
    dead = sum(v == "DEAD" for v in p["verdicts"].values())
    print(
        f"{len(p['notes'])} notes, {len(p['sweeps'])} sweeps ({dead} DEAD), {len(p['presets'])} presets, "
        f"{len(p['dupes'])} duplicate pair(s): {out / 'palette.html'}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
