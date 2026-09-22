#!/usr/bin/env python3
"""Relocate the UI's source machine-list table into the MAIN OS cave, live.

Milestone A of adding an eighth machine.
`docs/findings/02-machines-and-parameters.md`'s "The ColdFire
machine dispatch" section established that the UI's *source* machine list is
seven big-endian u32 at rodata `0x401e1958`-`0x401e1974` (`{0,1,2,3,6,4,5}`),
copied into a `std::vector<int>` by `FUN_40051fbc` via two `pea` bounds at
`0x40052000` (END) and `0x4005200a` (START). This tool patches a resumed
snapshot, before `FUN_40051fbc` runs, so that:

  - a new 8-entry table `{0,1,2,3,6,4,5,7}` is written into the MAIN OS cave
    at `0x402f9c14` (58,188 zero bytes in the static image, confirmed clear
    of the one runtime-written byte at `0x402fa193`);
  - the two `pea` operands are repointed at the cave copy instead of the
    original rodata table.

It then proves the firmware actually reads the relocated copy -- and not the
original -- by installing `mmiotrace.py`'s `CountingSink` over both the cave
range and the original table's rodata range, and reporting which one saw
read traffic.

Index 7 has no eighth descriptor entry, so it falls back to descriptor entry
6 (the existing "MANUAL SLICE" collapses index 6 and would-be index 7 to the
same descriptor). Expect the new row to render as a second MANUAL SLICE.
That is the correct, unsurprising result for Milestone A: this proves the
list-relocation plumbing works, not that a new machine type exists yet.

Milestone B (`--milestone b`) goes further: it installs an eighth machine
*descriptor*, reached via a cave trampoline that replaces `FUN_400caf48`
(the ColdFire machine dispatch at `0x400caf48`). Types 0..6 resolve exactly
as before (delegated back to the original `0x42923644` array), type 7
resolves to a new 44-byte descriptor built in the cave (fields copied from
entry 6, MANUAL SLICE, but with distinct name-string reps), and anything
out of range still falls back to entry 6, matching the original's forgiving
failure mode. It then unit-tests the patched dispatch directly, by calling
`0x400caf48` in the live guest for arguments 0..8 and checking D0 against
the expected descriptor address for each.

Bisecting Milestone B's halves independently: `--parts list` alone (the
8-entry table, dispatch left unpatched) fails to boot; `--parts dispatch`
alone boots fine. So the failure is caused by the list gaining an eighth
entry, not by the trampoline, the descriptor, or the cave writes. That
turned out to be specifically the value 7: `FUN_4005d7b8` maps machine
type to a UI group id, and mapped type 7 to group 0, a group nothing else
uses. `--parts group` patches its exact-equality bound test into a range
test so 7 lands in the same group as 6. `--parts name` installs an eighth
row in the display-name table (`FUN_400dcc50`) so the new machine gets its
own "Placeholder"/"PLC" strings instead of falling back to an existing
entry's.

`--parts rank` extends the sort comparator's ordering. `FUN_40051fbc`
stable-sorts the list with `FUN_400517c4`, which ranks machine types
through a function-local `std::map<int,int>` holding keys 0..6 only; with
type 7 in the list, `map::at(7)` throws `std::out_of_range` and boot ends
in `std::terminate`. The patch redirects the map's one-time range insert
at `0x40051872` to a cave shim that supplies eight `(type, position)`
pairs instead of seven.

`--parts permit` patches `FUN_400dcab8`, the permission check used by the
machine setter `FUN_40050cd6` and six other callers (assign, list
availability, sound load, paste, sound locks). It rejects type > 6 with
`moveq #6,D2` and reads a 7-long mask table at `0x401fbda6`; the patch
raises the bound to 7 and points the lea at an 8-long copy in cave B at
`+0x1a0` whose eighth entry is `clone_of`'s. Without it, selecting the
machine calls the commit with type 7 but the track keeps its old type.

`--parts hint` bounds and repoints the other two accessors of the name table,
`FUN_400dcc76` (short name) and `FUN_400dcc9c` (the header hint shown after a
trig, e.g. "Y:Slice Menu"; type 7 got "ERROR"), and gives row 8 `clone_of`'s
hint. It needs `name`. `--parts pertype` moves the 7-byte table at
`0x401d9f30` to cave B `+0x1c0` with `clone_of`'s byte as the eighth and
raises the `moveq #6` bound in its six readers. `--parts clone` sends each
firmware test of `type == clone_of` through a cave shim at `+0x300` that also
accepts 7: the Slice menu entry (`FUN_4005f0c0`), the page layout test
`(type & ~2) == 4` (`FUN_4005cb7c`), the step count (`FUN_4005be94`),
parameter 0xfc (`FUN_4003065a`, `FUN_40048660`) and a per-track loop
(`FUN_4005edd6`). Only SLICE's tests are known; for another `clone_of` the
part writes nothing.

`--parts both` (the default) applies all nine parts. `--eighth`
exists to tell "eight entries is too many" apart from "the value 7 is the
problem": run with `--parts list --eighth N` for some other N.

This tool patches **guest memory on a resumed snapshot only**. It does not
modify any file, does not touch the firmware image on disk, and produces
nothing flashable -- the patch evaporates when the emulator process exits.

The eighth machine's names, its display position and which stock descriptor
it clones are a `MachineSpec` (see `--machine NAME:SHORT[:CLONE_OF[:POSITION]]`
and `--fields`), defaulting to `DEFAULT_SPEC` ("Placeholder"/"PLC", cloned
from type 6). `plan_b()` is the pure planner underneath `patch_b()`: it takes
a `read(addr, n) -> bytes` callable instead of live guest memory, so the same
plan can be produced against a static MAIN OS image. Only one new machine
type is supported -- every bound this tool raises is raised to exactly 7
(`NEW_TYPE`).

Modelled on `tools/memdump.py` (resume/build, intro-handover spin loop,
argparse/JSON conventions) and `tools/mmiotrace.py` (`CountingSink`,
`install_mmio_trace`). As in both, `reg_read(UC_M68K_REG_SR)` is never
called between `emu_start` calls -- it clobbers condition codes on this
patched Unicorn build.

Usage:
    uv run python tools/machinepatch.py --syx Digitakt_II_OS1.15C.syx \\
        --snapshot snapshots/boot400M.snap --json out/machinepatch.json

    uv run python tools/machinepatch.py --syx Digitakt_II_OS1.15C.syx \\
        --snapshot snapshots/boot400M.snap --no-patch --json out/control.json
"""
import argparse
import json
import os
import struct
import sys
import time
from dataclasses import dataclass, replace

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from unicorn.m68k_const import UC_M68K_REG_A0, UC_M68K_REG_D0, UC_M68K_REG_PC

from addrtrace import load_main_image
from mmiotrace import CountingSink

import machineprofile

