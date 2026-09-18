"""Curated MCF5441x address, interrupt, and DMA lookup helpers.

The JSON contract is documentation-first: every nontrivial fact retains its
source.  This module gives emulator and tooling code one small, read-only API
instead of duplicating address arithmetic or parsing the manual at runtime.
"""

from __future__ import annotations

from functools import lru_cache
import json
from pathlib import Path
from typing import Any


CONTRACT_PATH = (
    Path(__file__).resolve().parent.parent
    / "docs"
    / "contracts"
    / "mcf5441x-reference-v1.json"
)


def as_int(value: int | str) -> int:
    return int(value, 0) if isinstance(value, str) else value


@lru_cache(maxsize=1)
def load_reference() -> dict[str, Any]:
    with CONTRACT_PATH.open(encoding="utf-8") as stream:
        return json.load(stream)


def _memory_range(address: int, reference: dict[str, Any]) -> dict[str, Any] | None:
    for region in reference["memory_map"]:
        if as_int(region["start"]) <= address <= as_int(region["end"]):
            return region
    return None


def resolve_vector(vector: int, reference: dict[str, Any] | None = None) -> dict[str, Any]:
    reference = reference or load_reference()
    controller = next(
        (
            item
            for item in reference["intc"]["controllers"]
            if item["vector_base"] <= vector < item["vector_base"] + 64
        ),
        None,
    )
    if controller is None:
        return {"kind": "vector", "vector": vector, "known": False}

    source = vector - controller["vector_base"]
    assignment = next(
        (
            item
            for item in reference["intc"]["sources"]
            if item["controller"] == controller["name"] and item["source"] == source
        ),
        None,
    )
    force_name = "INTFRCL" if source < 32 else "INTFRCH"
    force_offset = 0x14 if source < 32 else 0x10
    result: dict[str, Any] = {
        "kind": "vector",
        "known": True,
        "vector": vector,
        "controller": controller["name"],
        "source": source,
        "icr_address": as_int(controller["base"]) + as_int(controller["icr0_offset"]) + source,
        "force_register": f"{controller['name']}.{force_name}",
        "force_address": as_int(controller["base"]) + force_offset,
        "force_bit": source & 31,
    }
    if assignment is not None:
        result["assignment"] = assignment
    return result


def resolve_dma_channel(
    channel: int, reference: dict[str, Any] | None = None
) -> dict[str, Any]:
    reference = reference or load_reference()
    if not 0 <= channel < 64:
        return {
            "kind": "dma_channel",
            "known": False,
            "valid": False,
            "channel": channel,
            "error": "eDMA channel must be in the range 0..63",
        }
    edma = reference["edma"]
    routing = next(
        (item for item in edma["channels"] if item["channel"] == channel), None
    )
    result: dict[str, Any] = {
        "kind": "dma_channel",
        "known": routing is not None,
        "valid": True,
        "channel": channel,
        "tcd_address": as_int(edma["tcd_base"]) + channel * as_int(edma["tcd_stride"]),
    }
    if routing is not None:
        result["routing"] = routing
        result["completion"] = resolve_vector(routing["completion_vector"], reference)
    return result


def _resolve_tcd(address: int, reference: dict[str, Any]) -> dict[str, Any] | None:
    edma = reference["edma"]
    base = as_int(edma["tcd_base"])
    stride = as_int(edma["tcd_stride"])
    if not base <= address < base + 64 * stride:
        return None
    channel, field_offset = divmod(address - base, stride)
    for name, raw_offset in edma["tcd_offsets"].items():
        offset = as_int(raw_offset)
        width = edma["tcd_widths"][name]
        if offset <= field_offset < offset + width:
            return {
                "kind": "register",
                "name": f"EDMA.TCD{channel}.{name}",
                "block": "EDMA",
                "channel": channel,
                "field": name,
                "field_address": base + channel * stride + offset,
                "field_offset": field_offset - offset,
                "width": width,
                "access": "rw",
            }
    return {
        "kind": "register_block",
        "name": f"EDMA.TCD{channel}",
        "block": "EDMA",
        "channel": channel,
        "offset": field_offset,
    }


