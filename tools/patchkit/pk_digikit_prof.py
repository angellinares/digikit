#!/usr/bin/env python3
"""Per-phase wall-time profile of a digikit boot scenario (docs/findings/39-digikit-boot-tests.md, "Where the time goes").

    uv run python -m tools.patchkit.pk_digikit_prof NAME [--image dt2-1.17] [--keep DIR]
    DIGIKIT_PROFILE=1 uv run python -m tools.patchkit.pk_digikit_scen run NAME     (same table at the end of a normal run)

It wraps the runner's and the harness's functions with timers (no change of behaviour): LEAF phases are exclusive (the time goes to the
innermost active phase), SPANS are inclusive (a named step of the scenario). The emulator's speed is the emulated milliseconds run by
`Session.run_ms` against the wall time of those calls (steps that go through `run_until` are counted under `run_until`).
"""

from __future__ import annotations

import argparse
import functools
import sys
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]


class Prof:
    def __init__(self) -> None:
        self.leaf: dict[str, float] = defaultdict(float)
        self.count: dict[str, int] = defaultdict(int)
        self.notes: list[str] = []
        self.spans: list[tuple[str, float, float]] = []
        self.emu_ms: dict[str, float] = defaultdict(float)
        self.emu_wall: dict[str, float] = defaultdict(float)
        self._stack: list[str] = []
        self._t = time.monotonic()
        self.t0 = self._t

    def _switch(self, new: list[str]) -> None:
        now = time.monotonic()
        if self._stack:
            self.leaf[self._stack[-1]] += now - self._t
        self._t = now
        self._stack = new

    def enter(self, name: str) -> None:
        self.count[name] += 1
        self._switch([*self._stack, name])

    def leave(self) -> None:
        self._switch(self._stack[:-1])

    def wrap(self, owner: Any, attr: str, name: str, span: bool = False) -> None:
        fn = getattr(owner, attr)
        prof = self

        @functools.wraps(fn)
        def inner(*a: Any, **k: Any) -> Any:
            s0 = time.monotonic()
            prof.enter(name)
            try:
                return fn(*a, **k)
            finally:
                prof.leave()
                if span:
                    prof.spans.append((name, s0 - prof.t0, time.monotonic() - s0))

        setattr(owner, attr, inner)

    def report(self) -> str:
        total = time.monotonic() - self.t0
        self._switch(self._stack)
        lines = [f"{'phase (exclusive)':34s} {'calls':>5s} {'wall s':>8s} {'%':>5s}"]
        acct = 0.0
        for k, v in sorted(self.leaf.items(), key=lambda kv: -kv[1]):
            lines.append(f"{k:34s} {self.count[k]:5d} {v:8.1f} {100 * v / total:5.1f}")
            acct += v
        lines.append(
            f"{'(unaccounted: python glue)':34s} {'':5s} {total - acct:8.1f} {100 * (total - acct) / total:5.1f}"
        )
        lines.append(f"{'total':34s} {'':5s} {total:8.1f}")
        lines.append("")
        lines.append(f"{'span (inclusive)':34s} {'start s':>8s} {'wall s':>8s}")
        for name, start, dur in self.spans:
            lines.append(f"{name:34s} {start:8.1f} {dur:8.1f}")
        lines.append("")
        lines.append(
            f"{'emulation (Session.run_ms)':34s} {'emu s':>8s} {'wall s':>8s} {'x real':>7s}"
        )
        for k in self.emu_ms:
            w = self.emu_wall[k]
            lines.append(
                f"{k:34s} {self.emu_ms[k] / 1000:8.1f} {w:8.1f} {self.emu_ms[k] / 1000 / max(w, 1e-9):7.2f}"
            )
        return "\n".join(lines + ["", *self.notes])


PROF: Prof | None = None


