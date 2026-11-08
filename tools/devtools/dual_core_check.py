"""Prove that one `digi-emud` with both generated cores boots either device coupled.

The daemon starts on the first device (DN2) with the second device's ready inputs passed as
`--also-coupled`. Then it opens the other firmware (`firmware.load replace`, as the app's Open does),
runs it, restarts it and goes back. Each phase must report `status.dsp == "sharc"`, the audio
report's `dsp_core` of that device, advancing DSP frames, no DSP halt and (with `--pcm`) a PCM tap.

    DIGI_EMUD=dist/native-target/dual/release/digi-emud \\
        uv run python -m tools.devtools.dual_core_check [--runtime] [--steps N] [--pcm] [--module <author/name> [--play]]
        uv run python -m tools.devtools.dual_core_check --bench dt2 --binary second=BIN_A --binary first=BIN_B --pairs 3
        uv run python -m tools.devtools.dual_core_check --menu <machine id> --menu <effect id> [--shots DIR]

The binary comes from `cargo build --release -p digi-control --features second-core --bin digi-emud`
with SHARC_GEN_DIR (DN2) and SHARC_GEN_DIR_2 (DT2) set. It stops its daemon on exit.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path
from typing import Any

from tools import python_env
from tools.control.digi_client import ROOT, Emu
from tools.device import dkrun
from tools.device import dkupload as D
from tools.emulator import digi_env
from tools.patchkit import modroots
from tools.patchkit import pk_harness as H

STEP = 40_000_000  # ColdFire instructions per run slice (about 450 DSP frames)
MS = H.MS  # ColdFire instructions per emulated millisecond
DEFAULT_SHOTS = ROOT / "out" / "runs" / "lanes" / "dualcore-shots"


def phase(
    emu: Emu, label: str, steps: int, expect_core: str, pcm: bool
) -> dict[str, Any]:
    """Run STEPS slices and check the DSP session; returns the facts."""
    if pcm:
        emu.pcm_start()
    before = emu.status()["audio"] or {}
    first = int(before.get("frames") or 0)
    for _ in range(steps):
        emu.step(STEP)
    status = emu.status()
    audio = status["audio"] or {}
    facts: dict[str, Any] = {
        "phase": label,
        "dsp": status["dsp"],
        "dsp_core": audio.get("dsp_core"),
        "frames_advanced": int(audio.get("frames") or 0) - first,
        "halted": audio.get("halted"),
        "firmware": (status["firmware"] or {}).get("syx_sha256", "")[:12],
    }
    if pcm and status["dsp"] == "sharc":
        report = emu.pcm_report()
        facts["pcm_peak"] = report.get("peak")
        facts["pcm_rms"] = report.get("rms")
    ok = (
        facts["dsp"] == "sharc"
        and str(facts["dsp_core"]).startswith(expect_core)
        and facts["frames_advanced"] > 0
        and not facts["halted"]
    )
    facts["ok"] = ok
    print(json.dumps(facts), flush=True)
    return facts


def chord(emu: Emu, names: list[str], hold: int = 3_000_000) -> None:
    """Press the buttons together, run, release them."""
    for name in names:
        emu.button(H.button(name), True)
    emu.step(hold)
    for name in reversed(names):
        emu.button(H.button(name), False)
    emu.step(1_000_000)


def run_module(emu: Emu, run_dir: str, module: str) -> dict[str, Any]:
    """`dk run MODULE --emu`: upload, select, restart (the guest's soft reset: a cold boot on this device's core),
    and read the boot's module results from the log."""
    done = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.device.dkrun",
            module,
            "--emu",
            "--run-dir",
            run_dir,
            "--seconds",
            "1",
        ],
        cwd=ROOT,
        env=python_env(),
        capture_output=True,
        text=True,
    )
    out = done.stdout
    facts = {
        "module": module,
        "exit": done.returncode,
        "booted": "the device booted" in out,
        "mod_load": re.search(r"MOD_LOAD\s+index=0 result=1", out) is not None,
        "mod_dsp_ok": re.search(r"MOD_DSP\s+index=0 result=0", out) is not None,
    }
    print(json.dumps(facts), flush=True)
    if done.returncode:
        print(out[-1500:] + done.stderr[-1500:], file=sys.stderr)
    return facts


