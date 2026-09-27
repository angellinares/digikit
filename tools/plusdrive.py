#!/usr/bin/env python3
"""Build and read a +Drive card image, in the firmware's own on-disk format.

The format is reverse engineered in docs/findings/14-plus-drive-format.md.
Short version: +Drive has two independent regions at fixed, hard-coded
absolute sector numbers -- the ``MmcFs`` project/sound/kit pools (untouched
here) and a separate, real, path-addressable filesystem (128-byte metadata
records, extent-mapped 32 KiB content pages, hierarchical directories) that
sample files live in. This tool only writes the second region: a header
sector, an empty pool table, a root directory, and one file entry per input
WAV, all under the root.

    uv run python tools/plusdrive.py build samples/ -o out/plusdrive/dt2.img
    uv run python tools/plusdrive.py ls out/plusdrive/dt2.img

``build`` also doubles as the only writer of this format, and ``ls`` as a
reader for any card image in it (including one dumped from a real firmware
card overlay), since both share the same decode/encode tables below.

To boot the emulator from a built image, pass its path as ``card_image=`` to
``emu.dspboot.run``/``emu.longrun.build`` (or ``--card-image`` on the CLI
runners that expose it); both construct the card via
``emu.esdhc.Card.from_file``, which mmaps the file read-only -- writes during
emulation go to the in-RAM overlay (already how snapshots capture +Drive
writes; see ``Esdhc.checkpoint_state``), and this file is never modified.

Open questions the finding doc flags, most relevant to this tool:

* The exact attribute-byte values at record offsets 0x00/0x01/0x0c for
  "regular file" vs "directory" are inferred, not proven (see ATTR_* below).
* Sample payload bytes are stored verbatim (whole input WAV file, including
  its RIFF header) -- this is the only choice consistent with the proven
  "write path does no interpretation" finding, not a confirmed fact about
  what the browser/loader expects to unpack.
* The 0x10000 (name-hash) and 0x10002 (id-sorted) auxiliary index pages are
  deliberately NOT written -- believed unnecessary for on-device browsing
  and loading (as opposed to by-path MIDI RPC access), unconfirmed.

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

# Region 2 (the real filesystem): absolute sector numbers, hard-coded in the
# firmware, not derived from card capacity.
ID_BITMAP_SECTOR = 0x5D8040
PAGE_BITMAP_SECTOR = 0x5D80C0
PAGE_BITMAP_SECTOR_END = 0x5D8180
RECORD_AREA_SECTOR = 0x5D8180
CONTENT_AREA_SECTOR = 0x5EE180

# Region 2's superblock: FUN_4015a450 (mount) and FUN_4015a164 (format
# writer), see docs/findings/14-plus-drive-format.md. Sector-relative field
# values (0x14/0x18/0x1c/0x20) are each `<region-start-sector> - SUPERBLOCK_SECTOR`,
# confirmed by exact arithmetic against the sector constants above. Fields
# 0x2c/0x30 are two runtime-allocated physical sectors (each the start of a
# 44-page run reserved by FUN_40155698(0x2c,...), called twice); their
# consumer, FUN_4015ae12 (run at both format- and mount-time), scans exactly
# the sector range [0x5ee980, 0x5eff80) -- 88 pages, i.e. two consecutive
# 44-page runs starting at 0x5ee980 and 0x5ef480 -- so those are the literal
# values, not free choices; nothing here has confirmed what that scan is for
# (an RAM-resident record/page cache seed, per finding 14's open questions).
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

# Reserved logical pages inside a directory's own extent list.
LOGICAL_INDEX_PAGE = 0x10001  # readdir's "jump to entry N" index; required
# LOGICAL_HASH_PAGE = 0x10000   # by-name binary search; not written here
# LOGICAL_ID_PAGE   = 0x10002   # id-sorted index; not written here

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
    buf = bytearray(SECTOR // 2)  # 256 bytes; FUN_400f0628 reads exactly this
    struct.pack_into(">I", buf, 0x00, BOOT_CONFIG_MAGIC)
    struct.pack_into(">I", buf, 0x0C, 0)  # length field, must be < 0xF1
    checksum = (1 ^ 0) + (2 ^ 0)  # words at offsets 8 and 12, both 0
    struct.pack_into(">I", buf, 0x04, checksum)
    return bytes(buf)


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

        All the bit numbers this tool ever sets are tiny (single digits),
        so they land in the first word of the first page; a real 32 KiB
        bitmap page is still allocated (zero-filled by the sparse-file
        default) so the read side sees a well-formed page, not a hole.
        """
        page = bytearray(self.read(base_sector, PAGE))
        for bit in bit_indices:
            page[bit >> 3] |= 1 << (bit & 7)
        self.write(base_sector, bytes(page))


