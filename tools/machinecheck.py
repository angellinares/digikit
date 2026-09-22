#!/usr/bin/env python3
"""Select a machine on a track, turn its encoders, and read the TX frame.

Draft tool, built out of the two ad-hoc scripts (`confirm_xslice.py`,
`confirm_v2.py`) that first drove a new machine through the real panel path
and read its live state. Reusable pieces those scripts didn't have:

  1. **Two YESes, not one.** `MachineSelectionView::vfunc_2` (0x40061678 on
     Digitakt II 1.16) commits a *changed* selection immediately and does
     NOT close the list -- it only calls `View::close` on a second YES whose
     row again equals the now-updated stored index. A tool that presses YES
     once and then reads the SRC page reads the machine-select list instead
     (see the finding this tool grew out of). `--select-type` presses YES
     twice.
  2. **The TX frame is never live without asking.** `tools/sharcframe.py`'s
     docstring: "the emulator never raises vector 191" -- nothing in
     `emu.longrun`/`emu.pit`'s timer model fires the periodic DSPI2 send on
     its own. Opening the frame-build gate (writing 0 to the profile's
     `gate` address) is necessary but not sufficient by itself: the vector-
     191 handler still has to actually run once to rebuild the frame buffer
     at 0x80005348. This tool does what `sharcframe.py` does -- zero the
     pacing counter, `Machine.raise_vector(191)`, intercept the DSPI2 driver
     call so it fakes an immediate return instead of touching hardware, and
     run until the handler's `rte` lands back where the ColdFire was -- as a
     small side-quest inside an otherwise ordinary `spin()`-driven run,
     right before each frame read, instead of as a separate one-shot capture
     against a fresh restore.
  3. **A patched image has no framelink/device profile entry**, because
     `tools/framelink.py` keys by the MAIN OS image's own SHA-256 and a
     patch changes it. Rather than add a tracked entry for every build,
     `--frame-*` flags override the profile fields this tool needs
     (`handler`, `driver`, `counter`, `gate`, `vector`); omit them to fall
     back to `framelink.profile_for()` for a stock image. None of the nine
     `machinepatch.py` parts touch these addresses (confirmed disjoint by
     inspection), so a build's stock profile values remain correct for it.

`--down-taps` can be derived: a stock type's list position comes from
`machinepatch.ORIGINAL_TABLE` (the seven-entry display order every current
Digitakt-family image shares); a type at or past `machine_count` is assumed
appended last, matching `MachineSpec.position`'s default. A small safety
margin is added on top since the list clamps at its last row rather than
wrapping (confirmed empirically) -- overshooting is harmless, undershooting
lands on the wrong machine.

Usage:
    uv run python machinecheck.py --syx out/xslice.syx \\
        --sections out/xslice_sections --snapshot out/boot400M.snap \\
        --select-type 7 --turn 2:30 --turn 2:30 --turn 2:30 --turn 2:30 \\
        --turn 2:30 --turn 2:30 --turn 2:30 \\
        --frame-words 0x94,0xde,0xe6,0xea \\
        --frame-gate 0x409664f4 --frame-vector 191 \\
        --frame-handler 0x4002dd0c --frame-driver 0x400cd2bc \\
        --frame-counter 0x402a1488 \\
        --png-dir out/png --json out/result.json

    # Optional control run, launched as a second OS process in parallel:
        ... --baseline-snapshot snapshots/dt2-1.16/boot400M.snap \\
            --baseline-syx Digitakt_II_OS1.16.syx \\
            --baseline-sections out/sections/dt2-1.16 \\
            --baseline-select-type 6
"""
import argparse
import copy
import json
import os
import struct
import subprocess
import sys
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIGI_REPO = '/Users/em/src/digi/digitakt2'
for p in (DIGI_REPO, os.path.join(DIGI_REPO, 'tools')):
    if p not in sys.path:
        sys.path.insert(0, p)

TX_BASE = 0x80005348

