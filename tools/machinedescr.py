#!/usr/bin/env python3
"""Extract the ColdFire machine-descriptor tables from the 1.16 initializer.

`docs/findings/02-machines-and-parameters.md`'s "The ColdFire machine dispatch" section and
`tools/machineprofile.py`'s `DT2_116` profile establish the dispatch: a
44-byte-stride array of descriptors, each holding two name-string pointers
(+0, +4) followed by nine 4-byte ID fields (+8 .. +0x28, field n at
`+8+n*4`). The array is bss -- it does not exist in the static image -- and
is populated at boot by one large initializer function (`FUN_401bdee2` on
1.16) that writes every field as a literal immediate, either straight to
memory or through a register loaded a few instructions earlier. This tool
recovers that table statically by parsing the initializer's disassembly
text (from `tools/ghidradump.py`'s dump, `disasm/<entry>_FUN_<entry>.s`)
instead of running anything.

Per field, the tool finds the (last) instruction that stores to that
field's address and reports:

  - `clr`      -- `clr.l (addr).l`, value 0.
  - `imm`      -- `move.l #imm,(addr).l`, a literal straight to memory.
  - `reg:REG`  -- `move.l REG,(addr).l`; REG's value is then recovered by
                  scanning backward through the same straight-line function
                  for REG's most recent definition (`moveq`, `move.l #imm`,
                  `movea.w/l #imm`, or a `move.b #imm` byte patch on top of
                  one of those). A `jsr`/`bsr` crossed during that scan only
                  blocks resolution for the m68k GCC ABI's caller-saved
                  registers (D0/D1/A0/A1) -- D2/D3/A2-A6 are callee-saved
                  and observably survive calls in this function (e.g. A3 is
                  loaded once and reused as an argument across many
                  name-setting calls). A field that cannot be resolved this
                  way is reported as UNRESOLVED with the reason, never
                  guessed.

Each entry's two name fields are set through calls (`jsr (A2)`, a
string-install helper) rather than a direct store, so they cannot be found
by grepping for a store to the field address. Instead: find the `pea
(dest).l` that pushes the field's own address as an argument, then scan
backward for the nearest earlier `pea (source).l` whose operand -- resolved
against the raw image, not trusted from the Ghidra symbol name -- reads back
a real, non-empty, printable C string. `source` may be an auto-named
`s_NAME_ADDR[+OFFSET]` symbol or a plain `DAT_ADDR`/`0xADDR` that Ghidra
never recognised as a string; both are resolved the same way, by reading
bytes out of the raw MAIN OS image until a NUL.

Two tables are extracted by default, both anchored this session by reading
`FUN_400c8840`/`FUN_400c8862`'s bounds and bases directly:

  - the 7-entry machine-type descriptor table, base `0x4293b960`
    (`machineprofile.DT2_116['descriptor_base']`), dispatched by
    `FUN_400c8840`.
  - a second, 6-entry table immediately before it at `0x4293b858`,
    dispatched by the contiguous `FUN_400c8862` (bound `moveq #5`). Its
    decoded names (STATE VARIABLE/LOWPASS 4/EQUALIZER/COMB-/LEGACY LP-HP/
    COMB+) show it is the filter-type table, not a second machine list --
    it is included because the task that produced this tool asked for it
    by address, not because it shares the machine dispatch's semantics.

Also greps the raw image for each resolved name string and reports every
address it appears at, independent of the initializer parse, so the
entry<->name binding this tool infers can be cross-checked a second way.

Usage:
    uv run python tools/machinedescr.py
    uv run python tools/machinedescr.py --json out/machinedescr.json
    uv run python tools/machinedescr.py --check-entry6   # cross-check only
"""
import argparse
import glob
import json
import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import machineprofile  # noqa: E402

DEFAULT_DISASM_DIR = os.path.join(REPO_ROOT, 'out', 'ghidra', 'dt2-1.16-seeded')
DEFAULT_IMAGE = os.path.join(REPO_ROOT, 'out', 'sections', 'dt2-1.16', 'section_3_MAIN_OS.bin')
INITIALIZER_ADDR = 0x401bdee2
LOAD_ADDR = machineprofile.LOAD_ADDR

