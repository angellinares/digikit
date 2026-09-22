#!/usr/bin/env python3
"""Find code caves in a MAIN OS image: dead bytes that survive cold boot.

A "cave" here means a run of identical filler bytes (0x00 or 0xFF) that (a)
holds no firmware code or data -- nothing in the image or at runtime treats
it as meaningful -- and (b) is still there, unmodified, after the device has
booted. Both properties matter for a patch: a byte range that merely LOOKS
free in the static image but gets overwritten by the reset path, or that is
secretly the tail of a referenced table, is not free at all.

Known-good example (Digitakt II 1.16): 0x403117c4-0x40312000, 2108 bytes,
verified by hand before this tool existed. This tool reproduces that answer
from the image bytes alone, and extends it to Digitakt II 1.15C and
Digitone II 1.11 without hardcoding any of their addresses.

Method, in the order the pipeline runs:

  1. BOOT MAP. The reset handler runs two routines, byte-identical in all
     three images tested (only four embedded absolute addresses differ,
     found at fixed offsets read off one disassembly -- COPY_OFF/CLEAR_OFF):
     one copies a tail of the image out to the on-chip SRAM at 0x80000000,
     the other zero-fills a range of SDRAM starting where that copy source
     begins. Both are found by a masked byte signature -- the four operand
     offsets are wildcarded, everything else must match literally -- not by
     fixed address, so a fourth image whose linker laid things out
     differently still resolves as long as the routines themselves are
     unchanged (see `_mask_at_offsets`, which explains why this masks by
     KNOWN offset rather than by auto-detecting embedded addresses the way
     `emu/symbols.py`'s `Sig` does -- that auto-detection has a real false-
     positive hazard that bites on exactly this signature). The region that
     survives cold boot is [load_addr, clear_start) minus whatever the copy
     routine reads its source from (in every image tested the copy source
     and the clear range start at the same address, so this is usually a
     no-op, but the subtraction is general).

  2. CACHE/MEMORY-MAP CHECK. The same reset path (fixed address in all three
     images: 0x40000554) loads two immediates into d0 and MOVECs them into
     CACR and ACR0. Found generically by scanning for MOVEC-to opcodes
     (4E7B) whose control-register field selects CACR (0x002) or an ACRn
     (0x004-0x007) -- see `find_control_writes` -- not by assuming this
     exact address, so a build that sets more ACRs (ACR1-3) is still
     reported completely. The values are decoded against the MCF5441x
     reference manual (`out/refs/MCF5441XRM/`) and cited by page. See
     `decode_cacr`/`decode_acrn` and `cache_report`.

  3. CANDIDATES. Runs of a single repeated byte (0x00 or 0xFF) of at least
     `--min` bytes, wholly inside the survives-boot region, start rounded up
     to a 4-byte boundary.

  4. EXCLUSIONS, each candidate independently:
       (a)/(c) POINTER LITERAL SCAN. Any 32-bit big-endian value anywhere in
           the image that falls inside [run_start - 64, run_end) --
           `_range_regex` compiles a regex that matches exactly the
           addresses in that window, so this is one linear pass over the
           image per candidate (milliseconds, not a disassembly). It catches
           both a literal absolute-address operand (lea.l/pea/move.l #imm --
           these ARE 32-bit big-endian values in the instruction stream) and
           a plain data-table pointer. It does NOT catch a PC-relative
           lea/pea, which encodes a displacement, not the target address, as
           a literal; `--deep-refs` adds a `tools/refscan.py` disassembly
           pass (several seconds, off by default) that also catches those.
           The -64 lookback is because a referenced object starting just
           before the run can extend into it (see `apply_exclusions`).
       (b) GHIDRA. When `--ghidra DIR` is given and DIR/manifest.json's
           image_sha256 matches this image, any `function_ranges` row
           overlapping the run rejects/trims it (Ghidra thinks real code is
           there), and any `data_refs`/`calls` row landing in
           [run_start-64, run_end) is treated the same as a pointer-literal
           hit. Optional: the tool works without it and says so.
     Any hit -- whether in the 64-byte lookback before the run, right at
     its start, or partway through it -- can only be trimmed down to a
     surviving remainder if something gives an EXACT bound for the object
     doing the referencing; only Ghidra's `function_ranges` provides that.
     A hit found by the pointer-literal scan alone, with no such bound,
     rejects the whole run, even if the hit looks like it only touches an
     edge: without a bound, the referencing object could start anywhere at
     or before the hit.

  5. RUNTIME EVIDENCE. For each surviving candidate, its bytes are compared
     against every snapshot rung available for this image (see
     `emu/snapshot.py`'s pickle+zlib page format, read directly here -- no
     Unicorn needed just to check bytes). Any rung where they differ rejects
     the candidate: something writes it at runtime, static analysis or not.

  6. CONFIRMATION (`--confirm SYX`, Digitakt II 1.16 only. off by default).
     Writes a distinct canary word into every surviving candidate in a copy
     of the MAIN OS section, rebuilds a .syx with `dt2.build.rebuild`,
     extracts it, boots it fresh to 60,000,000 instructions via
     `emu.checkpoint.make`, and checks every canary word is still there in
     that snapshot. This is the only step that runs the emulator.

Usage:
    uv run python tools/cavefind.py out/sections/dt2-1.16/section_3_MAIN_OS.bin
    uv run python tools/cavefind.py IMAGE --ghidra out/ghidra/dt2-1.16-emac
    uv run python tools/cavefind.py IMAGE --json out.json
    uv run python tools/cavefind.py IMAGE --confirm Digitakt_II_OS1.16.syx
"""
import argparse
import hashlib
import json
import os
import pickle
import re
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

