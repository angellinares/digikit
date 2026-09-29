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
import json
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


def _limit(value: int) -> int:
    if not 1 <= value <= 1000:
        raise ValueError("limit must be between 1 and 1000")
    return value


def _regs(uc, constants) -> dict[str, object]:
    from tools.cf_lockstep import uc_get_regs

    regs = uc_get_regs(uc, constants)
    return {
        "d": [int(value) for value in regs.d],
        "a": [int(value) for value in regs.a],
        "pc": int(regs.pc),
        "sr": int(regs.sr),
    }


def _trace_states(product, uc, constants, trace, limit, run_step):
    """Capture bounded boundaries; a valid branch may deliberately retain PC."""
    states = [{**_regs(uc, constants), "clock": 0}]
    for step in range(limit):
        result = run_step(uc, constants, trace, int(states[-1]["pc"]))
        if result.exception:
            raise RuntimeError(f"{product}: oracle exception at step {step}")
        if result.unmapped:
            raise RuntimeError(f"{product}: oracle unmapped access at step {step}")
        states.append({**_regs(uc, constants), "clock": step + 1})
    return states


def oracle_trace(product: str, syx: Path, limit: int = 1000) -> Path:
    """Write exactly ``limit`` trusted single-instruction oracle boundaries."""
    from unicorn import m68k_const as constants

    from emu import snapshot as emu_snapshot
    from tools.cf_lockstep import StepTrace, run_uc_step

    limit = _limit(limit)
    trusted = verify(product, syx)
    machine, _, _ = emu_snapshot.restore(str(trusted.path))
    image = ROOT / PRODUCTS[product].section_dir / "section_3_MAIN_OS.bin"
    machine.install_mmio()
    machine.install_isa_patches_scoped(image.read_bytes(), LOAD_ADDRESS)
    trace = StepTrace()
    trace.install(machine.uc)
    states = _trace_states(product, machine.uc, constants, trace, limit, run_uc_step)
    output = ROOT / "out/native/checkpoint-gate" / f"{product}-oracle-1k.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(
            {"format_version": 1, "product": product, "limit": limit, "states": states},
            separators=(",", ":"),
        )
    )
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


def diff(dt2_syx: Path, dn2_syx: Path, limit: int = 1000) -> None:
    """Provenance-gated Python-oracle/native differential run."""
    limit = _limit(limit)
    # Both source checks must complete before Cargo is allowed to start.
    dt2 = prepare("dt2", dt2_syx)
    dn2 = prepare("dn2", dn2_syx)
    dt2_trace = oracle_trace("dt2", dt2_syx, limit)
    dn2_trace = oracle_trace("dn2", dn2_syx, limit)
    env = os.environ | {
        "NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK": "unverified-local-inputs",
        "DT2_CHECKPOINT_MSTATE": str(dt2.resolve()),
        "DN2_CHECKPOINT_MSTATE": str(dn2.resolve()),
        "DT2_CHECKPOINT_TRACE": str(dt2_trace.resolve()),
        "DN2_CHECKPOINT_TRACE": str(dn2_trace.resolve()),
        "NATIVE_CHECKPOINT_DIFF_LIMIT": str(limit),
    }
    print("source-verified products=dt2,dn2", flush=True)
    subprocess.run(
        [
            "cargo",
            "test",
            "--release",
            "--test",
            "checkpoint_diff",
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
    diff_parser = commands.add_parser("diff")
    diff_parser.add_argument("--dt2-syx", type=Path, required=True)
    diff_parser.add_argument("--dn2-syx", type=Path, required=True)
    diff_parser.add_argument("--limit", type=int, default=1000)
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args.product, args.syx, args.snapshot)
    elif args.command == "gate":
        gate(args.dt2_syx, args.dn2_syx)
    else:
        diff(args.dt2_syx, args.dn2_syx, args.limit)


if __name__ == "__main__":
    main()