# Table A: the machine-type descriptors (machineprofile.DT2_116).
TABLE_A_BASE = machineprofile.DT2_116['descriptor_base']
TABLE_A_COUNT = machineprofile.DT2_116['machine_count']
TABLE_A_STRIDE = machineprofile.DT2_116['descriptor_stride']
assert machineprofile.DT2_116['fallback_descriptor'] == (
    TABLE_A_BASE + (TABLE_A_COUNT - 1) * TABLE_A_STRIDE)

# Table B: the second, contiguous dispatch (`FUN_400c8862`, bound `moveq
# #5`), immediately before table A in memory. Not in machineprofile.py --
# established this session by reading FUN_400c8862's disasm directly; not
# a machine-type table (see module docstring).
TABLE_B_BASE = 0x4293b858
TABLE_B_COUNT = 6
TABLE_B_STRIDE = TABLE_A_STRIDE
assert TABLE_B_BASE + TABLE_B_COUNT * TABLE_B_STRIDE == TABLE_A_BASE

FIELD_COUNT = 9
FIELD_OFFSET = 8

# entry 6 (MANUAL SLICE)'s nine descriptor longs, captured live on 1.15C
# (tools/machinepatch.py's ENTRY6_FIELDS). 1.15C and 1.16 share the same
# dispatch shape (machinepatch.py's Anchors docstring), so this is this
# tool's cross-check, not an assumption it relies on.
ENTRY6_FIELDS_115C = (0xf8, 0xf9, 0, 0xfb, 0xfc, 0xfd, 0, 0xfe, 0x0a)

LINE_RE = re.compile(r'^([0-9a-f]{8})  (.{24})  (.*)$')

STORE_RE = re.compile(
    r'^(move\.[lbw]|clr\.[lbw])\s*(?:(D[0-7]|A[0-7]|#(-?0x[0-9a-f]+)),)?'
    r'\((?:DAT_)?(?:0x)?([0-9a-fA-F]{6,8})\)\.l$')
PEA_RE = re.compile(r'^pea\s+\(([^)]+)\)\.l$')
OFFSET_SUFFIX_RE = re.compile(r'^(.*)\+(0x[0-9a-fA-F]+|\d+)$')

FULLDEF_RE = re.compile(r'^(move\.l|movea\.l)\s+#(-?0x[0-9a-f]+),(D[0-7]|A[0-7])$')
MOVEQ_RE = re.compile(r'^moveq\s+#?(-?0x[0-9a-f]+),(D[0-7])$')
MOVEA_W_RE = re.compile(r'^movea\.w\s+#(-?0x[0-9a-f]+),(A[0-7])$')
WORD_RE = re.compile(r'^move\.w\s+#(-?0x[0-9a-f]+),(D[0-7])w$')
BYTE_RE = re.compile(r'^move\.b\s+#(-?0x[0-9a-f]+),(D[0-7])b$')
CLR_FULL_RE = re.compile(r'^clr\.l\s+(D[0-7]|A[0-7])$')
CLR_WORD_RE = re.compile(r'^clr\.w\s+(D[0-7])w?$')
CLR_BYTE_RE = re.compile(r'^clr\.b\s+(D[0-7])b?$')
JSR_RE = re.compile(r'^(jsr|bsr)\b')
REGDEF_TAIL_RE = re.compile(r'.*[,\s](D[0-7]|A[0-7])$')

# m68k GCC ABI: D0/D1/A0/A1 are caller-saved (scratch, clobbered by any
# call); D2-D7/A2-A6 are callee-saved. Observed directly in this function:
# A3 is loaded once and reused as an argument across many name-setting
# calls, and a D2 def survives a jsr between it and its use. A backward
# register scan only treats a jsr as a wall for the caller-saved four.
CALLER_SAVED = {'D0', 'D1', 'A0', 'A1'}

REG_SCAN_WINDOW = 2000
NAME_SCAN_WINDOW = 120


