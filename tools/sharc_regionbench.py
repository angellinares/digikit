"""Region microbenchmark loop for the generated DSP block code.

A codegen experiment on the hot DSP regions costs minutes through the coupled
build. This tool makes a gen dir that holds only the selected regions
(tools/sharc_rsgen.py --only), captures the engine state at each region's
entry from the DN2 note replay, and times the region's generated entry
function from that state (native/sharc/examples/region_bench.rs).

    sharc_regionbench.py gen [--variant base] [--tools-dir DIR] [-- RSGEN ARGS]
    sharc_regionbench.py capture [--variant base]
    sharc_regionbench.py bench [--variant base] [--regions PC,..] [--reps 5]
    sharc_regionbench.py compare A B [--rounds 5]
    sharc_regionbench.py check [--variant base]
    sharc_regionbench.py fast-bench [--variant base] [--regions PC,..] [--reps 5]

Everything lives under out/native/regionbench/: work-<hash>/ (the transpile
inference, keyed by the transpiler sources so a generator-only change does not
re-run it), states/ (captured entry states, shared by all variants so A/B
compares the same inputs), variants/NAME/ (gen/, target/, region_bench binary
and its sha256, bench-*.jsonl). The firmware-derived files stay under out/.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import statistics
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCH = os.path.join(ROOT, "out", "native", "regionbench")
PROV = os.path.join(ROOT, "out", "native", "gen-provenance-20261003")
INPUTS = os.path.join(PROV, "inputs")
FULL_GEN = os.path.join(PROV, "gen-A")
IMAGE_NAME = "dn2-1.11"
FIXTURE = os.path.join(ROOT, "snapshots", "dn2-audio-ready-2026-10-03")
PACKED_IMAGE = os.path.join(FIXTURE, "digi-audio-dn2-image.bin")
STATE = os.path.join(FIXTURE, "digi-audio-loop2-fresh.bin")
# Copied from /private/tmp/dn2-audio-restart-20261003/dsp-profile/ (DSPI2 TX
# frames of the note capture).
FRAMES = os.path.join(BENCH, "digi-audio-note.frames")
# Hand-written versions of hot regions (firmware-derived: stays under out/,
# compiled into region_bench only when present, see native/sharc/build.rs).
SOL = os.path.join(BENCH, "sol", "sol.rs")
DEFAULT_REGIONS = "1C399A,1C3862,1C364F,1C2484,1C5765,B823DB"
TOOLCHAIN = "1.98.1"
TRIPLE = "aarch64-apple-darwin"
RSGEN_BASE = [
    "--model-safe",
    "--explicit-memory-model",
    "0",
    "--chain",
    "--exclude",
    "0xb88a49:0xb88abc",
    "--unknown-fallbacks",
    "0x1c253f",
]
# The app's release profile (native/live/Cargo.toml), without LTO or PGO.
PROFILE_ENV = {
    "CARGO_PROFILE_RELEASE_OPT_LEVEL": "3",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16",
    "CARGO_PROFILE_RELEASE_LTO": "false",
    "CARGO_PROFILE_RELEASE_DEBUG": "false",
    "CARGO_PROFILE_RELEASE_PANIC": "abort",
    "CARGO_PROFILE_RELEASE_INCREMENTAL": "false",
}


def sha256_files(paths: list[str]) -> str:
    h = hashlib.sha256()
    for p in sorted(paths):
        h.update(p.encode() + b"\0")
        with open(p, "rb") as fh:
            h.update(fh.read())
    return h.hexdigest()


def tree_files(top: str, suffixes: tuple[str, ...]) -> list[str]:
    out = []
    for dirpath, _dirs, names in os.walk(top):
        out += [os.path.join(dirpath, n) for n in names if n.endswith(suffixes)]
    return out


def variant_dir(name: str) -> str:
    return os.path.join(BENCH, "variants", name)


def python() -> str:
    return os.path.join(ROOT, ".venv", "bin", "python")


def run(cmd: list[str], **kw) -> float:
    t = time.perf_counter()
    subprocess.run(cmd, check=True, cwd=kw.pop("cwd", ROOT), **kw)
    return time.perf_counter() - t


# -- regions ---------------------------------------------------------------


def region_blocks(names: list[str]) -> dict[str, list[str]]:
    """Region name -> its block starts, read from the full generated core
    (each block entry b_X calls its region r_NAME). Cached."""
    cache = os.path.join(BENCH, "regions.json")
    known: dict[str, list[str]] = {}
    if os.path.exists(cache):
        with open(cache) as fh:
            known = json.load(fh)
    if all(n in known for n in names):
        return known
    pat = re.compile(r"pub fn b_([0-9A-F]+)\(s: &mut St\) -> u32 \{\n(.*?)\n\}\n", re.S)
    found: dict[str, list[str]] = {}
    for path in sorted(tree_files(FULL_GEN, (".rs",))):
        if not os.path.basename(path).startswith("blocks_"):
            continue
        with open(path) as fh:
            text = fh.read()
        for m in pat.finditer(text):
            r = re.search(r"\br_([0-9A-F]+)\(s, ", m.group(2))
            if r:
                found.setdefault(r.group(1), []).append(m.group(1))
    known.update(found)
    os.makedirs(BENCH, exist_ok=True)
    with open(cache, "w") as fh:
        json.dump(known, fh, indent=1)
    missing = [n for n in names if n not in known]
    if missing:
        sys.exit("regions not in the full gen: %s" % missing)
    return known


# -- gen -------------------------------------------------------------------


def work_dir(tools_dir: str) -> str:
    files = [os.path.join(tools_dir, "sharc_transpile.py")]
    files += [
        os.path.join(tools_dir, f)
        for f in os.listdir(tools_dir)
        if f.startswith("sharc_transpile") and f.endswith(".py")
    ]
    files += tree_files(os.path.join(tools_dir, "sharc_core"), (".py",))
    return os.path.join(BENCH, "work-" + sha256_files(sorted(set(files)))[:12])


def cmd_gen(args, extra: list[str]) -> None:
    tools = os.path.abspath(args.tools_dir)
    names = args.regions.split(",")
    blocks = region_blocks(names)
    only = ",".join(b for n in names for b in blocks[n])
    work = work_dir(tools)
    vdir = variant_dir(args.variant)
    gen = os.path.join(vdir, "gen")
    os.makedirs(gen, exist_ok=True)
    timings = {}
    if os.path.isdir(work) and os.path.exists(os.path.join(work, ".done")):
        timings["transpile"] = 0.0
    else:
        timings["transpile"] = run(
            [python(), os.path.join(tools, "sharc_transpile.py"), "--strict"]
            + ["--out", os.path.join(work + "-tp-out"), "--work", work]
            + shlex.split(args.transpile_args),
            stdout=subprocess.DEVNULL,
        )
        open(os.path.join(work, ".done"), "w").close()
    timings["rsgen"] = run(
        [python(), os.path.join(tools, "sharc_rsgen.py"), IMAGE_NAME]
        + ["--coverage", os.path.join(INPUTS, "cov6.txt")]
        + ["--entries", os.path.join(INPUTS, "ent9.txt")]
        + ["--transitions", os.path.join(INPUTS, "trans9.txt")]
        + RSGEN_BASE
        + ["--work", work, "--out", gen, "--only", only]
        + extra,
        stdout=subprocess.DEVNULL,
    )
    with open(os.path.join(vdir, "gen.json"), "w") as fh:
        json.dump(
            {
                "regions": names,
                "only": only,
                "work": work,
                "rsgen_extra": extra,
                "tools_dir": tools,
                "seconds": timings,
            },
            fh,
            indent=1,
        )
    print("gen %s: transpile %.1fs rsgen %.1fs" % (args.variant, *timings.values()))


# -- build -----------------------------------------------------------------


def benchlock(mode: str, label: str):
    """Lock for builds (shared) and timed runs (exclusive), see benchlock.py."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    try:
        import benchlock as module
    finally:
        sys.path.pop(0)
    return module.hold(mode, label)


