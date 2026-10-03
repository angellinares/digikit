#!/usr/bin/env python3
"""Build the shared panel and run the native host; coupling is local-only and opt-in."""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]


def cargo_command(args: list[str]) -> list[str]:
    command = [
        "cargo",
        "run",
        "--release",
        "--locked",
        "--manifest-path",
        "packages/desktop/src-tauri/Cargo.toml",
    ]
    if "--coupled" in args:
        command.extend(["--features", "coupled-audio"])
    return [*command, "--", *args]


def main(args: list[str] | None = None) -> None:
    args = sys.argv[1:] if args is None else args
    if "--coupled" in args:
        generated = os.environ.get("SHARC_GEN_DIR")
        if not generated or not pathlib.Path(generated).is_dir():
            raise SystemExit(
                "--coupled needs SHARC_GEN_DIR pointing to a local generated DN2 core"
            )
    subprocess.run(["pnpm", "--filter", "@digi/web", "build"], cwd=ROOT, check=True)
    subprocess.run(cargo_command(args), cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
