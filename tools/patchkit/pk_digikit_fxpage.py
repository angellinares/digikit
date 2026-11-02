#!/usr/bin/env python3
"""Offline PCM comparisons for FUNC + FX pages and kit persistence acceptance.

Device acceptance lives in modules/core/adapters/fxpages/tests/fx-pages.device.test.ts and fx-kit-persist.device.test.ts.
"""

from __future__ import annotations

from pathlib import Path

from tools.patchkit.hook_ab import compare_aligned as scaled


def main(argv: list[str] | None = None) -> int:
    """Expose the existing PCM oracle without imposing the generic CLI's 2-ULP gate."""
    import argparse
    import json

    from tools.patchkit.hook_ab import read_wav

    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    compare = sub.add_parser("compare")
    compare.add_argument("base", type=Path)
    compare.add_argument("sample", type=Path)
    compare.add_argument("--gain", type=float, required=True)
    args = parser.parse_args(argv)
    print(
        json.dumps(
            scaled(read_wav(args.base), read_wav(args.sample), args.gain),
            allow_nan=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
