#!/usr/bin/env python3
"""Exhaustive static scan for absolute-address references into a byte range
of a ColdFire firmware image.

Answers one general question: "what, if anything, in this image's code
statically references address range [LO, HI)?" -- by linearly disassembling
a span of the image with `dt2.coldfire.disasm` (Capstone m68k + this repo's
own ColdFire MVS/MVZ and FF1.L patches, so a scripted sweep does not
silently desync on those two opcodes -- see that module's docstring) and
regex-extracting every operand that looks like a 32-bit absolute address:
absolute addressing-mode operands (`$xxxxxxxx.l`/`.w`), immediate loads
(`#$xxxxxxxx`), and resolved branch/jsr/jmp targets. Values below
--min-value (default 0x10000) are dropped as noise (small immediates,
displacements, SR masks, etc); `(An)`-style displacement operands are
excluded by construction (a `$xx` immediately followed by `(` is a
displacement, not an absolute address, and is skipped) -- **except**
PC-relative operands. `dt2.coldfire.disasm` already resolves those to
the absolute target and prints it as `$400dd6a6(pc)`, so the value in
front of `(pc` is an address, not a displacement, and is kept. GCC
emits `lea %pc@(target),%aN` followed by `jsr %aN@` for calls in
loops, so skipping these hides real callers entirely.

This is a *static, no-semantics* scan. It cannot see an address that is
computed (built in a register via arithmetic, e.g. a base + index*stride)
or reached only through a pointer already sitting in memory/bss -- so a
clean scan is a lower bound on real references, never a proof that nothing
touches a range. Report the coverage percentage honestly alongside any
"zero hits" claim.

Coverage = (bytes yielded as real decoded instructions) / (bytes swept).
Bytes `coldfire.disasm` cannot decode are emitted as `.word` and do not
count as covered; a byte range with many of those is a sign the sweep has
run into data/a jump table/a desync, and its hits should be treated with
more suspicion than a byte range with none.

Usage:
    uv run python tools/refscan.py sections/section_3_MAIN_OS.bin \\
        --base 0x40000400 --range 0x402f9c14 0x40307f60

    # Restrict the sweep itself (default: the whole image):
    uv run python tools/refscan.py IMG.bin --base 0x40000400 \\
        --scan-start 0x40000400 --scan-end 0x40307f60 \\
        --range 0x402f9c14 0x40307f60

    # Map referenced clusters and gaps inside the target range:
    uv run python tools/refscan.py IMG.bin --base 0x40000400 \\
        --range 0x402f9c14 0x40307f60 --map

    # Machine-readable:
    uv run python tools/refscan.py IMG.bin --base 0x40000400 \\
        --range 0x402f9c14 0x40307f60 --json out.json

Exit status is always 0; "zero hits" is a valid, useful answer and is not
an error.
"""
import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from dt2.coldfire import disasm  # noqa: E402

# A bare "$hex" token not immediately followed by "(" -- that "(" case is a
# displacement operand like "$14(a7)" or "-$14(a7)", not an absolute address.
# The one exception is "(pc": disasm() prints a PC-relative operand with its
# target already resolved, so "$400dd6a6(pc), a4" names 0x400dd6a6 itself.
# The token must also be taken whole: without (?![0-9A-Fa-f]) the regex backs
# off to a shorter prefix when the lookahead fails, so "$14(a7)" yielded 0x1
# and "$400dd6a6(a2)" yielded 0x400dd6a -- a false hit above --min-value.
_TOKEN_RE = re.compile(r'\$([0-9A-Fa-f]+)(?![0-9A-Fa-f])(?!\((?!pc))')


def extract_refs(ops, min_value):
    """-> sorted list of int values in `ops` (a disasm() operand string)
    that look like absolute addresses (hex, >= min_value, not a displacement)."""
    out = []
    for m in _TOKEN_RE.finditer(ops):
        v = int(m.group(1), 16)
        if v >= min_value:
            out.append(v)
    return out


