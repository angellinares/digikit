#!/usr/bin/env python3
"""Build a flashable .syx with an eighth machine baked into MAIN OS.

Wraps the steps `tools/machinepatch.py`'s `plan_b` already does for a
*static* image, plus the two things a flash-time build additionally needs
that `plan_b` documents but does not do itself:

  1. capture the ENTRY6 descriptor fields live, from a resumed post-boot
     snapshot -- the descriptor array is bss, so it cannot be read from the
     static MAIN OS image, and this tool refuses to guess it for an image
     other than the one `machinepatch.ENTRY6_FIELDS` was pinned against;
  2. plan all nine parts against the target profile, verify every write's
     `old` bytes against the image, apply them to a copy, and diff the
     result against the original: every changed byte must land either in a
     listed write or inside the profile's flash_cave. This is the same
     invariant `tests/test_machinepatch_profiles.py`'s
     `test_every_patch_site_belongs_to_this_image` checks, run here as a
     precondition before rebuilding anything.

Then it rebuilds the .syx (`dt2.build.rebuild`, replacing section 3 with the
patched bytes), and re-extracts the result to confirm section 3 equals the
patched copy and every other section is untouched -- the same shape as
`tools/roundtrip.py`'s acceptance gate, scoped to the one section this tool
changes.

    The plan is placed in the profile's `flash_cave`, not `cave_b`: `cave_b`
    is zeroed by the reset path (`FUN_400004b2`, from 0x40312000 up on
    Digitakt II 1.16) before the OS ever reaches it, so bytes baked into it
    would be gone before `main()` runs. `flash_cave` is free space that
    survives a cold boot. `cave_b` is only usable for live patches applied to
    an already-resumed snapshot -- see `tools/machinepatch.py`.

Usage:
    uv run python tools/machinebuild.py --syx Digitakt_II_OS1.16.syx \\
        --profile dt2-1.16 --fields-from snapshots/dt2-1.16/boot400M.snap \\
        --out out/Digitakt_II_OS1.16.eighth.syx

    # Only a subset of the nine parts (see machinepatch.PARTS):
    uv run python tools/machinebuild.py --syx Digitakt_II_OS1.16.syx \\
        --profile dt2-1.16 --fields-from snapshots/dt2-1.16/boot400M.snap \\
        --parts list,dispatch,group,name,rank --out out/eighth.syx
"""
import argparse
import hashlib
import os
import struct
import sys
from dataclasses import replace

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import machinepatch as mp
import machineprofile as mprof

PROFILE_BY_NAME = {
    'dt2-1.15C': mprof.DT2_115C,
    'dt2-1.16': mprof.DT2_116,
    'dn2-1.11': mprof.DN2_111,
}


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    with open(path, 'rb') as fh:
        return sha256_bytes(fh.read())


def capture_fields_live(syx, snapshot, profile, entry, sdgate, esdhc):
    """-> 9-tuple of the descriptor's ID fields for `entry`, read from a
    resumed snapshot. The descriptor array is bss and only exists once the
    firmware has run its startup init, so this needs a live guest, not the
    static image (see plan_b's docstring)."""
    sys.path.insert(0, REPO_ROOT)
    from emu.longrun import build

    m, ev, st, pc, inq, at = build(
        snapshot, syx=syx, unblock=True, softfloat=True, bitmap=True,
        dsp=True, slc=True, sdgate=sdgate, esdhc=esdhc)
    addr = profile['descriptor_base'] + entry * profile['descriptor_stride'] + 8
    raw = bytes(m.uc.mem_read(addr, 36))
    m.close()
    return struct.unpack('>9I', raw)


def _profile_addresses(profile):
    """-> set of every int found in `profile`'s values, recursively through
    tuples, lists and dicts."""
    out = set()

    def walk(v):
        if isinstance(v, bool):
            return
        if isinstance(v, int):
            out.add(v)
        elif isinstance(v, (tuple, list)):
            for x in v:
                walk(x)
        elif isinstance(v, dict):
            for x in v.values():
                walk(x)

    for v in profile.values():
        walk(v)
    return out


def audit_literals(writes, profile):
    """Fail on any word in the new bytes that is a Digitakt II 1.15C address
    (from DT2_115C or a machinepatch module constant) but not an address of
    `profile`. Copied pointers and branch targets decoded from the image are
    this image's own and pass. build_rank_shim once jumped to 1.15C's
    RANK_INSERT on every image; this finds that class in milliseconds
    instead of a hung boot."""
    if profile is mprof.DT2_115C:
        return
    allowed = _profile_addresses(profile)
    mp_constants = {v for k, v in vars(mp).items() if k.isupper() and isinstance(v, int)}
    fifteenc_allowed = _profile_addresses(mprof.DT2_115C) | mp_constants

    offenders = []
    for addr, old, new in writes:
        for i in range(0, len(new) - 3, 2):
            v = int.from_bytes(new[i:i + 4], 'big')
            if not (0x40000400 <= v < 0x40400000):
                continue
            if v in allowed:
                continue
            if len(old) >= i + 4 and old[i:i + 4] == new[i:i + 4]:
                continue
            if v not in fifteenc_allowed:
                continue
            offenders.append((addr + i, v))

    if offenders:
        lines = ['  %#010x: %#010x (1.15C address)' % (a, v) for a, v in offenders]
        raise SystemExit('machinebuild: literal address audit failed:\n'
                         + '\n'.join(lines))


