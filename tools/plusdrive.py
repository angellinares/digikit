#!/usr/bin/env python3
"""Build and read a +Drive card image, in the firmware's own on-disk format.

The format is reverse engineered in docs/findings/14-plus-drive-format.md.
Short version: +Drive has two independent regions at fixed, hard-coded
absolute sector numbers -- the ``MmcFs`` project/sound/kit pools (untouched
here) and a separate, real, path-addressable filesystem (128-byte metadata
records, extent-mapped 32 KiB content pages, hierarchical directories) that
sample files live in. This tool only writes the second region: a header
sector, an empty pool table, a root directory (with "." and "..", and the
three index pages the firmware keeps per directory), and one file per input
WAV, all under the root.

    uv run python tools/plusdrive.py build samples/ -o out/plusdrive/dt2.img
    uv run python tools/plusdrive.py ls out/plusdrive/dt2.img

``tools/plusdrive_check.py`` runs the firmware's own mount, directory,
reference, project-decode and sample-loader code on a built image.

``build`` also doubles as the only writer of this format, and ``ls`` as a
reader for any card image in it (including one dumped from a real firmware
card overlay), since both share the same decode/encode tables below.

To boot the emulator from a built image, pass its path as ``card_image=`` to
``emu.dspboot.run``/``emu.longrun.build`` (or ``--card-image`` on the CLI
runners that expose it); both construct the card via
``emu.esdhc.Card.from_file``, which mmaps the file read-only -- writes during
emulation go to the in-RAM overlay (already how snapshots capture +Drive
writes; see ``Esdhc.checkpoint_state``), and this file is never modified.

Samples are stored in the drive's native format, the one the sampler's
"save recording" writer (``FUN_40153994``) produces and the sample loader
(``FUN_40154540``) reads: a 64-byte header, big-endian 16-bit PCM at 48 kHz
(L/R interleaved when stereo), and a 16-byte trailer. Input WAVs are
converted and resampled (see ``wav_to_native``). Each file record carries
the firmware's content hash (``content_hash``) at +0x0C with bit 0 set, and
the same word goes into the hash table at sector 0x5EE980, as
``FUN_4015af0c`` does after a recording is closed.

With ``--main-os`` (default: ``out/sections/dt2-1.16/section_3_MAIN_OS.bin``
when present), ``build`` also writes the active-project record at sectors
0x40000/0x48000: the COKi header plus the firmware's own built-in project,
depacked from that image at build time, with every sample reference pointed
at the first input file. At boot the firmware decodes that project, mounts
the drive and its "Load all samples" job streams the sample to the DSP, with
no UI. Nothing from the firmware is stored in this repository.

Open questions the finding doc flags, most relevant to this tool:

* Record offset 0x01's meaning is not pinned down (see ATTR_* below); 0x00
  = 1 marks a directory (FUN_40155ed6 sets it on every new directory).
* A directory is limited to one content page, a file to eight extents.

This tool also writes the real filesystem's superblock at sector 0x5D8000
(``FUN_4015a450``'s mount check; see docs/findings/14-plus-drive-format.md's
"undocumented real-FS superblock" section) -- without it the mount always
fails and ``FileSystemDirectory`` never reports itself valid, which is what
threw the uncaught ``std::logic_error`` documented in
docs/findings/07-emulator.md. The checksum is a firmware-specific streaming
variant of Bob Jenkins' public-domain ``lookup3.c`` ``hashlittle`` (seeded
``initval + 0xDEADBEEF``, no length folded into the seed, and a
zero-length-input quirk that skips the finalizer -- see ``hashlittle()``
below); the Python reimplementation here was checked bit-for-bit against 44
real calls to the firmware's own ``FUN_4015abb2`` (lengths 0-29, 508, 511,
512, 600, 1199-1201, 1211, 2401, each with random content), covering every
tail case of its switch and both the single-block and multi-block streaming
paths -- see ``tests/test_plusdrive.py``.
"""

import argparse
import array
import hashlib
import math
import operator
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

SECTOR = 512
PAGE = 0x8000  # 32 KiB, 0x40 sectors
RECORD_SIZE = 0x80  # 128 bytes
RECORDS_PER_PAGE = PAGE // RECORD_SIZE  # 256

# Region 1 (MmcFs pools): left untouched (all zero), but the header and the
# empty pool-occupancy table are written so first-boot format detection
# passes and the pools read as validly-empty rather than corrupt. See
# docs/findings/14-plus-drive-format.md.
HEADER_SECTOR = 0
HEADER_MAGIC = 0xBEEFBACE
POOL_TABLE_SECTOR = 0x800

# A third, separate on-disk structure, unrelated to +Drive itself: a
# 256-byte "boot config"/calibration record FUN_400f0628 (called from
# FUN_400cc864, unconditionally, on every boot, before the eMMC-identity
# whitelist's own mount/format decision is even reached) reads from this
# sector and a redundant backup at BOOT_CONFIG_SECTOR_2. Left all-zero (as
# tools/plusdrive.py did until this was found), its validator
# (FUN_400c0e90 -> FUN_400c0e54: magic `0x434F4B69` ("COKi"), a checksum,
# and a bounded length field) fails on both copies, and FUN_400f0628 falls
# back to a "restore a ~13.85 MiB factory image from internal flash and
# rewrite this sector" repair path (FUN_400f04e2) that issues on the order
# of 28,000 CMD25 writes -- confirmed, by an instrumented cold boot, to be
# where a real boot task ends up permanently pending on `sd_dma_sem`
# (unposted in this build's Esdhc model either way; see emu/esdhc.py) far
# short of completing that many writes. Writing a minimal, self-consistent
# valid record here instead (matching FUN_400c0e54's own check) skips that
# whole unrelated repair path, the same way the region-1 header above is
# written just to satisfy ITS OWN first-boot format check.
BOOT_CONFIG_SECTOR = 0x40000
BOOT_CONFIG_SECTOR_2 = 0x48000
BOOT_CONFIG_MAGIC = 0x434F4B69  # "COKi"

# A fourth on-disk structure, also unrelated to +Drive's own filesystem: a
# 32 KiB "factory drum-hit table" FUN_4015a124 (mount's own continuation,
# past the superblock check) reads from sector 0x458000 via
# FUN_4002cd6a -> FUN_4002ccd0, and validates before going on to register
# ~1265 factory sample paths against it (FUN_4015bede/FUN_4002cc7e -- not
# implemented here; see FACTORY_TABLE_SECTOR's comment). Disassembled (not
# decompiled -- the decompile showed two fields at offsets 8/10 as if they
# overlapped a single 32-bit read, which is wrong): `FUN_4002ccd0` checks,
# in order: magic 0x4D61476A ("MaGj") at offset 0; a 16-bit field at offset
# 10 == 3; a 16-bit field at offset 8 == 0x2B; then a two-stage CRC-32/IEEE
# (`FUN_4013e06c`, confirmed standard by its table-init loop, poly
# 0xEDB88320): first pass seeded 0xFFFFFFFF over `record[8:8+length]` where
# `length = record[12:16] - 8` (a self-describing "total size" field, here
# built as small as possible: `length=4`, covering exactly the two 16-bit
# fields already checked); second pass chained from that result over
# `record[4:8]`; the final value must equal the literal `0xDEBB20E3`.
# `record[4:8]`'s own value isn't checked directly -- solved for with a
# GF(2) linear-algebra CRC inversion (the update function is linear in its
# input bits for a fixed starting state, the same property CRC-combine
# tools rely on) once the other fields were fixed, not guessed.
FACTORY_TABLE_SECTOR = 0x458000
FACTORY_TABLE_MAGIC = 0x4D61476A  # "MaGj"
FACTORY_TABLE_FIELD_8 = 0x2B
FACTORY_TABLE_FIELD_10 = 3
FACTORY_TABLE_LENGTH_FIELD = 12  # record[12:16]; CRC1 covers record[8:8+(12-8)]
FACTORY_TABLE_CRC_TARGET = 0xDEBB20E3
FACTORY_TABLE_FIELD_4 = b"\xa7\x27\x55\x8c"  # solved, see build_factory_table_record()

