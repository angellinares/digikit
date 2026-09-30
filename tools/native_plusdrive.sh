#!/usr/bin/env python3
"""Build and run the standalone +Drive CLI without shell interpolation."""
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "native" / "plusdrive" / "Cargo.toml"
if any(arg in ("--help", "-h") for arg in sys.argv[1:]):
    subprocess.run(["cargo", "run", "--manifest-path", str(MANIFEST), "--bin", "plusdrive", "--", *sys.argv[1:]], cwd=ROOT, check=True)
    raise SystemExit
subprocess.run(["cargo", "build", "--manifest-path", str(MANIFEST), "--bin", "plusdrive", "--release", "--locked", "--offline"], cwd=ROOT, check=True)
subprocess.run([str(ROOT / "native" / "plusdrive" / "target" / "release" / "plusdrive"), *sys.argv[1:]], cwd=ROOT, check=True)
