#!/usr/bin/env python3
# pyright: reportMissingImports=false
"""Resolve MCF5441x addresses, vectors, and DMA channels from the contract."""

from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.hwref import (  # noqa: E402
    decode_register_value,
    resolve_address,
    resolve_dma_channel,
    resolve_vector,
)


def parse_int(value: str) -> int:
    return int(value, 0)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    sub = parser.add_subparsers(dest="command", required=True)

    address = sub.add_parser("address", help="resolve an address")
    address.add_argument("address", type=parse_int)
    address.add_argument("--value", type=parse_int, help="also decode a register value")

    vector = sub.add_parser("vector", help="resolve an exception vector")
    vector.add_argument("vector", type=parse_int)

    dma = sub.add_parser("dma", help="resolve an eDMA channel")
    dma.add_argument("channel", type=parse_int)
    return parser.parse_args(argv)


def lookup(args):
    if args.command == "address":
        result = resolve_address(args.address)
        if args.value is not None:
            result["decoded"] = decode_register_value(result, args.value)
        return result
    if args.command == "vector":
        return resolve_vector(args.vector)
    return resolve_dma_channel(args.channel)


def _hex_fields(value):
    if isinstance(value, dict):
        return {key: _hex_fields(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_hex_fields(item) for item in value]
    return value


def render(result):
    lines = []
    if result["kind"] == "vector":
        lines.append(
            f"vector {result['vector']} = {result.get('controller', '?')} "
            f"source {result.get('source', '?')}"
        )
        assignment = result.get("assignment")
        if assignment:
            lines.append(assignment["description"])
            lines.append(f"clear: {assignment['clear']}")
        lines.append(
            f"software force: {result.get('force_register')} bit "
            f"{result.get('force_bit')} @ {result.get('force_address', 0):#010x}"
        )
    elif result["kind"] == "dma_channel":
        if not result.get("valid", True):
            return f"invalid eDMA channel {result['channel']}: {result['error']}"
        lines.append(
            f"eDMA channel {result['channel']} TCD @ {result['tcd_address']:#010x}"
        )
        routing = result.get("routing")
        if routing:
            lines.append(f"request: {routing['request']} — {routing['request_description']}")
            lines.append(
                f"completion: {routing['completion_controller']} source "
                f"{routing['completion_source']} / vector {routing['completion_vector']}"
            )
    else:
        lines.append(f"{result['address']:#010x}: {result.get('name') or 'unknown'}")
        if result.get("memory_range"):
            region = result["memory_range"]
            lines.append(f"range: {region['name']} — {region['description']}")
        if result.get("decoded"):
            lines.append(f"decoded: {result['decoded']}")
    return "\n".join(lines)


def main(argv=None):
    args = parse_args(argv)
    result = lookup(args)
    if args.json:
        print(json.dumps(_hex_fields(result), indent=2, sort_keys=True))
    else:
        print(render(result))


if __name__ == "__main__":
    main()
