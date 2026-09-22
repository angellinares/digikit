#!/usr/bin/env python3
"""Watch for evidence the UI consumed the Milestone B patch's new machine
string, then optionally try to drive the panel to the machine-select screen.

`docs/findings/02-machines-and-parameters.md`'s "The ColdFire machine
dispatch" section and
`tools/machinepatch.py` establish that Milestone B installs an eighth machine
descriptor in a MAIN OS cave (default base 0x40303e5c) whose two name fields
are libstdc++ COW string reps ("PLACEHOLDER" / "PLHD"), reached by
redirecting `FUN_400caf48`. What is NOT yet shown is that the running UI ever
reads any of it: `FUN_4005e022` (`MachineListView`) does not fire on an idle
post-intro run, and screen-based identification is documented as unreliable
on this build (see `docs/HANDOVER-2026-09-13.md`).

This tool reuses `tools/machinepatch.py`'s `patch_b()` verbatim (imported,
not duplicated) to install the patch on a resumed snapshot, then installs
three pixel-free signals and reports whether any of them fire:

  1. a code hook on `FUN_4005e022` (MachineListView) -- does the list-drawing
     routine run at all;
  2. an `install_mmio_trace` range watch over the cave descriptor
     (cave_b+0x100 .. +0x12b) and both name reps (length/capacity/refcount
     header plus chars) -- does anything read our bytes back;
  3. a code hook on `FUN_401d3aba`, the COW string copy constructor, that
     dereferences A1 (the source std::string's data pointer, per
     `docs/findings/03-ui-and-panel.md`) and reports a hit whenever it equals one of our two
     cave chars addresses -- does the UI actually clone our string.

A bonus fourth hook on `FUN_400607b2` (`MachineSelectionView`'s constructor)
is included for context; it is not one of the three required signals.

Every run should be compared against a `--no-patch` control on the same
snapshot: the cave addresses are only ever touched because the dispatch was
redirected there, so a control run must show zero hits on all four hooks.

`--drive` additionally injects a scripted button sequence through
`emu/panelin.py` (profile resolved via `symbols.resolve(main_img)`, the same
idiom `tools/panelsweep.py` uses around its own `build()` call) trying to
reach the machine-select screen, settling after each injection with
`tools/panelsweep.py`'s own `probe_wait` (reused, not reimplemented) in its
no-calibration mode, which drains until `queue_send` traffic quiets down --
the project's validated "has the UI reacted yet" signal. If any of the four
hooks' counts move during `--drive`, or if `--capture` is given, the
front-buffer is dumped as a PNG via `emu/panel.py`.

Never reads UC_M68K_REG_SR between emu_start calls (clobbers condition codes
on this Unicorn build -- see machinepatch.py, mmiotrace.py).

Usage:
    uv run python tools/uidrive.py --syx Digitakt_II_OS1.15C.syx \\
        --snapshot snapshots/boot400M.snap --json out/uidrive-patched.json

    uv run python tools/uidrive.py --syx Digitakt_II_OS1.15C.syx \\
        --snapshot snapshots/boot400M.snap --no-patch \\
        --json out/uidrive-control.json

    uv run python tools/uidrive.py --syx Digitakt_II_OS1.15C.syx \\
        --snapshot snapshots/boot400M.snap --drive \\
        --capture out/uidrive-fb.png --json out/uidrive-drive.json
"""
import argparse
import json
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from unicorn.m68k_const import UC_M68K_REG_A0, UC_M68K_REG_A7, UC_M68K_REG_PC

from addrtrace import load_main_image
from mmiotrace import CountingSink
import machinepatch as mp
import panelsweep as ps

from emu import panel, symbols
import emu.panelin as panelin
from emu.dtim import Dtims, Timers
from emu.longrun import build, spin
from emu.pit import Pits, intro_running