def find_initializer_file(disasm_dir, addr):
    """disasm_dir may be a ghidradump.py dump root (holding disasm/) or the
    disasm/ directory itself."""
    for base in (disasm_dir, os.path.join(disasm_dir, 'disasm')):
        hits = glob.glob(os.path.join(base, '%08x_*.s' % addr))
        if hits:
            return hits[0]
    raise SystemExit('machinedescr: no disasm file for %#010x under %s (or its disasm/)'
                     % (addr, disasm_dir))


def load_lines(path):
    out = []
    with open(path) as f:
        for raw in f:
            m = LINE_RE.match(raw.rstrip('\n'))
            if m:
                out.append((int(m.group(1), 16), m.group(3).strip()))
    return out


def parse_imm(text):
    neg = text.startswith('-')
    v = int(text[1:] if neg else text, 16)
    return -v if neg else v


def op_to_addr(op):
    """A `pea`/store operand ('s_NAME_ADDR[+OFF]', 'DAT_ADDR', '0xADDR') ->
    the address it names. The '+OFF' split only fires on a genuine trailing
    numeric addend -- some string symbols have a literal '+' in the name
    itself (e.g. `s_COMB+_402412aa`), so a plain str.partition('+') would
    misparse those."""
    m = OFFSET_SUFFIX_RE.match(op)
    base, offtxt = (m.group(1), m.group(2)) if m else (op, None)
    if base.startswith('s_'):
        hexpart = base.rsplit('_', 1)[-1]
    elif base.startswith('DAT_'):
        hexpart = base[4:]
    elif base.startswith('0x'):
        hexpart = base[2:]
    else:
        raise ValueError('unrecognized operand base %r' % base)
    addr = int(hexpart, 16)
    if offtxt:
        addr += int(offtxt, 16) if offtxt.startswith('0x') else int(offtxt, 10)
    return addr


class Image:
    def __init__(self, path, load_addr=LOAD_ADDR):
        with open(path, 'rb') as f:
            self.data = f.read()
        self.load_addr = load_addr
        self.path = path

    def read_cstr(self, addr, limit=80):
        off = addr - self.load_addr
        if off < 0 or off >= len(self.data):
            return None
        end = self.data.find(b'\x00', off, off + limit)
        if end < 0:
            return None
        raw = self.data[off:end]
        if not raw or not all(32 <= b < 127 for b in raw):
            return None
        try:
            return raw.decode('ascii')
        except UnicodeDecodeError:
            return None

    def find_all(self, s):
        """-> every guest address a NUL-terminated ASCII string occurs at."""
        needle = s.encode('ascii') + b'\x00'
        out = []
        start = 0
        while True:
            i = self.data.find(needle, start)
            if i < 0:
                break
            out.append(self.load_addr + i)
            start = i + 1
        return out