# The stock seven-machine display order every current Digitakt-family image
# shares (machinepatch.ORIGINAL_TABLE). Kept as a local constant, not an
# import, so this module's pure functions have no emulator-side import cost
# and can be unit-tested standalone; the emulator-driving functions below
# import machinepatch directly and could cross-check this against it.
STOCK_ORDER = (0, 1, 2, 3, 6, 4, 5)

DOWN_CH, DOWN_BIT = 1, 5
YES_CH, YES_BIT = 1, 1
FUNC_CH, FUNC_BIT = 2, 0
SRC_CH, SRC_BIT = 0, 1


# --------------------------------------------------------------------------
# Pure functions: input-sequence building, wire-byte encoding, frame-offset
# maths. No emulator, no guest memory -- these are what tests/test_machinecheck.py
# exercises directly.
# --------------------------------------------------------------------------

def derive_down_taps(select_type, machine_count, stock_order=STOCK_ORDER, margin=2):
    """-> number of DOWN taps from row 0 to `select_type`'s row, plus a
    safety margin (the list clamps at its last row rather than wrapping, so
    overshooting is harmless and undershooting is wrong).

    A stock type (found in `stock_order`) sits at its index there -- an
    interior row, where the margin does not apply since the list only
    clamps at its last row. Anything else -- a type at or past
    `machine_count` -- is assumed appended last, matching
    `machinepatch.MachineSpec.position`'s default of the machine count, and
    gets the margin.
    """
    if select_type in stock_order:
        return stock_order.index(select_type)
    return machine_count + margin


def parse_turn(text):
    """'CHANNEL:DELTA' -> (channel:int, delta:int). Both may be 0x-hex."""
    ch, sep, delta = text.partition(':')
    if not sep:
        raise argparse.ArgumentTypeError('want CHANNEL:DELTA, got %r' % text)
    return int(ch, 0), int(delta, 0)


def parse_frame_words(text):
    """'0x94,0xde,0xe6' -> [0x94, 0xde, 0xe6]."""
    return [int(x, 0) for x in text.split(',') if x.strip()]


def frame_addr(offset, track):
    """-> the absolute TX-frame address for `offset` on `track`.

    Offset 0x94 is the per-track MACHINE-TYPE word array, addressed
    `TX_BASE + 0x94 + 2*track` -- a flat, 2-byte-per-track table, separate
    from the per-track parameter block. Every other offset is inside the
    0x60-byte-per-track SRC/filter/amp/FX block, addressed
    `TX_BASE + track*0x60 + offset` (docs/findings/04-coldfire-dsp-link.md,
    "The mirror index to TX frame map, and the 17-word header": mirror index
    27 (CFADE) -> +0xde, 31 (SLICE) -> +0xe6, 32 (LEN) -> +0xe8, 33 -> +0xea).
    """
    if offset == 0x94:
        return TX_BASE + 0x94 + 2 * track
    return TX_BASE + track * 0x60 + offset


def build_func_src_chord():
    """-> [(channel, mask), ...] for the FUNC+SRC chord that opens machine
    select: FUNC asserted and held, SRC tapped underneath it, both released
    (docs/findings/03-ui-and-panel.md, "Driving panel chords: modifiers must
    latch")."""
    return [
        (FUNC_CH, 1 << FUNC_BIT),
        (SRC_CH, 1 << SRC_BIT),
        (SRC_CH, 0x00),
        (FUNC_CH, 0x00),
    ]


def build_down_taps(n):
    """-> [(channel, mask), ...] for `n` DOWN taps (press, release each)."""
    out = []
    for _ in range(n):
        out.append((DOWN_CH, 1 << DOWN_BIT))
        out.append((DOWN_CH, 0x00))
    return out


def build_yes():
    """-> [(channel, mask), ...] for one YES tap (press, release)."""
    return [(YES_CH, 1 << YES_BIT), (YES_CH, 0x00)]


def build_select_sequence(select_type, machine_count, stock_order=STOCK_ORDER, margin=2):
    """-> the full [(channel, mask), ...] message list to open machine
    select, scroll to `select_type`, commit it (YES #1) and close the list
    (YES #2)."""
    taps = derive_down_taps(select_type, machine_count, stock_order, margin)
    return (build_func_src_chord() + build_down_taps(taps)
            + build_yes() + build_yes())