LOAD_ADDR = 0x40000400   # emu/symbols.py LOAD_ADDR, emu/dspboot.py MAIN_LOAD
DATA_HI = 0x48000000     # top of the SDRAM window covered by ACR0 (see cache_report)
PAGE = 0x100000          # emu/harness.py PAGE -- snapshot page size

# --------------------------------------------------------------------------
# 1. Boot map: the reset-path copy and clear routines, found by masked
# signature (any embedded address INTO the loaded image is wildcarded, same
# idea as emu/symbols.py's Sig). Captured from Digitakt II 1.16 at
# 0x4000045c/0x400004b2; verified byte-identical (mask aside) at those same
# addresses in Digitakt II 1.15C and Digitone II 1.11, but matched by
# content here, not by address -- a fourth image with a different layout
# still resolves as long as the routines are unchanged.
#
# COPY_HEX: move.l a2,-(a7); lea SRC1,a2; lea 0x80000000,a1; { move.l
# (a2)+,(a1); lea 4(a1),a0; cmpa.l MID,a2; bcc; movea a0,a1; bra } clr loop
# to DST1_END; lea MID,a2; lea DST1_END,a1; { same copy loop to SRC_END };
# clr loop to DST2_END; movea (a7)+,a2; rts.
COPY_HEX = (
    '2f0a45f94031200043f98000000041e90004229ab5fc40318e80'
    '6404224860ee4298b1fc8000800065f645f940318e8043f98000'
    '8000229a41e90004b5fc4031ff606404224860ee4298b1fc8001'
    '000065f6245f4e75'
)
COPY_OFF = {
    'copy1_start': 4, 'dest1': 10, 'copy1_end': 22, 'dest_bound1': 36,
    'copy2_start': 44, 'dest2': 50, 'copy2_end': 62, 'dest_bound2': 76,
}
# Only these four move between builds (the SDRAM source addresses); the four
# SRAM destination constants (0x80000000/0x80008000/0x80010000, offsets
# dest1/dest2/dest_bound1/dest_bound2) are fixed by the hardware and are left
# unmasked, which only strengthens the match.
COPY_MASK = ('copy1_start', 'copy1_end', 'copy2_start', 'copy2_end')

# CLEAR_HEX: lea -0x10(a7),a7; movem.l d4-d7,(a7); movea CLEAR_START,a0;
# move.l CLEAR_END,d1; { clr d4..d7; movem.l d4-d7,(a0); lea 0x10(a0),a0;
# dbra-style loop }; movem.l (a7),d4-d7; lea 0x10(a7),a7; rts.
CLEAR_HEX = (
    '4feffff048d700f0207c40312000223c47e284709288e88142844285'
    '4286428748d000f041e80010538166f44cd700f04fef00104e75'
)
CLEAR_OFF = {'clear_start': 10, 'clear_end': 16}
CLEAR_MASK = ('clear_start', 'clear_end')


def _mask_at_offsets(raw, off_map, names, width=4):
    """-> compiled regex matching `raw` literally except for `width`-byte
    windows at off_map[name] for each name in `names`, which match anything.

    Earlier this masked windows by CONTENT (any embedded value that looks
    like an address into the loaded image, same idea as emu/symbols.py's
    Sig), scanning `raw` byte-by-byte. That auto-detection has a real
    hazard the Sig docstring itself calls out: an opcode's own bytes can
    combine with the top half of a following operand to look like a
    different, unrelated address and get masked instead -- `45f9`
    (lea.l abs.l) directly followed by an operand starting `40` or `41`..
    `47` triggers exactly this here (`45f9 4031...` decodes as the
    in-range address 0x45f94031), shifting the whole mask and breaking the
    match on any image whose real operand differs. Since the exact operand
    offsets are already known (COPY_OFF/CLEAR_OFF, read off the disassembly
    once), masking those offsets directly sidesteps the hazard rather than
    working around it."""
    mask = bytearray(len(raw))
    for name in names:
        off = off_map[name]
        for k in range(width):
            mask[off + k] = 1
    pattern = b''.join(b'.' if m else re.escape(bytes([b]))
                        for b, m in zip(raw, mask))
    return re.compile(pattern, re.DOTALL)


def _find_signature(img, hexstr, off_map, mask_names, what):
    """-> byte offset of the unique masked-signature match, or raise."""
    raw = bytes.fromhex(hexstr)
    hits = [m.start() for m in _mask_at_offsets(raw, off_map, mask_names).finditer(img)]
    if len(hits) != 1:
        raise SystemExit(
            '%s: %d match(es) for the %s signature (%d bytes), need exactly '
            '1 -- this image'"'"'s reset path may differ; find its equivalent '
            'by hand and extend the signature, or report why not.'
            % (what, len(hits), what, len(raw)))
    return hits[0]


def _op32(img, off):
    return struct.unpack_from('>I', img, off)[0]


