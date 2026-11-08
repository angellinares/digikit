#!/usr/bin/env python3
"""Shared cargo target dirs for the native workspace, keyed by generated core.

The generated SHARC core (``packages/emulator/sharc-gen``, includes ``SHARC_GEN_DIR``) is
80 MB of Rust and takes minutes to compile. Cargo recompiles it for every
distinct target dir and every distinct ``SHARC_GEN_DIR``, so every tool picks
its target dir from here:

    out/native/target/<gen-key>[/<variant>]

``gen-key`` names the generated core, never the tool:

    prod       out/native/dn2-audio/gen (what the desktop app and PGO use)
    nogen      no generated core (SHARC_GEN_DIR unset or missing)
    generic    any other transpile-only gen (no image.rs), e.g. the browser
               measurement's out/native/dn2-audio/gen-generic
    g-<hash>   any other gen with an image: hash of its resolved path

In a git worktree (``.git`` is a file) the root is ``dist/native-target/``
inside the worktree instead (``dist/`` is git-ignored and nothing cleans it).
The target dirs belong to the lane and are deleted with it. ``out/`` is linked
to the main tree's, and two checkouts building into one target dir would keep
invalidating each other's crates. A worktree cannot reuse the main tree's
compiled crates (the package paths differ), so it pays one ``sharc-gen`` compile
per gen and then reuses it.

Builds that differ by RUSTFLAGS (PGO instrument / use) cannot share compiled
crates and get their own ``variant`` subdir under the same key. Plain release
builds, the desktop app, the sharc-fast-cl examples and the tests all use the
key dir itself, so they reuse one sharc-gen.

    uv run python -m tools.emulator.native_target key [--gen DIR]
    uv run python -m tools.emulator.native_target dir [--gen DIR] [--variant NAME]
"""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.emulator import digi_env  # noqa: E402

# A worktree's .git is a file; its target dirs live inside the worktree. Keep in step with
# targetDir in tools/nx/rust-env.mjs.
IS_WORKTREE = (ROOT / ".git").is_file()
TARGET_ROOT = (
    ROOT / "dist" / "native-target" if IS_WORKTREE else ROOT / "out" / "native" / "target"
)
PROD_GEN = digi_env.DEFAULT_GENERATED


def gen_key(gen: str | os.PathLike[str] | None) -> str:
    """The key of a generated-core directory (None or missing -> ``nogen``)."""
    if gen is None or str(gen) == "":
        return "nogen"
    path = pathlib.Path(gen)
    if not path.is_dir():
        return "nogen"
    resolved = path.resolve()
    if resolved == PROD_GEN.resolve():
        return "prod"
    if not (resolved / "image.rs").is_file():
        return "generic"
    return "g-" + hashlib.sha256(str(resolved).encode()).hexdigest()[:8]


def gen_from_env(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> str | None:
    environ = os.environ if environ is None else environ
    return environ.get("SHARC_GEN_DIR") or None


def target_dir(
    gen: str | os.PathLike[str] | None = None,
    variant: str = "",
    second: str | os.PathLike[str] | None = None,
) -> pathlib.Path:
    """Target dir for a gen (default: the SHARC_GEN_DIR environment variable); with a SECOND gen (the
    second-core build, SHARC_GEN_DIR_2) the key names both."""
    if gen is None:
        gen = gen_from_env()
    key = gen_key(gen)
    if second is not None and str(second) != "":
        key = f"{key}+{gen_key(second)}"
    path = TARGET_ROOT / key
    return path / variant if variant else path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    parser.add_argument("what", choices=["key", "dir"])
    parser.add_argument("--gen", help="generated core dir (default: $SHARC_GEN_DIR)")
    parser.add_argument("--variant", default="")
    args = parser.parse_args(argv)
    gen = args.gen if args.gen is not None else gen_from_env()
    print(gen_key(gen) if args.what == "key" else target_dir(gen, args.variant))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