def cmd_build(args) -> str:
    vdir = variant_dir(args.variant)
    gen = os.path.join(vdir, "gen")
    if not os.path.isdir(gen):
        sys.exit("no gen for variant %s: run gen first" % args.variant)
    src = tree_files(os.path.join(ROOT, "native", "sharc", "src"), (".rs",))
    src += [
        os.path.join(ROOT, "native", "sharc", "examples", "region_bench.rs"),
        os.path.join(ROOT, "native", "sharc", "build.rs"),
    ]
    # Modules of the example (region_bench/*.rs) and the optional hand-written
    # versions.
    src += tree_files(
        os.path.join(ROOT, "native", "sharc", "examples", "region_bench"), (".rs",)
    )
    if os.path.isfile(SOL):
        src.append(SOL)
    # The fast tier's Cranelift backend (a dev-dependency of the example).
    fast_cl = os.path.join(ROOT, "native", "sharc-fast-cl")
    src += tree_files(os.path.join(fast_cl, "src"), (".rs",))
    src += [
        os.path.join(fast_cl, "Cargo.toml"),
        os.path.join(ROOT, "native", "sharc", "Cargo.toml"),
        os.path.join(ROOT, "native", "sharc", "Cargo.lock"),
    ]
    gen_files = [
        os.path.join(gen, n)
        for n in os.listdir(gen)
        if n != "rsgen-report.json" and not n.startswith("transpile-report")
    ]
    pgo = getattr(args, "pgo", None)
    key = hashlib.sha256(
        (
            sha256_files(src + gen_files)
            + repr(pgo)
            + repr(sorted(PROFILE_ENV.items()))
        ).encode()
    ).hexdigest()
    exe = os.path.join(vdir, "region_bench")
    stamp = os.path.join(vdir, "build.json")
    # A variant with a FROZEN file keeps its binary whatever the sources do
    # (an A/B against a build of older sources).
    if os.path.exists(exe) and os.path.exists(os.path.join(vdir, "FROZEN")):
        print("build %s: frozen" % args.variant)
        return exe
    if os.path.exists(exe) and os.path.exists(stamp):
        with open(stamp) as fh:
            if json.load(fh).get("key") == key:
                print("build %s: up to date" % args.variant)
                return exe
    env = dict(os.environ, SHARC_GEN_DIR=gen, **PROFILE_ENV)
    if os.path.isfile(SOL):
        env["SHARC_REGIONBENCH_SOL"] = SOL
    if pgo:
        env["RUSTFLAGS"] = "-Cprofile-use=%s -Cllvm-args=-pgo-warn-missing-function" % (
            os.path.abspath(pgo)
        )
    target = os.path.join(vdir, "target")
    with benchlock("shared", "regionbench build"):
        secs = run(
            ["rustup", "run", TOOLCHAIN, "cargo", "build", "--release", "--locked"]
            + ["--example", "region_bench", "--target", TRIPLE]
            + ["--manifest-path", os.path.join(ROOT, "native", "sharc", "Cargo.toml")]
            + ["--target-dir", target],
            env=env,
        )
    built = os.path.join(target, TRIPLE, "release", "examples", "region_bench")
    with open(built, "rb") as fh:
        digest = hashlib.sha256(fh.read()).hexdigest()
    subprocess.run(["cp", built, exe], check=True)
    with open(exe + ".sha256", "w") as fh:
        fh.write("%s  region_bench\n" % digest)
    with open(stamp, "w") as fh:
        json.dump(
            {"key": key, "build_seconds": round(secs, 1), "pgo": pgo, "sha256": digest},
            fh,
        )
    print("build %s: %.1fs sha256 %s" % (args.variant, secs, digest[:12]))
    return exe