def boot_map(img, load_addr=LOAD_ADDR):
    """-> dict describing the reset path's copy/clear ranges and the region
    that survives a cold boot. Raises SystemExit (not caught) if either
    signature fails to resolve uniquely -- see _find_signature."""
    copy_off = _find_signature(img, COPY_HEX, COPY_OFF, COPY_MASK, 'copy routine')
    clear_off = _find_signature(img, CLEAR_HEX, CLEAR_OFF, CLEAR_MASK, 'clear routine')

    copy1_start = _op32(img, copy_off + COPY_OFF['copy1_start'])
    copy1_end = _op32(img, copy_off + COPY_OFF['copy1_end'])
    copy2_start = _op32(img, copy_off + COPY_OFF['copy2_start'])
    copy2_end = _op32(img, copy_off + COPY_OFF['copy2_end'])
    clear_start = _op32(img, clear_off + CLEAR_OFF['clear_start'])
    clear_end = _op32(img, clear_off + CLEAR_OFF['clear_end'])

    if copy1_end != copy2_start:
        raise SystemExit(
            'copy routine: first block ends at 0x%08x but second starts at '
            '0x%08x -- expected them to be contiguous' % (copy1_end, copy2_start))

    copy_ranges = [(copy1_start, copy2_end)]
    survive_start, survive_end = load_addr, clear_start
    for c_lo, c_hi in copy_ranges:
        lo, hi = max(c_lo, survive_start), min(c_hi, survive_end)
        if lo < hi:
            # The copy source overlaps the front of the survives-boot
            # region: only report a single contiguous survivor when the
            # overlap is a prefix or suffix, matching every image observed
            # (copy source starts exactly at clear_start, i.e. no overlap
            # at all in practice -- this branch exists for a build where it
            # does overlap, and is conservative: it keeps whichever side is
            # non-empty and reports the other as excluded).
            if lo == survive_start:
                survive_start = hi
            elif hi == survive_end:
                survive_end = lo
            # else: the copy source is a hole in the middle; candidates()
            # only ever looks inside [survive_start, survive_end), so such a
            # hole would need a real interval-list, not a pair. Not needed
            # by any image tested (see boot map facts in the module test).

    return {
        'copy_addr': load_addr + copy_off,
        'clear_addr': load_addr + clear_off,
        'copy_ranges': copy_ranges,
        'clear_start': clear_start,
        'clear_end': clear_end,
        'survive_start': survive_start,
        'survive_end': survive_end,
    }


# --------------------------------------------------------------------------
# 2. CACR/ACRn: found generically by scanning for MOVEC-to (4E7B) opcodes
# whose Cr field selects CACR or an ACRn, not by fixed address. Capstone
# (used by dt2.coldfire.disasm/tools/refscan.py) labels this Cr encoding
# with its classic-68040 name (e.g. "itt0" for 0x004) because it was built
# for 680x0, not ColdFire; the MCF5441x reference manual assigns 0x004-0x007
# to ACR0-ACR3 (see the citations in cache_report), so this module reports
# the ColdFire name, not Capstone's.
# --------------------------------------------------------------------------

_CR_NAMES = {0x002: 'CACR', 0x004: 'ACR0', 0x005: 'ACR1', 0x006: 'ACR2', 0x007: 'ACR3'}


def find_control_writes(img, load_addr=LOAD_ADDR):
    """-> [{'addr','reg','value','value_addr'}] for every MOVEC-to CACR/ACRn
    in the image. `value` is the immediate loaded into the source register by
    an immediately preceding `move.l #imm32,Dn` (the only pattern seen in any
    of the three images), else None -- the write is still reported, just
    without a decoded value."""
    out = []
    i = 0
    while True:
        i = img.find(b'\x4e\x7b', i)
        if i < 0 or i + 4 > len(img):
            break
        ext = struct.unpack_from('>H', img, i + 2)[0]
        cr = ext & 0xFFF
        if cr in _CR_NAMES:
            rm, reg = (ext >> 15) & 1, (ext >> 12) & 7
            value = value_addr = None
            if rm == 0 and i >= 6 and img[i - 6:i - 4] == bytes([0x20 | reg, 0x3c]):
                value = _op32(img, i - 4)
                value_addr = load_addr + i - 6
            out.append({
                'addr': load_addr + i, 'reg': _CR_NAMES[cr], 'value': value,
                'value_addr': value_addr, 'src': ('d%d' if rm == 0 else 'a%d') % reg,
            })
        i += 2
    return out


# CACR fields, MCF5441XRM p.167 Fig 6-4 (layout) and pp.168-170 Table 6-3.
_CACR_BITS = (
    (31, 'DEC', 'data cache enabled'), (30, 'DW', 'data write-protected'),
    (29, 'DESB', 'data store buffer enabled'), (28, 'DDPI', 'CPUSHL no-clear (data)'),
    (27, 'DHLCK', 'data half-cache lock'),
    (24, 'DCINVA', 'data cache invalidate-all (self-clearing)'),
    (23, 'DDSP', 'data default supervisor-protect'),
    (19, 'BEC', 'branch cache enabled'),
    (18, 'BCINVA', 'branch cache invalidate-all (self-clearing)'),
    (15, 'IEC', 'instruction cache enabled'), (14, 'SPA', 'search by physical address'),
    (13, 'DNFB', 'cache-inhibited fill buffer enabled'),
    (12, 'IDPI', 'CPUSHL no-clear (instruction)'), (11, 'IHLCK', 'instruction half-cache lock'),
    (10, 'IDCM', 'instruction default cache mode = cache-inhibited'),
    (8, 'ICINVA', 'instruction cache invalidate-all (self-clearing)'),
    (7, 'IDSP', 'instruction default supervisor-protect'),
    (5, 'EUSP', 'separate user/supervisor stack pointers'),
)
_CACR_DDCM = {0: 'cacheable write-through', 1: 'cacheable copyback',
              2: 'cache-inhibited precise', 3: 'cache-inhibited imprecise'}
_ACR_CM = {0: 'cacheable, write-through', 1: 'cacheable, copyback',
           2: 'cache-inhibited, precise', 3: 'cache-inhibited, imprecise'}
_ACR_S = {0: 'user mode only', 1: 'supervisor mode only',
          2: 'all accesses', 3: 'all accesses'}


def decode_cacr(value):
    """-> dict of CACR bit meanings. MCF5441XRM p.167 Fig 6-4 / pp.168-170
    Table 6-3."""
    out = {name: bool(value & (1 << bit)) for bit, name, _ in _CACR_BITS}
    out['DDCM'] = _CACR_DDCM[(value >> 25) & 0x3]
    return out


