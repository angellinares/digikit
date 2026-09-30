#!/usr/bin/env python3
"""Safe convenience entry point for the bounded native boot diagnostic."""

from __future__ import annotations

import os
import subprocess
import sys
import secrets
from pathlib import Path


USAGE = "usage: mise run boot -- SYX [--out DIR] [--card-image FILE] [--limit N] [--stop-at ready|limit]"
ROOT = Path(__file__).resolve().parent.parent


def fail(message: str) -> None:
    print(f"boot: {message}", file=sys.stderr)
    raise SystemExit(2)


def main(argv: list[str]) -> int:
    if argv == ["--help"] or argv == ["-h"]:
        print(USAGE)
        return 0
    if not argv or argv[0].startswith("-"):
        fail("SYX positional argument is required\n" + USAGE)
    syx = Path(argv.pop(0)).expanduser().resolve()
    values: dict[str, str] = {}
    while argv:
        flag = argv.pop(0)
        if flag not in {"--out", "--card-image", "--limit", "--stop-at"}:
            fail(f"unknown flag {flag}")
        if not argv:
            fail(f"missing value for {flag}")
        if flag in values:
            fail(f"duplicate {flag}")
        values[flag] = argv.pop(0)
    if not syx.is_file():
        fail(f"SYX is not a readable file: {syx}")
    card_image = values.get("--card-image")
    if card_image is not None:
        card = Path(card_image).expanduser().resolve()
        if not card.is_file():
            fail(f"--card-image is not a readable regular file: {card}")
        try:
            with card.open("rb"):
                pass
        except OSError as error:
            fail(f"--card-image is not readable: {error}")
        size = card.stat().st_size
        if size == 0 or size % 512:
            fail("--card-image length must be a nonzero multiple of 512")
        blocks = size // 512
        if blocks not in {0x760000, 0x3B0000}:
            fail(f"--card-image has unsupported capacity {blocks:#x} sectors")
    limit = values.get("--limit", "1000000000")
    if not limit.isdecimal() or not 1 <= int(limit) <= 1_000_000_000:
        fail("--limit must be an integer from 1 through 1000000000")
    stop_at = values.get("--stop-at", "ready")
    if stop_at not in {"ready", "limit"}:
        fail("--stop-at must be ready or limit")
    explicit_out = values.get("--out")
    if explicit_out is None:
        default_base = ROOT / "out" / "native" / "boot"
        default_base.mkdir(parents=True, exist_ok=True)
        while True:
            out = default_base / f"boot-{secrets.token_hex(8)}"
            if not out.exists():
                break
    else:
        out = Path(explicit_out).expanduser().resolve()
        if out.exists() and (not out.is_dir() or any(out.iterdir())):
            fail(f"--out must be a new or empty directory: {out}")
    print("Oracle diagnostic bundle enabled; full hardware and DSP remain pending.")
    print(f"report directory: {out}")
    subprocess.run([
        "cargo", "build", "--manifest-path", str(ROOT / "native/boot/Cargo.toml"),
        "--release", "--locked", "--offline",
    ], cwd=ROOT, check=True)
    command = [
        str(ROOT / "native/boot/target/release/elektron-native-boot"),
        "--syx", str(syx), "--out", str(out), "--mode", "oracle-diagnostic",
        "--limit", limit, "--stop-at", stop_at, "--diagnostic-services",
    ]
    if card_image is not None:
        command.extend(["--card-image", str(card)])
    return subprocess.run(command, cwd=ROOT).returncode


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
