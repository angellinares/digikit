"""Find call targets, vector handlers and code pointers for Ghidra to create.

    uv run python tools/codeseeds.py sections/section_3_MAIN_OS.bin
        [--base 0x40000400] [--code LO HI]...
        --json out/symbols/dt2-1.15C-seeds.json

A linear sweep with the repo disassembler (dt2.coldfire) over the code
ranges. It records
  calls    each jsr or bsr whose resolved target is in a code range;
  vectors  a handler written to the RAM vector table (VBR 0x40000000):
           `move.l #X,Dn` followed within three instructions by
           `move.l Dn,$4000xxxx.l` below 0x40000400, or
           `move.l #X,$4000xxxx.l`, with X in a code range.
  pointers an address taken as an immediate (`move.l #X,Dn`, and any other
           `#$X`) or by `pea $X.l`, where X is in a code range and the bytes
           there open with a function prologue. Ghidra makes the data
           reference and stops, so a function only ever reached through a
           stored pointer gets no function and no decompilation, and is
           invisible to a grep over the dump. Measured on Digitakt II 1.16:
           turning on Ghidra's own Function Start Search (with Search Data
           Blocks) and Aggressive Instruction Finder recovers none of them.
A linear sweep also decodes data, so these are candidates only.
tools/ghidraapply.py keeps a candidate when its site is not defined data and
creating the function adds no Error bookmark.

The default code ranges are Digitakt II 1.15C's: Ghidra has function entries
in 0x40000400-0x40200400 and 0x402c0400-0x402f9c14, and none in between,
where the vtables and typeinfo objects are. Give --code for other images.
"""

import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from dt2.coldfire import disasm  # noqa: E402

BASE = 0x40000400
VBR = 0x40000000
VECTORS_END = VBR + 0x400
CODE = ((0x40000400, 0x40200400), (0x402c0400, 0x402f9c14))

CALL = re.compile(r'\$([0-9a-fA-F]+)(?:\.l|\(pc\))?')
LOAD = re.compile(r'#\$([0-9a-fA-F]+), (d[0-7])')
STORE = re.compile(r'(d[0-7]|#\$([0-9a-fA-F]+)), \$([0-9a-fA-F]+)\.l')
IMM = re.compile(r'#\$([0-9a-fA-F]+)')
PEA = re.compile(r'\$([0-9a-fA-F]+)(?:\.l)?')


def in_code(value, code):
    return value % 2 == 0 and any(lo <= value < hi for lo, hi in code)


def prologue(data, base, value):
    """-> 'lea', 'link' or 'movem' if value looks like a function entry.

    The shapes every compiled function in these images opens with: a stack
    cut `lea -N(a7),a7` (0x4fef with a negative displacement), a `link.w
    aN,#-d` (0x4e50-0x4e57), or a `movem.l` register save (0x48e7 to
    -(a7), 0x48d7 to (a7)). Requiring one of these is what separates a
    pointer to code from a jump-table slot or a data address.
    """
    off = value - base
    if off < 0 or off + 4 > len(data):
        return None
    word = int.from_bytes(data[off:off + 2], 'big')
    if word == 0x4fef:
        return 'lea' if int.from_bytes(data[off + 2:off + 4], 'big') & 0x8000 else None
    if word in (0x48e7, 0x48d7):
        return 'movem'
    if 0x4e50 <= word <= 0x4e57:
        return 'link'
    return None


def scan(data, base, code):
    calls, vectors, pointers = [], [], []
    for lo, hi in code:
        loads = []  # (register, value), or None, for the last three instructions
        for pc, hx, mn, ops in disasm(data, base, lo, hi):
            taken = []
            if mn == 'pea':
                m = PEA.fullmatch(ops)
                if m:
                    taken.append(m)
            else:
                taken.extend(IMM.finditer(ops))
            for m in taken:
                value = int(m.group(1), 16)
                kind = prologue(data, base, value) if in_code(value, code) else None
                if kind is not None:
                    pointers.append({'site': pc, 'target': value, 'prologue': kind})
            if mn == 'jsr' or mn.startswith('bsr'):
                m = CALL.fullmatch(ops)
                if m and in_code(int(m.group(1), 16), code):
                    calls.append({'site': pc, 'target': int(m.group(1), 16)})
            m = STORE.fullmatch(ops) if mn == 'move.l' else None
            if m:
                slot = int(m.group(3), 16)
                handler = None
                if m.group(2):
                    handler = int(m.group(2), 16)
                else:
                    for load in reversed(loads):
                        if load and load[0] == m.group(1):
                            handler = load[1]
                            break
                if (VBR <= slot < VECTORS_END and slot % 4 == 0
                        and handler is not None and in_code(handler, code)):
                    vectors.append({'site': pc, 'handler': handler,
                                    'vector': (slot - VBR) // 4})
            m = LOAD.fullmatch(ops) if mn == 'move.l' else None
            loads = (loads + [(m.group(2), int(m.group(1), 16)) if m else None])[-3:]
    return {'base': base, 'code_ranges': [list(r) for r in code],
            'calls': calls, 'vectors': vectors, 'pointers': pointers}


def parse_args(argv=None):
    p = argparse.ArgumentParser(description='Find call targets and vector handlers.')
    p.add_argument('image')
    p.add_argument('--base', type=lambda s: int(s, 0), default=BASE)
    p.add_argument('--code', action='append', nargs=2, type=lambda s: int(s, 0),
                   metavar=('LO', 'HI'), help='code range, repeatable')
    p.add_argument('--json', required=True)
    return p.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    with open(args.image, 'rb') as f:
        data = f.read()
    code = [tuple(r) for r in args.code] if args.code else list(CODE)
    report = scan(data, args.base, code)
    report['image'] = args.image
    os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
    with open(args.json, 'w') as f:
        json.dump(report, f, indent=1)
    targets = {c['target'] for c in report['calls']}
    ptargets = {p['target'] for p in report['pointers']}
    print('%d calls to %d targets, %d vector-table writes, '
          '%d code pointers to %d targets (%d of them not a call target)'
          % (len(report['calls']), len(targets), len(report['vectors']),
             len(report['pointers']), len(ptargets), len(ptargets - targets)))
    for v in report['vectors']:
        print('  %#010x writes vector %d = %#010x' % (v['site'], v['vector'], v['handler']))
    return 0


if __name__ == '__main__':
    sys.exit(main())
