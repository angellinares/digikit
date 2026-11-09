#!/usr/bin/env python3
"""Generate tests/scenarios/dn2-soak.json: DN2 scenarios for the DSP DDR page map (`ddrcanary`).

    uv run python -m tools.emulator.dn2_soak [--out tests/scenarios/dn2-soak.json]

The project scenarios reuse the panel scripts of finding 25 (`tools/coldfire/panel_script.py`). The others
build a four-track pattern, p-locks, an LFO, FX sends and menu browsing with plain button and encoder events.
Events are in ColdFire instructions after the ready point; no scenario has a PCM pin.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from tools.coldfire import panel_script as P
from tools.coldfire.panel_script import Script

NS = 132_000_000  # ColdFire instructions per audio second (cf_ips of the report)
PAGE_TRIG, PAGE_SRC, PAGE_FILTER, PAGE_AMP, PAGE_FX, PAGE_MOD = 1, 2, 3, 4, 5, 6
TRACK, KEYBOARD, TRIG1 = 16, 18, 25


def _events(s: Script) -> list[dict]:
    out = []
    for _, spec in s.events:
        flag, _, v = spec.partition(" ")
        f = v.split(":")
        if flag == "--event":
            out.append(
                {"at_instr": int(f[0]), "button": int(f[1]), "down": f[2] == "down"}
            )
        elif flag == "--turn":
            out.append({"at_instr": int(f[0]), "turn": int(f[1]), "detents": int(f[2])})
    return out


def _scenario(name: str, desc: str, s: Script, seconds: float | None = None) -> dict:
    return {
        "name": name,
        "description": desc + " Not a pin.",
        "seconds": seconds or round(s.t / NS + 3.0, 1),
        "exact_events": True,
        "events": _events(s),
    }


def _select_track(s: Script, n: int) -> None:
    s.chord(TRACK, TRIG1 + n - 1, gap=30_000_000)


def _record_steps(s: Script, steps: list[int], lock_enc: int, lock_turn: int) -> None:
    """RECORD armed: trig keys toggle steps; one step gets a p-lock (hold, turn); RECORD off."""
    s.tap(P.REC, gap=30_000_000)
    for k in steps:
        s.tap(TRIG1 + k - 1, gap=20_000_000)
    s.down(TRIG1 + steps[0] - 1).wait(20_000_000)
    s.turn(lock_enc, lock_turn).turn(9, 2)
    s.up(TRIG1 + steps[0] - 1).wait(30_000_000)
    s.tap(P.REC, gap=30_000_000)


def _pattern(s: Script, tracks: int) -> None:
    patterns = [[1, 5, 9, 13], [3, 7, 11, 15], [1, 4, 8, 12, 16], [2, 6, 10, 14]]
    for n in range(1, tracks + 1):
        _select_track(s, n)
        _record_steps(s, patterns[n - 1], 3 + n % 3, 9 + 4 * n)


def project(name: str) -> dict:
    s = P.SCRIPTS[name]()
    s.t += 400_000_000
    return _scenario(
        "dn2-soak-" + name.replace("_", "-"), f"Finding 25 panel script {name}.", s
    )


def pattern_60s() -> dict:
    s = Script(t0=1_000_000)
    s.tap(P.NO, gap=30_000_000)
    _pattern(s, 4)
    # LFO on the MOD page and FX sends on the FX page of track 1, then PLAY for 60 s
    _select_track(s, 1)
    s.tap(PAGE_MOD, gap=30_000_000)
    for enc, det in ((1, 8), (2, 12), (3, 6), (5, 20), (6, 30)):
        s.turn(enc, det, gap=20_000_000)
    s.tap(PAGE_FX, gap=30_000_000)
    for enc, det in ((5, 30), (6, 30), (7, 30)):
        s.turn(enc, det, gap=20_000_000)
    s.tap(PAGE_TRIG, gap=30_000_000)
    s.tap(P.PLAY, gap=30_000_000)
    # while it plays: track and page changes and knob turns
    for i in range(12):
        s.wait(4 * NS)
        _select_track(s, 1 + i % 4)
        s.tap(PAGE_SRC + i % 5, gap=30_000_000)
        s.turn(1 + i % 8, 6 if i % 2 else -6, gap=20_000_000)
    s.wait(14 * NS)
    s.tap(P.STOP, gap=30_000_000)
    return _scenario(
        "dn2-soak-pattern-60s",
        "Four tracks of trigs with p-locks, LFO and three FX sends, PLAY, 12 track/page/knob changes during play, 60+ s.",
        s,
        seconds=round(s.t / NS + 1.0, 1),
    )


def menus() -> dict:
    s = Script(t0=1_000_000)
    s.tap(P.NO, gap=30_000_000)
    for page in range(1, 7):
        s.tap(page, gap=30_000_000)
        for enc in range(1, 9):
            s.turn(enc, 5, gap=10_000_000)
            s.turn(enc, -5, gap=10_000_000)
        s.tap(P.RIGHT, gap=20_000_000).tap(P.LEFT, gap=20_000_000)
    s.tap(P.SETTINGS, gap=30_000_000)
    for i in range(8):  # open each SETTINGS entry (not OS UPGRADE or FORMAT) and leave
        if i not in (7,):
            s.tap(P.YES, gap=50_000_000)
            s.tap(P.DOWN, gap=20_000_000).tap(P.UP, gap=20_000_000)
            s.tap(P.NO, gap=30_000_000)
        s.tap(P.DOWN, gap=20_000_000)
    s.tap(P.NO, gap=30_000_000)
    s.tap(KEYBOARD, gap=30_000_000)
    s.tap(P.PLAY, gap=100_000_000).tap(P.STOP, gap=30_000_000)
    return _scenario(
        "dn2-soak-menus",
        "Pages 1 to 6 with all eight encoders turned, SETTINGS entries opened and left, keyboard mode, PLAY and STOP.",
        s,
    )


def fx_combo() -> dict:
    s = Script(t0=1_000_000)
    s.tap(P.NO, gap=30_000_000)
    s.tap(PAGE_FX, gap=30_000_000)
    for enc in (5, 6, 7, 1, 2):  # delay, reverb, chorus sends and two track FX at once
        s.turn(enc, 60, gap=10_000_000)
    s.down(TRIG1).wait(2 * NS)
    s.down(TRIG1 + 4).down(TRIG1 + 8).wait(3 * NS)
    for enc in (8, 3, 4):
        s.turn(enc, 40, gap=10_000_000)
    s.wait(3 * NS)
    s.up(TRIG1).up(TRIG1 + 4).up(TRIG1 + 8).wait(4 * NS)
    return _scenario(
        "dn2-soak-fx-combo",
        "FX page: delay, reverb and chorus sends and two track FX raised, three trigs held, three more knobs, release tail.",
        s,
    )


def build() -> dict:
    return {
        "scenarios": [
            project("save_project"),
            project("load_project"),
            project("new_project"),
            pattern_60s(),
            menus(),
            fx_combo(),
        ]
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/scenarios/dn2-soak.json")
    a = ap.parse_args()
    Path(a.out).write_text(json.dumps(build(), indent=1) + "\n")
    for s in build()["scenarios"]:
        print(s["name"], s["seconds"], len(s["events"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