def _resolve_intc(address: int, reference: dict[str, Any]) -> dict[str, Any] | None:
    intc = reference["intc"]
    for controller in intc["controllers"]:
        base = as_int(controller["base"])
        if not base <= address < base + 0x4000:
            continue
        offset = address - base
        for register in intc["registers"]:
            reg_offset = as_int(register["offset"])
            if reg_offset <= offset < reg_offset + register["width"]:
                return {
                    **register,
                    "kind": "register",
                    "name": f"{controller['name']}.{register['name']}",
                    "block": controller["name"],
                    "field_address": base + reg_offset,
                    "field_offset": offset - reg_offset,
                }
        icr0 = as_int(controller["icr0_offset"])
        if icr0 <= offset < icr0 + 64:
            source = offset - icr0
            return {
                "kind": "register",
                "name": f"{controller['name']}.ICR{source}",
                "block": controller["name"],
                "source": source,
                "vector": controller["vector_base"] + source,
                "field_address": address,
                "width": 1,
                "access": "rw",
            }
        return {
            "kind": "register_block",
            "name": controller["name"],
            "block": controller["name"],
            "offset": offset,
        }
    return None


def _resolve_edma_register(
    address: int, reference: dict[str, Any]
) -> dict[str, Any] | None:
    edma = reference["edma"]
    base = as_int(edma["base"])
    for register in edma["registers"]:
        offset = as_int(register["offset"])
        if base + offset <= address < base + offset + register["width"]:
            return {
                **register,
                "kind": "register",
                "name": f"EDMA.{register['name']}",
                "block": "EDMA",
                "field_address": base + offset,
                "field_offset": address - (base + offset),
            }
    if base <= address < base + 0x4000:
        return {
            "kind": "register_block",
            "name": "EDMA",
            "block": "EDMA",
            "offset": address - base,
        }
    return None


def _resolve_register_block(
    address: int, reference: dict[str, Any]
) -> dict[str, Any] | None:
    for block in reference["register_blocks"]:
        base = as_int(block["base"])
        size = as_int(block["size"])
        if not base <= address < base + size:
            continue
        offset = address - base
        for register in block["registers"]:
            reg_offset = as_int(register["offset"])
            if reg_offset <= offset < reg_offset + register["width"]:
                return {
                    **register,
                    "kind": "register",
                    "name": f"{block['name']}.{register['name']}",
                    "block": block["name"],
                    "field_address": base + reg_offset,
                    "field_offset": offset - reg_offset,
                }
        return {
            "kind": "register_block",
            "name": block["name"],
            "block": block["name"],
            "offset": offset,
        }
    return None


def resolve_address(
    address: int, reference: dict[str, Any] | None = None
) -> dict[str, Any]:
    reference = reference or load_reference()
    result = (
        _resolve_tcd(address, reference)
        or _resolve_intc(address, reference)
        or _resolve_edma_register(address, reference)
        or _resolve_register_block(address, reference)
        or {"kind": "address", "name": None}
    )
    result = {"address": address, **result}
    region = _memory_range(address, reference)
    if region is not None:
        result["memory_range"] = region
    return result


def decode_register_value(
    resolved: dict[str, Any], value: int, reference: dict[str, Any] | None = None
) -> dict[str, Any]:
    reference = reference or load_reference()
    decoded: dict[str, Any] = {"value": value}
    if resolved.get("block") == "EDMA" and resolved.get("field") == "CSR":
        decoded["set_fields"] = [
            field["name"]
            for field in reference["edma"]["csr_fields"]
            if value & (1 << field["bit"])
        ]
    elif resolved.get("name", "").endswith(".INTFRCH"):
        decoded["forced_sources"] = [32 + bit for bit in range(32) if value & (1 << bit)]
    elif resolved.get("name", "").endswith(".INTFRCL"):
        decoded["forced_sources"] = [bit for bit in range(32) if value & (1 << bit)]
    return decoded
