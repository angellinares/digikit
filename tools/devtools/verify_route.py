"""Finer routing for tools/devtools/verify.py: which steps and test files one changed path needs.

verify.py asks `fine(path)` first. A path this module knows gets its own steps and its own test
files; any other path falls through to the prefix rules of verify.py (RULES, PY_SUITES). The
functions read the working tree (module manifests, test files, scenario TOMLs); only
`mise_paths` takes texts, so the mise.toml routing can be tested without git.

Rules (docs/TESTING.md "What verify runs"):

- a module's own tests live in `modules/<group>/<name>/tests/` (test_*.py, scenarios/*.toml, TypeScript *.test.ts): found by globbing,
  selected by any change in the module (and by a change in a module it requires); a change in that tests/ directory alone selects
  only python-tests. Module ids come from the module.toml files, not from a list of authors.
- a file of a module under modules/ or tests/fixtures/modules/: python-tests and rust-tests; web only for the files the web
  gallery bundle reads (module.toml, build/, gallery/, registry tables, not src/, sdk/, tests/).
  Framework modules (FRAMEWORK: every patched image includes them) select all of tests/patchkit
  and tests/digikit. Any other module selects the test files that name it or a module that
  requires it (transitively) and the digikit test files of the scenarios that name it. No PCM step.
- a machine module (a DSP routine behind a `[[voice]]` or a voices `[[entry]]`, tools/patchkit/pk_conform.py), a module that one
  depends on or builds from, and core/dsp-voice (the wrapper every routine runs through) also select the machine conformance
  suite, CONFORMANCE (docs/TESTING.md "Machine conformance"). Its cases for every machine run in one file.
- an effect module (an `[[entry]] registry = "fx"` with a DSP object) and core/fx, which every effect requires, select the FX conformance
  suite, FX_CONFORMANCE (docs/TESTING.md "FX conformance").
- a module of tools/device: the unit tests that import it (and the importers of those) plus the
  digikit test files of the scenario kinds whose runners call it (DEVICE_KINDS).
- a scenario file of tests/digikit/scenarios: the test files that run the scenario's kind.
- `tests/digikit/test_*.py`, `tests/patchkit/test_*.py`: the file itself.
- a Rust file of packages/patchkit/src in a group of PATCHKIT_GROUPS: the steps and tests of the group (checks, build, flash,
  wasm, codegen); any other file of the crate keeps the wide route.
- a scenario runner of tools/patchkit (RUNNER_KINDS): the digikit tests of its kinds and of the runners that import it.
- `mise.toml`: the tasks that changed, routed like the tool their `run` names (`mise_paths`).
"""

from __future__ import annotations

import ast
import json
import os
import re
import shlex
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass
from functools import cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools.patchkit import modroots, pk_conform  # noqa: E402

# Modules that every patched image or every runtime build includes: a change selects the whole
# patchkit and digikit suites.
FRAMEWORK = frozenset(
    {
        "core/runtime",
        "core/dsp-voice",
        "core/dsp-link",
        "core/machines",
        "core/params",
        "core/dynreg",
        "core/dyndsp",
        "core/digikit-menu",
        "core/health",
        "core/input",
        "core/expansion",
    }
)
# tools/device module -> the digikit scenario kinds whose runners (tools/patchkit/pk_digikit_*.py)
# call it.
DEVICE_KINDS: dict[str, tuple[str, ...]] = {
    "__init__": (),
    "dkupload": ("upload", "menu", "debug", "crash", "card"),
    "dksettings": (),  # the settings record CLI; no scenario kind runs it (the TS port is tests/regression/runtime/project-kit.ts)
    "dkrun": ("menu",),
    "dkneeds": (
        "menu",
        "debug",
    ),  # the needs of a .dkm, from `patchkit needs`: dktransfer, dkrun, the `inferred-needs` and `fits` parts of the debug scenarios
    "dktransfer": ("menu",),
    "dkpackall": (),  # a dev task: the files the workbench reads
    "dkscreen": ("upload",),
    "dklog": ("debug", "crash", "card"),
    "dklog_kinds": ("debug", "crash", "card"),  # the event kinds dklog reads
    "dkproto": (
        "upload",
        "menu",
        "debug",
        "crash",
        "card",
    ),  # the protocol numbers dkupload uses
    "devrpc": ("upload",),
    "midisniff": ("upload", "menu", "debug", "crash", "card"),
    "mmon": (),
    "dkprobe": (),  # the SHARC probe capture tool: no scenario kind runs it
    "dkprobe_seq": (),
    "dkprobe_facts": (),
    "dkprobe_run": (),
    "dktiming": (),  # the timing of `dkupload put`; no runner calls it
    "winmidi": (),  # the winmm transport of `dkupload --winmm`; a real device only
}
BROAD_TASK_GROUPS = ("verify", "build", "gen", "setup")
ALL_SUITES = ("tests/patchkit", "tests/digikit")
CONFORMANCE = "tests/modules/test_machine_conformance.py"
CONFORMANCE_WRAPPERS = frozenset({"core/dsp-voice"})
FX_CONFORMANCE = "tests/modules/test_fx_conformance.py"
MODULE_TESTS_GLOB = (
    "modules/*/*/tests"  # a module's own tests: test_*.py, scenarios/*.toml, *.test.ts
)


@dataclass(frozen=True)
class Fine:
    """The steps and pytest paths (files or directories) one path needs."""

    label: str
    steps: tuple[str, ...]
    tests: tuple[str, ...]


@dataclass(frozen=True, eq=False)
class RuntimeAcceptance:
    """One runtime acceptance record of an Nx project (`metadata.runtimeAcceptance`)."""

    owner: str
    files: tuple[str, ...]  # relative to tests/regression
    modules: tuple[str, ...]
    aliases: dict[str, str]  # alias -> source module
    sources: tuple[str, ...]  # repo-relative
    plusdrive: bool


@cache
def nx_graph() -> dict:
    """The Nx project graph, built with a private daemon-free cache; module projects come from tools/nx/modules.mjs."""
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "graph.json"
        env = {
            **os.environ,
            "NX_DAEMON": "false",
            "NX_NO_CLOUD": "true",
            "NX_WORKSPACE_DATA_DIRECTORY": str(Path(tmp) / "data"),
        }
        done = subprocess.run(
            ["mise", "exec", "--", "pnpm", "exec", "nx", "graph", f"--file={out}"],
            cwd=ROOT,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=300,
        )
        if done.returncode:
            raise RuntimeError(
                f"nx graph failed (exit {done.returncode}): {done.stderr[-2000:]}"
            )
        return json.loads(out.read_text())["graph"]


@cache
def manifests() -> dict[str, tuple[str, ...]]:
    """Module id -> the modules it depends on: those it `requires` and those whose files its
    build sources name. The edges are defined in tools/nx/modules.mjs."""
    graph = nx_graph()
    mods = {
        name
        for name, node in graph["nodes"].items()
        if "module" in node["data"].get("tags", [])
    }
    out = {
        name: tuple(
            sorted(
                dep["target"]
                for dep in graph["dependencies"].get(name, [])
                if dep["target"] in mods
            )
        )
        for name in sorted(mods)
    }
    disk = set(modroots.manifests())
    if set(out) != disk:
        raise ValueError(
            f"Nx module projects differ from module manifests: missing in Nx={sorted(disk - set(out))}, missing on disk={sorted(set(out) - disk)}"
        )
    return out


