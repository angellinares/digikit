"""Audit the declared Nx inputs of cached targets against the files the target really reads.

A cached target is safe only if every file it reads is in its declared `inputs`. This tool expands
the declared inputs to a file set, runs the target command with tools/devtools/fs_trace.mjs
preloaded (file reads and spawns are logged) and reports:

- undeclared: a tracked file was read but no input covers it.
- generated: an ignored file (out/**, build output) was read; declare a dependency task output or a runtime hash.
- spawned tool: python, uv, cargo, cc or a repo script was started; declare its sources.
- outside repo: a file under the home directory but outside the repo was read.

Usage: uv run python -m tools.devtools.nx_input_audit [PROJECT:TARGET ...] [--all-cached] [--json FILE]
Exit 1 if any undeclared, generated or spawned-tool finding exists.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import tools

ROOT = Path(__file__).resolve().parents[2]
TRACE_PRELOAD = Path(__file__).resolve().with_name("fs_trace.mjs")

NO_FILE_KEYS = ("runtime", "env", "externalDependencies", "dependentTasksOutputFiles")
SPAWN_TOOLS = frozenset(
    {"python", "python3", "uv", "cargo", "rustc", "cc", "gcc", "clang", "node"}
)
IGNORED_PARTS = ("node_modules", ".nx", ".git")


# ---- input expansion (pure) ---------------------------------------------------------------


def _brace_alternatives(pattern: str) -> list[str]:
    m = re.search(r"\{([^{}]*,[^{}]*)\}", pattern)
    if not m:
        return [pattern]
    out: list[str] = []
    for alt in m.group(1).split(","):
        out.extend(_brace_alternatives(pattern[: m.start()] + alt + pattern[m.end() :]))
    return out


def _glob_regex(pattern: str) -> str:
    out = ""
    i = 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out += "(?:.*/)?"
            i += 3
        elif pattern.startswith("**", i):
            out += ".*"
            i += 2
        elif pattern[i] == "*":
            out += "[^/]*"
            i += 1
        elif pattern[i] == "?":
            out += "[^/]"
            i += 1
        else:
            out += re.escape(pattern[i])
            i += 1
    return out


def glob_match(pattern: str, path: str) -> bool:
    """Match a repo-relative path against a glob with `**` and `{a,b}` support."""
    return any(
        re.fullmatch(_glob_regex(alt), path) for alt in _brace_alternatives(pattern)
    )


@dataclass
class Workspace:
    """What input expansion needs to know about the repo and the Nx graph."""

    files: frozenset[str]  # tracked plus untracked non-ignored, repo-relative
    roots: Mapping[str, str]  # project -> root
    named: Mapping[str, list[Any]]  # nx.json namedInputs
    project_named: Mapping[str, Mapping[str, list[Any]]] = field(default_factory=dict)
    deps: Mapping[str, list[str]] = field(default_factory=dict)  # direct dependencies


@dataclass
class Expanded:
    files: set[str] = field(default_factory=set)
    other: list[str] = field(default_factory=list)  # runtime/env/... entries


def _substitute(pattern: str, root: str) -> str:
    pattern = pattern.replace("{workspaceRoot}/", "").replace("{workspaceRoot}", "")
    if root in ("", "."):
        return pattern.replace("{projectRoot}/", "").replace("{projectRoot}", "")
    return pattern.replace("{projectRoot}", root)


def _named_entries(ws: Workspace, project: str, name: str) -> list[Any] | None:
    own = ws.project_named.get(project, {})
    if name in own:
        return list(own[name])
    if name in ws.named:
        return list(ws.named[name])
    if name in ("default", "production"):
        return ["{projectRoot}/**/*"]
    return None


def expand_inputs(
    ws: Workspace,
    project: str,
    inputs: Iterable[Any],
    _seen: frozenset[tuple[str, str]] = frozenset(),
) -> Expanded:
    """Expand `inputs` of `project` to a file set. `!` entries subtract from the entries of the same list."""
    result = Expanded()
    negated: set[str] = set()
    root = ws.roots.get(project, ".")
    for entry in inputs:
        if isinstance(entry, dict):
            if "input" in entry:
                sub = expand_inputs(ws, project, [entry["input"]], _seen)
                result.files |= sub.files
                result.other += sub.other
                continue
            for key in NO_FILE_KEYS:
                if key in entry:
                    result.other.append(f"{key}: {entry[key]}")
            continue
        if not isinstance(entry, str):
            continue
        if entry.startswith("!"):
            pattern = _substitute(entry[1:], root)
            negated |= {f for f in ws.files if glob_match(pattern, f)}
        elif entry.startswith("{"):
            pattern = _substitute(entry, root)
            result.files |= {f for f in ws.files if glob_match(pattern, f)}
        elif entry.startswith("^"):
            for dep in ws.deps.get(project, []):
                sub = _expand_named(ws, dep, entry[1:], _seen)
                result.files |= sub.files
                result.other += sub.other
        else:
            sub = _expand_named(ws, project, entry, _seen)
            result.files |= sub.files
            result.other += sub.other
    result.files -= negated
    return result


def _expand_named(
    ws: Workspace, project: str, name: str, seen: frozenset[tuple[str, str]]
) -> Expanded:
    if (project, name) in seen:
        return Expanded()
    entries = _named_entries(ws, project, name)
    if entries is None:
        return Expanded(other=[f"unknown named input {name} in {project}"])
    return expand_inputs(ws, project, entries, seen | {(project, name)})


# ---- classification (pure) ----------------------------------------------------------------


@dataclass
class Findings:
    undeclared: list[str] = field(default_factory=list)
    generated: list[str] = field(default_factory=list)
    outside_repo: list[str] = field(default_factory=list)
    spawned_tools: list[str] = field(default_factory=list)
    spawns: list[str] = field(default_factory=list)

    @property
    def failed(self) -> bool:
        return bool(self.undeclared or self.generated or self.spawned_tools)


def classify_reads(
    reads: Iterable[str],
    root: Path,
    declared: set[str],
    universe: frozenset[str],
    home: Path | None = None,
    ignore: Iterable[Path] = (),
) -> Findings:
    """Sort read paths into undeclared, generated and outside-repo findings."""
    found = Findings()
    skip = {Path(p) for p in ignore}
    undeclared: set[str] = set()
    generated: set[str] = set()
    outside: set[str] = set()
    for raw in reads:
        path = Path(raw)
        if path in skip:
            continue
        try:
            rel = path.relative_to(root)
        except ValueError:
            if home is not None and path.is_file():
                try:
                    path.relative_to(home)
                except ValueError:
                    continue
                if not any(p in IGNORED_PARTS for p in path.parts):
                    outside.add(str(path))
            continue
        if any(part in IGNORED_PARTS for part in rel.parts):
            continue
        if not path.is_file():
            continue
        rels = rel.as_posix()
        if rels in declared:
            continue
        if rels in universe:
            undeclared.add(rels)
        else:
            generated.add(rels)
    found.undeclared = sorted(undeclared)
    found.generated = sorted(generated)
    found.outside_repo = sorted(outside)
    return found


def classify_spawns(
    spawns: Iterable[list[str]], root: Path
) -> tuple[list[str], list[str]]:
    """Return (all distinct spawned commands, the ones that are hidden input tools)."""
    seen: dict[str, None] = {}
    tools_found: dict[str, None] = {}
    for argv in spawns:
        if not argv:
            continue
        text = shlex.join(argv)
        seen[text] = None
        name = Path(argv[0]).name
        in_repo = _in_repo(argv[0], root)
        if name == "node":
            script = [
                a for a in argv[1:] if not a.startswith("-") and _in_repo(a, root)
            ]
            if script:
                tools_found[text] = None
        elif name in SPAWN_TOOLS or in_repo:
            tools_found[text] = None
    return list(seen), list(tools_found)


def _in_repo(arg: str, root: Path) -> bool:
    if not os.path.isabs(arg):
        return False
    try:
        rel = Path(arg).relative_to(root)
    except ValueError:
        return False
    return not any(part in IGNORED_PARTS for part in rel.parts)


# ---- Nx and git access --------------------------------------------------------------------


def _run(cmd: list[str]) -> str:
    done = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, check=False, timeout=600
    )
    if done.returncode:
        raise RuntimeError(f"{shlex.join(cmd)} failed: {done.stderr[-1500:]}")
    return done.stdout


def nx_json(*args: str) -> Any:
    return json.loads(_run(["pnpm", "nx:pilot", *args, "--json"]))


def repo_files() -> frozenset[str]:
    tracked = _run(["git", "ls-files", "-z"]).split("\0")
    untracked = _run(["git", "ls-files", "-z", "--others", "--exclude-standard"]).split(
        "\0"
    )
    return frozenset(f for f in (*tracked, *untracked) if f)


def build_workspace(files: frozenset[str]) -> Workspace:
    from tools.devtools import verify_route

    graph = verify_route.nx_graph()
    nodes = graph["nodes"]
    roots = {n: d["data"].get("root", ".") for n, d in nodes.items()}
    project_named = {n: d["data"].get("namedInputs") or {} for n, d in nodes.items()}
    deps = {
        n: sorted({e["target"] for e in edges if e["target"] in nodes})
        for n, edges in graph["dependencies"].items()
    }
    named = json.loads((ROOT / "nx.json").read_text()).get("namedInputs", {})
    return Workspace(files, roots, named, project_named, deps)


def cached_targets() -> list[str]:
    out: list[str] = []
    for project in nx_json("show", "projects"):
        config = nx_json("show", "project", project)
        for target, cfg in sorted((config.get("targets") or {}).items()):
            if cfg.get("cache"):
                out.append(f"{project}:{target}")
    return out


def target_command(project: str, config: dict, target: str) -> tuple[str, Path]:
    cfg = config["targets"][target]
    opts = cfg.get("options") or {}
    command = opts.get("command")
    if command is None and opts.get("commands"):
        command = " && ".join(
            c if isinstance(c, str) else c["command"] for c in opts["commands"]
        )
    if command is None:
        raise RuntimeError(f"{project}:{target} has no options.command")
    command = command.replace("{projectRoot}", config.get("root", ".")).replace(
        "{projectName}", project
    )
    cwd = ROOT / opts.get("cwd", ".")
    return command, cwd


def run_traced(command: str, cwd: Path) -> tuple[int, list[dict]]:
    with tempfile.TemporaryDirectory() as tmp:
        trace = Path(tmp) / "trace.jsonl"
        trace.touch()
        old = os.environ.get("NODE_OPTIONS", "")
        env = tools.python_env(
            {
                **os.environ,
                "NODE_OPTIONS": f"--import {TRACE_PRELOAD} {old}".strip(),
                "DIGI_FS_TRACE": str(trace),
            }
        )
        done = subprocess.run(command, shell=True, cwd=cwd, env=env, check=False)
        events = []
        for line in trace.read_text().splitlines():
            try:
                events.append(json.loads(line))
            except ValueError:
                continue
        return done.returncode, events


# ---- driver -------------------------------------------------------------------------------


def runtime_hashed(other: Iterable[str], path: str) -> bool:
    """Whether a declared `runtime` input names repo path PATH: its command hashes the file, so a changed
    file changes the task hash (a generated file or a spawned binary declared by its content)."""
    return bool(path) and any(
        o.startswith("runtime:") and path in o.split() for o in other
    )


def _tool_path(spawn: str, root: Path) -> str:
    """The repo-relative path of a spawned command's executable, or "" when it is outside the repo."""
    first = spawn.split()[0] if spawn.split() else ""
    for path, base in ((Path(first), root), (Path(first).resolve(), root.resolve())):
        try:
            return path.relative_to(
                base
            ).as_posix()  # out/ may be a symlink into another checkout
        except ValueError:
            continue
    return ""