def decode_acrn(value):
    """-> dict of ACRn bit meanings. MCF5441XRM p.170-171 Fig 6-5 / Table 6-4.

    BA/ADMSK together give a base and mask on address bits [31:24]; the
    region is every address whose top byte, with the masked bits ignored,
    equals BA with those same bits cleared. AMM=0 means the mask covers
    16 MB-or-larger regions (each set ADMSK bit doubles the region size)."""
    ba = (value >> 24) & 0xFF
    admsk = (value >> 16) & 0xFF
    mask_bits = bin(admsk & 0xFF).count('1')  # low-order bits of ADMSK used per AMM=0
    region_size = 0x1000000 << mask_bits
    region_lo = (ba & ~admsk & 0xFF) << 24
    return {
        'BA': ba, 'ADMSK': admsk,
        'region': (region_lo, region_lo + region_size),
        'E': bool(value & (1 << 15)),
        'S': _ACR_S[(value >> 13) & 0x3],
        'AMM_1_16MB_or_more': not bool(value & (1 << 10)),
        'CM': _ACR_CM[(value >> 5) & 0x3],
        'SP_supervisor_only': bool(value & (1 << 3)),
        'W_write_protected': bool(value & (1 << 2)),
    }


CACHE_CITATIONS = (
    'MCF5441x Reference Manual, out/refs/MCF5441XRM/:',
    '  p.56   Table 1-2, System Memory Map: 0x40000000-0x7FFFFFFF is the '
    'SDRAM controller (serial-boot mode); 0x80000000-0x8FFFFFFF is the '
    'internal-SRAM backdoor.',
    '  p.192  Sec 7.2.1, RAMBAR: the 64 KB on-chip SRAM is placed at any '
    '0-mod-64K address in 0x80000000-0x8FFF0000; any address in '
    '0x80000000-0x8FFFFFFF aliases into it.',
    '  p.165-166  Sec 6.2.2, "The Cache at Start-Up": reset does not '
    'invalidate cache lines; CACR[DCINVA,ICINVA] must be set before the '
    'cache is enabled.',
    '  p.167  Sec 6.3.1 / Fig 6-4, Cache Control Register (CACR) layout.',
    '  p.168-170  Table 6-3, CACR field descriptions.',
    '  p.170-171  Sec 6.3.2 / Fig 6-5 + Table 6-4, Access Control Register '
    '(ACRn) layout and field descriptions.',
)


def cache_report(img, load_addr=LOAD_ADDR):
    """-> dict: every CACR/ACRn write found, decoded, with a verdict on
    whether the survives-boot region is SDRAM, cacheable, executable and not
    write-protected, plus whether a post-patch cache flush is needed for a
    FLASHED (not live-patched) cave."""
    writes = find_control_writes(img, load_addr)
    decoded = []
    for w in writes:
        d = dict(w)
        if w['value'] is not None:
            d['decoded'] = (decode_cacr(w['value']) if w['reg'] == 'CACR'
                            else decode_acrn(w['value']))
        decoded.append(d)

    acr0 = next((w for w in decoded if w['reg'] == 'ACR0' and w['value'] is not None), None)
    cacr = next((w for w in decoded if w['reg'] == 'CACR' and w['value'] is not None), None)
    verdict = {}
    if acr0:
        lo, hi = acr0['decoded']['region']
        verdict['sdram_region'] = (lo, hi)
        verdict['cacheable'] = 'cacheable' in acr0['decoded']['CM']
        verdict['write_protected'] = acr0['decoded']['W_write_protected']
        # ACR0/1/4/5 govern DATA accesses only (p.170); there is no separate
        # instruction ACR here (no ACR2/3 write found), so instruction
        # fetches from this region fall back to CACR's default instruction
        # mode. There is no execute-protect bit anywhere in the ColdFire
        # ACR/CACR model, so "executable" only depends on whether an
        # instruction fetch is even permitted here at all, which it is.
        verdict['executable_via_cacr_default'] = (
            cacr is not None and cacr['decoded']['IDCM'] == 'cacheable write-through'
            if cacr and cacr['decoded'].get('IDCM') else None)
    # Whether flashed-image cave bytes need a cache flush: they are written
    # to SDRAM before this code runs (the reset handler executing THIS
    # instruction is itself already resident in SDRAM), and the very same
    # CACR write that turns caching on also sets DCINVA/ICINVA/BCINVA (all
    # self-clearing invalidate-everything bits) in the SAME 32-bit write --
    # so the cache starts cold at the moment it is enabled. A byte baked
    # into the flashed image is therefore correctly seen on first fetch; no
    # flush is needed. This does NOT apply to a LIVE patch written after
    # boot with the cache already warm and possibly holding stale lines.
    verdict['flash_patch_needs_cache_flush'] = False
    verdict['flash_patch_flush_reasoning'] = (
        'CACR is written once, at %s, and that single write both enables '
        'the caches (DEC/IEC/BEC) and invalidates them (DCINVA/ICINVA/'
        'BCINVA) -- p.165-166. A byte flashed into the image is in SDRAM '
        'before this instruction executes (this code is itself running from '
        'SDRAM), so the cache comes up cold and correct. A patch applied '
        'AFTER boot, with the cache already warm, is a different case and '
        'is not covered by this reasoning.' % (
            '0x%08x' % cacr['addr'] if cacr else '<unresolved>'))
    return {'writes': decoded, 'verdict': verdict, 'citations': CACHE_CITATIONS}


# --------------------------------------------------------------------------
# 3. Candidates: runs of a single repeated fill byte inside the
# survives-boot region.
# --------------------------------------------------------------------------

