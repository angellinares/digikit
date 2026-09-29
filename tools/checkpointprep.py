#!/usr/bin/env python3
"""Prepare and gate trusted local 24M checkpoints without exporting firmware.

The only pickle boundary is :class:`tools.snapread.Snapshot`, used after the
selected local firmware source and extracted MAIN OS have matched their fixed
product hashes.  `prepare` writes portable MSTATE below ignored ``out/``;
`gate` repeats all checks then launches the bounded native test.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from tools import snapconv  # noqa: E402
from tools.snapread import Snapshot  # noqa: E402

FORMAT_VERSION = 1
LOAD_ADDRESS = 0x40000400  # emu/boot.py: LOAD


@dataclass(frozen=True)
class Product:
    source_sha256: str
    image_sha256: str
    section_dir: str
    snapshot: str
    output: str


PRODUCTS = {
    "dt2": Product(
        "278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec",
        "57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d",
        "out/sections/dt2-1.16",
        "out/mmio-trace/early/boot24M.snap",
        "out/native/checkpoint-gate/dt2-boot24M.mstate",
    ),
    "dn2": Product(
        "2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0",
        "57b06a7960b7c3dc9803bde6b31896b89a2fbdcc404a932ff503f3856d4d7a61",
        "out/sections/dn2-1.11",
        "out/mmio-trace/early/dn2-boot24M.snap",
        "out/native/checkpoint-gate/dn2-boot24M.mstate",
    ),
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require_hash(path: Path, expected: str, label: str) -> None:
    if not path.is_file():
        raise ValueError(f"{label} is missing")
    if sha256(path) != expected:
        raise ValueError(f"{label} SHA-256 mismatch")


def verify(product: str, syx: Path, snapshot_path: Path | None = None) -> Snapshot:
    """Verify local source/image provenance, then open the trusted snapshot."""
    config = PRODUCTS[product]
    require_hash(syx, config.source_sha256, f"{product} source")
    section_dir = ROOT / config.section_dir
    source_hash = section_dir / ".source-sha256"
    image = section_dir / "section_3_MAIN_OS.bin"
    if (
        not source_hash.is_file()
        or source_hash.read_text().strip() != config.source_sha256
    ):
        raise ValueError(f"{product} section source hash mismatch")
    require_hash(image, config.image_sha256, f"{product} MAIN OS")
    snapshot = Snapshot(str(snapshot_path or ROOT / config.snapshot))
    loaded = snapshot.read(LOAD_ADDRESS, image.stat().st_size)
    if loaded != image.read_bytes():
        raise ValueError(f"{product} snapshot loaded MAIN OS mismatch")
    if snapshot.components:
        raise ValueError(f"{product} snapshot has unsupported components")
    return snapshot


def prepare(product: str, syx: Path, snapshot_path: Path | None = None) -> Path:
    """Verify and convert one local snapshot to ignored portable MSTATE."""
    snapshot = verify(product, syx, snapshot_path)
    output = ROOT / PRODUCTS[product].output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(snapconv.convert_blob(snapshot._blob))
    return output


def gate(dt2_syx: Path, dn2_syx: Path) -> None:
    """Re-verify both products, prepare them, then run the ignored 1k gate."""
    dt2 = prepare("dt2", dt2_syx)
    dn2 = prepare("dn2", dn2_syx)
    env = os.environ | {
        "NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK": "unverified-local-inputs",
        "DT2_CHECKPOINT_MSTATE": str(dt2.resolve()),
        "DN2_CHECKPOINT_MSTATE": str(dn2.resolve()),
    }
    print("checkpointprep source-verified products=dt2,dn2", flush=True)
    subprocess.run(
        [
            "cargo",
            "test",
            "--release",
            "--test",
            "checkpoint",
            "--",
            "--ignored",
            "--nocapture",
        ],
        cwd=ROOT / "native/machine",
        env=env,
        check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", action="version", version=str(FORMAT_VERSION))
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("product", choices=PRODUCTS)
    prepare_parser.add_argument("--syx", type=Path, required=True)
    prepare_parser.add_argument("--snapshot", type=Path)
    gate_parser = commands.add_parser("gate")
    gate_parser.add_argument("--dt2-syx", type=Path, required=True)
    gate_parser.add_argument("--dn2-syx", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args.product, args.syx, args.snapshot)
    else:
        gate(args.dt2_syx, args.dn2_syx)


if __name__ == "__main__":
    main()