MACHINE_LIST_VIEW = 0x4005e022        # FUN_4005e022, never fires idle (docs/findings/02-machines-and-parameters.md)
MACHINE_SELECTION_VIEW = 0x400607b2   # FUN_400607b2, constructor -- bonus signal
COW_COPY = 0x401d3aba                 # FUN_401d3aba, the COW string copy ctor

# Digitakt button codes: channel*8 + bit + 1 (channels 0-5; see emu/panelin.py
# docstring -- channel 6 is not linear and not attempted here).
CODES = {
    'TRIG': 1, 'SRC': 2, 'FLTR': 3, 'AMP': 4, 'FX': 5, 'MOD': 6,
    'FUNC': 17, 'UP': 11, 'DOWN': 14, 'LEFT': 13, 'RIGHT': 15,
    'YES': 10, 'NO': 12,
}


def wire(code):
    return (code - 1) // 8, (code - 1) % 8


# --- signal instrumentation -------------------------------------------------

def install_signals(m, at, cave_b):
    """-> dict of counters/state, kept live for the whole run.

    `at` is longrun.build's begin==end code-hook registrar.
    """
    state = {
        'list_view_hits': 0,
        'list_view_first_instr': None,
        'selection_view_hits': 0,
        'cow_total_calls': 0,
        'cow_cave_hits': [],   # bounded sample of {caller, data_ptr, name}
    }
    long_chars = cave_b + mp.LCHARS_OFF
    short_chars = cave_b + mp.SCHARS_OFF
    cave_names = {long_chars: mp.LONG_NAME, short_chars: mp.SHORT_NAME}

    def on_list_view(uc, a, size, data):
        state['list_view_hits'] += 1

    def on_selection_view(uc, a, size, data):
        state['selection_view_hits'] += 1

    def on_cow(uc, a, size, data):
        state['cow_total_calls'] += 1
        try:
            a1 = uc.reg_read(UC_M68K_REG_A0 + 1)
            data_ptr = struct.unpack('>I', bytes(uc.mem_read(a1, 4)))[0]
        except Exception:
            return
        name = cave_names.get(data_ptr)
        if name is None:
            return
        caller = None
        try:
            sp = uc.reg_read(UC_M68K_REG_A7)
            caller = struct.unpack('>I', bytes(uc.mem_read(sp, 4)))[0]
        except Exception:
            pass
        if len(state['cow_cave_hits']) < 50:
            state['cow_cave_hits'].append({
                'caller': '0x%08x' % caller if caller is not None else None,
                'data_ptr': '0x%08x' % data_ptr,
                'name': name,
            })

    at(MACHINE_LIST_VIEW, on_list_view)
    at(MACHINE_SELECTION_VIEW, on_selection_view)
    at(COW_COPY, on_cow)

    sink = CountingSink()
    desc_lo, desc_hi = cave_b + mp.DESC_OFF, cave_b + mp.DESC_OFF + 0x2b
    long_lo, long_hi = cave_b + mp.LNAME_OFF, long_chars + len(mp.LONG_NAME)
    short_lo, short_hi = cave_b + mp.SNAME_OFF, short_chars + len(mp.SHORT_NAME)
    ranges = ((desc_lo, desc_hi), (long_lo, long_hi), (short_lo, short_hi))
    m.install_mmio_trace(sink, ranges=ranges, owned=False)

    state['sink'] = sink
    state['ranges'] = {
        'descriptor': (desc_lo, desc_hi),
        'long_rep': (long_lo, long_hi),
        'short_rep': (short_lo, short_hi),
    }
    state['long_chars'] = long_chars
    state['short_chars'] = short_chars
    return state