from emu import symbols
from emu.dtim import Dtims, Timers
from emu.harness import PAGE
from emu.longrun import build, spin
from emu.pit import Pits, intro_running

END_SITE = 0x40052000
START_SITE = 0x4005200a
END_WANT = bytes.fromhex('4879401e1974')
START_WANT = bytes.fromhex('4879401e1958')
TABLE_D_LO, TABLE_D_HI = 0x401e1958, 0x401e1973
ORIGINAL_TABLE = (0, 1, 2, 3, 6, 4, 5)
NEW_TABLE = ORIGINAL_TABLE + (7,)
DEFAULT_CAVE = 0x402f9c14
DEFAULT_EIGHTH = 7

DISPATCH = 0x400caf48
DISPATCH_WANT = bytes.fromhex('7206202f0004')
DESCRIPTOR_BASE = 0x42923644
DESCRIPTOR_STRIDE = 0x2c
FALLBACK_DESCRIPTOR = 0x4292374c
ENTRY6_FIELDS = (0xf8, 0xf9, 0, 0xfb, 0xfc, 0xfd, 0, 0xfe, 0x0a)
DESCRIPTOR_FIELDS = ENTRY6_FIELDS  # alias, kept for callers that used the old name
DEFAULT_CAVE_B = 0x40303e5c
TRAMP_OFF = 0x000
DESC_OFF = 0x100
LNAME_OFF = 0x140
LCHARS_OFF = 0x14c
SNAME_OFF = 0x160
SCHARS_OFF = 0x16c
TABLE_B_OFF = 0x180
SCRATCH_PAGE = 0x1ff00000
DISPATCH_SENTINEL = 0xdeadbee0

GROUP_ADDR = 0x4005d7ca
GROUP_WANT = bytes.fromhex('7206b2806604')
GROUP_NEW = bytes.fromhex('7207b2806504')

NAME_ADDR = 0x400dcc50
NAME_HEAD_WANT = bytes.fromhex('7206202f0004b2806514')
NAME_LEA_ADDR = 0x400dcc60
NAME_LEA_WANT = bytes.fromhex('41f9401fbc50')
NAME_TABLE_SRC = 0x401fbc50
NAME_TABLE_ROWS = 7
NAME_TABLE_ROW_BYTES = 12
NAME_TABLE_OFF = 0x200
LONGSTR_OFF = 0x280
SHORTSTR_OFF = 0x290

RANK_CALL = 0x40051872
RANK_CALL_WANT = bytes.fromhex('4eb940198948')
RANK_INSERT = 0x40198948
RANK_GUARD = 0x40984ce8
RANK_SHIM_OFF = 0x2a0
RANK_TABLE_OFF = 0x2c0

PERMIT_BOUND_ADDR = 0x400dcaba   # FUN_400dcab8: moveq #6,D2 -- rejects type > 6
PERMIT_BOUND_WANT = bytes.fromhex('7406')
PERMIT_LEA_ADDR = 0x400dcad0     # lea (0x401fbda6).l,A0 -- per-type mask table
PERMIT_TABLE_SRC = 0x401fbda6    # 7 longs; the first word of each is a track mask
PERMIT_LEA_WANT = bytes.fromhex('41f9') + struct.pack('>I', PERMIT_TABLE_SRC)
PERMIT_TABLE_OFF = 0x1a0         # 8 longs, between TABLE_B_OFF's 32 bytes and NAME_TABLE_OFF

# 'hint': the other two accessors of the name table, short name (+4) and
# header hint (+8). Same head as FUN_400dcc50, but `addi.l #table+column,D0`.
LABEL_FUNCS = ((0x400dcc76, 4), (0x400dcc9c, 8))
LABEL_ADDI_OFF = 0x12

# 'pertype': a 7-byte per-type table; byte 7 is the start of an unrelated string.
PERTYPE_TABLE_SRC = 0x401d9f30
PERTYPE_TABLE_OFF = 0x1c0
PERTYPE_SITES = (   # (moveq #6,Dn bound, lea (0x401d9f30).l,An) in each reader
    (0x400166fc, 0x40016702),   # FUN_400166b8
    (0x400178c6, 0x400178d0),   # FUN_40017828
    (0x40017d46, 0x40017d50),   # FUN_40017b56
    (0x40017d9a, 0x40017da8),   # FUN_40017b56, second read
    (0x4001709e, 0x400170a4),   # FUN_40017080
    (0x40016628, 0x40016648),   # FUN_40016624
)

# 'clone': the profile's `clone_sites` are the `type == 6` tests (SLICE) a
# clone must also match. Only the site address is per image; the replayed
# compare, the type register and both branch targets are read from the bytes
# at the site by decode_clone_site.
CLONE_SHIM_OFF = 0x300
CLONE_SITES_OF = 6
# FUN_4005cb7c (1.15C): `(type & ~2) == 4` matches types 4 and 6.
CLONE_MASK_HEAD = bytes.fromhex('72fdc0817204b280')


def _decode_bcc(read, at):
    """beq/bne at `at`, .b or .w -> (opcode byte, length, target)."""
    op, d8 = read(at, 2)
    if op not in (0x66, 0x67) or d8 == 0xff:
        raise SystemExit('machinepatch: %#010x is not beq/bne.b/.w (%02x%02x)'
                         % (at, op, d8))
    if d8 == 0:
        disp, = struct.unpack('>h', read(at + 2, 2))
        return op, 4, at + 2 + disp
    return op, 2, at + 2 + (d8 - 256 if d8 >= 0x80 else d8)


def decode_clone_site(read, site):
    """-> ('eq', (addr, original, prefix, type register, match, no-match))
    for `<4-byte prefix ending in cmp.l Dy,Dx>; beq/bne`, or
    ('mask', (addr, original, match, no-match)) for the mask test."""
    head = read(site, 8)
    if head == CLONE_MASK_HEAD:
        kind, plen = 'mask', 8
    else:
        cmp, = struct.unpack('>H', head[2:4])
        if cmp & 0xf1f8 != 0xb080:                      # cmp.l Dy,Dx
            raise SystemExit('machinepatch: %#010x is not a cmp.l Dy,Dx test (%s)'
                             % (site, head.hex()))
        kind, plen = 'eq', 4
    op, size, target = _decode_bcc(read, site + plen)
    end = site + plen + size
    match, nomatch = (target, end) if op == 0x67 else (end, target)
    want = read(site, plen + size).hex()
    if plen + size < 6:
        raise SystemExit('machinepatch: %#010x: %d bytes cannot hold a jmp.l'
                         % (site, plen + size))
    if kind == 'mask':
        return kind, (site, want, match, nomatch)
    return kind, (site, want, head[:4].hex(), cmp & 7, match, nomatch)


def clone_sites(read, sites):
    """The profile's (function, site) pairs -> (eq tuples, mask tuple or None)."""
    eq, mask = [], None
    for _function, site in sites:
        kind, t = decode_clone_site(read, site)
        if kind == 'eq':
            eq.append(t)
        elif mask is not None:
            raise SystemExit('machinepatch: two mask sites, %#010x and %#010x'
                             % (mask[0], site))
        else:
            mask = t
    return eq, mask