def _invalid(root: str, why: str) -> ValueError:
    return ValueError(f"{root}: invalid runtimeAcceptance {why}")


@cache
def acceptance_records() -> dict[str, RuntimeAcceptance]:
    """Test file (relative to tests/regression) -> its record, read from the `runtimeAcceptance`
    lists in the metadata of the Nx projects. Fails closed on any invalid or missing record."""
    known = manifests()
    out: dict[str, RuntimeAcceptance] = {}
    alias_owner: dict[str, str] = {}
    for name, node in sorted(nx_graph()["nodes"].items()):
        data = node["data"]
        root = data["root"]
        for meta in data.get("metadata", {}).get("runtimeAcceptance", []):
            if not isinstance(meta, dict) or not set(meta) <= {
                "files",
                "modules",
                "aliases",
                "sources",
                "plusdrive",
            }:
                raise _invalid(root, "fields")
            files = meta.get("files")
            modules = meta.get("modules", [])
            aliases = meta.get("aliases", {})
            sources = meta.get("sources", [])
            if (
                not isinstance(files, list)
                or not files
                or not isinstance(modules, list)
                or not isinstance(aliases, dict)
                or not isinstance(sources, list)
                or any(not isinstance(v, str) for v in [*files, *modules, *sources])
                or any(
                    not isinstance(k, str) or not isinstance(v, str)
                    for k, v in aliases.items()
                )
                or type(meta.get("plusdrive", False)) is not bool
            ):
                raise _invalid(root, "types")
            is_module = "module" in data.get("tags", [])
            # An uploadable owner is packed with its tests; a core/* owner is patched into the image.
            if is_module and name in RUNTIME_UPLOADS and name not in modules:
                modules = [*modules, name]
            if (
                len(set(modules)) != len(modules)
                or any(m not in known for m in [*modules, *aliases.values()])
                or any(a in known or a in alias_owner for a in aliases)
            ):
                raise _invalid(root, "modules")
            alias_owner.update(dict.fromkeys(aliases, name))
            for source in sources:
                if (
                    Path(source).is_absolute()
                    or ".." in Path(source).parts
                    or not (ROOT / source).is_file()
                ):
                    raise _invalid(root, f"source {source}")
            keys = []
            for filename in files:
                if (
                    Path(filename).is_absolute()
                    or ".." in Path(filename).parts
                    or not filename.endswith(
                        ".device.test.ts" if is_module else ".test.ts"
                    )
                    or not (ROOT / root / filename).is_file()
                ):
                    raise _invalid(root, f"file {filename}")
                key = Path(
                    os.path.relpath(ROOT / root / filename, ROOT / "tests/regression")
                ).as_posix()
                if key in out or key in keys:
                    raise _invalid(root, f"duplicate file {filename}")
                keys.append(key)
            record = RuntimeAcceptance(
                name,
                tuple(keys),
                tuple(modules),
                dict(aliases),
                tuple(sources),
                meta.get("plusdrive", False),
            )
            out.update(dict.fromkeys(keys, record))
    registered = {
        os.path.normpath(f"tests/regression/{key}")
        for key in out
        if key.startswith("../../")
    }
    discovered = {
        p.relative_to(ROOT).as_posix()
        for p in ROOT.glob("modules/**/tests/**/*.device.test.ts")
    }
    if registered != discovered:
        raise ValueError(
            f"module device acceptance registration differs: unregistered={sorted(discovered - registered)}, undiscovered={sorted(registered - discovered)}"
        )
    return out


def dependents(ids: set[str]) -> set[str]:
    """IDS and every module that depends on one of them (manifests), directly or not."""
    mods = manifests()
    found = set(ids)
    grew = True
    while grew:
        grew = False
        for mid, requires in mods.items():
            if mid not in found and found.intersection(requires):
                found.add(mid)
                grew = True
    return found


def _production_uploads() -> dict[str, tuple[str, ...]]:
    """Module id -> the shared conformance suite, for the shipped modules (under `modules/`) that are run-time
    uploads: `dynamic`, a service or a `host` module. tests/conformance/load.ts scans the manifests by the same rule."""
    out: dict[str, tuple[str, ...]] = {}
    for mid, moddir in sorted(modroots.manifests().items()):
        if not moddir.is_relative_to(ROOT / "modules"):
            continue
        meta = tomllib.loads((moddir / "module.toml").read_text(encoding="utf-8")).get(
            "module", {}
        )
        if not (
            meta.get("dynamic") or meta.get("host") or meta.get("type") == "service"
        ):
            continue
        out[mid] = CONFORMANCE_FILES
    return out


CONFORMANCE_FILES = (
    "tests/conformance/load-dt2.device.test.ts",
    "tests/conformance/load-dn2.device.test.ts",
)
# These production uploads have coupled lifecycle acceptance, in addition to their static owners.
RUNTIME_UPLOADS = _production_uploads()
# Step -> test directory, relative to tests/regression.
ACCEPTANCE_DIRS = {
    "stock": "dt2",
    "runtime-fixture": "emulator",
    "module-load": "../conformance",
}
ACCEPTANCE_SHARED = (
    "packages/test-support/src/",
    "packages/control-client/src/",
    "packages/emulator/",
    "devices/",
    "tests/regression/support.ts",
)
# Helper directories whose files a step's tests import from outside the step's own directory: a changed
# helper there goes to Vitest's import discovery for that step (the runtime and conformance suites import
# tests/regression/runtime/*.ts).
HARNESS_DIRS: dict[str, tuple[str, ...]] = {
    "runtime-fixture": ("tests/regression/runtime/",),
    "module-load": ("tests/regression/runtime/",),
}
RUNTIME_SHARED = (
    "sdk/",
    "packages/dk-session/src/",
    "packages/patchkit/",
)


def emulator_test_path(path: str) -> bool:
    """Rust integration-test sources cannot change the emulator's production binaries."""
    return re.fullmatch(r"packages/emulator/[^/]+/tests/.+\.rs", path) is not None