def module_type(mid: str) -> str:
    """The `type` of a module in its manifest (machine, fx, service): what the check does with it."""
    toml = modroots.module_dir(mid) / "module.toml"
    return tomllib.loads(toml.read_text(encoding="utf-8"))["module"]["type"]


def play_machine(emu: Emu, downs: int = 7) -> dict[str, Any]:
    """The run-time machine on track 1 (FUNC+SRC, DOWN, YES) and TRIG 1 held with a PCM tap: the note must be audible."""
    chord(emu, ["FUNC", "SRC"])
    for _ in range(downs):
        chord(emu, ["DOWN"], 1_000_000)
    for _ in range(2):
        chord(emu, ["YES"])
    emu.pcm_start()
    emu.button(H.button("TRIG 1"), True)
    emu.step(60_000_000)
    emu.button(H.button("TRIG 1"), False)
    emu.step(10_000_000)
    report = emu.pcm_report()
    facts = {"machine_rms": report.get("rms"), "machine_peak": report.get("peak")}
    facts["ok"] = bool((report.get("peak") or 0) > 1e-4)
    print(json.dumps(facts), flush=True)
    return facts


class Panel:
    """Keys and screenshots on the daemon: a tap runs MS emulated milliseconds, a shot saves the OLED as a PNG."""

    def __init__(self, emu: Emu, shots: Path, prefix: str) -> None:
        self.emu, self.shots, self.prefix = emu, shots, prefix
        shots.mkdir(parents=True, exist_ok=True)

    def tap(self, key: str, ms: float = 350) -> None:
        self.emu.tap(H.button(key), hold=20 * MS)
        self.emu.step(int(ms * MS))

    def chord(self, keys: list[str], ms: float = 400) -> None:
        for key in keys[:-1]:
            self.emu.button(H.button(key), True)
            self.emu.step(5 * MS)
        self.emu.tap(H.button(keys[-1]), hold=20 * MS)
        self.emu.step(25 * MS)
        for key in reversed(keys[:-1]):
            self.emu.button(H.button(key), False)
        self.emu.step(int(ms * MS))

    def shot(self, name: str) -> str:
        path = self.shots / f"{self.prefix}-{name}.png"
        path.write_bytes(self.emu.frame_png())
        print(f"shot {path}", flush=True)
        return str(path)


def machine_audio(emu: Emu) -> dict[str, Any]:
    """TRIG 1 held with a PCM tap: the level of the note (the track must already play the machine)."""
    emu.pcm_start()
    emu.button(H.button("TRIG 1"), True)
    emu.step(60_000_000)
    emu.button(H.button("TRIG 1"), False)
    emu.step(10_000_000)
    report = emu.pcm_report()
    return {"rms": report.get("rms"), "peak": report.get("peak")}