def resolve_register(lines, idx, reg):
    """Backward reaching-definition scan from lines[idx-1] for reg's value,
    as a bitmask merge: a `move.b`/`.w` immediate only pins the bits its
    width covers, so a def found further back only fills bits a *nearer*
    partial write hasn't already pinned (which is what real execution does,
    since the nearer write executes later and simply overwrites those bits
    again). -> (value, def_addr, note) or (None, None, reason)."""
    value = 0
    known = 0  # bitmask of bits already pinned by a nearer write
    patch_notes = []
    i = idx - 1
    steps = 0

    def merge(imm, mask, addr, label):
        nonlocal value, known
        new_bits = mask & ~known
        value |= imm & new_bits
        known |= mask
        patch_notes.append('%s %#x at %#010x' % (label, imm & mask, addr))

    while i >= 0 and steps < REG_SCAN_WINDOW:
        addr, text = lines[i]
        steps += 1
        if JSR_RE.match(text):
            if reg in CALLER_SAVED:
                return None, None, (
                    'crossed a call (jsr/bsr) at %#010x before finding a '
                    'full definition of caller-saved %s (bits known so far: '
                    '%#010x)' % (addr, reg, known))
            i -= 1
            continue

        m = FULLDEF_RE.match(text)
        if m and m.group(3) == reg:
            merge(parse_imm(m.group(2)) & 0xffffffff, 0xffffffff, addr, 'full def')
            return value, addr, ', '.join(patch_notes)
        m = MOVEQ_RE.match(text)
        if m and m.group(2) == reg:
            merge(parse_imm(m.group(1)) & 0xffffffff, 0xffffffff, addr, 'moveq')
            return value, addr, ', '.join(patch_notes)
        m = MOVEA_W_RE.match(text)
        if m and m.group(2) == reg:
            # movea always defines the full 32-bit address register,
            # sign-extending the 16-bit source.
            imm16 = parse_imm(m.group(1)) & 0xffff
            full = imm16 if imm16 < 0x8000 else imm16 - 0x10000
            merge(full & 0xffffffff, 0xffffffff, addr, 'movea.w')
            return value, addr, ', '.join(patch_notes)
        m = CLR_FULL_RE.match(text)
        if m and m.group(1) == reg:
            merge(0, 0xffffffff, addr, 'clr.l')
            return value, addr, ', '.join(patch_notes)

        m = WORD_RE.match(text)
        if m and m.group(2) == reg:
            merge(parse_imm(m.group(1)) & 0xffff, 0xffff, addr, 'move.w')
            i -= 1
            continue
        m = CLR_WORD_RE.match(text)
        if m and m.group(1) == reg:
            merge(0, 0xffff, addr, 'clr.w')
            i -= 1
            continue
        m = BYTE_RE.match(text)
        if m and m.group(2) == reg:
            merge(parse_imm(m.group(1)) & 0xff, 0xff, addr, 'move.b')
            i -= 1
            continue
        m = CLR_BYTE_RE.match(text)
        if m and m.group(1) == reg:
            merge(0, 0xff, addr, 'clr.b')
            i -= 1
            continue

        m = REGDEF_TAIL_RE.match(text)
        if (m and m.group(1) == reg
                and not text.startswith(('move.b', 'move.w', 'moveq', 'clr.'))):
            return None, None, (
                'defined via an unmodelled instruction at %#010x: %r '
                '(bits already pinned by nearer partial writes: %#010x)'
                % (addr, text, known))
        i -= 1

    if known:
        return None, None, (
            'reached the scan window with only partial bits pinned (%#010x); '
            'no full definition of %s found (%s)'
            % (known, reg, ', '.join(patch_notes)))
    return None, None, ('no definition of %s found within %d instructions '
                        'before the store' % (reg, REG_SCAN_WINDOW))


def resolve_name_source(lines, image, dest_idx, window=NAME_SCAN_WINDOW):
    """Nearest preceding `pea` whose operand reads back a real string from
    the image. -> (src_addr, src_op, resolved_addr, string) or a best-effort
    (src_addr, src_op, resolved_addr, None) if none of the candidates in the
    window read as a string."""
    i = dest_idx - 1
    seen = 0
    first = None
    while i >= 0 and seen < window:
        addr, text = lines[i]
        m = PEA_RE.match(text)
        if m:
            try:
                a = op_to_addr(m.group(1))
            except ValueError:
                a = None
            s = image.read_cstr(a) if a is not None else None
            if first is None:
                first = (addr, m.group(1), a, s)
            if s:
                return addr, m.group(1), a, s
        i -= 1
        seen += 1
    return first if first else (None, None, None, None)


def find_dest_pea(lines, target_addr):
    found = None
    for i, (addr, text) in enumerate(lines):
        m = PEA_RE.match(text)
        if not m:
            continue
        try:
            if op_to_addr(m.group(1)) == target_addr:
                found = i
        except ValueError:
            continue
    return found


