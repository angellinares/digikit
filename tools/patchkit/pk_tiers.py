#!/usr/bin/env python3
"""Attribute DSP cost of patched images by execution tier (AOT / fast tier / interpreter), DT2 1.17 or 1.16.

    uv run python -m tools.patchkit.pk_tiers [--config NAME ...] [--seconds 1.0] [--fw 1.17] [--hist] [--out FILE.json]

One coupled daemon per configuration, a TRIG 1 note held for SECONDS emulated seconds (a window of the dsp_tiers
counters of `status`, so boot and settling are excluded):

  stock         the stock firmware, the sample voice
  voice0        core/dsp-voice, no type registered
  pluck-idle    the PLUCK module set, PLUCK selected on track 1, no note
  pluck-ring    the same, TRIG 1 held at the default DECAY

Per configuration: wall seconds per emulated second, DSP worker CPU per audio second, DSP instructions per audio
second by tier (idle skip, interpreter, fast tier, AOT blocks). `--hist` also sets SHARC_FAST_HIST and
SHARC_FAST_LOG: the daemon prints the interpreted instructions per PC and the model gate counters (code_mismatch)
when it exits, and the tool splits the PCs into the stock image and the appended code (`--split` hex, default the
first L2 address of the patch plan).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.patchkit import pk_harness as H  # noqa: E402

TRIG1 = H.button("TRIG 1")
PLUCK_MODS = ["core/dsp-voice", "core/machines", "core/params", "test/patched-pluck"]
PLUCK_DOWNS = 7

# name -> (modules, DSP_FRAME_POKE, select PLUCK, hold TRIG 1)
CONFIGS: dict[str, tuple[list[str], str, bool, bool]] = {
    "stock": ([], "", False, True),
    "voice0": (["core/dsp-voice"], "", False, True),
    "pluck-idle": (PLUCK_MODS, "", True, False),
    "pluck-ring": (PLUCK_MODS, "", True, True),
}


def tiers(emu) -> dict:
    return (emu.status().get("audio") or {}).get("dsp_tiers") or {}


def select_pluck(s: H.Session) -> None:
    s.do_tap(["FUNC", "SRC"])
    s.run_ms(200)
    for _ in range(PLUCK_DOWNS):
        s.do_tap("DOWN")
        s.run_ms(80)
    s.do_tap("YES")
    s.run_ms(300)
    s.do_tap("YES")
    s.run_ms(300)


def measure(name: str, fw: str, seconds: float, hist: bool, pcm: bool = False) -> dict:
    mods, poke, pluck, hold = CONFIGS[name]
    env: dict[str, str] = {}
    if poke:
        env["DSP_FRAME_POKE"] = poke
    if hist:
        env["SHARC_FAST_HIST"] = "1"
        env["SHARC_FAST_LOG"] = "1"
    cell = H.Cell("dt2", fw, list(mods))
    s = H.Session(cell, H.Goldens(), run_name=f"tiers-{name}", env=env)
    s.open()
    emu = s.emu
    errlog = pathlib.Path(emu._run_dir.name) / "daemon.stderr"  # type: ignore[union-attr]
    s.reset()
    s.run_ms(1500)
    if pluck:
        select_pluck(s)
    wav = pathlib.Path(emu._run_dir.name) / "window.wav"  # type: ignore[union-attr]
    if pcm:
        emu.call("pcm.tap", action="capture", path=str(wav))
    if hold:
        emu.button(TRIG1, True)
        s.run_ms(100)  # the note-on settles
    t0, w0 = tiers(emu), time.monotonic()
    s.run_ms(seconds * 1000)
    t1, w1 = tiers(emu), time.monotonic()
    emu.button(TRIG1, False)
    pcm_hash = None
    if pcm:
        emu.call("pcm.tap", action="capture_stop")
        pcm_hash = hashlib.sha256(wav.read_bytes()).hexdigest()[:16]
    audio = t1["audio_s"] - t0["audio_s"]
    d = {
        k: t1[k] - t0[k]
        for k in (
            "instructions",
            "idle_skip",
            "interpreter",
            "fast_tier",
            "block_instructions",
        )
    }
    d["aot"] = d["block_instructions"] - d["fast_tier"]
    busy = d["instructions"] - d["idle_skip"]
    out = {
        "config": name,
        "pcm": pcm_hash,
        "wall_per_emu_s": (w1 - w0) / seconds,
        "dsp_cpu_per_audio_s": (t1["worker_cpu_s"] - t0["worker_cpu_s"]) / audio
        if audio
        else None,
        "audio_s": audio,
        "insn_per_audio_s": {k: v / audio for k, v in d.items()} if audio else None,
        "tier_share": {k: d[k] / busy for k in ("interpreter", "fast_tier", "aot")}
        if busy
        else None,
        "code": {
            k: t1.get(k, 0) - t0.get(k, 0)
            for k in (
                "code_gen",
                "code_verifies",
                "code_verify_ns",
                "code_mismatch",
                "interp_appended",
            )
        },
        "fast_built": t1["fast_built"],
        "fast_refused": t1["fast_refused"],
        "fast_runs": t1["fast_runs"] - t0["fast_runs"],
        "block_entries": t1["block_entries"] - t0["block_entries"],
    }
    plan = None
    if s.image:
        p = pathlib.Path(str(s.image)[: -len(".syx")] + ".plan.json")
        if p.is_file():
            plan = json.loads(p.read_text())
    # The daemon prints its histogram when it exits; the run directory goes away with the session.
    emu.__exit__(None, None, None)
    s.emu = None
    if hist and errlog.is_file():
        out["hist"] = parse_hist(errlog.read_text(errors="replace"), plan)
    s.close()
    return out


def parse_hist(text: str, plan: dict | None) -> dict:
    """The interpreted instructions per PC of the whole daemon run (one line per PC per engine, summed)."""
    per_pc: dict[int, int] = {}
    for m in re.finditer(r"hist: (0x[0-9a-f]+) (\d+)", text):
        per_pc[int(m.group(1), 16)] = per_pc.get(int(m.group(1), 16), 0) + int(
            m.group(2)
        )
    cm = re.search(r"code_mismatch (\d+)", text)
    ranked = sorted(per_pc.items(), key=lambda kv: -kv[1])
    return {
        "code_mismatch": int(cm.group(1)) if cm else None,
        "interpreted_total": sum(per_pc.values()),
        "top": [(hex(pc), n) for pc, n in ranked[:25]],
        "pcs": {hex(pc): n for pc, n in per_pc.items()},
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--config", action="append", choices=sorted(CONFIGS))
    ap.add_argument("--seconds", type=float, default=1.0)
    ap.add_argument("--fw", default="1.17")
    ap.add_argument("--hist", action="store_true")
    ap.add_argument(
        "--pcm",
        action="store_true",
        help="hash the PCM of the window (bit-identity between binaries)",
    )
    ap.add_argument("--out")
    a = ap.parse_args()
    rows = []
    for name in a.config or list(CONFIGS):
        r = measure(name, a.fw, a.seconds, a.hist, a.pcm)
        print(json.dumps(r), flush=True)
        rows.append(r)
    if a.out:
        pathlib.Path(a.out).write_text(json.dumps(rows, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
