#!/usr/bin/env python3
"""Build the shared static panel then run the native desktop host."""
from __future__ import annotations

import pathlib
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["pnpm", "--filter", "@digi/web", "build"], cwd=root, check=True)
subprocess.run(["cargo", "run", "--release", "--locked", "--manifest-path", "packages/desktop/src-tauri/Cargo.toml", "--", *sys.argv[1:]], cwd=root, check=True)
