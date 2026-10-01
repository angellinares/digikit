#!/usr/bin/env python3
"""Build the portable native runtime for the browser without changing toolchains."""
from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys


root = pathlib.Path(__file__).resolve().parents[1]
rustc = shutil.which("rustup")
if rustc is not None:
    found = subprocess.run([rustc, "which", "rustc"], text=True, capture_output=True)
    rustc = found.stdout.strip() if found.returncode == 0 else None
if rustc is None:
    rustc = shutil.which("rustc")
if rustc is None:
    raise SystemExit("rustc is required; install the pinned mise Rust toolchain first")
sysroot = subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip()
env = os.environ | {
    "PATH": f"{pathlib.Path(rustc).parent}:{os.environ['PATH']}",
    "DYLD_FALLBACK_LIBRARY_PATH": f"{sysroot}/lib",
    "RUSTC": rustc,
    "RUSTDOC": str(pathlib.Path(rustc).with_name("rustdoc")),
}
command = ["cargo", "build", "--manifest-path", "native/boot/Cargo.toml", "--locked", "--offline", "--release", "--lib", "--target", "wasm32-unknown-unknown"]
try:
    subprocess.run(command, cwd=root, env=env, check=True)
except subprocess.CalledProcessError:
    raise SystemExit("WASM build failed; install wasm32-unknown-unknown for the pinned Rust toolchain") from None
source = root / "native/boot/target/wasm32-unknown-unknown/release/elektron_native_boot.wasm"
target = root / "packages/web/public/emulator-core.wasm"
target.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(source, target)