# --------------------------------------------------------------------------
# Everything below drives the emulator. Not covered by the pure-function
# tests.
# --------------------------------------------------------------------------

def _lazy_imports():
    global emucheck, panel, panelin, spin, mp, mprof, framelink
    global UcError, UC_M68K_REG_A7, UC_M68K_REG_D0, UC_M68K_REG_PC
    import emucheck  # noqa: F401
    from emu import panel, panelin  # noqa: F401
    from emu.longrun import spin  # noqa: F401
    import machinepatch as mp  # noqa: F401
    import machineprofile as mprof  # noqa: F401
    import framelink  # noqa: F401
    from unicorn import UcError  # noqa: F401
    from unicorn.m68k_const import (  # noqa: F401
        UC_M68K_REG_A7, UC_M68K_REG_D0, UC_M68K_REG_PC)


def resolve_frame_profile(main_img_path, overrides):
    """-> a framelink-shaped dict: {'vector','handler','driver','counter','gate'}.

    Tries `framelink.profile_for()` first (works for a stock, unpatched
    image); any CLI override wins over it field by field, so a patched image
    -- whose MAIN OS SHA-256 framelink has never seen -- can still be
    checked without adding a tracked entry for it. Raises SystemExit naming
    the missing field if neither source has it.
    """
    base = {}
    try:
        _, prof = framelink.profile_for(main_img_path)
        base = dict(prof)
    except SystemExit:
        pass
    for key in ('vector', 'handler', 'driver', 'counter', 'gate'):
        val = overrides.get(key)
        if val is not None:
            base[key] = val
    missing = [k for k in ('vector', 'handler', 'driver', 'counter', 'gate')
               if base.get(k) is None]
    if missing:
        raise SystemExit(
            'machinecheck: no frame profile for this image and no --frame-%s '
            'override given' % missing[0])
    return base


def send(m, profile, messages, settle_instrs, pits, pc):
    """Feed `[(channel, mask), ...]` panel messages, settling between each.
    -> new pc."""
    for channel, mask in messages:
        pc = panelin.buttons(m, profile, channel, mask)
        pc, _done, stop = spin(m, pc, settle_instrs, pits=pits)
        if stop != 'limit':
            print('machinecheck: WARNING settle stopped early: %s' % stop)
    return pc


def capture_frame(m, prof, limit=8_000_000):
    """Open the gate, raise vector 191 through the handler with the DSPI2
    driver call intercepted, and run until it returns -- exactly
    `tools/sharcframe.py capture()`'s one pass, reused mid-run instead of
    against a fresh restore. -> True if the handler returned cleanly.
    """
    uc = m.uc
    m.uc.mem_write(prof['gate'], bytes(4))

    driver_calls = []

    def driver_hook(uc_, addr, size, user):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, tx_len, tx, rx_len, rx = struct.unpack('>IIIII', bytes(uc.mem_read(sp, 20)))
        driver_calls.append((tx, tx_len))
        uc.reg_write(UC_M68K_REG_D0, 0)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)

    from unicorn import UC_HOOK_CODE
    h = uc.hook_add(UC_HOOK_CODE, driver_hook, begin=prof['driver'], end=prof['driver'])
    try:
        resume = uc.reg_read(UC_M68K_REG_PC)
        uc.mem_write(prof['counter'], bytes(4))
        m.halt_vec = None
        if not m.raise_vector(prof['vector']):
            return False, driver_calls
        handler = uc.reg_read(UC_M68K_REG_PC)
        try:
            uc.emu_start(handler, resume, count=limit)
        except UcError:
            return False, driver_calls
        return uc.reg_read(UC_M68K_REG_PC) == resume, driver_calls
    finally:
        uc.hook_del(h)


def read_u16(m, addr):
    return struct.unpack('>H', bytes(m.uc.mem_read(addr, 2)))[0]


def read_frame_words(m, offsets, track):
    return {'0x%02x' % off: '0x%04x' % read_u16(m, frame_addr(off, track))
            for off in offsets}