@cache
def tracked_text() -> str:
    """The text of the tracked .py, .ts, .mjs, .json and .toml files, joined."""
    listed = subprocess.run(
        ["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.splitlines()
    parts = []
    for name in listed:
        if name.endswith((".py", ".ts", ".mjs", ".json", ".toml")):
            try:
                parts.append((ROOT / name).read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError):
                continue
    return "\n".join(parts)


def module_referenced(path: str, text: str) -> bool:
    """Whether TEXT names the Python module at PATH: its dotted name (also `-m tools.x.y`), or its
    stem in an `import` or `from ... import` line."""
    dotted = path.removesuffix(".py").replace("/", ".")
    stem = re.escape(dotted.rsplit(".", 1)[-1])
    if dotted in text:
        return True
    return (
        re.search(
            rf"^[ \t]*(?:import\s+(?:[\w.]+\s*,\s*)*{stem}\b"
            rf"|from\s+[\w.]+\s+import\s+(?:[^\n(]*\b{stem}\b|\([^)]*\b{stem}\b))",
            text,
            re.M,
        )
        is not None
    )


def deleted_path_referenced(path: str) -> bool:
    """Whether a tracked file still refers to the deleted Python module PATH (fail closed)."""
    return module_referenced(path, tracked_text())


@cache
def smoke_harness_closure() -> set[str]:
    """Paths of the Python modules the smoke harness (pk_harness) can run, function-local imports included."""
    return _import_closure(
        {
            "tools.patchkit.pk_harness",
            "tools.patchkit.patchkit",
            "tools.devtools.stablebin",
            "tools.sharc_dev.sharc_disasm",
        },
        local_imports=True,
    )


def outside_smoke_harness(path: str) -> bool:
    """A tools/ Python path (existing or deleted) the smoke harness cannot reach. A deleted path
    counts only when no tracked file refers to it."""
    if path.startswith("tests/support/") and path.endswith(".py"):
        return not (ROOT / path).is_file() and not deleted_path_referenced(path)
    if not (path.startswith("tools/") and path.endswith(".py")):
        return False
    if not (ROOT / path).is_file():
        return not deleted_path_referenced(path)
    return path not in smoke_harness_closure()


def invoked_script_names(
    scripts: dict[str, str], texts: list[str] | None = None
) -> set[str]:
    """The names of SCRIPTS (a package.json `scripts` table) that a gate step may invoke: named after
    `pnpm [run]`, `npm run` or `"run",` in the gate sources and mise.toml, or as a quoted argv word after
    "pnpm"/"npm"; then the scripts those scripts call. Conservative: when unsure, a script counts as invoked."""
    if texts is None:
        sources = [
            *sorted((ROOT / "tools/devtools").glob("verify*.py")),
            ROOT / "mise.toml",
        ]
        texts = [p.read_text(encoding="utf-8") for p in sources if p.is_file()]
    text = "\n".join(texts)
    invoked: set[str] = set()
    for name in scripts:
        n = re.escape(name)
        end = r"(?![\w:.-])"
        patterns = (
            rf"\b(?:pnpm|npm)\s+(?:run\s+)?{n}{end}",
            rf"\b(?:pnpm|npm)\s+[^\n\"']*\s(?:run\s+)?{n}{end}",
            rf"[\"']run[\"']\s*,\s*[\"']{n}[\"']",
            rf"[\"'](?:pnpm|npm)[\"'][^\]\n]*[\"']{n}[\"']",
        )
        if any(re.search(p, text) for p in patterns):
            invoked.add(name)
    todo = list(invoked)
    while todo:
        body = scripts.get(todo.pop(), "")
        for name in scripts:
            if name not in invoked and re.search(
                rf"(?<![\w:.-]){re.escape(name)}(?![\w:.-])", body
            ):
                invoked.add(name)
                todo.append(name)
    return invoked


def package_scripts_only(
    old: str | None, new: str | None, texts: list[str] | None = None
) -> bool:
    """Whether two package.json texts differ only in `scripts` that no gate step invokes."""
    if old is None or new is None:
        return False
    try:
        a, b = json.loads(old), json.loads(new)
    except ValueError:
        return False
    if not isinstance(a, dict) or not isinstance(b, dict):
        return False
    tables = []
    for data in (a, b):
        table = data.get("scripts", {})
        if not isinstance(table, dict) or any(
            not isinstance(v, str) for v in table.values()
        ):
            return False
        tables.append(table)
    invoked = invoked_script_names({**tables[0], **tables[1]}, texts)
    for data, table in zip((a, b), tables, strict=True):
        data["scripts"] = {k: v for k, v in table.items() if k in invoked}
    return a == b


# Module project.json path -> the acceptance files (relative to tests/regression) its records declare
# at head and, when it existed at base, at base (set by verify.changed_plan).
PROJECT_REGISTRATIONS: dict[str, tuple[str, ...]] = {}


def project_record_files(path: str, text: str | None) -> set[str]:
    """The existing acceptance files (relative to tests/regression) that the `runtimeAcceptance` records
    in the project.json TEXT at PATH declare; empty when TEXT is missing or invalid."""
    if text is None:
        return set()
    try:
        records = json.loads(text).get("metadata", {}).get("runtimeAcceptance", [])
        names = [f for r in records for f in r.get("files", [])]
    except (ValueError, AttributeError, TypeError):
        return set()
    root = Path(path).parent
    return {
        os.path.relpath(ROOT / root / f, ROOT / "tests/regression")
        for f in names
        if isinstance(f, str) and (ROOT / root / f).is_file()
    }


def registered_module_test(path: str) -> str | None:
    """The acceptance key of PATH when it is a registered modules/**/tests/**/*.device.test.ts."""
    if not (
        path.startswith("modules/")
        and "/tests/" in path
        and path.endswith(".device.test.ts")
    ):
        return None
    key = os.path.relpath(ROOT / path, ROOT / "tests/regression")
    return key if key in acceptance_records() else None


def lint_config_only(old: str | None, new: str | None) -> bool:
    """Whether two pyproject.toml texts differ only inside `tool.mypy` or `tool.ruff`."""
    if old is None or new is None:
        return False
    try:
        a, b = tomllib.loads(old), tomllib.loads(new)
    except tomllib.TOMLDecodeError:
        return False
    for data in (a, b):
        tool = data.get("tool")
        if isinstance(tool, dict):
            tool.pop("mypy", None)
            tool.pop("ruff", None)
            if not tool:
                del data["tool"]
    return a == b


def _without_docstrings(tree: ast.AST) -> ast.AST:
    for node in ast.walk(tree):
        if isinstance(
            node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)
        ):
            body = node.body
            if (
                body
                and isinstance(body[0], ast.Expr)
                and isinstance(body[0].value, ast.Constant)
                and isinstance(body[0].value.value, str)
            ):
                node.body = body[1:] or [ast.Pass()]
    return tree


def python_code_unchanged(old: str | None, new: str | None) -> bool:
    """Whether two Python texts differ only in comments, docstrings or layout."""
    if old is None or new is None:
        return False
    try:
        a, b = ast.parse(old), ast.parse(new)
    except (SyntaxError, ValueError):
        return False
    return ast.dump(_without_docstrings(a), include_attributes=False) == ast.dump(
        _without_docstrings(b), include_attributes=False
    )


def _import_closure(roots: set[str], *, local_imports: bool = False) -> set[str]:
    """Paths of ROOTS and of every module they import, using the existing import graph."""
    from tools.devtools import changed_tests

    graph = changed_tests.graph(local_imports=local_imports)
    found, todo = set(roots), list(roots)
    while todo:
        for dependency in graph.find_modules_directly_imported_by(todo.pop()):
            if dependency not in found:
                found.add(dependency)
                todo.append(dependency)
    return {changed_tests.path_of(module) for module in found}


@cache
def acceptance_shared(step: str) -> set[str]:
    """Dependencies of the preparation/client entry points that every acceptance file consumes."""
    # The graph omits function-local imports. These consumed call paths need stablebin from
    # patchkit.binary and sharc_disasm (imported inside the functions of the SHARC runner pk_sharc.run_call).
    return _import_closure(
        {
            "tools",
            "tools.patchkit.patchkit",
            "tools.devtools.stablebin",
            "tools.sharc_dev.sharc_disasm",
        }
    )


@cache
def acceptance_record_closure(record: RuntimeAcceptance) -> set[str]:
    """Dependencies of the Python sources one runtime record runs, and the record's own sources."""
    paths = _import_closure(
        {
            source.removesuffix(".py").replace("/", ".")
            for source in record.sources
            if source.endswith(".py") and (ROOT / source).is_file()
        }
    )
    paths.update(record.sources)
    # Module-owned Python models may be loaded dynamically rather than imported.
    if record.owner in manifests():
        paths.update(
            p.relative_to(ROOT).as_posix()
            for p in (modroots.module_dir(record.owner) / "tests").rglob("*.py")
        )
    return paths


@cache
def runtime_framework() -> set[str]:
    """Modules included by the actual runtime patch sets, including manifest dependencies."""
    required: set[str] = set(FRAMEWORK)
    for filename in ("digitakt-ii.toml", "digitone-ii.toml"):
        data = tomllib.loads((ROOT / "devices" / filename).read_text())
        for roots in data["patch_sets"].values():
            required.update(roots)
    todo = list(required)
    while todo:
        for mid in manifests().get(todo.pop(), ()):
            if mid not in required:
                required.add(mid)
                todo.append(mid)
    return required


def module_smoke_cells(ids: set[str]) -> list[str] | None:
    """Existing matrix cells consuming these leaves; None preserves unsupported cell settings."""
    affected = dependents(ids)
    matrix = tomllib.loads((ROOT / "tests/patchkit/smoke/matrix.toml").read_text())
    selected = []
    for cell in matrix["cell"]:
        modules = cell.get("modules", [])
        if any(mid not in manifests() for mid in modules):
            return None  # an unknown matrix module's dependencies cannot be proven unrelated
        if affected.intersection(modules):
            # --cell preserves the normal full-cell execution, but cannot encode custom settings.
            if set(cell) - {"device", "firmware", "modules"}:
                return None
            selected.append(f"{cell['device']}-{cell['firmware']}:" + ",".join(modules))
    return selected


def runtime_modules() -> dict[str, tuple[str, ...]]:
    """Fixture module -> the runtime test files that exercise it; the gate packs a native artifact for each."""
    owners: dict[str, list[str]] = {}
    for file, record in acceptance_records().items():
        for mid in record.modules:
            owners.setdefault(mid, []).append(file)
    return {mid: tuple(files) for mid, files in owners.items()}


def runtime_aliases() -> dict[str, tuple[str, tuple[str, ...]]]:
    """Alias -> (source module, the files of its record). Renamed packs share the source object; they are card identities."""
    return {
        alias: (source, record.files)
        for record in dict.fromkeys(acceptance_records().values())
        for alias, source in record.aliases.items()
    }


def conformance_modules(paths: list[str]) -> list[str]:
    """Module ids the conformance suite needs for PATHS; empty means all eligible modules."""
    ids: set[str] = set()
    roots = tuple(r + "/" for r in modroots.REPO_ROOTS)
    for path in paths:
        if not acceptance_files("module-load", [path], resolve_helpers=False):
            continue
        mid = modroots.module_id(path) if path.startswith(roots) else None
        if mid is None:
            return []
        hit = dependents({mid})
        if hit & runtime_framework():
            return []
        ids |= hit & set(RUNTIME_UPLOADS)
    return sorted(ids)


def runtime_plusdrive_files() -> set[str]:
    return {file for file, r in acceptance_records().items() if r.plusdrive}


@cache
def related_acceptance(step: str, paths: tuple[str, ...]) -> tuple[str, ...] | None:
    """Vitest's import discovery only; any unproven helper keeps the owner suite.

    One bounded subprocess handles the helpers together. Process-local memoization avoids
    repeating discovery when the plan and explanation ask about the same changed paths.
    """
    directory = ACCEPTANCE_DIRS[step]
    project = {
        "stock": "stock",
        "runtime-fixture": "runtime",
        "module-load": "conformance",
    }[step]
    owned = {f for f in acceptance_records() if f.startswith("../../")}
    try:
        done = subprocess.run(
            [
                "mise",
                "exec",
                "--",
                "node",
                "tests/regression/scripts/related-tests.mjs",
                project,
                *paths,
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if done.returncode:
            return None
        found = json.loads(done.stdout)
        if not isinstance(found, dict) or set(found) != set(paths):
            return None
        selected: set[str] = set()
        for files in found.values():
            if not isinstance(files, list) or not files:
                return None
            for path in files:
                if (
                    not isinstance(path, str)
                    or not (
                        path.startswith(f"{directory}/")
                        or (step == "runtime-fixture" and path in owned)
                    )
                    or not path.endswith(".test.ts")
                    or (
                        ".." in Path(path).parts
                        and not (step == "runtime-fixture" and path in owned)
                        and step != "module-load"
                    )
                    or not (ROOT / "tests/regression" / path).is_file()
                ):
                    return None
                selected.add(path)
        return tuple(sorted(selected))
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None


def acceptance_files(
    step: str, paths: list[str], *, resolve_helpers: bool = True
) -> list[str]:
    """Native Vitest filenames affected by PATHS, relative to tests/regression.

    Framework/upload dependencies are partly implicit in patched firmware, so shared code keeps
    cross-device coverage. Known static leaves retain their existing owners without unrelated uploads.
    """
    directory = ACCEPTANCE_DIRS[step]
    config = "tests/regression/vitest.config.ts"
    base = Path(os.path.normpath(ROOT / "tests/regression" / directory))
    all_files = sorted(
        Path(os.path.relpath(p, ROOT / "tests/regression")).as_posix()
        for p in base.rglob("*.test.ts")
    )
    registered = acceptance_records() if step == "runtime-fixture" else {}
    all_files = sorted(set(all_files) | set(registered))
    selected: set[str] = set()
    helpers: list[str] = []
    for path in paths:
        if (
            emulator_test_path(path)
            or path.endswith(".md")
            or path
            in {
                "devices/digitakt-ii-leds.toml",
                "devices/digitone-ii-leds.toml",
            }
        ):
            continue
        own_test = registered_module_test(path)
        if own_test or path in PROJECT_REGISTRATIONS:
            if step == "runtime-fixture":
                selected.update([own_test] if own_test else PROJECT_REGISTRATIONS[path])
            continue
        declared = {f for f, r in acceptance_records().items() if path in r.sources}
        if declared:
            if step == "runtime-fixture":
                selected.update(declared)
            continue
        own = os.path.normpath(f"tests/regression/{directory}") + "/"
        if path.startswith((own, *HARNESS_DIRS.get(step, ()))):
            rel = os.path.relpath(path, "tests/regression")
            if path.endswith(".test.ts") and rel in all_files:
                selected.add(rel)
            elif (
                path.endswith(".ts")
                and not path.endswith(".test.ts")
                and (ROOT / path).is_file()
            ):
                helpers.append(path)
            else:
                # Unknown scripts and deleted cases keep their owner suite.
                selected.update(all_files)
        elif (
            path == config
            or path == "tests/regression/scripts/related-tests.mjs"
            or path
            in ("tests/regression/package.json", "tests/regression/tsconfig.json")
            or path.startswith(ACCEPTANCE_SHARED)
            or (
                step != "stock"
                and path.startswith(RUNTIME_SHARED)
                and not patchkit_no_smoke(path)
            )
            or (step != "stock" and path == "tools/emulator/fast_regions.json")
            or (
                step != "stock"
                and path.startswith("tools/")
                and path.endswith(".py")
                and (
                    (
                        step == "runtime-fixture"
                        and not (ROOT / path).is_file()
                        and deleted_path_referenced(path)
                    )
                    or path in acceptance_shared(step)
                )
            )
        ):
            selected.update(all_files)
        elif (
            step == "runtime-fixture"
            and path.startswith("tools/")
            and path.endswith(".py")
        ):
            selected.update(
                f for f, r in registered.items() if path in acceptance_record_closure(r)
            )
        elif step != "stock" and path.startswith(
            tuple(r + "/" for r in modroots.REPO_ROOTS)
        ):
            mid = modroots.module_id(path)
            if mid is None:
                selected.update(all_files)
            else:
                hit = dependents({mid})
                if hit & runtime_framework():
                    selected.update(all_files)
                elif step == "module-load":
                    selected.update(
                        os.path.relpath(f, "tests/regression")
                        for m, fs in RUNTIME_UPLOADS.items()
                        if m in hit
                        for f in fs
                    )
                else:
                    selected.update(
                        f for f, r in registered.items() if hit.intersection(r.modules)
                    )
        elif step == "stock" and path.startswith("tests/regression/goldens/"):
            selected.update(all_files)
    if helpers and selected != set(all_files):
        # Step-only routing needs membership, not filename discovery: an unproven helper
        # already retains this suite. Resolve once the caller needs the actual file plan.
        related = (
            related_acceptance(step, tuple(sorted(set(helpers))))
            if resolve_helpers
            else None
        )
        selected.update(all_files if related is None else related)
    return sorted(selected)


@cache
def _ids_by_dir() -> dict[str, str]:
    """`category/name` of a module directory -> its id (a path form in a test names the module too)."""
    out: dict[str, str] = {}
    for mid, moddir in modroots.manifests().items():
        for root in modroots.REPO_ROOTS:
            if moddir.is_relative_to(ROOT / root):
                out[moddir.relative_to(ROOT / root).as_posix()] = mid
    return out


@cache
def _id_patterns() -> tuple[re.Pattern[str], re.Pattern[str]]:
    """(ID_RE, SPLIT_ID_RE) built from the module manifests: the ids and the `category/name` directory forms.
    No author or category is named here; a new module is found by its module.toml."""
    names = set(manifests()) | set(_ids_by_dir())
    names = {n for n in names if "/" in n}
    firsts = sorted({n.split("/", 1)[0] for n in names})
    alt = "|".join(re.escape(n) for n in sorted(names, key=lambda n: (-len(n), n)))
    first = "|".join(re.escape(f) for f in firsts) or "(?!)"
    # a path built from parts: ROOT / "modules" / "machines" / "pluck"
    split = re.compile(rf"""["']({first})["']\s*[/,]\s*["']([a-z0-9][a-z0-9-]*)["']""")
    full = re.compile(rf"(?<![\w-])(?:{alt})(?![\w-])" if alt else "(?!)")
    return full, split


def refs(text: str) -> set[str]:
    """Module ids named in TEXT (comment lines of a TOML file are not read)."""
    lines = (line for line in text.splitlines() if not line.lstrip().startswith("#"))
    body = "\n".join(lines)
    id_re, split_re = _id_patterns()
    found = set(id_re.findall(body)) | {f"{a}/{b}" for a, b in split_re.findall(body)}
    ids = manifests()
    by_dir = _ids_by_dir()
    return {m if m in ids else by_dir[m] for m in found if m in ids or m in by_dir}


# The scenario runners (tools/patchkit/pk_digikit_*.py) name modules in code, not only in the scenario
# TOMLs (test/dyn-guard-nan in the debug runner). A module a runner names selects the kinds the runner
# serves; a runner not listed here serves every kind.
RUNNER_KINDS: dict[str, tuple[str, ...]] = {
    "pk_digikit_debug.py": ("card", "debug", "crash"),
    "pk_digikit_upload.py": ("upload",),
    "pk_digikit_pages.py": ("pages",),
    "pk_digikit_fx.py": (),  # retained offline TS and legacy shared oracles
    "pk_digikit_fxsend.py": (),
    "pk_digikit_fxroute.py": (),  # retained offline TS oracles
    "pk_digikit_fxroutes.py": (),  # retained TS profile/card/OLED helpers
    "pk_digikit_fxpage.py": (),  # retained PCM comparison CLI
    "pk_digikit_fxshift.py": (),  # retained PCM/model analysis CLI
    "pk_digikit_capacity.py": ("debug",),
    "pk_digikit_memaudit.py": ("debug",),
    "pk_digikit_cost.py": ("debug",),
    "pk_dkmenu.py": ("menu",),
}


def code_refs(text: str) -> set[str]:
    """Module ids in the string literals of Python TEXT, docstrings left out (examples in a
    docstring name modules the code never builds)."""
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return refs(text)
    skip: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(
            node, ast.Module | ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef
        ):
            first = node.body[0] if node.body else None
            if isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant):
                skip.add(id(first.value))
    found: set[str] = set()
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Constant)
            and isinstance(node.value, str)
            and id(node) not in skip
        ):
            found |= refs(node.value)
    return found


