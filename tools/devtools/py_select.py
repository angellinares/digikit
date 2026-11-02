"""Python test selection and checks of the python-tests gate step.

    uv run python -m tools.devtools.py_select list --base main [--files F...]
    uv run python -m tools.devtools.py_select run --base main [--files F...] [--out DIR]
    uv run python -m tools.devtools.py_select checks

`list` prints the selected test files with the reason. `run` runs them (one pytest process per file) and
fails when a file failed or no test passed; nothing selected is a pass. `checks` runs the PY_CHECKS commands
(lint, format, types, layers, generated sources). Without --files the files are those changed since the
merge base with BASE.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

from tools import python_env
from tools.devtools import verify_budget

ROOT = Path(__file__).resolve().parent.parent.parent
PY = ["uv", "run", "python"]
# python-tests always runs PY_GATE (the staleness checks: built module objects and port files match their
# sources) and PY_CHECKS (lint, format, types, paths). A suite of PY_SUITES, (name, trigger prefixes, test paths), runs when a
# changed file starts with one of its prefixes (all suites with --full); a suite with a test directory
# triggers only on files that are not Python modules of tools/ or tests/, which select the tests that
# reach them in the import graph instead (changed_tests.gate_tests). Test paths may be globs or
# directories; one that matches nothing is an error.
PY_GATE = ("tests/patchkit/test_pk_current.py",)
# Plain commands python-tests runs as sub-checks (label "python-tests:NAME"): lint, format, types, generated sources.
# ruff and mypy read their file lists from pyproject.toml.
PY_CHECKS: tuple[tuple[str, list[str]], ...] = (
    ("ruff", [sys.executable, "-m", "ruff", "check", "--no-cache"]),
    ("ruff-format", [sys.executable, "-m", "ruff", "format", "--check", "--no-cache"]),
    ("mypy", [sys.executable, "-m", "mypy"]),
    # the module layering (docs/ARCHITECTURE.md): reads manifests, sources and built objects, under a second
    ("layercheck", [sys.executable, "-m", "tools.devtools.layercheck"]),
    # the generated sources match sdk/sdk.toml, devices/*.toml and abi.toml (mise run gen:modules)
    ("gen-check", [sys.executable, "-m", "tools.patchkit.patchkit", "gen", "--check"]),
)
PY_SUITES: tuple[tuple[str, tuple[str, ...], tuple[str, ...]], ...] = (
    (
        "patchkit",
        (
            "packages/patchkit/",
            "packages/dkneeds/",
            "devices/ports/",
            "tools/patchkit/pk_",
            "tools/patchkit/modroots.py",
            "tools/patchkit/port_resolve.py",
            "tools/patchkit/patchkit.py",
            "tests/patchkit/",
        ),
        ("tests/patchkit",),
    ),
    (
        "machine-conformance",
        (
            "tools/patchkit/pk_conform.py",
            "tools/patchkit/pk_modcheck.py",
            "tools/patchkit/pk_sharc.py",
            "tests/support/machine_rig.py",
            "tests/support/conform_worker.py",
            "tests/support/dsp_voice_rig.py",
            "tests/support/fx_rig.py",
            "tests/support/fx_conform_worker.py",
            "tests/support/fx_effects.s",
            "tests/modules/",
        ),
        ("tests/modules",),
    ),
    (
        # the tests inside the modules (modules/<group>/<name>/tests/test_*.py): found by globbing, so a new module test needs no entry;
        # a change in a module selects its own tests through verify_route.module_fine
        "module-tests",
        (),
        ("modules/*/*/tests/test_*.py",),
    ),
    (
        "digikit-boot",
        (
            "packages/patchkit/src/dkm.rs",
            "packages/patchkit/src/register.rs",
            "packages/emulator/plusdrive/",
            "tools/patchkit/pk_dkrt",
            "tools/patchkit/pk_digikit_",
            "tests/digikit/",
        ),
        ("tests/digikit",),
    ),
    (
        # the SHARC core and its generator (CORE_FINGERPRINT); in PY_GATE until 2026-10-21, so --full keeps them
        "sharc-core",
        (
            "tools/sharc_dev/sharc_core/",
            "tools/sharc_dev/sharc_transpile/",
            "tools/sharc_dev/sharc_transpile_infer.py",
            "tools/sharc_dev/sharc_rsgen.py",
        ),
        (
            "tests/sharc/integration/test_sharc_golden.py",
            "tests/sharc/semantics/test_sharc_denormal.py",
            "tests/sharc/generation/test_sharc_rsgen_density.py",
        ),
    ),
    (
        "control-client",
        ("tools/control/digi_client.py", "packages/emulator/control/"),
        ("tests/control",),
    ),
    (
        "devtools",
        (
            "tools/devtools/verify.py",
            "tools/devtools/py_select.py",
            "tools/devtools/fx_tier.py",
            "tools/devtools/verify_route.py",
            "tools/devtools/changed_tests.py",
            "tools/devtools/agent_guard.py",
            ".claude/settings.json",
            "tools/devtools/verify_budget.py",
            "tools/devtools/verify_build.py",
            "tools/devtools/rust_ws.py",
            "tools/patchkit/pk_proofs.py",
            "tools/devtools/benchlock.py",
            "tools/devtools/test_outcomes.py",
            "tools/devtools/layercheck.py",
        ),
        (
            "tests/devtools/test_layercheck.py",
            "tests/devtools/test_verify_budget.py",
            "tests/devtools/test_agent_guard.py",
            "tests/devtools/test_test_outcomes.py",
            "tests/devtools/test_benchlock.py",
        ),
    ),
    (
        "launcher",
        (
            "tools/emulator/native_emu.py",
            "tools/emulator/native_pgo.py",
            "tools/emulator/native_target.py",
            "tools/emulator/coupled.py",
            "tools/emulator/digi_env.py",
        ),
        (
            "tests/emulator/test_native_emu_launcher.py",
            "tests/emulator/test_native_pgo.py",
            "tests/emulator/test_native_target.py",
            "tests/emulator/test_coupled_tool.py",
            "tests/emulator/test_gen_fingerprint.py",
        ),
    ),
)


def git_lines(*args: str) -> list[str]:
    out = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
    )
    return [line for line in out.stdout.splitlines() if line]


def merge_base(base: str, head: str | None) -> str:
    if head is None:
        return git_lines("merge-base", base, "HEAD")[0] if base != "HEAD" else "HEAD"
    return git_lines("merge-base", base, head)[0]


def changed_files(base: str, head: str | None = None) -> list[str]:
    """Files changed since the merge base with BASE, committed or not, plus untracked files.
    With HEAD (a revision): the commits BASE..HEAD only, which explains a historical range
    (the working tree still decides what the files mean)."""
    since = merge_base(base, head)
    if head is not None:
        return sorted(git_lines("diff", "--name-only", since, head))
    files = set(git_lines("diff", "--name-only", since))
    files |= set(
        untracked_changes(git_lines("ls-files", "--others", "--exclude-standard"))
    )
    return sorted(files)


def untracked_changes(paths: list[str]) -> list[str]:
    """The untracked PATHS inside a directory. A file at the root (a log, a note) is no change: it would
    take the fallback route and the full smoke."""
    kept = [p for p in paths if "/" in p]
    for p in paths:
        if "/" not in p:
            print(
                f"verify: ignored untracked file at the root: {p} (add it to git to test it)",
                file=sys.stderr,
            )
    return kept


def is_python_source(path: str) -> bool:
    """A .py file of tools/ or tests/ (python-tests routes it by the import graph)."""
    return path.endswith(".py") and path.startswith(("tools/", "tests/"))


def is_module_test_dir(path: str) -> bool:
    """Whether PATH is inside a module's own tests/ directory (modules/<group>/<name>/tests/)."""
    parts = path.split("/")
    return len(parts) > 4 and parts[0] == "modules" and parts[3] == "tests"


