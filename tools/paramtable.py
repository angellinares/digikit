#!/usr/bin/env python3
"""Dump and decode the ColdFire parameter descriptor table at 0x4020f18c.

DT2 1.16 only. Every DT2 "machine" descriptor (see `tools/machineprofile.py`)
holds nine ID fields ("param_codes") that index this table. The verified
reader (`out/ghidra/dt2-1.16-seeded/disasm/400d9ed8_FUN_400d9ed8.s`) clamps
a param_code to `0x113` (275) entries, indexes a `0x3c`-byte (60-byte)
stride array at `0x4020f18c`, and returns the u32 at entry offset +4 -- the
"mirror index" used by the per-track UI mirror at
`0x80003362 + track*0x8e + idx*2`.

This tool reads the other 56 bytes of each entry. The layout below was
worked out from three levers, in order of strength:

  1. Other readers of the table at offsets besides +4, found by grepping
     `out/ghidra/dt2-1.16-seeded/` for the literal table address and its
     per-entry offsets, then reading the decompilation. The strongest of
     these is `FUN_4004fefc`, which walks all 0x113 entries once per
     track/context to refresh the whole UI mirror: for each param_code it
     calls `FUN_400da204(param_code)` (a 12-byte memcpy from entry+0x08,
     confirming +0x08/+0x0c/+0x10 are a {min, max, default} triple copied
     as a unit), reads entry+0x14 as a boolean formatting flag, calls the
     per-parameter formatter, calls `FUN_400d9ed8` for the mirror index,
     and writes the formatted value at `mirror_base + idx*2`.
  2. Resolving word10 (+0x28) and word12 (+0x30) as pointers and reading
     C strings at the target address: they resolve to a "long" display
     name ("Track Level", "Slice Select", ...) and a short all-caps mirror
     label ("LEV", "SLICE", ...) respectively. word11 (+0x2c) resolves to
     the owning machine's internal name string ("Stretch", "MSlice", ...),
     independently corroborating word0 as the owning machine-type id.
  3. Value-shape correlation across entries that are known, from
     `docs/findings/02-machines-and-parameters.md`'s machine descriptors, to be the SAME UI slot in
     different machines (e.g. STRETCH's `0xdd` and MANUAL SLICE's `0xf8`
     are both that machine's first SRC-page field): fields that must carry
     "the same kind of thing" for corresponding slots do turn out equal
     (mirror index, owning-name pointer family, formatter pointer, +0x1c,
     +0x28/+0x30 string pointers) which is what let entries with unknown
     param_codes (STRETCH's second and sixth ID fields) be found by
     scanning for the missing (owner_type=2, position) combinations.

Confirmed (see docs/findings/02-machines-and-parameters.md for the full mark-by-mark writeup):
    +0x00 owner_type   u32   the owning machine's type id (0 = global/common)
    +0x04 mirror_index i32   UI mirror slot, -1 = not mirrored [FUN_400d9ed8]
    +0x08 min           u32  ³ 12-byte {min,max,default} triple, memcpy'd as
    +0x0c max           u32  ³ a unit by FUN_400da204 (called by FUN_4004fefc
    +0x10 default       u32  ³ and the "Clear" handler in ParameterPageView)
    +0x14 flag          u32   boolean, passed to the value formatter
    +0x18 lo16a         i16   ³ two SEPARATE i16 fields [FUN_400da4b8 (+0x18),
    +0x1a lo16b         i16   ³ FUN_400da4dc (+0x1a), both undefined for most
                               entries: real only for a handful of low codes]
    +0x1c kind          u32   value-kind/unit selector [FUN_400da500], fed to
                               a value-transform routine by ParameterSet::vfunc_25
    +0x28 long_name     char* display name, e.g. "Track Level"
    +0x2c owner_name    char* owning machine's internal name, e.g. "Stretch"
    +0x30 short_name    char* mirror-row label, e.g. "LEV"

Not pinned down (value-shape only, no confirmed reader found this pass):
    +0x20 w20  u32  confirmed 32-bit accessor exists (FUN_400da330) but no
                     caller was located; value shape tracks param_code
                     closely for the "common" entries (0,1,2,0x0a) but not
                     for machine-owned ones -- open.
    +0x24 w24  u32  confirmed 32-bit accessor exists (FUN_400da524);
                     ParameterSet::vfunc_7 treats an equivalent field as a
                     bitmask (tests 0x10000/0x20000/0x40000) but only for
                     param_codes 0x4e/0x58/0x62, not the codes this task
                     cares about -- open for the rest.
    +0x34 w34  u32  looks like a function pointer (0x40 0e xxxx range,
                     inside MAIN OS text), shared identically between
                     UI-equivalent slots across machines -- open.
    +0x38 w38  u32  constant 0x4023f364 in every entry sampled (points at
                     an empty string just before "SEND SYSEX" in rodata) --
                     open, may simply be unused/sentinel.

Usage:
    uv run python tools/paramtable.py --codes 0,0xa,0xdd,0xf8
    uv run python tools/paramtable.py --all --json out/paramtable.json
"""
import argparse
import json
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import machineprofile  # noqa: E402

