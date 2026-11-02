#!/usr/bin/env python3
"""Offline BASIC FX peak oracle.

Device acceptance lives in modules/core/adapters/dynfx/tests/fx-chain.device.test.ts.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Existing BASIC FX acceptance oracles")
    sub = parser.add_subparsers(dest="command", required=True)
    peak = sub.add_parser("pcm-peak")
    peak.add_argument("wav", type=Path)
    args = parser.parse_args(argv)
    from tools.patchkit.hook_ab import read_wav

    print(json.dumps({"peak": float(np.abs(read_wav(args.wav)).max())}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