def walk_entry(lines, image, base):
    entry = {'address': base, 'names': {}, 'fields': []}
    for off, tag in ((0, 'name1'), (4, 'name2')):
        slot = base + off
        found = find_dest_pea(lines, slot)
        if found is None:
            entry['names'][tag] = {'address': slot, 'status': 'no dest pea found'}
            continue
        src_addr, src_op, resolved_addr, s = resolve_name_source(lines, image, found)
        entry['names'][tag] = {
            'address': slot,
            'dest_pea': lines[found][0],
            'src_pea': src_addr,
            'src_operand': src_op,
            'resolved_addr': resolved_addr,
            'string': s,
            'status': 'ok' if s is not None else 'string unresolved',
        }
    for n in range(FIELD_COUNT):
        faddr = base + FIELD_OFFSET + 4 * n
        stores = []
        for i, (addr, text) in enumerate(lines):
            m = STORE_RE.match(text)
            if not m:
                continue
            if int(m.group(4), 16) != faddr:
                continue
            stores.append((i, addr, text, m))
        if not stores:
            entry['fields'].append({
                'n': n, 'address': faddr, 'source': 'none',
                'value': None, 'note': 'no store found'})
            continue
        i, addr, text, m = stores[-1]
        mnem, src = m.group(1), m.group(2)
        if mnem.startswith('clr'):
            entry['fields'].append({
                'n': n, 'address': faddr, 'source': 'clr',
                'value': 0, 'note': 'clr.l at %#010x' % addr})
        elif src and src.startswith('#'):
            entry['fields'].append({
                'n': n, 'address': faddr, 'source': 'imm-direct',
                'value': parse_imm(src[1:]) & 0xffffffff,
                'note': 'direct immediate at %#010x' % addr})
        else:
            val, defaddr, note = resolve_register(lines, i, src)
            entry['fields'].append({
                'n': n, 'address': faddr,
                'source': ('reg:%s' % src) if val is None else ('reg:%s' % src),
                'value': val,
                'note': note if val is not None else 'UNRESOLVED: ' + note,
            })
    return entry


def walk_table(lines, image, base, count, stride):
    return [walk_entry(lines, image, base + i * stride) for i in range(count)]


def grep_names(image, entries):
    out = {}
    for e in entries:
        for tag in ('name1', 'name2'):
            s = e['names'].get(tag, {}).get('string')
            if s and s not in out:
                out[s] = image.find_all(s)
    return out


