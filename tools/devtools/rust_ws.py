#!/usr/bin/env python3
"""Run cargo over the native workspace and standalone dkrt host tests with the pinned toolchain.

    uv run python -m tools.devtools.rust_ws test [-- CARGO_ARGS...]   cargo nextest run --workspace --release
    uv run python -m tools.devtools.rust_ws test --packages A,B       cargo nextest run -p A -p B --release (verify's affected crates)
    uv run python -m tools.devtools.rust_ws check [-- CARGO_ARGS...]  cargo check --workspace --all-targets
    uv run python -m tools.devtools.rust_ws gates                     the ignored coupled gate (needs the private DN2 ready inputs)
    uv run python -m tools.devtools.rust_ws fmt                       cargo fmt --check on the clean crates

Workspace ``test`` and ``check`` use the generated core of ``tools/emulator/digi_env.py`` and its
target dir (``tools/emulator/native_target.py``), so they reuse the compiled
``sharc-gen``. The desktop crate (digiemu-desktop) is left out of ``test``: it embeds
``apps/desktop/ui/dist`` and links the generated core into its own test binary, and verify's
``desktop`` step builds and tests it (its own target dir). ``check`` still covers it when the
console has been built. ``fmt`` covers only ``CLEAN_CRATES``:
add a crate once ``cargo fmt -p CRATE --check`` passes; the list should only
grow. Standalone dkrt tests run sequentially after workspace tests;
``test --packages dkrt`` needs no generated core or native build. Extra test
arguments go only to nextest when workspace tests run; standalone-only selection
forwards them to cargo test instead.
"""

from __future__ import annotations

import functools
import json
import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]

from tools.emulator import digi_env, native_target  # noqa: E402

CLEAN_CRATES = [
    "bootstrap-emu",
    "coldfire",
    "device-profile",
    "digiemu-desktop",
    "dkneeds",
    "dkrt-fit",
    "dt2-firmware-loader",
    "live-audio",
    "patchkit",
    "periph",
    "plusdrive-format",
    "sharc-gen",
]


# ---------------------------------------------------------------------------
# Test selection: which workspace/standalone packages a set of changed files can affect.
# ---------------------------------------------------------------------------

# Files whose change can affect any crate: the whole workspace runs.
WHOLE_WORKSPACE = re.compile(
    r"^(Cargo\.toml|Cargo\.lock|rust-toolchain(\.toml)?|\.cargo/.*)$"
)

# Non-Rust inputs that Rust code reads: (regex on the repo-relative path, packages that read it, closure).
# closure=True: the file is compiled in (build.rs, include_str!), so every dependent's tests can see it and
# the reverse dependency closure is added. closure=False: only the listed packages' tests read the file at
# run time, so only they run.
READS: list[tuple[str, list[str], bool]] = [
    # packages/patchkit/build.rs:10-30 embeds every devices/*.toml (rerun-if-changed on the dir and
    # each file); src/modgen.rs:241-242 include_str! the two device files; packages/patchkit/tests/devdata.rs:83-117
    # reads them. (Not devices/ports/.)
    (r"^devices/[^/]+\.toml$", ["patchkit"], True),
    # packages/emulator/device-profile/src/lib.rs:27-36 and :542-548 include_str! the two device TOMLs.
    (r"^devices/(digitakt-ii|digitone-ii)\.toml$", ["device-profile"], True),
    # patchkit tests (typed.rs:74,92 preflight.rs:25 frameworks.rs:23 conflicts.rs:22 prims.rs:34
    # dynamic.rs:47 devdata.rs:136) and src/main.rs:59 read devices/ports/*.toml;
    # packages/emulator/boot/tests/patched_boot.rs:14 reads it too.
    (r"^devices/ports/", ["patchkit", "elektron-native-boot"], False),
    # packages/emulator/control/src/load.rs:18 include_str! devices/cycle-factors.json.
    (r"^devices/cycle-factors\.json$", ["digi-control"], True),
    # Library::scan_repo (modules/, tests/fixtures/modules/) reads every module.toml, the committed build/*.dko.json of core/* and builds
    # the objects of content modules from their sources into out/cache/dko:
    # packages/patchkit/tests/devdata.rs:19, conflicts.rs:27, frameworks.rs:27, dynamic.rs:17,
    # typed.rs:186 (tests/fixtures/modules/patched-pluck); packages/emulator/boot/tests/patched_boot.rs:15.
    # Markdown under modules/ is documentation only.
    (
        r"^(modules|tests/fixtures/modules)/(?!.*\.md$)",
        ["patchkit", "elektron-native-boot"],
        False,
    ),
    # packages/emulator/coupled/src/scenario.rs:12 include_str! tests/scenarios/dn2.json (the other
    # scenario files are passed to coupled_run by path at run time, never read by cargo tests).
    (r"^tests/scenarios/dn2\.json$", ["digi-coupled"], True),
    # Stock firmware (untracked, so never in a git diff; listed for local edits): patchkit tests
    # (typed.rs:93 preflight.rs:24 codec.rs:14 dynamic.rs:49 conflicts.rs:16), boot
    # (src/runtime.rs:3208.., packages/emulator/boot/tests/patched_boot.rs:44), plusdrive (src/project.rs:365),
    # control (packages/emulator/control/tests/card_roundtrip.rs:47, card_export.rs:39, card_load.rs:34, src/cards.rs:309).
    (
        r"^firmware/",
        ["patchkit", "elektron-native-boot", "plusdrive-format", "digi-control"],
        False,
    ),
    # packages/emulator/sharc-gen/build.rs:35-39 (rerun-if-changed on SHARC_GEN_DIR and its .rs
    # files, include! in src/lib.rs:106-121); packages/emulator/sharc/build.rs:26 reads the same
    # dir. out/native/*/gen is untracked; listed for local edits.
    (r"^out/native/[^/]+/gen/", ["sharc-gen", "sharc-native"], True),
    # modules/core/adapters/ui/host/build.rs compiles its modules/core/adapters/ui/host/tests/c sources against sdk/include (rerun-if-changed on the dir); modules/core/adapters/ui/host/tests/c/menu_fake.c includes the C source of
    # core/digikit-menu and builds it with the runtime's header and core/ui's core-only header.
    (r"^sdk/include/", ["dkui-host"], False),
    (
        r"^modules/core/(runtime/include/dkrt\.h|adapters/ui/include/)",
        ["dkui-host"],
        False,
    ),
    (r"^modules/core/apps/digikit-menu/src/", ["dkui-host"], False),
    # apps/desktop/src-tauri/tauri.conf.json frontendDist = ../ui/dist (embedded by tauri_build).
    (r"^apps/desktop/ui/dist/", ["digiemu-desktop"], True),
]

