#!/usr/bin/env python3
"""Convert trusted local Unicorn checkpoints to portable MSTATE version 1.

This tool is deliberately the only pickle boundary: it loads local ``.snap``
files through emu.snapshot._load_blob and emits a non-pickle interchange file.
"""

import argparse
import json
import struct
import sys
import zlib
from pathlib import Path

# Direct `python tools/snapconv.py` puts tools/ rather than the repository
# root on sys.path. Keep the documented module and script entry points equal.
ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from emu.snapshot import _load_blob  # noqa: E402

MAGIC = b"MSTATE\x00\x01"
PAGE_SIZE = 1024 * 1024
MAX_HEADER_SIZE = 1024 * 1024
MAX_MAPPED_PAGES = 512
REG_NAMES = tuple(
    ["d%d" % index for index in range(8)]
    + ["a%d" % index for index in range(8)]
    + ["pc", "sr"]
)


def _address_map(value, name):
    """Normalize an emulator u32-address map for JSON without changing values."""
    if not isinstance(value, dict):
        raise ValueError("%s must be a dictionary" % name)
    normalized = {}
    for address, item in value.items():
        if type(address) is not int or not 0 <= address <= 0xFFFFFFFF:
            raise ValueError("%s address is not a u32" % name)
        if type(item) is not int or not 0 <= item <= 0xFFFFFFFF:
            raise ValueError("%s value at %#x is not a u32" % (name, address))
        normalized[str(address)] = item
    return normalized


def _json_value(value, name):
    """Reject non-JSON pickle values rather than giving them a new meaning."""
    if value is None or type(value) in (bool, float, str):
        return value
    if type(value) is int:
        if not -(1 << 63) <= value <= 0xFFFFFFFFFFFFFFFF:
            raise ValueError("%s integer is outside portable JSON bounds" % name)
        return value
    if isinstance(value, list):
        return [_json_value(item, name) for item in value]
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        return {key: _json_value(item, name) for key, item in value.items()}
    raise ValueError("%s contains a value that is not JSON-safe" % name)


def convert_blob(blob, clock=0):
    """Return portable version-1 bytes for a blob already trusted by _load_blob."""
    if type(clock) is not int or not 0 <= clock <= 0xFFFFFFFFFFFFFFFF:
        raise ValueError("clock must be a u64")
    mapped = blob["all_mapped"]
    if len(mapped) > MAX_MAPPED_PAGES:
        raise ValueError("mapped page count exceeds portable limit")
    mapped = sorted(mapped)
    pages = blob["pages"]
    nonzero = []
    for base in mapped:
        compressed = pages.get(base)
        if compressed is None:
            continue
        try:
            page = zlib.decompress(compressed)
        except zlib.error as exc:
            raise ValueError("invalid source page %#x" % base) from exc
        if len(page) != PAGE_SIZE:
            raise ValueError("invalid source page length %#x" % base)
        if page.strip(b"\0"):
            nonzero.append((base, page))
    if len(nonzero) > MAX_MAPPED_PAGES:
        raise ValueError("nonzero page count exceeds portable limit")
    # ``extra`` remains outside the frozen MSTATE header, but must still be
    # portable before a trusted checkpoint crosses this interchange seam.
    _json_value(blob.get("extra", {}), "extra")
    header = {
        "clock": clock,
        "clock_basis": "checkpoint_relative_zero",
        "components": _json_value(blob.get("components", {}), "components"),
        "ctlregs": _address_map(blob["ctlregs"], "ctlregs"),
        "ff1_count": blob["ff1_count"],
        "format_version": 1,
        "manifest": _json_value(blob.get("manifest"), "manifest"),
        "mapped_bases": mapped,
        "mmio_forced": _address_map(blob["mmio"], "mmio"),
        "movec_count": blob["movec_count"],
        "page_count": len(nonzero),
        "regs": {name: blob["regs"][name] for name in REG_NAMES},
    }
    encoded_header = json.dumps(
        header,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")
    if len(encoded_header) > MAX_HEADER_SIZE:
        raise ValueError("portable header exceeds limit")
    output = bytearray(MAGIC + struct.pack("<I", len(encoded_header)) + encoded_header)
    for base, page in nonzero:
        compressed = zlib.compress(page, 6)
        output.extend(struct.pack("<III", base, PAGE_SIZE, len(compressed)))
        output.extend(compressed)
    return bytes(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="trusted local .snap input")
    parser.add_argument("output", type=Path, help="portable MSTATE output")
    parser.add_argument(
        "--clock", type=int, default=0, help="checkpoint-relative clock"
    )
    args = parser.parse_args()
    args.output.write_bytes(convert_blob(_load_blob(args.source), args.clock))


if __name__ == "__main__":
    main()