def mmio_range_report(sink, lo, hi):
    keys = [k for k in sink.first_pc if lo <= k[0] <= hi and k[1] == 'read']
    addrs = sorted(set(k[0] for k in keys))

    def total(key):
        return sink.cur.get(key, 0) + sum(b.get(key, 0) for b in sink.buckets)

    return {
        'range': '0x%08x-0x%08x' % (lo, hi),
        'events': sum(total(k) for k in keys),
        'distinct_addrs': len(addrs),
        'first_pc': {'0x%08x' % a: '0x%08x' % sink.first_pc[(a, 'read')]
                      for a in addrs},
        'pcs': {'0x%08x' % a: sorted('0x%08x' % p for p in sink.pcs[(a, 'read')])
                for a in addrs},
    }


def signal_report(state):
    sink = state['sink']
    ranges = state['ranges']
    return {
        'list_view_hits': state['list_view_hits'],
        'selection_view_hits': state['selection_view_hits'],
        'cow_total_calls': state['cow_total_calls'],
        'cow_cave_hits': state['cow_cave_hits'],
        'mmio': {
            name: mmio_range_report(sink, lo, hi)
            for name, (lo, hi) in ranges.items()
        },
    }


def any_signal_fired(state):
    """True iff one of the THREE REQUIRED signals fired (list_view hits, a
    COW copy sourced from our cave chars, or an mmio read of the cave
    descriptor/rep ranges). `selection_view_hits` is bonus context only --
    docs/findings/02-machines-and-parameters.md already establishes MachineSelectionView's constructor
    rebuilds the index vectors on every idle post-intro run regardless of
    the patch, so by itself it says nothing about string consumption and
    must not count toward this verdict."""
    if state['list_view_hits']:
        return True
    if state['cow_cave_hits']:
        return True
    for lo, hi in state['ranges'].values():
        if mmio_range_report(state['sink'], lo, hi)['events']:
            return True
    return False


def counter_snapshot(state):
    return (state['list_view_hits'], state['selection_view_hits'],
            len(state['cow_cave_hits']),
            tuple(mmio_range_report(state['sink'], lo, hi)['events']
                  for lo, hi in state['ranges'].values()))


# --- run scaffolding (mirrors machinepatch.run_b) ---------------------------

def reach_post_intro(m, at, profile, pc, instrs, max_instrs, chunk):
    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    phase = {'post_intro': not intro}

    if intro and profile.intro_done is not None:
        def handover(uc, a, size, data):
            pits.release()
            phase['post_intro'] = True
        at(profile.intro_done, handover)

    done, pc_, stop = 0, pc, 'limit'
    target = instrs
    while True:
        pc_, executed, stop = spin(m, pc_, max(target - done, chunk), pits=pits)
        done += executed
        if stop != 'limit':
            break
        if done >= target and phase['post_intro']:
            break
        if done >= target and not phase['post_intro']:
            if done >= max_instrs:
                break
            target = min(target + chunk, max_instrs)
    return pc_, pits, phase, done, stop


# --- navigation --------------------------------------------------------------

def wire_mask(code):
    """-> (channel, single-bit mask) for `code`."""
    channel, bit = wire(code)
    return channel, 1 << bit


def assert_channel(m, profile, pits, qs_state, channel, mask, probe_cap,
                    log=None, label=''):
    """Send one channel's WHOLE button byte and settle. -> new pc.

    This is the only way a cross-channel chord (FUNC + a page button, say)
    can be expressed: the wire carries one channel's full 8-bit state per
    message, and the firmware keeps whatever it last saw for a channel until
    a new message for THAT channel arrives (see emu/panelin.py's Held class
    docstring). So a modifier "held" across a different channel's press is
    not a parameter this API takes -- it is simply not sending a new message
    for the modifier's channel while the other channel changes. Passing
    mask=0x00 lets a caller go through this exact same code path to release
    a channel, so assert and release are symmetric and equally logged.

    Every call is logged with the queue_send record(s) it produced -- code,
    raw bytes, and the flag byte at +0x0b (button convention: 0x01 press,
    0x10 release, per tools/panelsweep.py) -- so a formed chord (or a
    momentary one that never lands) is visible directly in the trace, not
    just inferred from downstream silence.
    """
    qs_state['candidates'].clear()
    pc_ = panelin.buttons(m, profile, channel, mask)
    pc_, instrs, stop, recs = ps.probe_wait(m, pc_, pits, qs_state, probe_cap)
    if log:
        log('    buttons(ch=%d, mask=0x%02x) %s: instrs=%d stop=%s recs=%d' %
            (channel, mask, label, instrs, stop, len(recs)))
        for r in recs:
            raw = r['raw']
            log('      record: ret=0x%08x raw=%s type=%d code=0x%02x '
                'flag@0x0b=0x%02x' %
                (r['ret'], raw.hex(), raw[0], raw[7], raw[0x0b]))
    return pc_