def scan(img, base, scan_start, scan_end, lo, hi, min_value=0x10000):
    """Sweep [scan_start, scan_end) and return
    (hits, total_instructions, covered_bytes, swept_bytes).

    `hits` is a list of (pc, hexbytes, mnemonic, ops, target) for every
    candidate absolute-address operand whose value falls in [lo, hi).
    """
    hits = []
    total_instructions = 0
    covered_bytes = 0
    swept_bytes = scan_end - scan_start
    for pc, hx, mn, ops in disasm(img, base, scan_start, scan_end):
        size = len(hx) // 2
        if mn == '.word':
            continue
        total_instructions += 1
        covered_bytes += size
        for target in extract_refs(ops, min_value):
            if lo <= target < hi:
                hits.append((pc, hx, mn, ops, target))
    return hits, total_instructions, covered_bytes, swept_bytes


def build_map(hits, lo, hi):
    """-> list of gap dicts between referenced targets, sorted by size desc.

    Treats each distinct referenced target address as a single occupied
    point (this is a static scan; real object sizes are not known without
    a runtime dump or Ghidra data-type info -- see tools/memdump.py and
    tools/ghidraq.py for that). Gaps are computed between consecutive
    distinct target addresses, and from lo to the first / last to hi.
    """
    targets = sorted(set(t for *_, t in hits))
    gaps = []
    prev = lo
    for t in targets:
        if t > prev:
            gaps.append({'start': prev, 'end': t, 'size': t - prev})
        prev = max(prev, t + 1)
    if prev < hi:
        gaps.append({'start': prev, 'end': hi, 'size': hi - prev})
    gaps.sort(key=lambda g: -g['size'])
    return targets, gaps


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('image', help='path to a raw firmware section, e.g. sections/section_3_MAIN_OS.bin')
    ap.add_argument('--base', required=True, type=lambda s: int(s, 0),
                     help='guest load address of image[0]')
    ap.add_argument('--range', nargs=2, required=True, metavar=('LO', 'HI'),
                     type=lambda s: int(s, 0), help='target address range [LO, HI) to report hits for')
    ap.add_argument('--scan-start', type=lambda s: int(s, 0), default=None,
                     help='sweep start address (default: --base)')
    ap.add_argument('--scan-end', type=lambda s: int(s, 0), default=None,
                     help='sweep end address (default: base + len(image))')
    ap.add_argument('--min-value', type=lambda s: int(s, 0), default=0x10000,
                     help='drop candidate values below this (default 0x10000, filters small immediates/displacements)')
    ap.add_argument('--map', action='store_true',
                     help='also print referenced-target clusters and the gaps between them')
    ap.add_argument('--json', metavar='PATH', help='write machine-readable results here')
    args = ap.parse_args(argv)

    img = open(args.image, 'rb').read()
    scan_start = args.scan_start if args.scan_start is not None else args.base
    scan_end = args.scan_end if args.scan_end is not None else args.base + len(img)
    lo, hi = args.range

    hits, total_insns, covered, swept = scan(img, args.base, scan_start, scan_end, lo, hi, args.min_value)
    pct = 100.0 * covered / swept if swept else 0.0

    print('scanned 0x%08x-0x%08x (%d bytes), %d instructions decoded, '
          '%.2f%% byte coverage' % (scan_start, scan_end, swept, total_insns, pct))
    print('target range 0x%08x-0x%08x (%d bytes): %d hits' % (lo, hi, hi - lo, len(hits)))
    for pc, hx, mn, ops, target in hits:
        print('  0x%08x  %-16s %-9s %-28s -> 0x%08x' % (pc, hx, mn, ops, target))

    result = {
        'scan_start': scan_start, 'scan_end': scan_end, 'swept_bytes': swept,
        'total_instructions': total_insns, 'covered_bytes': covered, 'coverage_pct': pct,
        'range_lo': lo, 'range_hi': hi,
        'hits': [{'pc': pc, 'bytes': hx, 'mnemonic': mn, 'ops': ops, 'target': target}
                 for pc, hx, mn, ops, target in hits],
    }

    if args.map:
        targets, gaps = build_map(hits, lo, hi)
        print('\n%d distinct referenced target address(es) in range' % len(targets))
        for t in targets:
            print('  0x%08x' % t)
        print('\ngaps (unreferenced runs), largest first:')
        for g in gaps:
            print('  0x%08x-0x%08x  %d bytes' % (g['start'], g['end'], g['size']))
        result['distinct_targets'] = targets
        result['gaps'] = gaps

    if args.json:
        with open(args.json, 'w') as f:
            json.dump(result, f, indent=2)
        print('\nwrote %s' % args.json)

    return 0


if __name__ == '__main__':
    sys.exit(main())