def plan_and_verify(image_path, profile, spec, parts):
    """-> (writes, patched_bytes). Verifies every write's `old` against the
    image, applies to a copy, and diffs: every changed byte must be either a
    listed write or inside a cave. Raises SystemExit on any violation."""
    with open(image_path, 'rb') as f:
        image = bytearray(f.read())
    load_addr = mprof.LOAD_ADDR
    read = mprof.image_reader(image_path, load_addr)
    flash_cave_addr, flash_cave_size = profile['flash_cave']
    cave_b = profile['cave_b']
    cave_b_size = profile.get('cave_b_size', 16647)

    writes = mp.plan_b(read, flash_cave_addr, parts=parts, spec=spec, profile=profile)
    audit_literals(writes, profile)

    for addr, old, new in writes:
        got = read(addr, len(old))
        if got != old:
            raise SystemExit(
                'machinebuild: %#010x holds %s in the image, plan expected %s'
                % (addr, got.hex(), old.hex()))
        end = addr + len(new)
        if flash_cave_addr <= addr < flash_cave_addr + flash_cave_size and \
                end > flash_cave_addr + flash_cave_size:
            raise SystemExit(
                'machinebuild: write at %#010x-%#010x overruns flash_cave '
                '(%#010x-%#010x)' % (addr, end, flash_cave_addr,
                                     flash_cave_addr + flash_cave_size))
        if cave_b <= addr < cave_b + cave_b_size or \
                (addr < cave_b < end):
            raise SystemExit(
                'machinebuild: write at %#010x-%#010x lands in cave_b '
                '(%#010x-%#010x), which is zeroed at reset'
                % (addr, end, cave_b, cave_b + cave_b_size))

    patched = bytearray(image)
    for addr, old, new in writes:
        off = addr - load_addr
        if off < 0 or off + len(new) > len(patched):
            raise SystemExit('machinebuild: write at %#010x falls outside the image'
                             % addr)
        patched[off:off + len(new)] = new

    write_ranges = [(a, a + len(n)) for a, _o, n in writes]

    def in_a_write(a):
        return any(lo <= a < hi for lo, hi in write_ranges)

    def in_flash_cave(a):
        return flash_cave_addr <= a < flash_cave_addr + flash_cave_size

    bad = []
    for i in range(len(image)):
        if image[i] != patched[i]:
            addr = load_addr + i
            if not (in_a_write(addr) or in_flash_cave(addr)):
                bad.append(addr)
    if bad:
        raise SystemExit(
            'machinebuild: %d changed byte(s) landed outside every listed '
            'write and outside flash_cave, first at %#010x'
            % (len(bad), bad[0]))

    return writes, bytes(patched)