TABLE_ADDR = 0x4020f18c
STRIDE = 0x3c
COUNT = 0x113

DEFAULT_IMAGE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    'out', 'sections', 'dt2-1.16', 'section_3_MAIN_OS.bin')

KNOWN_IMAGE_SHA256 = (
    '57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d')


def _cstring(read, addr, limit=64):
    """-> ASCII string at guest address `addr`, or None if not printable."""
    if addr < 0x40000000 or addr > 0x40ffffff:
        return None
    out = bytearray()
    while len(out) < limit:
        try:
            b = read(addr + len(out), 1)
        except SystemExit:
            return None
        if b == b'\x00':
            break
        out += b
    if not out:
        return None
    try:
        s = out.decode('ascii')
    except UnicodeDecodeError:
        return None
    if not all(32 <= ord(c) < 127 for c in s):
        return None
    return s


# Named 32-bit fields, in entry order. `word` is the byte offset.
FIELDS = (
    ('owner_type', 0x00),
    ('mirror_index', 0x04),
    ('min', 0x08),
    ('max', 0x0c),
    ('default', 0x10),
    ('flag_0x14', 0x14),
    ('kind_0x1c', 0x1c),
    ('w20', 0x20),
    ('w24', 0x24),
    ('long_name_ptr', 0x28),
    ('owner_name_ptr', 0x2c),
    ('short_name_ptr', 0x30),
    ('w34_fnptr', 0x34),
    ('w38', 0x38),
)

# i32 fields (signed); everything else in FIELDS is read as u32.
SIGNED_FIELDS = {'mirror_index'}

STR_FIELDS = ('long_name_ptr', 'owner_name_ptr', 'short_name_ptr')