def is_changed_test(path: str) -> bool:
    """A changed test file that python-tests runs itself."""
    name = path.rsplit("/", 1)[-1]
    return (
        (path.startswith("tests/") or is_module_test_dir(path))
        and name.startswith("test_")
        and name.endswith(".py")
        and (ROOT / path).is_file()
    )


def resolve_tests(paths: tuple[str, ...]) -> list[str]:
    """Repo-relative test paths for PATHS (globs and directories); a name that matches
    nothing is a registry error, so a rename cannot drop a suite silently."""
    found: list[str] = []
    for pattern in paths:
        if any(c in pattern for c in "*?["):
            hits = sorted(ROOT.glob(pattern))
        else:
            hits = [ROOT / pattern] if (ROOT / pattern).exists() else []
        if not hits and pattern.startswith("modules/"):
            continue  # module-owned tests: there are none until a module has one
        if not hits:
            raise SystemExit(f"verify: PY_SUITES entry matches nothing: {pattern}")
        found += [hit.relative_to(ROOT).as_posix() for hit in hits]
    return found


def support_users(path: str) -> list[str]:
    """The test files that import the changed helper module tests/support/NAME.py."""
    if not (path.startswith("tests/support/") and path.endswith(".py")):
        return []
    module = "support." + path.removeprefix("tests/support/").removesuffix(".py")
    tests = sorted((ROOT / "tests").rglob("test_*.py"))
    users = [t for t in tests if module in t.read_text(encoding="utf-8")]
    return [t.relative_to(ROOT).as_posix() for t in users]