PARTS = ('list', 'dispatch', 'group', 'name', 'rank', 'permit', 'hint', 'pertype', 'clone')

NEW_TYPE = 7    # the one new machine type this tool installs


class Anchors:
    """The image-specific addresses `plan_b` patches, taken from a profile.

    `tools/machineprofile.py` holds these per image, keyed by MAIN OS SHA-256.
    Its `checks` tuple already carries the expected bytes at the nine
    load-bearing sites -- the same values this module kept as separate `*_WANT`
    constants -- so both the address and the bytes come from one `checks` entry
    and cannot drift apart.

    Cave offsets are deliberately not here. `TRAMP_OFF` and its siblings are
    this tool's own scratch layout, not firmware structure, so they stay
    module constants and are the same on every image.

    An anchor the profile does not carry is None rather than absent, and
    `require()` turns that into an error naming the part and the image. That is
    how Digitone II, which has no sort comparator and no per-type table, says
    so instead of planning a write to address None.
    """

    # (address attribute, expected-bytes attribute, check label, profile key).
    # The address comes from the named profile field where there is one, so an
    # image whose anchors are known but whose bytes have not been checked yet
    # still plans; the expected bytes come from `checks` and are None without
    # one. `name table lea` has no named field, so that site is reachable only
    # through its check -- a profile gap worth closing if another image needs it.
    FROM_CHECKS = (
        ('DISPATCH', 'DISPATCH_WANT', 'dispatch head', 'dispatch'),
        ('END_SITE', 'END_WANT', 'list end pea', 'list_end_site'),
        ('START_SITE', 'START_WANT', 'list start pea', 'list_start_site'),
        ('GROUP_ADDR', 'GROUP_WANT', 'group bound', 'group_bound'),
        ('NAME_ADDR', 'NAME_HEAD_WANT', 'name accessor head', None),
        ('NAME_LEA_ADDR', 'NAME_LEA_WANT', 'name table lea', None),
        ('RANK_CALL', 'RANK_CALL_WANT', 'rank insert jsr', 'rank_call'),
        ('PERMIT_BOUND_ADDR', 'PERMIT_BOUND_WANT', 'permit bound', 'permit_bound'),
        ('PERMIT_LEA_ADDR', 'PERMIT_LEA_WANT', 'permit table lea', 'permit_lea'),
    )

    # (profile key, attribute): the anchors that are a plain address or count.
    FROM_FIELDS = (
        ('descriptor_base', 'DESCRIPTOR_BASE'),
        ('descriptor_stride', 'DESCRIPTOR_STRIDE'),
        ('fallback_descriptor', 'FALLBACK_DESCRIPTOR'),
        ('list_source', 'TABLE_D_LO'),
        ('name_table', 'NAME_TABLE_SRC'),
        ('name_table_rows', 'NAME_TABLE_ROWS'),
        ('name_table_row_bytes', 'NAME_TABLE_ROW_BYTES'),
        ('rank_insert', 'RANK_INSERT'),
        ('rank_guard', 'RANK_GUARD'),
        ('permit_table', 'PERMIT_TABLE_SRC'),
        ('permit_table_rows', 'PERMIT_TABLE_ROWS'),
        ('pertype_table', 'PERTYPE_TABLE_SRC'),
        ('pertype_sites', 'PERTYPE_SITES'),
        ('cave_a', 'DEFAULT_CAVE'),
        ('cave_b', 'DEFAULT_CAVE_B'),
    )

    def __init__(self, profile):
        self.profile = profile
        self.name = profile['name']
        self.count = profile['machine_count']
        # The new machine takes the next type number after the stock ones, and
        # every bound this tool raises is raised to exactly that.
        self.NEW_TYPE = self.count
        checks = {label: (addr, bytes.fromhex(want))
                  for addr, want, label in profile['checks']}
        accessors = profile.get('name_accessors') or ()
        for addr_attr, want_attr, label, key in self.FROM_CHECKS:
            checked, want = checks.get(label, (None, None))
            addr = profile.get(key) if key else None
            if addr_attr == 'NAME_ADDR' and addr is None and accessors:
                addr = accessors[0][0]
            if addr is not None and checked is not None and addr != checked:
                raise SystemExit(
                    'machinepatch: %s disagrees with itself: %s is %#010x but '
                    'its %r check is at %#010x'
                    % (self.name, key or addr_attr, addr, label, checked))
            setattr(self, addr_attr, checked if addr is None else addr)
            setattr(self, want_attr, want)
        for key, attr in self.FROM_FIELDS:
            setattr(self, attr, profile.get(key))
        end = profile.get('list_source_end')
        self.TABLE_D_HI = None if end is None else end - 1
        # The other two name-table accessors, as (function, column offset).
        self.LABEL_FUNCS = tuple(
            (addr, table - self.NAME_TABLE_SRC) for addr, table in accessors[1:])
        # `moveq #count,D1; cmp.l D0,D1; bcs.s +4`: the range test that replaces
        # the stock equality against count - 1.
        self.GROUP_NEW = bytes([0x72, self.count]) + bytes.fromhex('b2806504')
        # Entry `clone_of`'s nine descriptor longs. The descriptor array is
        # bss, so these cannot be read from a static image; a profile that has
        # not had them captured from a run leaves this None and the caller must
        # pass spec.fields.
        self.ENTRY_FIELDS = profile.get('entry_fields')

    def require(self, part, *attrs):
        """SystemExit naming every anchor this image does not carry."""
        missing = [a for a in attrs if getattr(self, a, None) is None]
        if missing:
            raise SystemExit(
                'machinepatch: %s carries no %s, so the %r part cannot be '
                'planned for it. See machineprofile.PROFILES[...]["missing"].'
                % (self.name, ', '.join(missing), part))


def anchors_for(profile=None):
    """-> Anchors for a profile dict, or for Digitakt II 1.15C by default."""
    if profile is None:
        profile = machineprofile.DT2_115C
    if isinstance(profile, Anchors):
        return profile
    return Anchors(profile)


@dataclass(frozen=True)
class MachineSpec:
    """One new machine, type NEW_TYPE. Only one is supported: every bound this
    tool raises is raised to exactly 7."""
    name: str = 'Placeholder'      # display-name table, long; the UI upper-cases it
    short: str = 'PLC'             # display-name table, abbreviation
    desc_name: str = 'PLACEHOLDER'  # descriptor std::string rep, long
    desc_short: str = 'PLHD'       # descriptor std::string rep, short
    clone_of: int = 6              # stock type whose nine descriptor fields are copied
    fields: tuple = None           # nine longwords overriding clone_of's (rung 3)
    position: int = 7              # display position in the source list, 0..7


DEFAULT_SPEC = MachineSpec()

