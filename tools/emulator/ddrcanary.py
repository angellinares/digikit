#!/usr/bin/env python3
"""Canary fill and page map for the DSP's DDR (SDRAM) in `coupled-run` state files.

    uv run python -m tools.emulator.ddrcanary fill READY.snap.dsp OUT.snap.dsp
    coupled-run ... --dsp-state OUT.snap.dsp --scenario NAME --dump-dsp DUMP.bin
    uv run python -m tools.emulator.ddrcanary map --ready READY.snap.dsp DUMP.bin [DUMP2.bin ...] [--min-free-kib 64]

`fill` adds every DDR word that the ready state has not written (canonical byte
addresses [0x80000000, 0xa0000000), finding 106) as a word with an address-derived,
non-zero hash value. A scenario run from the filled state then writes the canary
only where the stock firmware stores to DDR. `map` reads dumps of such runs (the
engine exports every written byte, so the canary words are always present) and lists
the 4 KiB pages where a word differs from the canary, the touched ranges, and the
largest untouched ranges. A store of the exact canary value is not seen; the hash
makes that accidental case negligible. Reads are not seen (finding 97 has the bounded
access monitor). A range that the stock reads as zero before it writes it (a canary there halts the DSP;
DN2 1.12: `0x8052d790-0x8052fbe0`) is left out with `--exclude`.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

PAGE = 4096
DDR_START = 0x80000000
DDR_END = 0xA0000000
SIDECAR = b"DT2DSP01"
CHUNK = 1 << 26  # bytes per canary chunk


def canary_words(address: int, count: int) -> np.ndarray:
    """The canary words for `count` words from byte `address` (address 4-aligned)."""
    index = (np.arange(count, dtype=np.uint64) + np.uint64(address // 4)) & np.uint64(
        0xFFFFFFFF
    )
    mixed = (index * np.uint64(2654435761) + np.uint64(0x9E3779B9)) & np.uint64(
        0xFFFFFFFF
    )
    mixed ^= mixed >> np.uint64(15)
    return (mixed | np.uint64(1)).astype("<u4")  # never 0: stock clears are seen


def split_blob(raw: bytes) -> tuple[bytes, bytes]:
    """(sidecar header or b"", SHRD state blob)."""
    start = raw.find(b"SHRD")
    if start < 0:
        raise SystemExit("no SHRD state blob")
    return raw[:start], raw[start:]


def read_ddr_bytes(path: str) -> list[tuple[int, bytes]]:
    """DDR ranges of a dump: (canonical byte address, bytes)."""
    from tools.sharc_dev import sharc_diff

    _, blob = split_blob(Path(path).read_bytes())
    state = sharc_diff.unpack_state(blob)
    return [
        (addr, bytes.fromhex(r["data"]))
        for addr, r in sorted(state["memory_ranges"].items())
        if DDR_START <= addr < DDR_END
    ]


def read_ddr(path: str) -> list[tuple[int, np.ndarray]]:
    """DDR ranges of a dump as whole little-endian words (partial end words dropped)."""
    out = []
    for addr, data in read_ddr_bytes(path):
        lo = (addr + 3) & ~3
        hi = (addr + len(data)) & ~3
        if hi > lo:
            out.append((lo, np.frombuffer(data[lo - addr : hi - addr], "<u4")))
    return out


def parse_range(text: str) -> tuple[int, int]:
    lo, hi = text.split("-")
    return int(lo, 0), int(hi, 0)


def fill(
    src: str, dst: str, start: int, end: int, exclude: list[tuple[int, int]]
) -> None:
    from tools.sharc_dev import sharc_diff

    header, blob = split_blob(Path(src).read_bytes())
    state = sharc_diff.unpack_state(blob)
    written = sorted(
        [
            (a, a + r["length"])
            for a, r in state["memory_ranges"].items()
            if DDR_START <= a < DDR_END
        ]
        + exclude
    )
    gaps, cursor = [], start
    for lo, hi in written:
        if lo > cursor:
            gaps.append((cursor, min(lo, end)))
        cursor = max(cursor, hi)
    if cursor < end:
        gaps.append((cursor, end))
    total = 0
    for lo, hi in gaps:
        if hi <= lo:
            continue
        lo = (lo + 3) & ~3
        parts = []
        for pos in range(lo, hi, CHUNK):
            n = min(CHUNK, hi - pos) // 4
            parts.append(canary_words(pos, n).tobytes().hex())
        state["memory_ranges"][lo] = {"length": hi - lo, "data": "".join(parts)}
        total += hi - lo
    Path(dst).write_bytes(header + sharc_diff.pack_state(state))
    print(f"canary: {total / 1048576:.1f} MiB in {len(gaps)} ranges -> {dst}")


def ready_ranges(path: str) -> list[tuple[int, int]]:
    """DDR byte ranges the ready state had written (never canary)."""
    return [(a & ~3, (a + len(d) + 3) & ~3) for a, d in read_ddr_bytes(path)]


def touched_pages(
    dumps: list[str], start: int, end: int, skip: list[tuple[int, int]]
) -> dict[int, int]:
    """Page base -> number of canary words that changed (in any dump)."""
    pages: dict[int, int] = {}
    for path in dumps:
        for addr, words in read_ddr(path):
            lo = max(addr, start)
            hi = min(addr + 4 * len(words), end)
            if hi <= lo:
                continue
            got = words[(lo - addr) // 4 : (hi - addr) // 4]
            want = np.empty(len(got), "<u4")
            for pos in range(0, len(got), CHUNK // 4):
                n = min(CHUNK // 4, len(got) - pos)
                want[pos : pos + n] = canary_words(lo + 4 * pos, n)
            for s_lo, s_hi in skip:
                a, b = max(s_lo, lo), min(s_hi, hi)
                if b > a:
                    want[(a - lo) // 4 : (b - lo) // 4] = got[
                        (a - lo) // 4 : (b - lo) // 4
                    ]
            for idx in np.nonzero(got != want)[0]:
                base = (lo + 4 * int(idx)) & ~(PAGE - 1)
                pages[base] = pages.get(base, 0) + 1
    return pages


def ranges_of(bases: list[int]) -> list[tuple[int, int]]:
    out: list[tuple[int, int]] = []
    for b in sorted(bases):
        if out and out[-1][1] == b:
            out[-1] = (out[-1][0], b + PAGE)
        else:
            out.append((b, b + PAGE))
    return out


def changed_ready_words(ready: str, dump: str) -> int:
    """Words of the ready state's own ranges that a run changed (a check that the dump
    records stores; the stock rewrites its buffers while it runs)."""
    count = 0
    base = read_ddr(ready)
    for addr, words in read_ddr(dump):
        for r_addr, r_words in base:
            lo = max(addr, r_addr)
            hi = min(addr + 4 * len(words), r_addr + 4 * len(r_words))
            if hi > lo:
                a = words[(lo - addr) // 4 : (hi - addr) // 4]
                b = r_words[(lo - r_addr) // 4 : (hi - r_addr) // 4]
                count += int(np.count_nonzero(a != b))
    return count


def report(
    dumps: list[str],
    ready: str,
    start: int,
    end: int,
    min_free_kib: int,
    exclude: list[tuple[int, int]],
) -> None:
    known = ready_ranges(ready) + exclude
    pages = touched_pages(dumps, start, end, known)
    touched = ranges_of(list(pages))
    print(f"canary window {start:#x}..{end:#x}, dumps: {len(dumps)}")
    for path in dumps:
        print(
            f"  {Path(path).name}: {changed_ready_words(ready, path)} ready words changed"
        )
    print(f"touched pages: {len(pages)} ({len(pages) * PAGE // 1024} KiB)")
    for lo, hi in touched:
        words = sum(pages[b] for b in range(lo, hi, PAGE) if b in pages)
        print(f"  touched {lo:#x}..{hi:#x}  {(hi - lo) // 1024:>7} KiB  {words} words")
    busy = ranges_of(
        [
            *pages,
            *(
                b
                for lo, hi in known
                for b in range(lo & ~(PAGE - 1), hi, PAGE)
                if start - PAGE < b < end
            ),
        ]
    )
    free, cursor = [], start & ~(PAGE - 1)
    for lo, hi in [*busy, (end, end)]:
        if lo > cursor:
            free.append((cursor, lo))
        cursor = max(cursor, hi)
    free.sort(key=lambda r: r[0] - r[1])
    print("largest untouched ranges (pages without a stock write or ready data):")
    for lo, hi in free[:8]:
        print(f"  free {lo:#x}..{hi:#x}  {(hi - lo) // 1024} KiB")
    big = [r for r in free if r[1] - r[0] >= min_free_kib * 1024]
    print(f"untouched ranges >= {min_free_kib} KiB: {len(big)}")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fill", help="write a state file with the canary added")
    f.add_argument("src")
    f.add_argument("dst")
    f.add_argument("--start", type=lambda s: int(s, 0), default=DDR_START)
    f.add_argument("--end", type=lambda s: int(s, 0), default=DDR_END)
    f.add_argument(
        "--exclude",
        type=parse_range,
        action="append",
        default=[],
        metavar="LO-HI",
        help="byte range to leave out (the stock reads it as zero)",
    )
    m = sub.add_parser("map", help="page map of canary changes in dumps")
    m.add_argument("--ready", required=True, help="the unfilled ready state")
    m.add_argument("dumps", nargs="+")
    m.add_argument("--start", type=lambda s: int(s, 0), default=DDR_START)
    m.add_argument("--end", type=lambda s: int(s, 0), default=DDR_END)
    m.add_argument(
        "--exclude",
        type=parse_range,
        action="append",
        default=[],
        metavar="LO-HI",
        help="byte range to leave out (the stock reads it as zero)",
    )
    m.add_argument("--min-free-kib", type=int, default=64)
    a = p.parse_args(argv)
    if a.cmd == "fill":
        fill(a.src, a.dst, a.start, a.end, a.exclude)
    else:
        report(a.dumps, a.ready, a.start, a.end, a.min_free_kib, a.exclude)
    return 0


if __name__ == "__main__":
    sys.exit(main())