def python_test_plan(files: list[str], full: bool) -> list[tuple[str, str]]:
    """(pytest path, source) in run order, without duplicates: the gate files, the
    suites a changed file triggers (all of them when FULL), changed test files, then
    the tests that import a changed tests/support/ module."""
    from tools.devtools import changed_tests  # it imports this module

    plan_: list[tuple[str, str]] = [(path, "gate") for path in PY_GATE]
    # A changed Python module selects the tests that reach it in the import graph; the suites that
    # name a test directory trigger only on the other files (and on a module the root conftest reaches).
    graph_tests, conftest_broad = changed_tests.gate_tests(
        [f for f in files if is_python_source(f)]
    )
    by_dir = [f for f in files if not is_python_source(f) or f in conftest_broad]
    for name, triggers, tests in PY_SUITES:
        hit = by_dir if any((ROOT / t).is_dir() for t in tests) else files
        if full or any(f.startswith(triggers) for f in hit):
            plan_ += [(path, name) for path in resolve_tests(tests)]
    plan_ += [(t, "imports") for t in graph_tests]
    plan_ += [(f, "changed") for f in files if is_changed_test(f)]
    plan_ += [(t, "support") for f in files for t in support_users(f)]
    seen: set[str] = set()
    unique: list[tuple[str, str]] = []
    for p, s in plan_:
        if p not in seen:
            seen.add(p)
            unique.append((p, s))
    return unique


def python_tests(files: list[str], full: bool) -> list[str]:
    """The ordered, de-duplicated pytest paths for the python-tests step."""
    return [path for path, _ in python_test_plan(files, full)]


def python_tests_passed(out_dir: Path) -> str | None:
    """A python-tests run whose files passed no test at all is not a pass."""
    summary = out_dir / "summary.tsv"
    if not summary.exists():
        return f"no {summary.name}"
    rows = summary.read_text().splitlines()[1:]
    passed = 0
    for row in rows:
        cols = row.split("\t")
        if len(cols) > 4 and cols[4].isdigit():
            passed += int(cols[4])
    return None if passed > 0 else "nothing passed (0 tests passed)"


def python_tests_command(
    out: Path, files: list[str], jobs: int | None = None
) -> list[str]:
    """The per-file runner for the python-tests step (one pytest process per file; JOBS files at
    once, default python_jobs())."""
    cmd = [
        *PY,
        "-m",
        "tools.devtools.test_outcomes",
        "run",
        "--check",
        "--out",
        str(out),
    ]
    cmd = [*cmd, "--timeout", str(python_timeout())]
    return [*cmd, "--jobs", str(jobs if jobs is not None else python_jobs()), *files]


def python_jobs(share: int | None = None) -> int:
    """Test files run at once in python-tests: DIGI_PY_JOBS, else the node's share of the verify
    budget (verify_budget.workers_for), outside verify max(2, cpus // 4)."""
    env = os.environ.get("DIGI_PY_JOBS")
    if env:
        return max(1, int(env))
    if share is not None:
        return verify_budget.workers_for("python-tests", share).py_files
    return max(2, (os.cpu_count() or 1) // 4)


def python_timeout() -> int:
    """Per-file timeout for python-tests: catches hangs, not slow files (test_pluck.py
    takes ~450 s alone and more than 900 s under verify:full load)."""
    env = os.environ.get("DIGI_PY_TIMEOUT")
    if env:
        seconds = int(env)
        if seconds < 1:
            raise SystemExit("verify: DIGI_PY_TIMEOUT must be at least 1")
        return seconds
    return 3600


def selected(args: argparse.Namespace) -> list[tuple[str, str]]:
    files = args.files if args.files else changed_files(args.base)
    return python_test_plan(files, False)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("list", "run"):
        cmd = sub.add_parser(name)
        cmd.add_argument("--base", required=True, help="revision to diff against")
        cmd.add_argument("--files", nargs="+", help="changed files (default: git diff)")
        if name == "run":
            cmd.add_argument(
                "--out",
                type=Path,
                default=ROOT / "out" / "runs" / "py-select",
                help="default out/runs/py-select",
            )
    sub.add_parser("checks")
    args = parser.parse_args(argv)
    if args.command == "list":
        for path, reason in selected(args):
            print(f"{path}\t{reason}")
        return 0
    if args.command == "run":
        tests = [path for path, _ in selected(args)]
        if not tests:
            print("py_select: no Python tests selected")
            return 0
        args.out.mkdir(parents=True, exist_ok=True)
        done = subprocess.run(
            python_tests_command(args.out, tests), cwd=ROOT, env=python_env()
        )
        problem = python_tests_passed(args.out)
        if done.returncode or problem is not None:
            print(f"py_select: FAIL (exit {done.returncode}) {problem or ''}".rstrip())
            return 1
        return 0
    failed = []
    for label, check_cmd in PY_CHECKS:
        print(f"python-tests:{label}", flush=True)
        if subprocess.run(check_cmd, cwd=ROOT, env=python_env()).returncode:
            failed.append(label)
    if failed:
        print(f"py_select: checks failed: {', '.join(failed)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