def drive(m, at, profile, pc, pits, qs_state, sig_state, probe_cap, log):
    """Scripted attempt at the machine-select screen. -> (pc, steps).

    Every injection is an explicit `assert_channel` (a full channel byte,
    logged with its queue_send record), never a `press`/`release` pair that
    could be mistaken for latching a cross-channel modifier when it isn't
    one. Cross-channel chords hold their modifier channel asserted across
    the second channel's assert-then-release, exactly per the correction
    that prompted this rewrite: FUNC (channel 2) and the page buttons
    (channel 0) / YES-NO-UP-DOWN (channel 1) are different channels, so
    "held" is expressed by simply not re-sending the modifier's channel,
    not by any parameter this function takes.

    Each step is (label, checkpoint-signal-snapshot-after). Stops early, and
    reports it, the first time any of the three required signals fires.
    """
    steps = []

    log('  -- firmware\'s own names for the codes this drive plan uses '
        '(panel_button_names, live) --')
    for name, code in CODES.items():
        fw_name = panelin.control_name(m, profile, code, 'button')
        log('    code=%2d (%-5s assumed) -> firmware says: %r' %
            (code, name, fw_name))

    def checkpoint(label):
        pc_now = m.uc.reg_read(UC_M68K_REG_PC)
        steps.append({'label': label, 'pc': '0x%08x' % pc_now,
                       'list_view_hits': sig_state['list_view_hits'],
                       'selection_view_hits': sig_state['selection_view_hits'],
                       'cow_cave_hits': len(sig_state['cow_cave_hits']),
                       'fired': any_signal_fired(sig_state)})
        log('  [%s] list_view=%d selection_view=%d cow_cave=%d fired=%s' %
            (label, sig_state['list_view_hits'], sig_state['selection_view_hits'],
             len(sig_state['cow_cave_hits']), steps[-1]['fired']))
        return steps[-1]['fired']

    pc_box = [pc]

    def A(code, label):
        channel, mask = wire_mask(code)
        pc_box[0] = assert_channel(m, profile, pits, qs_state, channel, mask,
                                    probe_cap, log, label)

    def R(code, label):
        channel, _mask = wire_mask(code)
        pc_box[0] = assert_channel(m, profile, pits, qs_state, channel, 0x00,
                                    probe_cap, log, label)

    # Plain single taps first, each fully latched (assert, settle, THEN
    # deassert, settle) rather than a quick press/release -- in case the
    # earlier attempt's momentary pair was too short-lived for the UI task
    # to ever observe, per the correction.
    for name in ('SRC', 'YES', 'NO'):
        A(CODES[name], 'assert %s (latched)' % name)
        R(CODES[name], 'deassert %s' % name)
        if checkpoint('tap %s' % name):
            return pc_box[0], steps

    # FUNC+SRC chord: FUNC (channel 2) stays asserted while SRC (channel 0)
    # is asserted and released; FUNC is released last. Checkpointed at each
    # sub-stage so the record trace shows exactly when both channels were
    # simultaneously non-zero.
    A(CODES['FUNC'], 'FUNC down (latch, channel 2)')
    A(CODES['SRC'], 'SRC down, FUNC still held (channel 0)')
    fired = checkpoint('FUNC+SRC: both channels held')
    R(CODES['SRC'], 'SRC up, FUNC still held')
    if fired:
        R(CODES['FUNC'], 'FUNC up (last)')
        return pc_box[0], steps
    fired = checkpoint('FUNC+SRC: SRC released, FUNC still held')
    R(CODES['FUNC'], 'FUNC up (last)')
    if fired or checkpoint('FUNC+SRC: fully released'):
        return pc_box[0], steps

    # FUNC+YES chord: selecting the highlighted parameter with FUNC held.
    A(CODES['FUNC'], 'FUNC down (latch, channel 2)')
    A(CODES['YES'], 'YES down, FUNC still held (channel 1)')
    fired = checkpoint('FUNC+YES: both channels held')
    R(CODES['YES'], 'YES up, FUNC still held')
    R(CODES['FUNC'], 'FUNC up (last)')
    if fired or checkpoint('FUNC+YES: fully released'):
        return pc_box[0], steps

    # SRC page, then DOWN/UP to try to land on a MACHINE parameter row, YES
    # to select -- each button latched and settled individually.
    A(CODES['SRC'], 'assert SRC (latched)')
    R(CODES['SRC'], 'deassert SRC')
    for nav in ('DOWN', 'UP'):
        A(CODES[nav], 'assert %s (latched)' % nav)
        R(CODES[nav], 'deassert %s' % nav)
        A(CODES['YES'], 'assert YES (latched)')
        R(CODES['YES'], 'deassert YES')
        if checkpoint('SRC, %s, YES' % nav):
            return pc_box[0], steps

    return pc_box[0], steps


