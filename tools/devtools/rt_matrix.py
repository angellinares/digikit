#!/usr/bin/env python3
"""Real-time (RT) matrix of the emulator daemon: emulated audio seconds per wall second.

    uv run python -m tools.devtools.rt_matrix run [--device dn2] [--cells fmtone:aud,fmdrum:par]
        [--module 'some/module*'] [--setting base --setting 4,2,0] [--binary PATH] [--measure plain|timing] [--out DIR]
    uv run python -m tools.devtools.rt_matrix stall [--cell fmdrum] [--setting 4,2,0] [--seconds 12]
    uv run python -m tools.devtools.rt_matrix gaps --prefix PREFIX [--device dn2] [--top 25]
    uv run python -m tools.devtools.rt_matrix table RESULTS.json

`run` measures one cell at a time on a private daemon (own run directory and control socket under
out/runs/rt-matrix/). A cell is a scenario (machine and route, see SCENARIOS) with an activity
(`aud`: TRIG 1 audition once per second; `par`: the same plus an encoder turn every 50 ms). The
settings of one cell run next to each other. `base` sets no SHARC_FAST_AUTO; any other setting is
`threshold,min_insns,blocks` for SHARC_FAST_AUTO, and SHARC_FAST_LOG=summary is added so that the
engine writes its summary lines when the daemon quits.

Scenarios are data (SCENARIOS, FX_SETUPS, ACTIVITIES). A cell is `scenario[:activity]`, and each scenario names its
devices. Setups come from the existing panel scenarios (tests/scenarios, tests/regression/scenarios): FX cells
(`fmtone-del`, `sample-rev`, `sample-comp`, `sample-trk`, ...) run the FX setup of those scenarios, then
play Trig 1 once per second; `seq16` plays the full pattern of load-seq16 (no Trig 1); `par` adds encoder turns;
`pages` also moves through the SRC, FLTR, AMP, FX and MOD pages, one per second. A cell must prove what it claims
(`judge`): non-silent PCM, matching inputs, FX state changed in the firmware (the sound and FX blocks that
DEVICES `fxpeek` names), sequencer LED states moving for a pattern, several distinct screens for `pages`. A cell that
fails is INVALID: the table gives the reasons and no RT, and the numbers move to `unproven` in the result.
`run --profile PREFIX` captures a SHARC profile per cell (docs/INSTRUMENTATION.md) at PREFIX-<cell>, for AOT
training and `gaps`, and lists them in <out>/profiles.json.

Method: the machine is switched in stepped (paused) mode and checked from firmware state (the type
byte of track 1 that the audio side reads). Then 3 s settle and 20 s emulated audio run in steps of one
emulated second. The inputs are scheduled in emulated time, so every setting of a cell gets the same
inputs; the recorded input count is compared with the schedule. RT is the change of
`source_audio_seconds` over the change of `session_wall_seconds` of `audio.report`, as the workbench
computes it. The result of a cell has RT over the whole window and over the last 5 s, five sub-window
RTs, the tier shares of active work (fast tier, generated blocks, interpreter), the idle share, the
fast-tier summary counts and the SHA-256 of the PCM of the window. A setting's PCM hash is compared
with the first setting of the cell.

`--measure timing` holds an exclusive benchlock around each cell, so no other benchmark or build
runs at the same time. A cell stops early after 300 s of wall time and is marked partial.

Routes: `stock` starts the daemon on the stock image and the ready inputs of the device. `runtime`
starts it through tools.patchkit.pk_devreload (the patched runtime, with the module store of the
scenario) and attaches to its run directory. The run is refused when a daemon already answers in
that run directory, because it belongs to someone else.

`stall` runs a cell in `run.mode free` and reads `audio.report` back to back while the first notes
compile, to find the longest time in which no audio was produced.

`gaps` reads one SHARC_PROFILE_PREFIX capture (docs/INSTRUMENTATION.md) and prints the interpreted work by
function, joined to the sharcdb of the device: whether the function entry lies in a generated block
range, its static reach status and its loops. This is the view that `fast_roots rank` hides.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import hashlib
import json
import math
import os
import pathlib
import re
import sqlite3
import subprocess
import sys
import threading
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent

from tools import python_env  # noqa: E402
from tools.control.digi_client import Emu, EmuError, oled_png  # noqa: E402
from tools.devtools import benchlock  # noqa: E402
from tools.emulator import digi_env  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402

MS = H.MS  # ColdFire instructions per emulated millisecond
CF_IPS = 132_000_000  # ColdFire instructions per emulated second
TRIG1 = H.button("TRIG 1")
CELL_CAP_S = 300.0  # wall seconds before a cell is cut short and marked partial

# Per device: firmware, ready inputs and the addresses of the audio-side copy of track 1's machine type
# (valid after a TRIG). Only the Digitone II 1.12 addresses are known.
DEVICES: dict[str, dict[str, Any]] = {
    "dn2": {
        "syx": "firmware/Digitone_II_OS1.12.syx",
        "ready": "out/ready-fix-dn2-112",
        "mirror": 0x8000487C,
        "tx_type": 0x80005E60 + 0x94,
        "db": "out/sharcdb/dn2-1.12.sqlite",
        "trig": TRIG1,
        # (pointer address, offset, bytes) of the sound and the FX block the audio side reads
        "fxpeek": {"snd": (0x800052A0, 0x48, 202), "fx": (0x800052A4, 0, 202)},
    },
    # DT2 1.17 on the one-sample card of tools/emulator/dt2_sample_check.py: a TRIG plays the card's
    # sample. No machine switch, so there is no mirror address; the audition is checked by PCM and tiers.
    "dt2": {
        "syx": "firmware/Digitakt_II_OS1.17.syx",
        "trig": 25,
        "card": True,
        "db": "out/sharcdb/dt2-1.17.sqlite",
        "fxpeek": {"snd": (0x8000470C, 0x14, 142), "fx": (0x80004708, 0, 142)},
    },
}

# Scenarios as data. `machine`: ("menu", downs from FM TONE in the FUNC+SRC list, type id the audio side must
# show) or ("dynamic", row of the first loaded machine, None: any id outside the stock ids 0..3).
STOCK_MACHINES = {
    "fmtone": ("menu", 0, 0),
    "fmdrum": ("menu", 1, 2),
    "wavetone": ("menu", 2, 1),
    "swarmer": ("menu", 3, 3),
}
SETUP_DIR = "tests/regression/scenarios"
# Panel setups as data: (kind, file, scenario name). `json`: a scenario of tests/scenarios; `ts`: a scenario
# exported by a module of SETUP_DIR (run with node). The setup events of an FX scenario are its panel events
# without the Trig 1 note at the end: this tool plays Trig 1 itself, once per second, after the setup.
FX_SETUPS: dict[str, dict[str, tuple[str, str, str]]] = {
    "del": {
        "dn2": ("ts", "dn2-sendfx.ts", "sf-del-ref"),
        "dt2": ("ts", "dt2-sendfx.ts", "sf-del-ref"),
    },
    "rev": {
        "dn2": ("ts", "dn2-sendfx.ts", "sf-rev-ref"),
        "dt2": ("ts", "dt2-sendfx.ts", "sf-rev-ref"),
    },
    "comp": {
        "dn2": ("ts", "dn2-sendfx.ts", "sf-mst-thr0"),
        "dt2": ("ts", "dt2-sendfx.ts", "sf-mst-thr0"),
    },
    "trk": {"dt2": ("ts", "dt2-trackfx.ts", "tf-over-127")},
}
BOTH = ("dn2", "dt2")
SCENARIOS: dict[str, dict[str, Any]] = {
    "sample": {"route": "stock", "machine": ("none", 0, None), "devices": ("dt2",)},
    # All 16 steps of all 16 tracks, PLAY at +676M instructions (tests/scenarios/dn2-load.json load-seq16).
    # The firmware must be seen playing: the LED states move.
    "seq16": {
        "route": "stock",
        "machine": ("none", 0, None),
        "setup": ("json", "tests/scenarios/dn2-load.json", "load-seq16"),
        "pattern": True,
        "devices": BOTH,
    },
    **{
        name: {"route": "stock", "machine": m, "devices": ("dn2",)}
        for name, m in STOCK_MACHINES.items()
    },
    # A stock machine with an FX setup: `fmtone-del` is FM Tone with the delay send up.
    **{
        f"{name}-{fx}": {
            "route": "stock",
            "machine": STOCK_MACHINES[name],
            "setup": FX_SETUPS[fx]["dn2"],
            "fx": fx,
            "devices": ("dn2",),
        }
        for name in ("fmtone", "fmdrum")
        for fx in ("del", "rev", "comp")
    },
    **{
        f"sample-{fx}": {
            "route": "stock",
            "machine": ("none", 0, None),
            "setup": FX_SETUPS[fx]["dt2"],
            "fx": fx,
            "devices": ("dt2",),
        }
        for fx in ("del", "rev", "comp", "trk")
    },
    **{
        f"rt-{name}": {
            "route": "runtime",
            "target": "emulator",
            "store": [],
            "machine": m,
            "devices": ("dn2",),
        }
        for name, m in STOCK_MACHINES.items()
    },
}
# name -> kind of panel activity during the audition
ACTIVITIES = {"aud": "", "par": "par", "pages": "pages"}
# Pages visited by `pages` (button codes, one page per second): SRC/SYN, FLTR, AMP, FX, MOD.
PAGE_BUTTONS = (2, 3, 4, 5, 6)
BASE = "base"


_SETUPS: dict[tuple[str, str, str], list[dict[str, Any]]] = {}


def load_setup(kind: str, file: str, name: str) -> list[dict[str, Any]]:
    """Panel events of one scenario (see FX_SETUPS)."""
    key = (kind, file, name)
    if key not in _SETUPS:
        if kind == "json":
            found = json.loads((ROOT / file).read_text())["scenarios"]
        else:
            code = (
                "const m = await import(process.env.SETUP_FILE);"
                "process.stdout.write(JSON.stringify(m.scenarios));"
            )
            found = json.loads(
                subprocess.run(
                    ["node", "--input-type=module", "-e", code],
                    cwd=ROOT / SETUP_DIR,
                    env={**os.environ, "SETUP_FILE": f"./{file}"},
                    capture_output=True,
                    text=True,
                    check=True,
                ).stdout
            )
        _SETUPS[key] = next(x for x in found if x["name"] == name)["events"]
    return _SETUPS[key]


def log(msg: str, logfile: pathlib.Path | None = None) -> None:
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(line, flush=True)
    if logfile is not None:
        with logfile.open("a") as f:
            f.write(line + "\n")


PROFILE: str | None = (
    None  # --profile PREFIX: SHARC_PROFILE_PREFIX=PREFIX-<cell> for every cell
)
TIER_TIME = False  # --tier-time: SHARC_TIER_TIME=1 for every cell (host time per tier, per thread)


def setting_env(setting: str) -> dict[str, str]:
    env = {"SHARC_TIER_TIME": "1"} if TIER_TIME else {}
    if setting == BASE:
        return env
    return {**env, "SHARC_FAST_AUTO": setting, "SHARC_FAST_LOG": "summary"}


def host_note() -> str:
    """Load average and the emulator processes that are not ours (read only)."""
    procs = subprocess.run(
        ["ps", "-axo", "pid=,%cpu=,command="], capture_output=True, text=True
    ).stdout
    others = []
    for line in procs.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[2].split(" ")[0].endswith("/digi-emud"):
            others.append(f"{parts[0]}({parts[1]}%cpu)")
    return f"load1={os.getloadavg()[0]:.2f} emulators: {' '.join(others) or 'none'}"


def binary_for(device: str, given: str | None) -> pathlib.Path:
    """--binary, else DIGI_EMUD_<DEVICE> (the Nx `digi-control:daemons` store). Nothing is built here."""
    path = given or os.environ.get(f"DIGI_EMUD_{device.upper()}")
    if not path or not pathlib.Path(path).is_file():
        raise SystemExit(
            f"rt_matrix: no daemon binary: pass --binary or set DIGI_EMUD_{device.upper()} "
            "(`pnpm nx:pilot run digi-control:daemons` fills the store; "
            "`uv run python -m tools.devtools.verify_build daemons --env-out FILE` prints the paths)"
        )
    return pathlib.Path(path)


class Cell:
    """One scenario, one activity, one setting, on one daemon."""

    def __init__(
        self,
        device: str,
        scenario: str,
        activity: str,
        setting: str,
        binary: pathlib.Path,
        out: pathlib.Path,
        audio_s: float,
        settle_s: float,
        logfile: pathlib.Path,
    ) -> None:
        self.device, self.scenario, self.activity = device, scenario, activity
        self.setting, self.binary = setting, binary
        self.audio_s, self.settle_s = audio_s, settle_s
        self.spec = DEVICES[device]
        self.trig = self.spec["trig"]
        self.sc = SCENARIOS[scenario]
        self.name = f"{scenario}-{activity}-{setting.replace(',', '_')}"
        self.dir = out / self.name
        self.logfile = logfile
        self.res: dict[str, Any] = {
            "cell": self.name,
            "scenario": scenario,
            "activity": activity,
            "setting": setting,
            "device": device,
            "binary": str(binary),
        }
        self.emu: Emu
        self.pid = 0
        self.stderr_path = self.dir / "daemon.stderr"
        self.sched_trig = 0
        self.sched_turn = 0
        self.cap_s = CELL_CAP_S

    def say(self, msg: str) -> None:
        log(f"CELL {self.name} {msg}", self.logfile)

    # ----- stepped (paused) helpers: emulated time, as the harness does it
    def run_ms(self, ms: float) -> None:
        n = int(ms * MS)
        while n > 0:
            chunk = min(n, 200_000_000)
            self.emu.step(chunk, timeout_wall_s=300)
            n -= chunk

    def health(self, where: str) -> dict[str, Any]:
        st = self.emu.status()
        cf, audio = st["cf"], st.get("audio") or {}
        if cf.get("error") or cf["phase"] == "fault":
            raise RuntimeError(f"fault at {where}: {cf.get('error')}")
        if audio.get("halted"):
            raise RuntimeError(f"DSP halt at {where}: {audio['halted']}")
        return st

    def tap(self, keys: str | list[str], hold_ms: float = 20) -> None:
        seq = keys if isinstance(keys, list) else [keys]
        held = [H.button(k) for k in seq[:-1]]
        for code in held:
            self.emu.button(code, True)
            self.run_ms(5)
        self.emu.tap(H.button(seq[-1]), hold=int(hold_ms * MS))
        self.run_ms(hold_ms + 5)
        for code in reversed(held):
            self.emu.button(code, False)
            self.run_ms(5)
        self.health(f"tap {keys}")

    def hold(self, key: str | int, ms: float) -> None:
        code = key if isinstance(key, int) else H.button(key)
        self.emu.button(code, True)
        self.run_ms(ms)
        self.emu.button(code, False)
        self.run_ms(5)
        self.health(f"hold {key}")

    def shot(self, tag: str) -> None:
        _, raw = self.emu.frame()
        (self.dir / f"{tag}.png").write_bytes(oled_png(raw))

    def wire_type(self) -> tuple[int, int]:
        mirror = ((self.emu.peek32(self.spec["mirror"]) or 0) >> 24) & 0xFF
        tx = ((self.emu.peek32(self.spec["tx_type"]) or 0) >> 16) & 0xFFFF
        return mirror, tx

    # ----- daemon
    def start(self) -> None:
        env = {**setting_env(self.setting)}
        if PROFILE:
            self.profile_prefix = f"{PROFILE}-{self.name}"
            pathlib.Path(self.profile_prefix).parent.mkdir(parents=True, exist_ok=True)
            env["SHARC_PROFILE_PREFIX"] = self.profile_prefix
        regions = digi_env.fast_regions_for(ROOT / self.spec["syx"])
        roots = digi_env.blocktrans_roots_for(ROOT / self.spec["syx"])
        base_env = {}
        if regions:
            base_env["SHARC_FAST_REGIONS"] = regions
        if roots:
            base_env["SHARC_BLOCKTRANS"] = roots
        for key in [k for k in os.environ if k.startswith("SHARC_")]:
            self.res.setdefault("ignored_env", []).append(key)
        clean = {k: v for k, v in os.environ.items() if not k.startswith("SHARC_")}
        self.dir.mkdir(parents=True, exist_ok=True)
        if self.sc["route"] == "stock":
            syx = ROOT / self.spec["syx"]
            if self.spec.get("card"):
                from tools.emulator import dt2_sample_check

                inputs, card_args = dt2_sample_check.card_inputs()
                syx = inputs["firmware"]
                ready_args = [
                    "--dsp-image",
                    str(inputs["DSP image"]),
                    "--dsp-state",
                    str(inputs["DSP state"]),
                    "--cf-snapshot",
                    str(inputs["CF snapshot"]),
                    *card_args,
                ]
            else:
                ready = ROOT / self.spec["ready"]
                ready_args = [
                    "--dsp-image",
                    str(ready / "digi-audio-dn2-image.bin"),
                    "--dsp-state",
                    str(ready / "digi-audio-m5.snap.dsp"),
                    "--cf-snapshot",
                    str(ready / "digi-audio-m5.snap"),
                ]
            self.emu = Emu.spawn(
                str(syx),
                "--auto-coupled",
                "--audio-profile-syx",
                str(syx),
                *ready_args,
                "--no-audio",
                binary=self.binary,
                run_dir=self.dir,
                env={**clean, **base_env, **env},
                timeout=300,
            )
            proc = self.emu._proc
            assert proc is not None
            self.pid = proc.pid
            self.proc = proc
            self.stderr_path = self.dir / "daemon.stderr"
            return
        # runtime route: pk_devreload starts and keeps the daemon; we attach to its run directory
        from tools.patchkit import pk_devreload

        target, store = self.sc["target"], list(self.sc["store"])
        run_dir = pk_devreload.run_dir_of(self.device, target, store, False, "sharc")
        if pk_devreload.connect(run_dir) is not None:
            raise SystemExit(
                f"rt_matrix: a daemon already answers in {run_dir}; it is not ours, stopping"
            )
        cmd = [sys.executable, "-m", "tools.patchkit.pk_devreload", target]
        cmd += ["--device", self.device, "--keys", "wait:300"]
        if store:
            cmd += ["--store", ",".join(store)]
        done = subprocess.run(
            cmd,
            cwd=ROOT,
            env={
                **python_env(clean),
                **env,
                f"DIGI_EMUD_{self.device.upper()}": str(self.binary),
            },
            capture_output=True,
            text=True,
            check=False,
        )
        (self.dir / "devreload.txt").write_text(done.stdout + done.stderr)
        if done.returncode:
            raise RuntimeError(
                f"pk_devreload failed: {(done.stdout + done.stderr)[-400:]}"
            )
        self.emu = Emu.connect(run_dir=run_dir)
        self.pid = int(json.loads((run_dir / "current.json").read_text())["pid"])
        self.proc = None
        self.stderr_path = run_dir / "daemon.stderr"
        self.run_dir = run_dir

    def stop(self) -> None:
        """Quit our daemon through its socket and wait for the exit of our own PID."""
        try:
            self.emu._sock.settimeout(60)  # noqa: SLF001
            self.emu.quit()
        except (OSError, EmuError) as e:
            self.say(f"quit call: {e!r}")
        proc = getattr(self, "proc", None)
        if proc is not None:
            try:
                self.res["exit_code"] = proc.wait(timeout=90)
            except subprocess.TimeoutExpired:
                self.say(f"PID {self.pid} did not exit in 90 s; killing our own PID")
                proc.kill()
                self.res["exit_code"] = proc.wait(timeout=30)
        else:
            gone = threading.Event()
            for _ in range(90):  # bounded wait for a daemon that is not our child
                alive = subprocess.run(
                    ["ps", "-p", str(self.pid)], capture_output=True, check=False
                )
                if alive.returncode:
                    gone.set()
                    break
                threading.Event().wait(1)
            self.res["exit_code"] = 0 if gone.is_set() else None
        self.say(f"PID {self.pid} exited code {self.res.get('exit_code')}")

    # ----- scenario
    def prepare(self) -> None:
        """Switch track 1 to the scenario's machine (stepped mode) and prove it from firmware state."""
        emu = self.emu
        emu.subscribe()
        if self.sc["route"] == "stock":
            emu.restart(auto_no=True)
            emu.drain_events()
            self.run_ms(1500)
        else:
            self.run_ms(300)
        emu.pcm_start()
        self.health("ready")
        self.shot("00-ready")
        kind, arg, want = self.sc["machine"]
        if kind == "none" and self.sc.get("pattern"):
            self.res["switch_ok"] = True
            return
        if kind == "none":
            self.res["switch_ok"] = True
            self.hold("TRIG 1" if self.device == "dn2" else self.trig, 500)
            self.run_ms(500)
            if ACTIVITIES[self.activity] and not self.sc.get("setup"):
                self.tap("SYN")
                self.run_ms(300)
            return
        self.tap(["FUNC", "SRC"])
        self.run_ms(200)
        if kind == "dynamic":
            for _ in range(16):
                self.tap("UP")
                self.run_ms(40)
        for _ in range(arg):
            self.tap("DOWN")
            self.run_ms(80)
        self.shot("01-menu-cursor")
        self.tap("YES")
        self.run_ms(300)
        self.tap("YES")
        self.run_ms(300)
        self.hold("TRIG 1", 500)
        self.run_ms(500)
        mirror, tx = self.wire_type()
        ok = mirror == want == tx if want is not None else mirror not in (0, 1, 2, 3)
        self.res.update(wire_type_mirror=mirror, wire_type_tx=tx, expected_type=want)
        self.res["switch_ok"] = ok
        self.say(
            f"machine check: expected {want if want is not None else 'dynamic'}, audio side {mirror}, "
            f"TX frame {tx} -> {'OK' if ok else 'MISMATCH'}"
        )
        if not ok:
            raise RuntimeError("machine switch not confirmed")
        if ACTIVITIES[self.activity] and not self.sc.get("setup"):
            self.tap("SYN")
            self.run_ms(300)

    def setup_events(self) -> tuple[list[dict[str, Any]], int]:
        """The panel events of the scenario's setup and the instruction at which the last one happens."""
        spec = self.sc.get("setup")
        if not spec:
            return [], 0
        events = load_setup(*spec)
        if not self.sc.get("pattern"):
            # an FX setup: leave out its Trig 1 note, this tool plays Trig 1
            events = [e for e in events if e.get("button") != TRIG1]
        return events, max(e["at_instr"] for e in events)

    def schedule(self, start: int, end: int) -> None:
        """Inputs in emulated time, the same for every setting. The setup events of the scenario come
        first. Then TRIG 1 goes down every 1000 ms (500 ms hold; none in a pattern). Activity `par` adds an
        encoder turn every 50 ms; `pages` does the same and also moves to the next page every second.
        `at_instr` counts from `elapsed`."""
        events, setup_end = self.setup_events()
        self.sched_turn += sum("turn" in e for e in events)
        events = [{**e, "at_instr": start + e["at_instr"]} for e in events]
        t0 = start + (setup_end + 100 * MS if events else 0)
        if not self.sc.get("pattern"):
            t = t0
            while t + 500 * MS <= end:
                events.append({"at_instr": t, "button": self.trig, "down": True})
                events.append(
                    {"at_instr": t + 500 * MS, "button": self.trig, "down": False}
                )
                self.sched_trig += 1
                t += 1000 * MS
        else:
            self.sched_trig = None
        kind = ACTIVITIES[self.activity]
        if kind:
            t, k = t0 + 20 * MS, 0
            while t < end:
                if kind == "pages" and (t - t0) % (1000 * MS) < 50 * MS:
                    page = PAGE_BUTTONS[((t - t0) // (1000 * MS)) % len(PAGE_BUTTONS)]
                    events.append({"at_instr": t, "button": page, "down": True})
                    events.append(
                        {"at_instr": t + 20 * MS, "button": page, "down": False}
                    )
                sign = 1 if (k // 8) % 2 == 0 else -1
                events.append(
                    {"at_instr": t + 25 * MS, "turn": 1 + k % 8, "detents": 4 * sign}
                )
                self.sched_turn += 1
                t += 50 * MS
                k += 1
        events.sort(key=lambda e: e["at_instr"])
        for i in range(0, len(events), 200):
            self.emu.schedule(events[i : i + 200])

    def recorded_counts(self) -> tuple[int, int]:
        events = self.emu.recorded().get("events", [])
        trig = sum(
            1 for e in events if e.get("button") == self.trig and e.get("down", True)
        )
        turn = sum(1 for e in events if "turn" in e)
        return trig, turn

    def measure(self) -> None:
        emu = self.emu
        pre_trig, pre_turn = self.recorded_counts()
        st0 = self.health("pre")
        icount, elapsed0 = st0["icount"], st0["elapsed"]
        t0 = icount + 100 * MS
        # the settle time covers the setup events of the scenario and one second more
        setup_end = self.setup_events()[1]
        self.settle_s = max(self.settle_s, math.ceil(setup_end / CF_IPS + 0.1) + 1.0)
        total_s = self.settle_s + self.audio_s
        fx0 = self.fx_state()
        self.schedule(elapsed0 + 100 * MS, elapsed0 + 100 * MS + int(total_s * CF_IPS))
        frame_before = emu.frame_hash()
        started = time.monotonic()
        partial = False

        def step_to(sec: float) -> bool:
            nonlocal partial
            target = t0 + int(sec * CF_IPS)
            while True:
                cur = emu.status()["icount"]
                if cur >= target:
                    return True
                left = self.cap_s - (time.monotonic() - started)
                if left <= 0:
                    partial = True
                    return False
                try:
                    emu.step(min(target - cur, CF_IPS), timeout_wall_s=left)
                except (OSError, EmuError) as e:
                    self.say(f"step ended early: {e!r}")
                    partial = True
                    return False

        if self.sc.get("pattern"):
            step_to(self.settle_s - 0.6)
            self.res["led_states_seen"] = self.led_probe()
        step_to(self.settle_s)
        self.res["fx_changed"] = self.fx_changed(fx0, self.fx_state())
        self.shot("02-settled")
        audio = emu.audio()
        if not audio.get("flow_started") or audio["frames"] < 100:
            raise RuntimeError("audio is not flowing after the settle time")
        self.res["display_changed_by_turns"] = frame_before != emu.frame_hash()
        window = self.dir / "window.wav"
        emu.call("pcm.tap", action="capture", path=str(window))
        marks: list[tuple[float, float, dict[str, Any]]] = []
        self.host_marks: list[dict[str, Any]] = []

        def mark() -> None:
            a = emu.audio()
            marks.append(
                (a["source_audio_seconds"], a["session_wall_seconds"], a["dsp_tiers"])
            )
            dt = a["dsp_tiers"] or {}
            tt = dt.get("tier_time") or {}
            lt = a.get("link_timing") or {}
            aw = dt.get("audio_s") or 0.0
            # tier_time values are per audio second of the worker since it started: make them totals
            self.host_marks.append(
                {
                    "cf_cpu_s": emu.status().get("controller_cpu_ns", 0) / 1e9,
                    "worker_audio_s": aw,
                    "worker_cpu_s": dt.get("worker_cpu_s", 0.0),
                    "tt": {k: v * aw for k, v in tt.items() if isinstance(v, float)},
                    "reply_wait_s": lt.get("host_reply_wait_ns", 0) / 1e9,
                    "done_wait_s": lt.get("host_done_wait_ns", 0) / 1e9,
                    "fast_compile_s": (dt.get("fast_compile_ns") or 0) / 1e9,
                }
            )

        mark()
        page_hashes = [emu.frame_hash()]
        for k in range(1, int(self.audio_s) + 1):
            if not step_to(self.settle_s + k):
                break
            mark()
            page_hashes.append(emu.frame_hash())
            m0, m1 = marks[-2], marks[-1]
            self.say(f"t={k}s RT(1s)={(m1[0] - m0[0]) / (m1[1] - m0[1]):.3f}")
        emu.call("pcm.tap", action="capture_stop")
        self.res["partial"] = partial
        self.summarise(marks)
        trig, turn = self.recorded_counts()
        self.res["scheduled"] = {"trig": self.sched_trig, "turn": self.sched_turn}
        self.res["recorded"] = {"trig": trig - pre_trig, "turn": turn - pre_turn}
        self.res["inputs_valid"] = (
            None
            if partial or self.sched_trig is None
            else (trig - pre_trig, turn - pre_turn)
            == (self.sched_trig, self.sched_turn)
        )
        if window.is_file():
            self.res["pcm_sha256"] = hashlib.sha256(window.read_bytes()).hexdigest()
            self.res["pcm_nonzero_share"] = nonzero_share(window)
            self.res["pcm"] = pcm_stats(window)
        self.res["distinct_frames_in_window"] = len(set(page_hashes))
        self.shot("03-end")
        self.health("end")
        self.judge()

    def fx_state(self) -> dict[str, list[int]]:
        """Longwords of the sound and FX blocks the audio side reads (DEVICES `fxpeek`)."""
        out: dict[str, list[int]] = {}
        for key, (ptr, off, size) in self.spec.get("fxpeek", {}).items():
            base = self.emu.peek32(ptr) or 0
            out[key] = [
                self.emu.peek32(base + off + 4 * i) or 0 for i in range((size + 3) // 4)
            ]
        return out

    @staticmethod
    def fx_changed(a: dict[str, list[int]], b: dict[str, list[int]]) -> dict[str, int]:
        return {k: sum(x != y for x, y in zip(a[k], b[k], strict=False)) for k in a}

    def led_probe(self) -> int:
        """Distinct LED states in 0.5 s of emulated time, in steps of 62 ms: a running sequencer moves them."""
        seen = set()
        for _ in range(8):
            self.run_ms(62)
            seen.add(json.dumps(self.emu.leds().get("leds")))
        return len(seen)

    def judge(self) -> None:
        """A cell is valid only when the checks of what it claims pass; reasons go to `invalid`."""
        r, why = self.res, []
        if r.get("partial"):
            why.append("window cut short")
        if r.get("inputs_valid") is False:
            why.append("recorded inputs differ from the schedule")
        if r.get("switch_ok") is False:
            why.append("machine switch not confirmed")
        pcm = r.get("pcm") or {}
        if pcm.get("active_share", 0) < 0.5:
            why.append(f"PCM mostly silent ({pcm})")
        if self.sc.get("fx") and not any(r["fx_changed"].values()):
            why.append("no FX or send parameter changed in the firmware state")
        if self.sc.get("pattern") and r.get("led_states_seen", 0) < 3:
            why.append(
                f"sequencer not seen playing (LED states {r.get('led_states_seen')})"
            )
        if self.activity == "par" and not r.get("display_changed_by_turns"):
            why.append("encoder turns did not change the display")
        if self.activity == "pages" and r["distinct_frames_in_window"] < 3:
            why.append("fewer than 3 distinct screens: pages not visited")
        r["valid"] = not why
        r["invalid"] = why
        if why:  # no number of an unproven cell is reported
            r["unproven"] = {
                k: r.pop(k)
                for k in ("rt_whole", "rt_last5", "tiers", "host", "subs")
                if k in r
            }
        for w in why:
            self.say(f"INVALID: {w}")

    def summarise(self, marks: list[tuple[float, float, dict[str, Any]]]) -> None:
        n = len(marks) - 1
        self.res["audio_s_measured"] = marks[-1][0] - marks[0][0] if n > 0 else 0.0
        if n < 1:
            return

        def rt(i: int, j: int) -> float:
            return (marks[j][0] - marks[i][0]) / (marks[j][1] - marks[i][1])

        def tiers(i: int, j: int) -> dict[str, Any]:
            keys = (
                "fast_tier",
                "aot_block",
                "interpreter",
                "idle_skip",
                "instructions",
            )
            d = {k: marks[j][2][k] - marks[i][2][k] for k in keys}
            d["interp_appended"] = marks[j][2].get("interp_appended", 0) - marks[i][
                2
            ].get("interp_appended", 0)
            audio = marks[j][0] - marks[i][0]
            active = d["fast_tier"] + d["aot_block"] + d["interpreter"]
            return {
                "active_per_audio_s": active / audio,
                "fast_share": d["fast_tier"] / active if active else 0.0,
                "aot_share": d["aot_block"] / active if active else 0.0,
                "interp_share": d["interpreter"] / active if active else 0.0,
                "idle_share": d["idle_skip"] / d["instructions"]
                if d["instructions"]
                else 0.0,
                "raw": d,
            }

        self.res["rt_whole"] = rt(0, n)
        self.res["host"] = self.host_summary(0, n, marks)
        self.res["tiers"] = tiers(0, n)
        if n >= 5:
            self.res["rt_last5"] = rt(n - 5, n)
            bounds = [round(i * n / 5) for i in range(6)]
            self.res["subs"] = [
                {"rt": rt(bounds[i], bounds[i + 1]), **tiers(bounds[i], bounds[i + 1])}
                for i in range(5)
            ]

    def host_summary(
        self, i: int, j: int, marks: list[tuple[float, float, dict[str, Any]]]
    ) -> dict[str, Any]:
        """Host seconds per emulated audio second over marks i..j, by thread and tier."""
        audio = marks[j][0] - marks[i][0]
        wall = marks[j][1] - marks[i][1]
        a, b = self.host_marks[i], self.host_marks[j]
        waudio = b["worker_audio_s"] - a["worker_audio_s"]
        out: dict[str, Any] = {
            "rt": audio / wall,
            "wall_per_audio_s": wall / audio,
            "cf_cpu_per_audio_s": (b["cf_cpu_s"] - a["cf_cpu_s"]) / audio,
            "dsp_cpu_per_audio_s": (b["worker_cpu_s"] - a["worker_cpu_s"]) / waudio
            if waudio
            else None,
            "cf_wait_reply_per_audio_s": (b["reply_wait_s"] - a["reply_wait_s"])
            / audio,
            "cf_wait_done_per_audio_s": (b["done_wait_s"] - a["done_wait_s"]) / audio,
            "fast_compile_per_audio_s": (b["fast_compile_s"] - a["fast_compile_s"])
            / audio,
        }
        if waudio and b["tt"]:
            out["dsp_tier_time_per_audio_s"] = {
                k: (b["tt"][k] - a["tt"].get(k, 0.0)) / waudio for k in b["tt"]
            }
        return out

    def read_summary(self) -> None:
        try:
            text = self.stderr_path.read_text(errors="replace")
        except OSError:
            text = ""
        lines = [
            ln for ln in text.splitlines() if "fast engine" in ln or "fast tier" in ln
        ]
        self.res["fast_summary"] = [ln[:700] for ln in lines]
        self.res.update(parse_summary(lines))
        for ln in lines:
            self.say(f"summary: {ln[:300]}")

    def run(self) -> dict[str, Any]:
        self.say(f"start | {host_note()}")
        self.start()
        self.say(f"daemon PID {self.pid} (child of this process, niced by the caller)")
        try:
            self.prepare()
            self.measure()
            t = self.res.get("tiers")
            if t is None:
                t = self.res["unproven"]["tiers"]
                self.say("cell is INVALID: no RT is reported")
            else:
                self.say(
                    f"RT whole={self.res['rt_whole']:.3f} last5={self.res.get('rt_last5', 0):.3f} "
                    f"partial={self.res['partial']} shares fast/aot/interp="
                    f"{t['fast_share']:.2f}/{t['aot_share']:.2f}/{t['interp_share']:.3f} "
                    f"inputs_valid={self.res['inputs_valid']} "
                    f"pcm={self.res.get('pcm_sha256', '')[:12]}"
                )
        finally:
            self.stop()
        self.read_summary()
        if PROFILE:
            prefix = pathlib.Path(self.profile_prefix)
            files = sorted(f.name for f in prefix.parent.glob(prefix.name + ".e*"))
            self.res["profile"] = {"prefix": str(prefix), "files": files}
            self.say(f"profile {prefix}.e* ({len(files)} files)")
        self.say(f"end | {host_note()}")
        (self.dir / "result.json").write_text(
            json.dumps(self.res, indent=1, default=str)
        )
        return self.res


def pcm_stats(wav: pathlib.Path) -> dict[str, float]:
    """RMS and the share of 100 ms blocks above -80 dBFS RMS of the captured window."""
    import numpy as np

    from tools.patchkit import hook_ab

    x = np.asarray(hook_ab.read_wav(wav), dtype=float)
    x = x.reshape(len(x), -1)
    n = len(x) // 4800
    if not n:
        return {"rms": 0.0, "active_share": 0.0}
    blocks = np.sqrt((x[: n * 4800] ** 2).reshape(n, 4800, -1).mean(axis=(1, 2)))
    return {
        "rms": float(np.sqrt((x**2).mean())),
        "active_share": float((blocks > 1e-4).mean()),
    }


def nonzero_share(wav: pathlib.Path) -> float | None:
    """Share of 480-sample blocks of the window that have a non-zero sample."""
    import numpy as np

    from tools.patchkit import hook_ab

    x = np.asarray(hook_ab.read_wav(wav))
    x = x.reshape(len(x), -1)
    blocks = len(x) // 480
    if not blocks:
        return None
    peak = np.abs(x[: blocks * 480]).reshape(blocks, 480, -1).max(axis=(1, 2))
    return float((peak > 1e-6).mean())


TIER_RE = re.compile(
    r"fast tier \[[^\]]*\]: (\d+) armed(?: \((\d+) skipped for aot\))?, (\d+) built "
    r"\((\d+) ms compile, (\d+) ms build\), (\d+) refused \((\d+) cached\), (\d+) unprofitable"
)
ENGINE_RE = re.compile(
    r"fast engine: (\d+) insns: fast tier (\d+) .*?aot (\d+) .*?interpreter (\d+)"
)
MISMATCH_RE = re.compile(r"code_mismatch (\d+)")


def parse_summary(lines: list[str]) -> dict[str, Any]:
    """Counts from the last `fast tier` and `fast engine` lines (the engine that ran; the first pair of an
    exit is an engine that never ran)."""
    out: dict[str, Any] = {}
    for ln in reversed(lines):
        m = TIER_RE.search(ln)
        if m:
            keys = ("armed", "skipped_for_aot", "built", "compile_ms", "build_ms")
            vals = [int(x) if x is not None else 0 for x in m.groups()]
            out["counts"] = dict(zip(keys, vals[:5], strict=True))
            out["counts"].update(
                refused=vals[5], cached_refusals=vals[6], unprofitable=vals[7]
            )
            break
    for ln in reversed(lines):
        m = MISMATCH_RE.search(ln)
        if m:
            out["code_mismatch"] = int(m.group(1))
            break
    return out


def result_table(results: list[dict[str, Any]]) -> str:
    """One line per cell: RT, ratio to the first setting of the scenario and activity, shares, counts, PCM."""
    first: dict[tuple[str, str], dict[str, Any]] = {}
    rows = [
        "cell                          RT     last5  ratio  fast/aot/interp   "
        "armed/skip/built/refused/ms     pcm==base inputs"
    ]
    for r in results:
        if r.get("valid") is False:
            rows.append(f"{r['cell']:<28}  INVALID {'; '.join(r['invalid'])[:200]}")
            continue
        if "rt_whole" not in r:
            rows.append(f"{r['cell']:<28}  FAILED {r.get('failed', '')}")
            continue
        base = first.setdefault((r["scenario"], r["activity"]), r)
        t, c = r["tiers"], r.get("counts")
        counts = (
            f"{c['armed']}/{c['skipped_for_aot']}/{c['built']}/{c['refused']}/{c['compile_ms']}"
            if c
            else "-"
        )
        same = r.get("pcm_sha256") == base.get("pcm_sha256")
        rows.append(
            f"{r['cell']:<28} {r['rt_whole']:6.3f} {r.get('rt_last5', 0):6.3f} "
            f"{r['rt_whole'] / base['rt_whole']:6.2f}  "
            f"{t['fast_share']:.2f}/{t['aot_share']:.2f}/{t['interp_share']:.3f}  {counts:<30} "
            f"{'yes' if same else 'NO':<9} {r.get('inputs_valid')}"
            f"{' PARTIAL' if r.get('partial') else ''}"
        )
    return "\n".join(rows)


def cmd_run(a: argparse.Namespace) -> int:
    global TIER_TIME, PROFILE
    TIER_TIME = a.tier_time
    PROFILE = a.profile
    binary = binary_for(a.device, a.binary)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = pathlib.Path(a.out) if a.out else ROOT / "out" / "runs" / "rt-matrix" / stamp
    out.mkdir(parents=True, exist_ok=True)
    logfile = pathlib.Path(a.log) if a.log else out / "run.log"
    settings = a.setting or [BASE, "4,2,0"]
    if a.module:
        # One runtime scenario with the loaded module store entry (the first loaded machine).
        SCENARIOS["module"] = {
            "route": "runtime",
            "target": "emulator",
            "store": [a.module],
            "machine": ("dynamic", 5, None),
            "devices": ("dn2",),
        }
    cells = []
    for spec in a.cells.split(","):
        scenario, _, activity = spec.partition(":")
        if scenario not in SCENARIOS or (activity or "aud") not in ACTIVITIES:
            raise SystemExit(f"rt_matrix: unknown cell {spec!r}")
        if a.device not in SCENARIOS[scenario].get("devices", BOTH):
            raise SystemExit(f"rt_matrix: cell {spec!r} is not for {a.device}")
        cells.append((scenario, activity or "aud"))
    log(
        f"RUN binary={binary} measure={a.measure} audio={a.audio}s settle={a.settle}s "
        f"settings={settings} out={out}",
        logfile,
    )
    results: list[dict[str, Any]] = []
    stopped = False
    for scenario, activity in cells:
        for setting in settings:
            if os.getloadavg()[0] > a.max_load:
                log(f"STOP: load1 {os.getloadavg()[0]:.2f} > {a.max_load}", logfile)
                stopped = True
                break
            cell = Cell(
                a.device,
                scenario,
                activity,
                setting,
                binary,
                out,
                a.audio,
                a.settle,
                logfile,
            )
            cell.cap_s = a.cap
            try:
                if a.measure == "timing":
                    with benchlock.hold(
                        "exclusive", f"rt_matrix {cell.name}", timeout=a.lock_timeout
                    ):
                        results.append(cell.run())
                else:
                    results.append(cell.run())
            except benchlock.LockTimeout as e:
                log(f"STOP: exclusive lock expired: {e}", logfile)
                stopped = True
                break
            except (RuntimeError, OSError, EmuError) as e:
                log(f"CELL {cell.name} FAILED: {e!r}", logfile)
                results.append({**cell.res, "failed": repr(e)})
        if stopped:
            break
    (out / "results.json").write_text(json.dumps(results, indent=1, default=str))
    if PROFILE:
        (out / "profiles.json").write_text(
            json.dumps({r["cell"]: r.get("profile") for r in results}, indent=1)
        )
    table = result_table(results)
    (out / "table.txt").write_text(table + "\n")
    log("\n" + table, logfile)
    return 1 if stopped else 0


def cmd_table(a: argparse.Namespace) -> int:
    print(result_table(json.loads(pathlib.Path(a.results).read_text())))
    return 0


def cmd_stall(a: argparse.Namespace) -> int:
    """First notes in `run.mode free`: the longest time without produced audio, from back-to-back reports."""
    binary = binary_for(a.device, a.binary)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = (
        pathlib.Path(a.out)
        if a.out
        else ROOT / "out" / "runs" / "rt-matrix" / f"stall-{stamp}"
    )
    out.mkdir(parents=True, exist_ok=True)
    logfile = pathlib.Path(a.log) if a.log else out / "run.log"
    cell = Cell(a.device, a.cell, "aud", a.setting, binary, out, 0, 0, logfile)
    cell.name = f"stall-{cell.name}"
    cell.dir = out / cell.name
    cell.say(f"start | {host_note()}")
    cell.start()
    cell.say(f"daemon PID {cell.pid}")
    emu = cell.emu
    try:
        cell.prepare()
        # Quiet time first, so the first notes of the run compile from a cold tier.
        st = cell.health("pre")
        emu.schedule(
            [
                {
                    "at_instr": st["elapsed"] + 200 * MS,
                    "button": TRIG1,
                    "down": True,
                },
                {
                    "at_instr": st["elapsed"] + 700 * MS,
                    "button": TRIG1,
                    "down": False,
                },
            ]
        )
        emu.drain_events()
        emu.call("run.mode", mode="free")
        samples: list[tuple[float, float, int]] = []
        begin = time.monotonic()
        while time.monotonic() - begin < a.seconds:
            t = time.monotonic() - begin
            rep = emu.audio()
            samples.append((t, rep["source_audio_seconds"], rep["frames"]))
        emu.call("run.mode", mode="paused")
        status = emu.status()
        events = emu.drain_events()
        final = emu.audio()
    finally:
        cell.stop()
    cell.read_summary()
    # Longest time between two reports whose produced audio differs, and the slowest 100 ms window.
    longest, since = 0.0, samples[0][0]
    last_audio = samples[0][1]
    for t, audio, _ in samples[1:]:
        if audio > last_audio:
            longest = max(longest, t - since)
            since, last_audio = t, audio
    longest = max(longest, samples[-1][0] - since)
    latency = max(b[0] - a_[0] for a_, b in zip(samples, samples[1:], strict=False))
    worst_rt = None
    j = 0
    for i in range(len(samples)):
        while j < len(samples) and samples[j][0] - samples[i][0] < 0.1:
            j += 1
        if j >= len(samples):
            break
        dt = samples[j][0] - samples[i][0]
        rt = (samples[j][1] - samples[i][1]) / dt
        worst_rt = rt if worst_rt is None else min(worst_rt, rt)
    kinds = collections.Counter(e["kind"] for e in events if e["kind"] != "frame")
    result = {
        "reports": len(samples),
        "audio_s_produced": samples[-1][1] - samples[0][1],
        "longest_no_audio_s": longest,
        "max_report_interval_s": latency,
        "slowest_100ms_window_rt": worst_rt,
        "underrun_count": status["heartbeat"].get("underrun_count"),
        "sink_connected": final.get("sink_connected"),
        "audio_mode": final.get("audio_mode"),
        "event_kinds": dict(kinds),
        "counts": cell.res.get("counts"),
    }
    (out / "stall.json").write_text(json.dumps(result, indent=1, default=str))
    cell.say(f"stall result: {json.dumps(result, default=str)}")
    return 0


def parse_code_ranges(text: str) -> list[tuple[int, int]]:
    m = re.search(r"CODE_RANGES[^=]*=(.*)\];", text, re.S)
    if not m:
        return []
    return [
        (int(a, 16), int(a, 16) + int(n))
        for a, n in re.findall(r"\((0x[0-9a-f]+), (\d+), \"", m.group(1))
    ]


def cmd_gaps(a: argparse.Namespace) -> int:
    """Interpreted work by function from one capture, with generated-block status and static reach."""
    from tools.devtools import fast_roots as fr
    from tools.emulator import coupled

    interp, starts = fr.read_capture(a.prefix)
    total = sum(interp.values())
    gen = coupled.generated(a.device)
    text = (gen / "image.rs").read_text()
    ranges = parse_code_ranges(text)
    range_starts = [r[0] for r in ranges]

    def in_blocks(pc: int) -> bool:
        i = bisect.bisect_right(range_starts, pc) - 1
        return i >= 0 and pc < ranges[i][1]

    db = sqlite3.connect(a.db or str(ROOT / DEVICES[a.device]["db"]))
    functions = db.execute(
        "select entry_sw, end_sw, n_insns, name, label from functions order by entry_sw"
    ).fetchall()
    entries = [f[0] for f in functions]
    reach = {r[0] for r in db.execute("select distinct function_sw from reach")}
    unentered = {r[0] for r in db.execute("select function_sw from unentered")}
    loops = collections.Counter(
        r[0] for r in db.execute("select function_sw from loops")
    )

    def function_of(pc: int) -> tuple[Any, ...] | None:
        i = bisect.bisect_right(entries, pc) - 1
        return functions[i] if i >= 0 and pc < functions[i][1] else None

    by_function: dict[int, int] = collections.Counter()
    start_by_function: dict[int, int] = collections.Counter()
    outside = 0
    for pc, n in interp.items():
        f = function_of(pc)
        if f:
            by_function[f[0]] += n
        else:
            outside += n
    for pc, n in starts.items():
        f = function_of(pc)
        if f:
            start_by_function[f[0]] += n
    in_range = sum(n for pc, n in interp.items() if in_blocks(pc))
    print(
        f"capture {a.prefix}: {total} interpreted instructions at {len(interp)} PCs; "
        f"{100 * in_range / total:.1f}% at PCs inside generated block ranges, "
        f"{100 * outside / total:.1f}% outside any known function"
    )
    for label, group in (("reach", reach), ("unentered", unentered)):
        share = sum(n for e, n in by_function.items() if e in group)
        print(
            f"  work in functions with static status {label}: {100 * share / total:.1f}%"
        )
    print("share  entry     insns loops block  reach      starts  insn/start  name")
    for entry, n in by_function.most_common(a.top):
        f = function_of(entry)
        assert f is not None
        status = (
            "unentered" if entry in unentered else "reach" if entry in reach else "-"
        )
        s = start_by_function[entry]
        print(
            f"{100 * n / total:5.2f}% {entry:#9x} {f[2]:6d} {loops[entry]:5d} "
            f"{'yes' if in_blocks(entry) else 'no':<5} {status:<10} {s:6d} "
            f"{n / s if s else 0:10.1f}  {f[3] or ''} {f[4] or ''}"
        )
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp: argparse.ArgumentParser) -> None:
        sp.add_argument("--device", default="dn2", choices=sorted(DEVICES))
        sp.add_argument("--binary", help="digi-emud (default: DIGI_EMUD_<DEVICE>)")
        sp.add_argument(
            "--out", help="output folder (default out/runs/rt-matrix/<time>)"
        )
        sp.add_argument("--log", help="log file (default <out>/run.log)")

    r = sub.add_parser("run", help="measure cells")
    common(r)
    r.add_argument(
        "--cells",
        default="fmtone:aud",
        help="scenario:activity,... (scenarios: " + ", ".join(SCENARIOS) + ")",
    )
    r.add_argument(
        "--module",
        metavar="STORE_PATTERN",
        help="adds the scenario `module` with this module store entry, e.g. 'some/module*'; "
        "select its cells as module:aud",
    )
    r.add_argument(
        "--setting",
        action="append",
        help="base or threshold,min_insns,blocks (repeat; default base and 4,2,0)",
    )
    r.add_argument(
        "--measure",
        choices=("plain", "timing"),
        default="plain",
        help="timing holds an exclusive benchlock per cell",
    )
    r.add_argument(
        "--lock-timeout",
        type=float,
        default=2700.0,
        help="seconds to wait for the exclusive lock",
    )
    r.add_argument(
        "--audio", type=float, default=20.0, help="emulated audio seconds measured"
    )
    r.add_argument(
        "--cap",
        type=float,
        default=CELL_CAP_S,
        help="wall seconds before a cell is cut short and marked partial",
    )
    r.add_argument(
        "--settle", type=float, default=3.0, help="emulated seconds before the window"
    )
    r.add_argument(
        "--max-load",
        type=float,
        default=12.0,
        help="stop after the current cell above this 1-minute load",
    )
    r.add_argument(
        "--tier-time",
        action="store_true",
        help="SHARC_TIER_TIME=1: report host seconds per audio second by thread and DSP tier",
    )
    r.add_argument(
        "--profile",
        metavar="PREFIX",
        help="capture a SHARC profile per cell (SHARC_PROFILE_PREFIX=PREFIX-<cell>, for AOT training and "
        "`gaps`); the files and prefixes are listed in <out>/profiles.json",
    )
    r.set_defaults(func=cmd_run)

    s = sub.add_parser("stall", help="longest gap in produced audio in run.mode free")
    common(s)
    s.add_argument("--cell", default="fmdrum", choices=sorted(SCENARIOS))
    s.add_argument("--setting", default="4,2,0")
    s.add_argument("--seconds", type=float, default=12.0)
    s.set_defaults(func=cmd_stall)

    g = sub.add_parser(
        "gaps", help="interpreted work by function from a profile capture"
    )
    g.add_argument(
        "--prefix", required=True, help="SHARC_PROFILE_PREFIX of the capture"
    )
    g.add_argument("--device", default="dn2", choices=sorted(DEVICES))
    g.add_argument("--db", help="sharcdb file (default from the device)")
    g.add_argument("--top", type=int, default=25)
    g.set_defaults(func=cmd_gaps)

    t = sub.add_parser("table", help="print the table of a results.json")
    t.add_argument("results")
    t.set_defaults(func=cmd_table)

    a = p.parse_args(argv)
    return int(a.func(a))


if __name__ == "__main__":
    raise SystemExit(main())
