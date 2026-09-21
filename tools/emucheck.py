#!/usr/bin/env python3
"""Deterministic post-boot milestone check: from a ladder rung, run a fixed
instruction budget and assert the things a human staring at the GUI would
call "it worked" -- the main screen (not a splash/dialog), a sane task
count, +Drive formatted, and no `unblock` pend faked against a semaphore
that has a guest task-code poster.

Unlike tools/bootcheck.py's MAIN_OS_RUNNING verdict (mainloop/job_pump/PIT3/
DTIM3/vector-208 marks -- whether the OS took over), this checks whether the
RUN ITSELF is trustworthy: a frame that only looks like progress and a pend
list that says `unblock` raced ahead of a real poster are both a false
MAIN_OS_RUNNING. Built as its own tool because the checks (panel frame
density, eSDHC overlay, `ev['satisfied_by_sem']`) do not fit run_arm's
existing observation dict without changing what bootcheck.py itself asserts.

Usage:
    uv run python tools/emucheck.py --device dt2 --syx Digitakt_II_OS1.16.syx \
        --sections out/sections/dt2-1.16 \
        --snapshot snapshots/dt2-1.16/boot400M.snap --instrs 600000000

    uv run python tools/emucheck.py --device dn2 --syx Digitone_II_OS1.11.syx \
        --sections out/sections/dn2-1.11 \
        --snapshot snapshots/dn2-1.11/boot400M.snap --instrs 600000000
"""
import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu import config, symbols
from emu.longrun import build, spin
from emu.pit import Pits, intro_running
from emu.dtim import Dtims, Timers
from emu import panel

# Measured on DT2 1.16 from snapshots/dt2-1.16/boot400M.snap (see
# out/stall-1.16/rerun-classified/): the "INITIALIZING +DRIVE..." and
# "FACTORY PROJECT >> +DRIVE..." splash/dialog frames (an icon plus one line
# of text on an otherwise empty 768x384 panel) set 6.5% of pixels; the real
# main/sample-edit page (icon row, knob row, labels) sets 23.1%. The
# threshold sits well clear of both.
MAIN_SCREEN_ON_FRACTION = 0.12

# A sector's worth of overlay bytes actually written to sector 0 -- the
# format's own boot/header sector -- is what "formatted" means here; see the
# module docstring for why this stops short of checking an exact filesystem
# signature.
DRIVE_HEADER_MIN_BYTES = 512


def frame_on_fraction(buf):
    """-> fraction of the 128x64 1bpp panel frame (see emu/panel.py's `read`)
    that is lit, using the same bit layout `panel.lit()` decodes."""
    if not buf:
        return 0.0
    return len(panel.lit(buf)) / float(panel.W * panel.H)


def run(args):
    profile = symbols.resolve(open(config.main_image(), 'rb').read())

    m, ev, st, pc, inq, at = build(
        args.snapshot, syx=args.syx, unblock=True, softfloat=True,
        bitmap=True, dsp=True, slc=True, sdgate=True, esdhc=True)

    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    if intro and profile.intro_done is not None:
        at(profile.intro_done, lambda uc, a, s, d: pits.release())

    last_frame = {'buf': None}
    if profile.panel_diff is not None and profile.fb_front is not None:
        def latch(uc, a, s, d):
            last_frame['buf'] = panel.read(m, profile.fb_front)
        at(profile.panel_diff, latch)

    pc, done, stop = spin(m, pc, args.instrs, chunk=args.chunk, pits=pits)

    # Force one more frame read at the end even if panel_diff never fired
    # again in the tail of the budget, so "no frame yet" isn't mistaken for
    # "blank frame".
    frame = last_frame['buf']
    frac = frame_on_fraction(frame) if frame else None

    overlay = ev.get('esdhc').card.overlay if ev.get('esdhc') else {}
    header_bytes = sum(1 for off in overlay if 0 <= off < DRIVE_HEADER_MIN_BYTES)

    skip = ev.get('unblock_skip', set())
    satisfied_by_sem = ev.get('satisfied_by_sem', {})
    # frame_sem is a KNOWN, documented exception, not a bug this check
    # should flag: it only joins `skip` when intro_done's hook fires, and on
    # a snapshot taken after the intro already finished (any ladder rung
    # here), that hook never fires again -- the intro's own exit loop still
    # pends the same semaphore afterwards (see emu/symbols.py's intro_park
    # comment) and unblock keeps faking it, harmlessly, for the rest of the
    # run. This is the "175 in the intro exit loop" gap from the task that
    # asked for this tool; the 1.16 site is the pend's return address,
    # confirmed against a guirun.py run of this same rung: 0x400d1930.
    known_residual = {profile.frame_sem} if profile.frame_sem is not None else set()
    # Any OTHER force-satisfied pend against a semaphore not on unblock_skip
    # is, by construction (see emu/longrun.py's `never_fake`), one this
    # build's give/give_b audit found no task-code or modelled-ISR poster
    # for -- i.e. legitimately faked. This check instead catches the case
    # that actually indicates a bug: unblock_skip itself failing to resolve
    # (a None profile symbol silently dropping a semaphore off the list) is
    # covered by tests/test_symbol_audit.py, not here.
    unexpected = {sem: n for sem, n in satisfied_by_sem.items()
                 if sem in skip and sem not in known_residual}

    reasons = []
    ok = True

    def check(name, cond, detail=''):
        nonlocal ok
        ok = ok and cond
        reasons.append('%s: %s%s' % ('PASS' if cond else 'FAIL', name,
                                     (' -- %s' % detail) if detail else ''))

    check('run completed the budget', stop == 'limit', 'stop=%r' % stop)
    check('main screen reached (non-dialog frame)',
          frac is not None and frac >= MAIN_SCREEN_ON_FRACTION,
          'on_fraction=%r (need >= %.2f)' % (frac, MAIN_SCREEN_ON_FRACTION))
    check('task count is plausible', len(ev['tasks']) >= args.min_tasks,
          'tasks=%d (need >= %d)' % (len(ev['tasks']), args.min_tasks))
    check('+Drive formatted (overlay header written)',
          header_bytes >= DRIVE_HEADER_MIN_BYTES,
          'header_bytes=%d/%d' % (header_bytes, DRIVE_HEADER_MIN_BYTES))
    check('no force-satisfied pend escaped unblock_skip',
          not unexpected, 'unexpected=%r' % unexpected)

    verdict = 'PASS' if ok else 'FAIL'
    print('%s %s: %d/%d instrs, tasks=%d, frame_on=%r, drive_header_bytes=%d, '
          'satisfied=%d (skip-listed sems=%d)'
          % (verdict, args.device, done, args.instrs, len(ev['tasks']), frac,
             header_bytes, ev.get('satisfied', 0), len(skip)))
    for r in reasons:
        print('  ' + r)
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--device', required=True, help='label for the report, e.g. dt2 or dn2')
    ap.add_argument('--syx', required=True)
    ap.add_argument('--sections', help='sets DT2_SECTIONS for this process')
    ap.add_argument('--snapshot', required=True, help='ladder rung to resume from')
    ap.add_argument('--instrs', type=int, default=600_000_000)
    ap.add_argument('--chunk', type=int, default=10_000_000)
    ap.add_argument('--min-tasks', type=int, default=6)
    args = ap.parse_args()
    if args.sections:
        os.environ['DT2_SECTIONS'] = args.sections
    return run(args)


if __name__ == '__main__':
    sys.exit(main())
