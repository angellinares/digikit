"""Diff guest memory between two emulator snapshots.

    uv run python tools/snapdiff.py A.snap B.snap [--range LO HI[=NAME]]...
        [--gap N] [--max-bytes N] [--json OUT]

Reads the page map that emu/snapshot.py pickles, so it builds no Machine.
Without --range it compares the MCF5441x on-chip SRAM
(0x80000000-0x80010000), where the ColdFire keeps the tables it streams to
the SHARC. Changed bytes separated by at most --gap unchanged bytes are
reported as one run. A run that starts in a known Digitakt II table (the same
in 1.15C and 1.16, tools/framelink.py) is labelled with the table, the track
row and the offset in the row (see
docs/findings/04-coldfire-dsp-link.md, "The ColdFire tells the SHARC through
a periodic DSPI2 frame").
"""

import argparse
import json
import os
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.harness import PAGE  # noqa: E402
from emu.snapshot import _load_blob  # noqa: E402
import framelink  # noqa: E402

SRAM = (0x80000000, 0x80010000, 'sram')

# (base, row size, rows, name), from tools/framelink.py.
TABLES = framelink.TABLES


def read(blob, lo, hi):
    """Bytes [lo, hi) of a snapshot blob. Pages it does not store read as 0."""
    out = bytearray(hi - lo)
    base = lo & ~(PAGE - 1)
    while base < hi:
        comp = blob['pages'].get(base)
        if comp is not None:
            page = zlib.decompress(comp)
            start, end = max(lo, base), min(hi, base + PAGE)
            out[start - lo:end - lo] = page[start - base:end - base]
        base += PAGE
    return bytes(out)


def runs(a, b, lo, gap):
    """Yield (start, end) address pairs of changed bytes, merging close runs."""
    start = end = None
    for i in range(len(a)):
        if a[i] != b[i]:
            if start is None:
                start = i
            elif i - end > gap:
                yield lo + start, lo + end
                start = i
            end = i + 1
    if start is not None:
        yield lo + start, lo + end


def label(addr):
    for base, size, rows, name in TABLES:
        if base <= addr < base + size * rows:
            row, off = divmod(addr - base, size)
            if rows > 1:
                return '%s[%d]+%#x' % (name, row, off)
            return '%s+%#x' % (name, off)
    return ''


def parse_args(argv=None):
    p = argparse.ArgumentParser(description='Diff guest memory between two snapshots.')
    p.add_argument('a')
    p.add_argument('b')
    p.add_argument('--range', action='append', nargs=2, metavar=('LO', 'HI[=NAME]'))
    p.add_argument('--gap', type=lambda s: int(s, 0), default=4)
    p.add_argument('--max-bytes', type=lambda s: int(s, 0), default=64,
                   help='bytes of each run to print')
    p.add_argument('--json')
    return p.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    ranges = [SRAM]
    if args.range:
        ranges = []
        for lo, hi in args.range:
            hi, _, name = hi.partition('=')
            ranges.append((int(lo, 0), int(hi, 0), name or '%s-%s' % (lo, hi)))
    blobs = _load_blob(args.a), _load_blob(args.b)
    for path, blob in zip((args.a, args.b), blobs):
        print('%s: extra %s' % (path, blob.get('extra', {})))
    report = {'a': args.a, 'b': args.b, 'regions': []}
    for lo, hi, name in ranges:
        a, b = read(blobs[0], lo, hi), read(blobs[1], lo, hi)
        changed = [{'start': s, 'end': e, 'label': label(s),
                    'a': a[s - lo:e - lo].hex(), 'b': b[s - lo:e - lo].hex()}
                   for s, e in runs(a, b, lo, args.gap)]
        count = sum(x != y for x, y in zip(a, b))
        report['regions'].append({'name': name, 'lo': lo, 'hi': hi,
                                  'changed_bytes': count, 'runs': changed})
        print('%s %#010x-%#010x: %d bytes differ in %d runs'
              % (name, lo, hi, count, len(changed)))
        for r in changed:
            print('  %#010x-%#010x %s' % (r['start'], r['end'], r['label']))
            print('    a %s' % r['a'][:2 * args.max_bytes])
            print('    b %s' % r['b'][:2 * args.max_bytes])
    if args.json:
        with open(args.json, 'w') as f:
            json.dump(report, f, indent=1)
    return 0


if __name__ == '__main__':
    sys.exit(main())