def find_candidates(img, load_addr, survive_start, survive_end, min_size):
    """-> [{'start','end','size','fill'}], start 4-byte aligned, sorted by
    address. `fill` is 0x00 or 0xFF."""
    out = []
    for fill, pat in ((0x00, re.compile(b'\x00+')), (0xFF, re.compile(b'\xff+'))):
        lo_off, hi_off = survive_start - load_addr, survive_end - load_addr
        for m in pat.finditer(img, lo_off, hi_off):
            s, e = m.start(), m.end()
            aligned_s = (s + 3) & ~3
            if e - aligned_s >= min_size:
                out.append({
                    'start': load_addr + aligned_s, 'end': load_addr + e,
                    'size': e - aligned_s, 'fill': fill,
                })
    out.sort(key=lambda c: c['start'])
    return out


# --------------------------------------------------------------------------
# 4. Exclusions.
# --------------------------------------------------------------------------

def _byte_class(lo, hi):
    if hi - lo == 1:
        return b'\\x%02x' % lo
    return b'[\\x%02x-\\x%02x]' % (lo, hi - 1)


def _range_regex(lo, hi, nbytes=4):
    """-> bytes regex (no anchors) matching any big-endian `nbytes`-byte
    integer v with lo <= v < hi. Recurses one byte at a time so the
    resulting alternation only branches where the range actually crosses a
    byte-value boundary -- for a cave-sized range (well under 64 KB) that is
    a handful of branches, and the whole scan is a single compiled-regex
    pass over the image (milliseconds; see the module docstring)."""
    if nbytes == 1:
        return _byte_class(lo, hi)
    span = 1 << (8 * (nbytes - 1))
    parts, v = [], lo
    while v < hi:
        top = v // span
        nxt = min((top + 1) * span, hi)
        sub_lo, sub_hi = v - top * span, nxt - top * span
        tail = (b'.' * (nbytes - 1) if sub_lo == 0 and sub_hi == span
                else _range_regex(sub_lo, sub_hi, nbytes - 1))
        parts.append(_byte_class(top, top + 1) + tail)
        v = nxt
    return parts[0] if len(parts) == 1 else b'(?:' + b'|'.join(parts) + b')'


def _find_be32_in_range(img, lo, hi):
    """-> sorted {value, ...} of every distinct big-endian 32-bit VALUE found
    anywhere in the image (any byte offset, not just aligned ones) that
    falls in [lo, hi) -- i.e. the set of addresses something in the image
    points at, not the locations of the pointers themselves. By construction
    every match decodes to a value in [lo, hi), so the scan only needs the
    compiled range regex; matches may overlap (e.g. two back-to-back
    pointers sharing no bytes still need the scan to not skip ahead by more
    than 1 byte after a hit), hence the manual search loop rather than
    finditer, which advances past the whole previous match."""
    if lo >= hi:
        return set()
    pat = re.compile(_range_regex(lo, hi), re.DOTALL)
    out, pos = set(), 0
    while True:
        m = pat.search(img, pos)
        if not m:
            return out
        out.add(struct.unpack_from('>I', img, m.start())[0])
        pos = m.start() + 1


class GhidraDump:
    """Optional Ghidra xrefs -- function ranges and data/call references,
    used to (a) reject a candidate Ghidra thinks is real code and (b) bound
    an object well enough to trim rather than reject a candidate referenced
    only at its edge. Refuses to load (returns None from `load`) if the
    dump's manifest doesn't match this image's SHA-256, rather than trusting
    a stale dump silently."""

    def __init__(self, conn):
        self._conn = conn

    @staticmethod
    def load(dirpath, image_sha256):
        if not dirpath:
            return None, 'no --ghidra given'
        manifest_path = os.path.join(dirpath, 'manifest.json')
        sqlite_path = os.path.join(dirpath, 'xrefs.sqlite')
        if not (os.path.exists(manifest_path) and os.path.exists(sqlite_path)):
            return None, '%s has no manifest.json/xrefs.sqlite' % dirpath
        with open(manifest_path) as f:
            manifest = json.load(f)
        if manifest.get('image_sha256') != image_sha256:
            return None, ('%s manifest image_sha256 %s does not match this '
                          'image (%s) -- stale dump, ignoring'
                          % (dirpath, manifest.get('image_sha256'), image_sha256))
        import sqlite3
        conn = sqlite3.connect(sqlite_path)
        return GhidraDump(conn), 'loaded %s (sha256 matches)' % dirpath

    def covering_range(self, addr):
        """-> (lo, hi) of the tightest known function range containing
        `addr`, or None. Used to bound a trim; see apply_exclusions."""
        row = self._conn.execute(
            'SELECT lo, hi FROM function_ranges WHERE lo <= ? AND ? < hi '
            'ORDER BY (hi - lo) ASC LIMIT 1', (addr, addr)).fetchone()
        return tuple(row) if row else None

    def overlap(self, lo, hi):
        """-> True if any known function range overlaps [lo, hi)."""
        row = self._conn.execute(
            'SELECT 1 FROM function_ranges WHERE lo < ? AND ? < hi LIMIT 1',
            (hi, lo)).fetchone()
        return row is not None

    def refs_into(self, lo, hi):
        """-> sorted {addr, ...} referenced by any data_ref/call landing in
        [lo, hi)."""
        out = set()
        for row in self._conn.execute(
                'SELECT DISTINCT to_addr FROM data_refs WHERE ? <= to_addr AND to_addr < ?',
                (lo, hi)):
            out.add(row[0])
        for row in self._conn.execute(
                'SELECT DISTINCT to_addr FROM calls WHERE to_addr IS NOT NULL '
                'AND ? <= to_addr AND to_addr < ?', (lo, hi)):
            out.add(row[0])
        return sorted(out)


