"""Paired counter runs of coupled-run binaries on runtime fixtures: steady-state host
instruction counts, tier counters, PCM and DSP state parity, and fast-tier exit tables.

    uv run python -m tools.devtools.sharc_cells A=BIN_A B=BIN_B --state dn2-reset --state dt2 \\
        [--seconds 3,6] [--jobs 4] [--scenario NAME] [--cov] \\
        [--roots-add B:0x1c4b72] [--roots-remove A:0x1c4b72] [--out DIR] [--json]

A cell is one binary on one state (a directory of out/fixtures/runtime/). Each cell runs
once per length in --seconds; the first label is the reference for parity and ratios.
Steady state per audio second is (long - short) / (audio seconds difference), which
removes the start-up work. --cov adds one short run per cell with SHARC_FAST_COV and
prints the fast-tier root and exit tables from its `cov run` lines.

This tool does not take the bench lock. Host instruction counts
(`cpu.dsp_thread_insns_per_audio_s`) repeat within 0.02% on a busy host, so counter runs
may run in parallel (--jobs, under `nice -n 10`). They say nothing about time: do the final
timing A/B with tools/devtools/coupled_ab.py, which holds the lock.

The exit code is non-zero when a run fails or PCM or DSP state differs from the first
label's at the same length.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import re
import sys
import time
from collections import defaultdict

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.devtools import coupled_ab  # noqa: E402
from tools.emulator import coupled, digi_env  # noqa: E402

FIXTURES = ROOT / "out" / "fixtures" / "runtime"
DEFAULT_SCENARIO = {"dn2": "trig1-hold", "dt2": "dt2-sample-trig"}
# (column, key in the report's dsp_instructions, scale)
# Roots listed in a cov table (the engine also chases many small ones).
TOP_ROOTS = 25
COUNTERS = (
    ("aot", "aot_block"),
    ("fast", "fast_tier"),
    ("interp", "interpreter"),
    ("idle", "idle_skip"),
    ("blk_entries", "block_entries"),
    ("fast_calls", "fast_calls"),
)


def parse_cov(text: str) -> list[dict]:
    """The `cov run` lines of a SHARC_FAST_COV log: one dict per root.

    Exits are (pc, reason, count, kernel insns, gap insns) as written by
    packages/emulator/sharc/src/fast/mod.rs (FastEngine::drop)."""
    roots = []
    for line in text.splitlines():
        if not line.startswith("cov run "):
            continue
        f = dict(re.findall(r"(\w+)=(\S+)", line.split(" declwhy=")[0]))
        exits = []
        for item in f.get("exits", "").split(";"):
            p = item.split(":")
            if len(p) == 5:
                exits.append((int(p[0], 16), p[1], int(p[2]), int(p[3]), int(p[4])))
        # declined=[shape, req, window, budget, exit0]; the list has spaces, so read it apart.
        m = re.search(r"declined=\[([\d, ]*)\]", line)
        names = ("shape", "req", "window", "budget", "exit0")
        declined = [int(x) for x in m[1].split(",")] if m else []
        roots.append(
            {
                "root": int(f["root"], 16),
                "calls": int(f.get("calls", 0)),
                "insns": int(f.get("insns", 0)),
                "declined": dict(zip(names, declined, strict=False)),
                "exits": exits,
            }
        )
    return roots


def is_guard(reason: str) -> bool:
    return reason == "guard" or reason.startswith("g@")


def cov_tables(roots: list[dict], audio_s: float) -> str:
    """Root table, top exits and totals as text, rates per audio second."""
    out = [
        "%-10s %10s %12s %8s  %s"
        % ("root", "calls", "insns", "per call", "declined (nonzero)")
    ]
    allexits = []
    declined: dict[str, int] = defaultdict(int)
    guard: dict[str, int] = defaultdict(int)
    for i, r in enumerate(sorted(roots, key=lambda r: -r["insns"])):
        for k, v in r["declined"].items():
            declined[k] += v
        dec = " ".join(f"{k}={v}" for k, v in r["declined"].items() if v)
        if i < TOP_ROOTS:
            out.append(
                "%#-10x %10d %12d %8.1f  %s"
                % (
                    r["root"],
                    r["calls"],
                    r["insns"],
                    r["insns"] / max(r["calls"], 1),
                    dec,
                )
            )
        for at, why, n, *_ in r["exits"]:
            allexits.append((n, r["root"], at, why, r["calls"]))
            if is_guard(why):
                guard[why] += n
    if len(roots) > TOP_ROOTS:
        out.append(
            f"... {len(roots) - TOP_ROOTS} more roots (all counted in the totals)"
        )
    out += [
        "",
        "top exits   root        exit pc    reason                       per audio s   share of calls",
    ]
    for n, root, at, why, calls in sorted(allexits, reverse=True)[:15]:
        out.append(
            "          %#-10x  %#-10x %-28s %11.0f   %6.2f%%"
            % (root, at, why, n / audio_s, 100 * n / max(calls, 1))
        )
    out.append("")
    out.append(
        "declined by reason: "
        + (", ".join(f"{k}={v}" for k, v in declined.items() if v) or "none")
    )
    out.append(
        "guard exits by reason (per audio s): "
        + (
            ", ".join(f"{k}={v / audio_s:.0f}" for k, v in sorted(guard.items()))
            or "none"
        )
    )
    return "\n".join(out)


def load_state(name: str) -> dict:
    folder = FIXTURES / name
    prov = json.loads((folder / "provenance.json").read_text())
    files = prov["files"]
    return {
        "name": name,
        "device": prov["device"],
        "stock": prov["stock_sha256"],
        "inputs": {
            "firmware": folder / files["firmware"],
            "DSP image": folder / files["dsp_image"],
            "DSP state": folder / files["dsp_state"],
            "CF snapshot": folder / files["cf_snapshot"],
        },
        "card": folder / files["card"] if files.get("card") else None,
    }


def cell_env(state: dict, label: str, add: dict, remove: dict) -> dict[str, str]:
    """A clean environment: HOME and PATH plus the fast-tier lists of the firmware,
    in the engine slot of the device. The table is keyed by the stock firmware."""
    entry = digi_env.fast_table().get(state["stock"], {})
    lists = {
        "SHARC_FAST_REGIONS": entry.get("fast_regions", ""),
        "SHARC_FAST_FALLBACK_REGIONS": entry.get("fallback_regions", ""),
        "SHARC_BLOCKTRANS": entry.get("blocktrans", ""),
    }
    roots = [r for r in lists["SHARC_FAST_REGIONS"].split(",") if r]
    roots += [r for r in add.get(label, []) if r not in roots]
    lists["SHARC_FAST_REGIONS"] = ",".join(
        r for r in roots if r not in remove.get(label, [])
    )
    env = {k: os.environ[k] for k in ("HOME", "PATH") if k in os.environ}
    env.update(
        digi_env.slot_env(state["device"], {k: v for k, v in lists.items() if v})
    )
    return env


def run_cell(
    binary: str,
    state: dict,
    scenario: str,
    seconds: float,
    env: dict,
    folder: pathlib.Path,
) -> dict:
    scen_file = ROOT / "tests" / "scenarios" / f"{state['device']}.json"
    extra = [
        "--scenarios", str(scen_file), "--scenario", scenario,
        "--out", str(folder / "report.json"),
        "--wav", str(folder / "audio.wav"),
        "--dump-dsp", str(folder / "dsp.bin"),
        "--seconds", str(seconds),
    ]  # fmt: skip
    if state["card"]:
        extra += ["--card-image", str(state["card"])]
    argv = ["nice", "-n", "10", *coupled.run_args(binary, state["inputs"], extra)]
    coupled_ab.process(argv, env, folder, 1800)
    sc = json.loads((folder / "report.json").read_text())["scenarios"][0]
    cpu, tiers, audio = sc["cpu"], sc["dsp_instructions"], sc["audio_seconds"]
    dump = pathlib.Path(sc["dsp_dump"]["path"])
    return {
        "audio_s": audio,
        "insns": cpu["dsp_thread_insns_per_audio_s"] * audio,
        "cycles": cpu["dsp_thread_cycles_per_audio_s"] * audio,
        **{col: tiers.get(key, 0) for col, key in COUNTERS},
        "rtf": sc.get("real_time_factor"),
        "pcm": sc["pcm"]["sha256"],
        "state": hashlib.sha256(dump.read_bytes()).hexdigest(),
    }


def steady(short: dict, long: dict) -> dict:
    """Counters per audio second from two lengths (insns and cycles in G, others in M)."""
    d = long["audio_s"] - short["audio_s"]
    out = {}
    for k in ("insns", "cycles", *(c for c, _ in COUNTERS)):
        scale = 1e9 if k in ("insns", "cycles") else 1e6
        out[k] = (long[k] - short[k]) / d / scale
    return out


def state_table(
    state: dict, scenario: str, labels: list[str], lengths: list[float], runs: dict
) -> tuple[str, dict, bool]:
    """The text table of one state, its JSON data and whether parity held."""
    ref = labels[0]
    ok = True
    head = "%-6s %8s %8s %7s %7s %7s %7s %7s %8s" % (
        "label", "insn G", "cyc G", "aot M", "fast M", "interp", "idle M", "blk M", "fcall M",
    )  # fmt: skip
    lines = [
        f"== {state['name']} ({state['device']}, {scenario}), lengths {lengths} s, "
        "steady state per audio s ==",
        head + "   B/A insns   rtf per length   parity (pcm/state per length)",
    ]
    data = {}
    base = None
    for label in labels:
        rs = [runs[(label, n)] for n in lengths]
        s = steady(rs[0], rs[-1])
        base = s["insns"] if base is None else base
        par = []
        for n, r in zip(lengths, rs, strict=True):
            a = runs[(ref, n)]
            same = (r["pcm"] == a["pcm"], r["state"] == a["state"])
            ok &= all(same)
            par.append(f"{n:g}s:" + "/".join("=" if x else "DIFF" for x in same))
        lines.append(
            "%-6s %8.3f %8.3f %7.1f %7.1f %7.2f %7.1f %7.2f %8.2f   %-9.4f  %-15s  %s"
            % (
                label, s["insns"], s["cycles"], s["aot"], s["fast"], s["interp"],
                s["idle"], s["blk_entries"], s["fast_calls"], s["insns"] / base,
                "/".join("%.2f" % (r["rtf"] or 0) for r in rs), " ".join(par),
            )
        )  # fmt: skip
        data[label] = {
            "steady": s,
            "runs": dict(zip(map(str, lengths), rs, strict=True)),
        }
    return "\n".join(lines), data, ok


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("binaries", nargs="+", metavar="LABEL=BINARY")
    ap.add_argument(
        "--state", action="append", required=True, help="dir under out/fixtures/runtime"
    )
    ap.add_argument(
        "--seconds", default="3,6", help="two or more audio lengths (default 3,6)"
    )
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--scenario", help="default: per device (%s)" % DEFAULT_SCENARIO)
    ap.add_argument(
        "--cov", action="store_true", help="one SHARC_FAST_COV run per binary and state"
    )
    ap.add_argument("--roots-add", action="append", default=[], metavar="LABEL:ADDR")
    ap.add_argument("--roots-remove", action="append", default=[], metavar="LABEL:ADDR")
    ap.add_argument("--out", help="default: out/runs/sharc-cells/<timestamp>")
    ap.add_argument("--json", action="store_true", help="also print the JSON data")
    args = ap.parse_args()

    bins: list[tuple[str, str]] = []
    for item in args.binaries:
        label, _, path = item.partition("=")
        if not path or not pathlib.Path(path).is_file():
            sys.exit(f"expected LABEL=BINARY, got {item!r}")
        bins.append((label, str(pathlib.Path(path).resolve())))
    labels = [label for label, _ in bins]
    lengths = sorted(float(x) for x in args.seconds.split(","))
    if len(lengths) < 2:
        sys.exit("--seconds needs two lengths")
    add: dict[str, list[str]] = defaultdict(list)
    remove: dict[str, list[str]] = defaultdict(list)
    for opt, dest in ((args.roots_add, add), (args.roots_remove, remove)):
        for item in opt:
            label, _, addr = item.partition(":")
            if label not in labels or not addr:
                sys.exit(f"expected LABEL:ADDR with a known label, got {item!r}")
            dest[label].append(addr)
    states = [load_state(n) for n in args.state]
    out = (
        pathlib.Path(args.out)
        if args.out
        else ROOT / "out" / "runs" / "sharc-cells" / time.strftime("%Y%m%d-%H%M%S")
    )

    jobs: list[tuple] = []  # (key, binary, state, scenario, seconds, folder)
    for st in states:
        scen = args.scenario or DEFAULT_SCENARIO[st["device"]]
        for label, path in bins:
            for n in lengths:
                jobs.append(
                    (
                        (st["name"], label, n),
                        path,
                        st,
                        scen,
                        n,
                        out / st["name"] / label / f"{n:g}s",
                    )
                )
            if args.cov:
                jobs.append(
                    (
                        (st["name"], label, "cov"),
                        path,
                        st,
                        scen,
                        lengths[0],
                        out / st["name"] / label / "cov",
                    )
                )
    print(
        f"plan: {len(jobs)} runs, {len(bins)} binaries x {len(states)} states, lengths {lengths}, jobs {args.jobs}"
    )
    out.mkdir(parents=True, exist_ok=True)
    print(f"output: {out}", flush=True)

    results: dict = {}
    failed = False

    def work(job):
        key, path, st, scen, n, folder = job
        env = cell_env(st, key[1], add, remove)
        if key[2] == "cov":
            env |= {"SHARC_FAST_COV": "1", "SHARC_FAST_LOG": "summary"}
        t0 = time.time()
        try:
            return key, run_cell(path, st, scen, n, env, folder), time.time() - t0
        except Exception as e:  # keep the other runs going
            return key, e, time.time() - t0

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        done = 0
        futures = [pool.submit(work, j) for j in jobs]
        for fut in concurrent.futures.as_completed(futures):
            key, res, secs = fut.result()
            done += 1
            if isinstance(res, Exception):
                failed = True
                print(
                    f"[{done}/{len(jobs)}] {key[0]} {key[1]} {key[2]}: FAILED {res} ({secs:.0f}s wall)",
                    flush=True,
                )
                continue
            results[key] = res
            print(
                f"[{done}/{len(jobs)}] {key[0]} {key[1]} {key[2]}: insns/s {res['insns'] / res['audio_s'] / 1e9:.3f}G rtf {res['rtf'] or 0:.2f} ({secs:.0f}s wall)",
                flush=True,
            )

    ok = not failed
    report: dict = {}
    for st in states:
        scen = args.scenario or DEFAULT_SCENARIO[st["device"]]
        if any((st["name"], lb, n) not in results for lb in labels for n in lengths):
            print(f"\n{st['name']}: incomplete, no table")
            continue
        runs = {(lb, n): results[(st["name"], lb, n)] for lb in labels for n in lengths}
        text, data, good = state_table(st, scen, labels, lengths, runs)
        ok &= good
        print("\n" + text)
        report[st["name"]] = data
        if args.cov:
            for lb in labels:
                r = results.get((st["name"], lb, "cov"))
                log = out / st["name"] / lb / "cov" / "stderr.log"
                if r is None or not log.is_file():
                    continue
                print(f"\n-- cov {st['name']} {lb} ({r['audio_s']:.2f} audio s) --")
                print(
                    cov_tables(parse_cov(log.read_text(errors="replace")), r["audio_s"])
                )
    (out / "report.json").write_text(json.dumps(report, indent=1))
    if args.json:
        print(json.dumps(report, indent=1))
    if not ok:
        print("\nFAILED: a run failed or PCM/state differs from the first label")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
