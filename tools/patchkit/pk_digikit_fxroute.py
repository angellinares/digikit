#!/usr/bin/env python3
"""Offline project-byte and card-write oracles for the TypeScript track SEND suite.

Device execution lives in modules/core/adapters/fxpages/tests/fx-track-route.device.test.ts.
"""

from __future__ import annotations

import argparse
import json
import tempfile
from pathlib import Path
from typing import Any

from tools.emulator import card_overlay
from tools.patchkit import pk_digikit_project as P

CLASSES = {
    "dt2": 0x4293C040,
    "dn2": 0x4243425C,
}  # the second per-code table (value-class objects of 0x44 bytes; the accessor of core/params' anchor cf.param.fn.value_class)


def lock_check(
    device: str,
    card: Path,
    base: Path,
    none: Path,
    amount: Path,
    send: Path,
) -> dict[str, Any]:
    """The legacy lock comparisons on complete base-card images with each overlay applied."""
    overlays = {"base": base, "none": none, "amount": amount, "send": send}
    touched: set[int] = set()
    for overlay in overlays.values():
        touched.update(P.slot_sectors(overlay))
    with tempfile.TemporaryDirectory(prefix="fxroute-lock-", dir=card.parent) as work:
        images = {}
        for name, overlay in overlays.items():
            after = Path(work) / f"{name}.img"
            card_overlay.apply(card, overlay, after)
            images[name] = P.slot_image(after, touched)
    return {
        "trig_changed_bytes": len(P.pattern0_changes(images["base"], images["none"])),
        "send_changed_bytes": len(P.pattern0_changes(images["none"], images["send"])),
        "amount_diff": P.diff_slot(images["send"], images["amount"], device, P.STAMP),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Track FX routing offline comparison helpers"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    lock = commands.add_parser("lock-check")
    lock.add_argument("--device", choices=("dt2", "dn2"), required=True)
    lock.add_argument("--card", type=Path, required=True)
    for name in ("base", "none", "amount", "send"):
        lock.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        result = lock_check(
            args.device, args.card, args.base, args.none, args.amount, args.send
        )
    except (OSError, ValueError) as exc:
        parser.exit(1, f"error: {exc}\n")
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