# Region 2 (the real filesystem): absolute sector numbers, hard-coded in the
# firmware, not derived from card capacity.
ID_BITMAP_SECTOR = 0x5D8040
PAGE_BITMAP_SECTOR = 0x5D80C0
PAGE_BITMAP_SECTOR_END = 0x5D8180
RECORD_AREA_SECTOR = 0x5D8180
CONTENT_AREA_SECTOR = 0x5EE180

# Content hash table: one BE u32 per record id, `hash | 1` (0 = none), 0x2000
# ids per 32 KiB page, 44 pages from sector 0x5EE980 (content pages
# 0x20-0x4B). FUN_4015af0c writes word `id & 0x1fff` of page `id >> 13`;
# FUN_4015ae12 (mount) scans exactly [0x5EE980, 0x5EF480) into the RAM hash
# index that FUN_4015a94c binary-searches.
HASH_TABLE_SECTOR = 0x5EE980
HASH_TABLE_END_SECTOR = 0x5EF480
# The format (FUN_4015a164) reserves content pages 0x00-0x1F
# (FUN_40155698(0x20,...)), then two 44-page runs, 0x20-0x4B (the hash
# table) and 0x4C-0x77 (zero-filled, purpose unknown). The first page it
# can hand out afterwards is 0x78; file and directory pages start there.
RESERVED_PAGES = 0x78
FIRST_FILE_PAGE = RESERVED_PAGES
# FUN_4015b4f0 (format) sets ids 0 and 1 in the id bitmap before the root
# (id 2) is allocated, so the allocator never hands them out.
RESERVED_IDS = (0, 1)

# Region 2's superblock: FUN_4015a450 (mount) and FUN_4015a164 (format
# writer), see docs/findings/14-plus-drive-format.md. Sector-relative field
# values (0x14/0x18/0x1c/0x20) are each `<region-start-sector> - SUPERBLOCK_SECTOR`,
# confirmed by exact arithmetic against the sector constants above. Fields
# 0x2c/0x30 are two runtime-allocated physical sectors (each the start of a
# 44-page run reserved by FUN_40155698(0x2c,...), called twice; the format
# zero-fills [0x5ee980, 0x5ef480) and [0x5ef480, 0x5eff80)). The first run is
# the content hash table (HASH_TABLE_SECTOR below): FUN_4015ae12 scans only
# [0x5ee980, 0x5ef480), 44 pages of 0x2000 words, one per record id. What
# the second run holds is not known.
# Fields 0x24/0x28 are literal constants in the writer, not derived from a
# call: 0x24 = PAGE/SECTOR (sectors per 32 KiB page); 0x28 is the argument
# the writer itself passes to one FUN_40155698 call (a page-count request),
# reused here as-is. None of these five fields are read back by the mount
# path (FUN_4015a450/FUN_4015a124 only check magic/version/checksum), so
# they cannot fail a mount; they are written for fidelity, not correctness.
SUPERBLOCK_SECTOR = 0x5D8000
SUPERBLOCK_MAGIC = 0x656B4653
SUPERBLOCK_VERSION = 4  # mount accepts 3 or 4
SUPERBLOCK_HASH_SEED = 0x31323334
SUPERBLOCK_HASH_LEN = 0x1FC  # bytes [0x00:0x1FC) are checksummed
SUPERBLOCK_CHECKSUM_OFFSET = 0x1FC  # last 4 bytes of the 512-byte sector

ROOT_ID = 2
ROOT_PARENT = 2  # the root is its own parent; nothing reads this for id 2

# Reserved logical pages inside a directory's own extent list. A new
# directory (FUN_40155ed6) gets content page 0 and these three index pages
# as one 0x18000-byte allocation at byte offset 0x80000000 (logical page
# 0x10000). Each index page is u16 count at 0, then 8-byte entries from
# offset 8: {u32 key, u32 position}, position = content page * 0x8000 +
# byte offset of the entry (FUN_40156334 writes all three; the readers index
# them as `(ushort *)page + i * 4 + 6`, i.e. byte 8 * i + 12).
LOGICAL_HASH_PAGE = 0x10000  # key: name_hash(name), sorted; FUN_401572e0 (by name)
LOGICAL_INDEX_PAGE = 0x10001  # key: first 4 name bytes, listing order; FUN_40157072
LOGICAL_ID_PAGE = 0x10002  # key: record id, sorted; FUN_401574e2 (path from id)

# Record offset 0x00 / 0x01 bit-0 semantics are not disambiguated in the
# finding (candidates: is-directory, protected/read-only, a third flag).
# Best inference: offset 0x00 bit 0 marks a directory (this is the byte the
# directory-entry writer copies into its own per-entry "type" field, and
# what FileSystemDirectory's attribute vfuncs test), offset 0x01 keeps the
# allocator's own default (2, bit 0 clear) for both kinds.
ATTR_DIR = 0x01
ATTR_FILE = 0x00
DEFAULT_OFFSET01 = 0x02

DEFAULT_CAPACITY_BLOCKS = 0x00760000  # matches emu/esdhc.py's Card default

# Native sample file (FUN_40153994 writes it, FUN_40154540 reads it):
#   0x00       0
#   0x01       1 = stereo (L/R interleaved), 0 = mono
#   0x04  u32  PCM data length in bytes
#   0x08  u32  sample rate, always 48000 from the writer
#   0x14       0x7F
#   rest of the 64 bytes 0
#   0x40..     signed 16-bit big-endian PCM (FUN_400cce2c sends each
#              16-bit half low byte first, so the DSP receives it as
#              little-endian int16)
#   end        16 bytes copied from 0x405a50a0, which nothing else in the
#              image references (tools/refscan.py): zero
# File size = data length + 0x50 (FUN_40158534(file, 0, len + 0x50)).
NATIVE_HEADER_SIZE = 0x40
NATIVE_TRAILER_SIZE = 0x10
NATIVE_RATE = 48000
NATIVE_HEADER_0x14 = 0x7F
# FUN_4015af0c seeds its streaming hashlittle state a = b = c = 0x43FA243A
# before hashing the whole file (header, PCM and trailer). hashlittle()
# below takes `initval` and adds 0xDEADBEEF itself.
CONTENT_HASH_STATE = 0x43FA243A

# Active-project record (FUN_400f0628 reads it at every boot): a 0x110-byte
# COKi header, then the project container that FUN_400c0c2c decodes, read as
# one 0xDD9714-byte block from sector 0x40000 (0x48000 if the first header
# fails its check).
PROJECT_RECORD_SIZE = 0xDD9714
PROJECT_HEADER_SIZE = 0x110
PROJECT_CONTAINER_SIZE = PROJECT_RECORD_SIZE - PROJECT_HEADER_SIZE  # 0xDD9604
# COKi header word at +0x104 (0x4099d68c at run time) is the filesystem's
# record sequence counter: FUN_4015b7ac sets a new record's +0x10 to
# ++counter. +0x18 (0x4099d5a0) receives the superblock version at mount
# (FUN_400c0eea).
PROJECT_HEADER_SEQ_OFFSET = 0x104
PROJECT_HEADER_FS_VERSION_OFFSET = 0x18