# -- capture / bench -------------------------------------------------------


def states_dir() -> str:
    return os.path.join(BENCH, "states")


def cmd_capture(args) -> None:
    exe = cmd_build(args)
    names = args.regions.split(",")
    blocks = region_blocks(names)
    entries = [b for n in names for b in blocks[n]]
    secs = run(
        [exe, "capture", PACKED_IMAGE, STATE, FRAMES, states_dir(), ",".join(entries)]
        + ["--min-frame", str(args.min_frame)]
    )
    print("capture: %.1fs" % secs)


def read_meta(entry: str) -> dict[str, str] | None:
    path = os.path.join(states_dir(), "r_%s.meta" % entry)
    if not os.path.exists(path):
        return None
    with open(path) as fh:
        return dict(line.rstrip("\n").split("=", 1) for line in fh if "=" in line)


def pick_entries(names: list[str]) -> dict[str, str]:
    """Region -> the entry block with the most instructions run in the
    capture window (the hot way into the region)."""
    blocks = region_blocks(names)
    picked = {}
    for n in names:
        best = (0, "")
        for b in blocks[n]:
            meta = read_meta(b)
            if meta and int(meta["total_insns_in_window"]) > best[0]:
                best = (int(meta["total_insns_in_window"]), b)
        if not best[1]:
            sys.exit("region %s has no captured entry: run capture" % n)
        picked[n] = best[1]
    return picked