@cache
def pk_refs(name: str) -> frozenset[str]:
    """Module ids named in tools/patchkit/NAME.py (empty when there is no such file)."""
    path = ROOT / "tools/patchkit" / f"{name}.py"
    return (
        frozenset(refs(path.read_text(encoding="utf-8")))
        if path.is_file()
        else frozenset()
    )


def runner_kinds(hit: set[str]) -> set[str]:
    """The scenario kinds whose runner code names a module of HIT."""
    kinds: set[str] = set()
    runners = [
        *(ROOT / "tools/patchkit").glob("pk_digikit_*.py"),
        ROOT / "tools/patchkit/pk_dkmenu.py",
    ]
    for path in sorted(runners):
        if pk_refs(path.stem) & hit:
            kinds |= set(RUNNER_KINDS.get(path.name, tuple(kind_files()) + ("pages",)))
    return kinds


def module_test_files(mid: str | None = None) -> list[str]:
    """The pytest files of the modules' own tests (`modules/<group>/<name>/tests/**/test_*.py`), repo-relative:
    those of module MID, or of every module. Found by globbing; no list names them."""
    base = modroots.module_dir(mid) / "tests" if mid else None
    found = (
        sorted(base.rglob("test_*.py"))
        if base is not None
        else sorted(ROOT.glob(f"{MODULE_TESTS_GLOB}/**/test_*.py"))
    )
    return [
        p.relative_to(ROOT).as_posix() for p in found if "node_modules" not in p.parts
    ]