# Files that no Rust code reads. tools/emulator/fast_regions.json is passed to the engine through
# the ABI by Python (packages/emulator/boot/src/abi.rs:501 only mentions it in a comment); the other
# out/ inputs (mmio-trace, native/live, card images) are untracked, env-gated test inputs.
NO_RUST = re.compile(
    r"^(docs/|tools/|tests/|apps/web/|apps/desktop/(ui/|package\.json)|samples/|plus_drive/|out/|"
    r"packages/(control-client|test-support)/|\.claude/|node_modules/)"
    r"|\.md$"
    r"|^(mise\.toml|pyproject\.toml|uv\.lock|package\.json|pnpm-.*|LICENSE|NOTICE|\.gitignore|"
    r"\.gitattributes|\.editorconfig|\.git-blame-ignore-revs|\.oxlintrc\.json|\.prettier.*)$"
)


@functools.cache
def _metadata(manifest: str | None = None) -> dict:
    out = subprocess.run(
        [
            "rustup",
            "run",
            digi_env.TOOLCHAIN,
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            *(["--manifest-path", manifest] if manifest else []),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(out)


STANDALONE = {"dkrt": "modules/core/runtime/src/rs/Cargo.toml"}


def _graph() -> tuple[dict[str, str], dict[str, set[str]]]:
    """(package name -> manifest dir relative to ROOT, package name -> its local dependents)."""
    packages = list(_metadata()["packages"])
    for manifest in STANDALONE.values():
        packages.extend(_metadata(manifest)["packages"])
    dirs = {
        p["name"]: pathlib.Path(p["manifest_path"]).parent.relative_to(ROOT).as_posix()
        for p in packages
    }
    dependents: dict[str, set[str]] = {name: set() for name in dirs}
    by_path = {(ROOT / directory).resolve(): name for name, directory in dirs.items()}
    for p in packages:
        for dep in p["dependencies"]:  # normal, dev and build dependencies
            if dep.get("path") and (
                owner := by_path.get(pathlib.Path(dep["path"]).resolve())
            ):
                dependents[owner].add(p["name"])
    return dirs, dependents


def affected_packages(changed: list[str]) -> list[str] | None:
    """Workspace/standalone packages whose tests the changed files (repo-relative) can affect.

    Returns a sorted list of package names (empty: nothing Rust is affected), or None when the
    whole workspace must run: root Cargo.toml / Cargo.lock / rust-toolchain* / .cargo/*, or a
    file that fits no package, no READS row and no NO_RUST pattern.
    """
    dirs, dependents = _graph()
    seeds: set[str] = set()  # packages whose dependents also run
    readers: set[str] = set()  # packages that run alone (run-time reads)
    for raw in changed:
        path = raw.strip().removeprefix("./")
        if not path:
            continue
        if WHOLE_WORKSPACE.match(path):
            return None
        owners = [n for n, d in dirs.items() if path.startswith(d + "/")]
        if owners:
            seeds.add(max(owners, key=lambda n: len(dirs[n])))
        # Preserve existing workspace ownership rules. A newly owned standalone
        # source must also retain its prior run-time readers (patchkit/boot).
        rows = (
            [(pkgs, closure) for rx, pkgs, closure in READS if re.search(rx, path)]
            if not owners or any(n in STANDALONE for n in owners)
            else []
        )
        for pkgs, closure in rows:
            (seeds if closure else readers).update(pkgs)
        if not owners and not rows and not NO_RUST.search(path):
            return None
    todo, closure = list(seeds), set(seeds)
    while todo:
        for user in dependents[todo.pop()]:
            if user not in closure:
                closure.add(user)
                todo.append(user)
    return sorted(closure | readers)


DESKTOP = "digiemu-desktop"


def workspace_args(mode: str = "check") -> list[str]:
    """`--workspace` and what it leaves out: the desktop crate in `test` (verify's desktop step tests it), and
    in `check` when its console (apps/desktop/ui/dist) has not been built."""
    args = ["--workspace"]
    if mode == "test" or not (ROOT / "apps/desktop/ui/dist").is_dir():
        args += ["--exclude", DESKTOP]
    return args


def cargo(sub: list[str], env: dict[str, str]) -> int:
    command = ["rustup", "run", digi_env.TOOLCHAIN, "cargo", *sub]
    return subprocess.run(command, cwd=ROOT, env=env, check=False).returncode


def test_packages(packages: list[str] | None, extra: list[str]) -> int:
    """Run the selected workspace and standalone tests sequentially in one job."""
    workspace = (
        None
        if packages is None
        else [p for p in packages if p != DESKTOP and p not in STANDALONE]
    )
    standalone = (
        list(STANDALONE)
        if packages is None
        else [p for p in STANDALONE if p in packages]
    )
    if workspace is None or workspace:
        env = digi_env.rust_env()
        gen = digi_env.generated_dir()
        if not gen.is_dir():
            raise SystemExit(f"rust_ws: generated core dir is missing: {gen}")
        digi_env.check_generated(gen)
        env["SHARC_GEN_DIR"] = str(gen)
        selection = (
            workspace_args("test")
            if workspace is None
            else [x for p in workspace for x in ("-p", p)]
        )
        result = cargo(
            [
                "nextest",
                "run",
                *selection,
                "--release",
                "--target-dir",
                str(native_target.target_dir(gen)),
                "--locked",
                *extra,
            ],
            env,
        )
        if result:
            return result
    for package in standalone:
        # Keep this host-only path independent of generated-core configuration.
        env = digi_env.rust_env(
            {
                k: v
                for k, v in os.environ.items()
                if k not in ("SHARC_GEN_DIR", "SHARC_GEN_DIR_2")
            }
        )
        result = cargo(
            [
                "test",
                "--manifest-path",
                STANDALONE[package],
                "--lib",
                "--offline",
                "--target-dir",
                str(native_target.target_dir("", f"host-{package}")),
                "--locked",
                *(extra if workspace == [] else []),
            ],
            env,
        )
        if result:
            return result
    if workspace == [] and not standalone:
        print("rust_ws: no selected packages (desktop tests run in the desktop step)")
    return 0


def main(argv: list[str]) -> int:
    if not argv or argv[0] not in ("test", "check", "gates", "fmt"):
        print(__doc__)
        return 2
    mode, extra = argv[0], argv[1:]
    packages: list[str] | None = None
    if extra[:1] == ["--packages"] and len(extra) > 1:
        packages = [p for p in extra[1].split(",") if p]
        extra = extra[2:]
    if extra[:1] == ["--"]:
        extra = extra[1:]
    if mode == "test":
        return test_packages(packages, extra)
    env = digi_env.rust_env()
    if mode == "fmt":
        crates = [x for name in CLEAN_CRATES for x in ("-p", name)]
        return cargo(["fmt", "--check", *crates], env)
    gen = digi_env.generated_dir()
    if not gen.is_dir():
        raise SystemExit(f"rust_ws: generated core dir is missing: {gen}")
    digi_env.check_generated(gen)
    env["SHARC_GEN_DIR"] = str(gen)
    target = ["--target-dir", str(native_target.target_dir(gen)), "--locked"]
    if mode == "gates":
        # coupled_ready_exactness: restores the ready session, 100M instructions.
        env["DIGI_COUPLED_FIXTURES"] = str(digi_env.READY)
        env["DIGI_COUPLED_SYX"] = str(digi_env.firmware())
        sub = [
            "test",
            "--release",
            "-p",
            "digi-coupled",
            "--features",
            "coupled-audio",
            "--lib",
            *target,
            "gate_tests",
            "--",
            "--ignored",
        ]
        return cargo(sub, env)
    if mode == "check":
        sub = ["check", *workspace_args(), "--all-targets", *target]
    return cargo([*sub, *extra], env)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