def audit_target(ws: Workspace, spec: str) -> dict[str, Any]:
    project, _, target = spec.partition(":")
    config = nx_json("show", "project", project)
    inputs = config["targets"][target].get("inputs") or ["default", "^default"]
    expanded = expand_inputs(ws, project, inputs)
    command, cwd = target_command(project, config, target)
    code, events = run_traced(command, cwd)
    reads = [e["read"] for e in events if "read" in e]
    spawns = [e["spawn"] for e in events if "spawn" in e]
    found = classify_reads(
        reads,
        ROOT,
        expanded.files,
        ws.files,
        home=Path.home(),
        ignore=[],
    )
    found.spawns, found.spawned_tools = classify_spawns(spawns, ROOT)
    found.generated = [
        p for p in found.generated if not runtime_hashed(expanded.other, p)
    ]
    found.spawned_tools = [
        s
        for s in found.spawned_tools
        if not runtime_hashed(expanded.other, _tool_path(s, ROOT))
    ]
    return {
        "target": spec,
        "command": command,
        "exit": code,
        "declared_files": len(expanded.files),
        "other_inputs": sorted(set(expanded.other)),
        "undeclared": found.undeclared,
        "generated": found.generated,
        "outside_repo": found.outside_repo,
        "spawned_tools": found.spawned_tools,
        "spawns": found.spawns,
        "failed": found.failed,
    }