def module_scenario_files(mid: str) -> list[str]:
    """The scenario files of module MID (`tests/scenarios/*.toml` in its directory), repo-relative."""
    base = modroots.module_dir(mid) / "tests/scenarios"
    return [p.relative_to(ROOT).as_posix() for p in sorted(base.glob("*.toml"))]


@cache
def test_files() -> tuple[str, ...]:
    """Every tests/**/test_*.py and every module-owned test_*.py, repo-relative (node_modules skipped)."""
    return tuple(
        rel
        for rel in (
            *(
                p.relative_to(ROOT).as_posix()
                for p in sorted((ROOT / "tests").rglob("test_*.py"))
            ),
            *module_test_files(),
        )
        if "node_modules" not in rel
    )


@cache
def test_refs() -> dict[str, frozenset[str]]:
    """Pytest file -> the module ids it names."""
    out: dict[str, frozenset[str]] = {}
    for rel in test_files():
        if rel.startswith("tests/devtools/"):  # the routing tests name modules as data
            continue
        text = (ROOT / rel).read_text(encoding="utf-8")
        found = refs(text).union(
            *(pk_refs(n) for n in set(re.findall(r"\bpk_[a-z0-9_]+\b", text)))
        )
        if found:
            out[rel] = frozenset(found)
    return out