LOOKBACK = 64


def apply_exclusions(candidate, img, load_addr, ghidra=None, deep_hits=()):
    """-> (kept_range_or_None, [evidence strings]).

    Reject is the default outcome of any hit. Trimming only ever happens
    when something gives an EXACT bound for the object doing the
    referencing -- currently only Ghidra's function_ranges can. A hit found
    by the plain pointer-literal scan alone always rejects the whole run,
    even when it looks like it only touches the run's edge: with no bound,
    the referenced object could start anywhere at or before the hit. See
    the module docstring's step 4 for the reasoning this implements.
    """
    start, end = candidate['start'], candidate['end']
    evidence = []

    if ghidra is not None and ghidra.overlap(start, end):
        return None, ['rejected: overlaps a Ghidra function_ranges entry '
                      '(Ghidra thinks real code is here)']

    hits = set(_find_be32_in_range(img, start - LOOKBACK, end))
    hits.update(deep_hits)
    if ghidra is not None:
        hits.update(a for a in ghidra.refs_into(start - LOOKBACK, end))

    if not hits:
        return (start, end), ['no reference into [0x%08x, 0x%08x) found '
                              '(pointer-literal scan%s)'
                              % (start - LOOKBACK, end,
                                 ' + Ghidra' if ghidra is not None else '')]

    front = sorted(h for h in hits if h < start)
    inside = sorted(h for h in hits if start <= h < end)

    cur_start = start
    if front:
        nearest = max(front)
        bound = ghidra.covering_range(nearest) if ghidra is not None else None
        if bound is not None and bound[1] <= start:
            evidence.append('reference at 0x%08x, %s bytes before the run, is '
                            'bounded by Ghidra to end at 0x%08x (before the run)'
                            % (nearest, start - nearest, bound[1]))
        elif bound is not None and bound[1] < end:
            evidence.append('trimmed start to 0x%08x: preceding reference at '
                            '0x%08x bounded by Ghidra to end there' % (bound[1], nearest))
            cur_start = max(cur_start, bound[1])
        else:
            return None, ['rejected: reference at 0x%08x, %d bytes before the '
                          'run, with no bound available to say it doesn'"'"'t reach in'
                          % (nearest, start - nearest)]

    if inside:
        # A reference landing anywhere inside the run -- whether at its very
        # start or partway through -- means something treats that address as
        # real. Without a bound on how big that something is, its start
        # could be anywhere at or before the reference (a `move.l
        # #(base+FIELD_OFFSET),d0` idiom commonly references a field partway
        # into a struct that starts well before it), so the seemingly-clean
        # PREFIX before the reference cannot be trusted either: only reject,
        # unless Ghidra gives an exact bound to trim to.
        t0 = min(inside)
        bound = ghidra.covering_range(t0) if ghidra is not None else None
        if bound is not None and bound[1] < end:
            evidence.append('trimmed start to 0x%08x: object containing the '
                            'reference at 0x%08x bounded by Ghidra to end there'
                            % (bound[1], t0))
            cur_start = max(cur_start, bound[1])
        else:
            return None, ['rejected: reference at 0x%08x inside the run '
                          '(offset +%d), no size bound available'
                          % (t0, t0 - start)]

    if cur_start >= end:
        return None, evidence + ['rejected: nothing left after trimming']
    return (cur_start, end), evidence


def deep_ref_scan(image_path, load_addr, lo, hi):
    """-> sorted {addr, ...} of every PC-relative-or-absolute lea/pea/branch
    target in [lo, hi) found by ONE full tools/refscan.py disassembly of the
    image. Several seconds (a single Capstone pass over the whole image);
    called once per image under --deep-refs, not once per candidate -- the
    per-candidate filtering happens afterwards against this one result set,
    since re-disassembling the image per candidate is what made an early
    version of this function take minutes instead of seconds."""
    tools_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools')
    sys.path.insert(0, tools_dir)
    import refscan
    with open(image_path, 'rb') as f:
        img = f.read()
    hits, *_ = refscan.scan(img, load_addr, load_addr, load_addr + len(img), lo, hi)
    return sorted({h[-1] for h in hits})


# --------------------------------------------------------------------------
# 5. Runtime evidence: compare candidate bytes against snapshot rungs.
# --------------------------------------------------------------------------

class _RestrictedUnpickler(pickle.Unpickler):
    """Snapshots hold only primitive state; refuse anything else. Reimplemented
    rather than importing emu/snapshot.py, which pulls in Unicorn (via
    emu/harness.py) merely to unpickle a dict -- unwanted for this tool's
    otherwise-Unicorn-free static path."""

    def find_class(self, module, name):
        raise pickle.UnpicklingError('refusing to unpickle %s.%s' % (module, name))


def load_snapshot(path):
    with open(path, 'rb') as f:
        return _RestrictedUnpickler(f).load()


def snapshot_bytes(blob, load_addr, addr, n):
    """-> ('ok', bytes) | ('unmapped', None). A page present in all_mapped but
    absent from `pages` is an all-zero page (emu/snapshot.py's save() only
    stores a page if it has any non-zero byte)."""
    out = bytearray()
    a = addr
    while a < addr + n:
        base = a - (a % PAGE)
        take = min(PAGE - (a - base), addr + n - a)
        if base in blob['pages']:
            import zlib
            page = zlib.decompress(blob['pages'][base])
            out += page[a - base: a - base + take]
        elif base in set(blob['all_mapped']):
            out += bytes(take)
        else:
            return 'unmapped', None
        a += take
    return 'ok', bytes(out)