def _record_offset(record_id):
    group, slot = divmod(record_id, RECORDS_PER_PAGE)
    return (RECORD_AREA_SECTOR + group * (PAGE // SECTOR)) * SECTOR + slot * RECORD_SIZE


def _write_record(img, record_id, attr0, size, parent, extents):
    """extents: list of (logical_start_page, length_pages, physical_page)."""
    if len(extents) > 8:
        raise ValueError(
            "more than 8 extents needs the indirect-page format (unimplemented)"
        )
    rec = bytearray(RECORD_SIZE)
    rec[0x00] = attr0
    rec[0x01] = DEFAULT_OFFSET01
    struct.pack_into(">H", rec, 0x02, 1)  # link count
    struct.pack_into(">I", rec, 0x04, size)
    struct.pack_into(">I", rec, 0x08, parent)
    struct.pack_into(">I", rec, 0x0C, 0)
    struct.pack_into(">I", rec, 0x10, record_id)  # sequence number, any unique value
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


def build(samples_dir, out_path, capacity_blocks=DEFAULT_CAPACITY_BLOCKS):
    """Build a +Drive card image at out_path from every file directly under
    samples_dir (non-recursive; anything other than .wav is skipped with a
    warning). -> list of (name, size) written."""
    entries = []
    for name in sorted(os.listdir(samples_dir)):
        if not name.lower().endswith(".wav"):
            continue
        path = os.path.join(samples_dir, name)
        if not os.path.isfile(path):
            continue
        with open(path, "rb") as f:
            data = f.read()
        entries.append((name, data))
    if not entries:
        raise ValueError("no .wav files found directly under %r" % samples_dir)

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

        # Unrelated third structure: without this, every boot (regardless of
        # +Drive/mount content) falls into FUN_400f0628's ~13.85 MiB
        # internal-flash "repair" path and the boot task ends up pending
        # forever on an unposted semaphore -- see BOOT_CONFIG_SECTOR's
        # comment and docs/findings/14-plus-drive-format.md.
        boot_config = build_boot_config_record()
        img.write(BOOT_CONFIG_SECTOR, boot_config)
        img.write(BOOT_CONFIG_SECTOR_2, boot_config)

        # Region 2: allocate physical pages up front -- page 0 is the root
        # directory's content (the child listing), page 1 is its 0x10001
        # enumeration index, then each sample gets ceil(size/32KiB)
        # contiguous pages so a single inline extent covers it.
        next_page = 0
        root_content_page = next_page
        next_page += 1
        root_index_page = next_page
        next_page += 1

        dir_content = bytearray()
        file_records = []  # (id, name, size)
        next_id = ROOT_ID + 1
        for name, data in entries:
            name_bytes = name.encode("ascii", "replace")
            if len(name_bytes) > 0xFF:
                name_bytes = name_bytes[:0xFF]
            n_pages = max(1, (len(data) + PAGE - 1) // PAGE)
            start_page = next_page
            next_page += n_pages
            for i in range(n_pages):
                chunk = data[i * PAGE : (i + 1) * PAGE]
                _write_page(img, start_page + i, chunk)

            record_id = next_id
            next_id += 1
            _write_record(
                img,
                record_id,
                ATTR_FILE,
                len(data),
                ROOT_ID,
                [(0, n_pages, start_page)],
            )

            slot_len = 8 + len(name_bytes)
            entry = bytearray(slot_len)
            struct.pack_into(">I", entry, 0x00, record_id)
            struct.pack_into(">H", entry, 0x04, slot_len)
            entry[0x06] = len(name_bytes)
            entry[0x07] = ATTR_FILE
            entry[0x08 : 0x08 + len(name_bytes)] = name_bytes
            file_records.append((record_id, name, len(data), len(dir_content)))
            dir_content += entry

        _write_page(img, root_content_page, bytes(dir_content))

        index_page = bytearray(6 + 4 * len(file_records))
        struct.pack_into(">H", index_page, 0x00, len(file_records))
        for i, (_, _, _, byte_offset) in enumerate(file_records):
            if byte_offset >= PAGE:
                raise ValueError(
                    "root directory content exceeds one 32 KiB page (unimplemented)"
                )
            packed = byte_offset & 0x7FFF  # page field 0 (all entries on page 0)
            struct.pack_into(">I", index_page, 6 + i * 4, packed)
        _write_page(img, root_index_page, bytes(index_page))

        _write_record(
            img,
            ROOT_ID,
            ATTR_DIR,
            len(dir_content),
            ROOT_PARENT,
            [(0, 1, root_content_page), (LOGICAL_INDEX_PAGE, 1, root_index_page)],
        )

        img.set_bits(
            ID_BITMAP_SECTOR, [ROOT_ID] + [rid for rid, _, _, _ in file_records]
        )
        img.set_bits(PAGE_BITMAP_SECTOR, list(range(next_page)))
    finally:
        img.close()

    return [(name, len(data)) for name, data in entries]


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

    lsp = sub.add_parser("ls", help="list a +Drive image's root directory")
    lsp.add_argument("image")

    args = p.parse_args(argv)
    if args.cmd == "build":
        entries = build(args.samples_dir, args.out, args.capacity_blocks)
        print(
            "wrote %s (%d bytes logical) with %d sample(s):"
            % (args.out, args.capacity_blocks * SECTOR, len(entries))
        )
        for name, size in entries:
            print("  %-32s %d bytes" % (name, size))
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
