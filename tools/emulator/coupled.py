#!/usr/bin/env python3
"""Build and run the headless coupled runner (``coupled-run``) and bisect DSP tiers.

``coupled-run`` (packages/emulator/coupled/src/bin/coupled_run.rs) restores the DN2 ready
session and plays a scenario of panel events, reporting the PCM hash and RMS.
The binary is built into the prod generated-core target dir
(tools/emulator/native_target.py), so it reuses the compiled ``sharc-gen``.

    uv run python -m tools.emulator.coupled build
    uv run python -m tools.emulator.coupled scenario NAME [NAME ...] [--wav] [--out JSON] [-- EXTRA...]
    uv run python -m tools.emulator.coupled scenario --all [--device dn2|dt2]
    uv run python -m tools.emulator.coupled replay FILE [--wav] [--out JSON]
    uv run python -m tools.emulator.coupled bisect NAME [--seconds S]
    uv run python -m tools.emulator.coupled test-quick

Every command except ``build`` and ``test-quick`` builds first (a no-op when
current) unless ``--no-build`` is given. ``bisect`` finds which DSP tier changes
a scenario's PCM: the interpreter-only reference (cached under
``out/native/bisect/<name>/``) against the default tiers, then single changes
from the default (no fast regions, no AOT, one region or cluster root removed).
Exit codes: ``scenario`` and ``replay`` return coupled-run's (0 ok, 2 gate
failure, 1 error); ``bisect`` returns 0 when the default matches the reference.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import pathlib
import random
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]

from tools.devtools import render_cache  # noqa: E402
from tools.emulator import digi_env, native_target  # noqa: E402

TOOLCHAIN = digi_env.TOOLCHAIN
DEFAULT_FEATURES = "coupled-audio"
FEATURES = os.environ.get("DIGI_COUPLED_FEATURES", DEFAULT_FEATURES)
BINARY_NAME = "coupled-run"
BISECT_DIR = ROOT / "out" / "native" / "bisect"
RUNS_DIR = digi_env.RUNS_ROOT


def benchlock(mode: str, label: str):
    """Lock for builds (shared) and timed runs (exclusive), see benchlock.py."""
    from tools.devtools import benchlock as module

    return module.hold(mode, label)


# Pure helpers


# Set by enable_dt2_card(): the one-sample card's ready inputs and `--card-image`.
CARD: tuple[dict[str, pathlib.Path], list[str]] | None = None


def enable_dt2_card() -> None:
    """Run DT2 on the one-sample card of tools/emulator/dt2_sample_check.py: `inputs`
    gives the card's ready state and `run_args` adds `--card-image`. Needed by
    the dt2-trackfx / dt2-sendfx scenarios (a trig plays the card's sample)."""
    global CARD
    if CARD is None:
        from tools.emulator import dt2_sample_check

        CARD = dt2_sample_check.card_inputs()


def run_args(
    binary: str | os.PathLike[str], inputs: dict[str, pathlib.Path], extra: list[str]
) -> list[str]:
    """The coupled-run argv: inputs first, then the extra arguments in order."""
    return [
        str(binary),
        "--syx", str(inputs["firmware"]),
        "--dsp-image", str(inputs["DSP image"]),
        "--dsp-state", str(inputs["DSP state"]),
        "--cf-snapshot", str(inputs["CF snapshot"]),
        *(CARD[1] if CARD else []),
        *extra,
    ]  # fmt: skip


def split_list(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def bisect_configs(fast: dict[str, str]) -> list[tuple[str, dict[str, str]]]:
    """Single-change configurations from the default (not reference or default)."""
    regions = split_list(fast.get("SHARC_FAST_REGIONS", ""))
    roots = split_list(fast.get("SHARC_BLOCKTRANS", ""))

    def env(rs: list[str], cs: list[str]) -> dict[str, str]:
        return {"SHARC_FAST_REGIONS": ",".join(rs), "SHARC_BLOCKTRANS": ",".join(cs)}

    configs = [
        ("no-fast", env([], [])),
        ("no-aot", {**env(regions, roots), "SHARC_AOT": "0"}),
    ]
    for region in regions:
        rest = [r for r in regions if r != region]
        configs.append((f"fast-minus-{region}", env(rest, roots)))
    for root in roots:
        rest = [c for c in roots if c != root]
        configs.append((f"roots-minus-{root}", env(regions, rest)))
    return configs


def first_jump(windows: list[float], db: float = 6.0) -> int | None:
    """First window whose RMS differs from the previous by more than ``db`` dB.

    RMS 0 is -inf dB: a jump from or to silence counts when the other side is
    above 1e-4.
    """
    for i in range(1, len(windows)):
        prev, cur = windows[i - 1], windows[i]
        if prev == 0 and cur == 0:
            continue
        if prev == 0 or cur == 0:
            if max(prev, cur) > 1e-4:
                return i
            continue
        if abs(20 * math.log10(cur / prev)) > db:
            return i
    return None


def first_divergence(a: list[float], b: list[float], rel: float = 1e-6) -> int | None:
    """First index where a and b differ by more than ``rel`` relative."""
    for i, (x, y) in enumerate(zip(a, b, strict=False)):
        scale = max(abs(x), abs(y))
        if scale > 0 and abs(x - y) > rel * scale:
            return i
    if len(a) != len(b):
        return min(len(a), len(b))
    return None


def scenario_results(report: dict) -> list[dict]:
    """name, sha256, ok and 100 ms RMS windows of each scenario in a report."""
    results = []
    for sc in report.get("scenarios", []):
        pcm = sc.get("pcm") or {}
        results.append(
            {
                "name": sc.get("name"),
                "sha256": pcm.get("sha256"),
                "ok": (sc.get("gates") or {}).get("ok"),
                "windows": list(pcm.get("window_rms_100ms") or []),
                "peek": sc.get("peek") or {},
            }
        )
    return results


# Paths and processes


def generated(
    device: str = "dn2", image: pathlib.Path | str | None = None
) -> pathlib.Path:
    """The generated core of a device: SHARC_GEN_DIR, else the one registered
    for its packed DSP image (IMAGE, else the device's ready-input default)."""
    if image is None:
        image = digi_env.ready_inputs(device=device)["DSP image"]
    gen = digi_env.generated_dir(image=image)
    if not gen.is_dir():
        raise SystemExit(f"coupled: generated core dir is missing: {gen}")
    return gen


def inputs(device: str = "dn2") -> dict[str, pathlib.Path]:
    if CARD:
        return CARD[0]
    found = digi_env.ready_inputs(device=device)
    missing = [f"{k}: {v}" for k, v in found.items() if not pathlib.Path(v).exists()]
    if missing:
        raise SystemExit("coupled: missing inputs:\n  " + "\n  ".join(missing))
    return found


def feature_variant(features: str | None = None) -> str:
    """Target-dir variant for a feature list: empty for the default, else `feat-<sorted names>`.

    A feature build gets its own dir, so it never replaces the default binary."""
    raw = FEATURES if features is None else features
    names = sorted({n.strip() for n in raw.split(",") if n.strip()})
    if names == sorted(DEFAULT_FEATURES.split(",")):
        return ""
    return "feat-" + "+".join(names)


def binary_path(device: str = "dn2", image=None) -> pathlib.Path:
    target = native_target.target_dir(generated(device, image), feature_variant())
    return target / "release" / digi_env.exe_name(BINARY_NAME)


def cargo(args: list[str]) -> list[str]:
    return ["rustup", "run", TOOLCHAIN, "cargo", *args]


def build(device: str = "dn2", image=None) -> pathlib.Path:
    gen = generated(device, image)
    digi_env.check_generated(gen)
    target = native_target.target_dir(gen, feature_variant())
    env = {**digi_env.rust_env(), "SHARC_GEN_DIR": str(gen)}
    command = cargo(
        [
            "build", "--release", "--locked",
            "--manifest-path", "packages/emulator/coupled/Cargo.toml",
            "--features", FEATURES,
            "--bin", BINARY_NAME,
            "--target-dir", str(target),
        ]
    )  # fmt: skip
    with benchlock("shared", "coupled-run build"):
        done = subprocess.run(command, cwd=ROOT, env=env, check=False)
    if done.returncode != 0:
        raise SystemExit(done.returncode)
    binary = target / "release" / digi_env.exe_name(BINARY_NAME)
    if not binary.is_file():
        raise SystemExit(f"coupled: build left no binary at {binary}")
    return binary


def ensure_binary(no_build: bool, device: str = "dn2", image=None) -> pathlib.Path:
    if not no_build:
        return build(device, image)
    binary = binary_path(device, image)
    if not binary.is_file():
        raise SystemExit(f"coupled: no binary at {binary} (drop --no-build)")
    return binary


def run_env(
    overrides: dict[str, str] | None = None, *, syx: pathlib.Path | None = None
) -> dict[str, str]:
    env = {**os.environ, **(overrides or {})}
    env.update(digi_env.fast_env(env, syx=syx))
    return env


def run_kind(selection: list[str]) -> str:
    """The out/runs/<kind> name of a run: its scenario names, or the replay file."""
    names = [
        b for a, b in zip(selection, selection[1:], strict=False) if a == "--scenario"
    ]
    if names:
        return "+".join(names)
    if not selection:
        return "all"
    return "replay-" + pathlib.Path(selection[-1]).stem


def read_report(path: pathlib.Path) -> dict:
    with path.open() as handle:
        return json.load(handle)


def summarize(report_path: pathlib.Path) -> None:
    if not report_path.is_file():
        print(f"no report at {report_path}")
        return
    for r in scenario_results(read_report(report_path)):
        jump = first_jump(r["windows"])
        sha = (r["sha256"] or "-")[:16]
        print(
            f"{r['name']:<16} sha256 {sha} ok={r['ok']} "
            f"first >6dB window {'-' if jump is None else jump}"
        )
        for name, hexval in (r.get("peek") or {}).items():
            print(f"  peek {name} = {hexval}")
    print(f"report: {report_path}")


def run_and_report(binary: pathlib.Path, selection: list[str], args) -> int:
    """Run into out/runs/<kind>/<stamp>/ (report.json, audio.wav, logs/); --out
    names the report instead and the rest goes next to it."""
    out = (
        pathlib.Path(args.out)
        if args.out
        else digi_env.run_dir(run_kind(selection)) / "report.json"
    )
    diag = digi_env.parse_diag(args.diag or "")
    wav = args.wav or any(n == "pcm" for n, _ in diag)
    extra = [*selection, "--out", str(out)]
    if wav:
        extra += ["--wav", str(out.parent / "audio.wav")]
    if getattr(args, "seconds", None) is not None:
        extra += ["--seconds", str(args.seconds)]
    extra += args.extra
    device = getattr(args, "device", "dn2")
    scenario_file = getattr(args, "scenarios", None)
    if scenario_file:
        extra = ["--scenarios", str(scenario_path(scenario_file)), *extra]
    elif device != "dn2":
        extra = [
            "--scenarios",
            str(ROOT / "tests" / "scenarios" / f"{device}.json"),
            *extra,
        ]
    for spec in getattr(args, "peek", None) or []:
        extra += ["--peek", spec]
    if getattr(args, "dump_dsp", False):
        extra += ["--dump-dsp", str(out.parent / "dsp.bin")]
    actual_inputs = inputs(device)
    argv = run_args(binary, actual_inputs, extra)
    env = run_env(digi_env.diag_env(diag, out.parent), syx=actual_inputs["firmware"])
    if digi_env.diag_stderr(diag):
        with (out.parent / "logs" / "stderr.log").open("wb") as err:
            done = subprocess.run(argv, cwd=ROOT, env=env, stderr=err, check=False)
    else:
        done = subprocess.run(argv, cwd=ROOT, env=env, check=False)
    summarize(out)
    if diag:
        print(f"diagnostics: {out.parent / 'logs'}")
    return done.returncode


def scenario_document(scenarios_file: pathlib.Path | str) -> dict:
    """Read JSON or execute an erasable TypeScript scenario source with Node >=24.

    Always evaluate source afresh; callers may retain this document for their batch.
    Enumeration does not write generated files (including in an explain-only run).
    """
    path = pathlib.Path(scenarios_file).resolve()
    if path.suffix == ".ts":
        done = subprocess.run(
            ["node", str(path)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        )
        return json.loads(done.stdout)
    return json.loads(path.read_text())


def scenario_path(scenarios_file: pathlib.Path | str) -> pathlib.Path:
    """Materialize TS as content-addressed JSON only when a native runner needs it."""
    source = pathlib.Path(scenarios_file).resolve()
    if source.suffix != ".ts":
        return source
    payload = json.dumps(scenario_document(source), separators=(",", ":")).encode()
    digest = hashlib.sha256(payload).hexdigest()
    path = ROOT / "out" / "scenarios" / f"{digest}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as handle:
            temporary = pathlib.Path(handle.name)
            try:
                handle.write(payload)
                handle.close()
                os.replace(temporary, path)
            finally:
                temporary.unlink(missing_ok=True)
    return path


def scenario_seconds(scenarios_file: pathlib.Path | str) -> dict[str, float]:
    """Audio seconds of each scenario in a scenario file (its cost to render)."""
    doc = scenario_document(scenarios_file)
    return {
        sc["name"]: float(sc.get("seconds") or 0.0) for sc in doc.get("scenarios", [])
    }


def shard_names(
    names: list[str], seconds: dict[str, float], jobs: int
) -> list[list[str]]:
    """Split ``names`` into at most ``jobs`` non-empty shards of similar total
    ``seconds`` (longest first onto the lightest shard); each shard keeps the
    order of ``names``. A name missing from ``seconds`` weighs the mean."""
    jobs = max(1, min(jobs, len(names)))
    known = [seconds[n] for n in names if seconds.get(n)]
    mean = sum(known) / len(known) if known else 1.0
    weight = {n: seconds.get(n) or mean for n in names}
    shards: list[list[str]] = [[] for _ in range(jobs)]
    load = [0.0] * jobs
    for n in sorted(names, key=lambda n: -weight[n]):
        i = min(range(jobs), key=lambda k: (load[k], k))
        shards[i].append(n)
        load[i] += weight[n]
    order = {n: i for i, n in enumerate(names)}
    return [sorted(s, key=order.__getitem__) for s in shards if s]


def merge_reports(reports: list[dict], names: list[str]) -> dict:
    """One report from shard reports: the first's top-level fields and the
    scenario entries in the order of ``names``."""
    by_name = {sc["name"]: sc for r in reports for sc in r.get("scenarios", [])}
    missing = [n for n in names if n not in by_name]
    if missing:
        raise RuntimeError(f"coupled-run reports lack scenarios: {missing}")
    return {**reports[0], "scenarios": [by_name[n] for n in names]}


def default_jobs() -> int:
    """Parallel coupled-run processes for tests: DIGI_JOBS, else
    min(cpus - 2, 6) (tests/scenarios/README.md, "Parallel rendering")."""
    env = os.environ.get("DIGI_JOBS")
    if env:
        return max(1, int(env))
    return max(1, min((os.cpu_count() or 1) - 2, 6))


def _run_one(
    argv: list[str], env: dict[str, str], run_dir: pathlib.Path
) -> pathlib.Path:
    out = run_dir / "report.json"
    with (
        (run_dir / "logs" / "stdout.log").open("wb") as stdout,
        (run_dir / "logs" / "stderr.log").open("wb") as stderr,
    ):
        done = subprocess.run(
            argv, cwd=ROOT, env=env, stdout=stdout, stderr=stderr, check=False
        )
    if done.returncode not in (0, 2) or not out.is_file():
        raise RuntimeError(
            f"coupled-run failed ({done.returncode}); see {run_dir / 'logs'}"
        )
    return out


def recheck_probability() -> float:
    """``DIGI_RENDER_CACHE_CHECK``: chance (0..1, default 0) that a cache hit is
    re-rendered and compared."""
    raw = os.environ.get("DIGI_RENDER_CACHE_CHECK", "0")
    try:
        value = float(raw)
    except ValueError:
        value = math.nan
    if not 0.0 <= value <= 1.0:
        raise SystemExit(
            f"DIGI_RENDER_CACHE_CHECK={raw!r}: expected a probability from 0 to 1"
        )
    return value


def pick_rechecks(
    names: list[str], probability: float, rng: random.Random
) -> list[str]:
    """The names (in order) chosen for a fresh re-render, each with
    ``probability``."""
    if probability <= 0:
        return []
    return [n for n in names if probability >= 1 or rng.random() < probability]


def _recheck_view(entry: dict) -> tuple[object, object]:
    return ((entry.get("pcm") or {}).get("sha256"), entry.get("peek"))


def run_batch(
    device: str,
    scenarios_file: pathlib.Path | str,
    names: list[str],
    run_dir: pathlib.Path | str,
    peeks: tuple[str, ...] | list[str] = (),
    diag: str | None = None,
    binary: pathlib.Path | None = None,
    no_build: bool = False,
    jobs: int = 1,
    *,
    cache: render_cache.RenderCache | None = None,
) -> dict:
    """Render ``names`` of a scenario file into ``run_dir`` (report.json,
    audio-<name>.wav, logs/) and return the parsed report. ``peeks`` are
    ``NAME=SPEC`` guest reads at the end of each scenario (``--peek``). With
    ``jobs`` > 1 the names are sharded by audio seconds over that many
    concurrent coupled-run processes (shards in ``run_dir/shard-N/``; each
    scenario restores its own snapshot, so the PCM is identical) and the
    reports are merged; the processes hold the benchlock shared. Raises
    RuntimeError when coupled-run fails to run (exit 1 or no report); exit 2
    (failed gate or pin) still returns the report, whose ``gates`` say which.
    ``binary`` skips the build step."""
    parsed = digi_env.parse_diag(diag or "")
    if binary is None:
        binary = ensure_binary(no_build, device)
    run_dir = pathlib.Path(run_dir)
    (run_dir / "logs").mkdir(parents=True, exist_ok=True)
    scn = str(scenario_path(scenarios_file))
    if cache is None:
        cache = render_cache.RenderCache()
    use_cache = cache.enabled and not parsed
    keys: dict[str, str] = {}
    hits: dict[str, tuple[dict, dict]] = {}
    if use_cache:
        base = run_args(binary, inputs(device), [])
        context = render_cache.context_key(
            binary,
            base[1:],
            run_env(syx=inputs(device)["firmware"]),
            device,
            list(peeks),
        )
        with pathlib.Path(scn).open() as handle:
            objs = {sc["name"]: sc for sc in json.load(handle)["scenarios"]}
        for name in names:
            if name not in objs:
                continue
            keys[name] = render_cache.scenario_key(context, objs[name])
            found = cache.load(keys[name])
            if found is None:
                continue
            top, entry, wav_path = found
            shutil.copyfile(wav_path, run_dir / f"audio-{name}.wav")
            entry = dict(entry)
            entry["render_cache"] = "hit"
            hits[name] = (top, entry)
    misses = [n for n in names if n not in hits]
    rechecks = (
        pick_rechecks(
            [n for n in names if n in hits],
            recheck_probability(),
            random.Random(os.urandom(16)),
        )
        if hits
        else []
    )
    for name in rechecks:
        hits[name][1]["render_cache"] = "hit-rechecked"
    todo = misses + rechecks
    shards = (
        shard_names(todo, scenario_seconds(scn), jobs)
        if jobs > 1
        else ([list(todo)] if todo else [])
    )

    def launch(shard: list[str], d: pathlib.Path) -> pathlib.Path:
        (d / "logs").mkdir(parents=True, exist_ok=True)
        extra = ["--scenarios", scn]
        for name in shard:
            extra += ["--scenario", name]
        extra += ["--out", str(d / "report.json"), "--wav", str(d / "audio.wav")]
        for spec in peeks:
            extra += ["--peek", spec]
        actual_inputs = inputs(device)
        argv = run_args(binary, actual_inputs, extra)
        return _run_one(
            argv,
            run_env(digi_env.diag_env(parsed, d), syx=actual_inputs["firmware"]),
            d,
        )

    rendered: list[dict] = []
    entries: dict[str, dict] = {}
    if len(shards) == 1:
        dirs = [run_dir]
        outs = [launch(shards[0], run_dir)]
    elif shards:
        dirs = [run_dir / f"shard-{i}" for i in range(len(shards))]
        with (
            benchlock("shared", f"coupled-run x{len(shards)}"),
            concurrent.futures.ThreadPoolExecutor(len(shards)) as pool,
        ):
            futures = [
                pool.submit(launch, sh, d) for sh, d in zip(shards, dirs, strict=True)
            ]
            outs = [f.result() for f in futures]
    else:
        dirs, outs = [], []
    for shard, d in zip(shards, dirs, strict=True):
        for name in shard:
            wav = d / f"audio-{name}.wav"
            if not wav.exists() and len(shard) == 1:
                wav = d / "audio.wav"
            dst = run_dir / f"audio-{name}.wav"
            if wav.exists() and wav != dst:
                os.replace(wav, dst)
    rendered = [read_report(o) for o in outs]
    for rep in rendered:
        for sc in rep.get("scenarios", []):
            entries[sc["name"]] = sc
    if rendered:
        # Raises when a requested scenario is missing from the reports.
        merge_reports(rendered, todo)
    if use_cache:
        for rep in rendered:
            top = {k: v for k, v in rep.items() if k != "scenarios"}
            for name in misses:
                wav = run_dir / f"audio-{name}.wav"
                if (
                    name in keys
                    and entries.get(name) is not None
                    and name in {sc["name"] for sc in rep.get("scenarios", [])}
                    and wav.is_file()
                ):
                    cache.store(keys[name], top, entries[name], wav)
    top_report = (
        {k: v for k, v in rendered[0].items() if k != "scenarios"}
        if rendered
        else {k: v for k, v in next(iter(hits.values()))[0].items() if k != "scenarios"}
    )
    all_entries = {**entries, **{n: h[1] for n, h in hits.items()}}
    merged = {
        **top_report,
        "scenarios": [all_entries[n] for n in names],
        "render_cache": {
            "hits": len(hits),
            "rechecked": len(rechecks),
            "rendered": len(misses),
        },
    }
    (run_dir / "report.json").write_text(json.dumps(merged))
    print(
        f"render cache: {len(hits)} hit ({len(rechecks)} rechecked), "
        f"{len(misses)} rendered"
    )
    for name in rechecks:
        cached, fresh = hits[name][1], entries[name]
        a = _recheck_view(cached)
        b = _recheck_view(fresh)
        if a != b:
            raise RuntimeError(
                f"stale render cache for {name}: cached sha256 {a[0]} != fresh "
                f"{b[0]} (key {keys[name]}); clear it with `uv run python -m tools.emulator.coupled cache "
                "clear` and report the missing key input"
            )
    return merged


def run_parallel(binary: pathlib.Path, args) -> int:
    """`scenario --jobs N`: run_batch over the named scenarios (or all of the
    file's), then the usual summary. Exit 2 when a gate or pin failed."""
    if args.seconds is not None or args.extra or args.dump_dsp:
        raise SystemExit(
            "coupled: --jobs does not combine with --seconds/--dump-dsp/--"
        )
    device = getattr(args, "device", "dn2")
    scn = pathlib.Path(
        args.scenarios or ROOT / "tests" / "scenarios" / f"{device}.json"
    )
    names = args.names or list(scenario_seconds(scn))
    run_dir = (
        pathlib.Path(args.out).parent
        if args.out
        else digi_env.run_dir(
            run_kind([x for n in args.names for x in ("--scenario", n)])
        )
    )
    report = run_batch(
        device,
        scn,
        names,
        run_dir,
        peeks=args.peek or (),
        diag=args.diag,
        binary=binary,
        jobs=args.jobs,
        cache=render_cache.RenderCache(enabled=False)
        if getattr(args, "no_cache", False)
        else None,
    )
    if args.out and pathlib.Path(args.out).name != "report.json":
        pathlib.Path(args.out).write_text((run_dir / "report.json").read_text())
    summarize(pathlib.Path(args.out) if args.out else run_dir / "report.json")
    bad = [r["name"] for r in scenario_results(report) if r["ok"] is False]
    return 2 if bad else 0


def cmd_build(args) -> int:
    binary = build(getattr(args, "device", "dn2"))
    print(binary)
    return 0


def want_dump(args) -> None:
    """`--diag dump` needs the fast-dump build feature."""
    global FEATURES
    if any(n == "dump" for n, _ in digi_env.parse_diag(args.diag or "")):
        FEATURES += ",fast-dump"


def cmd_scenario(args) -> int:
    want_dump(args)
    if getattr(args, "check_generated", False):
        digi_env.check_generated(generated(getattr(args, "device", "dn2")))
    binary = ensure_binary(args.no_build, getattr(args, "device", "dn2"))
    if not args.names and not args.all:
        raise SystemExit("coupled: give scenario names or --all")
    # No --scenario means every scenario of the file; coupled-run exits 2 on
    # any expect_sha256 mismatch or failed gate.
    selection = [x for name in args.names for x in ("--scenario", name)]
    eligible = (
        args.seconds is None and not args.extra and not args.dump_dsp and not args.diag
    )
    if args.jobs > 1 or eligible:
        return run_parallel(binary, args)
    return run_and_report(binary, selection, args)


def cmd_cache(args) -> int:
    cache = render_cache.RenderCache()
    if args.action == "clear":
        print(f"removed {cache.clear()} entries")
    else:
        stats = cache.stats()
        print(f"{stats['entries']} entries, {stats['bytes'] / 1e6:.1f} MB")
    return 0


def cmd_replay(args) -> int:
    want_dump(args)
    binary = ensure_binary(args.no_build, getattr(args, "device", "dn2"))
    return run_and_report(binary, ["--replay", args.file], args)


def file_digest(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def bisect_run(binary, name, seconds, label, overrides) -> pathlib.Path:
    """Run one configuration, writing out/native/bisect/<name>/<label>.json."""
    out = BISECT_DIR / name / f"{label}.json"
    return bisect_run_to(binary, name, seconds, out, overrides)


def bisect_run_to(binary, name, seconds, out, overrides) -> pathlib.Path:
    extra = ["--scenario", name, "--out", str(out)]
    if seconds is not None:
        extra += ["--seconds", str(seconds)]
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        out.unlink()
    actual_inputs = inputs()
    done = subprocess.run(
        run_args(binary, actual_inputs, extra),
        cwd=ROOT,
        env=run_env(overrides, syx=actual_inputs["firmware"]),
        check=False,
    )
    if done.returncode == 1 or not out.is_file():
        raise SystemExit(f"coupled: run failed for {out.stem} ({done.returncode})")
    return out


def result_of(path: pathlib.Path, name: str) -> dict:
    for r in scenario_results(read_report(path)):
        if r["name"] == name:
            return r
    raise SystemExit(f"coupled: no scenario {name} in {path}")


def cmd_bisect(args) -> int:
    binary = ensure_binary(args.no_build, getattr(args, "device", "dn2"))
    name, seconds = args.name, args.seconds
    key = hashlib.sha256(
        "\0".join(
            [
                file_digest(binary),
                name,
                str(seconds),
                str(generated(getattr(args, "device", "dn2")).resolve()),
            ]
        ).encode()
    ).hexdigest()
    ref_path = BISECT_DIR / name / f"reference-{key[:16]}.json"
    if not ref_path.is_file():
        print("interpreter-only reference (slow, cached)", flush=True)
        bisect_run_to(
            binary,
            name,
            seconds,
            ref_path,
            {"SHARC_FAST_REGIONS": "", "SHARC_BLOCKTRANS": "", "SHARC_AOT": "0"},
        )
    ref = result_of(ref_path, name)

    fast = digi_env.fast_env()
    base = {
        "SHARC_FAST_REGIONS": fast.get("SHARC_FAST_REGIONS", ""),
        "SHARC_BLOCKTRANS": fast.get("SHARC_BLOCKTRANS", ""),
    }
    default_path = bisect_run(binary, name, seconds, "default", base)
    default = result_of(default_path, name)
    if default["sha256"] == ref["sha256"]:
        print("all tiers agree with the interpreter")
        return 0
    print(
        f"default differs from the reference ({ref['sha256']} vs {default['sha256']})"
    )

    suspects = []
    for label, overrides in bisect_configs(fast):
        path = bisect_run(binary, name, seconds, label, overrides)
        matches = result_of(path, name)["sha256"] == ref["sha256"]
        print(f"{label:<28} {'matches reference' if matches else 'differs'}")
        if matches:
            suspects.append(label)
    print("suspects (hash equals the reference): " + (", ".join(suspects) or "none"))
    index = first_divergence(default["windows"], ref["windows"])
    if index is None:
        print("window_rms_100ms: no divergence above 1e-6 relative")
    else:
        print(f"first diverging 100 ms window: {index} (t = {index / 10:.1f} s)")
    return 1


def cmd_test_quick(args) -> int:
    gen = generated()
    digi_env.check_generated(gen)
    nogen = native_target.target_dir("")
    prod = native_target.target_dir(gen, feature_variant())
    base_env = digi_env.rust_env()
    base_env.pop("SHARC_GEN_DIR", None)
    steps = [
        (
            "packages/emulator/live",
            base_env,
            ["--manifest-path", "packages/emulator/live/Cargo.toml", "--lib",
             "--target-dir", str(nogen)],
        ),
        (
            "packages/emulator/boot",
            base_env,
            ["--manifest-path", "packages/emulator/boot/Cargo.toml", "--features", "play",
             "--lib", "--target-dir", str(nogen)],
        ),
        (
            "packages/emulator/coupled",
            {**base_env, "SHARC_GEN_DIR": str(gen)},
            ["--manifest-path", "packages/emulator/coupled/Cargo.toml",
             "--features", FEATURES, "--lib", "--bins",
             "--target-dir", str(prod)],
        ),
    ]  # fmt: skip
    for label, env, tail in steps:
        command = cargo(["test", "--release", "--locked", *tail])
        with benchlock("shared", f"cargo test {label}"):
            done = subprocess.run(command, cwd=ROOT, env=env, check=False)
        if done.returncode != 0:
            return done.returncode
    return 0


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = top.add_subparsers(dest="command", required=True)

    def add(name: str, func, help_: str) -> argparse.ArgumentParser:
        p = sub.add_parser(name, help=help_)
        p.set_defaults(func=func)
        return p

    def run_flags(p: argparse.ArgumentParser) -> None:
        p.add_argument("--no-build", action="store_true", help="skip the cargo build")
        p.add_argument(
            "--dt2-card",
            action="store_true",
            help="DT2: the one-sample card (tools/emulator/dt2_sample_check.py) and its "
            "ready state; DIGI_DT2_CARD=1 does the same",
        )
        p.add_argument(
            "--wav", action="store_true", help="write audio.wav next to the report"
        )
        p.add_argument(
            "--out",
            help="report JSON (default out/runs/<scenario>/<stamp>/report.json)",
        )
        p.add_argument(
            "--diag",
            metavar="LIST",
            help="diagnostics into <run>/logs/: "
            + ",".join(digi_env.DIAG)
            + " (docs/INSTRUMENTATION.md)",
        )

    p = add("build", cmd_build, "build coupled-run")
    p.add_argument("--device", choices=["dn2", "dt2"], default="dn2")
    p = add("scenario", cmd_scenario, "run named scenarios")
    p.add_argument("names", nargs="*", metavar="NAME")
    p.add_argument("--all", action="store_true", help="run every scenario of the file")
    p.add_argument("--device", choices=["dn2", "dt2"], default="dn2")
    p.add_argument(
        "--check-generated",
        action="store_true",
        help="fail before rendering if the generated core is stale or missing",
    )
    p.add_argument("--seconds", type=float)
    p.add_argument(
        "--jobs",
        type=int,
        default=1,
        metavar="N",
        help="render over N concurrent coupled-run processes (PCM identical; "
        "default 1)",
    )
    p.add_argument(
        "--scenarios", metavar="FILE", help="scenario file (replaces the device's)"
    )
    p.add_argument(
        "--peek",
        action="append",
        metavar="NAME=SPEC",
        help="guest read at the end: ADDR:LEN or *ADDR+OFF:LEN (repeatable)",
    )
    p.add_argument(
        "--dump-dsp",
        action="store_true",
        help="write the DSP state at the end to <run>/dsp-<scenario>.bin (tools/emulator/dspdiff.py)",
    )
    p.add_argument(
        "--no-cache", action="store_true", help="render without the render cache"
    )
    run_flags(p)
    p = add("replay", cmd_replay, "replay a scenario or diagnostics export")
    p.add_argument("file", metavar="FILE")
    run_flags(p)
    p = add("bisect", cmd_bisect, "find which DSP tier changes a scenario's PCM")
    p.add_argument("name", metavar="NAME")
    p.add_argument("--seconds", type=float)
    p.add_argument("--no-build", action="store_true", help="skip the cargo build")
    p = add("cache", cmd_cache, "render cache: stats or clear")
    p.add_argument("action", choices=["stats", "clear"])
    add("test-quick", cmd_test_quick, "fast cargo tests with the pinned toolchain")
    return top


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    extra: list[str] = []
    if "--" in argv:
        cut = argv.index("--")
        argv, extra = argv[:cut], argv[cut + 1 :]
    args = parser().parse_args(argv)
    args.extra = extra
    if getattr(args, "dt2_card", False) or os.environ.get("DIGI_DT2_CARD"):
        enable_dt2_card()
    if extra and args.command not in ("scenario", "replay"):
        raise SystemExit(
            "coupled: extra arguments after -- only apply to scenario/replay"
        )
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
