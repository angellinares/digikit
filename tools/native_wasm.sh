#!/usr/bin/env python3
"""Build the portable native runtime for the browser without changing toolchains."""

from __future__ import annotations

import argparse
import os
import pathlib
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--diagnostics",
    action="store_true",
    help="include bounded PC profiling and event history",
)
parser.add_argument(
    "--sharc",
    action="store_true",
    help="dev build with the coupled SHARC+ engine (needs SHARC_GEN_DIR, firmware-derived "
    "generated code) written to emulator-core-sharc.wasm; the default core is left alone",
)
args = parser.parse_args()
if args.sharc and not os.environ.get("SHARC_GEN_DIR"):
    raise SystemExit("--sharc needs SHARC_GEN_DIR (a directory written by tools/sharc_transpile.py and sharc_rsgen.py)")
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
command = [
    "cargo",
    "build",
    "--manifest-path",
    "native/boot/Cargo.toml",
    "--locked",
    "--offline",
    "--release",
    "--lib",
    "--target",
    "wasm32-unknown-unknown",
]
features = []
if args.diagnostics:
    features += ["diagnostic-profile", "diagnostic-events"]
if args.sharc:
    features.append("sharc")
if features:
    command.extend(["--features", ",".join(features)])
try:
    subprocess.run(command, cwd=root, env=env, check=True)
except subprocess.CalledProcessError:
    raise SystemExit(
        "WASM build failed; install wasm32-unknown-unknown for the pinned Rust toolchain"
    ) from None
target_dir = pathlib.Path(env.get("CARGO_TARGET_DIR", root / "native/boot/target"))
if not target_dir.is_absolute():
    target_dir = root / target_dir
source = target_dir / "wasm32-unknown-unknown/release/elektron_native_boot.wasm"
target = root / "packages/web/public" / (
    "emulator-core-sharc.wasm" if args.sharc else "emulator-core.wasm"
)
target.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(source, target)
