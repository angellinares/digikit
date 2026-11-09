"""Alternating A/B timing of coupled-run binaries (DSP thread CPU per audio second).

Each pair runs A then B (order swapped on odd pairs) under one exclusive
benchlock hold, so nothing else heavy runs in between. The binaries are
`coupled-run` builds of any generated core (copy them away first: a target dir
is rebuilt in place). Default config: the fast tier and the AOT core, as
`tools/emulator/coupled.py scenario` sets it (digi_env.fast_env). Every run checks the
scenario's PCM hash against the file's pin (or against the first run).

    uv run python -m tools.devtools.coupled_ab A=BIN_A B=BIN_B -s trig1-taps -s tf-br-127 \\
        [--pairs 6] [--scenarios tests/regression/scenarios/dn2-trackfx.ts] [--out OUT.json]
    uv run python -m tools.devtools.coupled_ab A=BIN --pairs 5 -s trig1-taps     # one binary: N runs
    uv run python -m tools.devtools.coupled_ab A=BIN B=BIN --patch B=X.patch ...  # one binary, image B patched

The report prints, per scenario and binary, the median DSP CPU per audio s
(IQR), the CF thread CPU, the real-time factor and the PCM hash; with two
binaries also the paired ratio B/A (median, min, max). The machine load average
is recorded at every run (a quiet machine is not required, a flat one helps).
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import math
import os
import pathlib
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.devtools import benchlock  # noqa: E402
from tools.emulator import coupled  # noqa: E402

# Set by --dt2-card: the sample card's ready inputs and its `--card-image` argument.
CARD: tuple[dict[str, pathlib.Path], list[str]] | None = None
# Set by --seconds: cut every run to this many audio seconds.
SECONDS: str | None = None
# Set by --patch LABEL=FILE: a DSP patch file (coupled-run --dsp-patch) for the runs of that label, so one binary can run two images.
PATCHES: dict[str, str] = {}
# Set by --env: extra environment for the runs (interpreter-only: SHARC_AOT=0 ...).
ENV: dict[str, str] = {}


class Invalid(ValueError):
    """Invalid native sample or failed proof."""


def emulation_key(key: str) -> bool:
    return key.startswith(("SHARC_", "DIGI_", "DSP_", "DIGIKIT_")) or key in {
        "OMP_NUM_THREADS",
        "RAYON_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
    }


def write(path: Path, data: dict) -> None:
    partial = path.with_suffix(path.suffix + ".part")
    partial.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    partial.replace(path)


def process(argv: list[str], env: dict[str, str], folder: Path, timeout: float) -> None:
    """Keep logs on failures too; kill only this child's process group on timeout."""
    folder.mkdir(parents=True, exist_ok=True)
    write(
        folder / "command.json",
        {
            "argv": argv,
            "env": {k: v for k, v in env.items() if emulation_key(k)},
            "timeout": timeout,
        },
    )
    with (
        (folder / "stdout.log").open("wb") as stdout,
        (folder / "stderr.log").open("wb") as stderr,
        subprocess.Popen(
            argv,
            cwd=ROOT,
            env=env,
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
        ) as proc,
    ):
        try:
            code = proc.wait(timeout=timeout)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            raise Invalid(f"interrupted or timed out: {folder}") from None
    write(folder / "exit.json", {"exit_code": code})
    if code:
        raise Invalid(f"child exit {code}: {folder}")


