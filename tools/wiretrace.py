"""Compare bounded, wire-order DSPI2 TX traces; do not render SHARC audio.

DTFR v1 is the format emitted by tools/live_gui_check.py --frames-out:
``DTFR``, little-endian version/count, then length-prefixed TX buffers.
The comparison is an Oracle gate only when both producers start from the
same state and receive input at the same guest instruction clocks.
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

MAX_FRAMES = 256
MAX_FRAME_BYTES = 4096
MAX_FILE_BYTES = 12 + MAX_FRAMES * (4 + MAX_FRAME_BYTES)


def read(path: Path) -> list[bytes]:
    """Reject malformed, unbounded, or trailing data instead of guessing."""
    if path.stat().st_size > MAX_FILE_BYTES:
        raise ValueError(f"{path}: wire trace exceeds bounded size")
    blob = path.read_bytes()
    if len(blob) < 12 or blob[:4] != b"DTFR":
        raise ValueError(f"{path}: not a DTFR v1 wire trace")
    version, count = struct.unpack_from("<II", blob, 4)
    if version != 1 or not 1 <= count <= MAX_FRAMES:
        raise ValueError(f"{path}: unsupported version or frame count")
    offset = 12
    frames = []
    for index in range(count):
        if offset + 4 > len(blob):
            raise ValueError(f"{path}: missing length for frame {index}")
        length = struct.unpack_from("<I", blob, offset)[0]
        offset += 4
        if length > MAX_FRAME_BYTES or offset + length > len(blob):
            raise ValueError(f"{path}: invalid length for frame {index}")
        frames.append(blob[offset : offset + length])
        offset += length
    if offset != len(blob):
        raise ValueError(f"{path}: trailing bytes after frame {count - 1}")
    return frames


def first_mismatch(
    expected: list[bytes], actual: list[bytes], prefix: int | None = None
) -> str | None:
    """Return the first ordered difference, or None for a matched prefix/full trace."""
    if prefix is not None and not 1 <= prefix <= len(expected):
        raise ValueError("prefix must be between 1 and the expected frame count")
    count = len(expected) if prefix is None else prefix
    for index in range(min(count, len(actual))):
        a, b = expected[index], actual[index]
        if len(a) != len(b):
            return f"frame {index}: expected {len(a)} bytes, got {len(b)}"
        for offset, (want, got) in enumerate(zip(a, b, strict=True)):
            if want != got:
                return f"frame {index} byte {offset:#x}: expected {want:#04x}, got {got:#04x}"
    if len(actual) < count:
        return f"frame {len(actual)}: missing; expected at least {count} frames"
    if prefix is None and len(actual) != count:
        return f"frame {count}: extra; expected {count} frames, got {len(actual)}"
    return None


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "expected", type=Path, help="Python ColdFire DTFR under ignored out/"
    )
    p.add_argument("actual", type=Path, help="native ColdFire DTFR under ignored out/")
    p.add_argument("--prefix", type=int, help="compare only the first N ordered frames")
    a = p.parse_args(argv)
    try:
        expected, actual = read(a.expected), read(a.actual)
        mismatch = first_mismatch(expected, actual, a.prefix)
    except (OSError, ValueError) as exc:
        p.error(str(exc))
    if mismatch is not None:
        print(mismatch)
        return 1
    count = a.prefix if a.prefix is not None else len(expected)
    print(
        f"matched {count} ordered wire frames ({'prefix' if a.prefix else 'full trace'})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