# The fixed cave layout's slot sizes: the descriptor reps' chars run from
# LCHARS_OFF/SCHARS_OFF to the next slot, 0x14 bytes each including the NUL;
# LONGSTR_OFF/SHORTSTR_OFF to the next slot are 0x10 bytes each including
# the NUL.
DESC_NAME_MAX = SNAME_OFF - LCHARS_OFF - 1
DESC_SHORT_MAX = TABLE_B_OFF - SCHARS_OFF - 1
NAME_MAX = SHORTSTR_OFF - LONGSTR_OFF - 1
SHORT_MAX = RANK_SHIM_OFF - SHORTSTR_OFF - 1


def spec_from_arg(value):
    """'NAME:SHORT[:CLONE_OF[:POSITION]]' -> MachineSpec. The descriptor's own
    names are the display names upper-cased."""
    parts = value.split(':')
    if not 2 <= len(parts) <= 4:
        raise SystemExit(
            'machinepatch: --machine wants NAME:SHORT[:CLONE_OF[:POSITION]], '
            'got %r' % value)
    name, short = parts[0], parts[1]
    clone_of = int(parts[2], 0) if len(parts) >= 3 else MachineSpec.clone_of
    position = int(parts[3], 0) if len(parts) >= 4 else MachineSpec.position
    return MachineSpec(name=name, short=short,
                        desc_name=name.upper(), desc_short=short.upper(),
                        clone_of=clone_of, position=position)


def validate_spec(spec, count=7):
    """SystemExit on a spec that cannot be installed on a `count`-machine image."""
    for field_name in ('name', 'short', 'desc_name', 'desc_short'):
        value = getattr(spec, field_name)
        if not value:
            raise SystemExit('machinepatch: spec.%s must not be empty' % field_name)
        try:
            value.encode('ascii')
        except UnicodeEncodeError:
            raise SystemExit('machinepatch: spec.%s must be pure ASCII, got %r'
                             % (field_name, value))
    if len(spec.desc_name) > DESC_NAME_MAX:
        raise SystemExit('machinepatch: spec.desc_name %r is longer than %d '
                         'chars' % (spec.desc_name, DESC_NAME_MAX))
    if len(spec.desc_short) > DESC_SHORT_MAX:
        raise SystemExit('machinepatch: spec.desc_short %r is longer than %d '
                         'chars' % (spec.desc_short, DESC_SHORT_MAX))
    if len(spec.name) > NAME_MAX:
        raise SystemExit('machinepatch: spec.name %r is longer than %d chars'
                         % (spec.name, NAME_MAX))
    if len(spec.short) > SHORT_MAX:
        raise SystemExit('machinepatch: spec.short %r is longer than %d chars'
                         % (spec.short, SHORT_MAX))
    if spec.clone_of not in range(count):
        raise SystemExit('machinepatch: spec.clone_of must be 0..%d, got %r'
                         % (count - 1, spec.clone_of))
    if spec.position not in range(count + 1):
        raise SystemExit('machinepatch: spec.position must be 0..%d, got %r'
                         % (count, spec.position))
    if spec.fields is not None:
        if len(spec.fields) != 9 or not all(
                isinstance(f, int) and 0 <= f <= 0xffffffff for f in spec.fields):
            raise SystemExit('machinepatch: spec.fields must be 9 ints, each '
                             '0..0xffffffff, got %r' % (spec.fields,))


# These describe DEFAULT_SPEC only -- kept as module constants because
# tools/uidrive.py reads them.
LONG_NAME = DEFAULT_SPEC.desc_name
SHORT_NAME = DEFAULT_SPEC.desc_short
LONGSTR = DEFAULT_SPEC.name.encode('ascii') + b'\x00'
SHORTSTR = DEFAULT_SPEC.short.encode('ascii') + b'\x00'


def build_trampoline(cave_b, profile=None):
    """The cave dispatch: the new type gets the cave descriptor, a stock type
    indexes the original array, anything else falls back -- the same forgiving
    failure mode as the code it replaces."""
    a = anchors_for(profile)
    desc = cave_b + DESC_OFF
    return (
        bytes.fromhex('202f0004')
        + bytes([0x72, a.NEW_TYPE])                 # moveq #new,D1
        + bytes.fromhex('b280')
        + bytes.fromhex('6608')
        + bytes.fromhex('203c') + struct.pack('>I', desc)
        + bytes.fromhex('4e75')
        + bytes([0x72, a.count - 1])                # moveq #count-1,D1
        + bytes.fromhex('b280')
        + bytes.fromhex('6510')
        + bytes.fromhex('123c') + struct.pack('>H', a.DESCRIPTOR_STRIDE)
        + bytes.fromhex('4c010800')
        + bytes.fromhex('0680') + struct.pack('>I', a.DESCRIPTOR_BASE)
        + bytes.fromhex('4e75')
        + bytes.fromhex('203c') + struct.pack('>I', a.FALLBACK_DESCRIPTOR)
        + bytes.fromhex('4e75')
    )


def build_descriptor(cave_b, fields):
    return struct.pack('>11I', cave_b + LCHARS_OFF, cave_b + SCHARS_OFF,
                        *fields)


def build_rep(name):
    chars = name.encode('ascii') + b'\x00'
    return struct.pack('>IIi', len(name), len(name), -1) + chars


def build_rank_table(order):
    """(type, display position) pairs, the same shape as FUN_400517c4's own
    seven-pair initialiser -- which is exactly this over ORIGINAL_TABLE."""
    return b''.join(struct.pack('>II', t, i) for i, t in enumerate(order))


def build_rank_shim(cave_b, table_len, rank_insert=RANK_INSERT):
    """Replace the insert's [begin, end) stack arguments, then tail-jump to it.

    Entered by the repointed jsr at RANK_CALL, so 4(a7) is the map, 8(a7)
    begin and 0xc(a7) end; the caller's `lea $20(a7), a7` discards both
    afterwards, so overwriting them is safe.

    `rank_insert` is the image's range insert (`a.RANK_INSERT`); the 1.15C
    default is only for callers without a profile.
    """
    table = cave_b + RANK_TABLE_OFF
    return (
        bytes.fromhex('2f7c') + struct.pack('>I', table) + bytes.fromhex('0008')
        + bytes.fromhex('2f7c') + struct.pack('>I', table + table_len)
        + bytes.fromhex('000c')
        + bytes.fromhex('4ef9') + struct.pack('>I', rank_insert)
    )


def build_eq_shim(prefix, reg, match, nomatch, new_type=NEW_TYPE):
    """Replay a site's 4-byte compare; go to `match` if it matched or the
    type is the new one."""
    assert len(prefix) == 4
    return (prefix
            + bytes.fromhex('67000012')                     # beq.w match
            + struct.pack('>HI', 0x0c80 | reg, new_type)    # cmpi.l #new,Dreg
            + bytes.fromhex('67000008')                     # beq.w match
            + bytes.fromhex('4ef9') + struct.pack('>I', nomatch)
            + bytes.fromhex('4ef9') + struct.pack('>I', match))