# --- main --------------------------------------------------------------------

def run(args):
    log = (lambda s: print(s, flush=True)) if args.verbose else (lambda s: None)

    main_img, _ = load_main_image(args.syx)
    profile = symbols.resolve(main_img)

    m, ev, st, pc, inq, at = build(
        args.snapshot, syx=args.syx, unblock=True, softfloat=True,
        bitmap=True, dsp=True, slc=args.slc,
        sdgate=args.sdgate, esdhc=args.esdhc)

    patch_lines = []
    if not args.no_patch:
        patch_lines = mp.patch_b(m, args.cave_b)
    mode = 'control' if args.no_patch else 'patched'

    sig_state = install_signals(m, at, args.cave_b)

    qs_state = ps.make_qs_state()
    ps.install_qs_hook(m, at, qs_state)

    log('== reaching post-intro / idle settle (target=%d, max=%d) ==' %
        (args.instrs, args.max_instrs))
    t0 = time.time()
    pc, pits, phase, done, stop = reach_post_intro(
        m, at, profile, pc, args.instrs, args.max_instrs, args.chunk)
    log('post_intro=%s instrs=%d stop=%s (%.1fs)' %
        (phase['post_intro'], done, stop, time.time() - t0))

    idle_done = done
    if phase['post_intro'] and args.settle > 0 and stop == 'limit':
        log('== extra idle settle: %d instrs (also builds the queue_send '
            'ignore-set) ==' % args.settle)
        d = 0
        while d < args.settle:
            pc, executed, stop = spin(m, pc, min(args.chunk, args.settle - d),
                                       pits=pits)
            d += executed
            if stop != 'limit':
                break
        idle_done += d

    qs_state['ignore'] = frozenset(qs_state['settle_keys'])
    qs_state['candidates'].clear()

    idle_signals = signal_report(sig_state)
    log('idle signals: list_view=%d selection_view=%d cow_cave_hits=%d' %
        (idle_signals['list_view_hits'], idle_signals['selection_view_hits'],
         len(idle_signals['cow_cave_hits'])))
    for name, rep in idle_signals['mmio'].items():
        log('  mmio[%s] events=%d distinct_addrs=%d first_pc=%s' %
            (name, rep['events'], rep['distinct_addrs'], rep['first_pc']))

    drive_steps = []
    capture_path = None
    if args.drive and phase['post_intro'] and stop == 'limit':
        log('== driving panel toward machine-select ==')
        pc, drive_steps = drive(m, at, profile, pc, pits, qs_state, sig_state,
                                 args.probe, log)

    if args.capture and phase['post_intro']:
        buf = panel.read(m, profile.fb_front)
        if buf is not None:
            capture_path = panel.write_png(buf, args.capture)
            log('wrote %s' % capture_path)
            for row in panel.ascii_art(buf):
                log('  ' + row)
        else:
            log('no panel buffer at fb_front=%s' % profile.fb_front)

    final_signals = signal_report(sig_state)
    fired = any_signal_fired(sig_state)

    report = {
        'syx': args.syx,
        'snapshot': args.snapshot,
        'mode': mode,
        'cave_b': '0x%08x' % args.cave_b,
        'patch_lines': patch_lines,
        'reached_post_intro': phase['post_intro'],
        'idle_instrs': idle_done,
        'stop': stop,
        'idle_signals': idle_signals,
        'drove': bool(args.drive),
        'drive_steps': drive_steps,
        'final_signals': final_signals,
        'capture': capture_path,
        'any_signal_fired': fired,
    }
    return report


