#!/usr/bin/env python3
"""Anchor and advance local checkpoints without trusting an old late snapshot.

An anchor checks the *entire* loaded MAIN OS of a trusted 24M snapshot against
the pinned local source. Each child is captured by a bounded, recorded
``mmio_record`` run and links the parent's snapshot and ledger hashes to its
own snapshot and trace hashes. These are local *integrity* receipts, not
authentication of arbitrary files or proof that the emulator models the
hardware correctly. Capture requires trusted local inputs. All outputs stay under the
ignored ``out/native/checkpoint-chain`` directory; neither snapshots nor
traces are distributed or committed.

``capture --limit N`` requests at least N guest instructions. The existing
timer stepper finishes the last scheduler interval: an active timer interval
has no universal 1M-instruction cap. Each process has a 120-second wall-clock
timeout, and the receipt records the *actual* instruction count; the requested
limit is a floor, not a strict guest-instruction ceiling.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from emu import mmiotrace  # noqa: E402
from tools import checkpointprep, snapconv, snapeq  # noqa: E402
from tools.snapread import Snapshot  # noqa: E402

MAX_LIMIT = 10_000_000
CAPTURE_TIMEOUT_S = 120


def sha256(path: Path) -> str:
    """Hash a local artifact without loading a large snapshot into memory."""
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1 << 20):
            digest.update(block)
    return digest.hexdigest()


def _directory() -> Path:
    return ROOT / "out/native/checkpoint-chain"


def _under_chain(path: Path) -> Path:
    resolved = path.resolve()
    if not resolved.is_relative_to(_directory().resolve()):
        raise ValueError("ledger and derived artifacts must stay under ignored out/")
    return resolved


def _sources(product: str, syx: Path) -> dict[str, str]:
    if product not in checkpointprep.PRODUCTS:
        raise ValueError("unknown checkpoint product")
    config = checkpointprep.PRODUCTS[product]
    checkpointprep.require_hash(syx, config.source_sha256, "source")
    section = ROOT / config.section_dir
    if (section / ".source-sha256").read_text().strip() != config.source_sha256:
        raise ValueError("section source SHA-256 mismatch")
    checkpointprep.require_hash(
        section / "section_3_MAIN_OS.bin", config.image_sha256, "MAIN OS"
    )
    return {"source_sha256": config.source_sha256, "image_sha256": config.image_sha256}


def _write_receipt(path: Path, receipt: dict) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt, sort_keys=True, separators=(",", ":")))
    temporary.replace(path)


def _same_json(left: object, right: object) -> bool:
    """Compare the trace header with pickle metadata after JSON tuple conversion."""
    return json.dumps(left, sort_keys=True) == json.dumps(right, sort_keys=True)


def _check_derived_image(
    product: str, snapshot: Snapshot, control: Path | None = None
) -> None:
    """Require original code or an equal-clock no-record control replay.

    Changed code must be corroborated by a second run from the same verified
    private input. This is still local reproducibility, not authentication.
    """
    image = (
        ROOT / checkpointprep.PRODUCTS[product].section_dir / "section_3_MAIN_OS.bin"
    )
    unchanged = (
        snapshot.read(checkpointprep.LOAD_ADDRESS, image.stat().st_size)
        == image.read_bytes()
    )
    if control is None and not unchanged:
        raise ValueError("derived loaded MAIN OS mismatch")
    if control is not None:
        if snapeq.compare(snapshot.path, str(control)) is not None:
            raise ValueError("recorded/control mismatch")
        other = Snapshot(str(control))
        recorded_done = snapshot._blob.get("extra", {}).get("mmio_record_done")
        control_done = other._blob.get("extra", {}).get("mmio_record_done")
        if type(recorded_done) is not int or recorded_done != control_done:
            raise ValueError("recorded/control completion count mismatch")
        if not _same_json(
            snapshot._blob.get("components"), other._blob.get("components")
        ):
            raise ValueError("recorded/control components mismatch")
        if not _same_json(snapshot._blob.get("manifest"), other._blob.get("manifest")):
            raise ValueError("recorded/control manifest mismatch")


def anchor(product: str, syx: Path, snapshot: Path | None = None) -> Path:
    """Create a local root receipt only after full loaded-image verification."""
    source = _sources(product, syx)
    snapshot = (snapshot or ROOT / checkpointprep.PRODUCTS[product].snapshot).resolve()
    # An arbitrary snapshot whose code happens to match is not the trusted
    # fixture; the early checkpoint is the only root this workflow permits.
    expected = (ROOT / checkpointprep.PRODUCTS[product].snapshot).resolve()
    if snapshot != expected:
        raise ValueError("anchor must use the product's trusted early snapshot")
    checkpointprep.verify(product, syx, snapshot)
    snapshot_hash = sha256(snapshot)
    base = _directory()
    base.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix=f"{product}-anchor-", dir=base))
    ledger = directory / "chain.json"
    _write_receipt(
        ledger,
        {
            "version": 1,
            "kind": "anchor",
            "product": product,
            **source,
            "snapshot": str(snapshot),
            "snapshot_sha256": snapshot_hash,
        },
    )
    return ledger


def _validate_derived_payload(entry: dict, snapshot: Path, trace: Path) -> None:
    restored = Snapshot(str(snapshot))
    control = None
    if "control_sha256" in entry:
        control = trace.parent / "control.snap"
        checkpointprep.require_hash(
            control, entry["control_sha256"], "control snapshot"
        )
    _check_derived_image(entry["product"], restored, control)
    saved = restored._blob
    reader = mmiotrace.Reader(str(trace))
    header = reader.header
    if (
        header.get("syx", {}).get("sha256") != entry["source_sha256"]
        or header.get("main_image", {}).get("sha256") != entry["image_sha256"]
        or header.get("snapshot", {}).get("sha256") != entry["input_snapshot_sha256"]
        or header.get("instrs") != entry["limit"]
        or not _same_json(header.get("manifest"), saved.get("manifest"))
        or bool(header.get("build", {}).get("slc")) != (entry["product"] == "dn2")
    ):
        raise ValueError("capture header/source manifest mismatch")
    ends = list(reader.blobs(mmiotrace.END))
    if len(ends) != 1:
        raise ValueError("capture END record missing or repeated")
    summary = ends[0][1]
    done = entry["done"]
    if (
        summary.get("stop") != "limit"
        or summary.get("done") != done
        or type(done) is not int
        or done < entry["limit"]
        or summary.get("errors") != 0
        or saved.get("extra", {}).get("mmio_record_done") != done
    ):
        raise ValueError("capture did not finish its bounded window")


def verify(ledger: Path, syx: Path, *, _depth: int = 0) -> dict:
    """Recheck a trusted local chain's integrity, not its authenticity."""
    if _depth > 16:
        raise ValueError("checkpoint chain is too deep")
    ledger = _under_chain(ledger)
    try:
        entry = json.loads(ledger.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError("invalid checkpoint chain receipt") from exc
    if entry.get("version") != 1 or entry.get("kind") not in ("anchor", "derived"):
        raise ValueError("unsupported checkpoint chain receipt")
    source = _sources(entry["product"], syx)
    if any(entry.get(name) != value for name, value in source.items()):
        raise ValueError("source/image SHA-256 mismatch")
    snapshot = (
        Path(entry["snapshot"]).resolve()
        if entry["kind"] == "anchor"
        else ledger.parent / "final.snap"
    )
    if entry["kind"] == "derived":
        snapshot = _under_chain(snapshot)
        parent_path = _under_chain(Path(entry["parent"]))
        checkpointprep.require_hash(
            parent_path, entry["parent_sha256"], "parent ledger"
        )
        parent = verify(parent_path, syx, _depth=_depth + 1)
        if parent["product"] != entry["product"] or parent[
            "snapshot_sha256"
        ] != entry.get("input_snapshot_sha256"):
            raise ValueError("parent snapshot SHA-256 mismatch")
        trace = _under_chain(ledger.parent / "capture.mmio")
        checkpointprep.require_hash(trace, entry["trace_sha256"], "trace")
        if (
            type(entry.get("limit")) is not int
            or not 1 <= entry["limit"] <= MAX_LIMIT
            or type(entry.get("done")) is not int
        ):
            raise ValueError("invalid bounded capture count")
    checkpointprep.require_hash(snapshot, entry["snapshot_sha256"], "snapshot")
    if entry["kind"] == "anchor":
        if (
            snapshot
            != (ROOT / checkpointprep.PRODUCTS[entry["product"]].snapshot).resolve()
        ):
            raise ValueError("anchor must use the trusted early snapshot")
        checkpointprep.verify(entry["product"], syx, snapshot)
    else:
        _validate_derived_payload(
            entry, snapshot, _under_chain(ledger.parent / "capture.mmio")
        )
    return entry


def capture(
    parent_ledger: Path, syx: Path, limit: int, *, control: bool = False
) -> Path:
    """Advance a source-verified parent via the existing bounded recorder."""
    if type(limit) is not int or not 1 <= limit <= MAX_LIMIT:
        raise ValueError(f"limit must be between 1 and {MAX_LIMIT}")
    parent = verify(parent_ledger, syx)
    product = parent["product"]
    input_snapshot = (
        Path(parent["snapshot"])
        if parent["kind"] == "anchor"
        else _under_chain(Path(parent_ledger).parent / "final.snap")
    )
    directory = Path(tempfile.mkdtemp(prefix=f"{product}-run-", dir=_directory()))
    trace, final = directory / "capture.mmio", directory / "final.snap"
    # Pass the recorder the verified bytes, not a mutable path that another
    # process might replace between this check and its later restore().
    private_input = directory / "input.snap"
    shutil.copyfile(input_snapshot, private_input)
    checkpointprep.require_hash(
        private_input, parent["snapshot_sha256"], "private input snapshot"
    )
    args = [
        sys.executable,
        str(ROOT / "tools/mmio_record.py"),
        "record",
        str(private_input),
        "--syx",
        str(syx),
        "--sections",
        str(ROOT / checkpointprep.PRODUCTS[product].section_dir),
        "--instrs",
        str(limit),
        "--out",
        str(trace),
        "--save-final",
        str(final),
    ]
    if product == "dn2":
        args.append("--slc")
    subprocess.run(args, cwd=ROOT, check=True, timeout=CAPTURE_TIMEOUT_S)
    control_file = directory / "control.snap"
    if control:
        control_args = list(args)
        control_args[control_args.index("--save-final") + 1] = str(control_file)
        control_args.append("--no-record")
        subprocess.run(control_args, cwd=ROOT, check=True, timeout=CAPTURE_TIMEOUT_S)
    # Do not certify a run against an input or local source changed while it ran.
    verify(parent_ledger, syx)
    reader = mmiotrace.Reader(str(trace))
    ends = list(reader.blobs(mmiotrace.END))
    if len(ends) != 1 or type(ends[0][1].get("done")) is not int:
        raise ValueError("capture has no completed END count")
    ledger = directory / "chain.json"
    entry = {
        "version": 1,
        "kind": "derived",
        "product": product,
        "source_sha256": parent["source_sha256"],
        "image_sha256": parent["image_sha256"],
        "parent": str(Path(parent_ledger).resolve()),
        "parent_sha256": sha256(Path(parent_ledger)),
        "input_snapshot_sha256": parent["snapshot_sha256"],
        "snapshot_sha256": sha256(final),
        "trace_sha256": sha256(trace),
        "limit": limit,
        "done": ends[0][1]["done"],
    }
    if control:
        entry["control_sha256"] = sha256(control_file)
    # Validate before publishing the receipt. An interrupted/invalid run may
    # leave ignored scratch files, but they have no usable chain.json.
    _validate_derived_payload(entry, final, trace)
    _write_receipt(ledger, entry)
    return ledger


def portable(ledger: Path, syx: Path) -> Path:
    """Convert one integrity-checked local snapshot; never parse pickle in Rust."""
    entry = verify(ledger, syx)
    path = (
        Path(entry["snapshot"])
        if entry["kind"] == "anchor"
        else _under_chain(ledger.parent / "final.snap")
    )
    # The source path is mutable even after verify(). Decode only a private
    # copy whose hash has been checked against the receipt.
    with tempfile.TemporaryDirectory(prefix="portable-", dir=_directory()) as scratch:
        private = Path(scratch) / "input.snap"
        shutil.copyfile(path, private)
        checkpointprep.require_hash(
            private, entry["snapshot_sha256"], "private portable input snapshot"
        )
        data = snapconv.convert_blob(Snapshot(str(private))._blob)
    output = _under_chain(ledger.parent / "portable.mstate")
    temporary = output.with_suffix(".mstate.tmp")
    temporary.write_bytes(data)
    temporary.replace(output)
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    start = commands.add_parser("anchor")
    start.add_argument("product", choices=checkpointprep.PRODUCTS)
    start.add_argument("--syx", type=Path, required=True)
    follow = commands.add_parser("capture")
    follow.add_argument("parent", type=Path)
    follow.add_argument("--syx", type=Path, required=True)
    follow.add_argument("--limit", type=int, required=True)
    follow.add_argument(
        "--control",
        action="store_true",
        help="require a matching no-record run for changed MAIN OS and host state",
    )
    check = commands.add_parser("verify")
    check.add_argument("ledger", type=Path)
    check.add_argument("--syx", type=Path, required=True)
    convert = commands.add_parser("portable")
    convert.add_argument("ledger", type=Path)
    convert.add_argument("--syx", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "anchor":
        result = anchor(args.product, args.syx)
    elif args.command == "capture":
        result = capture(args.parent, args.syx, args.limit, control=args.control)
    elif args.command == "portable":
        result = portable(args.ledger, args.syx)
    else:
        verify(args.ledger, args.syx)
        result = args.ledger
    label = {
        "anchor": "local integrity checked",
        "capture": "local capture recorded",
        "portable": "local portable state generated",
        "verify": "local integrity checked",
    }[args.command]
    print(f"checkpoint-chain {label}: {result}")


if __name__ == "__main__":
    main()