def wav_pcm(path: Path) -> tuple[str, float]:
    data = path.read_bytes()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise Invalid(f"invalid WAV: {path}")
    offset, rate, align, pcm, encoding, bits = 12, 0, 0, None, 0, 0
    while offset + 8 <= len(data):
        tag, size = struct.unpack_from("<4sI", data, offset)
        chunk = data[offset + 8 : offset + 8 + size]
        if len(chunk) != size:
            raise Invalid(f"truncated WAV: {path}")
        if tag == b"fmt ":
            encoding, _, rate, _, align, bits = struct.unpack_from("<HHIIHH", chunk)
        elif tag == b"data":
            pcm = chunk
        offset += 8 + size + size % 2
    if not pcm or not rate or not align or len(pcm) % align:
        raise Invalid(f"empty or invalid PCM: {path}")
    if encoding == 3:
        if bits not in (32, 64) or len(pcm) % (bits // 8):
            raise Invalid(f"invalid float PCM: {path}")
        if any(
            not math.isfinite(value)
            for (value,) in struct.iter_unpack("<f" if bits == 32 else "<d", pcm)
        ):
            raise Invalid(f"nonfinite PCM: {path}")
    elif encoding != 1:
        raise Invalid(f"unsupported PCM encoding: {path}")
    return hashlib.sha256(pcm).hexdigest(), len(pcm) / align / rate


def artifact_run(
    binary: str,
    scenario: str,
    scenarios: str,
    *,
    inputs: dict,
    env: dict,
    seconds: float | None,
    folder: Path,
    timeout: float,
    card: str | None = None,
    patch: str | None = None,
) -> dict:
    folder.mkdir(parents=True, exist_ok=True)
    extra = [
        "--scenarios",
        str(coupled.scenario_path(scenarios)),
        "--scenario",
        scenario,
        "--out",
        str(folder / "report.json"),
        "--wav",
        str(folder / "audio.wav"),
        "--dump-dsp",
        str(folder / "dsp.bin"),
    ]
    if seconds is not None:
        extra += ["--seconds", str(seconds)]
    if card:
        extra += ["--card-image", str(card)]
    if patch:
        extra += ["--dsp-patch", patch]
    argv = coupled.run_args(
        binary, {k: Path(v).resolve() for k, v in inputs.items()}, extra
    )
    load = os.getloadavg()[0]
    process(argv, env, folder, timeout)
    report = json.loads((folder / "report.json").read_text())
    sc = next(s for s in report["scenarios"] if s["name"] == scenario)
    gates = sc["gates"]
    if (
        len(report["scenarios"]) != 1
        or gates["ok"] is not True
        or gates["missing_blocks"] != 0
        or gates["halted"] is not None
        or gates["step_error"] is not None
        or gates["pcm_non_finite"] != 0
        or gates["guard_ok"] is not True
    ):
        raise Invalid(f"replay gates failed: {folder}")
    pcm, duration = wav_pcm(folder / "audio.wav")
    if pcm != sc["pcm"]["sha256"]:
        raise Invalid(f"report/full WAV PCM hash mismatch: {folder}")
    state = folder / f"dsp-{scenario}.bin"
    if state.read_bytes()[:4] != b"SHRD":
        raise Invalid(f"not a canonical SHRD state: {state}")
    for value in (
        duration,
        sc["audio_seconds"],
        sc["cpu"]["dsp_thread_per_audio_s"],
        sc["cpu"]["cf_thread_per_audio_s"],
    ):
        if (
            not isinstance(value, (int, float))
            or not math.isfinite(value)
            or value <= 0
        ):
            raise Invalid(f"nonpositive/nonfinite duration or CPU: {folder}")
    if (
        seconds is not None and (duration < seconds - 0.002 or duration > seconds + 0.1)
    ) or abs(sc["audio_seconds"] - duration) > 0.002:
        raise Invalid(f"short or inconsistent audio duration: {folder}")
    icount = sc["dsp_dump"]["dsp_icount"]
    if not isinstance(icount, int) or icount <= 0:
        raise Invalid(f"invalid DSP instruction count: {folder}")
    return {
        "pcm": pcm,
        "state": hashlib.sha256(state.read_bytes()).hexdigest(),
        "icount": icount,
        "dsp_cpu": sc["cpu"]["dsp_thread_per_audio_s"],
        "dsp_cycles": sc["cpu"].get("dsp_thread_cycles_per_audio_s"),
        "dsp_insns": sc["cpu"].get("dsp_thread_insns_per_audio_s"),
        "audio_s": duration,
        "tiers": sc["dsp_instructions"],
        "cf_cpu": sc["cpu"]["cf_thread_per_audio_s"],
        "rtf": sc.get("real_time_factor"),
        "ok": True,
        "exit_code": 0,
        "load": load,
        **{
            k: sc["dsp_instructions"].get(k)
            for k in ("block_entries", "block_traps", "aot_block", "interpreter")
        },
    }


def one_run(
    binary: str,
    scenario: str,
    scenarios: str | None,
    device: str,
    patch: str | None = None,
) -> dict:
    """One validated native sample; the matrix uses the same persistent helper."""
    ins, card_args = CARD or (coupled.inputs(device), [])
    card = card_args[card_args.index("--card-image") + 1] if card_args else None
    scenarios = scenarios or str(ROOT / "tests" / "scenarios" / f"{device}.json")
    with tempfile.TemporaryDirectory() as tmp:
        return artifact_run(
            binary,
            scenario,
            scenarios,
            inputs=ins,
            env=coupled.run_env(ENV, syx=ins["firmware"]),
            seconds=float(SECONDS) if SECONDS else None,
            folder=Path(tmp),
            timeout=600,
            card=card,
            patch=patch,
        )


def iqr(values: list[float]) -> float:
    if len(values) < 4:
        return max(values) - min(values)
    q = statistics.quantiles(values, n=4)
    return q[2] - q[0]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("binaries", nargs="+", metavar="LABEL=BINARY")
    ap.add_argument("-s", "--scenario", action="append", required=True)
    ap.add_argument("--scenarios", help="scenario file (default: the device's)")
    ap.add_argument("--device", default="dn2")
    ap.add_argument(
        "--dt2-card",
        action="store_true",
        help="DT2: run on the one-sample card of tools/emulator/dt2_sample_check.py",
    )
    ap.add_argument("--pairs", type=int, default=6)
    ap.add_argument("--seconds", help="audio seconds per run (default: the scenario's)")
    ap.add_argument(
        "--patch",
        action="append",
        default=[],
        metavar="LABEL=FILE",
        help="a DSP patch file for the runs of LABEL (tools/patchkit/pk_dsp_diff.py writes one from two images)",
    )
    ap.add_argument(
        "--env", action="append", default=[], metavar="K=V", help="run environment"
    )
    ap.add_argument("--out")
    args = ap.parse_args()
    global SECONDS
    SECONDS = args.seconds
    ENV.update(e.partition("=")[::2] for e in args.env)
    for item in args.patch:
        label, _, path = item.partition("=")
        if not pathlib.Path(path).is_file():
            sys.exit("expected LABEL=FILE, got %r" % item)
        PATCHES[label] = str(pathlib.Path(path).resolve())
    if args.dt2_card:
        from tools.emulator import dt2_sample_check

        global CARD
        CARD = dt2_sample_check.card_inputs()
    bins = []
    for item in args.binaries:
        label, _, path = item.partition("=")
        if not path or not pathlib.Path(path).is_file():
            sys.exit("expected LABEL=BINARY, got %r" % item)
        bins.append((label, path))
    runs: dict[tuple[str, str], list[dict]] = {}
    with benchlock.hold("exclusive", "coupled_ab"):
        for scenario in args.scenario:
            for i in range(args.pairs):
                order = bins if i % 2 == 0 else list(reversed(bins))
                for label, path in order:
                    r = one_run(
                        path, scenario, args.scenarios, args.device, PATCHES.get(label)
                    )
                    r["pair"] = i
                    runs.setdefault((scenario, label), []).append(r)
                    print(
                        "%-12s %-3s pair %d  dsp %.3f  pcm %s  load %.1f"
                        % (
                            scenario,
                            label,
                            i,
                            r["dsp_cpu"],
                            (r["pcm"] or "-")[:16],
                            r["load"],
                        ),
                        flush=True,
                    )
    print()
    for scenario in args.scenario:
        for label, _ in bins:
            rs = runs[(scenario, label)]
            d = [r["dsp_cpu"] for r in rs]
            pcms = sorted({(r["pcm"] or "-")[:16] for r in rs})
            ns = [
                r["dsp_cpu"] * r["audio_s"] * 1e9 / r["interpreter"]
                for r in rs
                if r.get("interpreter") and r.get("audio_s")
            ]
            if ns:
                print(
                    "%-12s %-3s ns per DSP instruction  median %.1f  min %.1f  max %.1f"
                    % (scenario, label, statistics.median(ns), min(ns), max(ns))
                )
            print(
                "%-12s %-3s dsp cpu/audio s  median %.3f  IQR %.3f  min %.3f  max %.3f  n=%d  pcm %s"
                % (
                    scenario,
                    label,
                    statistics.median(d),
                    iqr(d),
                    min(d),
                    max(d),
                    len(d),
                    ",".join(pcms),
                )
            )
        if len(bins) == 2:
            a, b = bins[0][0], bins[1][0]
            ra, rb = runs[(scenario, a)], runs[(scenario, b)]
            ratios = [y["dsp_cpu"] / x["dsp_cpu"] for x, y in zip(ra, rb, strict=True)]
            print(
                "%-12s %s/%s paired ratio  median %.3f  min %.3f  max %.3f"
                % (scenario, b, a, statistics.median(ratios), min(ratios), max(ratios))
            )
    if args.out:
        pathlib.Path(args.out).write_text(
            json.dumps(
                {
                    "when": time.strftime("%F %T"),
                    "runs": {"%s/%s" % k: v for k, v in runs.items()},
                },
                indent=1,
            )
        )


if __name__ == "__main__":
    main()
