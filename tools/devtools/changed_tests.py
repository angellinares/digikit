"""Fast host preflight for changed files; full validation belongs to `mise run verify`.

Static checks and an exact audited
Python unit-test boundary cannot build firmware, modules, Rust or emulators.
The existing selection/import graph below remains the full gate's coverage source.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import subprocess
import time
import tomllib
from dataclasses import dataclass, field
from functools import cache
from pathlib import Path

import grimp

ROOT = Path(__file__).resolve().parent.parent.parent

from tools import python_env  # noqa: E402
from tools.devtools import (  # noqa: E402
    py_select,
    verify,
    verify_budget,
    verify_route,
)
from tools.emulator import digi_env  # noqa: E402
from tools.patchkit import modroots, pk_conform  # noqa: E402


@dataclass
class Selection:
    tests: dict[str, str] = field(default_factory=dict)  # test file -> reason
    gate_adds: list[str] = field(default_factory=list)  # what `mise run verify` adds
    deep_only: dict[str, str] = field(default_factory=dict)  # selected by --deep only


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
        path.startswith(("tools/", "tests/")) or py_select.is_module_test_dir(path)
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
        (path.startswith("tests/") or py_select.is_module_test_dir(path))
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
    """For the merge gate (tools/devtools/verify.py): test path -> reason for the tests that reach a changed
    Python module of tools/ or tests/ through any chain of the graph or run it by name, and the changed files
    whose module the root conftest reaches (every test imports them: verify keeps their directory suites)."""
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
        # Device scenarios (verify.DAEMON_TESTS) run for their own triggers (fine rules) and
        # their direct importers, never through a deep import chain.
        deep = {
            t: r
            for t, r in deep_tests(module).items()
            if not t.startswith(verify.DAEMON_TESTS)
        }
        # Filter per source: a changed scenario helper must restore its consumers
        # even when one of the bounded launchers changes alongside it.
        for t, reason in {**direct, **deep}.items():
            if not t.startswith("tests/scenarios/"):
                out.setdefault(t, reason)
    return out, broad


def framework_tests(mid: str) -> list[str]:
    """The narrow set for a module a framework module depends on: the tests that name MID itself, the
    conformance suite for a machine module, the digikit tests of the scenarios that name it."""
    tests = [t for t, ids in verify_route.test_refs().items() if mid in ids]
    if mid in verify_route.CONFORMANCE_WRAPPERS or pk_conform.is_machine_module(mid):
        tests.append(verify_route.CONFORMANCE)
    if any(pk_conform.is_fx_module(m) for m in verify_route.dependents({mid})):
        tests.append(verify_route.FX_CONFORMANCE)
    kinds = {
        kind for kind, ids in verify_route.scenario_refs().values() if mid in ids
    } | verify_route.runner_kinds({mid})
    if kinds:
        tests += verify_route.scenario_tests(kinds)
    return list(dict.fromkeys(tests))


def select(files: list[str], deep: bool = False) -> Selection:
    sel = Selection()

    dropped = False

    def selectable(path: str) -> bool:
        nonlocal dropped
        if path.startswith("tests/scenarios/"):
            dropped = True
            return False
        return True

    def add(path: str, reason: str) -> None:
        if selectable(path) and (ROOT / path).is_file():
            sel.tests.setdefault(path, reason)
            sel.deep_only.pop(path, None)

    for f in files:
        if f.endswith(".md"):
            continue
        if py_select.is_changed_test(f) and (ROOT / f).is_file():
            add(f, "changed")
        fine = verify_route.fine(f)
        if fine is not None:
            for entry in fine.tests:
                if (ROOT / entry).is_dir():
                    sel.gate_adds.append(f"{entry} ({fine.label})")
                else:
                    add(entry, fine.label)
            mid = modroots.module_id(f)
            if (
                mid is not None
                and f.startswith(modroots.REPO_ROOTS)
                and verify_route.dependents({mid}) & verify_route.FRAMEWORK
            ):
                for t in framework_tests(mid):
                    add(t, f"{fine.label}: names {mid}")
        module = module_of(f)
        if module is None or is_test_file(f) or module not in graph().modules:
            continue
        for t, reason in importing_tests(module).items():
            add(t, reason)
        if "tests.conftest" in graph().find_modules_that_directly_import(module):
            sel.gate_adds.append(f"tests (the root conftest imports {module})")
        for t, reason in deep_tests(module).items():
            if deep:
                add(t, reason)
            elif t not in sel.tests and selectable(t):
                sel.deep_only.setdefault(t, reason)
    if dropped:
        sel.gate_adds.append(
            "tests/scenarios (the check steps run the scenario oracles)"
        )
    steps = [
        s for s in verify.steps_for(files) if s not in ("python-tests", "rust-tests")
    ]
    if steps:
        sel.gate_adds.append(f"steps: {', '.join(steps)}")
    plan_ = py_select.python_test_plan(files, False)
    label = {p: s for p, s in plan_}
    more = [
        (rel, label[entry])
        for rel, entry in verify.python_files([p for p, _ in plan_])
        if rel not in sel.tests
    ]
    if more:
        sources = ", ".join(dict.fromkeys(s for _, s in more))
        sel.gate_adds.append(f"python-tests: {len(more)} more files ({sources})")
    return sel


# Audited host-only units: the tests/devtools/ files of the PY_SUITES "devtools" suite (routing/data,
# mocked commands, bounded subprocesses). Do not infer safety from a directory or a test's name.
FAST_PYTHON_TESTS = frozenset(
    test
    for name, _, tests in py_select.PY_SUITES
    if name == "devtools"
    for test in tests
    if test.startswith("tests/devtools/")
)
JS_SUFFIXES = (".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts")


def typed_files() -> list[str]:
    """The mypy file list of [tool.mypy].files in pyproject.toml."""
    config = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    files = config.get("tool", {}).get("mypy", {}).get("files")
    if not files:
        raise SystemExit("test:changed: no [tool.mypy].files in pyproject.toml")
    return list(files)


def host_plan(files: list[str]) -> list[tuple[str, list[str] | None, str]]:
    """Delegate host test selection to Nx; retain the existing static-check scope."""
    out: list[tuple[str, list[str] | None, str]] = []
    if files:
        out.append(
            (
                "host-tests",
                [
                    "pnpm",
                    "nx:pilot",
                    "affected",
                    "--target=test-host",
                    "--parallel=1",
                    "--stdin",
                ],
                "",
            )
        )
    js = [f for f in files if f.endswith(JS_SUFFIXES)]
    # Preserve the static-check eligibility independently of Nx test selection.
    assets = [f for f in files if f.startswith("apps/desktop/ui/src/") and f not in js]
    config = any(
        f.endswith((".json", ".yaml", ".yml"))
        or Path(f).name.startswith(
            (
                "tsconfig",
                "vitest.",
                "vite.",
                ".oxlintrc",
                ".prettierrc",
                "prettier.config",
            )
        )
        for f in files
    )
    if not js and not assets and not config:
        return out
    existing = [f for f in js if (ROOT / f).is_file()]
    if existing:
        out.append(("oxlint", ["pnpm", "exec", "oxlint", *existing], ""))
    formatted = [
        f
        for f in files
        if (ROOT / f).is_file()
        and f.endswith((*JS_SUFFIXES, ".json", ".yaml", ".yml", ".css", ".astro"))
    ]
    if formatted:
        out.append(
            ("prettier", ["pnpm", "exec", "prettier", "--check", *formatted], "")
        )
    if js or config:
        out.append(("types", ["pnpm", "-r", "run", "typecheck"], ""))
    return out


def plan(
    files: list[str], out_dir: Path, deep: bool = False
) -> list[tuple[str, list[str] | None, str]]:
    """Only static checks and positively audited host tests, never arbitrary selected tests."""
    py = [
        f
        for f in files
        if f.endswith(".py")
        and (f.startswith(("tests/", "tools/")) or py_select.is_module_test_dir(f))
        and (ROOT / f).is_file()
    ]
    typed = [
        e for e in typed_files() if any(f == e or f.startswith(e + "/") for f in files)
    ]
    tests = sorted(set(select(files, deep).tests) & FAST_PYTHON_TESTS)
    return [
        (
            "ruff-check",
            [*verify.PY, "-m", "ruff", "check", "--no-cache", *py] if py else None,
            "no changed Python files",
        ),
        (
            "ruff-format",
            [*verify.PY, "-m", "ruff", "format", "--check", "--no-cache", *py]
            if py
            else None,
            "no changed Python files",
        ),
        (
            "mypy",
            [*verify.PY, "-m", "mypy", *typed] if typed else None,
            "no changed TYPED files",
        ),
        ("layercheck", [*verify.PY, "-m", "tools.devtools.layercheck"], ""),
        (
            "gen-check",
            [*verify.PY, "-m", "tools.patchkit.patchkit", "gen", "--check"],
            "",
        ),
        (
            "python-tests",
            py_select.python_tests_command(
                out_dir, tests, py_select.python_jobs(verify_budget.resolve_budget())
            )
            if tests
            else None,
            "no audited host Python tests selected",
        ),
        *host_plan(files),
    ]


def print_deferred(files: list[str], sel: Selection) -> None:
    print("Deferred to `mise run verify` (fast checks do not complete validation):")
    python_gate = "python-tests" in verify.steps_for(files)
    gate_python = py_select.python_test_plan(files, False) if python_gate else []
    selected = (
        set(sel.tests)
        | set(sel.deep_only)
        | {rel for rel, _ in verify.python_files([p for p, _ in gate_python])}
    )
    ran = set(sel.tests) & FAST_PYTHON_TESTS
    for path in sorted(selected - ran):
        print(f"  Python: {path}")
    scope = verify.rust_scope(files)
    if scope is None:
        print("  Rust: whole workspace")
    elif scope:
        print(f"  Rust crates: {', '.join(scope)}")
    for path in files:
        print(f"  Full gate coverage: {path}")
    for line in sel.gate_adds:
        if python_gate or not line.startswith("python-tests:"):
            print(f"  {line}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Fast host preflight; not complete validation or the merge gate."
    )
    ap.add_argument("--base", default="HEAD", help="merge base ref (default: HEAD)")
    ap.add_argument("--head", help="compare committed changes through this revision")
    ap.add_argument(
        "--files", nargs="+", help="explicit changed paths instead of git discovery"
    )
    ap.add_argument(
        "--explain", action="store_true", help="print the checks, run nothing"
    )
    ap.add_argument(
        "--deep",
        action="store_true",
        help="include transitive tooling import selection within the audited host boundary",
    )
    ap.add_argument(
        "--foreground",
        action="store_true",
        help="run at normal priority (default: nice 10)",
    )
    args = ap.parse_args(argv)
    if not args.explain:
        verify_budget.lower_priority(args.foreground)
    files = (
        args.files
        if args.files is not None
        else (
            py_select.changed_files(args.base, args.head)
            if args.head
            else py_select.changed_files(args.base)
        )
    )
    if any("\n" in path or "\r" in path for path in files):
        ap.error(
            "changed paths cannot contain newlines (Nx --stdin uses one path per line)"
        )
    sel = select(files, args.deep)
    print(f"test:changed: fast preflight for {len(files)} changed files")
    print_deferred(files, sel)
    failed: list[str] = []
    out_dir = Path("OUT") if args.explain else Path(digi_env.run_dir("test-changed"))
    for name, cmd, note in plan(files, out_dir, args.deep):
        if cmd is None:
            print(f"SKIP {name}: {note}", flush=True)
            continue
        if args.explain:
            print(f"RUN  {name}: {' '.join(cmd)}")
            if name == "host-tests":
                print(f"     changed paths supplied on stdin: {json.dumps(files)}")
            continue
        start = time.monotonic()
        env = verify_budget.bounded_env(
            "python-tests" if name == "python-tests" else "",
            verify_budget.resolve_budget(),
        )
        if name == "python-tests":
            print(f"python-tests logs: {out_dir}", flush=True)
        done: subprocess.CompletedProcess[str] | subprocess.CompletedProcess[bytes]
        if name == "host-tests":
            done = subprocess.run(
                cmd,
                cwd=ROOT,
                env=python_env(env),
                input="\n".join(files) + "\n",
                text=True,
            )
        else:
            done = subprocess.run(cmd, cwd=ROOT, env=python_env(env))
        if done.returncode:
            failed.append(name)
        print(
            f"{'FAIL' if done.returncode else 'PASS'} {name} ({time.monotonic() - start:.0f} s)",
            flush=True,
        )
    if failed:
        print(f"test:changed: FAILED: {', '.join(failed)}.")
    elif args.explain:
        print("test:changed: explained, nothing run.")
    else:
        print("test:changed: fast checks passed; deferred gate coverage remains.")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