def format_entry(idx, e):
    lines = ['  entry %d @ %#010x' % (idx, e['address'])]
    n1 = e['names']['name1']
    n2 = e['names']['name2']
    lines.append('    name1 @ %#010x -> %r (src %s, resolved %s)' % (
        n1['address'], n1.get('string'),
        n1.get('src_operand'),
        '%#010x' % n1['resolved_addr'] if n1.get('resolved_addr') else 'None'))
    lines.append('    name2 @ %#010x -> %r (src %s, resolved %s)' % (
        n2['address'], n2.get('string'),
        n2.get('src_operand'),
        '%#010x' % n2['resolved_addr'] if n2.get('resolved_addr') else 'None'))
    vals = []
    for f in e['fields']:
        if f['value'] is None:
            vals.append('f%d=UNRESOLVED(%s)' % (f['n'], f['note']))
        else:
            vals.append('f%d=%#x' % (f['n'], f['value']))
    lines.append('    fields: ' + ', '.join(vals))
    return '\n'.join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--disasm-dir', default=DEFAULT_DISASM_DIR,
                    help='ghidradump.py disasm/ directory (default: %(default)s)')
    ap.add_argument('--initializer', type=lambda s: int(s, 0), default=INITIALIZER_ADDR,
                    help='address of the initializer function (default: %#010x)'
                         % INITIALIZER_ADDR)
    ap.add_argument('--image', default=DEFAULT_IMAGE,
                    help='raw MAIN OS image, for resolving name strings '
                         '(default: %(default)s)')
    ap.add_argument('--table', action='append', metavar='BASE:COUNT',
                    help='override the tables to extract (repeatable); '
                         'default is the 7-entry machine table at %#010x and '
                         'the 6-entry table at %#010x' % (TABLE_A_BASE, TABLE_B_BASE))
    ap.add_argument('--stride', type=lambda s: int(s, 0), default=TABLE_A_STRIDE,
                    help='descriptor stride in bytes (default: %#x)' % TABLE_A_STRIDE)
    ap.add_argument('--json', metavar='PATH',
                    help='also write the full result as JSON to PATH')
    ap.add_argument('--check-entry6', action='store_true',
                    help='only run the entry-6 cross-check against '
                         'machinepatch.ENTRY6_FIELDS and exit')
    args = ap.parse_args(argv)

    disasm_path = find_initializer_file(args.disasm_dir, args.initializer)
    lines = load_lines(disasm_path)
    if not os.path.exists(args.image):
        raise SystemExit('machinedescr: no image at %s' % args.image)
    image = Image(args.image)

    if args.table:
        tables = []
        for spec in args.table:
            base_s, count_s = spec.split(':')
            tables.append(('table_%#x' % int(base_s, 0),
                           int(base_s, 0), int(count_s, 0)))
    else:
        tables = [
            ('table_a (machine types, %d entries)' % TABLE_A_COUNT,
             TABLE_A_BASE, TABLE_A_COUNT),
            ('table_b (filter types, %d entries)' % TABLE_B_COUNT,
             TABLE_B_BASE, TABLE_B_COUNT),
        ]

    result = {'disasm_file': disasm_path, 'image': args.image, 'tables': {}}
    for label, base, count in tables:
        result['tables'][label] = {
            'base': base, 'count': count, 'stride': args.stride,
            'entries': walk_table(lines, image, base, count, args.stride),
        }

    # Entry-6 cross-check: table_a's last entry (MANUAL SLICE) against the
    # live-captured 1.15C fields.
    entry6_ok = None
    entry6_got = None
    table_a_key = next((k for k in result['tables'] if k.startswith('table_a')
                        or result['tables'][k]['base'] == TABLE_A_BASE), None)
    if table_a_key:
        entries = result['tables'][table_a_key]['entries']
        if len(entries) >= TABLE_A_COUNT:
            e6 = entries[TABLE_A_COUNT - 1]
            entry6_got = tuple(f['value'] for f in e6['fields'])
            entry6_ok = entry6_got == ENTRY6_FIELDS_115C
    result['entry6_check'] = {
        'expected': ENTRY6_FIELDS_115C, 'got': entry6_got, 'ok': entry6_ok,
    }

    if args.check_entry6:
        print('entry6 expected: %s' % (ENTRY6_FIELDS_115C,))
        print('entry6 got:      %s' % (entry6_got,))
        print('MATCH' if entry6_ok else 'MISMATCH')
        return 0 if entry6_ok else 1

    # Independent string grep, over every resolved name across every table.
    all_entries = [e for t in result['tables'].values() for e in t['entries']]
    name_hits = grep_names(image, all_entries)
    result['name_grep'] = {s: [('%#010x' % a) for a in addrs]
                           for s, addrs in name_hits.items()}

    print('initializer: %s (%s)' % (disasm_path, '%#010x' % args.initializer))
    print('image: %s' % args.image)
    for label, tdata in result['tables'].items():
        print('\n=== %s: base %#010x, %d entries, stride %#x ===' % (
            label, tdata['base'], tdata['count'], tdata['stride']))
        for idx, e in enumerate(tdata['entries']):
            print(format_entry(idx, e))

    print('\n=== entry-6 cross-check (vs machinepatch.ENTRY6_FIELDS, 1.15C) ===')
    print('  expected: %s' % (ENTRY6_FIELDS_115C,))
    print('  got:      %s' % (entry6_got,))
    print('  %s' % ('MATCH' if entry6_ok else 'MISMATCH'))

    print('\n=== independent name-string grep (image, not the initializer) ===')
    for s, addrs in sorted(name_hits.items()):
        print('  %r: %s' % (s, ', '.join('%#010x' % a for a in addrs)))

    if args.json:
        with open(args.json, 'w') as f:
            json.dump(result, f, indent=2, default=str)
        print('\nwrote %s' % args.json)

    return 0 if entry6_ok else 1


if __name__ == '__main__':
    raise SystemExit(main())