@cache
def kind_files() -> dict[str, tuple[str, ...]]:
    """Scenario kind -> the tests/digikit files that run it (their `cases((kinds), device)` call)."""
    out: dict[str, list[str]] = {}
    for path in sorted((ROOT / "tests/digikit").glob("test_*.py")):
        match = re.search(r"cases\(\(([^)]*)\)", path.read_text(encoding="utf-8"))
        if not match:
            continue
        for kind in re.findall(r'"([a-z]+)"', match.group(1)):
            out.setdefault(kind, []).append(path.relative_to(ROOT).as_posix())
    return {kind: tuple(files) for kind, files in out.items()}


@cache
def scenario_refs() -> dict[str, tuple[str, frozenset[str]]]:
    """Scenario file -> (kind, module ids it names). A module's own scenarios (modules/*/*/tests/scenarios) belong to
    their module whether or not the file names it."""
    out: dict[str, tuple[str, frozenset[str]]] = {}
    paths = [
        *sorted((ROOT / "tests/digikit/scenarios").glob("*.toml")),
        *sorted(ROOT.glob(f"{MODULE_TESTS_GLOB}/scenarios/*.toml")),
    ]
    for path in paths:
        text = path.read_text(encoding="utf-8")
        kind = tomllib.loads(text).get("scenario", {}).get("kind", "")
        ids = refs(text)
        owner = modroots.module_id(path)
        if owner is not None and path.is_relative_to(ROOT / "modules"):
            ids.add(owner)
        out[path.relative_to(ROOT).as_posix()] = (kind, frozenset(ids))
    return out


def scenario_tests(kinds: set[str]) -> list[str]:
    """The digikit test files that run scenarios of KINDS."""
    files = {f for k in kinds for f in kind_files().get(k, ())}
    return sorted(files)


def own_module_tests(mid: str) -> list[str]:
    """Pytest paths of the tests inside module MID's directory: its test_*.py and the digikit test files that run
    the kinds of its scenarios."""
    kinds = {
        scenario_refs()[f][0]
        for f in module_scenario_files(mid)
        if f in scenario_refs()
    }
    return [*module_test_files(mid), *scenario_tests(kinds)]


def module_tests(mid: str) -> list[str]:
    """Pytest paths for a change in module MID (see the module docstring)."""
    hit = dependents({mid})
    if hit & FRAMEWORK:
        return [
            *ALL_SUITES,
            *module_test_files(),
            *([CONFORMANCE] if mid in CONFORMANCE_WRAPPERS else []),
        ]
    # the module's own tests (and those of the modules that depend on it), then the tests that name it
    tests = list(dict.fromkeys(t for m in sorted(hit) for t in own_module_tests(m)))
    tests += [t for t, ids in test_refs().items() if ids & hit and t not in tests]
    if any(pk_conform.is_machine_module(m) for m in hit) and CONFORMANCE not in tests:
        tests.append(CONFORMANCE)
    if any(pk_conform.is_fx_module(m) for m in hit) and FX_CONFORMANCE not in tests:
        tests.append(FX_CONFORMANCE)
    kinds = {
        kind for kind, ids in scenario_refs().values() if ids & hit
    } | runner_kinds(hit)
    tests += [t for t in scenario_tests(kinds) if t not in tests]
    return tests


def web_reads(rel: str) -> bool:
    """Whether the web gallery bundle can read RELATIVE (to the module directory): the manifest,
    built objects, registry tables and the gallery files, not sources, the SDK, smoke tests."""
    top = rel.split("/", 1)[0]
    return top not in ("src", "sdk", "tests") and not rel.endswith(".md")


def module_fine(path: str) -> Fine | None:
    root = next((r for r in modroots.REPO_ROOTS if path.startswith(r + "/")), None)
    if root is None or "/" not in path[len(root) + 1 :]:
        return None
    if path.endswith(".md"):
        return Fine("modules docs", (), ())
    mid = modroots.module_id(path)
    inside = (ROOT / path).relative_to(modroots.module_dir(mid)).parts if mid else ()
    if inside[:1] == ("tests",):
        # a module's own test or scenario: only the tests read it (no Rust, no web bundle)
        steps = ("python-tests", "host") if path.endswith(".ts") else ("python-tests",)
        return Fine(f"module {mid} tests", steps, tuple(own_module_tests(mid)))
    if mid is None:
        # a file outside any module, or a module that is gone: stay broad
        return Fine(
            "modules (unknown module)",
            ("python-tests", "rust-tests", "web"),
            ALL_SUITES,
        )
    steps = ["python-tests", "rust-tests"]
    if web_reads((ROOT / path).relative_to(modroots.module_dir(mid)).as_posix()):
        steps.append("web")
    label = f"module {mid}" + (" (framework)" if mid in FRAMEWORK else "")
    return Fine(label, tuple(steps), tuple(module_tests(mid)))


def importers(stem: str) -> list[str]:
    """tools/device modules that import tools.device.STEM, and the tests that import it."""
    pat = re.compile(
        rf"^\s*(?:from\s+tools\.device\s+import\s+[^\n]*\b{stem}\b"
        rf"|from\s+tools\.device\.{stem}\s+import|import\s+tools\.device\.{stem}\b)",
        re.M,
    )
    users = [
        p.relative_to(ROOT).as_posix() for p in sorted(ROOT.glob("tools/device/*.py"))
    ]
    return [
        rel
        for rel in [*users, *test_files()]
        if rel != f"tools/device/{stem}.py"
        and pat.search((ROOT / rel).read_text(encoding="utf-8"))
    ]