def runtime_survives(candidate, img, load_addr, snapshot_paths):
    """-> (True/False/None, [evidence]). None means no snapshot said anything
    either way (no snapshots available, or every rung had this page
    unmapped) -- a candidate is not rejected on this alone, but the caller
    should say so rather than silently treating None as a pass."""
    start, end = candidate['start'], candidate['end']
    off = start - load_addr
    expected = img[off:off + (end - start)]
    evidence = []
    checked = 0
    for path in snapshot_paths:
        blob = load_snapshot(path)
        status, data = snapshot_bytes(blob, load_addr, start, end - start)
        name = os.path.basename(path)
        if status == 'unmapped':
            evidence.append('%s: page not mapped, no evidence' % name)
            continue
        checked += 1
        if data != expected:
            first = next(i for i in range(len(data)) if data[i] != expected[i])
            evidence.append('%s: DIFFERS at 0x%08x (0x%02x -> 0x%02x)'
                            % (name, start + first, expected[first], data[first]))
            return False, evidence
        evidence.append('%s: unchanged' % name)
    if checked == 0:
        return None, evidence or ['no snapshots available for this image']
    return True, evidence


def derive_snapshots_dir(image_path):
    """-> the conventional snapshots dir for an image under out/sections/X/,
    i.e. snapshots/X -- or None if the image path doesn't follow that
    layout. Mirrors how out/ghidra dumps and out/sections both key off the
    same X (see the manifest.json 'sections_image' field)."""
    norm = os.path.normpath(image_path)
    marker = os.path.join('out', 'sections') + os.sep
    idx = norm.find(marker)
    if idx < 0:
        return None
    rest = norm[idx + len(marker):]
    parts = rest.split(os.sep)
    if len(parts) < 2:
        return None
    return os.path.join('snapshots', parts[0])


def available_snapshots(snapshots_dir, image_sha256):
    """-> sorted [path, ...] of *.snap files whose ladder's main_sha256
    matches this image (via the sibling .ladder.json), or all *.snap files
    if there's no ladder file to check against."""
    if not snapshots_dir or not os.path.isdir(snapshots_dir):
        return []
    ladder_path = os.path.join(snapshots_dir, '.ladder.json')
    if os.path.exists(ladder_path):
        with open(ladder_path) as f:
            ladder = json.load(f)
        if ladder.get('main_sha256') != image_sha256:
            return []
    import glob
    return sorted(glob.glob(os.path.join(snapshots_dir, '*.snap')))


# --------------------------------------------------------------------------
# 6. Confirmation: one bounded emulator run, canary bytes in every surviving
# candidate, fresh boot to 60,000,000 instructions.
# --------------------------------------------------------------------------

def confirm(image_path, syx_path, candidates, load_addr, out_dir, limit=60_000_000):
    """-> dict with 'ok', 'snapshot', 'checked', 'failures'. Writes into
    `out_dir` (caller's tempdir); does not touch the repo's own sections/
    or snapshots/."""
    import struct as _struct
    import tempfile

    with open(image_path, 'rb') as f:
        img = bytearray(f.read())
    canaries = {}
    for c in candidates:
        a, off = c['start'], 0
        while a + 4 <= c['end']:
            word = 0xCAFE0000 | (off & 0xFFFF)
            img[a - load_addr:a - load_addr + 4] = _struct.pack('>I', word)
            canaries[a] = word
            a, off = a + 4, off + 4

    from dt2.build import rebuild
    new_syx = rebuild(syx_path, replacements={3: bytes(img)})
    tmp_syx = os.path.join(out_dir, 'canary.syx')
    with open(tmp_syx, 'wb') as f:
        f.write(new_syx)

    from emu.extract import extract
    extract_dir = os.path.join(out_dir, 'sections')
    extract(tmp_syx, extract_dir)

    prefix = os.path.join(out_dir, 'boot')
    saved_env = {k: os.environ.get(k) for k in ('DT2_SECTIONS', 'DT2_SYX', 'DT2_MAIN_IMG')}
    os.environ['DT2_SECTIONS'] = extract_dir
    os.environ['DT2_SYX'] = tmp_syx
    os.environ.pop('DT2_MAIN_IMG', None)
    try:
        from emu.checkpoint import make
        saved = make([limit], prefix=prefix, syx=tmp_syx)
    finally:
        for k, v in saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    if not saved:
        return {'ok': False, 'reason': 'run stopped before reaching %d instructions' % limit}
    snap_path = saved[-1][1]
    blob = load_snapshot(snap_path)
    failures = []
    for addr in sorted(canaries):
        status, data = snapshot_bytes(blob, load_addr, addr, 4)
        actual = _struct.unpack('>I', data)[0] if status == 'ok' else None
        if actual != canaries[addr]:
            failures.append({'addr': addr, 'expected': canaries[addr],
                             'actual': actual, 'status': status})
    return {'ok': not failures, 'snapshot': snap_path, 'checked': len(canaries),
           'failures': failures}


# --------------------------------------------------------------------------
# 7. Ranking and CLI.
# --------------------------------------------------------------------------