def menu_path(
    emu: Emu, device: str, modules: list[str], shots: Path
) -> list[dict[str, Any]]:
    """The on-device path of plan item 11 on the device just opened with the runtime: upload the modules to the store, SETTINGS >
    DIGIKIT > MODULES, check them all (the list is sorted by name), NO and YES on APPLY & RESTART?, wait for the boot, read the
    boot's module results, play a machine on track 1 and look at the SEND FX pages. Screenshots go to SHOTS."""
    panel = Panel(emu, shots, f"{device}-menu")
    kinds = {module_type(m) for m in modules}
    results: list[dict[str, Any]] = []
    emu.subscribe()
    up = D.Uploader(D.EmuTransport(emu))
    for module in modules:
        dkm = dkrun.build_pack(module, device)
        dkrun.upload_if_changed(up, dkrun.module_path(module), dkm.read_bytes())
    stored = [path for path, _, _ in up.ls() if path.startswith("modules/")]
    panel.tap("NO", 400)
    panel.tap("SETTINGS", 600)
    for _ in range(8):  # the DIGIKIT row of the settings list
        panel.tap("DOWN", 200)
    panel.shot("1-settings-digikit-row")
    panel.tap("YES", 700)
    panel.shot("2-digikit")
    for _ in range(3):  # ABOUT, HEALTH, MEMORY, MODULES
        panel.tap("DOWN", 300)
    panel.tap("YES", 900)
    panel.shot("3-modules")
    for i in range(
        len(modules)
    ):  # the list is sorted by name: check each row, DOWN between
        if i:
            panel.tap("DOWN", 300)
        panel.tap("YES", 500)
    panel.shot("4-checked")
    panel.tap("NO", 800)  # changes pending: the list asks APPLY & RESTART?
    panel.shot("5-apply-question")
    emu.drain_events()
    panel.tap("YES", 300)  # save and restart
    dkrun.emu_boot_waiter(emu, timeout_s=300.0)()
    emu.step(150_000_000)
    panel.shot("7-after-restart")
    up = D.Uploader(D.EmuTransport(emu))
    done = {"n": 0}

    def sleep(seconds: float) -> None:
        emu.step(int(seconds * dkrun.EMU_IPS))
        done["n"] += int(seconds * dkrun.EMU_IPS)

    res = dkrun.module_results(up, sleep, 60.0, lambda: done["n"] / dkrun.EMU_IPS)
    loads = [d for d in res if d["kind"] == "MOD_LOAD"]
    dsps = [d for d in res if d["kind"] == "MOD_DSP"]
    facts = {
        "menu": device,
        "stored": stored,
        "mod_load": [d["args"][1] for d in loads],
        "mod_dsp": [d["args"][1] for d in dsps],
    }
    facts["ok"] = (
        len(loads) == len(modules)
        and all(r in (0, 1) for r in facts["mod_load"])
        and len(dsps) >= 1
        and all(r == 0 for r in facts["mod_dsp"])
    )
    print(json.dumps(facts), flush=True)
    results.append(facts)
    if device == "dt2" and "machine" in kinds:
        panel.chord(["FUNC", "SRC"])
        for _ in range(7):
            panel.chord(["DOWN"], 200)
        panel.shot("8-machine-choice")
        for _ in range(2):
            panel.chord(["YES"])
        panel.shot("9-machine-on-track-1")
        audio = machine_audio(emu)
        audio["ok"] = bool((audio["peak"] or 0) > 1e-4)
        print(json.dumps({"machine": audio}), flush=True)
        results.append(audio)
    if "fx" in kinds:
        panel.chord(
            ["FUNC", "FX"], 600
        )  # the SEND FX pages: DEL, REV, CHO, then the effect
        panel.shot("10-send-fx-page-1")
        for _ in range(3):
            panel.tap("DOWN", 400)
        page = panel.shot("11-send-fx-effect-page")
        results.append({"fx_page_shot": page, "ok": True})
    return results


def daemon_args(start: str, ready: dict[str, dict[str, Path]]) -> list[str]:
    """The daemon's command line: starts on START, the other device's ready inputs as `--also-coupled`."""
    other = "dt2" if start == "dn2" else "dn2"
    return [
        str(ready[start]["firmware"]),
        "--auto-coupled",
        "--audio-profile-syx",
        str(ready[start]["firmware"]),
        "--dsp-image",
        str(ready[start]["DSP image"]),
        "--dsp-state",
        str(ready[start]["DSP state"]),
        "--cf-snapshot",
        str(ready[start]["CF snapshot"]),
        "--also-coupled",
        str(ready[other]["DSP image"]),
        str(ready[other]["DSP state"]),
        str(ready[other]["CF snapshot"]),
        str(ready[other]["firmware"]),
        "--no-audio",
    ]


def tiers(emu: Emu) -> dict[str, Any]:
    audio = emu.status()["audio"] or {}
    return {**(audio.get("dsp_tiers") or {}), "frames": audio.get("frames", 0)}