# The built-in project in DT2 1.16's MAIN OS (depacked on factory reset,
# FUN_400c0c2c's flags & 6): an ELZ-packed projectStorage v3 container. The
# decoder upgrades v3 -> v4 -> v5 in place (FUN_400e0c26).
MAIN_OS_BASE = 0x40000400
MAIN_OS_116_SHA256 = "57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d"
DEFAULT_MAIN_OS = os.path.join("out", "sections", "dt2-1.16", "section_3_MAIN_OS.bin")
BUILTIN_PROJECT_ADDR = 0x4025F1D8
CONTAINER_MAGIC = 0xBEEFBACE
CONTAINER_END_MAGIC = 0xBACEF00C
V3_SIZE = 0xC3F424
V3_END_MARKER = 0xC3E400  # word 0x30f900, checked by FUN_400dedb6
V3_REF_TABLE = 0xC2D6FB  # 1024 x {id, hash|1, size, seq}, packed BE
V3_REF_COUNT = 0x400
V3_EMPTY_REF = struct.pack(">IIII", 0xFFFFFFFF, 0, 0, 0)
# v3 kits: 129 x 0x2800 from 0xAE0200 (FUN_400dedb6's loop); 16 tracks of
# 0x155 bytes from kit + 0x3C (FUN_400dea22). In a track: int16 sample slot
# at +0x64, and a copy of that slot's 16-byte reference at +0x129.
V3_KITS = 0xAE0200
V3_KIT_SIZE = 0x2800
V3_KIT_COUNT = 129
V3_TRACK0 = 0x3C
V3_TRACK_SIZE = 0x155
V3_TRACK_SLOT = 0x64
V3_TRACK_REF = 0x129
# The kit the built-in project decodes as active: FUN_4004e598 (the slot
# list "Load all samples" walks) run on the decoded project lists kit 0's
# track slots first (7, 1, 3, 8, ...), tools/plusdrive_check.py.
ACTIVE_KIT = 0


def _u16(v):
    return struct.pack(">H", v & 0xFFFF)


def _u32(v):
    return struct.pack(">I", v & 0xFFFFFFFF)


_MASK32 = 0xFFFFFFFF


def _rotl(x, n):
    return ((x << n) | (x >> (32 - n))) & _MASK32


def _hashlittle_mix(a, b, c, k0, k1, k2):
    """One 12-byte mix() round, transliterated instruction-for-instruction
    from FUN_4015a6dc/FUN_4015aa20's decompiled loop body (rotate constants
    4, 6, 8, 16, 19, 4) rather than reconstructed from the textbook
    lookup3.c macro, so it stays bit-exact even though the compiler fused
    the usual `a+=k0; b+=k1; c+=k2;` pre-step into the first two rounds."""
    c = (k2 + c) & _MASK32
    t2 = (k1 + b + c) & _MASK32
    u1 = (_rotl(c, 4) ^ ((k0 + a - c) & _MASK32)) & _MASK32
    t3 = (t2 + u1) & _MASK32
    u2 = (((k1 + b - u1) & _MASK32) ^ _rotl(u1, 6)) & _MASK32
    t4 = (t3 + u2) & _MASK32
    u3 = (((t2 - u2) & _MASK32) ^ _rotl(u2, 8)) & _MASK32
    t2b = (t4 + u3) & _MASK32
    u4 = (((t3 - u3) & _MASK32) ^ _rotl(u3, 16)) & _MASK32
    a = (t2b + u4) & _MASK32
    u5 = (((t4 - u4) & _MASK32) ^ _rotl(u4, 19)) & _MASK32
    b = (a + u5) & _MASK32
    c = (((t2b - u5) & _MASK32) ^ _rotl(u5, 4)) & _MASK32
    return a, b, c


def _hashlittle_final(a, b, c):
    """final(a,b,c) -> c, transliterated from the tail of FUN_4015a6dc
    (rotate constants 14, 11, 25, 16, 4, 14, 24 -- Jenkins' standard
    lookup3.c final())."""
    u1 = ((b ^ c) - _rotl(b, 14)) & _MASK32
    u2 = ((u1 ^ a) - _rotl(u1, 11)) & _MASK32
    u3 = ((u2 ^ b) - _rotl(u2, 25)) & _MASK32
    u4 = ((u3 ^ u1) - _rotl(u3, 16)) & _MASK32
    a2 = ((u4 ^ u2) - _rotl(u4, 4)) & _MASK32
    b2 = ((a2 ^ u3) - _rotl(a2, 14)) & _MASK32
    c2 = ((b2 ^ u4) - _rotl(b2, 24)) & _MASK32
    return c2


def _tail_word(data, off, n):
    """Read n (<=4) bytes big-endian, left-justified in a 32-bit word with
    the missing low bytes read as zero -- matches the firmware's own
    `*puVar8 & 0xff000000`-style masked read of a short, in-bounds-buffer
    tail word (the mask keeps the top n bytes and zeros the low 4-n)."""
    buf = data[off : off + n] + b"\0" * (4 - n)
    return struct.unpack(">I", buf)[0]


def hashlittle(data, initval):
    """The firmware's real-filesystem superblock checksum (FUN_4015abb2 /
    FUN_4015aa20 / FUN_4015a6dc): a streaming variant of Bob Jenkins'
    public-domain lookup3.c ``hashlittle`` -- same mix()/final() rotate
    constants, but seeded ``initval + 0xDEADBEEF`` with no length folded in
    (unlike the textbook one-shot version), and with a firmware-specific
    quirk: a zero-length input's tail switch jumps straight past final(),
    returning the raw seed unfinalized. See docs/findings/14 and this
    module's docstring. Bit-exact for all 32-bit lengths (verified against
    44 real firmware calls covering every tail case 0-12 and multi-block
    streaming; tests/test_plusdrive.py has the golden vectors)."""
    a = b = c = (initval + 0xDEADBEEF) & _MASK32
    n = len(data)
    if n == 0:
        return c  # the finalize-skipping quirk described above
    off = 0
    while n > 12:
        k0, k1, k2 = struct.unpack_from(">III", data, off)
        a, b, c = _hashlittle_mix(a, b, c, k0, k1, k2)
        off += 12
        n -= 12
    # n is now the 1..12-byte tail, mirroring FUN_4015a6dc's switch exactly.
    if n == 1:
        a = (a + _tail_word(data, off, 1)) & _MASK32
    elif n == 2:
        a = (a + _tail_word(data, off, 2)) & _MASK32
    elif n == 3:
        a = (a + _tail_word(data, off, 3)) & _MASK32
    elif n == 4:
        (k0,) = struct.unpack_from(">I", data, off)
        a = (a + k0) & _MASK32
    elif 5 <= n <= 8:
        (k0,) = struct.unpack_from(">I", data, off)
        a = (a + k0) & _MASK32
        b = (b + _tail_word(data, off + 4, n - 4)) & _MASK32
    elif 9 <= n <= 12:
        k0, k1 = struct.unpack_from(">II", data, off)
        a = (a + k0) & _MASK32
        b = (b + k1) & _MASK32
        c = (c + _tail_word(data, off + 8, n - 8)) & _MASK32
    return _hashlittle_final(a, b, c)