def print_report(r: dict[str, Any]) -> None:
    status = "FAIL" if r["failed"] else "ok"
    print(
        f"== {r['target']}: {status} (command exit {r['exit']}, {r['declared_files']} declared files)"
    )
    if r["exit"]:
        print("  note: the target command failed; its reads may be incomplete")
    for item in r["other_inputs"]:
        print(f"  non-file input: {item}")
    sections = (
        ("undeclared", "undeclared"),
        (
            "generated",
            "generated input: declare a dependency task output or a runtime hash",
        ),
        ("spawned_tools", "spawned tool: declare its sources"),
        ("outside_repo", "outside repo"),
    )
    for key, label in sections:
        for item in r[key]:
            print(f"  {label}: {item}")
    others = [s for s in r["spawns"] if s not in r["spawned_tools"]]
    if others:
        print(f"  other spawns: {len(others)}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("targets", nargs="*", metavar="PROJECT:TARGET")
    ap.add_argument(
        "--all-cached", action="store_true", help="audit every target with cache: true"
    )
    ap.add_argument("--json", metavar="FILE", help="write the full results as JSON")
    args = ap.parse_args(argv)
    specs = list(args.targets)
    if args.all_cached:
        specs += [s for s in cached_targets() if s not in specs]
    if not specs:
        ap.error("give PROJECT:TARGET or --all-cached")
    ws = build_workspace(repo_files())
    results = []
    for spec in specs:
        result = audit_target(ws, spec)
        print_report(result)
        results.append(result)
    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2) + "\n")
    return 1 if any(r["failed"] for r in results) else 0


if __name__ == "__main__":
    sys.exit(main())