def bench_once(
    binary: str,
    start: str,
    device: str,
    ready: dict[str, dict[str, Path]],
    slices: int,
    keep: str,
) -> dict[str, Any]:
    """DEVICE opened with the runtime patched in (a patched DSP image: the AOT blocks of the changed code are barred) on a daemon of
    BINARY that started on START. The tier counters over SLICES run slices, after three warm-up slices. No fast tier (the lists
    exist for DN2 only, so a DT2 core has none in the launcher either)."""
    with Emu.spawn(
        *daemon_args(start, ready), binary=binary, run_dir=keep, timeout=600.0
    ) as emu:
        emu.step(STEP)
        emu.call(
            "firmware.load",
            syx=str(ready[device]["firmware"]),
            replace=True,
            runtime=True,
        )
        for _ in range(3):
            emu.step(STEP)
        before = tiers(emu)
        t0 = time.monotonic()
        for _ in range(slices):
            emu.step(STEP)
        wall = time.monotonic() - t0
        after = tiers(emu)
        core = (emu.status()["audio"] or {}).get("dsp_core")
    delta = {
        k: after.get(k, 0) - before.get(k, 0)
        for k in (
            "frames",
            "instructions",
            "aot_block",
            "fast_tier",
            "interpreter",
            "idle_skip",
            "worker_cpu_s",
            "block_traps",
            "code_mismatch",
        )
    }
    audio_s = delta["frames"] / 1500
    blocks = delta["aot_block"] + delta["fast_tier"] + delta["interpreter"]
    return {
        "binary": Path(binary).name,
        "start": start,
        "device": device,
        "core": core,
        "audio_s": round(audio_s, 3),
        "dsp_cpu_per_audio_s": round(delta["worker_cpu_s"] / audio_s, 3),
        "wall_per_audio_s": round(wall / audio_s, 3),
        "rt_factor_wall": round(audio_s / wall, 3),
        "aot_block_share": round(delta["aot_block"] / max(1, blocks), 4),
        "interpreter_insns": delta["interpreter"],
        "code_mismatch_calls": delta["code_mismatch"],
        "load": round(os.getloadavg()[0], 2),
    }