def install() -> Prof:
    """Wrap the runner, the boot tool and the harness. Idempotent."""
    global PROF
    if PROF is not None:
        return PROF
    from tools.patchkit import pk_digikit_project as P
    from tools.patchkit import pk_digikit_scen as S
    from tools.patchkit import pk_dkrt_boot as B
    from tools.patchkit import pk_dkrt_machine as M
    from tools.patchkit import pk_harness as H

    p = PROF = Prof()
    p.wrap(B.Env, "build_image", "image build+patch")
    p.wrap(B.Env, "pack", "module pack")
    p.wrap(B, "build_card", "card build")
    p.wrap(M.FolderSession, "open", "daemon spawn")
    p.wrap(H.Session, "load", "cold boot / boot-cache load")
    p.wrap(H.Session, "reset", "session reset (restart+settle)")
    p.wrap(B.BootRig, "wait_dsp", "wait DSP load")
    p.wrap(B.BootRig, "restart", "restart (soft reset + cold boot)")
    p.wrap(M.Rig, "note", "note + PCM capture")
    orig_note = M.Rig.note

    def note(self: Any, wav: Any, trig_frame: int) -> Any:
        p.notes.append(
            f"note {self.tag}: frames before {self.frames()}, trig {trig_frame}"
        )
        return orig_note(self, wav, trig_frame)

    M.Rig.note = note  # type: ignore[method-assign]
    p.wrap(M.Rig, "select", "panel: machine select")
    p.wrap(M.Rig, "finish", "rig finish")
    p.wrap(B.BootRig, "watch_summaries", "daemon close+watch")
    p.wrap(M.Rig, "dsp_bytes", "peek DSP")
    for fn, label in (
        ("decode_tables", "decode tables"),
        ("diff_tables", "oracle: table diff"),
    ):
        p.wrap(S, fn, label)
    p.wrap(M, "pcm_check", "oracle: PCM compare")
    for fn in (
        "diff_slot",
        "slot_image",
        "pattern0_changes",
        "slot_sectors",
        "card_after",
    ):
        p.wrap(P, fn, f"oracle: {fn}")
    p.wrap(S.Ctx, "common_checks", "common checks")
    p.wrap(S.Ctx, "done", "collect writes")
    for fn in ("build_project", "save_project", "load_project"):
        p.wrap(P, fn, f"panel: {fn}", span=True)
        # a call through the module's own name resolves at call time, so wrapping the module attribute is enough
    p.wrap(S.Ctx, "rig", "rig setup", span=True)
    for kind in ("run_equiv",):
        p.wrap(S, kind, f"scenario {kind}")

    orig_run_ms = H.Session.run_ms

    @functools.wraps(orig_run_ms)
    def run_ms(self: Any, ms: float) -> None:
        t = time.monotonic()
        p.enter("emulation run_ms")
        try:
            return orig_run_ms(self, ms)
        finally:
            p.leave()
            seg = self.segment or "-"
            p.emu_ms[seg] += ms
            p.emu_wall[seg] += time.monotonic() - t

    H.Session.run_ms = run_ms  # type: ignore[method-assign]
    from tools.control import digi_client

    orig_call = digi_client.Emu.call

    @functools.wraps(orig_call)
    def call(self: Any, method: str, **params: Any) -> dict[str, Any]:
        if method in ("run_until", "step", "pcm.tap", "card.overlay", "firmware.load"):
            p.enter(f"rpc {method}")
            t = time.monotonic()
            try:
                r = orig_call(self, method, **params)
                if method == "firmware.load":
                    keep = {
                        k: r.get(k)
                        for k in (
                            "start_used",
                            "cache_hit",
                            "wall_seconds",
                            "icount",
                            "frames",
                            "dsp_start_seconds",
                        )
                    }
                    p.notes.append(f"firmware.load {time.monotonic() - t:.1f} s {keep}")
                elif method == "run_until":
                    p.notes.append(f"run_until {params} {time.monotonic() - t:.1f} s")
                return r
            finally:
                p.leave()
        return orig_call(self, method, **params)

    digi_client.Emu.call = call  # type: ignore[method-assign]
    orig_step = digi_client.Emu.step

    @functools.wraps(orig_step)
    def step(self: Any, *a: Any, **k: Any) -> Any:
        p.enter("rpc step")
        try:
            return orig_step(self, *a, **k)
        finally:
            p.leave()

    digi_client.Emu.step = step  # type: ignore[method-assign]
    return p


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("name")
    ap.add_argument("--image")
    ap.add_argument("--keep", type=Path)
    a = ap.parse_args(argv)
    from tools.patchkit import pk_digikit_scen as S

    p = install()
    scen = S.selected("full", [a.name])[0]
    image = a.image or scen.images("quick")[0]
    rep = S.run_scenario(scen, image, a.keep, "quick")
    print(p.report())
    print("PASS" if rep["pass"] else "FAIL", scen.name, image, rep["seconds"], "s")
    return 0 if rep["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