def run_bench(exe: str, names: list[str], reps: int, calls: int) -> list[dict]:
    """Rows keyed by region name (entry in row['entry'])."""
    entries = pick_entries(names)
    back = {e: n for n, e in entries.items()}
    out = subprocess.run(
        [exe, "bench", PACKED_IMAGE, states_dir(), ",".join(entries.values())]
        + ["--reps", str(reps), "--calls", str(calls)],
        check=True,
        cwd=ROOT,
        capture_output=True,
        text=True,
    ).stdout
    rows = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    for r in rows:
        r["entry"] = r["region"]
        r["region"] = back[r["entry"]]
    return rows


def summarize(rows: list[dict]) -> list[tuple]:
    by: dict[str, list[dict]] = {}
    for r in rows:
        by.setdefault(r["region"], []).append(r)
    table = []
    for reg, rs in by.items():
        med = [r["trim_ns"] for r in rs]
        m = statistics.median(med)
        ins = statistics.mean(r["insns_mean"] for r in rs)
        spread = (max(med) - min(med)) / m * 100 if m else 0.0
        const = all(
            r["insns_min"] == r["insns_max"] and not r["exit_varies"] for r in rs
        )
        table.append(
            (
                "%s/%s" % (reg, rs[0]["entry"]),
                ins,
                m,
                m / ins if ins else 0.0,
                spread,
                statistics.median(r["p10_ns"] for r in rs),
                statistics.median(r["p90_ns"] for r in rs),
                const,
            )
        )
    return table


def print_table(table: list[tuple]) -> None:
    print(
        "region/entry    insns/call  ns/call(trim)  ns/insn  rep-spread%  p10..p90 ns   constant"
    )
    for reg, ins, m, per, spread, p10, p90, const in table:
        print(
            "%-15s %10.1f %13.0f %8.2f %12.2f  %6.0f..%-6.0f  %s"
            % (reg, ins, m, per, spread, p10, p90, "yes" if const else "NO")
        )


def cmd_bench(args) -> None:
    exe = cmd_build(args)
    names = args.regions.split(",")
    t = time.perf_counter()
    with benchlock("exclusive", "regionbench bench"):
        rows = run_bench(exe, names, args.reps, args.calls)
    secs = time.perf_counter() - t
    path = os.path.join(variant_dir(args.variant), "bench-%d.jsonl" % int(time.time()))
    with open(path, "w") as fh:
        fh.write("".join(json.dumps(r) + "\n" for r in rows))
    print_table(summarize(rows))
    print("bench run: %.1fs -> %s" % (secs, path))


def cmd_fast_bench(args) -> None:
    """Time the fast tier next to the generated region on the captured
    entries (region_bench fast-bench): ns/call of the generated region
    (aot), the fast kernel alone (fast_loop), the kernel plus the generated
    block at the pc it stops at (fast_total), and Cranelift compile time."""
    exe = cmd_build(args)
    names = args.regions.split(",")
    entries = pick_entries(names)
    back = {e: n for n, e in entries.items()}
    with benchlock("exclusive", "regionbench fast-bench"):
        out = subprocess.run(
            [exe, "fast-bench", PACKED_IMAGE, states_dir(), ",".join(entries.values())]
            + ["--reps", str(args.reps), "--calls", str(args.calls)],
            check=True,
            cwd=ROOT,
            capture_output=True,
            text=True,
        ).stdout
    rows = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    path = os.path.join(
        variant_dir(args.variant), "fastbench-%d.jsonl" % int(time.time())
    )
    with open(path, "w") as fh:
        fh.write("".join(json.dumps(r) + "\n" for r in rows))
    print("region/entry      mode          ns/call(trim)  vs aot   compile us")
    for entry, name in back.items():
        aot = [
            r["trim_ns"] for r in rows if r["region"] == entry and r["mode"] == "aot"
        ]
        base = statistics.median(aot)
        for mode in ("aot", "fast_loop", "fast_total"):
            v = [
                r["trim_ns"] for r in rows if r["region"] == entry and r["mode"] == mode
            ]
            m = statistics.median(v)
            cu = statistics.median(
                r["compile_us"] for r in rows if r["region"] == entry
            )
            print(
                "%-17s %-12s %12.0f %8.2fx %10.0f"
                % ("%s/%s" % (name, entry), mode, m, base / m, cu)
            )
    print("rows -> %s" % path)