def bench(
    device: str,
    binaries: dict[str, str],
    start: str | None,
    pairs: int,
    slices: int,
    keep: str,
) -> int:
    """Alternating runs of each binary (LABEL=PATH; the order rotates on each pair). A DT2 core that is the second core of one binary
    and the first of another makes the comparison of the role. START is the device the daemon starts on (default: the other one);
    pass the same for both. Prints each run and the medians per binary."""
    ready = {d: digi_env.ready_inputs(device=d) for d in ("dn2", "dt2")}
    start = start or ("dt2" if device == "dn2" else "dn2")
    rows: dict[str, list[dict[str, Any]]] = {label: [] for label in binaries}
    labels = list(binaries)
    for pair in range(pairs):
        shift = pair % len(labels)
        for label in labels[shift:] + labels[:shift]:
            row = bench_once(binaries[label], start, device, ready, slices, keep)
            rows[label].append({"label": label, **row})
            print(json.dumps(rows[label][-1]), flush=True)
    for label, mine in rows.items():
        print(
            json.dumps(
                {
                    "label": label,
                    "runs": len(mine),
                    "median_dsp_cpu_per_audio_s": statistics.median(
                        r["dsp_cpu_per_audio_s"] for r in mine
                    ),
                    "median_rt_factor_wall": statistics.median(
                        r["rt_factor_wall"] for r in mine
                    ),
                }
            ),
            flush=True,
        )
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument(
        "--steps", type=int, default=3, help="40M-instruction slices per phase"
    )
    ap.add_argument(
        "--runtime",
        action="store_true",
        help="open each device with the runtime patched in",
    )
    ap.add_argument(
        "--pcm", action="store_true", help="report the PCM level of each phase"
    )
    ap.add_argument("--keep", help="keep the daemon's run directory here")
    ap.add_argument(
        "--module",
        help="after each open, load this run-time module with dk run (implies --runtime), restart, and check the DSP session again",
    )
    ap.add_argument(
        "--play",
        action="store_true",
        help="with a machine --module: choose it on DT2 track 1 and hold a note (the PCM must not be silent)",
    )
    ap.add_argument(
        "--start",
        choices=("dn2", "dt2"),
        default="dn2",
        help="the device the daemon starts on (the other one is opened next)",
    )
    ap.add_argument(
        "--menu",
        action="append",
        metavar="MODULE",
        help="on the other device (the second core when starting on DN2): upload MODULE (repeatable), then the on-device path SETTINGS > DIGIKIT > MODULES > tick > APPLY & RESTART, with screenshots (implies --runtime; a machine also plays a note, an effect: its FX page)",
    )
    ap.add_argument(
        "--shots",
        type=Path,
        default=DEFAULT_SHOTS,
        help="the --menu screenshots directory",
    )
    ap.add_argument(
        "--bench",
        choices=("dn2", "dt2"),
        help="measure this device with the runtime patched in (DSP CPU per audio second, AOT share, real-time factor) on each --binary, alternating --pairs; no fast tier",
    )
    ap.add_argument(
        "--binary",
        action="append",
        metavar="LABEL=PATH",
        help="--bench: a digi-emud to measure (repeatable; default: $DIGI_EMUD). A build whose first core is the device against one whose second core is it shows what the role costs",
    )
    ap.add_argument(
        "--bench-start",
        choices=("dn2", "dt2"),
        help="--bench: the device the daemon starts on (default: the other one)",
    )
    ap.add_argument("--pairs", type=int, default=3)
    ap.add_argument(
        "--slices", type=int, default=6, help="--bench: 40M-instruction slices measured"
    )
    args = ap.parse_args()
    if args.bench:
        binaries = dict(x.split("=", 1) for x in args.binary or []) or {
            "default": os.environ.get("DIGI_EMUD", "")
        }
        return bench(
            args.bench,
            binaries,
            args.bench_start,
            args.pairs,
            args.slices,
            args.keep or tempfile.mkdtemp(prefix="digi-dual-"),
        )
    args.runtime = args.runtime or bool(args.module) or bool(args.menu)
    keep = args.keep or tempfile.mkdtemp(prefix="digi-dual-")

    ready = {d: digi_env.ready_inputs(device=d) for d in ("dn2", "dt2")}
    start = args.start
    other = "dt2" if start == "dn2" else "dn2"
    first = [
        str(ready[start]["firmware"]),
        "--auto-coupled",
        "--audio-profile-syx",
        str(ready[start]["firmware"]),
        "--dsp-image",
        str(ready[start]["DSP image"]),
        "--dsp-state",
        str(ready[start]["DSP state"]),
        "--cf-snapshot",
        str(ready[start]["CF snapshot"]),
        "--also-coupled",
        str(ready[other]["DSP image"]),
        str(ready[other]["DSP state"]),
        str(ready[other]["CF snapshot"]),
        str(ready[other]["firmware"]),
        "--no-audio",
    ]
    results: list[dict[str, Any]] = []
    with Emu.spawn(*first, run_dir=keep, timeout=300.0) as emu:
        results.append(phase(emu, f"{start} at start", args.steps, start, args.pcm))
        for device, came_from in ((other, start), (start, other)):
            inputs = ready[device]
            report = emu.call(
                "firmware.load",
                syx=str(inputs["firmware"]),
                replace=True,
                **({"runtime": True} if args.runtime else {}),
            )
            print(
                json.dumps(
                    {
                        "load": device,
                        "audio": report.get("audio"),
                        "start_used": report.get("start_used"),
                    }
                ),
                flush=True,
            )
            results.append(
                phase(
                    emu,
                    f"{device} after open (from {came_from})",
                    args.steps,
                    device,
                    args.pcm,
                )
            )
            if args.menu and device == other:
                results.extend(menu_path(emu, device, args.menu, args.shots))
                results.append(
                    phase(
                        emu,
                        f"{device} after the menu path",
                        args.steps,
                        device,
                        args.pcm,
                    )
                )
            if args.module:
                loaded = run_module(emu, keep, args.module)
                results.append(
                    {
                        "ok": loaded["exit"] == 0
                        and loaded["booted"]
                        and loaded["mod_dsp_ok"]
                    }
                )
                results.append(
                    phase(
                        emu,
                        f"{device} after {args.module} and restart",
                        args.steps,
                        device,
                        args.pcm,
                    )
                )
                if args.play and device == "dt2":
                    results.append(play_machine(emu))
    ok = all(r["ok"] for r in results)
    print("DUAL CORE OK" if ok else "DUAL CORE FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