def rank(kept):
    """Sort by size descending; among equal sizes, a candidate NOT adjacent
    to any referenced data (its exact boundary touches nothing) sorts above
    one that is -- a cave squeezed directly against a referenced object has
    less margin for the object to be mis-bounded than an isolated one."""
    def key(c):
        return (-c['size'], c.get('adjacent_to_ref', False))
    return sorted(kept, key=key)


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('image', help='a MAIN OS image (section_3_MAIN_OS.bin)')
    ap.add_argument('--min', type=int, default=256, help='minimum cave size in bytes (default 256)')
    ap.add_argument('--ghidra', metavar='DIR', help='a tools/ghidradump.py output directory')
    ap.add_argument('--snapshots', metavar='DIR', help='snapshot directory (default: derived from the image path)')
    ap.add_argument('--deep-refs', action='store_true',
                    help='also run a tools/refscan.py disassembly pass for PC-relative lea/pea (slow: several seconds)')
    ap.add_argument('--confirm', metavar='SYX', help='run the one bounded emulator confirmation (Digitakt II 1.16 only)')
    ap.add_argument('--json', metavar='PATH', help='write machine-readable results here')
    args = ap.parse_args(argv)

    with open(args.image, 'rb') as f:
        img = f.read()
    image_sha256 = hashlib.sha256(img).hexdigest()

    bmap = boot_map(img)
    creport = cache_report(img)

    ghidra, ghidra_msg = GhidraDump.load(args.ghidra, image_sha256)

    candidates = find_candidates(img, LOAD_ADDR, bmap['survive_start'], bmap['survive_end'], args.min)

    deep_hits_all = set()
    if args.deep_refs and candidates:
        # One disassembly pass, target range wide enough to cover every
        # candidate's own [start-LOOKBACK, end) window -- see deep_ref_scan.
        deep_lo = min(c['start'] for c in candidates) - LOOKBACK
        deep_hi = max(c['end'] for c in candidates)
        deep_hits_all = deep_ref_scan(args.image, LOAD_ADDR, deep_lo, deep_hi)

    kept = []
    rejected = []
    for c in candidates:
        deep_hits = {a for a in deep_hits_all if c['start'] - LOOKBACK <= a < c['end']}
        kept_range, evidence = apply_exclusions(
            c, img, LOAD_ADDR, ghidra=ghidra, deep_hits=deep_hits)
        if kept_range is None:
            rejected.append({**c, 'evidence': evidence})
            continue
        start, end = kept_range
        size = end - start
        if size < args.min:
            rejected.append({**c, 'evidence': evidence + [
                'rejected: trimmed size %d < --min %d' % (size, args.min)]})
            continue
        c2 = dict(c, start=start, end=end, size=size, evidence=evidence,
                 adjacent_to_ref=bool(evidence and any('reference' in e for e in evidence)))
        kept.append(c2)

    snapshots_dir = args.snapshots or derive_snapshots_dir(args.image)
    snapshot_paths = available_snapshots(snapshots_dir, image_sha256)
    survivors = []
    for c in kept:
        survives, rt_evidence = runtime_survives(c, img, LOAD_ADDR, snapshot_paths)
        if survives is False:
            rejected.append({**c, 'evidence': c['evidence'] + rt_evidence})
            continue
        c3 = dict(c, runtime_evidence=rt_evidence, runtime_checked=(survives is True))
        survivors.append(c3)

    survivors = rank(survivors)

    confirm_result = None
    if args.confirm:
        import tempfile
        with tempfile.TemporaryDirectory(prefix='cavefind-confirm-') as tmp:
            confirm_result = confirm(args.image, args.confirm, survivors, LOAD_ADDR, tmp)

    _print_report(args.image, image_sha256, bmap, creport, ghidra_msg,
                  snapshots_dir, snapshot_paths, survivors, rejected, confirm_result)

    if args.json:
        with open(args.json, 'w') as f:
            json.dump({
                'image': args.image, 'image_sha256': image_sha256,
                'boot_map': bmap, 'cache_report': creport,
                'ghidra': ghidra_msg, 'snapshots_dir': snapshots_dir,
                'snapshots_used': snapshot_paths,
                'survivors': survivors, 'rejected': rejected,
                'confirm': confirm_result,
            }, f, indent=2)
        print('\nwrote %s' % args.json)
    return 0


def _print_report(image, sha, bmap, creport, ghidra_msg, snapshots_dir,
                  snapshot_paths, survivors, rejected, confirm_result):
    print('%s (sha256 %s...)' % (image, sha[:16]))
    print('boot map: survives [0x%08x, 0x%08x), clear [0x%08x, 0x%08x), '
         'copy source [0x%08x, 0x%08x)'
         % (bmap['survive_start'], bmap['survive_end'], bmap['clear_start'],
            bmap['clear_end'], bmap['copy_ranges'][0][0], bmap['copy_ranges'][0][1]))
    print('\ncache/memory-map:')
    for line in creport['citations']:
        print('  ' + line)
    for w in creport['writes']:
        if w['value'] is None:
            print('  0x%08x  movec %s, %s  (value not a simple immediate load)'
                 % (w['addr'], w['src'], w['reg']))
        else:
            print('  0x%08x  movec %s, %s = 0x%08x  %s'
                 % (w['addr'], w['src'], w['reg'], w['value'], w['decoded']))
    for k, v in creport['verdict'].items():
        print('  %s: %s' % (k, v))
    print('\nghidra: %s' % ghidra_msg)
    print('snapshots: %s (%d rung(s) used: %s)'
         % (snapshots_dir, len(snapshot_paths),
            ', '.join(os.path.basename(p) for p in snapshot_paths) or 'none'))

    print('\n%-28s %8s  %-4s  evidence' % ('range', 'size', 'fill'))
    for c in survivors:
        print('0x%08x-0x%08x  %8d  0x%02x  %s'
             % (c['start'], c['end'], c['size'], c['fill'],
                '; '.join(c['evidence'] + c['runtime_evidence'])))
    print('\n%d rejected candidate(s)' % len(rejected))
    for c in rejected:
        print('  0x%08x-0x%08x  %8d  0x%02x  %s'
             % (c['start'], c['end'], c['size'], c['fill'], '; '.join(c['evidence'])))

    if confirm_result is not None:
        print('\nconfirmation: %s' % ('OK' if confirm_result.get('ok') else 'FAILED'))
        print('  %s' % confirm_result)


if __name__ == '__main__':
    raise SystemExit(main())
