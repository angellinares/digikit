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
import ast
import os
import re
import subprocess
import sys
from functools import cache
from pathlib import Path

import grimp

from tools import python_env

ROOT = Path(__file__).resolve().parent.parent.parent
PY = ["uv", "run", "python"]
# python-tests always runs PY_GATE (the staleness checks: built module objects and port files match their
# sources) and PY_CHECKS (lint, format, types, paths). A suite of PY_SUITES, (name, trigger prefixes, test paths), runs when a
# changed file starts with one of its prefixes (all suites with --full); a suite with a test directory
# triggers only on files that are not Python modules of tools/ or tests/, which select the tests that
# reach them in the import graph instead (gate_tests). Test paths may be globs or
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
        # a change in a module selects its own tests through the Nx module projects
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
            "tools/devtools/py_select.py",
            "tools/devtools/fx_tier.py",
            "tools/devtools/nx_graph.py",
            "tools/devtools/agent_guard.py",
            ".claude/settings.json",
            "tools/devtools/verify_build.py",
            "tools/devtools/rust_ws.py",
            "tools/patchkit/pk_proofs.py",
            "tools/devtools/benchlock.py",
            "tools/devtools/test_outcomes.py",
            "tools/devtools/layercheck.py",
        ),
        (
            "tests/devtools/test_layercheck.py",
            "tests/devtools/test_fx_tier.py",
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


# Device scenarios: they run for their own triggers and their direct importers, never through a deep import chain.
DAEMON_TESTS = (
    "tests/digikit/",
    "tests/patchkit/smoke/",
    "tests/patchkit/test_patchkit_dkrt_boot.py",
)

# A module named in a string: `-m tools.x.y`, `-m support.x` (tests/ is on the pytest path), or a script path.
_RUN_NAME = re.compile(r"(?:tools|support)(?:\.\w+)+|(?:tools|tests)/[\w/]+\.py")


def _named_module(text: str) -> str | None:
    if text.startswith("support."):
        return "tests." + text
    if text.endswith(".py"):
        return module_of(text)
    return text


@cache
def graph(*, local_imports: bool = False) -> grimp.ImportGraph:
    """The import graph of tools/ and tests/, corrected in three ways:
    - an import inside a function of a tools module is dropped (kept with `local_imports=True`): it runs only
      when that function runs, and such edges join unrelated tools (tools/emulator/digi_env.py reaches all of
      patchkit through one);
    - a test's import of a helper by its pytest path (`support.x`, a sibling `scen_cases`) is an edge to the
      tests.* module (grimp does not resolve those names);
    - a module named by a whole string constant (`[sys.executable, "-m", "tools.x.y"]`, a script path) is an
      edge: the module runs it in a subprocess."""
    # tests/ is a namespace package; grimp reads it all the same
    g = grimp.build_graph("tools", "tests", cache_dir=None)
    mods = set(g.modules)
    for mod in sorted(mods):
        try:
            text = (ROOT / path_of(mod)).read_text(encoding="utf-8")
        except OSError:
            continue
        if mod.startswith("tools.") and not local_imports:
            lines = text.splitlines()
            for dst in sorted(g.find_modules_directly_imported_by(mod)):
                details = g.get_import_details(importer=mod, imported=dst)
                if details and all(
                    lines[d["line_number"] - 1][:1] in (" ", "\t") for d in details
                ):
                    g.remove_import(importer=mod, imported=dst)
        try:
            nodes = list(ast.walk(ast.parse(text)))
        except SyntaxError:
            nodes = []
        targets = {
            _named_module(n.value) or ""
            for n in nodes
            if isinstance(n, ast.Constant)
            and isinstance(n.value, str)
            and _RUN_NAME.fullmatch(n.value)
        }
        if mod.startswith("tests."):
            # pytest puts tests/ and each test's own directory on the path: `support.x`, `from support import x`
            # and a sibling helper (`from scen_cases import run` in tests/digikit) are tests.* modules
            here = mod.rsplit(".", 1)[0]
            for n in nodes:
                if isinstance(n, ast.ImportFrom) and n.level == 0 and n.module:
                    names = [n.module] + [f"{n.module}.{a.name}" for a in n.names]
                elif isinstance(n, ast.Import):
                    names = [a.name for a in n.names]
                else:
                    continue
                targets |= {f"tests.{x}" for x in names} | {
                    f"{here}.{x}" for x in names
                }
        for dst in sorted(targets & mods - {mod}):
            g.add_import(importer=mod, imported=dst)
    _add_module_tests(g, mods)
    return g


def module_test_files() -> list[str]:
    """Every .py file in a module's own tests/ directory (modules/<kind>/<name>/tests/), tests and helpers."""
    return sorted(
        p.relative_to(ROOT).as_posix()
        for p in (ROOT / "modules").glob("*/*/tests/**/*.py")
        if "__pycache__" not in p.parts
    )


def _add_module_tests(g: grimp.ImportGraph, mods: set[str]) -> None:
    """Add the module-owned test files as nodes (grimp reads only tools/ and tests/). pytest puts a test's
    own directory and tests/ on the path, so an import resolves to tools.x, tests.support.x, or a sibling
    file of the same tests/ directory (`from some_rig import x`); a whole string naming a tools module
    is an edge as for tests/."""
    files = {m: f for f in module_test_files() if (m := module_of(f))}
    for mod in files:
        g.add_module(mod)
    for mod, f in files.items():
        here = mod.rsplit(".", 1)[0]
        try:
            nodes = list(ast.walk(ast.parse((ROOT / f).read_text(encoding="utf-8"))))
        except (OSError, SyntaxError):
            continue
        names: set[str] = set()
        for n in nodes:
            if isinstance(n, ast.ImportFrom) and n.level == 0 and n.module:
                names |= {n.module, *(f"{n.module}.{a.name}" for a in n.names)}
            elif isinstance(n, ast.Import):
                names |= {a.name for a in n.names}
            elif (
                isinstance(n, ast.Constant)
                and isinstance(n.value, str)
                and _RUN_NAME.fullmatch(n.value)
            ):
                names.add(_named_module(n.value) or "")
        targets = names | {f"tests.{x}" for x in names} | {f"{here}.{x}" for x in names}
        for dst in sorted(targets & (mods | set(files)) - {mod}):
            g.add_import(importer=mod, imported=dst)


def module_of(path: str) -> str | None:
    """The dotted module of a .py file under tools/ or tests/, or in a module's own tests/ directory."""
    if not path.endswith(".py") or not (
        path.startswith(("tools/", "tests/")) or is_module_test_dir(path)
    ):
        return None
    parts = path.removesuffix(".py").split("/")
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def path_of(module: str) -> str:
    base = module.replace(".", "/")
    return f"{base}.py" if (ROOT / f"{base}.py").is_file() else f"{base}/__init__.py"


def is_test_file(path: str) -> bool:
    return (
        (path.startswith("tests/") or is_module_test_dir(path))
        and path.rsplit("/", 1)[-1].startswith("test_")
        and (path.endswith(".py"))
    )


def conftest_tests(conftest_path: str) -> list[str]:
    """The test files below the directory of a conftest, recursively."""
    base = (ROOT / conftest_path).parent
    found = (p.relative_to(ROOT).as_posix() for p in base.rglob("test_*.py"))
    return sorted(p for p in found if is_test_file(p))


@cache
def test_texts() -> dict[str, str]:
    found = [
        q.relative_to(ROOT).as_posix() for q in (ROOT / "tests").rglob("test_*.py")
    ]
    found += [p for p in module_test_files() if is_test_file(p)]
    return {
        p: (ROOT / p).read_text(encoding="utf-8")
        for p in sorted(found)
        if "node_modules" not in p
    }


def importing_tests(module: str) -> dict[str, str]:
    """Test path -> reason for the tests that import MODULE, directly or through a tests/ helper, and
    those whose text runs it by name."""
    g = graph()
    out: dict[str, str] = {}
    by_conftest: dict[str, str] = {}
    seen = {module}
    todo = [module]
    while todo:
        cur = todo.pop(0)
        via = "" if cur == module else f" via {cur}"
        for imp in sorted(g.find_modules_that_directly_import(cur)):
            if imp in seen or not imp.startswith(("tests.", "modules.")):
                continue
            seen.add(imp)
            path = path_of(imp)
            if imp.rsplit(".", 1)[-1] == "conftest":
                if path == "tests/conftest.py":
                    continue  # the caller puts the root conftest in gate_adds
                for t in conftest_tests(path):
                    by_conftest.setdefault(t, f"{path} imports {module}")
            elif is_test_file(path):
                out.setdefault(path, f"imports {module}{via}")
            else:
                todo.append(imp)
    for t, r in by_conftest.items():
        out.setdefault(t, r)
    # a helper of tests/ is imported as `support.x` (tests/ is on the pytest path), which the graph
    # does not see as tests.support.x
    if module.startswith("tests."):
        short = re.escape(module.removeprefix("tests."))
        pat = re.compile(rf"^\s*(?:from\s+{short}\s+import|import\s+{short}\b)", re.M)
        for p, text in test_texts().items():
            if pat.search(text):
                out.setdefault(p, f"imports {module}")
    path = path_of(module)
    quoted = (f'"{module}"', f"'{module}'", path)
    for p, text in test_texts().items():
        if any(q in text for q in quoted):
            out.setdefault(p, f"runs {module}")
    return out


def deep_tests(module: str) -> dict[str, str]:
    """Test files that reach MODULE through any chain of imports and that importing_tests did not take."""
    direct = importing_tests(module)
    out: dict[str, str] = {}
    for imp in sorted(graph().find_downstream_modules(module)):
        path = path_of(imp)
        if is_test_file(path) and path not in direct:
            out[path] = f"reaches {module} through tools"
    return out


def gate_tests(files: list[str]) -> tuple[dict[str, str], list[str]]:
    """For the python-tests step: test path -> reason for the tests that reach a changed
    Python module of tools/ or tests/ through any chain of the graph or run it by name, and the changed files
    whose module the root conftest reaches (every test imports them: the gate keeps their directory suites)."""
    g = graph()
    out: dict[str, str] = {}
    broad: list[str] = []
    for f in files:
        module = module_of(f)
        if module is None or is_test_file(f) or module not in g.modules:
            continue
        if "tests.conftest" in g.find_downstream_modules(module):
            broad.append(f)
        direct = importing_tests(module)
        # Device scenarios (DAEMON_TESTS) run for their own triggers and
        # their direct importers, never through a deep import chain.
        deep = {
            t: r
            for t, r in deep_tests(module).items()
            if not t.startswith(DAEMON_TESTS)
        }
        # Filter per source: a changed scenario helper must restore its consumers
        # even when one of the bounded launchers changes alongside it.
        for t, reason in {**direct, **deep}.items():
            if not t.startswith("tests/scenarios/"):
                out.setdefault(t, reason)
    return out, broad


def python_test_plan(files: list[str], full: bool) -> list[tuple[str, str]]:
    """(pytest path, source) in run order, without duplicates: the gate files, the
    suites a changed file triggers (all of them when FULL), changed test files, then
    the tests that import a changed tests/support/ module."""
    plan_: list[tuple[str, str]] = [(path, "gate") for path in PY_GATE]
    # A changed Python module selects the tests that reach it in the import graph; the suites that
    # name a test directory trigger only on the other files (and on a module the root conftest reaches).
    graph_tests, conftest_broad = gate_tests([f for f in files if is_python_source(f)])
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


def python_jobs() -> int:
    """Test files run at once in python-tests: DIGI_PY_JOBS, else max(2, cpus // 4)."""
    env = os.environ.get("DIGI_PY_JOBS")
    if env:
        return max(1, int(env))
    return max(2, (os.cpu_count() or 1) // 4)


def python_timeout() -> int:
    """Per-file timeout for python-tests: catches hangs, not slow files (test_pluck.py
    takes ~450 s alone and more than 900 s under gate:full load)."""
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