def device_fine(path: str) -> Fine | None:
    if not (path.startswith("tools/device/") and path.endswith(".py")):
        return None
    stem = path.rsplit("/", 1)[-1].removesuffix(".py")
    seen, todo = {stem}, [stem]
    tests: list[str] = []
    while todo:  # the importers of the importers, inside tools/device
        cur = todo.pop()
        own = ROOT / f"tests/digikit/test_{cur}.py"
        if own.is_file():
            tests.append(own.relative_to(ROOT).as_posix())
        for user in importers(cur):
            if user.startswith("tests/"):
                tests.append(user)
            else:
                name = user.rsplit("/", 1)[-1].removesuffix(".py")
                if name not in seen:
                    seen.add(name)
                    todo.append(name)
    kinds = {k for s in seen for k in DEVICE_KINDS.get(s, ())}
    tests += scenario_tests(kinds)
    ordered = list(dict.fromkeys(tests))
    return Fine(f"tools/device/{stem}", ("python-tests",), tuple(ordered))


def digikit_fine(path: str) -> Fine | None:
    if path.startswith("tests/digikit/scenarios/") and path.endswith(".toml"):
        kind = scenario_refs().get(path, ("", frozenset()))[0]
        if not kind:  # deleted or unreadable: the whole suite
            return Fine(
                "tests/digikit/scenarios", ("python-tests",), ("tests/digikit",)
            )
        return Fine(
            f"scenario kind {kind}", ("python-tests",), tuple(scenario_tests({kind}))
        )
    name = path.rsplit("/", 1)[-1]
    for suite in ALL_SUITES:
        if (
            path.startswith(suite + "/")
            and path.count("/") == 2
            and name.startswith("test_")
            and name.endswith(".py")
            and (ROOT / path).is_file()
        ):
            return Fine(f"{suite}/{name}", ("python-tests",), (path,))
    return None


PATCHKIT_SRC = "packages/patchkit/src/"
# packages/patchkit/src groups: (label, files, steps, test files). A file of the crate in no group keeps the
# wide route. `smoke` in the steps keeps the smoke matrix; the groups without it skip it (patchkit_no_smoke).
PATCHKIT_GROUPS: tuple[
    tuple[str, tuple[str, ...], tuple[str, ...], tuple[str, ...]], ...
] = (
    (
        "patchkit/checks",
        ("build/sharc_legal.rs", "dspscan.rs"),
        ("rust-tests", "python-tests"),
        (
            "tests/patchkit/test_dsp_guard.py",
            "tests/patchkit/test_dsp_guard_contract.py",
            "tests/patchkit/test_pk_dsp_budget.py",
            "tests/patchkit/test_pk_sharc.py",
        ),
    ),
    (
        "patchkit/build",
        ("build_cli.rs", "dko.rs", "link.rs", "abigen.rs", "modgen.rs", "explain.rs"),
        ("rust-tests", "python-tests", "smoke"),
        (
            "tests/patchkit/test_patchkit_cmodules.py",
            "tests/patchkit/test_patchkit_rust.py",
            "tests/patchkit/test_patchkit_dkrt.py",
            "tests/patchkit/test_patchkit_registry.py",
            "tests/patchkit/test_patchkit_expansion.py",
            "tests/patchkit/test_patchkit_dkrt_machine.py",
            "tests/patchkit/test_patchkit_dkrt_boot.py",
            "tests/patchkit/test_pk_cf.py",
            "tests/patchkit/test_dsplink.py",
            "tests/patchkit/test_dyn_fx.py",
            "tests/patchkit/test_health.py",
            "tests/patchkit/test_fx*.py",
            "tests/patchkit/test_digikit_menu.py",
            "tests/patchkit/test_modcheck_needs.py",
            "tests/modules",
        ),
    ),
    (
        "patchkit/flash",
        ("flash.rs", "flash_rpc.rs", "flash_cli.rs"),
        ("rust-tests", "web"),
        (),
    ),
    ("patchkit/wasm", ("wasm.rs",), ("rust-tests", "web"), ()),
    (
        "patchkit/codegen",
        ("sdkgen.rs", "devgen.rs", "fwgen.rs", "desc.rs", "devdata.rs"),
        ("rust-tests", "python-tests"),
        (
            "tests/patchkit/test_pk_gen_include.py",
            "tests/patchkit/test_pk_new.py",
            "tests/patchkit/test_pk_conform.py",
            "tests/patchkit/test_pk_types.py",
        ),
    ),
)
# Files of build/ outside the checks group that the build group takes; build/mod.rs stays on the wide route.
PATCHKIT_BUILD_DIR_WIDE = ("build/mod.rs",)


def patchkit_fine(path: str) -> Fine | None:
    if not (path.startswith(PATCHKIT_SRC) and path.endswith(".rs")):
        return None
    rel = path.removeprefix(PATCHKIT_SRC)
    for label, files, steps, tests in PATCHKIT_GROUPS:
        in_build = (
            label == "patchkit/build"
            and rel.startswith("build/")
            and rel not in PATCHKIT_BUILD_DIR_WIDE
            and not any(rel in g[1] for g in PATCHKIT_GROUPS if g[0] != label)
        )
        if rel in files or in_build:
            found = [
                p.relative_to(ROOT).as_posix()
                for t in tests
                for p in sorted(ROOT.glob(t))
            ]
            return Fine(label, steps, tuple(found))
    return None


def patchkit_no_smoke(path: str) -> str | None:
    """The label of PATH's patchkit group when it needs no smoke matrix, else None."""
    found = patchkit_fine(path)
    return found.label if found and "smoke" not in found.steps else None


def runner_users(stem: str) -> list[str] | None:
    """The modules of tools/patchkit that import the runner STEM. None when the dispatcher
    pk_digikit_scen uses it outside its one-line wrapper for the kind."""
    wrapper = re.compile(
        rf"    from tools\.patchkit import {stem} as \w+\n\n    \w+\.run_\w+\(c\)\n"
    )
    uses = re.compile(
        rf"from tools\.patchkit import [^\n]*\b{stem}\b|from tools\.patchkit\.{stem} import"
    )
    users: list[str] = []
    for path in sorted((ROOT / "tools/patchkit").glob("*.py")):
        if path.stem == stem:
            continue
        text = path.read_text(encoding="utf-8")
        if path.stem == "pk_digikit_scen":
            text = wrapper.sub("", text)
        if uses.search(text):
            if path.stem == "pk_digikit_scen":
                return None
            users.append(path.stem)
    return users