def rebuild_and_check(syx_path, patched_section3, out_path, extract_dir,
                       stock_sections_dir=None):
    """Rebuild the .syx with section 3 replaced, re-extract it, and confirm
    section 3 equals the patched bytes (and, if `stock_sections_dir` is
    given, every other section still equals stock). -> True if everything
    checked passes."""
    from dt2 import build as dbuild
    import roundtrip as rt

    out_bytes = dbuild.rebuild(syx_path, replacements={3: patched_section3})
    os.makedirs(os.path.dirname(os.path.abspath(out_path)) or '.', exist_ok=True)
    with open(out_path, 'wb') as f:
        f.write(out_bytes)

    ok = rt.check_authenticity(out_bytes, out_path)

    os.makedirs(extract_dir, exist_ok=True)
    rc = os.system('cd %s && uv run python -m emu.extract %s -o %s >/dev/null'
                    % (REPO_ROOT, out_path, extract_dir))
    if rc != 0:
        print('machinebuild: re-extract failed (exit %d)' % rc)
        return False

    from emu.extract import NAMES
    all_ok = ok
    for sid, name in NAMES.items():
        fn = 'section_%d_%s.bin' % (sid, name)
        got_path = os.path.join(extract_dir, fn)
        if not os.path.exists(got_path):
            print('  %s: MISSING from re-extraction' % fn)
            all_ok = False
            continue
        got_sha = sha256_file(got_path)
        if sid == 3:
            want_sha = sha256_bytes(patched_section3)
            label = 'patched section 3'
        elif stock_sections_dir:
            stock_path = os.path.join(stock_sections_dir, fn)
            if not os.path.exists(stock_path):
                continue
            want_sha = sha256_file(stock_path)
            label = 'stock section %d' % sid
        else:
            continue
        match = got_sha == want_sha
        all_ok = all_ok and match
        print('  %s vs %s: %s' % (fn, label, 'MATCH' if match else 'MISMATCH'))
    return all_ok


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True, help='stock source .syx')
    ap.add_argument('--profile', required=True, choices=sorted(PROFILE_BY_NAME),
                     help='which machineprofile.py entry to plan against')
    ap.add_argument('--fields-from', metavar='SNAPSHOT',
                     help='resume this snapshot to capture the live ENTRY6 '
                          'descriptor fields (required unless --fields is given '
                          'and --clone-of is 6 on Digitakt II 1.15C, whose '
                          'fields machinepatch.py already pins)')
    ap.add_argument('--fields', help='9 comma-separated ints, overriding '
                    '--fields-from')
    ap.add_argument('--clone-of', type=lambda s: int(s, 0), default=6,
                     help='stock machine type whose descriptor fields the new '
                          'machine clones (default: 6)')
    ap.add_argument('--machine', default=None,
                     help='NAME:SHORT[:CLONE_OF[:POSITION]] for the new '
                          'machine (default: Placeholder/PLC, cloned from '
                          'type 6, position 7). Overrides --clone-of.')
    ap.add_argument('--parts', default=None,
                     help='comma-separated subset of machinepatch.PARTS '
                          '(default: all nine)')
    ap.add_argument('--section3', help='override: use this image instead of '
                    'extracting section 3 from --syx (must already match --syx)')
    ap.add_argument('--sections-dir', help='pre-extracted stock sections dir, '
                    'to cross-check the other sections after rebuild')
    ap.add_argument('--out', required=True, help='rebuilt .syx path')
    ap.add_argument('--extract-dir', help='where to re-extract the rebuilt .syx '
                    'for the post-build check (default: alongside --out)')
    ap.add_argument('--no-sdgate', dest='sdgate', action='store_false', default=True)
    ap.add_argument('--no-esdhc', dest='esdhc', action='store_false', default=True)
    args = ap.parse_args(argv)

    profile = PROFILE_BY_NAME[args.profile]
    parts = tuple(args.parts.split(',')) if args.parts else mp.PARTS

    if args.fields:
        fields = tuple(int(x, 0) for x in args.fields.split(','))
    elif args.fields_from:
        print('capturing ENTRY%d fields live from %s ...' % (args.clone_of, args.fields_from))
        fields = capture_fields_live(args.syx, args.fields_from, profile,
                                      args.clone_of, args.sdgate, args.esdhc)
        print('  fields: %s' % (fields,))
    elif args.clone_of == 6 and profile is mprof.DT2_115C:
        fields = mp.ENTRY6_FIELDS
        print('using machinepatch.ENTRY6_FIELDS (pinned 1.15C values): %s' % (fields,))
    else:
        raise SystemExit('machinebuild: need --fields or --fields-from for this '
                         'profile/--clone-of combination (the descriptor array '
                         'is bss and cannot be read from the static image)')

    if args.machine:
        spec = replace(mp.spec_from_arg(args.machine), fields=fields)
    else:
        spec = mp.MachineSpec(clone_of=args.clone_of, fields=fields)

    if args.section3:
        section3_path = args.section3
    else:
        # Extract section 3 fresh from --syx into a scratch location next to
        # --out, so this tool needs no pre-existing sections/ directory.
        scratch = os.path.join(os.path.dirname(os.path.abspath(args.out)) or '.',
                               '.machinebuild-src-sections')
        os.makedirs(scratch, exist_ok=True)
        rc = os.system('cd %s && uv run python -m emu.extract %s -o %s >/dev/null'
                        % (REPO_ROOT, args.syx, scratch))
        if rc != 0:
            raise SystemExit('machinebuild: could not extract section 3 from %s'
                             % args.syx)
        section3_path = os.path.join(scratch, 'section_3_MAIN_OS.bin')

    print('planning %s against %s ...' % (', '.join(parts), profile['name']))
    writes, patched = plan_and_verify(section3_path, profile, spec, parts)
    print('  %d writes, all old-bytes verified, diff confined to writes+caves' % len(writes))

    extract_dir = args.extract_dir or (args.out + '.extracted')
    print('rebuilding %s -> %s ...' % (args.syx, args.out))
    ok = rebuild_and_check(args.syx, patched, args.out, extract_dir, args.sections_dir)

    print()
    print('machine: %s/%s, clone of type %d, position %d'
          % (spec.name, spec.short, spec.clone_of, spec.position))
    print('parts: %s' % (parts,))
    print('output: %s (%s)' % (args.out, 'OK' if ok else 'CHECKS FAILED'))
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
