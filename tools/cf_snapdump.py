"""Export an emu snapshot's registers + all mapped RAM/flash pages
to a flat binary the Rust bench can mmap without depending on Python.

Format (little-endian header ints; page bytes are the raw big-endian
ColdFire memory content, untouched):
  8s   magic "CFDUMP1\0"
  8x u32   d0..d7
  8x u32   a0..a7
  u32      pc
  u32      sr
  u32      npages
  then npages * (u32 base, 0x100000 bytes)
"""
import struct
import sys

sys.path.insert(0, ".")
from tools.snapread import Snapshot

PAGE = 0x100000


def export(snap_path, out_path):
    s = Snapshot(snap_path)
    r = s.regs
    bases = s.mapped_bases
    with open(out_path, "wb") as f:
        f.write(b"CFDUMP1\0")
        for i in range(8):
            f.write(struct.pack("<I", r[f"d{i}"] & 0xFFFFFFFF))
        for i in range(8):
            f.write(struct.pack("<I", r[f"a{i}"] & 0xFFFFFFFF))
        f.write(struct.pack("<I", r["pc"] & 0xFFFFFFFF))
        f.write(struct.pack("<I", r["sr"] & 0xFFFFFFFF))
        f.write(struct.pack("<I", len(bases)))
        for base in bases:
            data = s.read(base, PAGE)
            f.write(struct.pack("<I", base))
            f.write(data)
    print(f"{snap_path} -> {out_path}: {len(bases)} pages, pc=0x{r['pc']:08x}")


if __name__ == "__main__":
    export(sys.argv[1], sys.argv[2])
