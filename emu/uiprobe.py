"""Does the user interface run, and if not, what is it waiting for?

This is the post-intro counterpart to `emu/probe.py`. That one answers "why
does only one task run"; this one answers "the tasks run, so why is the panel
still blank", and it is the harness the DMA timer work was measured with.

What it showed, from `snapshots/postintro.snap` over 60M instructions, with
`dsp=True` throughout and only the DMA timer channels changing:

    channels   instrs      tasks  setPixel  mainloop  msgs  DTIM3  flush  stop
    ()         60,061,748      6         8         1     0      0     11  limit
    ()         60,061,748      6         8         1     0      0     11  limit
    (3,)       60,060,760      6        59       153   153    246     62  limit
    (1,)       60,061,748      6         8         1     0      0     11  limit
    (3, 1)     55,615,618      6        88       202   202    212     91  FAULT

The two `()` rows are the same-config control, and they agree exactly --
HANDOVER section 0 warning 2 and trap 3 both insist on that before any row
below them is believed.

The result is `(3,)`: delivering DMA timer 3 takes the main application task
from one pass through its message loop to a hundred and fifty-three. DTIM3's
ISR `0x400c30e4` is the only thing at boot that calls `queue_send` on
`0x4094ef3c`, which is the queue the main task blocks on at `0x40033492`, so
until it is delivered the whole user interface waits on a queue nothing feeds.

`(3, 1)` reaches further into the UI and then faults; `(1,)` alone leaves the
main loop where it was but lets the priority-3 job worker reach its second
job. Neither is the default. See emu/dtim.py.

Usage:

    python -m emu.uiprobe sweep  [snapshot] [instrs]   # the table above
    python -m emu.uiprobe run    [snapshot] [instrs] [channels]

`run` does one long run and prints the panel as ASCII, the site counts, the
messages the main loop received, and a validated backtrace if the firmware
reaches the terminal loop at `0x4012d2fa`.
"""
import collections
import struct
import sys

from unicorn import UC_HOOK_CODE, UcError
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_D0

from emu.dtim import Dtims, Timers
from emu.longrun import build, spin, setpixel_count
from emu import panel
from emu.pit import Pits, intro_running

W, H = 128, 64

# Every site here is a function entry, which is a basic-block boundary: a
# mid-function UC_HOOK_CODE reads registers that contradict the branch the CPU
# then takes (HANDOVER trap 4). `0x4003349e` below is the exception and is
# only ever used to read D0, which the call it follows has just set.
SITES = {
    0x40033492: 'main: message-loop head',
    0x4003349e: 'main: message received',
    0x4012a874: 'main: message 5 -> UI redraw',
    0x40125f6a: 'progress geometry setter (sets 0x44e2d5cc)',
    0x4012606a: 'progress-screen task',
    0x400f1ce0: 'job enqueue',
    0x400f1b80: 'job pump',
    0x400cfd40: 'coprocessor 4KB page/command push',
    0x40128c7c: 'sleep(us) via DMA timer 1',
    0x40128c4c: 'DTIM1 ISR  (sleep expired)',
    0x400c30e4: 'DTIM3 ISR  (-> main-loop queue)',
    0x40126332: 'panel flush (double-buffer diff)',
    0x4012651e: 'display tick callback',
    0x4010fcae: 'fault handler',
    0x4012d2fa: 'TERMINAL LOOP (bra.b to itself)',
    0x401d0e24: 'C++ throw',
}

TERMINAL = 0x4012d2fa
MSG_RECV = 0x4003349e
ENQUEUE = 0x400f1ce0


def _cstr(uc, addr, n=64):
    if not 0x40000000 <= addr < 0x50000000:
        return None
    try:
        return bytes(uc.mem_read(addr, n)).split(b'\0')[0].decode('latin1',
                                                                 'replace')
    except UcError:
        return None


def _call_site(uc, ret):
    """-> the call site if `ret` looks like a real return address, else None.

    HANDOVER section 14: a naive stack scan gives nonsense, so only accept a
    word that is preceded by an actual call opcode -- `jsr abs.l` (4EB9) six
    bytes back, `bsr`/`jsr d16(pc)`/`jsr d8(pc,xn)` (4EBA/4EB8/6100) four back,
    or `jsr (an)` (4E8x) two back.
    """
    def w16(a):
        try:
            return struct.unpack('>H', bytes(uc.mem_read(a, 2)))[0]
        except UcError:
            return None

    for back, ops in ((6, (0x4EB9,)), (4, (0x4EBA, 0x4EB8, 0x6100))):
        if w16(ret - back) in ops:
            return ret - back
    op = w16(ret - 2)
    return ret - 2 if op is not None and (op & 0xFFC0) == 0x4E80 else None


def backtrace(uc, limit=14, depth=512):
    """-> [(stack offset, return address, call site)] at the current A7."""
    sp, out = uc.reg_read(UC_M68K_REG_A7), []
    for off in range(0, depth, 4):
        try:
            word = struct.unpack('>I', bytes(uc.mem_read(sp + off, 4)))[0]
        except UcError:
            break
        if not 0x40000000 <= word < 0x40300000:
            continue
        site = _call_site(uc, word)
        if site is not None:
            out.append((off, word, site))
            if len(out) >= limit:
                break
    return out


