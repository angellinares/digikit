#!/usr/bin/env python3
"""Device facts, pristine-card settings seeds and OLED rendering for FX route suites.

Device execution lives in modules/core/adapters/fx/tests/fx-routes-panel.device.test.ts,
fx-routes-persist.device.test.ts and modules/core/adapters/fxpages/tests/fx-track-route.device.test.ts.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
from pathlib import Path
from typing import Any

from tools.coldfire import arena_safety
from tools.control.digi_client import oled_png
from tools.device import dksettings
from tools.device.dksettings import encode as encode_record
from tools.patchkit import pk_device
from tools.patchkit import pk_dkrt_machine as M
from tools.patchkit.pk_digikit_fxroute import CLASSES


def profile(device: str) -> dict[str, Any]:
    """Device facts used by the FX routing acceptance scenarios (sectors, hi exclusive)."""
    limits = pk_device.limits(device)
    store = pk_device.store_of(device)
    return {
        "track_type_word": M.TRACK_OBJ_WORD,
        "value_classes": CLASSES[device],
        "selection": {"copy_a": store.sel_a, "copy_b": store.sel_b},
        "fx_slots": limits.fx_slots,
        "send_slots": limits.send_slots,
        "settings": {"copy_a": store.set_a, "copy_b": store.set_b},
        "kit_areas": arena_safety.KIT_AREAS[device],
        "fx": pk_device.load(device)["fx"],
    }


def seed_card(card: Path, device: str, entries: Any) -> dict[str, Any]:
    """Seed copy A on an existing caller-owned card with empty settings copies."""
    if not isinstance(entries, list):
        raise ValueError("entries must be a JSON array of {key, value_hex} objects")
    pairs: list[tuple[bytes, bytes]] = []
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"key", "value_hex"}
            or not isinstance(entry["key"], str)
            or not isinstance(entry["value_hex"], str)
        ):
            raise ValueError("each entry must contain string key and value_hex fields")
        try:
            key = entry["key"].encode("ascii")
            value = bytes.fromhex(entry["value_hex"])
        except (UnicodeError, ValueError) as exc:
            raise ValueError("entry needs an ASCII key and hexadecimal value") from exc
        if not dksettings.valid_key(key) or len(value) > dksettings.VALUE_MAX:
            raise ValueError("entry exceeds settings key or value limits")
        pairs.append((key, value))
    record = encode_record(1, pairs)
    reason = dksettings.why(record)
    if reason != "ok":
        raise ValueError(f"invalid settings record: {reason}")
    padded = record.ljust(((len(record) + 511) // 512) * 512, b"\0")
    store = pk_device.store_of(device)
    if len(padded) > store.set_copy_sectors * 512:
        raise ValueError("settings record exceeds the device's copy area")
    with card.open("r+b") as file:
        file.seek(0, 2)
        if file.tell() < (store.set_b + store.set_copy_sectors) * 512:
            raise ValueError("card is too small for the device settings copies")
        for sector in (store.set_a, store.set_b):
            file.seek(sector * 512)
            if any(file.read(store.set_copy_sectors * 512)):
                raise ValueError("card settings copies must be empty before seeding")
        file.seek(store.set_a * 512)
        file.write(padded)
    return {
        "card": str(card),
        "sector": store.set_a,
        "bytes": len(padded),
        "seq": 1,
        "entries": len(pairs),
    }


def render_shots(directory: Path) -> dict[str, Any]:
    """Render saved raw OLED frames, copying diagnostics as the legacy Shots helper does."""
    if not directory.is_dir():
        raise ValueError(f"not a shots directory: {directory}")
    frames = [(path, path.read_bytes()) for path in sorted(directory.glob("*.oled"))]
    for path, raw in frames:
        if len(raw) != 1024:
            raise ValueError(f"{path}: expected 1024 OLED bytes, got {len(raw)}")
    copy_dir = Path(out) if (out := os.environ.get("DIGIKIT_SHOTS")) else None
    if copy_dir is not None:
        copy_dir.mkdir(parents=True, exist_ok=True)
    files = []
    for path, raw in frames:
        destination = path.with_suffix(".png")
        destination.write_bytes(oled_png(raw, 3))
        if copy_dir is not None:
            copied = copy_dir / destination.name
            if copied.resolve() != destination.resolve():
                shutil.copy(destination, copied)
        files.append(str(destination))
    return {"files": files, "count": len(files)}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="FX routing scenario data helpers")
    commands = parser.add_subparsers(dest="command", required=True)
    facts = commands.add_parser("profile")
    facts.add_argument("--device", choices=("dt2", "dn2"), required=True)
    seed = commands.add_parser("seed-card")
    seed.add_argument("card", type=Path)
    seed.add_argument("--device", choices=("dt2", "dn2"), required=True)
    seed.add_argument("--entries", type=Path, required=True)
    shots = commands.add_parser("render-shots")
    shots.add_argument("directory", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "profile":
            result = profile(args.device)
        elif args.command == "render-shots":
            result = render_shots(args.directory)
        else:
            result = seed_card(
                args.card, args.device, json.loads(args.entries.read_text())
            )
    except (OSError, ValueError) as exc:
        parser.exit(1, f"error: {exc}\n")
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
