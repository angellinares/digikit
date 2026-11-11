#!/usr/bin/env python3
"""The settings record of the digikit runtime, read on the host (docs/design/digikit-store.md, "The settings record").

    uv run python -m tools.device.dksettings FILE            # decode one record file (what `dkupload settings -o FILE` saves)
    uv run python -m tools.device.dksettings FILE --json

The same rules as `dkrt_sdk::settings` (the runtime and `patchkit store dump`); `modules/core/runtime/vectors/settings-vectors.json` keeps the three equal (tests/digikit/test_dksettings.py).
A record is `"DKST" | u16 version = 1 | u32 seq | u32 used | u32 crc32 | entries`, big-endian; the crc32 (zlib's) covers bytes 0..14 and 18..used; an entry is
`u8 key_len | key | u16 value_len | value`. The limits and the keys by owner come from `sdk/sdk.toml [settings]` (`tools/device/dkproto.py`).
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tools.device.dkproto import (
    SETTINGS_COPIES,
    SETTINGS_KEY_MAX,
    SETTINGS_KEYS,
    SETTINGS_SECTORS,
    SETTINGS_VALUE_MAX,
)

MAGIC = b"DKST"
VERSION = 1
HEADER = 18
AREA = SETTINGS_SECTORS * 512  # bytes of one copy
KEY_MAX = SETTINGS_KEY_MAX
VALUE_MAX = SETTINGS_VALUE_MAX
COPIES = SETTINGS_COPIES


def _crc(b: bytes, used: int) -> int:
    return zlib.crc32(b[:14] + b[HEADER:used]) & 0xFFFFFFFF


def valid_key(key: bytes) -> bool:
    return 1 <= len(key) <= KEY_MAX and all(0x21 <= c <= 0x7E for c in key)


def _entries(b: bytes, used: int) -> list[tuple[bytes, bytes]] | None:
    """The entries of a record of USED bytes, or None when one overruns it, has an illegal key, or a key is there twice."""
    out: list[tuple[bytes, bytes]] = []
    at = HEADER
    while at < used:
        kl = b[at]
        ks = at + 1
        if kl == 0 or kl > KEY_MAX or ks + kl + 2 > used:
            return None
        key = b[ks : ks + kl]
        (vl,) = struct.unpack_from(">H", b, ks + kl)
        vs = ks + kl + 2
        if vl > VALUE_MAX or vs + vl > used or not valid_key(key):
            return None
        if any(k == key for k, _ in out):
            return None
        out.append((key, b[vs : vs + vl]))
        at = vs + vl
    return out if at == used else None


def why(b: bytes) -> str:
    """`"ok"`, or the first rule the record breaks: magic, version, used, crc, entry (as `dkrt_sdk::settings::why`)."""
    if len(b) < HEADER or b[:4] != MAGIC:
        return "magic"
    (version,) = struct.unpack_from(">H", b, 4)
    if version != VERSION:
        return "version"
    (used,) = struct.unpack_from(">I", b, 10)
    if not HEADER <= used <= AREA or used > len(b):
        return "used"
    (crc,) = struct.unpack_from(">I", b, 14)
    if crc != _crc(b, used):
        return "crc"
    return "ok" if _entries(b, used) is not None else "entry"


@dataclass
class Record:
    seq: int
    used: int
    entries: list[tuple[bytes, bytes]] = field(default_factory=list)

    def get(self, key: str) -> bytes | None:
        k = key.encode()
        return next((v for e, v in self.entries if e == k), None)


def decode(b: bytes) -> Record | None:
    """The record, or None when it is not a valid one."""
    if why(b) != "ok":
        return None
    (seq,) = struct.unpack_from(">I", b, 6)
    (used,) = struct.unpack_from(">I", b, 10)
    return Record(seq, used, _entries(b, used) or [])


def encode(seq: int, entries: list[tuple[bytes, bytes]]) -> bytes:
    """A record of the entries in order (the runtime appends a new key at the end and keeps the order of the others)."""
    body = b"".join(
        bytes([len(k)]) + k + struct.pack(">H", len(v)) + v for k, v in entries
    )
    head = MAGIC + struct.pack(">HII", VERSION, seq, HEADER + len(body))
    return head + struct.pack(">I", zlib.crc32(head + body) & 0xFFFFFFFF) + body


def pick(a: bytes | None, b: bytes | None) -> int | None:
    """Which copy (0 = A, 1 = B) the runtime uses: the valid one with the higher seq, A when equal; None when neither is valid."""
    ra = decode(a) if a is not None else None
    rb = decode(b) if b is not None else None
    if ra and rb:
        return 1 if rb.seq > ra.seq else 0
    return 0 if ra else 1 if rb else None


def kind_of(key: bytes) -> str:
    """How a value is decoded: `u8`, `route`, `u8x16` (the owner's prefix in `sdk/sdk.toml`), or "" for a key no prefix covers."""
    text = key.decode("ascii", "replace")
    for prefix, kind in SETTINGS_KEYS:
        if text.startswith(prefix) and len(text) > len(prefix):
            return kind
    return ""


def decode_value(kind: str, v: bytes) -> tuple[str, Any]:
    """(the text a reader prints, the JSON value) of a value."""
    if kind == "route" and len(v) == 1 and v[0] <= 1:
        w = "master" if v[0] == 0 else "send"
        return f"{w} ({v[0]})", w
    if kind == "u8" and len(v) == 1:
        return str(v[0]), v[0]
    if kind == "u8x16" and len(v) == 16:
        return "[" + ", ".join(str(x) for x in v) + "]", list(v)
    if v and all(0x20 <= c < 0x7F for c in v):
        s = v.decode("ascii")
        return json.dumps(s), s
    return "0x" + v.hex(), None


def lines(r: Record) -> list[str]:
    out = [f"settings record: seq {r.seq}, {r.used} bytes, {len(r.entries)} entries"]
    for k, v in r.entries:
        out.append(f"    {k.decode()}  {decode_value(kind_of(k), v)[0]}")
    return out


def as_json(r: Record) -> dict[str, Any]:
    return {
        "seq": r.seq,
        "used": r.used,
        "entries": [
            {
                "key": k.decode(),
                "kind": kind_of(k),
                "value_hex": v.hex(),
                "value": decode_value(kind_of(k), v)[1],
            }
            for k, v in r.entries
        ],
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("file", type=Path)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    raw = args.file.read_bytes()
    r = decode(raw)
    if r is None:
        print(
            f"error: {args.file}: not a valid settings record ({why(raw)})",
            file=sys.stderr,
        )
        return 1
    print(json.dumps(as_json(r), indent=2) if args.json else "\n".join(lines(r)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