def build_mask_shim(clone_of, match, nomatch, new_type=NEW_TYPE):
    """FUN_4005cb7c: turn the new type into clone_of, then replay
    (type & ~2) == 4."""
    return (struct.pack('>HI', 0x0c80, new_type)            # cmpi.l #new,D0
            + bytes.fromhex('6602')                         # bne.b +2
            + bytes([0x70, clone_of])                       # moveq #clone_of,D0
            + bytes.fromhex('72fdc0817204b280')             # moveq #-3,D1; and.l D1,D0; moveq #4,D1; cmp.l D0,D1
            + bytes.fromhex('67000008')                     # beq.w match
            + bytes.fromhex('4ef9') + struct.pack('>I', nomatch)
            + bytes.fromhex('4ef9') + struct.pack('>I', match))


def check_parts(parts):
    unknown = [p for p in parts if p not in PARTS]
    if unknown or not parts:
        raise SystemExit('machinepatch: unknown part(s) %s; known parts are %s'
                         % (', '.join(repr(p) for p in unknown) or '(none given)',
                            '+'.join(PARTS)))


def plan_b(read, cave_b, parts=PARTS, eighth=None, spec=DEFAULT_SPEC,
           profile=None):
    """-> [(addr, old, new), ...] in write order.

    Every byte comes from `read(addr, n) -> bytes` or from the spec, and every
    address from `profile`, so the same plan can be applied to live guest
    memory or to a static MAIN OS image, for any image `machineprofile.py`
    carries anchors for. `profile` defaults to Digitakt II 1.15C, which is what
    every address in this module used to be hard-coded to.

    One image-specific value is still not in the profile: `ENTRY6_FIELDS`, the
    nine descriptor longs cloned when `spec.fields` is None. The descriptor
    array is bss, so it cannot be read from a static image and has to be
    captured from a run. Pass `spec.fields` explicitly for any image other
    than 1.15C.
    """
    a = anchors_for(profile)
    prof = profile if profile is not None else machineprofile.DT2_115C
    check_parts(parts)
    validate_spec(spec, a.count)
    if eighth is None:
        eighth = a.NEW_TYPE
    writes = []

    if 'dispatch' in parts:
        cur = read(a.DISPATCH, 6)
        if cur != a.DISPATCH_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.DISPATCH, cur.hex(), a.DISPATCH_WANT.hex()))
    if 'list' in parts:
        for site, want in ((a.END_SITE, a.END_WANT), (a.START_SITE, a.START_WANT)):
            cur = read(site, 6)
            if cur != want:
                raise SystemExit(
                    'machinepatch: %#010x holds %s, expected %s'
                    % (site, cur.hex(), want.hex()))
    if 'group' in parts:
        cur = read(a.GROUP_ADDR, len(a.GROUP_WANT))
        if cur != a.GROUP_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.GROUP_ADDR, cur.hex(), a.GROUP_WANT.hex()))
    if 'name' in parts:
        cur = read(a.NAME_ADDR, len(a.NAME_HEAD_WANT))
        if cur != a.NAME_HEAD_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.NAME_ADDR, cur.hex(), a.NAME_HEAD_WANT.hex()))
        cur = read(a.NAME_LEA_ADDR, len(a.NAME_LEA_WANT))
        if cur != a.NAME_LEA_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.NAME_LEA_ADDR, cur.hex(), a.NAME_LEA_WANT.hex()))
    if 'rank' in parts:
        cur = read(a.RANK_CALL, len(a.RANK_CALL_WANT))
        if cur != a.RANK_CALL_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.RANK_CALL, cur.hex(), a.RANK_CALL_WANT.hex()))
    if 'permit' in parts:
        cur = read(a.PERMIT_BOUND_ADDR, len(a.PERMIT_BOUND_WANT))
        if cur != a.PERMIT_BOUND_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.PERMIT_BOUND_ADDR, cur.hex(), a.PERMIT_BOUND_WANT.hex()))
        cur = read(a.PERMIT_LEA_ADDR, len(a.PERMIT_LEA_WANT))
        if cur != a.PERMIT_LEA_WANT:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (a.PERMIT_LEA_ADDR, cur.hex(), a.PERMIT_LEA_WANT.hex()))

    if spec.fields is not None:
        fields = spec.fields
    elif spec.clone_of == 6:
        fields = ENTRY6_FIELDS
    else:
        # The descriptor array is bss, so a static-image caller must pass
        # `fields` for any clone other than 6.
        fields = struct.unpack(
            '>9I', read(a.DESCRIPTOR_BASE + spec.clone_of * a.DESCRIPTOR_STRIDE + 8, 36))

    order = list(struct.unpack('>%dI' % a.count,
                               read(a.TABLE_D_LO, a.count * 4)))
    order.insert(spec.position, eighth)

    def add(addr, new):
        old = read(addr, len(new))
        writes.append((addr, old, new))

    if 'dispatch' in parts:
        add(cave_b + TRAMP_OFF, build_trampoline(cave_b, a))
        add(cave_b + DESC_OFF, build_descriptor(cave_b, fields))
        add(cave_b + LNAME_OFF, build_rep(spec.desc_name))
        add(cave_b + SNAME_OFF, build_rep(spec.desc_short))

    if 'list' in parts:
        tbytes = struct.pack('>8I', *order)
        add(cave_b + TABLE_B_OFF, tbytes)
        for site, new_ptr in ((a.START_SITE, cave_b + TABLE_B_OFF),
                               (a.END_SITE, cave_b + TABLE_B_OFF + len(tbytes))):
            add(site + 2, struct.pack('>I', new_ptr))

    if 'group' in parts:
        add(a.GROUP_ADDR, a.GROUP_NEW)

    if 'name' in parts:
        table_addr = cave_b + NAME_TABLE_OFF
        long_addr = cave_b + LONGSTR_OFF
        short_addr = cave_b + SHORTSTR_OFF
        longstr = spec.name.encode('ascii') + b'\x00'
        shortstr = spec.short.encode('ascii') + b'\x00'

        rows = read(a.NAME_TABLE_SRC, a.NAME_TABLE_ROWS * a.NAME_TABLE_ROW_BYTES)
        add(table_addr, rows)

        row8 = struct.pack('>III', long_addr, short_addr, 0)
        row8_addr = table_addr + a.NAME_TABLE_ROWS * a.NAME_TABLE_ROW_BYTES
        add(row8_addr, row8)

        add(long_addr, longstr)
        add(short_addr, shortstr)

        # The bound is the moveq's IMMEDIATE, the second byte of `72 06`, not
        # the opcode byte -- writing at NAME_ADDR itself destroys the
        # instruction.
        add(a.NAME_ADDR + 1, b'\x07')

        add(a.NAME_LEA_ADDR + 2, struct.pack('>I', table_addr))

    if 'rank' in parts:
        table = build_rank_table(order)
        add(cave_b + RANK_TABLE_OFF, table)

        shim = build_rank_shim(cave_b, len(table), a.RANK_INSERT)
        add(cave_b + RANK_SHIM_OFF, shim)

        # The jsr's operand only, not its opcode.
        add(a.RANK_CALL + 2, struct.pack('>I', cave_b + RANK_SHIM_OFF))

    if 'dispatch' in parts:
        add(a.DISPATCH, b'\x4e\xf9' + struct.pack('>I', cave_b))

    if 'permit' in parts:
        slot = cave_b + PERMIT_TABLE_OFF
        old = read(slot, 32)
        if old != b'\x00' * 32:
            raise SystemExit(
                'machinepatch: cave slot %#010x is not free (holds %s)'
                % (slot, old.hex()))
        stock = read(a.PERMIT_TABLE_SRC, 28)
        table = stock + stock[spec.clone_of * 4:spec.clone_of * 4 + 4]
        writes.append((slot, old, table))
        writes.append((a.PERMIT_LEA_ADDR, a.PERMIT_LEA_WANT,
                       bytes.fromhex('41f9') + struct.pack('>I', slot)))
        writes.append((a.PERMIT_BOUND_ADDR, a.PERMIT_BOUND_WANT,
                       bytes([0x74, a.NEW_TYPE])))

    if 'hint' in parts:
        if 'name' not in parts:
            raise SystemExit('machinepatch: hint needs name, whose relocated '
                             'table it fills')
        table_addr = cave_b + NAME_TABLE_OFF
        for func, column in a.LABEL_FUNCS:
            addi = bytes.fromhex('0680') + struct.pack('>I', a.NAME_TABLE_SRC + column)
            for addr, want in ((func, a.NAME_HEAD_WANT), (func + LABEL_ADDI_OFF, addi)):
                cur = read(addr, len(want))
                if cur != want:
                    raise SystemExit('machinepatch: %#010x holds %s, expected %s'
                                     % (addr, cur.hex(), want.hex()))
            add(func + 1, bytes([a.NEW_TYPE]))
            add(func + LABEL_ADDI_OFF + 2, struct.pack('>I', table_addr + column))
        # The name part leaves row 8's hint 0; give it clone_of's.
        add(table_addr + a.NAME_TABLE_ROWS * a.NAME_TABLE_ROW_BYTES + 8,
            read(a.NAME_TABLE_SRC + spec.clone_of * a.NAME_TABLE_ROW_BYTES + 8, 4))

    if 'pertype' in parts:
        slot = cave_b + PERTYPE_TABLE_OFF
        old = read(slot, 8)
        if old != bytes(8):
            raise SystemExit('machinepatch: cave slot %#010x is not free (holds %s)'
                             % (slot, old.hex()))
        for bound, lea in a.PERTYPE_SITES:
            cur = read(bound, 2)
            if cur[0] & 0xf1 != 0x70 or cur[1] != 6:
                raise SystemExit('machinepatch: %#010x holds %s, expected moveq #6,Dn'
                                 % (bound, cur.hex()))
            cur = read(lea, 6)
            if (cur[0] & 0xf1 != 0x41 or cur[1] != 0xf9
                    or cur[2:] != struct.pack('>I', a.PERTYPE_TABLE_SRC)):
                raise SystemExit('machinepatch: %#010x holds %s, expected lea (%#010x).l,An'
                                 % (lea, cur.hex(), a.PERTYPE_TABLE_SRC))
        stock = read(a.PERTYPE_TABLE_SRC, 7)
        add(slot, stock + stock[spec.clone_of:spec.clone_of + 1])
        for bound, lea in a.PERTYPE_SITES:
            add(bound + 1, bytes([a.NEW_TYPE]))
            add(lea + 2, struct.pack('>I', slot))

    if 'clone' in parts:
        eq, mask = clone_sites(read, prof['clone_sites']
                               if spec.clone_of == CLONE_SITES_OF else ())
        sites = [(addr, bytes.fromhex(want),
                  build_eq_shim(bytes.fromhex(prefix), reg, match, nomatch,
                                a.NEW_TYPE))
                 for addr, want, prefix, reg, match, nomatch in eq]
        if mask is not None:
            addr, want, match, nomatch = mask
            sites.append((addr, bytes.fromhex(want),
                          build_mask_shim(spec.clone_of, match, nomatch,
                                          a.NEW_TYPE)))
        shim = cave_b + CLONE_SHIM_OFF
        total = sum(len(code) for _, _, code in sites)
        old = read(shim, total)
        if old != bytes(total):
            raise SystemExit('machinepatch: cave slot %#010x is not free (holds %s)'
                             % (shim, old.hex()))
        for addr, want, code in sites:
            cur = read(addr, len(want))
            if cur != want:
                raise SystemExit('machinepatch: %#010x holds %s, expected %s'
                                 % (addr, cur.hex(), want.hex()))
            add(shim, code)
            jump = bytes.fromhex('4ef9') + struct.pack('>I', shim)
            add(addr, jump + bytes.fromhex('4e71') * ((len(want) - len(jump)) // 2))
            shim += len(code)

    return writes


def patch_b(m, cave_b, parts=PARTS, eighth=None, spec=DEFAULT_SPEC):
    check_parts(parts)

    if 'rank' in parts:
        # The map is a function-local static: once its guard is set the
        # insert never runs again, and repointing it would change nothing.
        guard = bytes(m.uc.mem_read(RANK_GUARD, 1))
        if guard != b'\x00':
            raise SystemExit(
                'machinepatch: rank map already built (guard %#010x = %s); '
                'patch before FUN_40051fbc runs' % (RANK_GUARD, guard.hex()))

    m.ensure(cave_b)

    read = lambda addr, n: bytes(m.uc.mem_read(addr, n))
    writes = plan_b(read, cave_b, parts, eighth, spec)

    lines = []
    for addr, old, new in writes:
        m.uc.mem_write(addr, new)
        lines.append('%#010x  %s -> %s' % (addr, old.hex(), new.hex()))
    return lines


def call_dispatch(m, arg, scratch_sp, sentinel):
    uc = m.uc
    saved_d = [uc.reg_read(UC_M68K_REG_D0 + i) for i in range(8)]
    saved_a = [uc.reg_read(UC_M68K_REG_A0 + i) for i in range(8)]
    saved_pc = uc.reg_read(UC_M68K_REG_PC)

    uc.mem_write(scratch_sp, struct.pack('>I', sentinel))
    uc.mem_write(scratch_sp + 4, struct.pack('>I', arg))
    uc.reg_write(UC_M68K_REG_A0 + 7, scratch_sp)

    uc.emu_start(DISPATCH, sentinel)
    d0 = uc.reg_read(UC_M68K_REG_D0) & 0xffffffff

    for i in range(8):
        uc.reg_write(UC_M68K_REG_D0 + i, saved_d[i])
        uc.reg_write(UC_M68K_REG_A0 + i, saved_a[i])
    uc.reg_write(UC_M68K_REG_PC, saved_pc)
    return d0


def read_cstr(m, addr, limit=64):
    chars = bytearray()
    for i in range(limit):
        b = bytes(m.uc.mem_read(addr + i, 1))[0]
        if b == 0:
            break
        chars.append(b)
    return chars.decode('latin1')


def read_rep(m, addr):
    length, cap, refcount = struct.unpack('>IIi', bytes(m.uc.mem_read(addr, 12)))
    chars = bytes(m.uc.mem_read(addr + 12, length))
    return {
        'length': length,
        'capacity': cap,
        'refcount': refcount,
        'text': chars.decode('latin1'),
    }


def patch(m, cave_addr):
    lines = []
    for site, want in ((END_SITE, END_WANT), (START_SITE, START_WANT)):
        cur = bytes(m.uc.mem_read(site, 6))
        if cur != want:
            raise SystemExit(
                'machinepatch: %#010x holds %s, expected %s'
                % (site, cur.hex(), want.hex()))

    m.ensure(cave_addr)
    new_bytes = struct.pack('>8I', *NEW_TABLE)
    old_cave = bytes(m.uc.mem_read(cave_addr, len(new_bytes)))
    m.uc.mem_write(cave_addr, new_bytes)
    lines.append('%#010x  %s -> %s' % (cave_addr, old_cave.hex(), new_bytes.hex()))

    for site, new_ptr in ((START_SITE, cave_addr), (END_SITE, cave_addr + 32)):
        old = bytes(m.uc.mem_read(site + 2, 4))
        new = struct.pack('>I', new_ptr)
        m.uc.mem_write(site + 2, new)
        lines.append('%#010x  %s -> %s' % (site + 2, old.hex(), new.hex()))

    return lines


def run(args):
    main_img, _ = load_main_image(args.syx)
    profile = symbols.resolve(main_img)

    m, ev, st, pc, inq, at = build(
        args.snapshot, syx=args.syx, unblock=True, softfloat=True,
        bitmap=True, dsp=True, slc=args.slc,
        sdgate=args.sdgate, esdhc=args.esdhc)

    patch_lines = []
    if not args.no_patch:
        patch_lines = patch(m, args.cave)

    sink = CountingSink()
    ranges = ((args.cave, args.cave + 31), (TABLE_D_LO, TABLE_D_HI))
    m.install_mmio_trace(sink, ranges=ranges, owned=False)

    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    phase = {'post_intro': not intro}

    if intro and profile.intro_done is not None:
        def handover(uc, a, size, data):
            pits.release()
            phase['post_intro'] = True
        at(profile.intro_done, handover)

    done, pc_, stop = 0, pc, 'limit'
    t0 = time.time()
    target = args.instrs
    while True:
        pc_, executed, stop = spin(m, pc_, max(target - done, args.chunk),
                                    pits=pits)
        done += executed
        if stop != 'limit':
            break
        if done >= target and phase['post_intro']:
            break
        if done >= target and not phase['post_intro']:
            if done >= args.max_instrs:
                break
            target = min(target + args.chunk, args.max_instrs)

    raw = bytes(m.uc.mem_read(args.cave, 32))
    table = list(struct.unpack('>8I', raw))

    def range_report(lo, hi):
        keys = [k for k in sink.first_pc if lo <= k[0] <= hi and k[1] == 'read']
        addrs = sorted(set(k[0] for k in keys))
        events = sum(v for k, v in _counts(sink, lo, hi))
        return {
            'range': '0x%08x-0x%08x' % (lo, hi),
            'events': events,
            'distinct_addrs': len(addrs),
            'first_pc': {'0x%08x' % a: '0x%08x' % sink.first_pc[(a, 'read')]
                         for a in addrs},
        }

    cave_report = range_report(args.cave, args.cave + 31)
    tabled_report = range_report(TABLE_D_LO, TABLE_D_HI)

    mode = 'control' if args.no_patch else 'patched'
    if mode == 'patched':
        cave_ok = cave_report['events'] > 0
        tabled_ok = tabled_report['events'] == 0
        if cave_ok and tabled_ok:
            verdict = 'PASS: relocated table read, original table D untouched'
        else:
            fails = []
            if not cave_ok:
                fails.append('relocated range saw zero reads')
            if not tabled_ok:
                fails.append('original table D range still saw reads')
            verdict = 'FAIL: ' + '; '.join(fails)
    else:
        verdict = ('control: original table D events=%d, cave events=%d'
                   % (tabled_report['events'], cave_report['events']))

    return {
        'syx': args.syx,
        'snapshot': args.snapshot,
        'mode': mode,
        'cave': '0x%08x' % args.cave,
        'patch_lines': patch_lines,
        'cave_range': cave_report,
        'table_d_range': tabled_report,
        'readback_table': ['0x%08x' % v for v in table],
        'reached_post_intro': phase['post_intro'],
        'instrs': done,
        'stop': stop,
        'elapsed_s': round(time.time() - t0, 1),
        'verdict': verdict,
    }


def _counts(sink, lo, hi):
    keys = [k for k in sink.first_pc if lo <= k[0] <= hi and k[1] == 'read']
    for key in keys:
        total = sum(bucket.get(key, 0) for bucket in sink.buckets)
        total += sink.cur.get(key, 0)
        yield key, total


def run_b(args):
    main_img, _ = load_main_image(args.syx)
    profile = symbols.resolve(main_img)

    m, ev, st, pc, inq, at = build(
        args.snapshot, syx=args.syx, unblock=True, softfloat=True,
        bitmap=True, dsp=True, slc=args.slc,
        sdgate=args.sdgate, esdhc=args.esdhc)

    patch_lines = []
    if not args.no_patch:
        patch_lines = patch_b(m, args.cave_b, parts=args.parts, eighth=args.eighth,
                              spec=args.spec)

    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    phase = {'post_intro': not intro}

    if intro and profile.intro_done is not None:
        def handover(uc, a, size, data):
            pits.release()
            phase['post_intro'] = True
        at(profile.intro_done, handover)

    done, pc_, stop = 0, pc, 'limit'
    t0 = time.time()
    target = args.instrs
    while True:
        pc_, executed, stop = spin(m, pc_, max(target - done, args.chunk),
                                    pits=pits)
        done += executed
        if stop != 'limit':
            break
        if done >= target and phase['post_intro']:
            break
        if done >= target and not phase['post_intro']:
            if done >= args.max_instrs:
                break
            target = min(target + args.chunk, args.max_instrs)

    mode = 'control' if args.no_patch else 'patched'
    table = []
    descriptor = None
    long_name = None
    short_name = None
    name_row8 = None

    if mode == 'patched':
        m.ensure(SCRATCH_PAGE)
        scratch_sp = SCRATCH_PAGE + PAGE - 0x100
        expected = {a: DESCRIPTOR_BASE + a * DESCRIPTOR_STRIDE for a in range(7)}
        expected[7] = (args.cave_b + DESC_OFF if 'dispatch' in args.parts
                        else FALLBACK_DESCRIPTOR)
        expected[8] = FALLBACK_DESCRIPTOR
        for a in range(9):
            got = call_dispatch(m, a, scratch_sp, DISPATCH_SENTINEL)
            table.append({
                'arg': a,
                'expected': '0x%08x' % expected[a],
                'actual': '0x%08x' % got,
                'match': got == expected[a],
            })

        desc_raw = bytes(m.uc.mem_read(args.cave_b + DESC_OFF, 44))
        descriptor = ['0x%08x' % w for w in struct.unpack('>11I', desc_raw)]
        long_name = read_rep(m, args.cave_b + LNAME_OFF)
        short_name = read_rep(m, args.cave_b + SNAME_OFF)

        if 'name' in args.parts:
            row8_addr = (args.cave_b + NAME_TABLE_OFF
                         + NAME_TABLE_ROWS * NAME_TABLE_ROW_BYTES)
            row8_raw = bytes(m.uc.mem_read(row8_addr, NAME_TABLE_ROW_BYTES))
            long_ptr, short_ptr, third_ptr = struct.unpack('>III', row8_raw)
            name_row8 = {
                'addr': '0x%08x' % row8_addr,
                'raw': row8_raw.hex(),
                'long_ptr': '0x%08x' % long_ptr,
                'short_ptr': '0x%08x' % short_ptr,
                'third_ptr': '0x%08x' % third_ptr,
                'long_str': read_cstr(m, long_ptr),
                'short_str': read_cstr(m, short_ptr),
            }

        dispatch_ok = all(row['match'] for row in table)
        if dispatch_ok and phase['post_intro']:
            verdict = 'PASS: all nine dispatch results match, reached post-intro'
        else:
            fails = []
            if not dispatch_ok:
                fails.append('dispatch mismatch on arg(s) %s'
                              % [row['arg'] for row in table if not row['match']])
            if not phase['post_intro']:
                fails.append('did not reach post-intro')
            verdict = 'FAIL: ' + '; '.join(fails)
    else:
        verdict = 'control: no patch applied, dispatch not exercised'

    return {
        'syx': args.syx,
        'snapshot': args.snapshot,
        'mode': mode,
        'cave_b': '0x%08x' % args.cave_b,
        'patch_lines': patch_lines,
        'dispatch_table': table,
        'descriptor': descriptor,
        'long_name': long_name,
        'short_name': short_name,
        'name_row8': name_row8,
        'reached_post_intro': phase['post_intro'],
        'instrs': done,
        'stop': stop,
        'elapsed_s': round(time.time() - t0, 1),
        'verdict': verdict,
    }


def print_report(report):
    print(json.dumps(report, indent=2))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--snapshot', required=True)
    ap.add_argument('--cave', type=lambda s: int(s, 0), default=DEFAULT_CAVE,
                     help='MAIN OS cave address to write the relocated table '
                          'into (default: 0x402f9c14)')
    ap.add_argument('--milestone', choices=('a', 'b'), default='a',
                     help='a: relocate the source table (default). '
                          'b: install an eighth machine descriptor via a '
                          'cave trampoline and unit-test the dispatch.')
    ap.add_argument('--cave-b', dest='cave_b', type=lambda s: int(s, 0),
                     default=DEFAULT_CAVE_B,
                     help='cave address for --milestone b (default: '
                          '0x40303e5c)')
    ap.add_argument('--parts',
                     choices=('list', 'dispatch', 'group', 'name', 'rank',
                              'permit', 'hint', 'pertype', 'clone', 'both'),
                     default='both',
                     help='which part of --milestone b to apply: the list '
                          'relocation, the dispatch trampoline, the group-id '
                          'range fix, the display-name table, the sort '
                          'comparator ranking, the permission-table bound, '
                          'the short-name and hint accessors, the per-type '
                          'byte table, the clone_of type tests, or both/all '
                          'nine (default: both)')
    ap.add_argument('--eighth', type=lambda s: int(s, 0), default=None,
                     help='value to write as the 8th entry of the relocated '
                          'machine list for --milestone b (default is the '
                          'new type, 7). Use this to distinguish "eight '
                          'entries is too many" from "the value 7 '
                          'specifically is the problem".')
    ap.add_argument('--machine', default=None,
                     help='NAME:SHORT[:CLONE_OF[:POSITION]] for the new '
                          'machine installed by --milestone b (default: '
                          'Placeholder/PLC, cloned from type 6, position 7)')
    ap.add_argument('--fields', default=None,
                     help='nine comma-separated ints overriding the cloned '
                          'descriptor fields (0..0xffffffff each)')
    ap.add_argument('--no-patch', action='store_true',
                     help='skip the patch, for a control run')
    ap.add_argument('--instrs', type=lambda s: int(s, 0), default=90_000_000,
                     help='instruction budget to run forward before '
                          'observing (default: 90M, enough for post-intro '
                          'handover from snapshots/boot400M.snap)')
    ap.add_argument('--max-instrs', type=lambda s: int(s, 0),
                     default=400_000_000,
                     help='hard ceiling; if handover has not happened by '
                          '--instrs, keep spinning in --chunk steps up to '
                          'this many instructions total')
    ap.add_argument('--chunk', type=lambda s: int(s, 0), default=5_000_000,
                     help='spin() chunk size for the extra post-budget '
                          'search for handover')
    ap.add_argument('--slc', action='store_true', default=True)
    ap.add_argument('--sdgate', dest='sdgate', action='store_true', default=True)
    ap.add_argument('--no-sdgate', dest='sdgate', action='store_false')
    ap.add_argument('--esdhc', dest='esdhc', action='store_true', default=True)
    ap.add_argument('--no-esdhc', dest='esdhc', action='store_false')
    ap.add_argument('--json', help='write the full report here')
    args = ap.parse_args(argv)
    args.parts = (PARTS
                  if args.parts == 'both' else (args.parts,))
    args.spec = spec_from_arg(args.machine) if args.machine else DEFAULT_SPEC
    if args.fields is not None:
        fields = tuple(int(x, 0) for x in args.fields.split(','))
        args.spec = replace(args.spec, fields=fields)

    report = run(args) if args.milestone == 'a' else run_b(args)
    if args.json:
        os.makedirs(os.path.dirname(os.path.abspath(args.json)) or '.',
                     exist_ok=True)
        with open(args.json, 'w') as fh:
            json.dump(report, fh, indent=2)
    print_report(report)
    if report['mode'] == 'control':
        return 0
    return 0 if report['verdict'].startswith('PASS') else 1


if __name__ == '__main__':
    sys.exit(main())