def cmd_compare(args) -> None:
    exes = {}
    for v in (args.a, args.b):
        args.variant = v
        exes[v] = cmd_build(args)
    names = args.regions.split(",")
    meds: dict[str, dict[str, list[float]]] = {
        n: {args.a: [], args.b: []} for n in names
    }
    with benchlock("exclusive", "regionbench compare"):
        for i in range(args.rounds):
            order = (args.a, args.b) if i % 2 == 0 else (args.b, args.a)
            for v in order:
                for r in run_bench(exes[v], names, 1, args.calls):
                    meds[r["region"]][v].append(r["trim_ns"])
    print("region   ratio B/A (median of paired)  min..max   A ns   B ns")
    for n in names:
        a, b = meds[n][args.a], meds[n][args.b]
        ratios = [y / x for x, y in zip(a, b, strict=True)]
        print(
            "%-8s %8.4f  %.4f..%.4f  %8.0f %8.0f"
            % (
                n,
                statistics.median(ratios),
                min(ratios),
                max(ratios),
                statistics.median(a),
                statistics.median(b),
            )
        )


def cmd_check(args) -> None:
    """Diff each region's generated body against the full generated core,
    modulo variant ids and chain targets outside the mini gen."""
    gen = os.path.join(variant_dir(args.variant), "gen")
    names = args.regions.split(",")
    blocks = region_blocks(names)
    inside = {b for n in names for b in blocks[n]}

    def norm(text: str) -> str:
        text = re.sub(r"spec_\d+::(\w+?)__v\d+", r"\1__v#", text)
        pat = r"crate::generated::image::blocks_\d+::b_([0-9A-F]+)|\bb_([0-9A-F]+)\b"
        return re.sub(
            pat,
            lambda m: (
                ("B_" + (m.group(1) or m.group(2)))
                if (m.group(1) or m.group(2)) in inside
                else "crate::no_block"
            ),
            text,
        )

    def body(path_dir: str, name: str) -> str:
        for p in sorted(tree_files(path_dir, (".rs",))):
            if not os.path.basename(p).startswith("blocks_"):
                continue
            with open(p) as fh:
                t = fh.read()
            i = t.find("pub fn r_%s(" % name)
            if i >= 0:
                return norm(t[i : t.index("\npub fn ", i + 10)])
        sys.exit("no region %s in %s" % (name, path_dir))

    for n in names:
        same = body(FULL_GEN, n) == body(gen, n)
        print("r_%s: %s" % (n, "identical" if same else "DIFFERENT"))


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    extra: list[str] = []
    if "--" in argv:
        i = argv.index("--")
        argv, extra = argv[:i], argv[i + 1 :]
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp, variant=True):
        if variant:
            sp.add_argument("--variant", default="base")
        sp.add_argument("--regions", default=DEFAULT_REGIONS)
        sp.add_argument("--pgo", help="profdata for -Cprofile-use (default: none)")
        sp.add_argument("--calls", type=int, default=4000)
        return sp

    g = common(sub.add_parser("gen", help="mini gen of the selected regions"))
    g.add_argument("--tools-dir", default=os.path.join(ROOT, "tools"))
    g.add_argument("--transpile-args", default="")
    c = common(sub.add_parser("capture", help="capture entry states"))
    c.add_argument("--min-frame", type=int, default=4620)
    b = common(sub.add_parser("bench", help="build and time one variant"))
    b.add_argument("--reps", type=int, default=5)
    k = common(sub.add_parser("compare", help="alternating A/B"), variant=False)
    k.add_argument("a")
    k.add_argument("b")
    k.add_argument("--rounds", type=int, default=5)
    common(sub.add_parser("check", help="diff region bodies against the full gen"))
    f = common(
        sub.add_parser("fast-bench", help="fast tier next to the generated region")
    )
    f.add_argument("--reps", type=int, default=5)
    args = p.parse_args(argv)
    if args.cmd == "gen":
        cmd_gen(args, extra)
    elif args.cmd == "capture":
        cmd_capture(args)
    elif args.cmd == "bench":
        cmd_bench(args)
    elif args.cmd == "compare":
        cmd_compare(args)
    elif args.cmd == "fast-bench":
        cmd_fast_bench(args)
    else:
        cmd_check(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
