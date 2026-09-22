#!/usr/bin/env python3
"""Assemble a VISA snippet with Selache and cross-check it against our own decoder.

Selache (https://github.com/js216/selache) reads VISA 16-bit parcels
big-endian; the DT2 image and tools/sharc_disasm.py read them little-endian
(the boot-stream convention -- see docs/findings/05-sharc-isa-and-decoding.md,
"selache encodes and decodes DT2 SHARC+ code, once parcel order is fixed").
So a snippet built with selas has to have every parcel byte-swapped before
our decoder can read it, and the resulting listing has to be compared to
Selache's own disassembly of the *unswapped* object, not to the source.

This assembles ONE snippet section with selas, reads its raw bytes straight
out of the ELF object (no dependency on any particular seldump text format),
and prints our decoder's listing next to Selache's, aligned by byte offset,
so a human can spot a disagreement immediately. It does not resolve
relocations (an unresolved branch target reads as zero); that is a
by-hand, per-snippet step, not something this script attempts.

Usage:
    uv run python selasm.py snippet.s
    uv run python selasm.py snippet.s --selache-dir /private/tmp/selache-public \
        --section seg_pmco
"""
from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_TOOLS = "/Users/em/src/digi/digitakt2/tools"
for path in (HERE, REPO_TOOLS):
    if path not in sys.path:
        sys.path.insert(0, path)

from sharc_disasm import disassemble  # noqa: E402

DEFAULT_SELACHE_DIR = "/private/tmp/selache-public"
DEFAULT_PROC = "ADSP-21569"


def swap_parcels(data: bytes) -> bytes:
    """Byte-swap every 16-bit parcel (a trailing odd byte is left alone)."""
    out = bytearray(data)
    for i in range(0, len(out) - 1, 2):
        out[i], out[i + 1] = out[i + 1], out[i]
    return bytes(out)


def assemble(selas: str, proc: str, src_path: str, doj_path: str) -> None:
    result = subprocess.run(
        [selas, "-proc", proc, "-o", doj_path, src_path],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"selas failed:\n{result.stdout}{result.stderr}")


def find_section(doj_path: str, name: str) -> tuple[int, int]:
    """-> (file_offset, size) of section NAME, read straight from the ELF
    section header table (avoids depending on seldump's text output)."""
    with open(doj_path, "rb") as f:
        data = f.read()
    # ELF32 header: e_shoff at 0x20, e_shentsize at 0x2e, e_shnum at 0x30,
    # e_shstrndx at 0x32 (all little-endian on this target's object files;
    # seldump's own hex dump of a known section matches file bytes 1:1, so
    # this reads the same way).
    endian = "<" if data[5] == 1 else ">"
    e_shoff, = struct.unpack_from(endian + "I", data, 0x20)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from(endian + "HHH", data, 0x2E)
    shstrtab_hdr = e_shoff + e_shstrndx * e_shentsize
    shstr_off, = struct.unpack_from(endian + "I", data, shstrtab_hdr + 0x10)
    shstr_size, = struct.unpack_from(endian + "I", data, shstrtab_hdr + 0x14)
    strtab = data[shstr_off:shstr_off + shstr_size]

    def sh_name(idx: int) -> str:
        start = idx
        end = strtab.index(b"\0", start)
        return strtab[start:end].decode()

    for i in range(e_shnum):
        base = e_shoff + i * e_shentsize
        name_off, = struct.unpack_from(endian + "I", data, base + 0x00)
        sh_offset, = struct.unpack_from(endian + "I", data, base + 0x10)
        sh_size, = struct.unpack_from(endian + "I", data, base + 0x14)
        if sh_name(name_off) == name:
            return sh_offset, sh_size
    raise SystemExit(f"section {name!r} not found in {doj_path}")


def selache_disasm(seldump: str, doj_path: str, section: str) -> str:
    result = subprocess.run(
        [seldump, "-ns", section, doj_path], capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"seldump failed:\n{result.stdout}{result.stderr}")
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("source", help="VISA .s file to assemble")
    parser.add_argument("--selache-dir", default=DEFAULT_SELACHE_DIR,
                        help=f"Selache checkout with target/release built (default: {DEFAULT_SELACHE_DIR})")
    parser.add_argument("--proc", default=DEFAULT_PROC, help=f"selas -proc target (default: {DEFAULT_PROC})")
    parser.add_argument("--section", default="seg_pmco", help="ELF section to compare (default: seg_pmco)")
    args = parser.parse_args()

    release = os.path.join(args.selache_dir, "target", "release")
    selas = os.path.join(release, "selas")
    seldump = os.path.join(release, "seldump")
    for exe in (selas, seldump):
        if not os.path.isfile(exe):
            raise SystemExit(f"missing {exe} -- build selache with `cargo build --release` first")

    with tempfile.TemporaryDirectory() as tmp:
        doj_path = os.path.join(tmp, "snippet.doj")
        assemble(selas, args.proc, args.source, doj_path)

        offset, size = find_section(doj_path, args.section)
        with open(doj_path, "rb") as f:
            f.seek(offset)
            raw = f.read(size)

        print(f"== Selache's own disassembly ({args.section}, {size} bytes) ==")
        print(selache_disasm(seldump, doj_path, args.section))

        swapped = swap_parcels(raw)
        print("== our decoder (tools/sharc_disasm.py), after the parcel-order swap ==")
        for insn in disassemble(swapped, on_unknown="yield"):
            if insn.kind == "unknown":
                print(f"  {insn.offset:#06x}  <{insn.note}>")
                break
            print(f"  {insn.offset:#06x}  {insn.type_name:<12s} {insn.kind:<10s} {insn.fields}")


if __name__ == "__main__":
    main()