def run_one(args, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    png_dir = os.path.join(out_dir, 'png')
    os.makedirs(png_dir, exist_ok=True)

    events = []
    t0 = time.time()

    def rec(msg):
        line = '[%.1fs] %s' % (time.time() - t0, msg)
        print(line, flush=True)
        events.append(line)

    os.environ['DT2_SECTIONS'] = args.sections
    m, ev, st, pc, inq, at, pits, profile = emucheck.setup(args.snapshot, args.syx)
    rec('setup complete pc=0x%08x' % pc)

    main_img_path = os.path.join(args.sections,
                                  [f for f in os.listdir(args.sections)
                                   if 'MAIN_OS' in f][0])
    overrides = {
        'vector': args.frame_vector, 'handler': args.frame_handler,
        'driver': args.frame_driver, 'counter': args.frame_counter,
        'gate': args.frame_gate,
    }
    prof = resolve_frame_profile(main_img_path, overrides)
    rec('frame profile: %r' % prof)

    # open the gate once up front too, as sharcframe.py's --open-gate does;
    # capture_frame() also (re-)opens it right before each read, so a
    # rewrite in between (none known -- docs/findings/04) would not matter.
    m.uc.mem_write(prof['gate'], bytes(4))

    CHUNK = 15_000_000
    HARD_CAP = 550_000_000
    done_total = 0
    frac = None
    while done_total < HARD_CAP:
        pc, done, stop = spin(m, pc, CHUNK, pits=pits)
        done_total += done
        buf = panel.read(m, profile.fb_front)
        frac = emucheck.frame_on_fraction(buf) if buf else None
        if stop != 'limit':
            rec('FAIL: run stopped early at +%dM: %s' % (done_total // 1_000_000, stop))
            return {'label': args.label, 'pass': False, 'events': events}
        if frac is not None and frac >= emucheck.MAIN_SCREEN_ON_FRACTION:
            rec('dialog cleared at +%dM, on_fraction=%r' % (done_total // 1_000_000, frac))
            break

    def snap_png(name):
        buf = panel.read(m, profile.fb_front)
        path = os.path.join(png_dir, name)
        if buf is not None:
            panel.write_png(buf, path)
        else:
            path = None
        rec('PNG %s' % path)
        return path

    result = {'label': args.label, 'events': events, 'pngs': {}, 'frames': {}}
    result['pngs']['main_screen'] = snap_png('0_main_screen.png')

    machine_count = args.machine_count
    down_taps = args.down_taps
    if down_taps is None:
        down_taps = derive_down_taps(args.select_type, machine_count)
        rec('derived down_taps=%d for select_type=%d (machine_count=%d)'
            % (down_taps, args.select_type, machine_count))

    select_seq = (build_func_src_chord() + build_down_taps(down_taps)
                  + build_yes() + build_yes())
    pc = send(m, profile, select_seq, 3_000_000, pits, pc)
    result['pngs']['after_select'] = snap_png('1_after_select_srcpage.png')

    for channel, delta in args.turn:
        pc = panelin.encoder(m, profile, channel, delta)
        pc, _done, stop = spin(m, pc, 2_000_000, pits=pits)
    if args.turn:
        pc, _done, stop = spin(m, pc, 5_000_000, pits=pits)
        rec('sent %d --turn message(s)' % len(args.turn))
    result['pngs']['after_turn'] = snap_png('2_after_turn.png')

    ok, driver_calls = capture_frame(m, prof)
    rec('capture_frame: handler returned cleanly=%r, driver calls=%d' % (ok, len(driver_calls)))
    result['frame_capture_ok'] = ok

    words = read_frame_words(m, args.frame_words, args.track)
    rec('frame words (track %d): %r' % (args.track, words))
    result['frames']['after_turn'] = words

    result['pass'] = bool(ok)
    result['wall_seconds'] = round(time.time() - t0, 1)
    with open(os.path.join(out_dir, 'result.json'), 'w') as fh:
        json.dump(result, fh, indent=2)
    return result


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--sections', required=True)
    ap.add_argument('--snapshot', required=True)
    ap.add_argument('--select-type', type=lambda s: int(s, 0), required=True)
    ap.add_argument('--machine-count', type=int, default=7,
                     help='stock machine count for --down-taps derivation (default 7, DT2)')
    ap.add_argument('--down-taps', type=int, default=None,
                     help='override the derived DOWN-tap count')
    ap.add_argument('--turn', action='append', default=[], type=parse_turn,
                     metavar='CHANNEL:DELTA', help='encoder message, repeatable; '
                     'the firmware clamps one message to +/-30, so reaching a '
                     'large cumulative value needs several')
    ap.add_argument('--frame-words', default='0x94,0xde,0xe6', type=parse_frame_words)
    ap.add_argument('--track', type=int, default=0)
    ap.add_argument('--frame-vector', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--frame-handler', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--frame-driver', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--frame-counter', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--frame-gate', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--png-dir', required=True)
    ap.add_argument('--json', required=True)
    ap.add_argument('--label', default='primary')

    ap.add_argument('--baseline-snapshot', default=None)
    ap.add_argument('--baseline-syx', default=None)
    ap.add_argument('--baseline-sections', default=None)
    ap.add_argument('--baseline-select-type', type=lambda s: int(s, 0), default=None)
    ap.add_argument('--baseline-json', default=None)
    ap.add_argument('--baseline-png-dir', default=None)

    args = ap.parse_args(argv)
    _lazy_imports()

    baseline_proc = None
    if args.baseline_snapshot:
        if not (args.baseline_syx and args.baseline_sections
                and args.baseline_select_type is not None
                and args.baseline_json and args.baseline_png_dir):
            raise SystemExit('machinecheck: --baseline-snapshot needs --baseline-syx, '
                              '--baseline-sections, --baseline-select-type, '
                              '--baseline-json and --baseline-png-dir')
        baseline_argv = [
            sys.executable, os.path.abspath(__file__),
            '--syx', args.baseline_syx, '--sections', args.baseline_sections,
            '--snapshot', args.baseline_snapshot,
            '--select-type', str(args.baseline_select_type),
            '--machine-count', str(args.machine_count),
            '--frame-words', ','.join('0x%x' % w for w in args.frame_words),
            '--track', str(args.track),
            '--png-dir', args.baseline_png_dir, '--json', args.baseline_json,
            '--label', 'baseline',
        ]
        for ch, delta in args.turn:
            baseline_argv += ['--turn', '%d:%d' % (ch, delta)]
        if args.frame_vector is not None:
            baseline_argv += ['--frame-vector', str(args.frame_vector)]
        if args.frame_handler is not None:
            baseline_argv += ['--frame-handler', hex(args.frame_handler)]
        if args.frame_driver is not None:
            baseline_argv += ['--frame-driver', hex(args.frame_driver)]
        if args.frame_counter is not None:
            baseline_argv += ['--frame-counter', hex(args.frame_counter)]
        if args.frame_gate is not None:
            baseline_argv += ['--frame-gate', hex(args.frame_gate)]
        print('machinecheck: launching baseline in parallel: %s' % ' '.join(baseline_argv))
        baseline_proc = subprocess.Popen(baseline_argv)

    primary = run_one(args, os.path.dirname(args.json) or '.')
    with open(args.json, 'w') as fh:
        json.dump(primary, fh, indent=2)

    baseline_result = None
    if baseline_proc is not None:
        rc = baseline_proc.wait()
        if os.path.exists(args.baseline_json):
            with open(args.baseline_json) as fh:
                baseline_result = json.load(fh)
        print('machinecheck: baseline process exit=%d' % rc)

    print()
    print('%-10s %-6s %s' % ('label', 'pass', 'frame words'))
    print('%-10s %-6s %s' % (primary['label'], primary.get('pass'),
                              primary.get('frames', {}).get('after_turn')))
    if baseline_result:
        print('%-10s %-6s %s' % (baseline_result['label'], baseline_result.get('pass'),
                                  baseline_result.get('frames', {}).get('after_turn')))

    ok = bool(primary.get('pass')) and (baseline_result is None or bool(baseline_result.get('pass')))
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