def measure(snapshot, instrs, channels=(3,), on_pixel=None, trace=True):
    """Run once with `channels` delivered. -> a dict of everything counted."""
    fb = {}

    def pixel(x, y, val, bmp):
        if 0 <= x < W and 0 <= y < H:
            fb[(x, y)] = val
        if on_pixel:
            on_pixel(x, y, val, bmp)

    m, ev, st, pc, inq, at = build(snapshot, unblock=True, softfloat=True,
                                   bitmap=True, dsp=True, on_pixel=pixel)
    uc = m.uc
    hits, msgs, jobs, bt = collections.Counter(), collections.Counter(), [], []

    for addr, name in SITES.items():
        at(addr, (lambda nm: lambda u, a, s, d: hits.update([nm]))(name))
    at(MSG_RECV, lambda u, a, s, d: msgs.update(
        [bytes(u.mem_read(u.reg_read(UC_M68K_REG_D0), 1))[0]]))
    at(ENQUEUE, lambda u, a, s, d: jobs.append(_cstr(u, struct.unpack(
        '>I', bytes(u.mem_read(u.reg_read(UC_M68K_REG_A7) + 8, 4)))[0])))
    if trace:
        # The terminal loop branches to itself, so this fires on every pass.
        # Take the backtrace once, on the way in, while the stack is intact.
        at(TERMINAL, lambda u, a, s, d: bt or bt.extend(backtrace(u)))

    # Same per-build resolution as emu/panel.py and emu/gui.py: the intro's
    # PIT3 handler and its exit point both move between builds.
    from emu import config as _config, symbols as _symbols
    profile = _symbols.resolve(open(_config.main_image(), 'rb').read())
    intro = intro_running(m, profile.intro_pit3_isr)
    src = [Pits(m, hold=intro)]
    if channels:
        src.append(Dtims(m, channels=channels, hold=intro))
    timers = Timers(*src)
    if timers.held:
        if profile.intro_done is None:
            print('warning: intro_done unresolved for this image -- the '
                  'timers will stay held for the whole run')
        else:
            at(profile.intro_done, lambda u, a, s, d: timers.release())

    pc, done, stop = spin(m, pc, instrs, pits=timers)
    # The firmware's own framebuffer, not the setPixel HLE. `fb`/`lit` below
    # only see Bitmap::setPixel, which the main OS does not draw through, so
    # they read near zero while a full user interface sits in RAM. This is a
    # plain memory read and installs no hook, so it cannot move any count
    # above it -- see emu/panel.py.
    pbuf = panel.read(m)
    return dict(done=done, stop=stop, pc=pc, hits=hits, msgs=dict(msgs),
                jobs=jobs, bt=bt, fb=fb, ev=ev, m=m, timers=timers,
                panel=pbuf, panel_lit=len(panel.lit(pbuf)) if pbuf else 0,
                lit=sum(1 for v in fb.values() if v),
                count=struct.unpack('>I', bytes(uc.mem_read(0x44e2d5cc, 4)))[0])


def sweep(snapshot, instrs):
    """The table in the module docstring. Row one is the control; run it twice.

    Every row builds identically and installs the identical hook set, so the
    only difference between them is which DMA timers are delivered. Anything
    less than that is not an A/B -- HANDOVER trap 1.
    """
    cases = [(), (), (3,), (1,), (3, 1)]
    print('%-10s %13s %6s %9s %9s %5s %6s %6s %6s  %s'
          % ('channels', 'instrs', 'tasks', 'setPixel', 'mainloop', 'msgs',
             'DTIM3', 'flush', 'panel', 'stop'))
    for ch in cases:
        r = measure(snapshot, instrs, ch, trace=False)
        h = r['hits']
        print('%-10s %13s %6d %9d %9d %5d %6d %6d %6d  %s'
              % (str(ch), format(r['done'], ','), len(r['ev']['tasks']),
                 setpixel_count(r['ev']), h['main: message-loop head'],
                 sum(r['msgs'].values()), h['DTIM3 ISR  (-> main-loop queue)'],
                 h['panel flush (double-buffer diff)'], r['panel_lit'],
                 r['stop']))


def run(snapshot, instrs, channels):
    r = measure(snapshot, instrs, channels)
    t, ev = r['timers'], r['ev']
    print('=== %s channels=%s  %s instrs  stop=%s  pc=%#010x ==='
          % (snapshot, channels, format(r['done'], ','), r['stop'], r['pc']))
    print('fired  %s' % t.fired)
    print('missed %s' % t.missed)
    print('tasks=%d  setPixel=%d  lit=%d  progress count 0x44e2d5cc=%#010x'
          % (len(ev['tasks']), setpixel_count(ev), r['lit'], r['count']))
    if 'dsp' in ev:
        print('coprocessor port: bursts=%s words=%s'
              % (format(ev['dsp'].bursts, ','), format(ev['dsp'].words, ',')))
    print()
    for addr, name in SITES.items():
        print('  %12s  %#010x  %s' % (format(r['hits'][name], ','), addr, name))
    print('\nmain-loop messages by type: %s' % r['msgs'])
    print('jobs enqueued: %s' % r['jobs'])
    if r['bt']:
        print('\nreached the terminal loop. Validated backtrace:')
        for off, ret, site in r['bt']:
            print('   +%#05x  ret=%#010x   call at %#010x' % (off, ret, site))
    print()
    if r['panel']:
        print('--- the firmware\'s own panel buffer, %d lit ---' % r['panel_lit'])
        for row in panel.ascii_art(r['panel']):
            print(row)
    else:
        print('--- no panel buffer ---')
    print('--- Bitmap::setPixel HLE, %d lit (intro path only) ---' % r['lit'])
    for y in range(H):
        print(''.join('#' if r['fb'].get((x, y)) else '.' for x in range(W)))


if __name__ == '__main__':
    mode = sys.argv[1] if len(sys.argv) > 1 else 'sweep'
    snap = sys.argv[2] if len(sys.argv) > 2 else 'snapshots/postintro.snap'
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 60_000_000
    if mode == 'sweep':
        sweep(snap, n)
    else:
        ch = tuple(int(c) for c in sys.argv[4].split(',')) \
            if len(sys.argv) > 4 else (3,)
        run(snap, n, ch)
