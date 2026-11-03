"""Pure parts of tools/devtools/nx_input_audit.py: input expansion and read classification."""

from __future__ import annotations

from pathlib import Path

from tools.devtools import nx_input_audit as audit

FILES = frozenset(
    {
        "apps/a/src/x.ts",
        "apps/a/README.md",
        "libs/b/index.ts",
        "libs/b/test/t.ts",
        "devices/dt2.toml",
        "package.json",
    }
)


def make_ws() -> audit.Workspace:
    return audit.Workspace(
        files=FILES,
        roots={"a": "apps/a", "b": "libs/b"},
        named={
            "src": ["{projectRoot}/**/*", "!{projectRoot}/**/*.md"],
            "tool": ["{workspaceRoot}/package.json", {"env": "CI"}],
            "both": ["src", "tool"],
        },
        project_named={"b": {"src": ["{projectRoot}/*.ts"]}},
        deps={"a": ["b"]},
    )


def test_glob_double_star() -> None:
    assert audit.glob_match("apps/a/**/*", "apps/a/src/x.ts")
    assert audit.glob_match("apps/a/**/*", "apps/a/README.md")
    assert not audit.glob_match("apps/a/*", "apps/a/src/x.ts")
    assert audit.glob_match("tsconfig*.json", "tsconfig.base.json")


def test_named_input_negation_and_other() -> None:
    got = audit.expand_inputs(make_ws(), "a", ["both"])
    assert got.files == {"apps/a/src/x.ts", "package.json"}
    assert got.other == ["env: CI"]


def test_caret_uses_direct_dependency_and_its_own_named_input() -> None:
    got = audit.expand_inputs(make_ws(), "a", ["^src"])
    assert got.files == {"libs/b/index.ts"}


def test_workspace_root_glob() -> None:
    got = audit.expand_inputs(make_ws(), "a", ["{workspaceRoot}/devices/*.toml"])
    assert got.files == {"devices/dt2.toml"}


def test_classify_reads(tmp_path: Path) -> None:
    for rel in (
        "apps/a/src/x.ts",
        "devices/dt2.toml",
        "out/gen.bin",
        "node_modules/m/i.js",
    ):
        f = tmp_path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text("x")
    universe = frozenset({"apps/a/src/x.ts", "devices/dt2.toml"})
    reads = [
        str(tmp_path / "apps/a/src/x.ts"),
        str(tmp_path / "devices/dt2.toml"),
        str(tmp_path / "out/gen.bin"),
        str(tmp_path / "node_modules/m/i.js"),
        str(tmp_path / "apps"),  # directory
        str(tmp_path / "missing.txt"),
    ]
    got = audit.classify_reads(reads, tmp_path, {"apps/a/src/x.ts"}, universe)
    assert got.undeclared == ["devices/dt2.toml"]
    assert got.generated == ["out/gen.bin"]
    assert got.failed


def test_classify_reads_outside_repo(tmp_path: Path) -> None:
    repo = tmp_path / "home" / "repo"
    repo.mkdir(parents=True)
    other = tmp_path / "home" / "cfg.json"
    other.write_text("{}")
    elsewhere = tmp_path / "etc.txt"
    elsewhere.write_text("x")
    got = audit.classify_reads(
        [str(other), str(elsewhere)], repo, set(), frozenset(), home=tmp_path / "home"
    )
    assert got.outside_repo == [str(other)]
    assert not got.failed


def test_classify_spawns() -> None:
    root = Path("/repo")
    all_spawns, hidden = audit.classify_spawns(
        [
            ["uv", "run", "python", "-m", "tools.x"],
            ["uv", "run", "python", "-m", "tools.x"],
            ["node", "/repo/node_modules/.bin/vitest"],
            ["node", "/repo/tools/nx/run.mjs"],
            ["sh", "-c", "echo"],
        ],
        root,
    )
    assert len(all_spawns) == 4
    assert hidden == [
        "uv run python -m tools.x",
        "node /repo/tools/nx/run.mjs",
    ]


def test_runtime_hash_declares_a_generated_file() -> None:
    other = ["runtime: shasum -a 256 out/bin/tool 2>/dev/null || echo none", "env: CI"]
    assert audit.runtime_hashed(other, "out/bin/tool")
    assert not audit.runtime_hashed(other, "out/bin/other")
    assert not audit.runtime_hashed(["env: out/bin/tool"], "out/bin/tool")
    assert not audit.runtime_hashed(other, "")


def test_target_command_run_script() -> None:
    config = {
        "root": "packages/p",
        "targets": {
            "typecheck": {
                "executor": "nx:run-script",
                "options": {"script": "typecheck"},
            }
        },
    }
    assert audit.target_command("p", config, "typecheck") == (
        "pnpm run typecheck",
        audit.ROOT / "packages/p",
    )
