#!/usr/bin/env python3
"""Profile-guided optimisation for the coupled DN2 native desktop build.

State lives under ``out/native/pgo/dn2/<host-triple>/`` (ignored by git):

    manifest.json          what the current profile was trained from
    profiles/<sha>.profdata  merged profile, content addressed
    instrument-target/     cargo target dir of the instrumented test build
    target/                cargo target dir of the profile-use app build
    control-target/        plain release test build (benchmark control)
    bench-<time>/          copied binaries, logs, bench.tsv, summary.json

Commands: ``train``, ``check``, ``plan``, ``bench --pairs N``,
``bench-bins A B --pairs N`` (two prebuilt workload test binaries).

The training workload is the ignored test ``coupled_ready_exactness``. It
needs the private ready fixtures and a generated DSP core (``SHARC_GEN_DIR``).
The launcher (``tools/native_emu.sh``) imports :func:`launch_plan`.

The toolchain must be the rustup 1.98.1 distribution: a Homebrew rustc has the
same version string but different LLVM, and cannot read these profiles.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = "packages/desktop/src-tauri/Cargo.toml"
DEFAULT_GENERATED = ROOT / "out" / "native" / "dn2-audio" / "gen"
READY = ROOT / "snapshots" / "dn2-audio-ready-2026-10-03"
TOOLCHAIN = "1.98.1"
TRIPLE = "aarch64-apple-darwin"
FEATURES = "coupled-audio"
TEST_NAME = "desktop_runtime::coupled::tests::coupled_ready_exactness"
EXPECTED_FRAMES = 1139
TRAIN_RUNS = 4
FLAGS_TEMPLATE = "-Cprofile-use={profdata} -Cllvm-args=-pgo-warn-missing-function"
SKIP_DIRS = {"target", ".git", "node_modules"}
SKIP_FILES = {".DS_Store"}


def supported_host() -> bool:
    return sys.platform == "darwin" and platform.machine() == "arm64"


def state_dir(triple: str = TRIPLE) -> pathlib.Path:
    return ROOT / "out" / "native" / "pgo" / "dn2" / triple


def toolchain_dir() -> pathlib.Path:
    home = pathlib.Path(
        os.environ.get("RUSTUP_HOME") or pathlib.Path.home() / ".rustup"
    )
    return home / "toolchains" / f"{TOOLCHAIN}-{TRIPLE}"


def llvm_profdata() -> pathlib.Path:
    return toolchain_dir() / "lib" / "rustlib" / TRIPLE / "bin" / "llvm-profdata"


def generated_dir(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> pathlib.Path:
    environ = os.environ if environ is None else environ
    configured = environ.get("SHARC_GEN_DIR")
    return pathlib.Path(configured) if configured else DEFAULT_GENERATED


def rustc_version() -> str:
    rustc = toolchain_dir() / "bin" / "rustc"
    return subprocess.run(
        [str(rustc), "-vV"], capture_output=True, text=True, check=True
    ).stdout


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


class FileHasher:
    """sha256 of files, memoised on (size, mtime_ns) in a JSON cache."""

    def __init__(self, cache_path: pathlib.Path) -> None:
        self.cache_path = cache_path
        try:
            self.old: dict[str, list[object]] = json.loads(cache_path.read_text())
        except (OSError, ValueError):
            self.old = {}
        self.new: dict[str, list[object]] = {}

    def digest(self, path: pathlib.Path) -> str:
        st = path.stat()
        key = str(path)
        hit = self.old.get(key)
        if hit and hit[0] == st.st_size and hit[1] == st.st_mtime_ns:
            self.new[key] = hit
            return str(hit[2])
        sha = sha256_file(path)
        self.new[key] = [st.st_size, st.st_mtime_ns, sha]
        return sha

    def save(self) -> None:
        if self.new == self.old:
            return
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.cache_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.new))
        tmp.replace(self.cache_path)


def tree_files(top: pathlib.Path) -> list[pathlib.Path]:
    found: list[pathlib.Path] = []
    for dirpath, dirnames, filenames in os.walk(top):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        found.extend(
            pathlib.Path(dirpath) / n for n in sorted(filenames) if n not in SKIP_FILES
        )
    return found


def tree_digest(
    hasher: FileHasher, top: pathlib.Path, base: pathlib.Path, skip_top: set[str]
) -> tuple[str, int]:
    """Digest of (relative path, sha256) over a tree; skip_top names top-level dirs to leave out."""
    outer = hashlib.sha256()
    count = 0
    for path in tree_files(top):
        rel = path.relative_to(top)
        if rel.parts[0] in skip_top and len(rel.parts) > 1:
            continue
        outer.update(f"{path.relative_to(base)}\0{hasher.digest(path)}\n".encode())
        count += 1
    return outer.hexdigest(), count


def local_package_dirs() -> list[str]:
    """Repo-relative directories of the local crates in the coupled build graph."""
    out = subprocess.run(
        [
            "rustup",
            "run",
            TOOLCHAIN,
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            MANIFEST_PATH,
            "--features",
            FEATURES,
            "--filter-platform",
            TRIPLE,
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    dirs = set()
    for package in json.loads(out)["packages"]:
        if package["source"] is None:
            dirs.add(
                str(pathlib.Path(package["manifest_path"]).parent.relative_to(ROOT))
            )
    return sorted(dirs)


def compute_components(
    packages: list[str], gen: pathlib.Path, hasher: FileHasher
) -> tuple[dict[str, str], int]:
    outer = hashlib.sha256()
    files = 0
    for rel in packages:
        digest, count = tree_digest(hasher, ROOT / rel, ROOT, {"gen"})
        outer.update(f"{rel}\0{digest}\n".encode())
        files += count
    gen_digest, gen_count = (
        tree_digest(hasher, gen, gen, set()) if gen.is_dir() else ("absent", 0)
    )
    components = {
        "sources": outer.hexdigest(),
        "generated": gen_digest,
        "rustc": hashlib.sha256(rustc_version().encode()).hexdigest(),
        "triple": TRIPLE,
        "features": FEATURES,
        "flags_template": FLAGS_TEMPLATE,
    }
    return components, files + gen_count


def key_of(components: dict[str, str]) -> str:
    return hashlib.sha256(json.dumps(components, sort_keys=True).encode()).hexdigest()


def read_manifest() -> dict[str, object] | None:
    try:
        return json.loads((state_dir() / "manifest.json").read_text())
    except (OSError, ValueError):
        return None


def check(gen: pathlib.Path | None = None) -> tuple[str, str]:
    """Return (status, detail): status is ok, stale or missing."""
    manifest = read_manifest()
    if manifest is None:
        return "missing", "no manifest"
    profdata = pathlib.Path(str(manifest["profdata"]["path"]))  # type: ignore[index]
    if not profdata.is_file():
        return "missing", f"profile file {profdata} is gone"
    if not toolchain_dir().is_dir():
        return "stale", f"rustup toolchain {TOOLCHAIN} is not installed"
    hasher = FileHasher(state_dir() / "hash-cache.json")
    components, _ = compute_components(
        list(manifest["packages"]), gen or generated_dir(), hasher
    )  # type: ignore[call-overload]
    hasher.save()
    old = manifest["components"]
    changed = [name for name, value in components.items() if old.get(name) != value]  # type: ignore[attr-defined]
    if changed:
        return "stale", "changed: " + ", ".join(changed)
    return "ok", str(profdata)


def profile_use_flags(profdata: str) -> str:
    return FLAGS_TEMPLATE.format(profdata=profdata)


@dataclasses.dataclass
class LaunchPlan:
    prefix: list[str]
    cargo_args: list[str]
    env: dict[str, str]


def launch_plan(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> tuple[LaunchPlan | None, str]:
    """Decide whether the launcher can use the PGO build; returns (plan, notice).

    An empty notice means stay silent (opted out or unsupported host).
    """
    environ = os.environ if environ is None else environ
    if environ.get("DIGI_EMU_PGO") == "0" or not supported_host():
        return None, ""
    if environ.get("RUSTFLAGS") or environ.get("CARGO_ENCODED_RUSTFLAGS"):
        return None, "native PGO: skipped (RUSTFLAGS is set)"
    status, detail = check(generated_dir(environ))
    if status != "ok":
        suffix = f" ({detail})" if status == "stale" else ""
        return None, f"native PGO: {status}{suffix}; run tools/native_pgo.py train"
    plan = LaunchPlan(
        prefix=["rustup", "run", TOOLCHAIN],
        cargo_args=["--target", TRIPLE, "--target-dir", str(state_dir() / "target")],
        env={"RUSTFLAGS": profile_use_flags(detail)},
    )
    return plan, "native PGO: using profile " + pathlib.Path(detail).name


# --- training and benchmarking -------------------------------------------------


def base_env(gen: pathlib.Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")
    }
    env["SHARC_GEN_DIR"] = str(gen)
    return env


def run_env(deps_dir: pathlib.Path) -> dict[str, str]:
    return {
        "DYLD_LIBRARY_PATH": str(deps_dir),
        "NOTE_EVENTS": "trig",
        "DIGI_COUPLED_FIXTURES": os.environ.get("DIGI_COUPLED_FIXTURES", str(READY)),
        "DIGI_COUPLED_SYX": os.environ.get(
            "DIGI_COUPLED_SYX", str(ROOT / "Digitone_II_OS1.11.syx")
        ),
        "DN2_PROFILE_LINK": "1",
    }


def build_tests(
    target_dir: pathlib.Path,
    rustflags: str | None,
    gen: pathlib.Path,
    log: pathlib.Path,
) -> pathlib.Path:
    env = base_env(gen)
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    command = [
        "rustup",
        "run",
        TOOLCHAIN,
        "cargo",
        "test",
        "--release",
        "--no-run",
        "--locked",
        "--manifest-path",
        MANIFEST_PATH,
        "--features",
        FEATURES,
        "--target",
        TRIPLE,
        "--target-dir",
        str(target_dir),
        "--message-format=json",
    ]
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w") as handle:
        proc = subprocess.run(
            command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=handle, text=True
        )
    if proc.returncode:
        tail = "".join(log.read_text().splitlines(keepends=True)[-30:])
        raise SystemExit(f"test build failed (see {log}):\n{tail}")
    found = []
    for line in proc.stdout.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("executable")
            and message.get("profile", {}).get("test")
            and "digiemu-desktop" in message.get("package_id", "")
        ):
            found.append(pathlib.Path(message["executable"]))
    if len(found) != 1:
        raise SystemExit(f"expected one digiemu-desktop test binary, found {found}")
    return found[0]


@dataclasses.dataclass
class Result:
    elapsed: float
    dsp_ns: int
    frames: int
    user: float
    sys: float
    real: float
    line: str


def run_workload(
    binary: pathlib.Path, log: pathlib.Path, extra_env: dict[str, str] | None = None
) -> Result:
    """Run the workload under /usr/bin/time and enforce the gates."""
    env_pairs = {**run_env(binary.parent), **(extra_env or {})}
    command = [
        "/usr/bin/time",
        "-lp",
        "env",
        *(f"{k}={v}" for k, v in env_pairs.items()),
        str(binary),
        TEST_NAME,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]
    proc = subprocess.run(
        command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    log.write_text(proc.stdout)
    if proc.returncode:
        raise SystemExit(
            f"workload failed (exit {proc.returncode}), see {log}:\n"
            + "\n".join(proc.stdout.splitlines()[-25:])
        )
    text = proc.stdout

    def grab(pattern: str) -> str:
        found = re.search(pattern, text)
        if not found:
            raise SystemExit(f"gate: {pattern!r} not found in {log}")
        return found.group(1)

    result = Result(
        elapsed=float(grab(r"coupled_workload_elapsed_seconds=([0-9.]+)")),
        dsp_ns=int(grab(r"worker_dsp_ns: (\d+)")),
        frames=int(grab(r"frames_completed: (\d+)")),
        user=float(grab(r"(?m)^user\s+([0-9.]+)")),
        sys=float(grab(r"(?m)^sys\s+([0-9.]+)")),
        real=float(grab(r"(?m)^real\s+([0-9.]+)")),
        line=next(
            line
            for line in text.splitlines()
            if line.startswith("native_audio_link_timing_window=")
        ),
    )
    if result.frames != EXPECTED_FRAMES:
        raise SystemExit(
            f"gate: frames_completed {result.frames} != {EXPECTED_FRAMES} in {log}"
        )
    return result


def require_inputs(gen: pathlib.Path) -> None:
    if not supported_host():
        raise SystemExit("native PGO is only set up for aarch64 macOS")
    if not toolchain_dir().is_dir():
        raise SystemExit(f"missing rustup toolchain {toolchain_dir()}")
    if not gen.is_dir():
        raise SystemExit(f"generated DN2 core not found: {gen} (set SHARC_GEN_DIR)")
    for path in (
        run_env(pathlib.Path("."))["DIGI_COUPLED_FIXTURES"],
        run_env(pathlib.Path("."))["DIGI_COUPLED_SYX"],
    ):
        if not pathlib.Path(path).exists():
            raise SystemExit(f"missing private input {path}")


def cmd_train(args: argparse.Namespace) -> None:
    gen = generated_dir()
    require_inputs(gen)
    state = state_dir()
    timings: dict[str, float] = {}
    started = time.monotonic()

    def phase(name: str, since: float) -> float:
        now = time.monotonic()
        timings[name] = round(now - since, 2)
        print(f"phase {name}: {timings[name]} s", flush=True)
        return now

    t = time.monotonic()
    packages = local_package_dirs()
    hasher = FileHasher(state / "hash-cache.json")
    components, nfiles = compute_components(packages, gen, hasher)
    hasher.save()
    key = key_of(components)
    t = phase("key", t)

    raw = state / f"raw-{key[:16]}"
    shutil.rmtree(raw, ignore_errors=True)
    raw.mkdir(parents=True)
    binary = build_tests(
        state / "instrument-target",
        f"-Cprofile-generate={raw}",
        gen,
        state / "instrument-build.log",
    )
    t = phase("instrumented-build", t)

    gates = []
    for number in range(1, args.runs + 1):
        result = run_workload(
            binary,
            state / f"train-{number}.log",
            {"LLVM_PROFILE_FILE": str(raw / f"train-{number}-%m.profraw")},
        )
        gates.append(result.line)
        print(
            f"train run {number}: elapsed {result.elapsed:.3f} s frames {result.frames}",
            flush=True,
        )
    t = phase("training-runs", t)

    merged = raw / "merged.profdata"
    raws = sorted(str(p) for p in raw.glob("*.profraw"))
    if not raws:
        raise SystemExit("training produced no .profraw files")
    merge = [str(llvm_profdata()), "merge", "-o", str(merged), *raws]
    subprocess.run(merge, check=True)
    digest = sha256_file(merged)
    profiles = state / "profiles"
    profiles.mkdir(parents=True, exist_ok=True)
    final = profiles / f"{digest}.profdata"
    shutil.move(str(merged), final)
    shutil.rmtree(raw, ignore_errors=True)
    t = phase("merge", t)

    manifest = {
        "key": key,
        "components": components,
        "packages": packages,
        "generated_dir": str(gen),
        "file_count": nfiles,
        "rustc_vV": rustc_version(),
        "sysroot": str(toolchain_dir()),
        "triple": TRIPLE,
        "features": FEATURES,
        "profdata": {
            "path": str(final),
            "sha256": digest,
            "size": final.stat().st_size,
        },
        "train_commands": {
            "build_rustflags": f"-Cprofile-generate={raw}",
            "runs": args.runs,
            "merge": merge[:4] + ["<raw>..."],
            "test": TEST_NAME,
        },
        "gate_lines": gates,
        "phase_seconds": timings,
        "created": datetime.datetime.now(datetime.UTC).isoformat(),
    }
    (state / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"trained {final} ({final.stat().st_size} bytes) key {key[:16]} in {time.monotonic() - started:.1f} s"
    )


def cmd_check(_: argparse.Namespace) -> int:
    began = time.monotonic()
    status, detail = check()
    print(f"{status} {detail}" if status != "missing" else f"missing ({detail})")
    print(f"check took {time.monotonic() - began:.3f} s", file=sys.stderr)
    return 0 if status == "ok" else 1


def cmd_plan(_: argparse.Namespace) -> int:
    plan, notice = launch_plan()
    print(notice or "native PGO: not used")
    if plan:
        print(
            json.dumps(
                {"prefix": plan.prefix, "cargo_args": plan.cargo_args, "env": plan.env},
                indent=2,
            )
        )
    return 0 if plan else 1


def median(values: list[float]) -> float:
    return statistics.median(values)


def cmd_bench(args: argparse.Namespace) -> None:
    gen = generated_dir()
    require_inputs(gen)
    status, detail = check(gen)
    if status != "ok":
        raise SystemExit(f"profile is not current: {status} {detail}; run train first")
    state = state_dir()
    out = state / ("bench-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    out.mkdir(parents=True)
    t = time.monotonic()
    control_src = build_tests(
        state / "control-target", None, gen, out / "control-build.log"
    )
    print(f"control build {time.monotonic() - t:.1f} s", flush=True)
    t = time.monotonic()
    use_src = build_tests(
        state / "target", profile_use_flags(detail), gen, out / "use-build.log"
    )
    print(f"profile-use build {time.monotonic() - t:.1f} s", flush=True)
    binaries = {}
    hashes = {}
    for label, src in (("control", control_src), ("use", use_src)):
        dst = out / f"{label}-{src.name}"
        shutil.copy2(src, dst)
        binaries[label] = (dst, src.parent)
        hashes[label] = sha256_file(dst)
        print(f"{label} binary sha256 {hashes[label]}")
    paired_bench(
        out,
        binaries,
        "control",
        "use",
        args.pairs,
        {"binary_sha256": hashes, "profdata": detail},
    )


def paired_bench(
    out: pathlib.Path,
    binaries: dict[str, tuple[pathlib.Path, pathlib.Path]],
    label_a: str,
    label_b: str,
    pairs: int,
    extra: dict[str, object],
) -> None:
    """Alternate A,B / B,A runs of the workload; write bench.tsv, summary.json."""
    rows = []
    for pair in range(1, pairs + 1):
        order = [label_a, label_b] if pair % 2 else [label_b, label_a]
        results = {}
        for label in order:
            dst, deps = binaries[label]
            results[label] = run_workload(
                dst,
                out / f"run-{pair:02d}-{label}.log",
                {"DYLD_LIBRARY_PATH": str(deps)},
            )
        for label in (label_a, label_b):
            r = results[label]
            rows.append(
                (pair, label, r.elapsed, r.dsp_ns, r.frames, r.real, r.user, r.sys)
            )
        print(
            f"pair {pair} order {','.join(order)}: ratio wall {results[label_b].elapsed / results[label_a].elapsed:.3f}",
            flush=True,
        )
    with (out / "bench.tsv").open("w") as handle:
        handle.write(
            "pair\tlabel\telapsed_s\tworker_dsp_ns\tframes\treal_s\tuser_s\tsys_s\n"
        )
        for row in rows:
            handle.write("\t".join(str(v) for v in row) + "\n")

    def ratios(index: int | tuple[int, int]) -> list[float]:
        def value(row: tuple[object, ...]) -> float:
            if isinstance(index, tuple):
                return float(row[index[0]]) + float(row[index[1]])  # type: ignore[arg-type]
            return float(row[index])  # type: ignore[arg-type]

        out_ratios = []
        for pair in range(1, pairs + 1):
            by = {row[1]: value(row) for row in rows if row[0] == pair}
            out_ratios.append(by[label_b] / by[label_a])
        return out_ratios

    summary = {
        "pairs": pairs,
        "order": f"{label_a},{label_b} then {label_b},{label_a} alternating",
        **extra,
        f"median_ratio_{label_b}_over_{label_a}": {
            "workload_wall": median(ratios(2)),
            "worker_dsp": median(ratios(3)),
            "user_plus_sys": median(ratios((6, 7))),
        },
        "ratios": {
            "workload_wall": ratios(2),
            "worker_dsp": ratios(3),
            "user_plus_sys": ratios((6, 7)),
        },
        "rustc_vV": rustc_version(),
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"pair  {label_a}_wall  {label_b}_wall  {label_a}_dsp_ns  {label_b}_dsp_ns")
    for pair in range(1, pairs + 1):
        c = next(r for r in rows if r[0] == pair and r[1] == label_a)
        u = next(r for r in rows if r[0] == pair and r[1] == label_b)
        print(f"{pair:>4}  {c[2]:>12.3f}  {u[2]:>8.3f}  {c[3]:>14}  {u[3]:>10}")
    for name, value in summary[f"median_ratio_{label_b}_over_{label_a}"].items():  # type: ignore[attr-defined]
        print(f"median {label_b}/{label_a} {name}: {value:.3f}")
    print(f"results in {out}")


def cmd_bench_bins(args: argparse.Namespace) -> None:
    """Paired comparison of two prebuilt workload test binaries."""
    gen = generated_dir()
    require_inputs(gen)
    state = state_dir()
    out = state / ("bench-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    out.mkdir(parents=True)
    labels = {"a": args.label_a, "b": args.label_b}
    if labels["a"] == labels["b"]:
        raise SystemExit("--label-a and --label-b must differ")
    binaries = {}
    hashes = {}
    for key, source in (("a", args.a), ("b", args.b)):
        src = pathlib.Path(source).resolve()
        if not src.is_file():
            raise SystemExit(f"not a file: {src}")
        label = labels[key]
        dst = out / f"{label}-{src.name}"
        shutil.copy2(src, dst)
        binaries[label] = (dst, src.parent)
        hashes[label] = sha256_file(dst)
        print(f"{label} binary sha256 {hashes[label]} ({src})")
    paired_bench(
        out,
        binaries,
        labels["a"],
        labels["b"],
        args.pairs,
        {"binary_sha256": hashes, "sources": [args.a, args.b]},
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    train = sub.add_parser(
        "train", help="build instrumented, run the workload, merge a profile"
    )
    train.add_argument("--runs", type=int, default=TRAIN_RUNS)
    sub.add_parser("check", help="print ok/stale/missing for the current profile")
    sub.add_parser("plan", help="show what the launcher would do")
    bench = sub.add_parser("bench", help="paired control vs profile-use benchmark")
    bench.add_argument("--pairs", type=int, default=5)
    bins = sub.add_parser(
        "bench-bins", help="paired benchmark of two prebuilt workload test binaries"
    )
    bins.add_argument("a", help="baseline test binary")
    bins.add_argument("b", help="test binary compared against it")
    bins.add_argument("--pairs", type=int, default=5)
    bins.add_argument("--label-a", default="a")
    bins.add_argument("--label-b", default="b")
    args = parser.parse_args(argv)
    if args.command == "train":
        cmd_train(args)
    elif args.command == "bench":
        cmd_bench(args)
    elif args.command == "bench-bins":
        cmd_bench_bins(args)
    elif args.command == "check":
        return cmd_check(args)
    else:
        return cmd_plan(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
