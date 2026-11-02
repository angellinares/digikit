"""One command for the exactness gate, scaled to what changed.

    uv run python -m tools.devtools.verify                  # steps for the uncommitted changes
    uv run python -m tools.devtools.verify --base BRANCH    # a lane: its commits since BRANCH plus uncommitted changes
    uv run python -m tools.devtools.verify --full           # full acceptance and tier-sweep (NO_AOT_SUBSET; --all-no-aot: all 134 scenarios)
    uv run python -m tools.devtools.verify --full --dt2     # plus the full DT2 FX tier sweep
    uv run python -m tools.devtools.verify --only pins,check
    uv run python -m tools.devtools.verify --rerun-failed   # only the steps that failed in the last run
    uv run python -m tools.devtools.verify --jobs 8        # a CPU budget of 8 cores for the whole gate (default: physical cores - 2); 1 = sequential
    uv run python -m tools.devtools.verify --foreground    # normal priority (default: nice 10)
    uv run python -m tools.devtools.verify --full --all-no-aot   # AOT = no-AOT equality on every FX scenario, not the fixed subset
    uv run python -m tools.devtools.verify --explain        # show the routing and the plan, run nothing
    uv run python -m tools.devtools.verify --base A --head B --explain   # the same for the commits A..B
(Use owner checks while iterating; `mise run test:changed` is the fast host preflight.)

CPU budget (tools/devtools/verify_budget.py, docs/TESTING.md "CPU budget"): the whole gate has a budget of
cores (--jobs N, DIGI_VERIFY_BUDGET, else the physical cores - 2); a step starts when its share is free and
the shares of the running steps never add up to more; the share sets the step's inner pools (cargo jobs, libtest
threads, tier and scenario workers, python-tests files and daemons per file, node tests). Verify and its children run at nice 10
(--foreground: normal priority). The builds run once, one after the other, before the steps that use their
binaries (build, build-daemons, build-dt2: tools/devtools/verify_build.py).

Scenario renders go through the content-addressed render cache (tools/devtools/render_cache.py). Each
cache hit is also re-rendered from scratch with probability DIGI_RENDER_CACHE_CHECK (verify
default 0.03; 0 = off) and compared by PCM sha256 and peeks; a mismatch fails the step as a
stale cache. verify.py sets the variable for all steps once, in main(). --no-cache sets
DIGI_RENDER_CACHE=0 so everything is rendered (the release/publication gate).

python-tests runs one pytest process per file; each file is a job of 2 cores in the budget queue, longest first by the
times of earlier runs (tools/devtools/verify_times.py; DIGI_PY_TIMEOUT per file in seconds, default 3600), through tools/devtools/test_outcomes.py; a run that passed no test fails. The
tier steps run NO_AOT_SUBSET (a fixed subset of the FX scenarios, documented there; --all-no-aot: every seventh
for tier-quick, all for tier-sweep), cut their (default/no-aot x file) groups into small chunks of scenarios and run
them from one shared queue, longest first, on the step's share of the budget in emulator processes (DIGI_TIER_JOBS replaces it).
check-no-aot runs 17 native FX assertions even with --full (--all-no-aot: 61 normally, 191 with --full).
Normal routing includes relevant stock, runtime and module acceptance; full tier sweeps stay explicit.
python-tests runs the PY_CHECKS commands (ruff, ruff format, mypy, layercheck, gen-check), the PY_GATE files, the tests that reach a changed Python module of tools/ or tests/ in the
import graph (changed_tests.gate_tests), the PY_SUITES whose trigger prefixes match a changed file (a suite with
a test directory only for the other files),
every changed tests/**/test_*.py, and the
test files that import a changed tests/support module. With
--full it runs PY_GATE plus every suite, not all of tests/ (`mise run test` runs all of tests/).
rust-tests runs cargo nextest run --workspace --release, or only the crates rust_ws.affected_packages names
for the changed files (--full and --only --all-files keep the workspace). The finer rules for modules/,
tools/device/, tests/digikit scenarios and mise.toml tasks are in tools/devtools/verify_route.py
(docs/TESTING.md, "What verify runs for a change"). --explain takes the same options as a run, then
prints each changed file with its matched prefix and steps, the node plan with dependencies and
the python-tests files with their suite, without running anything or creating a run directory.

Prints the chosen steps, then one PASS/FAIL line per step as each finishes (independent steps
run in parallel after the builds they need; each step's output is printed whole); each step's output is in
out/runs/verify/<stamp>/<step>.log. Exits 1 when any step failed.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import subprocess
import sys
import threading
import time
import tomllib
import xml.etree.ElementTree as ET
from collections.abc import Callable
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

from tools import python_env  # noqa: E402
from tools.devtools import (  # noqa: E402
    benchlock,  # noqa: E402
    fx_tier,  # noqa: E402
    rust_ws,  # noqa: E402
    test_outcomes,  # noqa: E402
    verify_budget,  # noqa: E402
    verify_route,  # noqa: E402
    verify_times,  # noqa: E402
)
from tools.devtools.fx_tier import (  # noqa: E402
    DT2_FX_FILES,
    no_aot_env,
    stream_process,
)
from tools.devtools.py_select import (  # noqa: E402
    PY_CHECKS,
    PY_GATE,
    PY_SUITES,
    changed_files,
    git_lines,
    is_changed_test,
    merge_base,
    python_test_plan,
    python_tests_passed,
    python_timeout,
)
from tools.emulator import (  # noqa: E402
    digi_env,  # noqa: E402
)
from tools.patchkit import modroots  # noqa: E402

PY = ["uv", "run", "python"]
STEPS = (
    "build",
    "desktop",
    "host",
    "web",
    "stock",
    "runtime-fixture",
    "module-load",
    "pins",
    "check",
    "check-no-aot",
    "tier-quick",
    "tier-sweep",
    "python-tests",
    "rust-tests",
    "card",
    "smoke",
    "dt2-tone",
    "dt2-check",
    "dt2-tier-quick",
    "dt2-tier-sweep",
)
# Normal broad verification excludes full-tier or explicitly requested steps.
EVERYTHING = tuple(s for s in STEPS if s not in ("tier-sweep", "dt2-tier-sweep"))
FALLBACK = ("build", "desktop", "pins", "check", "python-tests")
# Paths that cannot change what the patchkit smoke matrix runs: the desktop console UI (TS, TSX, CSS, its
# preview harness and tests), the web components and styles, the web node tests, and documentation. The
# smoke step is skipped only when EVERY changed file matches one of these glob patterns (fnmatch; `*`
# crosses `/`); one other file keeps it, so the matrix goldens cannot rot behind UI-only changes.
# Left out on purpose: apps/web/src/pages and layouts (they read the gallery module data and the patch
# page), apps/web/src/{patch,runtime,modules,data} (patchkit wasm, protocol client), apps/desktop/src-tauri,
# and everything under tools/, modules/, packages/, devices/, sdk/, tests/ and the firmware tooling.
UI_ONLY: tuple[str, ...] = (
    "apps/desktop/ui/*",
    "apps/web/src/components/*",
    "apps/web/src/styles/*",
    "apps/web/src/transfer/*",
    "apps/web/tests/*.mjs",
    "docs/*",
    "*.md",
)
# Exact maintenance paths whose existing owner host/Python coverage is sufficient. Keep this a
# table, not a prefix rule: neighbouring helpers and configuration stay conservative.
SMOKE_EXEMPT: dict[str, str] = {
    "tools/devtools/agent_guard.py": "agent guard",
    "tools/devtools/layercheck.py": "layer checker",
    ".claude/settings.json": "agent settings",
}
# Test files and gate tooling that cannot change a patched image or the smoke harness: the tests of the
# devtools, patchkit and digikit Python suites (not tests/patchkit/smoke or pk_harness*), the device
# acceptance tests, and tools/devtools (the gate itself). Same matching as UI_ONLY.
TEST_ONLY: tuple[str, ...] = (
    "tests/devtools/*",
    "tests/patchkit/test_*.py",
    "tests/digikit/test_*.py",
    # test files of the emulator tools; the smoke harness closure is under tools/ and never imports them
    "tests/emulator/test_*.py",
    "tests/digikit/scenarios/*.toml",
    "tests/regression/*.test.ts",
    "tests/regression/emulator/*.ts",
    "tests/regression/runtime/*.ts",
    "tests/conformance/*.ts",
    "modules/**/tests/*.device.test.ts",
    "tests/regression/project.json",
    "tools/devtools/*",
    # Nx project graph plugins: they define projects and targets, not the image or the smoke harness
    "tools/nx/*",
)
# Host Vitest test files (vitest.host.projects.json). The host config runs them without a device, so
# a change that only touches them cannot change the smoke matrix. Helpers, config and new files that
# match no glob stay conservative.
HOST_PROJECTS = ROOT / "vitest.host.projects.json"


def host_test_globs() -> tuple[str, ...]:
    """The test globs of the host Vitest projects (`*` crosses `/`, like UI_ONLY, so `**/` is dropped)."""
    projects = json.loads(HOST_PROJECTS.read_text())
    return tuple(f"{p['dir']}/{p['include']}".replace("**/", "") for p in projects)


# The first rule whose prefix matches a changed path decides that path's steps.
RULES: tuple[tuple[tuple[str, ...], tuple[str, ...]], ...] = (
    (("docs/", "README", "AGENTS.md", "CLAUDE.md", "tests/scenarios/README"), ()),
    # GitHub settings files (templates, CODEOWNERS): no gate step reads them.
    ((".github/",), ()),
    # the agent hook settings (tools/devtools/agent_guard.py); .claude/skills/ holds links and .agents/skills/ markdown (the skills test)
    ((".claude/", ".agents/"), ("python-tests",)),
    (
        ("tools/emulator/dt2fx.py",),
        ("dt2-check", "dt2-tier-quick", "python-tests"),
    ),
    (
        (
            *DT2_FX_FILES,
            "tests/regression/scenarios/dt2-fx.test.ts",
        ),
        ("host", "dt2-check", "dt2-tier-quick", "python-tests"),
    ),
    (
        (
            "tests/regression/scenarios/fx-support.ts",
            "tests/regression/vitest.config.ts",
        ),
        (
            "host",
            "check",
            "check-no-aot",
            "tier-quick",
            "dt2-check",
            "dt2-tier-quick",
            "python-tests",
        ),
    ),
    (
        ("tests/regression/scenarios/",),
        ("host", "check", "check-no-aot", "tier-quick"),
    ),
    (
        (
            "packages/control-client/",
            "packages/test-support/",
            "tests/regression/",
            "vitest.host.config.ts",
            "vitest.host.projects.json",
            "packages/dk-session/tests/",
            "apps/desktop/ui/tests/",
        ),
        ("host",),
    ),
    (
        (
            ".oxlintrc.json",
            ".prettierrc.json",
            ".prettierignore",
            ".editorconfig",
            "packages/dk-session/",
        ),
        ("web",),
    ),
    # The Transfer pane is shared code: the desktop app builds and type-checks it too.
    (("apps/web/src/transfer/",), ("desktop", "web")),
    (("apps/web/",), ("web",)),
    # The mixed web samples suite still imports these desktop sources.
    (
        ("apps/desktop/ui/src/card-core.ts", "apps/desktop/ui/src/samples-core.ts"),
        ("desktop", "web"),
    ),
    (("apps/desktop/",), ("desktop", "host")),
    (
        (
            "tools/sharc_dev/sharc_core/",
            "tools/sharc_dev/sharc_transpile/",
            "tools/sharc_dev/sharc_transpile_infer.py",
            "tools/sharc_dev/sharc_rsgen.py",
            "packages/emulator/sharc-gen/",
            "packages/emulator/sharc-rt/",
        ),
        EVERYTHING,
    ),
    (
        (
            "packages/emulator/sharc/src/fast/",
            "packages/emulator/sharc-fast-cl/",
            "packages/emulator/sharc-fast-wasm/",
        ),
        ("build", "pins", "check", "check-no-aot", "tier-quick", "rust-tests"),
    ),
    (
        ("packages/emulator/sharc/",),
        (
            "build",
            "desktop",
            "pins",
            "check",
            "check-no-aot",
            "tier-quick",
            "rust-tests",
            "dt2-tone",
        ),
    ),
    # the digikit scenario runners and the runtime packer: python tests only (no web bundle, no Rust reads them)
    (("tools/patchkit/pk_digikit_", "tools/patchkit/pk_dkrt"), ("python-tests",)),
    # modules/ is routed by verify_route.module_fine, not here
    (
        (
            "packages/patchkit/",
            "devices/ports/",
            "tools/patchkit/pk_",
            "tools/patchkit/modroots.py",
            "tools/patchkit/port_resolve.py",
            "tools/patchkit/patchkit.py",
            "tests/patchkit/",
        ),
        ("rust-tests", "python-tests", "web"),
    ),
    # the Rust crate behind the browser upload (cdylib for packages/dkupload and apps/web/tests/dkupload.test.mjs)
    (("packages/dkupload/",), ("rust-tests", "web")),
    (
        ("packages/emulator/control/",),
        ("desktop", "rust-tests", "card", "python-tests"),
    ),
    (
        (
            "packages/emulator/plusdrive/",
            "packages/emulator/card/",
            "packages/emulator/boot/examples/plusdrive_check.rs",
            "tools/emulator/card_check.py",
            "tools/firmware/plusdrive_check.py",
        ),
        ("desktop", "rust-tests", "card"),
    ),
    (
        ("packages/emulator/",),
        ("build", "desktop", "pins", "check", "rust-tests", "card", "dt2-tone"),
    ),
    (
        (
            "tests/scenarios/",
            "tools/emulator/fxcheck.py",
            "tools/emulator/audiocheck.py",
            "tools/emulator/scenario_dsl.py",
        ),
        ("check", "python-tests"),
    ),
    (
        (
            "tools/emulator/coupled.py",
            "tools/emulator/digi_env.py",
            "tools/emulator/native_emu.py",
            "tools/emulator/native_pgo.py",
            "tools/emulator/native_target.py",
            "tools/devtools/fastcov.py",
        ),
        ("build", "pins", "python-tests"),
    ),
    # test files of the emulator tools; the smoke harness closure is under tools/ and never imports them
    (("tests/emulator/test_",), ("python-tests",)),
    # the Nx graph: host affected selection and verify_route.nx_graph read it
    (("nx.json", "tools/nx/"), ("host", "python-tests")),
    (("tools/", "tests/", "pyproject.toml"), ("python-tests",)),
    (
        ("Cargo.toml", "Cargo.lock", "mise.toml", "package.json", "pnpm-lock.yaml"),
        EVERYTHING,
    ),
)


def git_show(rev: str | None, path: str) -> str | None:
    """PATH at REV (None: the working tree); None when it does not exist there."""
    if rev is None:
        file = ROOT / path
        return file.read_text(encoding="utf-8") if file.is_file() else None
    done = subprocess.run(
        ["git", "show", f"{rev}:{path}"], cwd=ROOT, capture_output=True, text=True
    )
    return done.stdout if done.returncode == 0 else None


def expand_mise(
    files: list[str], base: str, head: str | None
) -> tuple[list[str], dict[str, str]]:
    """Replace a changed mise.toml by the paths of the tools its changed tasks run (a broad
    change keeps mise.toml, which selects every normal step). Also returns path -> origin note."""
    if "mise.toml" not in files:
        return files, {}
    since = merge_base(base, head)
    change = verify_route.mise_paths(
        git_show(since, "mise.toml"), git_show(head, "mise.toml")
    )
    if change is None:
        return files, {
            "mise.toml": "broad change (tools, env, verify/build/gen task, unknown run)"
        }
    paths, origin = change
    rest = [f for f in files if f != "mise.toml"]
    return sorted(set(rest) | set(paths)), origin


def member_dirs() -> dict[str, str]:
    """Workspace member package name -> directory, from the root Cargo.toml (globs allowed)."""
    members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]["members"]
    found: dict[str, str] = {}
    for pattern in members:
        for manifest in sorted(ROOT.glob(f"{pattern}/Cargo.toml")):
            pkg = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package")
            if pkg and "name" in pkg:
                found[pkg["name"]] = manifest.parent.relative_to(ROOT).as_posix()
    return found


def expand_lock(
    files: list[str], base: str, head: str | None
) -> tuple[list[str], dict[str, str]]:
    """Replace a changed Cargo.lock by the Cargo.toml of the workspace members the changed packages
    reach (BASE's merge base against HEAD or the working tree). Wide (unchanged) when that cannot
    be told. Also returns path -> origin note."""
    if "Cargo.lock" not in files:
        return files, {}
    since = merge_base(base, head)
    old, new = git_show(since, "Cargo.lock"), git_show(head, "Cargo.lock")
    names = verify_route.lock_members(old, new) if old and new else None
    dirs = member_dirs() if names else {}
    if not names or not names <= dirs.keys():
        return files, {}
    paths = [f"{dirs[n]}/Cargo.toml" for n in sorted(names)]
    rest = [f for f in files if f != "Cargo.lock"]
    note = "Cargo.lock: " + ", ".join(dirs[n] for n in sorted(names))
    return sorted(set(rest) | set(paths)), {p: note for p in paths}


def drop_metadata_objects(files: list[str], base: str, head: str | None) -> list[str]:
    """FILES without the committed .dko.json objects that were only rebuilt (same content, other
    toolchain or source hash): they are no change."""
    since = merge_base(base, head)
    kept = []
    for path in files:
        if path.endswith(".dko.json"):
            old, new = git_show(since, path), git_show(head, path)
            if old and new and verify_route.object_metadata_only(old, new):
                print(f"verify: object: metadata only: {path}", file=sys.stderr)
                continue
        kept.append(path)
    return kept


def base_route(path: str) -> tuple[str, tuple[str, ...]]:
    """How one changed path is routed: (matched prefix or "docs"/"fallback", steps)."""
    if path.endswith(".md"):
        return "docs", ()  # documentation selects no steps
    if path in COMMENT_ONLY:
        return "comments only", ()
    if path in PACKAGE_SCRIPTS_ONLY:
        return "package scripts", ("host",)
    found = verify_route.fine(path)
    if found is not None:
        return found.label, found.steps
    if (
        path.startswith("tests/support/")
        and path.endswith(".py")
        and not (ROOT / path).is_file()
        and not verify_route.deleted_path_referenced(path)
    ):
        return "deleted support module", ()  # nothing refers to it any more
    for prefixes, steps in RULES:
        for prefix in prefixes:
            if path.startswith(prefix):
                if verify_route.emulator_test_path(path):
                    steps = tuple(s for s in steps if s in ("build", "rust-tests"))
                return prefix, steps
    return "fallback", FALLBACK


def route(path: str) -> tuple[str, tuple[str, ...]]:
    """Keep the existing owner route and add relevant native acceptance."""
    label, original = base_route(path)
    if path in COMMENT_ONLY:
        return label, ()
    wanted = set(original)
    for step in verify_route.ACCEPTANCE_DIRS:
        if verify_route.acceptance_files(step, [path], resolve_helpers=False):
            wanted.add(step)
    return label, tuple(s for s in STEPS if s in wanted)


def acceptance_plan(step: str, files: list[str]) -> list[str]:
    """A broad owner must not be narrowed by a second leaf change; empty means all files."""
    if any(step in base_route(path)[1] for path in files):
        return []
    return verify_route.acceptance_files(
        step, [path for path in files if path not in COMMENT_ONLY]
    )


def ui_only_path(path: str) -> bool:
    """Whether one path matches the UI-only smoke exemption."""
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in UI_ONLY)


def ui_only(files: list[str]) -> bool:
    """Whether every changed file matches UI_ONLY (no file: False)."""
    return bool(files) and all(ui_only_path(path) for path in files)


def test_only_path(path: str) -> bool:
    """Whether one path matches the test-only smoke exemption (TEST_ONLY)."""
    return verify_route.emulator_test_path(path) or any(
        fnmatch.fnmatchcase(path, pattern) for pattern in TEST_ONLY
    )


def host_test_path(path: str) -> bool:
    """Whether one path is a host Vitest test file (vitest.host.projects.json)."""
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in host_test_globs())


# pyproject.toml when the current change touches only `tool.mypy` or `tool.ruff` (set by changed_plan);
# python-tests, which runs ruff and mypy, is its only step.
LINT_CONFIG_ONLY: set[str] = set()

# Changed .py files whose code is the same at base and head (set by changed_plan): no steps.
COMMENT_ONLY: set[str] = set()

# The root package.json when the current change touches only scripts that no gate step invokes (set by
# changed_plan); host is its only step.
PACKAGE_SCRIPTS_ONLY: set[str] = set()


def smoke_cells(files: list[str]) -> list[str] | None:
    """Filter only the three audited production leaves plus existing exemptions; None means full."""
    leaves: set[str] = set()
    for path in files:
        if (
            ui_only_path(path)
            or path in SMOKE_EXEMPT
            or host_test_path(path)
            or test_only_path(path)
            or path in LINT_CONFIG_ONLY
            or path in COMMENT_ONLY
            or path in PACKAGE_SCRIPTS_ONLY
            or path in verify_route.PROJECT_REGISTRATIONS
            or verify_route.outside_smoke_harness(path)
        ):
            continue
        mid = modroots.module_id(path)
        if mid not in verify_route.RUNTIME_UPLOADS:
            return None
        leaves.add(mid)
    return verify_route.module_smoke_cells(leaves) if leaves else None


def smoke_reason(files: list[str]) -> str | None:
    """Why this complete changed-file set may skip the smoke matrix, if any."""
    if ui_only(files):
        return "UI-only change"
    maintenance: list[str] = []
    ui = False
    host = False
    tests = False
    for path in files:
        if ui_only_path(path):
            ui = True
        elif reason := SMOKE_EXEMPT.get(path):
            maintenance.append(reason)
        elif group := verify_route.patchkit_no_smoke(path):
            maintenance.append(group)
        elif path in LINT_CONFIG_ONLY:
            maintenance.append("lint configuration")
        elif path in COMMENT_ONLY:
            maintenance.append("comments only")
        elif path in PACKAGE_SCRIPTS_ONLY:
            maintenance.append("package scripts")
        elif path in verify_route.PROJECT_REGISTRATIONS:
            maintenance.append("test registration")
        elif verify_route.outside_smoke_harness(path):
            maintenance.append("outside the smoke harness closure")
        elif host_test_path(path):
            host = True
        elif test_only_path(path):
            tests = True
        else:
            if smoke_cells(files) == []:
                return "audited production leaves have no dependent cells in the smoke matrix"
            return None
    if tests:
        maintenance.append("test-only")
    if maintenance:
        if host:
            maintenance.append("host-only test")
        kind = "UI-only and smoke-exempt" if ui else "smoke-exempt"
        return f"{kind} maintenance change ({', '.join(sorted(maintenance))})"
    if host:
        return "UI-only and host-only test change" if ui else "host-only test change"
    return None


def steps_for(files: list[str]) -> tuple[str, ...]:
    """The steps the changed files need, in gate order."""
    wanted: set[str] = set()
    for path in files:
        if path in COMMENT_ONLY:
            continue
        wanted.update(route(path)[1])
        if is_changed_test(path) or (
            not path.endswith(".md")
            and not verify_route.emulator_test_path(path)
            and verify_route.fine(path) is None
            and any(path.startswith(triggers) for _, triggers, _ in PY_SUITES)
        ):
            wanted.add("python-tests")
    if "web" in wanted:
        wanted.add("host")
    # The patchkit smoke matrix runs whenever the gate runs anything, except for the narrowly
    # documented complete change sets above (docs-only: nothing at all).
    if smoke_reason(files):
        wanted.discard("smoke")
    elif wanted:
        wanted.add("smoke")
    return tuple(s for s in STEPS if s in wanted)


def cache_env(
    base: dict[str, str] | None = None, no_cache: bool = False
) -> dict[str, str]:
    """``base`` (default os.environ) with the render-cache settings of the
    scenario-rendering steps: spot-check probability (default 0.03) and, with
    ``no_cache``, DIGI_RENDER_CACHE=0."""
    env = dict(os.environ if base is None else base)
    env.setdefault("DIGI_RENDER_CACHE_CHECK", "0.03")
    if no_cache:
        env["DIGI_RENDER_CACHE"] = "0"
    return env


def pytest_ran(text: str) -> str | None:
    """A pytest run that only skipped is not a pass."""
    lines = text.strip().splitlines()
    last = lines[-1] if lines else ""
    return None if " passed" in last else f"nothing passed ({last})"


class Gate:
    """Collects each step's output lines in a per-thread buffer, so parallel steps
    print whole and in order when they finish."""

    def __init__(
        self,
        out: Path,
        py_tests: list[str] | None = None,
        rust_packages: list[str] | None = None,
        all_no_aot: bool = False,
        budget: int = 1,
        acceptance: dict[str, list[str]] | None = None,
        smoke_selection: list[str] | None = None,
        py_checks: list[str] | None = None,
    ) -> None:
        self.out = out
        # the PY_CHECKS labels python-tests runs (None: all); a rerun names the checks that failed
        self.py_checks = py_checks
        self.rust_packages = rust_packages
        self.all_no_aot = all_no_aot
        self.budget = budget
        self.acceptance = acceptance or {}
        # module ids the conformance suite loads (empty: all eligible modules)
        self.conformance: list[str] = []
        self.smoke_selection = smoke_selection
        # variables set by an earlier node for the later ones (the pinned PGO daemons of build-daemons)
        self.shared_env: dict[str, str] = {}
        self.py_tests = py_tests if py_tests is not None else list(PY_GATE)
        self.failed: list[str] = []
        # python-tests: the record of every file job (test_outcomes.run_one) and when the first started
        self.py_records: dict[str, dict[str, object]] = {}
        self.py_files: list[
            str
        ] = []  # the files of py_tests (directories expanded), set by run_steps
        self.py_started: float | None = None
        self.py_checks_ran = False
        self.local = threading.local()
        self.lock = threading.Lock()

    def share(self) -> int:
        """The cores the running node holds (the whole budget outside run_steps)."""
        return getattr(self.local, "share", self.budget)

    def step_env(self, env: dict[str, str] | None = None) -> dict[str, str]:
        """ENV (default os.environ) plus the node's pool bounds (verify_budget.step_env) and the
        variables earlier nodes shared."""
        node = getattr(self.local, "node", "")
        base = dict(os.environ if env is None else env)
        if (
            node in DAEMON_NODES or verify_budget.is_file_job(node)
        ):  # only these run the pinned daemons (others render: no stray DIGI_ variables)
            base.update(self.shared_env)
        return verify_budget.bounded_env(node, self.share(), base)

    def say(self, line: str) -> None:
        buf = getattr(self.local, "lines", None)
        if buf is None:
            print(line, flush=True)
        else:
            buf.append(line)

    def report(
        self, name: str, problem: str | None, seconds: float, text: str = ""
    ) -> None:
        if problem is None:
            self.say(f"PASS {name} ({seconds:.0f} s)")
            return
        with self.lock:
            self.failed.append(name)
        tail = " | ".join(text.strip().splitlines()[-3:])
        self.say(f"FAIL {name} ({seconds:.0f} s): {problem}: {tail}")

    def run(
        self,
        name: str,
        cmd: list[str],
        env: dict[str, str] | None = None,
        check: Callable[[str], str | None] | None = None,
        cwd: Path = ROOT,
    ) -> None:
        """Run one step, log its output and print its line."""
        start = time.monotonic()
        with benchlock.hold("shared", f"verify {name}"):
            returncode, text = stream_process(
                cmd,
                self.out / f"{name}.log",
                cwd=cwd,
                env=python_env(self.step_env(env)),
            )
        if returncode:
            problem: str | None = f"exit {returncode}"
        else:
            problem = check(text) if check else None
        self.report(name, problem, time.monotonic() - start, text)


def tier(gate: Gate, name: str) -> None:
    """FX scenarios must give the same PCM in the default and no-AOT configurations (fx_tier.run)."""
    start = time.monotonic()
    jobs = fx_tier.tier_jobs_budget(gate.share())
    problem, text = fx_tier.run(
        name, gate.out, gate.all_no_aot, jobs, gate.step_env(), gate.say
    )
    gate.report(name, problem, time.monotonic() - start, text)


def desktop_check(gate: Gate) -> None:
    """The desktop console builds and type-checks, and the app crate compiles with it and passes its tests
    (rust-tests leaves the crate out: its own test binary links the generated core)."""
    gate.run("desktop-ui", ["pnpm", "--filter", "@digi/desktop", "run", "build:ui"])
    gate.run(
        "desktop-ui-types", ["pnpm", "--filter", "@digi/desktop", "run", "check:ui"]
    )
    env = digi_env.rust_env()
    env["SHARC_GEN_DIR"] = str(digi_env.generated_dir())
    cmd = ["cargo", "test", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml"]
    cmd += ["--target-dir", str(ROOT / "out" / "native" / "target" / "check-desktop")]
    gate.run("desktop", cmd, env=env)


def host_check(gate: Gate) -> None:
    gate.run("host", ["pnpm", "verify:host"])


def web_check(gate: Gate) -> None:
    web = ROOT / "apps" / "web"
    gate.run(
        "patchkit-wasm", [*PY, "-m", "tools.emulator.native_wasm", "--patchkit-only"]
    )
    gate.run(
        "web-tests",
        ["node", f"--test-concurrency={gate.share()}", "--test", "tests/*.test.mjs"],
        cwd=web,
    )
    gate.run("web-types", ["pnpm", "--filter", "@digi/web", "check"])


# Steps that need a built binary: DN2 steps the DN2 coupled-run, DT2 steps the DT2 one.
# The build nodes run first; the others are independent of each other.
DN2_BINARY = ("pins", "check", "check-no-aot", "tier-quick", "tier-sweep")
DT2_BINARY = ("dt2-tone", "dt2-check", "dt2-tier-quick", "dt2-tier-sweep")
# Test files that drive a `digi-emud` daemon: python-tests builds the daemons once (build-daemons) when
# its plan has one of them (a file not listed here still works: it builds the daemon itself).
DAEMON_NODES = (
    "python-tests",
    "card",
    "smoke",
    "stock",
    "runtime-fixture",
    "module-load",
)
DAEMON_TESTS = (
    "tests/digikit/",
    "tests/patchkit/smoke/",
    "tests/patchkit/test_patchkit_dkrt_boot.py",
)
# Builds into the generated-core target dirs (tools/emulator/native_target.py), one after the other and
# never beside each other: `build` (coupled-run, prod core: out/native/target/prod), `build-daemons`
# (digi-emud of both devices, plain in those two dirs and PGO in their pgo-emud-<device> variants;
# tools/devtools/verify_build.py), `build-dt2` (coupled-run, the DT2 core: target/g-<hash>).
# The launcher's `pgo-use` is never built here.
NO_AOT_STEPS = (
    "check-no-aot",
    "tier-quick",
    "tier-sweep",
    "dt2-tier-quick",
    "dt2-tier-sweep",
)
# build first: rust-tests (the longest step) waits for it, and it is not the python-tests files that wait for
# build-dt2, which only the DT2 steps need. A model of the 2026-10-19 full run (durations of that run, this
# scheduler) ends at 1770 s with this order, 2030 s with build-daemons first (rust-tests would start 430 s later).
BUILD_CHAIN = ("build", "build-daemons", "build-dt2")
# Longest first, so the pool is not left waiting on one long step at the end.
PRIORITY = (
    "build",
    "build-dt2",
    "build-daemons",
    "rust-tests",
    "tier-sweep",
    "dt2-tier-sweep",
    "dt2-tier-quick",
    "check-no-aot",
    "check",
    "smoke",
    "tier-quick",
    "dt2-check",
    "pins",
)


def needs_daemons(steps: tuple[str, ...], py_tests: list[str] | None) -> bool:
    """Whether a step drives a digi-emud daemon: card, smoke, or python-tests with a DAEMON_TESTS file."""
    if "card" in steps or "smoke" in steps:
        return True

    def overlaps(test: str) -> bool:  # a file or a directory of the plan
        mine = test.rstrip("/") + "/" if "." not in test.rsplit("/", 1)[-1] else test
        return any(d.startswith(mine) or mine.startswith(d) for d in DAEMON_TESTS)

    return "python-tests" in steps and any(overlaps(t) for t in (py_tests or []))


def plan(
    steps: tuple[str, ...], py_tests: list[str] | None = None
) -> tuple[list[str], dict[str, tuple[str, ...]]]:
    """The nodes to run (steps plus the builds they need) and each node's dependencies.

    The builds form a chain (BUILD_CHAIN order): each waits for the builds before it. A step waits
    for the build that makes its binary; rust-tests waits for `build` when that runs (both fill the
    prod target dir, so the generated core compiles once)."""
    nodes = list(steps)
    if "web" in nodes and "host" not in nodes:
        nodes.append("host")
    wanted = {
        "build": any(s in DN2_BINARY for s in steps),
        "build-dt2": any(s in DT2_BINARY for s in steps),
        "build-daemons": needs_daemons(steps, py_tests),
    }
    for build in BUILD_CHAIN:
        if (wanted[build] or build in steps) and build not in nodes:
            nodes.append(build)
    chain = [b for b in BUILD_CHAIN if b in nodes]
    deps: dict[str, tuple[str, ...]] = {}
    for node in nodes:
        if node in chain:
            deps[node] = tuple(chain[: chain.index(node)])
        elif node == "web":
            deps[node] = ("host",)
        elif node in DN2_BINARY:
            deps[node] = ("build",)
        elif node in DT2_BINARY:
            deps[node] = ("build-dt2",)
        elif node == "rust-tests" and "build" in nodes:
            deps[node] = ("build",)
        elif node in DAEMON_NODES and "build-daemons" in nodes:
            deps[node] = ("build-daemons",)
        else:
            deps[node] = ()
    order = {n: i for i, n in enumerate(PRIORITY)}
    nodes.sort(
        key=lambda n: (order.get(n, len(PRIORITY)), STEPS.index(n) if n in STEPS else 0)
    )
    return nodes, deps


# a python-tests job this long starts before the steps (the tail of the gate); a model of the 2026-10-19 run:
# 1930 s at 200, 1830 s at 100, 1810 s at 60
LONG_FILE_SECONDS = 100


def python_file_seconds(
    files: list[str], current: Path | None = None
) -> dict[str, float]:
    """Seconds of FILES in the newest earlier verify runs (verify_times), CURRENT (this run) left out."""
    return verify_times.file_seconds(
        Path(digi_env.RUNS_ROOT) / "verify", files, current
    )


def step_seconds(steps: list[str], current: Path | None = None) -> dict[str, float]:
    """Seconds of STEPS in the newest earlier verify runs (verify_times), CURRENT (this run) left out."""
    return verify_times.step_seconds(
        Path(digi_env.RUNS_ROOT) / "verify", steps, current
    )


def python_files(py_tests: list[str]) -> list[tuple[str, str]]:
    """(file, the plan entry it came from) for every test file of PY_TESTS (a directory is expanded as
    test_outcomes does), in plan order and without duplicates."""
    seen: set[str] = set()
    rows: list[tuple[str, str]] = []
    for entry in py_tests:
        for rel in test_outcomes.expand_paths(ROOT, [entry]):
            if rel not in seen:
                seen.add(rel)
                rows.append((rel, entry))
    return rows


def needs_daemon(test: str) -> bool:
    """Whether the python-tests file TEST drives a digi-emud daemon (DAEMON_TESTS)."""
    return any(test.startswith(d) for d in DAEMON_TESTS)


def expand_python(
    nodes: list[str],
    deps: dict[str, tuple[str, ...]],
    py_tests: list[str],
    seconds: dict[str, float],
    step_secs: dict[str, float],
) -> tuple[list[str], dict[str, tuple[str, ...]]]:
    """Replace the python-tests node by one job per file (FILE_PREFIX + path) plus the node itself, which
    only judges the files once they all ended (finish_python) and holds no cores.

    Audited fast files need no build barrier. A file that drives a daemon waits for build-daemons
    only; unaudited files keep every build barrier when daemons are built (they may build for themselves:
    the cargo steps of tests/emulator). The jobs are in the order of
    verify_times.longest_first; those of LONG_FILE_SECONDS or more come before the other steps, so the
    longest file starts as soon as its daemon is built; the rest fill the cores the steps leave.
    A step whose estimate in STEP_SECS is LONG_FILE_SECONDS or more joins that long part, longest first."""
    if "python-tests" not in nodes:
        return nodes, deps
    from tools.devtools.changed_tests import FAST_PYTHON_TESTS

    py_tests = [rel for rel, _ in python_files(py_tests)]
    builds = tuple(n for n in BUILD_CHAIN if n in nodes)
    ordered = verify_times.longest_first(py_tests, seconds)
    new_deps = dict(deps)
    names: dict[str, str] = {}
    for rel, _, _ in ordered:
        job = verify_budget.FILE_PREFIX + rel
        names[rel] = job
        if rel in FAST_PYTHON_TESTS or "build-daemons" not in nodes:
            new_deps[job] = ()
        elif needs_daemon(rel):
            new_deps[job] = ("build-daemons",)
        else:
            new_deps[job] = builds
    new_deps["python-tests"] = tuple(names.values())
    rest = [n for n in nodes if n != "python-tests"]
    built = [n for n in rest if n in BUILD_CHAIN]
    steps = [n for n in rest if n not in BUILD_CHAIN]
    long_part = [(sec, names[r]) for r, sec, _ in ordered if sec >= LONG_FILE_SECONDS]
    long_part += [
        (step_secs[n], n) for n in steps if step_secs.get(n, 0) >= LONG_FILE_SECONDS
    ]
    long_part.sort(key=lambda p: -p[0])  # stable
    long_steps = {n for _, n in long_part}
    other_steps = [n for n in steps if n not in long_steps]
    short_jobs = [names[r] for r, sec, _ in ordered if sec < LONG_FILE_SECONDS]
    return [
        *built,
        *(n for _, n in long_part),
        *other_steps,
        *short_jobs,
        "python-tests",
    ], new_deps


def next_starts(
    pending: list[str],
    deps: dict[str, tuple[str, ...]],
    done: set[str],
    budget: verify_budget.Budget,
) -> list[tuple[str, int]]:
    """The nodes of PENDING (in order) whose dependencies are DONE and that the budget admits now, with
    their shares (held in BUDGET). A ready step that was refused for cores keeps the file jobs after
    it waiting: they take the cores that are left, not the cores a step is waiting for."""
    started: list[tuple[str, int]] = []
    starved = False
    for node in pending:
        if not all(d in done for d in deps[node]):
            continue
        job = verify_budget.is_file_job(node)
        if job and starved:
            continue
        share = budget.admit(node)
        if share is None:
            starved = starved or not job
            continue
        started.append((node, share))
    return started


def rust_tests_command(packages: list[str] | None = None) -> list[str]:
    """cargo nextest run --workspace --release (rust_ws.py); digiemu-desktop is always excluded (the desktop
    step tests it). PACKAGES (from rust_scope) limit the run to those crates
    when rust_ws accepts `--packages`; until it does, the whole workspace runs."""
    cmd = [*PY, "-m", "tools.devtools.rust_ws", "test"]
    if packages and "--packages" in (rust_ws.__doc__ or ""):
        cmd += ["--packages", ",".join(packages)]
    return cmd


def with_desktop(
    steps: tuple[str, ...], rust_packages: list[str] | None
) -> tuple[str, ...]:
    """STEPS plus `desktop` when rust-tests runs and its crates include the desktop app (None: the whole
    workspace): rust-tests leaves digiemu-desktop out, the desktop step builds and tests it."""
    if "rust-tests" not in steps or "desktop" in steps:
        return steps
    if rust_packages is not None and rust_ws.DESKTOP not in rust_packages:
        return steps
    return tuple(s for s in STEPS if s in {*steps, "desktop"})


def rust_scope(files: list[str]) -> list[str] | None:
    """The workspace packages whose tests the changed FILES can affect: None is the whole
    workspace, [] no Rust test. Broad mise.toml and Rust routing changes keep the whole workspace."""
    if any(f in ("mise.toml", "tools/devtools/rust_ws.py") for f in files):
        return None
    return rust_ws.affected_packages(files)


def worker_plan_lines(
    nodes: list[str],
    deps: dict[str, tuple[str, ...]],
    budget: int,
    label: str | None = None,
) -> list[str]:
    """The worker plan: the share of every node in waves (verify_budget.wave_plan) and what the share sets."""
    lines = [
        f"worker plan: {label or f'budget {budget} cores'}, one emulator process = {verify_budget.EMULATOR_CORES} cores "
        "(waves are an estimate: a run starts a node as soon as the cores are free)"
    ]
    if "python-tests" in nodes:  # one job per file, in the same queue (expand_python)
        lines.append(
            f"  python-tests: one job per file, {min(verify_budget.EMULATOR_CORES, budget)} cores each, "
            f"longest first (files of {LONG_FILE_SECONDS} s or more before the steps), "
            f"{budget // min(verify_budget.EMULATOR_CORES, budget)} at once at most"
        )
        nodes = [n for n in nodes if n != "python-tests"]
    for number, wave in enumerate(verify_budget.wave_plan(nodes, deps, budget), 1):
        used = sum(share for _, share in wave)
        lines.append(f"  wave {number} ({used} of {budget} cores):")
        for node, share in wave:
            lines.append(
                f"    {node}: {share} cores ({verify_budget.describe(node, share)})"
            )
    return lines


def explain(
    steps: tuple[str, ...],
    files: list[str],
    py_plan: list[tuple[str, str]],
    per_file: bool,
    origin: dict[str, str] | None = None,
    rust_packages: list[str] | None = None,
    budget: int = 1,
    all_no_aot: bool = False,
    budget_label: str | None = None,
    acceptance: dict[str, list[str]] | None = None,
    smoke_selection: list[str] | None = None,
    reasons: list[tuple[str, str]] | None = None,
) -> None:
    """Print the routing, the node plan, the worker plan and the python-tests files; run nothing.
    ACCEPTANCE (files per step, empty: all) and SMOKE_SELECTION replace the routing of FILES;
    REASONS (unit, why) are the units of a rerun."""
    if reasons:
        print("rerun units:")
        for unit, why in reasons:
            print(f"  {unit} <- {why}")
    if per_file:
        print("changed files:")
        for path in files:
            label, wanted = route(path)
            note = f"  [{origin[path]}]" if origin and path in origin else ""
            print(f"  {path} -> {label} -> {', '.join(wanted) or 'no steps'}{note}")
    print(f"steps: {', '.join(steps) or 'nothing to run'}")
    if per_file and steps and "smoke" not in steps and (reason := smoke_reason(files)):
        print(f"smoke: skipped, {reason}")
    elif per_file and "smoke" in steps and (cells := smoke_cells(files)) is not None:
        print(
            "smoke: selected dependent matrix cells for audited production leaves: "
            + "; ".join(cells)
        )
    elif per_file and "smoke" in steps:
        conservative = [
            path
            for path in files
            if path not in SMOKE_EXEMPT
            and not verify_route.patchkit_no_smoke(path)
            and not ui_only_path(path)
            and not host_test_path(path)
            and not test_only_path(path)
            and path not in LINT_CONFIG_ONLY
            and path not in COMMENT_ONLY
            and path not in PACKAGE_SCRIPTS_ONLY
            and path not in verify_route.PROJECT_REGISTRATIONS
            and not verify_route.outside_smoke_harness(path)
        ]
        if conservative:
            print(f"smoke: selected, conservative path: {', '.join(conservative)}")
    if not per_file and "smoke" in steps:
        if smoke_selection:
            print(
                "smoke: selected dependent matrix cells: " + "; ".join(smoke_selection)
            )
        else:
            print("smoke: full matrix for explicit/full/rerun acceptance")
    for step in verify_route.ACCEPTANCE_DIRS:
        if step in steps:
            selected = (
                acceptance.get(step, [])
                if acceptance is not None
                else acceptance_plan(step, files)
            )
            print(
                f"{step}: native Vitest files (relevant changes, or all for broad/explicit acceptance): "
                + ", ".join(selected or ["all"])
            )
            if step == "module-load" or (
                step == "runtime-fixture" and runtime_pack_modules(selected)
            ):
                print(
                    f"{step}: prerequisite Nx run-many (pack targets, dist/modules); "
                    "also builds the native patchkit CLI used for store dump; "
                    "may build module toolchains, never emulator daemons"
                )
            if step == "runtime-fixture" and (
                not selected
                or verify_route.runtime_plusdrive_files().intersection(selected)
            ):
                print(
                    f"{step}: prerequisite plusdrive CLI build (Nx target plusdrive-format:cli)"
                )
    if "rust-tests" in steps:
        scope = ", ".join(rust_packages) if rust_packages else "whole workspace"
        print(f"rust-tests packages: {scope}")
    nodes, deps = plan(steps, [p for p, _ in py_plan])
    print("plan:")
    for node in nodes:
        print(f"  {node} <- {', '.join(deps[node]) or 'no dependencies'}")
    print("\n".join(worker_plan_lines(nodes, deps, budget, budget_label)))
    if any(s in steps for s in NO_AOT_STEPS):
        print(
            f"no-AOT comparison: {'all scenarios (--all-no-aot)' if all_no_aot else 'the fixed subset (NO_AOT_SUBSET)'}"
        )
    if "python-tests" in steps:
        source_of = dict(py_plan)
        rows = python_files([p for p, _ in py_plan])
        known = python_file_seconds([rel for rel, _ in rows])
        print(
            f"python-tests: {len(rows)} files, one job each, longest first (seconds of the newest earlier run):"
        )
        for path, sec, seen in verify_times.longest_first(
            [rel for rel, _ in rows], known
        ):
            when = f"{sec:.0f} s" if seen else "no time recorded"
            print(f"  {path} ({source_of[dict(rows)[path]]}, {when})")


def run_python_file(gate: Gate, rel: str) -> None:
    """One python-tests file: its own pytest process on the job's share (test_outcomes.run_one). The
    record is kept for finish_python, which judges the step; a crash is a record too."""
    out = gate.out / "python-tests"
    started = time.monotonic()
    with gate.lock:
        if gate.py_started is None:
            gate.py_started = started
    try:
        with benchlock.hold("shared", f"verify {rel}"):
            rec = test_outcomes.run_one(
                ROOT, out, rel, [], python_timeout(), python_env(gate.step_env())
            )
    except BaseException as exc:  # a crashed job is a failed file, not a lost one
        rec = {
            "file": rel,
            "status": "CRASH",
            "rc": -1,
            "seconds": time.monotonic() - started,
            "counts": None,
            "xml": out / "xml" / f"{test_outcomes.slug_of(rel)}.xml",
        }
        gate.say(f"crash {rel}: {type(exc).__name__}: {exc}")
    with gate.lock:
        gate.py_records[rel] = rec
    gate.say(test_outcomes.record_line(rec))


def finish_python(gate: Gate) -> None:
    """The python-tests step once every file job ended: summary.tsv and all.xml, then the verdict (a
    failed or lost file, or a run that passed no test, fails the step)."""
    out = gate.out / "python-tests"
    out.mkdir(parents=True, exist_ok=True)
    records = [gate.py_records[f] for f in gate.py_files if f in gate.py_records]
    lost = [f for f in gate.py_files if f not in gate.py_records]
    bad = test_outcomes.write_reports(out, records, python_timeout()) if records else []
    text = "\n".join(test_outcomes.record_line(r) for r in records)
    (gate.out / "python-tests.log").write_text(text + "\n")
    ran = [
        (n, c) for n, c in PY_CHECKS if gate.py_checks is None or n in gate.py_checks
    ]
    for label, cmd in ran:
        gate.run(f"python-tests:{label}", cmd)
    gate.py_checks_ran = True
    checks = [n for n in gate.failed if n.startswith("python-tests:")]
    problem: str | None = None
    if checks:
        problem = f"checks failed: {', '.join(checks)}"
    elif bad or lost:
        shown = ", ".join([*bad, *lost][:10])
        problem = f"{len(bad) + len(lost)} files failed: {shown}"
    elif records:
        problem = python_tests_passed(out)
    elif not gate.py_files and ran:  # a rerun of failed checks alone
        problem = None
    else:
        problem = "no test file ran"
    began = gate.py_started if gate.py_started is not None else time.monotonic()
    gate.report("python-tests", problem, time.monotonic() - began, text)


NX_PACK_JOBS = 3  # Nx packs at once in the module-load and runtime-fixture prepare node


def nx_pack(
    gate: Gate, step: str, targets: str, projects: list[str], plusdrive: bool = False
) -> None:
    """Build and pack the DKMs (dist/modules, dist/patchkit, dist/plusdrive) with one Nx run-many."""
    if projects:
        gate.run(
            f"{step}:pack",
            [
                "pnpm",
                "nx:pilot",
                "run-many",
                "-t",
                targets,
                "-p",
                ",".join(projects),
                "--parallel",
                str(NX_PACK_JOBS),
            ],
            cwd=ROOT,
        )
    if plusdrive:
        gate.run(
            f"{step}:plusdrive",
            ["pnpm", "nx:pilot", "run", "plusdrive-format:cli"],
            cwd=ROOT,
        )
    if any(n.startswith(f"{step}:") for n in gate.failed):
        raise RuntimeError(f"{step} preparation failed; see pack logs")


def runtime_pack_modules(selected: list[str]) -> list[str]:
    """Runtime fixture modules to pack for the SELECTED test files (empty: all files)."""
    return [
        mid
        for mid, files in verify_route.runtime_modules().items()
        if not selected or set(files) & set(selected)
    ]


def run_step(gate: Gate, step: str) -> None:
    jobs = gate.step_env()[
        "DIGI_JOBS"
    ]  # coupled-run processes at once: the node's share
    if step == "build":
        gate.run("build", [*PY, "-m", "tools.emulator.coupled", "build"])
    elif step == "build-dt2":
        gate.run(
            "build-dt2",
            [*PY, "-m", "tools.emulator.coupled", "build", "--device", "dt2"],
        )
    elif step == "build-daemons":
        env_file = gate.out / "build-daemons.env.json"
        gate.run(
            "build-daemons",
            [
                *PY,
                "-m",
                "tools.devtools.verify_build",
                "daemons",
                "--env-out",
                str(env_file),
            ],
        )
        if (
            env_file.is_file()
        ):  # the pinned PGO daemons: the later nodes run these copies
            gate.shared_env.update(json.loads(env_file.read_text()))
    elif step == "desktop":
        desktop_check(gate)
    elif step == "host":
        host_check(gate)
    elif step == "web":
        web_check(gate)
    elif step in verify_route.ACCEPTANCE_DIRS:
        if step != "stock" and gate.share() < verify_budget.EMULATOR_CORES:
            raise RuntimeError(f"{step} requires a shared budget of at least two cores")
        selected = getattr(gate, "acceptance", {}).get(step, [])
        env = gate.step_env()
        if step in ("runtime-fixture", "module-load"):
            env["DIGI_PATCHKIT"] = str(ROOT / "dist/patchkit/patchkit")
        if step == "stock":
            # Stock uses one stepped ColdFire thread per worker and the DT2 binary only.
            if binary := env.get("DIGI_EMUD_DT2"):
                env["DIGI_EMUD"] = binary
            env["REGRESSION_JOBS"] = str(gate.share())
        else:
            # One coupled daemon (two cores) per Vitest worker.
            env["REGRESSION_JOBS"] = str(
                verify_budget.workers_for(step, gate.share()).emulators
            )
        if step == "module-load":
            modules = list(gate.conformance or verify_route.RUNTIME_UPLOADS)
            env["DIGI_CONFORMANCE_MODULES"] = ",".join(modules)
            if "digikit/debug" not in modules:
                modules.append(
                    "digikit/debug"
                )  # every cell selects it next to its module (the log)
            nx_pack(gate, step, "pack", modules)
        plusdrive = step == "runtime-fixture" and (
            not selected
            or verify_route.runtime_plusdrive_files().intersection(selected)
        )
        if step == "runtime-fixture":
            modules = runtime_pack_modules(selected)
            if modules:
                # Alias owners are not in the route data: every project with a pack-aliases target.
                owners = [
                    name
                    for name, node in verify_route.nx_graph()["nodes"].items()
                    if "pack-aliases" in node["data"].get("targets", {})
                ]
                nx_pack(
                    gate,
                    step,
                    "pack,pack-aliases",
                    sorted({*modules, *owners}),
                    bool(plusdrive),
                )
            elif plusdrive:
                nx_pack(gate, step, "", [], True)
        if plusdrive:
            env["DIGI_PLUSDRIVE"] = str(ROOT / "dist/plusdrive/plusdrive")
        script = {
            "stock": "test:stock",
            "runtime-fixture": "test:runtime",
            "module-load": "test:conformance",
        }[step]
        env["REGRESSION_JUNIT_FILE"] = str(gate.out / f"{step}.junit.xml")
        gate.run(
            step, ["pnpm", "--filter", "@digi/regression", script, *selected], env=env
        )
    elif step == "pins":
        cmd = [
            *PY,
            "-m",
            "tools.emulator.coupled",
            "scenario",
            "--all",
            "--no-build",
            "--jobs",
            jobs,
        ]
        gate.run("pins", cmd)
    elif step == "check":
        gate.run(
            "check-fx",
            ["pnpm", "--filter", "@digi/regression", "test:fx"],
            env={
                **os.environ,
                "DIGI_FX_NO_AOT_SUBSET": "0",
                "DIGI_FX_SWEEP": "1"
                if os.environ.get("DIGIKIT_TIER") == "full"
                else "0",
            },
        )
    elif step == "check-no-aot":
        gate.run(
            "check-no-aot-fx",
            ["pnpm", "--filter", "@digi/regression", "test:fx"],
            env={
                **no_aot_env(),
                "DIGI_FX_NO_AOT_SUBSET": "0" if gate.all_no_aot else "1",
                "DIGI_FX_SWEEP": "1"
                if gate.all_no_aot and os.environ.get("DIGIKIT_TIER") == "full"
                else "0",
            },
        )
    elif step == "tier-quick":
        tier(gate, "tier-quick")
    elif step == "tier-sweep":
        tier(gate, "tier-sweep")
    elif step == "rust-tests":
        gate.run("rust-tests", rust_tests_command(gate.rust_packages))
    elif step == "card":
        gate.run("card", [*PY, "-m", "tools.emulator.card_check"])
        # The firmware's own view of a folder-built image (nested folders, plain files).
        gate.run("card-fs", [*PY, "-m", "tools.firmware.plusdrive_check"])
        # The module store through the firmware's block read, on the DN2 too.
        gate.run("card-fs-dn2", [*PY, "-m", "tools.firmware.plusdrive_check", "--dn2"])
    elif step == "smoke":
        # patchkit smoke matrix: one cell = one digi-emud daemon, so the share's emulator processes are the cells at once
        cmd = [*PY, "-m", "tools.patchkit.pk_harness", "run", "--jobs", jobs]
        cells = getattr(gate, "smoke_selection", None)
        if cells == []:
            raise RuntimeError(
                "empty smoke selection must skip the smoke node, not run the full matrix"
            )
        for cell in cells or []:
            cmd.extend(("--cell", cell))
        gate.run("smoke", cmd)
    elif step == "dt2-tone":
        gate.run("dt2-tone", [*PY, "-m", "tools.emulator.dt2_sample_check"])
    elif step == "dt2-check":
        gate.run(
            "dt2-check",
            ["pnpm", "--filter", "@digi/regression", "test:fx:dt2"],
            env={
                **os.environ,
                "DIGI_FX_NO_AOT_SUBSET": "0",
                "DIGI_FX_SWEEP": "1"
                if os.environ.get("DIGIKIT_TIER") == "full"
                else "0",
            },
        )
    elif step == "dt2-tier-quick":
        tier(gate, "dt2-tier-quick")
    elif step == "dt2-tier-sweep":
        tier(gate, "dt2-tier-sweep")


def run_steps(gate: Gate, steps: tuple[str, ...]) -> list[tuple[str, float]]:
    """Run the steps after the builds they need, within the CPU budget (gate.budget).

    A node starts when its dependencies passed and the budget admits it (verify_budget.Budget): it
    gets a share of the cores, which sets its inner pools, and the shares of the running nodes never
    add up to more than the budget. python-tests is one job per file (expand_python) in the same
    queue, longest first. Each step's output is buffered and printed whole when it finishes. A step
    whose prerequisite failed is skipped. Returns (node, seconds) in completion order.
    Writes jobs.tsv (node, start, end, share, load average at start and end) to the run directory."""
    nodes, deps = plan(steps, gate.py_tests)
    gate.py_files = [rel for rel, _ in python_files(gate.py_tests)]
    nodes, deps = expand_python(
        nodes,
        deps,
        gate.py_tests,
        python_file_seconds(gate.py_files, gate.out),
        step_seconds(
            [n for n in nodes if n not in BUILD_CHAIN and n != "python-tests"],
            gate.out,
        ),
    )
    budget = verify_budget.Budget(
        max(1, gate.budget),
        files={
            step: len(selected)
            for step, selected in getattr(gate, "acceptance", {}).items()
            if selected and step in verify_budget.COUPLED_ACCEPTANCE
        },
    )
    done: set[str] = set()
    failed_nodes: set[str] = set()
    pending = list(nodes)
    running: dict[object, tuple[str, float, int, float]] = {}
    times: list[tuple[str, float]] = []
    t0 = time.monotonic()
    jobs: list[list[str]] = []

    def work(node: str, share: int) -> list[str]:
        gate.local.lines = []
        gate.local.node = node
        gate.local.share = share
        try:
            if verify_budget.is_file_job(node):
                run_python_file(gate, node.removeprefix(verify_budget.FILE_PREFIX))
            else:
                run_step(gate, node)
        except BaseException as exc:  # a crashed step is a failed step
            with gate.lock:
                gate.failed.append(node)
            gate.say(f"FAIL {node}: {type(exc).__name__}: {exc}")
        return gate.local.lines

    with ThreadPoolExecutor(
        max_workers=verify_budget.MAX_STEPS + max(1, gate.budget)
    ) as pool:
        while pending or running:
            queued = len(pending)
            for node in list(pending):
                if any(d in failed_nodes for d in deps[node]):
                    pending.remove(node)
                    failed_nodes.add(node)
                    if not verify_budget.is_file_job(node):
                        print(f"SKIP {node}: a prerequisite failed", flush=True)
                    with gate.lock:
                        gate.failed.append(node)
                elif node == "python-tests" and all(d in done for d in deps[node]):
                    pending.remove(node)  # judges the files: no cores
                    finish_python(gate)
                    (failed_nodes if "python-tests" in gate.failed else done).add(node)
                    began = gate.py_started
                    times.append(
                        (node, time.monotonic() - began if began is not None else 0.0)
                    )
            started = next_starts(
                [n for n in pending if n != "python-tests"], deps, done, budget
            )
            for node, share in started:
                pending.remove(node)
                if not verify_budget.is_file_job(node):
                    print(
                        f"START {node}: {share} of {budget.total} cores "
                        f"({verify_budget.describe(node, share)}), "
                        f"log {gate.out / f'{node}.log'}",
                        flush=True,
                    )
                running[pool.submit(work, node, share)] = (
                    node,
                    time.monotonic(),
                    share,
                    os.getloadavg()[0],
                )
            if not running:
                if pending and not started and len(pending) == queued:
                    for node in pending:  # a dependency is neither done nor running
                        print(f"SKIP {node}: it can never start", flush=True)
                        with gate.lock:
                            gate.failed.append(node)
                    break
                continue
            finished, _ = wait(list(running), return_when=FIRST_COMPLETED)
            for fut in finished:
                node, began, node_share, load_start = running.pop(fut)
                budget.release(node)
                lines = fut.result()
                jobs.append(
                    [
                        node,
                        f"{began - t0:.1f}",
                        f"{time.monotonic() - t0:.1f}",
                        str(node_share),
                        f"{load_start:.1f}",
                        f"{os.getloadavg()[0]:.1f}",
                    ]
                )
                print("\n".join(lines), flush=True)
                if not verify_budget.is_file_job(node):
                    times.append((node, time.monotonic() - began))
                if any(line.startswith("FAIL") for line in lines):
                    failed_nodes.add(node)
                    if not verify_budget.is_file_job(node):
                        with gate.lock:
                            if node not in gate.failed:
                                gate.failed.append(node)
                else:
                    done.add(node)
    header = "node\tstart_s\tend_s\tshare\tload_start\tload_end"
    (gate.out / "jobs.tsv").write_text(
        "\n".join([header, *("\t".join(row) for row in jobs)]) + "\n"
    )
    return times


def load_last_result(verify_dir: Path, exclude: Path | None) -> dict | None:
    """The newest (by directory name) result.json under VERIFY_DIR, skipping EXCLUDE and runs
    whose "root" is another worktree (a result.json without "root" counts)."""
    if not verify_dir.is_dir():
        return None
    for run in sorted(verify_dir.iterdir(), key=lambda p: p.name, reverse=True):
        if exclude is not None and run.resolve() == exclude.resolve():
            continue
        path = run / "result.json"
        if run.is_dir() and not run.is_symlink() and path.is_file():
            result = json.loads(path.read_text())
            if result.get("root", str(ROOT)) != str(ROOT):
                continue
            return result
    return None


def junit_failed_files(path: Path) -> list[str]:
    """The test files with a failed or errored case in a Vitest JUnit report, relative to
    tests/regression. Vitest names the file (relative to its root) in the testsuite name and in
    the classname of each case; a suite with failures but no failed case counts by its name."""
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError):
        return []
    found: set[str] = set()
    for suite in root.iter("testsuite"):
        bad = [
            case
            for case in suite.iter("testcase")
            if case.find("failure") is not None or case.find("error") is not None
        ]
        names = {case.get("classname") or suite.get("name") for case in bad}
        if not bad and (
            int(suite.get("failures") or 0) or int(suite.get("errors") or 0)
        ):
            names = {suite.get("name")}
        for name in names:
            rel = (name or "").split(" > ")[0].removeprefix("tests/regression/")
            if rel.endswith(".test.ts"):
                found.add(rel)
    return sorted(found)


def failed_units(gate: Gate) -> dict[str, list[str]]:
    """The failed files of the steps a rerun can repeat alone: python-tests files (failed or never
    run) and the Vitest files of a failed acceptance step. A step whose files are unknown has no
    entry, so a rerun repeats it whole. Smoke has none: pk_harness writes no per-cell result."""
    units: dict[str, list[str]] = {}
    if "python-tests" in gate.failed:
        lost = [f for f in gate.py_files if f not in gate.py_records]
        files = [*verify_times.failed_files(gate.out), *lost]
        if gate.py_checks_ran:
            # the failed checks alone: a rerun then needs no file that passed
            units["python-tests:checks"] = [
                n.removeprefix("python-tests:")
                for n in gate.failed
                if n.startswith("python-tests:")
            ]
            units["python-tests"] = files
        elif files:
            units["python-tests"] = files
    for step in verify_route.ACCEPTANCE_DIRS:
        if step in gate.failed and (
            files := junit_failed_files(gate.out / f"{step}.junit.xml")
        ):
            units[step] = files
    return units


@dataclass
class Rerun:
    """What --rerun-failed runs: the steps, python-tests entries, files per acceptance step (empty:
    all), smoke cells (None: the matrix), Rust packages (None: the workspace), and why each unit is there."""

    steps: tuple[str, ...]
    py_tests: list[str]
    acceptance: dict[str, list[str]]
    smoke: list[str] | None
    rust_packages: list[str] | None
    reasons: list[tuple[str, str]]
    py_checks: list[str] | None = None  # the PY_CHECKS to run (None: all)


def changed_plan(
    base: str, head: str | None = None
) -> tuple[list[str], dict[str, str], tuple[str, ...], list[str] | None]:
    """(files, origin notes, steps, Rust packages) the changes since BASE route to."""
    files, origin = expand_mise(changed_files(base, head), base, head)
    files, lock_origin = expand_lock(files, base, head)
    origin |= lock_origin
    files = drop_metadata_objects(files, base, head)
    LINT_CONFIG_ONLY.clear()
    if "pyproject.toml" in files:
        since = merge_base(base, head)
        if verify_route.lint_config_only(
            git_show(since, "pyproject.toml"), git_show(head, "pyproject.toml")
        ):
            LINT_CONFIG_ONLY.add("pyproject.toml")
    PACKAGE_SCRIPTS_ONLY.clear()
    since = merge_base(base, head)
    if "package.json" in files and verify_route.package_scripts_only(
        git_show(since, "package.json"), git_show(head, "package.json")
    ):
        PACKAGE_SCRIPTS_ONLY.add("package.json")
    verify_route.PROJECT_REGISTRATIONS.clear()
    for path in files:
        if not (
            path.startswith("modules/")
            and path.endswith("/project.json")
            and (mid := modroots.module_id(path))
            and (ROOT / path).parent == modroots.module_dir(mid)
        ):
            continue
        new = git_show(head, path)
        if new is None:
            continue  # deleted: the module route stays
        found = verify_route.project_record_files(path, new)
        found |= verify_route.project_record_files(path, git_show(since, path))
        verify_route.PROJECT_REGISTRATIONS[path] = tuple(sorted(found))
    COMMENT_ONLY.clear()
    for path in files:
        if path.endswith(".py") and verify_route.python_code_unchanged(
            git_show(since, path), git_show(head, path)
        ):
            COMMENT_ONLY.add(path)
    steps = steps_for(files)
    rust_packages: list[str] | None = None
    if "rust-tests" in steps:
        rust_packages = rust_scope(files)
        if rust_packages == []:  # no changed file reaches a Rust test
            steps = tuple(s for s in steps if s != "rust-tests")
        steps = with_desktop(steps, rust_packages)
    return files, origin, steps, rust_packages


def rerun_plan(
    result: dict,
    changed: tuple[list[str], tuple[str, ...], list[str] | None] | None = None,
) -> Rerun:
    """The failed units of RESULT plus what CHANGED (files, steps, Rust packages of changed_plan
    since the failed run's head) routes to."""
    failed = tuple(result.get("failed", []))
    unknown = sorted(set(failed) - set(STEPS))
    if unknown:
        raise SystemExit(f"verify: unknown steps in last result: {', '.join(unknown)}")
    files, new_steps, new_rust = changed or ([], (), None)
    failed_why = f"failed in {result.get('run', '?')}"
    changed_why = f"changed since {str(result.get('head', '?'))[:10]}"
    units = result.get("failed_units", {})
    steps = tuple(s for s in STEPS if s in failed or s in new_steps)
    reasons = [(s, failed_why if s in failed else changed_why) for s in steps]
    py_tests: list[str] = []
    py_checks: list[str] | None = None
    if "python-tests" in failed:
        if "python-tests:checks" in units:  # the run knew which checks and files failed
            py_checks = list(units["python-tests:checks"])
            py_tests = list(units.get("python-tests", []))
        else:
            py_tests = list(units.get("python-tests") or result.get("py_tests", []))
        reasons += [(f"python-tests: {p}", failed_why) for p in py_tests]
        reasons += [(f"python-tests:{c}", failed_why) for c in py_checks or []]
    if "python-tests" in new_steps:
        py_checks = None  # changed code can break any check
        for path, _ in python_test_plan(files, False):
            if path not in py_tests:
                py_tests.append(path)
                reasons.append((f"python-tests: {path}", changed_why))
    acceptance: dict[str, list[str]] = {}
    for step in verify_route.ACCEPTANCE_DIRS:
        if step not in steps:
            continue
        named: dict[str, str] = {}
        everything = False
        if step in failed:
            if units.get(step):
                named.update({f: failed_why for f in units[step]})
            else:
                everything = True
        if step in new_steps:
            if selected := acceptance_plan(step, files):
                named.update({f: named.get(f, changed_why) for f in selected})
            else:
                everything = True
        acceptance[step] = [] if everything else sorted(named)
        if not everything:
            reasons += [(f"{step}: {f}", named[f]) for f in sorted(named)]
    smoke = None if "smoke" in failed else smoke_cells(files) if files else None
    rust = None if "rust-tests" in failed else new_rust
    return Rerun(steps, py_tests, acceptance, smoke, rust, reasons, py_checks)


def named_units(
    py_tests: list[str], tests: list[str]
) -> tuple[list[str], dict[str, list[str]]]:
    """The checked --py-tests files and the --tests files by acceptance step (by their directory in
    tests/regression, tests/conformance or a registered module test); ValueError names the first bad
    file. Files are repo-relative or relative to tests/regression."""
    for path in py_tests:
        if not (ROOT / path).is_file():
            raise ValueError(f"--py-tests: no such file: {path}")
    by_step: dict[str, list[str]] = {}
    registered = {
        file.removeprefix("../../"): file
        for file in verify_route.acceptance_records()
        if file.startswith("../../")
    }
    for path in tests:
        if path in registered:
            by_step.setdefault("runtime-fixture", []).append(registered[path])
            continue
        if path.startswith("tests/conformance/"):
            path = os.path.relpath(path, "tests/regression")
        else:
            path = path.removeprefix("tests/regression/")
        if Path(path).is_absolute() or (
            ".." in Path(path).parts and not path.startswith("../conformance/")
        ):
            raise ValueError(
                f"--tests: not a registered module test, emulator, conformance or stock path: {path}"
            )
        if not (ROOT / "tests/regression" / path).is_file():
            raise ValueError(
                f"--tests: no such file (tests/regression, tests/conformance or a registered module test): {path}"
            )
        step = next(
            (
                s
                for s, d in verify_route.ACCEPTANCE_DIRS.items()
                if path.startswith(d + "/")
            ),
            None,
        )
        if step is None:
            dirs = ", ".join(verify_route.ACCEPTANCE_DIRS.values())
            raise ValueError(f"--tests: {path} is not under one of: {dirs}")
        by_step.setdefault(step, []).append(path)
    return list(py_tests), by_step


def missing_daemons(steps: tuple[str, ...], py_tests: list[str]) -> list[str]:
    """The DIGI_EMUD_<DEV> variables that are unset or name no file, when the plan has steps that
    drive daemons but no build-daemons (--only never builds them). Stock needs the DT2 binary only."""
    nodes, _ = plan(steps, py_tests)
    if "build-daemons" in nodes:
        return []
    users = [
        n
        for n in nodes
        if n in DAEMON_NODES
        and (
            n != "python-tests"
            or any(needs_daemon(f) for f, _ in python_files(py_tests))
        )
    ]
    if not users:
        return []
    devices = ("DT2",) if users == ["stock"] else ("DT2", "DN2")
    return [
        f"DIGI_EMUD_{d}"
        for d in devices
        if not os.path.isfile(os.environ.get(f"DIGI_EMUD_{d}", ""))
    ]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Run the exactness gate for what changed, one line per step."
    )
    ap.add_argument(
        "--base",
        default="HEAD",
        help="compare with the merge base of this ref (default: HEAD)",
    )
    ap.add_argument(
        "--head",
        help="with --explain: the commits BASE..HEAD instead of the working tree "
        "(files are read from the current tree)",
    )
    ap.add_argument(
        "--full",
        action="store_true",
        help="full acceptance including runtime-fixture and tier-sweep (DT2 full sweep with --dt2)",
    )
    ap.add_argument(
        "--only",
        help=f"restrict the diff-derived plan to comma-separated steps from: {', '.join(STEPS)}",
    )
    ap.add_argument(
        "--all-files",
        action="store_true",
        help="with --only: run the requested suites in full, without changing the tier",
    )
    ap.add_argument(
        "--dt2",
        action="store_true",
        help="with --full: the full DT2 FX tier sweep (about 35 min more)",
    )
    ap.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="the CPU budget of the whole gate in cores (default: DIGI_VERIFY_BUDGET, else the "
        "physical cores - 2); the running steps share it and it sets their inner pools; 1 is sequential",
    )
    ap.add_argument(
        "--foreground",
        action="store_true",
        help="run at normal priority (default: nice 10, the whole gate and its children)",
    )
    ap.add_argument(
        "--all-no-aot",
        action="store_true",
        help="AOT = no-AOT equality on every FX scenario (default: the fixed NO_AOT_SUBSET; "
        "tier-sweep all 134, tier-quick every seventh, check-no-aot all 61 oracle tests)",
    )
    ap.add_argument(
        "--explain",
        action="store_true",
        help="print the routing, the plan and the python-tests files; run nothing",
    )
    ap.add_argument(
        "--rerun-failed",
        action="store_true",
        help="the units that failed in the most recent run (failed python-tests files, failed Vitest "
        "files) plus what changed since that run's HEAD",
    )
    ap.add_argument(
        "--py-tests",
        nargs="+",
        metavar="FILE",
        help="python-tests runs only these repo-relative test files (implies the step without --only)",
    )
    ap.add_argument(
        "--tests",
        nargs="+",
        metavar="FILE",
        help="Vitest files: registered module device tests (modules/.../tests/X.device.test.ts), "
        "tests/regression/emulator/<name>.test.ts, tests/regression/dt2/<name>.test.ts or tests/conformance/load-dt2.device.test.ts or load-dn2.device.test.ts; "
        "each runs in the acceptance step of its directory (implies those steps without --only)",
    )
    ap.add_argument(
        "--conformance",
        metavar="ID[,ID...]",
        help="run only the module-load step (the shared conformance suite) for these module ids",
    )
    ap.add_argument(
        "--no-cache",
        action="store_true",
        help="render every scenario (DIGI_RENDER_CACHE=0): the release gate",
    )
    args = ap.parse_args(argv)
    if args.rerun_failed and (args.only or args.full):
        ap.error("--rerun-failed cannot be combined with --only or --full")
    conformance_ids = [m for m in (args.conformance or "").split(",") if m]
    if args.conformance is not None:
        bad = [m for m in conformance_ids if m not in verify_route.RUNTIME_UPLOADS]
        if bad or not conformance_ids:
            ap.error(
                f"--conformance: not an eligible module (shipped dynamic, service or host): {', '.join(bad) or '(none given)'}"
            )
    named = bool(args.py_tests or args.tests or conformance_ids)
    if args.only and args.full:
        ap.error("--only cannot be combined with --full")
    if args.all_files and (not args.only or named or args.full or args.rerun_failed):
        ap.error(
            "--all-files requires --only and cannot be combined with named files, --full or --rerun-failed"
        )
    if named and (args.rerun_failed or args.full):
        ap.error(
            "--py-tests and --tests cannot be combined with --rerun-failed or --full"
        )
    if args.head and not args.explain:
        ap.error("--head only explains a range: add --explain")
    named_py: list[str] = []
    named_acceptance: dict[str, list[str]] = {}
    try:
        named_py, named_acceptance = named_units(args.py_tests or [], args.tests or [])
    except ValueError as exc:
        ap.error(str(exc))
    if conformance_ids:
        named_acceptance.setdefault("module-load", []).extend(
            os.path.relpath(f, "tests/regression")
            for f in verify_route.CONFORMANCE_FILES
        )
    implied = (["python-tests"] if named_py else []) + list(named_acceptance)
    files: list[str] = []
    origin: dict[str, str] = {}
    rust_packages: list[str] | None = None
    full, dt2 = args.full, args.dt2
    if full:  # tests/digikit: every scenario (the gate runs the quick tier)
        os.environ["DIGIKIT_TIER"] = "full"
    # The head the run starts on: a commit that lands during the run must not
    # replace it in result.json, or --rerun-failed misses what changed since.
    start_head = git_lines("rev-parse", "HEAD")[0]
    rerun: Rerun | None = None
    if args.rerun_failed:
        last = load_last_result(Path(digi_env.RUNS_ROOT) / "verify", None)
        if last is None:
            print("verify: no previous run with a result.json")
            return 2
        if not last.get("failed"):
            print("verify: the last run had no failed steps; nothing to do")
            return 0
        full, dt2 = bool(last.get("full")), bool(last.get("dt2"))
        old, new = str(last.get("head", "?")), start_head
        changed = None
        if old != new:
            try:
                new_files, origin, new_steps, new_rust = changed_plan(old)
            except subprocess.CalledProcessError:
                print(
                    f"verify: cannot compare with {old[:10]}; re-running the failed units only"
                )
            else:
                print(
                    f"verify: code changed since the failed run ({old[:10]} -> "
                    f"{new[:10]}); adding what the {len(new_files)} changed files route to"
                )
                changed = (new_files, new_steps, new_rust)
        rerun = rerun_plan(last, changed)
        steps, rust_packages = rerun.steps, rerun.rust_packages
        why = "--rerun-failed"
    elif args.only:
        requested = tuple(args.only.split(","))
        unknown = sorted(set(requested) - set(STEPS))
        if unknown:
            ap.error(f"unknown steps: {', '.join(unknown)}")
        for step in implied:
            if step not in requested:
                ap.error(f"--py-tests/--tests need the step {step}: add it to --only")
        why = "--only"
        if args.all_files:
            steps = requested
        else:
            files, origin, routed_steps, rust_packages = changed_plan(
                args.base, args.head
            )
            steps = tuple(s for s in requested if s in routed_steps or s in implied)
    elif named:
        steps = tuple(s for s in STEPS if s in implied)
        why = "named files"
    elif args.full:
        skip = {"tier-quick", "dt2-tier-quick" if args.dt2 else "dt2-tier-sweep"}
        steps = tuple(s for s in STEPS if s not in skip)
        why = "--full"
    else:
        files, origin, steps, rust_packages = changed_plan(args.base, args.head)
        why = f"{len(files)} changed files since {args.base}"
    if rerun is not None:
        py_plan = [(path, "rerun") for path in rerun.py_tests]
        acceptance = rerun.acceptance
        smoke_selection = rerun.smoke
    else:
        if named_py:
            py_plan = [(path, "named") for path in named_py]
        elif "python-tests" in steps:
            py_plan = python_test_plan(files, args.full or args.all_files)
        else:
            py_plan = []
        routed = not (args.full or args.all_files or why == "named files")
        acceptance = (
            {s: acceptance_plan(s, files) for s in verify_route.ACCEPTANCE_DIRS}
            if routed
            else {}
        )
        acceptance.update(named_acceptance)
        smoke_selection = smoke_cells(files) if routed else None
    budget = verify_budget.resolve_budget(args.jobs)
    budget_label = verify_budget.budget_text(args.jobs)
    if args.explain:
        explain(
            steps,
            files,
            py_plan,
            per_file=routed if rerun is None else False,
            origin=origin,
            rust_packages=rust_packages,
            budget=budget,
            all_no_aot=args.all_no_aot,
            budget_label=budget_label,
            acceptance=acceptance,
            smoke_selection=smoke_selection,
            reasons=rerun.reasons if rerun else None,
        )
        return 0
    if missing := missing_daemons(steps, [path for path, _ in py_plan]):
        print(
            f"verify: the plan runs steps that drive digi-emud daemons without building them, "
            f"and {', '.join(missing)} is unset or names no file. Build the daemons with "
            "`uv run python -m tools.devtools.verify_build daemons --env-out FILE` and export "
            "the variables it writes, or run a gate whose plan has build-daemons "
            "(`mise run verify`; --only never builds them).",
            file=sys.stderr,
        )
        return 2
    niced = verify_budget.lower_priority(args.foreground)
    # Every step, and no_aot_env(), inherits os.environ: set the cache env once.
    os.environ.update(cache_env(None, args.no_cache))
    out = Path(digi_env.run_dir("verify"))
    out.mkdir(parents=True, exist_ok=True)
    print(
        f"verify: {', '.join(steps) or 'nothing to run'} ({why}); logs in {out}",
        flush=True,
    )
    gate = Gate(
        out,
        [path for path, _ in py_plan],
        rust_packages,
        all_no_aot=args.all_no_aot,
        budget=budget,
        acceptance=acceptance,
        smoke_selection=smoke_selection,
        py_checks=rerun.py_checks if rerun else None,
    )
    if conformance_ids:
        gate.conformance = conformance_ids
    elif rerun is None and routed and "module-load" in steps:
        gate.conformance = verify_route.conformance_modules(files)
    nodes, deps = plan(steps, gate.py_tests)
    print(
        f"verify: {budget_label}; nice {niced if niced is not None else 'unchanged (--foreground or not allowed)'}; "
        "no-AOT comparison: "
        f"{'all scenarios' if args.all_no_aot else 'the fixed subset'}",
        flush=True,
    )
    print("\n".join(worker_plan_lines(nodes, deps, budget, budget_label)), flush=True)
    own = verify_budget.user_overrides()
    if own:
        print(
            f"verify: your {', '.join(own)} replace the share of the budget", flush=True
        )
    start = time.monotonic()
    times = run_steps(gate, steps)
    for name, seconds in times:
        print(f"  {name:<16} {seconds:6.0f} s", flush=True)
    print(
        f"verify: wall {time.monotonic() - start:.0f} s with a budget of {budget} cores",
        flush=True,
    )
    result = {
        "steps": list(steps),
        "failed": [s for s in gate.failed if s in STEPS],
        "full": full,
        "dt2": dt2,
        "base": args.base,
        "head": start_head,
        "py_tests": gate.py_tests,
        "failed_units": failed_units(gate),
        "run": out.name,
        "root": str(ROOT),
    }
    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    if gate.failed:
        print(f"verify: FAILED: {', '.join(gate.failed)}", flush=True)
        return 1
    print("verify: all passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