def build_superblock():
    """-> the 512-byte real-filesystem superblock FUN_4015a450 mounts.

    All fields except the checksum are literal or derived constants (see the
    SUPERBLOCK_* comment above); nothing here depends on card content, so
    this is deterministic and independent of what `build()` writes anywhere
    else.
    """
    buf = bytearray(SECTOR)
    struct.pack_into(">I", buf, 0x00, SUPERBLOCK_MAGIC)
    struct.pack_into(">I", buf, 0x04, SUPERBLOCK_VERSION)
    struct.pack_into(">I", buf, 0x08, PAGE)
    struct.pack_into(">I", buf, 0x0C, 0x58000)
    struct.pack_into(">I", buf, 0x10, 0xA0080)
    struct.pack_into(">I", buf, 0x14, ID_BITMAP_SECTOR - SUPERBLOCK_SECTOR)
    struct.pack_into(">I", buf, 0x18, PAGE_BITMAP_SECTOR - SUPERBLOCK_SECTOR)
    struct.pack_into(">I", buf, 0x1C, RECORD_AREA_SECTOR - SUPERBLOCK_SECTOR)
    struct.pack_into(">I", buf, 0x20, CONTENT_AREA_SECTOR - SUPERBLOCK_SECTOR)
    struct.pack_into(">I", buf, 0x24, PAGE // SECTOR)  # sectors per page
    struct.pack_into(">I", buf, 0x28, 0x20)
    struct.pack_into(">I", buf, 0x2C, 0x5EE980)
    struct.pack_into(">I", buf, 0x30, 0x5EF480)
    checksum = hashlittle(bytes(buf[:SUPERBLOCK_HASH_LEN]), SUPERBLOCK_HASH_SEED)
    struct.pack_into(">I", buf, SUPERBLOCK_CHECKSUM_OFFSET, checksum)
    return bytes(buf)


def build_boot_config_record():
    """-> the 256-byte record FUN_400c0e54 validates at BOOT_CONFIG_SECTOR
    (and its backup, BOOT_CONFIG_SECTOR_2) -- see BOOT_CONFIG_SECTOR's
    comment. Unrelated to +Drive's own filesystem; this only exists to make
    FUN_400f0628's unconditional boot-time check pass immediately instead of
    falling into its 13.85 MiB internal-flash "repair" path.

    FUN_400c0e54's check, transliterated: `buf[0]==0x434F4B69 and buf[3]<0xF1
    and FUN_400c0d3a(buf)==buf[1]` (all as big-endian 32-bit words).
    FUN_400c0d3a sums `(i ^ buf[2+i-1])` for i in 1..(buf[3]+8)>>2 -- i.e. it
    covers `buf[3]` itself (word index 1 relative to its own start at word
    2) as well as whatever payload precedes it. This picks the simplest
    valid record: length field (word 3) = 0, so the loop covers exactly
    words 2 and 3 (both left 0), giving a checksum of `(1^0) + (2^0) = 3`
    -- no payload beyond the header is written or needed for a boot that
    only checks validity, not content.
    """
    return build_project_header(SECTOR // 2)


def build_project_header(size=PROJECT_HEADER_SIZE, seq=0, fs_version=0):
    """-> the COKi header, `size` bytes (256 for the check alone, 0x110 in
    front of a project container). The checksum covers words 2 and 3 only
    (length field 0), so `seq` (+0x104) and `fs_version` (+0x18) do not
    change it. Word +0x14 stays 0: bit 0 or 1 set there makes FUN_400f0628
    skip reading the project container."""
    buf = bytearray(size)
    struct.pack_into(">I", buf, 0x00, BOOT_CONFIG_MAGIC)
    struct.pack_into(">I", buf, 0x0C, 0)  # length field, must be < 0xF1
    checksum = (1 ^ 0) + (2 ^ 0)  # words at offsets 8 and 12, both 0
    struct.pack_into(">I", buf, 0x04, checksum)
    if size > PROJECT_HEADER_SEQ_OFFSET:
        struct.pack_into(">I", buf, PROJECT_HEADER_SEQ_OFFSET, seq)
    struct.pack_into(">I", buf, PROJECT_HEADER_FS_VERSION_OFFSET, fs_version)
    return bytes(buf)


_CRC32_TABLE = None


def _crc32_table():
    """The exact table FUN_4013e06c builds once (poly 0xEDB88320, the
    standard reflected CRC-32/IEEE polynomial) and caches -- reproduced here
    from its own disassembly (`out/ghidra/dt2-1.16-emac/disasm/
    4013e06c_FUN_4013e06c.s`), not from a library, so the update loop below
    matches instruction-for-instruction."""
    global _CRC32_TABLE
    if _CRC32_TABLE is None:
        table = []
        for i in range(256):
            c = i
            for _ in range(8):
                c = (c >> 1) ^ (0xEDB88320 if c & 1 else 0)
            table.append(c & 0xFFFFFFFF)
        _CRC32_TABLE = table
    return _CRC32_TABLE


def crc32_ieee_raw(seed, data):
    """FUN_4013e06c(seed, data, len(data)) -- one byte at a time,
    `crc = table[(crc ^ byte) & 0xff] ^ (crc >> 8)`, starting from `seed`
    and returning the raw resulting register with **no** initial/final
    complement (unlike the textbook "full" CRC-32 checksum, which XORs
    0xFFFFFFFF in and out around exactly this same per-byte loop -- this
    firmware's caller does that XOR-in explicitly, by passing
    `seed=0xFFFFFFFF` itself, and never XORs the result back out, so the
    raw register value is what gets compared). Confirmed against the real
    firmware with a bounded call in `tests/test_factory_table.py`.
    """
    table = _crc32_table()
    crc = seed & 0xFFFFFFFF
    for byte in data:
        crc = (table[(crc ^ byte) & 0xFF] ^ (crc >> 8)) & 0xFFFFFFFF
    return crc


def build_factory_table_record():
    """-> the 32 KiB record `FUN_4002ccd0` validates at
    `FACTORY_TABLE_SECTOR` -- see that constant's comment for the full
    derivation. `FACTORY_TABLE_FIELD_4`'s bytes were solved (not guessed)
    from the other, checked fields via a GF(2) linear-algebra CRC
    inversion: `crc32_ieee_raw` is linear in its input bits for a fixed
    starting state (the same property behind CRC-combine algorithms), so
    `crc32_ieee_raw(crc1, V) = crc32_ieee_raw(crc1, b'\\0\\0\\0\\0') XOR
    (XOR of each set bit's own zero-seeded contribution)`; solving
    `target XOR A = M @ x` over GF(2) for the 32 unknown bits of `V` (a
    32x32 system, always solvable since the map is a bijection) gives a
    valid `V` directly, verified by re-running the forward computation
    below and, live, in `tests/test_factory_table.py`.

    The rest of the 32 KiB (everything after the checked/checksummed
    header) is left zero: `FUN_4002cd6a`'s own success path (reached once
    this record validates) goes on to register ~1265 factory sample paths
    against further fields of this buffer, but its own return value does
    not depend on that loop succeeding -- it returns success unconditionally
    once this record's header validates. **[O]**: the factory drum-hit
    content itself (not needed for mount to succeed) is not implemented.
    """
    buf = bytearray(PAGE)  # 32 KiB
    struct.pack_into(">I", buf, 0x00, FACTORY_TABLE_MAGIC)
    buf[0x04:0x08] = FACTORY_TABLE_FIELD_4
    struct.pack_into(">H", buf, 0x08, FACTORY_TABLE_FIELD_8)
    struct.pack_into(">H", buf, 0x0A, FACTORY_TABLE_FIELD_10)
    struct.pack_into(">I", buf, 0x0C, FACTORY_TABLE_LENGTH_FIELD)
    crc1 = crc32_ieee_raw(
        0xFFFFFFFF, bytes(buf[0x08 : 0x08 + (FACTORY_TABLE_LENGTH_FIELD - 8)])
    )
    crc2 = crc32_ieee_raw(crc1, bytes(buf[0x04:0x08]))
    assert crc2 == FACTORY_TABLE_CRC_TARGET, (
        "FACTORY_TABLE_FIELD_4 no longer satisfies the CRC -- re-derive it "
        "(see build_factory_table_record's docstring)"
    )
    return bytes(buf)


def content_hash(data):
    """-> the content hash FUN_4015af0c stores (before `| 1`) in a file's
    record at +0x0C and in the hash table: hashlittle over the whole file,
    state seeded a = b = c = CONTENT_HASH_STATE. The firmware streams the
    file through FUN_4015aa20 in 32 KiB reads; hashing it in one piece gives
    the same value (checked against FUN_4015af0c itself, see
    tools/plusdrive_check.py)."""
    return hashlittle(data, (CONTENT_HASH_STATE - 0xDEADBEEF) & _MASK32)


def name_hash(name):
    """FUN_40155f96(name, len): the key of a directory's 0x10000 index,
    transliterated from its disassembly (bytes unsigned, muls.l, and the
    `bpl` fold into 31 bits)."""
    cur, prev = 0x12A3FE2D, 0x37ABE8F9
    for b in name:
        v = (((b * 0x6D22F5) ^ cur) + prev) & _MASK32
        if v & 0x80000000:
            v = (v + 0x80000001) & _MASK32
        prev, cur = cur, v
    return (cur * 2) & _MASK32


def _name_cmp(a, b):
    """Listing order of names, after FUN_40135df2: case-insensitive, digit
    runs compared by value. Approximate (its digit-run branch is only
    matched for plain numbers); it only orders the 0x10001 listing."""
    i = j = 0
    while True:
        if i == len(a):
            return -1 if j < len(b) else 0
        if j == len(b):
            return 1
        if a[i : i + 1].isdigit() and b[j : j + 1].isdigit():
            i2, j2 = i, j
            while i2 < len(a) and a[i2 : i2 + 1].isdigit():
                i2 += 1
            while j2 < len(b) and b[j2 : j2 + 1].isdigit():
                j2 += 1
            na, nb = int(a[i:i2]), int(b[j:j2])
            if na != nb:
                return -1 if na < nb else 1
            i, j = i2, j2
            continue
        ca, cb = a[i : i + 1].lower(), b[j : j + 1].lower()
        if ca != cb:
            return -1 if ca < cb else 1
        i += 1
        j += 1


def build_directory(dir_id, parent_id, children):
    """-> (content page, 0x10000 page, 0x10001 page, 0x10002 page) for a
    directory holding "." (dir_id), ".." (parent_id) and `children`, a list
    of (name bytes, record id, type byte), laid out as FUN_40156334 leaves
    them after linking each in turn: entries back to back, each
    `(8 + name length)` rounded up to 4 bytes, the last one's slot length
    running to the end of the page. One content page only."""
    import functools

    entries = [(b".", dir_id, ATTR_DIR), (b"..", parent_id, ATTR_DIR)] + list(children)
    content = bytearray(PAGE)
    positions = []
    pos = 0
    for n, (name, rid, kind) in enumerate(entries):
        used = (len(name) + 0xB) & ~3
        if pos + used > PAGE:
            raise ValueError("directory exceeds one 32 KiB page (unimplemented)")
        slot = PAGE - pos if n == len(entries) - 1 else used
        struct.pack_into(">IHBB", content, pos, rid, slot, len(name), kind)
        content[pos + 8 : pos + 8 + len(name)] = name
        positions.append(pos)
        pos += used

    def index(keys):
        page = bytearray(8 + 8 * len(keys))
        struct.pack_into(">H", page, 0, len(keys))
        for i, (key, where) in enumerate(keys):
            struct.pack_into(">II", page, 8 + 8 * i, key, where)
        return bytes(page)

    by_hash = sorted(
        ((name_hash(e[0]), p) for e, p in zip(entries, positions, strict=True)),
        key=lambda kp: kp[0],
    )
    dirs_first = sorted(
        range(2, len(entries)),
        key=functools.cmp_to_key(
            lambda x, y: (
                (entries[y][2] == ATTR_DIR) - (entries[x][2] == ATTR_DIR)
                or _name_cmp(entries[x][0], entries[y][0])
            )
        ),
    )
    listing = [
        (struct.unpack(">I", entries[k][0][:4].ljust(4, b"\0"))[0], positions[k])
        for k in [0, 1] + dirs_first
    ]
    by_id = sorted(
        ((e[1], p) for e, p in zip(entries, positions, strict=True)),
        key=lambda kp: kp[0],
    )
    return bytes(content), index(by_hash), index(listing), index(by_id)


# -- WAV input ---------------------------------------------------------------

_WAVE_PCM = 1
_WAVE_FLOAT = 3
_WAVE_EXTENSIBLE = 0xFFFE


def read_wav(data):
    """-> (rate, channels): `channels` is a list of per-channel lists of
    floats in [-1, 1). Accepts PCM 8/16/24/32-bit, IEEE float 32/64, and
    WAVE_FORMAT_EXTENSIBLE wrapping either."""
    if data[0:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("not a RIFF/WAVE file")
    pos = 12
    fmt = None
    pcm = None
    while pos + 8 <= len(data):
        tag = data[pos : pos + 4]
        (size,) = struct.unpack_from("<I", data, pos + 4)
        body = data[pos + 8 : pos + 8 + size]
        if tag == b"fmt ":
            fmt = body
        elif tag == b"data":
            pcm = body
        pos += 8 + size + (size & 1)
    if fmt is None or pcm is None:
        raise ValueError("WAV has no fmt or data chunk")
    tag, n_ch, rate, _, block, bits = struct.unpack_from("<HHIIHH", fmt, 0)
    if tag == _WAVE_EXTENSIBLE:
        (tag,) = struct.unpack_from("<H", fmt, 24)  # SubFormat GUID's first field
    if n_ch < 1 or block != n_ch * bits // 8:
        raise ValueError("unsupported WAV layout (%d ch, block %d)" % (n_ch, block))
    n = len(pcm) // block * n_ch
    pcm = pcm[: n * (bits // 8)]
    if tag == _WAVE_PCM and bits == 8:
        vals = [(b - 128) / 128.0 for b in pcm]
    elif tag == _WAVE_PCM and bits in (16, 24, 32):
        # Widen every sample to a native int32 with the value in the top
        # bits, then scale.
        width = bits // 8
        wide = bytearray(n * 4)
        for i in range(width):
            wide[4 - width + i :: 4] = pcm[i::width]
        words = array.array("i", bytes(wide))
        if sys.byteorder == "big":
            words.byteswap()
        vals = [w / 2147483648.0 for w in words]
    elif tag == _WAVE_FLOAT and bits in (32, 64):
        floats = array.array("f" if bits == 32 else "d", pcm)
        if sys.byteorder == "big":
            floats.byteswap()
        vals = list(floats)
    else:
        raise ValueError("unsupported WAV encoding (tag %#x, %d bits)" % (tag, bits))
    return rate, [vals[c::n_ch] for c in range(n_ch)]


def _bessel_i0(x):
    total = term = 1.0
    k = 1
    while term > 1e-12 * total:
        term *= (x / (2.0 * k)) ** 2
        total += term
        k += 1
    return total


def resample(samples, src_rate, dst_rate, half_taps=32, rolloff=0.95, beta=8.6):
    """Band-limited rational resampling of one channel (list of floats):
    a Kaiser-windowed sinc, `half_taps` input samples each side, cut off at
    `rolloff` x the lower Nyquist frequency. Deterministic (no dither)."""
    if src_rate == dst_rate:
        return list(samples)
    g = math.gcd(src_rate, dst_rate)
    up, down = dst_rate // g, src_rate // g
    fc = min(1.0, dst_rate / src_rate) * rolloff
    i0_beta = _bessel_i0(beta)
    width = 2 * half_taps
    phases = []
    for p in range(up):
        taps = []
        for k in range(width):
            t = p / up + (half_taps - 1 - k)
            x = fc * t
            sinc = 1.0 if x == 0 else math.sin(math.pi * x) / (math.pi * x)
            r = t / half_taps
            win = (
                _bessel_i0(beta * math.sqrt(1.0 - r * r)) / i0_beta if r * r < 1 else 0
            )
            taps.append(fc * sinc * win)
        phases.append(taps)
    padded = [0.0] * half_taps + list(samples) + [0.0] * half_taps
    n_out = (len(samples) * up + down - 1) // down
    out = []
    mul = operator.mul
    for n in range(n_out):
        i, p = divmod(n * down, up)
        # taps[k] weighs input sample i - half_taps + 1 + k, i.e.
        # padded[i + 1 + k].
        out.append(sum(map(mul, phases[p], padded[i + 1 : i + 1 + width])))
    return out


def _to_int16_be(channels):
    """Interleave float channels into big-endian int16 bytes (round to
    nearest, clip)."""
    n = len(channels[0])
    out = array.array("h", bytes(2 * n * len(channels)))
    for c, chan in enumerate(channels):
        step = len(channels)
        for i, v in enumerate(chan):
            s = round(v * 32768.0)
            out[i * step + c] = 32767 if s > 32767 else (-32768 if s < -32768 else s)
    if sys.byteorder == "little":
        out.byteswap()
    return out.tobytes()


def build_native_sample(pcm_be16, stereo):
    """-> a native sample file around already-converted PCM (big-endian
    int16, L/R interleaved when stereo), laid out as FUN_40153994 writes
    it."""
    header = bytearray(NATIVE_HEADER_SIZE)
    header[0x01] = 1 if stereo else 0
    struct.pack_into(">I", header, 0x04, len(pcm_be16))
    struct.pack_into(">I", header, 0x08, NATIVE_RATE)
    header[0x14] = NATIVE_HEADER_0x14
    return bytes(header) + pcm_be16 + bytes(NATIVE_TRAILER_SIZE)


def wav_to_native(wav_bytes):
    """-> (native file bytes, info dict). Converts any supported WAV to what
    the loader expects: 48 kHz (band-limited resampling when the source rate
    differs), 16-bit big-endian, mono or stereo (more than two channels are
    rejected)."""
    rate, channels = read_wav(wav_bytes)
    if len(channels) > 2:
        raise ValueError("%d-channel WAV: only mono or stereo" % len(channels))
    converted = [resample(ch, rate, NATIVE_RATE) for ch in channels]
    pcm = _to_int16_be(converted)
    info = {
        "src_rate": rate,
        "channels": len(channels),
        "src_frames": len(channels[0]),
        "frames": len(converted[0]),
        "data_len": len(pcm),
    }
    return build_native_sample(pcm, len(channels) == 2), info


def parse_native_header(data):
    """-> (stereo, data_len, rate) as FUN_40154540 reads them."""
    stereo = data[0x01] == 1
    data_len, rate = struct.unpack_from(">II", data, 0x04)
    return stereo, data_len, rate


# -- Active-project record ---------------------------------------------------


def builtin_project(main_os):
    """-> the depacked v3 built-in project container from a DT2 1.16 MAIN OS
    image (bytes). Refuses any other image: every offset here is 1.16's."""
    from dt2.elz import depack_section

    digest = hashlib.sha256(main_os).hexdigest()
    if digest != MAIN_OS_116_SHA256:
        raise ValueError(
            "MAIN OS sha256 %s is not DT2 1.16's (%s)" % (digest, MAIN_OS_116_SHA256)
        )
    container = depack_section(main_os[BUILTIN_PROJECT_ADDR - MAIN_OS_BASE :])
    magic, version = struct.unpack_from(">II", container, 0)
    (end,) = struct.unpack_from(">I", container, V3_END_MARKER)
    if (len(container), magic, version, end) != (
        V3_SIZE,
        CONTAINER_MAGIC,
        3,
        CONTAINER_END_MAGIC,
    ):
        raise ValueError("built-in project is not the expected v3 container")
    return container


def container_refs(container):
    """-> list of (slot, 16-byte ref) for every non-empty slot of a v3
    container's reference table."""
    out = []
    for slot in range(V3_REF_COUNT):
        off = V3_REF_TABLE + slot * 16
        ref = bytes(container[off : off + 16])
        if ref != V3_EMPTY_REF and ref != bytes(16):
            out.append((slot, ref))
    return out


def kit_track_slot(container, kit, track):
    """-> the int16 sample slot of `track` (0-based) in v3 kit `kit`."""
    off = V3_KITS + kit * V3_KIT_SIZE + V3_TRACK0 + track * V3_TRACK_SIZE
    return struct.unpack_from(">h", container, off + V3_TRACK_SLOT)[0]


def point_refs_at(container, ref):
    """Replace every non-empty reference in the slot table with `ref` (16
    bytes), and every other copy of an old reference anywhere in the
    container (each kit track carries one at +0x129). -> (container,
    slots replaced, extra copies replaced)."""
    buf = bytearray(container)
    old = container_refs(buf)
    copies = 0
    for slot, _ in old:
        buf[V3_REF_TABLE + slot * 16 : V3_REF_TABLE + slot * 16 + 16] = ref
    for old_ref in sorted({r for _, r in old}):
        pos = buf.find(old_ref)
        while pos >= 0:
            buf[pos : pos + 16] = ref
            copies += 1
            pos = buf.find(old_ref, pos + 16)
    return bytes(buf), len(old), copies


def build_project_record(main_os, ref, seq=0):
    """-> (record, summary). record: the PROJECT_RECORD_SIZE-byte
    active-project record, COKi header (0x110 bytes, sequence counter `seq`)
    + the built-in v3 container with every sample reference replaced by
    `ref`, zero-padded to the size FUN_400f0628 reads. The v3 container is
    what the factory-reset path itself depacks into the same buffer.
    summary: slots and extra copies replaced, and the active kit's track
    slots."""
    source = builtin_project(main_os)
    container, slots, copies = point_refs_at(source, ref)
    header = build_project_header(
        PROJECT_HEADER_SIZE, seq=seq, fs_version=SUPERBLOCK_VERSION
    )
    record = header + container
    summary = {
        "slots": slots,
        "copies": copies,
        "track_slots": [kit_track_slot(source, ACTIVE_KIT, t) for t in range(16)],
    }
    return record + bytes(PROJECT_RECORD_SIZE - len(record)), summary


class Image:
    """An in-progress (or already-built) +Drive card image, as a sparse file.

    Only the bytes this tool actually writes are ever touched on disk; a
    freshly `truncate()`d file reads as zero everywhere else, matching how
    the real card behaves before anything is written there.
    """

    def __init__(self, path, capacity_blocks=DEFAULT_CAPACITY_BLOCKS):
        self.path = path
        self.capacity_blocks = capacity_blocks
        self._f = open(path, "w+b")  # noqa: SIM115 -- kept open for the object's life
        self._f.truncate(capacity_blocks * SECTOR)

    def close(self):
        self._f.close()

    def write(self, sector, data):
        self._f.seek(sector * SECTOR)
        self._f.write(data)

    def read(self, sector, length):
        self._f.seek(sector * SECTOR)
        data = self._f.read(length)
        if len(data) < length:
            data = data + b"\0" * (length - len(data))
        return data

    def set_bits(self, base_sector, bit_indices):
        """OR the given bit numbers into a bitmap starting at base_sector.

        The firmware's bitmaps are arrays of big-endian u32 words: bit n is
        `1 << (n & 31)` of word `n >> 5` (FUN_4015b93e and FUN_4015b7ac for
        record ids, FUN_40155698 for content pages). Bit 0 is therefore in
        byte 3, not byte 0. A whole 32 KiB page is written so the read side
        sees a page, not a hole.
        """
        top = max(bit_indices) if bit_indices else 0
        length = max(PAGE, ((top >> 5) + 1) * 4)
        buf = bytearray(self.read(base_sector, length))
        for bit in bit_indices:
            off = (bit >> 5) * 4
            (word,) = struct.unpack_from(">I", buf, off)
            struct.pack_into(">I", buf, off, word | (1 << (bit & 31)))
        self.write(base_sector, bytes(buf))

    def write_hash(self, record_id, value):
        """Store `value` (already `| 1`) as record_id's hash-table word."""
        sector = HASH_TABLE_SECTOR + (record_id >> 13) * (PAGE // SECTOR)
        self._f.seek(sector * SECTOR + (record_id & 0x1FFF) * 4)
        self._f.write(struct.pack(">I", value))


def _record_offset(record_id):
    group, slot = divmod(record_id, RECORDS_PER_PAGE)
    return (RECORD_AREA_SECTOR + group * (PAGE // SECTOR)) * SECTOR + slot * RECORD_SIZE


def _write_record(
    img, record_id, attr0, size, parent, extents, hash_word=0, seq=None, links=1
):
    """extents: list of (logical_start_page, length_pages, physical_page).
    hash_word: record +0x0C, `content_hash | 1` for a sample file (bit 0 is
    what FUN_4015ab5c requires), 0 for a directory. seq: record +0x10, a
    sample reference's fourth word must equal it for FUN_4015b178's exact
    match (defaults to the id)."""
    if len(extents) > 8:
        raise ValueError(
            "more than 8 extents needs the indirect-page format (unimplemented)"
        )
    rec = bytearray(RECORD_SIZE)
    rec[0x00] = attr0
    rec[0x01] = DEFAULT_OFFSET01
    struct.pack_into(">H", rec, 0x02, links)  # link count
    struct.pack_into(">I", rec, 0x04, size)
    struct.pack_into(">I", rec, 0x08, parent)
    struct.pack_into(">I", rec, 0x0C, hash_word)
    struct.pack_into(">I", rec, 0x10, record_id if seq is None else seq)
    struct.pack_into(">H", rec, 0x1E, len(extents))
    for i, (logical, length, phys) in enumerate(extents):
        off = 0x20 + i * 12
        struct.pack_into(">III", rec, off, logical, length, phys)
    off = _record_offset(record_id)
    img._f.seek(off)
    img._f.write(bytes(rec))


def _page_offset(page_id):
    return (CONTENT_AREA_SECTOR + page_id * (PAGE // SECTOR)) * SECTOR


def _write_page(img, page_id, data):
    if len(data) > PAGE:
        raise ValueError("page payload exceeds 32 KiB")
    img._f.seek(_page_offset(page_id))
    img._f.write(data)


def load_samples(samples_dir):
    """-> list of (name, native bytes, info) for every .wav directly under
    samples_dir (non-recursive), converted with wav_to_native. The stored
    name drops the .wav extension: the file is no longer a WAV."""
    entries = []
    for name in sorted(os.listdir(samples_dir)):
        if not name.lower().endswith(".wav"):
            continue
        path = os.path.join(samples_dir, name)
        if not os.path.isfile(path):
            continue
        with open(path, "rb") as f:
            native, info = wav_to_native(f.read())
        entries.append((os.path.splitext(name)[0], native, info))
    if not entries:
        raise ValueError("no .wav files found directly under %r" % samples_dir)
    return entries


def build(samples_dir, out_path, capacity_blocks=DEFAULT_CAPACITY_BLOCKS, main_os=None):
    """Build a +Drive card image at out_path from every .wav directly under
    samples_dir. With `main_os` (the DT2 1.16 MAIN OS image bytes) the
    active-project record points every sample reference at the first file;
    without it only the 256-byte COKi header is written, as before.

    -> list of dicts, one per file: name, id, size, hash (record +0x0C),
    seq, pages (first, count), ref (the 16-byte sample reference), info
    (conversion details); plus, on the first, project (slot/copy counts)
    when a project record was written."""
    entries = load_samples(samples_dir)

    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    img = Image(out_path, capacity_blocks)
    try:
        # Region 1: header + empty pool table, so first-boot format
        # detection sees a valid, already-formatted (if sample-only) card.
        header = bytearray(SECTOR)
        struct.pack_into(">I", header, 0x00, HEADER_MAGIC)
        struct.pack_into(">I", header, 0x04, 1)
        header[0x08] = 1
        header[0x09] = 1
        img.write(HEADER_SECTOR, bytes(header))
        img.write(POOL_TABLE_SECTOR, bytes(SECTOR))  # all-zero: nothing allocated

        # Region 2's superblock: without this, FUN_4015a450 (mount) always
        # fails and FileSystemDirectory never reports itself valid -- see
        # docs/findings/07-emulator.md's std::logic_error section.
        img.write(SUPERBLOCK_SECTOR, build_superblock())

        # Unrelated fourth structure: without this, mount (FUN_4015a450, via
        # FUN_4015a124 -> FUN_4002cd6a) fails cleanly even once the
        # superblock and boot-config checks both pass -- see
        # FACTORY_TABLE_SECTOR's comment above.
        img.write(FACTORY_TABLE_SECTOR, build_factory_table_record())

        # Region 2: pages below FIRST_FILE_PAGE belong to the format's own
        # reserved runs (see RESERVED_PAGES). Then the root directory's
        # content page and its three index pages (logical 0x10000-0x10002,
        # contiguous, as FUN_40155ed6 allocates them), then each sample in
        # ceil(size/32KiB) contiguous pages so one inline extent covers it.
        next_page = FIRST_FILE_PAGE
        root_content_page = next_page
        next_page += 1
        root_index_pages = next_page
        next_page += 3

        children = []
        written = []
        next_id = ROOT_ID + 1
        for name, data, info in entries:
            name_bytes = name.encode("ascii", "replace")[:0xFF]
            n_pages = max(1, (len(data) + PAGE - 1) // PAGE)
            start_page = next_page
            next_page += n_pages
            for i in range(n_pages):
                _write_page(img, start_page + i, data[i * PAGE : (i + 1) * PAGE])

            record_id = next_id
            next_id += 1
            hash_word = content_hash(data) | 1
            seq = record_id
            _write_record(
                img,
                record_id,
                ATTR_FILE,
                len(data),
                ROOT_ID,
                [(0, n_pages, start_page)],
                hash_word=hash_word,
                seq=seq,
            )
            img.write_hash(record_id, hash_word)

            children.append((name_bytes, record_id, ATTR_FILE))
            written.append(
                {
                    "name": name,
                    "id": record_id,
                    "size": len(data),
                    "hash": hash_word,
                    "seq": seq,
                    "pages": (start_page, n_pages),
                    "ref": struct.pack(">IIII", record_id, hash_word, len(data), seq),
                    "info": info,
                }
            )

        content, by_hash, listing, by_id = build_directory(ROOT_ID, ROOT_ID, children)
        _write_page(img, root_content_page, content)
        _write_page(img, root_index_pages, by_hash)
        _write_page(img, root_index_pages + 1, listing)
        _write_page(img, root_index_pages + 2, by_id)
        # A directory's size is whole content pages (FUN_40156334 grows it
        # by 0x8000); "." and ".." each add one link to the root.
        _write_record(
            img,
            ROOT_ID,
            ATTR_DIR,
            PAGE,
            ROOT_PARENT,
            [(0, 1, root_content_page), (LOGICAL_HASH_PAGE, 3, root_index_pages)],
            links=2,
        )

        img.set_bits(
            ID_BITMAP_SECTOR,
            list(RESERVED_IDS) + [ROOT_ID] + [w["id"] for w in written],
        )
        img.set_bits(PAGE_BITMAP_SECTOR, list(range(next_page)))

        # The COKi header (and, with main_os, the active project) at
        # 0x40000/0x48000. Without a valid header every boot falls into
        # FUN_400f0628's ~13.85 MiB internal-flash "repair" path and the boot
        # task pends forever -- see BOOT_CONFIG_SECTOR's comment. The
        # header's sequence counter continues after the ids used here.
        if main_os is not None:
            record, written[0]["project"] = build_project_record(
                main_os, written[0]["ref"], seq=next_id
            )
        else:
            record = build_boot_config_record()
        img.write(BOOT_CONFIG_SECTOR, record)
        img.write(BOOT_CONFIG_SECTOR_2, record)
    finally:
        img.close()

    return written


def _read_record(img, record_id):
    group, slot = divmod(record_id, RECORDS_PER_PAGE)
    sector = RECORD_AREA_SECTOR + group * (PAGE // SECTOR)
    page = img.read(sector, PAGE)
    rec = page[slot * RECORD_SIZE : slot * RECORD_SIZE + RECORD_SIZE]
    attr0 = rec[0]
    size = struct.unpack(">I", rec[4:8])[0]
    parent = struct.unpack(">I", rec[8:12])[0]
    n_extents = struct.unpack(">H", rec[0x1E:0x20])[0]
    extents = []
    for i in range(min(n_extents, 8)):
        off = 0x20 + i * 12
        extents.append(struct.unpack(">III", rec[off : off + 12]))
    return {
        "id": record_id,
        "attr0": attr0,
        "size": size,
        "parent": parent,
        "extents": extents,
    }


def _read_page(img, page_id):
    return img.read(_page_offset(page_id) // SECTOR, PAGE)


def ls(image_path):
    """-> list of dicts, one per entry directly under the root directory.

    Reads back exactly the layout `build` writes: the root's own record for
    its content-page extent, then the variable-length directory-entry list
    on that page. Works as a generic reader for any image in this format,
    not just ones this tool wrote -- e.g. a real firmware card overlay,
    once it holds sample files under `/`.
    """
    with open(image_path, "rb") as f:
        img = _ReadOnlyImage(f)
        root = _read_record(img, ROOT_ID)
        content_extents = [e for e in root["extents"] if e[0] < 0x10000]
        if not content_extents:
            return []
        _, length, phys = content_extents[0]
        content = b"".join(_read_page(img, phys + i) for i in range(length))
        content = content[: root["size"]]

        out = []
        pos = 0
        while pos < len(content):
            record_id = struct.unpack(">I", content[pos : pos + 4])[0]
            if record_id == 0:
                break
            slot_len = struct.unpack(">H", content[pos + 4 : pos + 6])[0]
            name_len = content[pos + 6]
            attr = content[pos + 7]
            name = content[pos + 8 : pos + 8 + name_len].decode("ascii", "replace")
            if name in (".", ".."):
                pos += slot_len
                continue
            rec = _read_record(img, record_id)
            out.append(
                {
                    "id": record_id,
                    "name": name,
                    "type": attr,
                    "size": rec["size"],
                }
            )
            pos += slot_len
        return out


class _ReadOnlyImage:
    """Just enough of Image's read() to share ls()/build() sector math."""

    def __init__(self, f):
        self._f = f

    def read(self, sector, length):
        self._f.seek(sector * SECTOR)
        data = self._f.read(length)
        if len(data) < length:
            data = data + b"\0" * (length - len(data))
        return data


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build", help="build a +Drive image from a folder of WAVs")
    b.add_argument("samples_dir")
    b.add_argument("-o", "--out", required=True)
    b.add_argument(
        "--capacity-blocks", type=lambda s: int(s, 0), default=DEFAULT_CAPACITY_BLOCKS
    )
    b.add_argument(
        "--main-os",
        default=DEFAULT_MAIN_OS,
        help="DT2 1.16 MAIN OS image to take the built-in project from "
        "(default: %(default)s)",
    )
    b.add_argument(
        "--no-project",
        action="store_true",
        help="write only the COKi header, no project (the old behaviour)",
    )

    lsp = sub.add_parser("ls", help="list a +Drive image's root directory")
    lsp.add_argument("image")

    args = p.parse_args(argv)
    if args.cmd == "build":
        main_os = None
        if not args.no_project:
            if not os.path.exists(args.main_os):
                p.error(
                    "no MAIN OS image at %s: extract DT2 1.16 with "
                    "`uv run python -m emu.extract SYX -o out/sections/dt2-1.16`, "
                    "pass --main-os, or build without a project (--no-project)"
                    % args.main_os
                )
            with open(args.main_os, "rb") as f:
                main_os = f.read()
        entries = build(args.samples_dir, args.out, args.capacity_blocks, main_os)
        print(
            "wrote %s (%d bytes logical) with %d sample(s):"
            % (args.out, args.capacity_blocks * SECTOR, len(entries))
        )
        for e in entries:
            info = e["info"]
            print(
                "  %-24s id=%d %d bytes, hash|1=%#010x, pages %#x+%d, "
                "%d ch %d Hz %d frames -> 48000 Hz %d frames"
                % (
                    e["name"],
                    e["id"],
                    e["size"],
                    e["hash"],
                    e["pages"][0],
                    e["pages"][1],
                    info["channels"],
                    info["src_rate"],
                    info["src_frames"],
                    info["frames"],
                )
            )
        if main_os is not None:
            proj = entries[0]["project"]
            print(
                "project record at %#x/%#x: %d slot references and %d track "
                "copies -> %s (%s); active kit %d track slots %s"
                % (
                    BOOT_CONFIG_SECTOR,
                    BOOT_CONFIG_SECTOR_2,
                    proj["slots"],
                    proj["copies"],
                    entries[0]["name"],
                    entries[0]["ref"].hex(),
                    ACTIVE_KIT,
                    proj["track_slots"],
                )
            )
    elif args.cmd == "ls":
        entries = ls(args.image)
        if not entries:
            print("(root directory is empty or unreadable)")
        for e in entries:
            kind = "dir " if e["type"] == ATTR_DIR else "file"
            print("id=%-6d %s %-32s %d bytes" % (e["id"], kind, e["name"], e["size"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