def runner_fine(path: str) -> Fine | None:
    """A scenario runner (RUNNER_KINDS) selects the digikit tests of the kinds it serves and of the
    runners that import it. A user that is no listed runner, a kind no test runs: the wide route."""
    name = path.rsplit("/", 1)[-1]
    if path != f"tools/patchkit/{name}" or name not in RUNNER_KINDS:
        return None
    stem = name.removesuffix(".py")
    seen, todo = {stem}, [stem]
    kinds: set[str] = set()
    while todo:
        cur = todo.pop()
        if f"{cur}.py" not in RUNNER_KINDS:
            return None
        kinds |= set(RUNNER_KINDS[f"{cur}.py"])
        users = runner_users(cur)
        if users is None:
            return None
        todo += [u for u in users if u not in seen]
        seen.update(users)
    if not kinds <= set(kind_files()):
        return None
    return Fine(
        f"scenario runner {name}", ("python-tests",), tuple(scenario_tests(kinds))
    )


def fine(path: str) -> Fine | None:
    """The finer route of PATH, or None to use the prefix rules of verify.py."""
    if registered_module_test(path) or path in PROJECT_REGISTRATIONS:
        return Fine("test registration", ("runtime-fixture",), ())
    return (
        module_fine(path)
        or device_fine(path)
        or digikit_fine(path)
        or patchkit_fine(path)
        or runner_fine(path)
    )


# ---- mise.toml ---------------------------------------------------------------------------------


def run_lines(task: dict) -> list[str]:
    run = task.get("run", [])
    return [run] if isinstance(run, str) else [str(x) for x in run]


@cache
def root_package_scripts() -> frozenset[str]:
    """The names of the `scripts` of the root package.json."""
    try:
        data = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return frozenset()
    table = data.get("scripts", {}) if isinstance(data, dict) else {}
    return frozenset(table) if isinstance(table, dict) else frozenset()


def paths_of_run(lines: list[str]) -> list[str] | None:
    """The repo paths a task's run lines name: `-m a.b.c` (a/b/c.py or a package), `node FILE`,
    test paths of `-m pytest`, pnpm and npx (the web workspace). None when a line names none."""
    found: list[str] = []
    for line in lines:
        try:
            words = shlex.split(line)
        except ValueError:
            return None
        mine: list[str] = []
        for i, word in enumerate(words):
            if word == "-m" and i + 1 < len(words) and words[i + 1] != "pytest":
                base = words[i + 1].replace(".", "/")
                for cand in (f"{base}.py", f"{base}/__init__.py"):
                    if (ROOT / cand).is_file():
                        mine.append(cand)
                        break
            elif word in ("pnpm", "npx"):
                rest = words[i + 1 :]
                if rest[:1] == ["run"]:
                    rest = rest[1:]
                if word == "pnpm" and rest and rest[0] in root_package_scripts():
                    mine.append("package.json")  # a script of the root package.json
                else:
                    mine.append("apps/web/package.json")
            elif (ROOT / word).exists() and "/" in word:
                mine.append(word)
        if not mine:
            return None
        found += mine
    return found


def lock_members(old: str, new: str) -> set[str] | None:
    """The workspace members (packages without a source) that a Cargo.lock change reaches: the members
    that are new or changed, and the first members on the way up from a package that is new, removed or
    changed (version, source, checksum or dependency list). None when a text does not parse or no member is
    reached (the caller keeps the wide route)."""
    try:
        packages = [tomllib.loads(t).get("package", []) for t in (old, new)]
        keyed = [
            {(p["name"], p["version"], p.get("source")): p for p in pkgs}
            for pkgs in packages
        ]
    except (tomllib.TOMLDecodeError, KeyError, TypeError):
        return None
    before, after = keyed

    def content(p: dict) -> tuple:
        return (p.get("checksum"), tuple(sorted(p.get("dependencies", ()))))

    changed = {
        k
        for k, p in after.items()
        if k not in before or content(before[k]) != content(p)
    }
    removed_names = {k[0] for k in before if k not in after}
    changed |= {k for k in after if k[0] in removed_names and k not in changed}

    def target(entry: str) -> list[tuple]:
        parts = entry.split(" ", 2)
        return [
            k
            for k in after
            if k[0] == parts[0] and (len(parts) < 2 or k[1] == parts[1])
        ]

    users: dict[tuple, set[tuple]] = {}
    for k, p in after.items():
        for entry in p.get("dependencies", ()):
            for t in target(entry):
                users.setdefault(t, set()).add(k)
    # the walk stops at a member: the members that use it are routed by that member's own route
    reached, todo = set(changed), [k for k in changed if k[2] is not None]
    while todo:
        for user in users.get(todo.pop(), ()):
            if user not in reached:
                reached.add(user)
                if user[2] is not None:
                    todo.append(user)
    members = {k[0] for k in reached if k[2] is None}
    return members or None


def object_metadata_only(old: str, new: str) -> bool:
    """Whether two versions of a committed .dko.json differ only in metadata (`toolchain`, `source_sha256`):
    the sections, symbols and relocations are the same. False when a text is not a JSON object."""
    try:
        a, b = json.loads(old), json.loads(new)
    except ValueError:
        return False
    if not isinstance(a, dict) or not isinstance(b, dict):
        return False
    meta = ("toolchain", "source_sha256")
    drop = lambda d: {k: v for k, v in d.items() if k not in meta}  # noqa: E731
    return drop(a) == drop(b)


def _without_text(task: object) -> object:
    """A mise task without the keys that only describe it."""
    if not isinstance(task, dict):
        return task
    return {k: v for k, v in task.items() if k not in ("description", "alias")}


def mise_paths(
    old: str | None, new: str | None
) -> tuple[list[str], dict[str, str]] | None:
    """Repo paths to route in place of a changed mise.toml, with the task each came from; None
    when the change is broad: a section other than [tasks], a verify/build/gen/setup task, a task
    key other than run/depends/description/alias, a run that names no known file, a bad TOML."""
    if old is None or new is None:
        return None
    try:
        before, after = tomllib.loads(old), tomllib.loads(new)
    except tomllib.TOMLDecodeError:
        return None
    tasks_old, tasks_new = before.pop("tasks", {}), after.pop("tasks", {})
    if before != after:
        return None
    paths: list[str] = []
    origin: dict[str, str] = {}
    for name in sorted(set(tasks_old) | set(tasks_new)):
        if tasks_old.get(name) == tasks_new.get(name):
            continue
        if (
            name in tasks_old
            and name in tasks_new
            and _without_text(tasks_old[name]) == _without_text(tasks_new[name])
        ):
            continue  # only the description or alias changed: nothing runs differently
        task = tasks_new.get(name)
        if task is None:  # a removed task: nothing runs it any more
            continue
        if name.split(":", 1)[0] in BROAD_TASK_GROUPS:
            return None
        other = {
            k: v
            for k, v in task.items()
            if k not in ("run", "depends", "description", "alias")
        }
        if other != {
            k: v for k, v in (tasks_old.get(name) or {}).items() if k in other
        }:
            return None
        lines = run_lines(task)
        old_deps = (tasks_old.get(name) or {}).get("depends", [])
        for dep in task.get("depends", []):
            if dep in old_deps:
                continue  # an unchanged dependency is routed by its own diff, if it changed
            lines += run_lines(tasks_new.get(dep, {}))
        named = paths_of_run(lines)
        if named is None:
            return None
        for path in named:
            origin.setdefault(path, f"mise.toml task {name}")
            paths.append(path)
    return list(dict.fromkeys(paths)), origin