def decode_entry(read, param_code):
    """-> dict decoding one 0x3c-byte entry for `param_code` (0..COUNT-1)."""
    idx = param_code if param_code < COUNT else 0  # clamp, per FUN_400d9ed8
    base = TABLE_ADDR + idx * STRIDE
    raw = read(base, STRIDE)
    words = struct.unpack('>15I', raw)

    entry = {
        'param_code': param_code,
        'clamped_index': idx,
        'addr': '%#010x' % base,
        'raw': raw.hex(),
    }
    for name, off in FIELDS:
        u32 = words[off // 4]
        if name in SIGNED_FIELDS and u32 & 0x80000000:
            u32 -= 1 << 32
        entry[name] = u32

    # +0x18 / +0x1a: two separate signed 16-bit fields (FUN_400da4b8 /
    # FUN_400da4dc), packed inside the same 32-bit word as +0x18 in FIELDS
    # would suggest if read as one u32 -- they are NOT one u32.
    w18 = raw[0x18:0x1c]
    hi16, lo16 = struct.unpack('>hh', w18)
    entry['i16_0x18'] = hi16
    entry['i16_0x1a'] = lo16

    for name in STR_FIELDS:
        entry[name + '_hex'] = '%#010x' % entry[name]
        entry[name.replace('_ptr', '')] = _cstring(read, entry[name])
        del entry[name]

    return entry


def scan_all(read):
    return [decode_entry(read, i) for i in range(COUNT)]


def format_entry(e):
    lines = []
    lines.append('param_code %#04x (%d)  addr %s%s' % (
        e['param_code'], e['param_code'], e['addr'],
        '  [CLAMPED to 0]' if e['clamped_index'] != e['param_code'] else ''))
    lines.append('  long_name=%r  short_name=%r  owner_name=%r' % (
        e['long_name'], e['short_name'], e['owner_name']))
    lines.append('  owner_type=%#x  mirror_index=%d' % (
        e['owner_type'], e['mirror_index']))
    lines.append('  min=%#x  max=%#x  default=%#x  flag_0x14=%#x' % (
        e['min'], e['max'], e['default'], e['flag_0x14']))
    lines.append('  i16@0x18=%d  i16@0x1a=%d  kind_0x1c=%#x' % (
        e['i16_0x18'], e['i16_0x1a'], e['kind_0x1c']))
    lines.append('  w20=%#x  w24=%#x  w34_fnptr=%#x  w38=%#x' % (
        e['w20'], e['w24'], e['w34_fnptr'], e['w38']))
    return '\n'.join(lines)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    p.add_argument('image', nargs='?', default=DEFAULT_IMAGE,
                    help='MAIN OS image (default: %s)' % DEFAULT_IMAGE)
    p.add_argument('--load-addr', type=lambda s: int(s, 0),
                    default=machineprofile.LOAD_ADDR,
                    help='guest load address of image[0] (default %#x)'
                    % machineprofile.LOAD_ADDR)
    p.add_argument('--codes', help='comma-separated param_codes (hex or '
                    'decimal) to print in full; default: the codes used by '
                    'the known machine descriptors')
    p.add_argument('--all', action='store_true',
                    help='print all %d entries in full (verbose)' % COUNT)
    p.add_argument('--json', metavar='PATH',
                    help='write the full decoded table (all entries) as JSON')
    p.add_argument('--no-sha-check', action='store_true',
                    help='skip the known-image sha256 check')
    args = p.parse_args(argv)

    if not os.path.exists(args.image):
        raise SystemExit('%s: not found' % args.image)

    if not args.no_sha_check:
        sha = machineprofile.sha256_file(args.image)
        if sha != KNOWN_IMAGE_SHA256:
            print('warning: %s sha256 %s does not match the known DT2 1.16 '
                  'MAIN OS (%s) -- addresses below may not apply'
                  % (args.image, sha, KNOWN_IMAGE_SHA256), file=sys.stderr)

    read = machineprofile.image_reader(args.image, args.load_addr)

    default_codes = (0x00, 0x0a, 0xdd, 0xdf, 0xe0, 0xe2, 0xe3, 0xf2, 0xf4,
                      0xf8, 0xf9, 0xfb, 0xfc, 0xfd, 0xfe)
    if args.codes:
        codes = [int(c, 0) for c in args.codes.split(',')]
    else:
        codes = list(default_codes)

    print('table @ %#010x, stride %#x, %d entries (%#x)'
          % (TABLE_ADDR, STRIDE, COUNT, COUNT))
    print('image %s, load_addr %#x' % (args.image, args.load_addr))
    print()

    if args.all:
        all_entries = scan_all(read)
        for e in all_entries:
            print(format_entry(e))
            print()
    else:
        all_entries = None
        for code in codes:
            print(format_entry(decode_entry(read, code)))
            print()

    if args.json:
        entries = all_entries if all_entries is not None else scan_all(read)
        with open(args.json, 'w') as f:
            json.dump({
                'table_addr': TABLE_ADDR,
                'stride': STRIDE,
                'count': COUNT,
                'image': args.image,
                'load_addr': args.load_addr,
                'entries': entries,
            }, f, indent=1)
        print('wrote %s (%d entries)' % (args.json, len(entries)))

    # Summary: named entries only (has a resolved long_name).
    all_entries = all_entries if all_entries is not None else scan_all(read)
    named = [e for e in all_entries if e['long_name']]
    print('summary: %d/%d entries resolve a long display name'
          % (len(named), COUNT))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
