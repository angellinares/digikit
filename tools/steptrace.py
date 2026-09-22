# pyright: reportMissingImports=false
# fmt: off
"""Trace what `spin` actually credits itself, pass by pass.

The GUI runs `spin(pits=..., fast=...)`. `fast` bounds each step with a block
hook instead of `count=`, and therefore has to *estimate* how many
instructions ran. If that estimate is wrong the emulated clock and the work
actually done come apart: timers stop coming due, the firmware grinds, and
nothing in the GUI says so -- it just gets slow.

This runs the GUI's machine, with the GUI's hooks and the GUI's pits, with no
window, and prints one line per `_FastStepper.run` call:

    step         instructions the deadline asked for
    blocks       basic-block entries the hook actually saw go by
    left         the entry budget still unspent when emu_start returned
                 (> 0 means emu_start stopped for some *other* reason)
    ret          instructions credited back to spin
    instr/block  the current instructions-per-block-entry estimate

plus a per-pass summary of wall time, credited instructions and timer ticks,
so `--fast` and `--exact` can be compared directly on the same snapshot.

    uv run python tools/steptrace.py SNAP --syx FW.syx --fast --passes 20
    uv run python tools/steptrace.py SNAP --syx FW.syx --exact --passes 20
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn import UC_HOOK_BLOCK, UC_HOOK_CODE
from unicorn.m68k_const import UC_M68K_REG_PC

from emu import config, longrun, symbols
from emu.dtim import Dtims, Timers
from emu.longrun import build, spin
from emu.pit import Pits, intro_running

BUDGET = 400_000          # emu/gui.py's own pass size


def instrument(detail):
    """Wrap _FastStepper.run so every call reports what it did. -> log list."""
    log = []
    original = longrun._FastStepper.run

    def traced(self, pc, step):
        t0 = time.time()
        ret = original(self, pc, step)
        row = {'step': step, 'blocks': self.blocks, 'left': self.left,
               'ret': ret, 'ratio': self.per_block, 'dt': time.time() - t0,
               'calibration': False}
        log.append(row)
        if detail:
            print('    step %-9d blocks %-9d left %-12d ret %-9d '
                  'instr/block %.3f  %.3fs%s'
                  % (row['step'], row['blocks'], row['left'], row['ret'],
                     row['ratio'], row['dt'],
                     '  [slow]' if row['dt'] > 0.5 else ''),
                  flush=True)
        return ret

    longrun._FastStepper.run = traced
    return log


def machine(snapshot, syx, weakptr=False, slc=False):
    """Build exactly what emu/gui.py builds. -> (m, ev, pc, pits, profile)."""
    px = [0]

    def on_pixel(x, y, val, bmp):
        px[0] += 1

    extra = {'syx': syx} if syx else {}
    m, ev, st, pc, inq, at = build(snapshot, unblock=True, softfloat=True,
                                   bitmap=True, dsp=True, on_pixel=on_pixel,
                                   weakptr=weakptr, slc=slc, **extra)
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    profile = symbols.resolve(open(config.main_image(), 'rb').read())

    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    if pits.held and profile.intro_done is not None:
        at(profile.intro_done, lambda uc, a, s, d: pits.release())

    # The same code hooks the GUI installs. They are part of the question:
    # a code hook changes how Unicorn translates, and _FastStepper reads its
    # estimate straight out of the translator's block sizes.
    marks = {'mainloop': 0, 'jobs': 0}
    if profile.mainloop is not None:
        at(profile.mainloop,
           lambda uc, a, s, d: marks.__setitem__('mainloop',
                                                 marks['mainloop'] + 1))
    if profile.job_pump is not None:
        at(profile.job_pump,
           lambda uc, a, s, d: marks.__setitem__('jobs', marks['jobs'] + 1))
    if profile.panel_diff is not None:
        at(profile.panel_diff, lambda uc, a, s, d: None)
    return m, ev, pc, pits, profile, px, marks


def hookprobe(snapshot, syx, count, weakptr=False, slc=False,
              warmup=False):
    """Measure how often UC_HOOK_BLOCK fires, counted against uncounted.

    `_FastStepper` calibrates by dividing block bytes by a *requested*
    instruction count, which is only meaningful if the block hook fires once
    per block in both modes. docs/findings/07-emulator.md, "What actually
    limits speed: our hook layer, not Unicorn", records that `count=` makes
    Unicorn install an internal per-instruction hook; if that also changes how
    often the block hook fires, the two arms of the calibration are measuring
    different things and the ratio it produces is not bytes per instruction.

    Two machines from the same snapshot at the same PC, one run counted and
    one run uncounted over a comparable stretch. -> nothing, prints.
    """
    for label in ('counted', 'uncounted', 'uncounted+code'):
        m, ev, pc, pits, profile, px, marks = machine(
            snapshot, syx, weakptr=weakptr, slc=slc)
        if warmup:
            # The number that matters is the one for the code the GUI spends
            # its time in, which is the main OS, not the intro.
            while pits.held:
                pc, done, stop = spin(m, pc, 4_000_000, pits=pits, fast=False)
                if stop != 'limit':
                    print('warmup halted: %s' % stop, flush=True)
                    return
        tally = {'calls': 0, 'bytes': 0, 'entries': 0, 'last': None,
                 'instrs': 0}

        def on_block(uc, addr, size, data, t=tally):
            t['calls'] += 1
            t['bytes'] += size
            if addr != t['last']:
                t['entries'] += 1
                t['last'] = addr
            if label == 'uncounted' and t['bytes'] >= count * 4:
                uc.emu_stop()

        m.uc.hook_add(UC_HOOK_BLOCK, on_block)
        if label == 'uncounted+code':
            # Ground truth. A code hook perturbs translation, but it is the
            # only way to learn how many instructions an UNCOUNTED run
            # executed -- and that, not the counted figure, is the number
            # _FastStepper's estimate has to be right about.
            def on_code(uc, addr, size, data, t=tally):
                t['instrs'] += 1
                if t['instrs'] >= count:
                    uc.emu_stop()
            m.uc.hook_add(UC_HOOK_CODE, on_code)
        t0 = time.time()
        if label == 'counted':
            m.uc.emu_start(pc, 0, count=count)
        else:
            m.uc.emu_start(pc, 0)
        dt = time.time() - t0
        known = count if label != 'uncounted' else 0
        print('%-15s  hook calls %-10d  bytes %-12d  bytes/call %6.2f  '
              'instrs %-10d  INSTR/BLOCK %s  %.3fs'
              % (label, tally['calls'], tally['bytes'],
                 tally['bytes'] / max(1, tally['calls']), known,
                 ('%7.3f' % (known / float(max(1, tally['calls']))))
                 if known else '      ?', dt), flush=True)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('snapshot')
    ap.add_argument('--syx')
    ap.add_argument('--fast', action='store_true',
                    help='block-bounded stepping (the GUI default)')
    ap.add_argument('--exact', action='store_true',
                    help='count= stepping, for comparison')
    ap.add_argument('--passes', type=int, default=20)
    ap.add_argument('--budget', type=int, default=BUDGET)
    ap.add_argument('--warmup', action='store_true',
                    help='run uncapped until the intro hands over, so the '
                         'traced passes see real PIT deadlines rather than '
                         'IDLE_STEP. No fixed cap: the handover is a hook '
                         'firing, not an instruction count.')
    ap.add_argument('--warmup-cap', type=int, default=400_000_000,
                    help='give up on the handover after this many instructions')
    ap.add_argument('--detail', action='store_true',
                    help='one line per _FastStepper.run call')
    ap.add_argument('--weakptr', action='store_true')
    ap.add_argument('--slc', action='store_true')
    ap.add_argument('--hookprobe', type=int, metavar='N',
                    help='measure UC_HOOK_BLOCK firing rate over N '
                         'instructions, counted against uncounted, and stop')
    args = ap.parse_args(argv)
    if args.hookprobe:
        hookprobe(args.snapshot,
                  config.firmware(args.syx) if args.syx else None,
                  args.hookprobe, weakptr=args.weakptr, slc=args.slc,
                  warmup=args.warmup)
        return 0
    if args.fast == args.exact:
        ap.error('pass exactly one of --fast / --exact')

    syx = config.firmware(args.syx) if args.syx else None
    log = instrument(args.detail) if args.fast else []
    m, ev, pc, pits, profile, px, marks = machine(
        args.snapshot, syx, weakptr=args.weakptr, slc=args.slc)
    print('snapshot %s   intro %s   pc %#010x'
          % (args.snapshot, 'running' if pits.held else 'done', pc), flush=True)

    if args.warmup:
        # The intro holds the timers, so every step is IDLE_STEP and nothing
        # about deadline stepping is exercised. Spin until the intro_done hook
        # releases them. A fixed instruction cap has burned several attempts
        # at this -- wait for the hook.
        w0, warm = time.time(), 0
        while pits.held and warm < args.warmup_cap:
            pc, executed, stop = spin(m, pc, 4_000_000, pits=pits,
                                      fast=args.fast)
            warm += executed
            if stop != 'limit':
                print('warmup halted: %s at %#010x after %dM'
                      % (stop, pc, warm // 1_000_000), flush=True)
                return 1
        print('warmup: intro %s after %dM instructions, %.1fs, pc %#010x'
              % ('handed over' if not pits.held else 'STILL RUNNING',
                 warm // 1_000_000, time.time() - w0, pc), flush=True)
        del log[:]                      # the trace is about the passes below

    t0 = time.time()
    credited = 0
    for n in range(args.passes):
        pt = time.time()
        mark = len(log)
        pc, executed, stop = spin(m, pc, args.budget, pits=pits,
                                  fast=args.fast)
        credited += executed
        dt = time.time() - pt
        fired = pits.fired
        calls = len(log) - mark
        print('pass %-3d  %8.3fs  credited %-9d total %-11d  calls %-6d '
              'PIT0 %-6d PIT2 %-6d PIT3 %-5d DTIM3 %-5d  px %-9d  %s'
              % (n, dt, executed, credited, calls,
                 fired.get('PIT0', 0), fired.get('PIT2', 0),
                 fired.get('PIT3', 0), fired.get('DTIM3', 0), px[0], stop),
              flush=True)
        if stop != 'limit':
            break

    wall = time.time() - t0
    print('\n%s: %d passes, %.2fs wall, %d instructions credited '
          '(%.2fM/s credited)'
          % ('fast' if args.fast else 'exact', args.passes, wall, credited,
             credited / wall / 1e6), flush=True)
    print('intro %s   mainloop %d   jobs %d   pixels %d   pc %#010x'
          % ('running' if pits.held else 'done', marks['mainloop'],
             marks['jobs'], px[0], m.uc.reg_read(UC_M68K_REG_PC)), flush=True)

    if log:
        early = [r for r in log if r['left'] > 0]
        short = [r for r in log if r['ret'] < r['step']]
        slow = [r for r in log if r['dt'] > 0.5]
        print('\n_FastStepper: %d calls, %d stopped early (left > 0), '
              '%d credited less than asked, %d took over half a second'
              % (len(log), len(early), len(short), len(slow)))
        if slow:
            print('  slowest call: asked %d, ran %d blocks, took %.3fs'
                  % (max(slow, key=lambda r: r['dt'])['step'],
                     max(slow, key=lambda r: r['dt'])['blocks'],
                     max(slow, key=lambda r: r['dt'])['dt']))
        if early:
            worst = min(early, key=lambda r: r['ret'] / max(1, r['step']))
            print('  worst early stop: asked %d, saw %d blocks, %d left, '
                  'credited %d' % (worst['step'], worst['blocks'],
                                   worst['left'], worst['ret']))
        asked = sum(r['step'] for r in log)
        got = sum(r['ret'] for r in log)
        print('  asked %d instructions in total, credited %d (%.1f%%)'
              % (asked, got, 100.0 * got / max(1, asked)))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