def print_report(report):
    print(json.dumps({k: v for k, v in report.items()
                       if k not in ('idle_signals', 'final_signals')},
                      indent=2))
    print('\n--- idle signals ---')
    print(json.dumps(report['idle_signals'], indent=2))
    if report['drove']:
        print('\n--- final signals (after drive) ---')
        print(json.dumps(report['final_signals'], indent=2))
    verdict = 'FIRED' if report['any_signal_fired'] else 'no signal observed'
    print('\nverdict (mode=%s): %s' % (report['mode'], verdict))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--snapshot', required=True)
    ap.add_argument('--cave-b', dest='cave_b', type=lambda s: int(s, 0),
                     default=mp.DEFAULT_CAVE_B)
    ap.add_argument('--no-patch', action='store_true',
                     help='skip the patch, for a control run')
    ap.add_argument('--instrs', type=lambda s: int(s, 0), default=90_000_000,
                     help='instruction budget to reach post-intro (default 90M)')
    ap.add_argument('--max-instrs', type=lambda s: int(s, 0), default=200_000_000)
    ap.add_argument('--chunk', type=lambda s: int(s, 0), default=5_000_000)
    ap.add_argument('--settle', type=lambda s: int(s, 0), default=20_000_000,
                     help='extra idle instructions after post-intro, used '
                          'both as deliverable-1 observation window and to '
                          'build the queue_send ignore-set (default 20M)')
    ap.add_argument('--drive', action='store_true',
                     help='attempt scripted navigation to machine-select')
    ap.add_argument('--probe', type=lambda s: int(s, 0), default=8_000_000,
                     help='per-injection settle cap for probe_wait (default 8M)')
    ap.add_argument('--capture', help='write the front framebuffer here as PNG')
    ap.add_argument('--slc', action='store_true', default=True)
    ap.add_argument('--sdgate', dest='sdgate', action='store_true', default=True)
    ap.add_argument('--no-sdgate', dest='sdgate', action='store_false')
    ap.add_argument('--esdhc', dest='esdhc', action='store_true', default=True)
    ap.add_argument('--no-esdhc', dest='esdhc', action='store_false')
    ap.add_argument('--quiet', dest='verbose', action='store_false', default=True)
    ap.add_argument('--json', help='write the full report here')
    args = ap.parse_args(argv)

    t0 = time.time()
    report = run(args)
    report['elapsed_s'] = round(time.time() - t0, 1)

    if args.json:
        os.makedirs(os.path.dirname(os.path.abspath(args.json)) or '.',
                     exist_ok=True)
        with open(args.json, 'w') as fh:
            json.dump(report, fh, indent=2)
    print_report(report)
    return 0


if __name__ == '__main__':
    sys.exit(main())
