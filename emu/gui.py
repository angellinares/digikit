"""Boot the thing and watch the panel.

A live view of the Digitakt II's 128x64 OLED, driven by the real firmware
running under Unicorn. The emulator runs on a worker thread and writes into a
shared framebuffer whenever the firmware calls Bitmap::setPixel; the UI thread
just samples that framebuffer on a timer. Nothing here reimplements the raster
-- every lit pixel is one setPixel call the firmware actually made.

There are TWO screens and this window has to switch between them, because the
intro and the main OS draw by different routes. Measured, over the same build:

    boot400M.snap, intro running    setPixel 616,823   panel buffer     17 lit
    postintro.snap, OS running      setPixel       0   panel buffer  2,373 lit

The intro draws through `Bitmap::setPixel`, so `on_pixel` is the right source
for it. The main OS composes straight into the firmware's own framebuffer and
never calls the intercepted primitive, so after the handover the source has to
become `emu.panel.read`. Showing setPixel throughout is what made this window
sit on the intro's last frame forever while a complete user interface was
rendering in RAM -- see HANDOVER warning 6. The switch happens at INTRO_DONE,
the same point the timers are released.

    uv run python -m emu.gui [snapshot]

--patch-machine installs the experimental eighth machine (PLACEHOLDER) into
the running emulator's machine list. Bare, it applies all nine parts of the
patch (list, dispatch, group, name, rank, permit, hint, pertype, clone);
--patch-machine=list, =dispatch, =group, =name, =rank, =permit, =hint,
=pertype, or =clone applies just one, and a
+-separated combination (--patch-machine=list+dispatch) applies exactly
those, for bisecting a boot failure. An optional :N suffix on the parts
value (--patch-machine=list:6)
sets the 8th list entry's value, default 7, to distinguish "eight entries is
too many" from "the value 7 is the problem". This patches guest memory in
the running emulator only -- it modifies no file on disk and is not a
flashable patch.

--machine=NAME:SHORT[:CLONE_OF[:POSITION]] overrides the new machine's
names, cloned descriptor and display position (see
tools/machinepatch.py's MachineSpec); without it the default spec
(Placeholder/PLC, cloned from type 6) is used.

--ips-at WHEN:N (repeatable) changes the timer rate to N instructions per
emulated second at instruction count WHEN (e.g. --ips-at 80M:18.72M after
boot); the GUI then runs slower than real time if the emulator cannot
keep up.

--post-intro-ips N sets the timer rate applied when the intro hands over;
default 18720000 (4x INSTR_PER_SEC); 0 keeps the default rate; ignored when
--ips-at is given.

tkinter only, no third-party GUI dependency. Note Homebrew's python@3.14 does
not ship tkinter; uv's managed CPython does, which is why pyproject pins 3.12.
"""
import collections
import os
import struct
import sys
import threading
import time
import tkinter as tk
from typing import cast

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn.unicorn import UcError
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_PC, UC_M68K_REG_SR
from emu.longrun import build, spin
from emu.dtim import Dtims, Timers
from emu import config, device as devices, panel, panelin, symbols
from emu.pit import INSTR_PER_SEC, Pits, intro_running
from emu.screen import png

# The intro's frame rate is not a guess. PIT3 is configured at 0x400d3a7a with
# PCSR=0x0936 (prescaler 2^10) and PMR=0x2191, so one frame is (8593+1)*1024 =
# 8,800,256 bus cycles; its ISR (vector 208, 0x400d2d70 on Digitakt -- resolved
# per build as profile.intro_pit3_isr) posts the semaphore the draw loop waits
# on at 0x400d4036 (also Digitakt-specific). The bus clock is 132 MHz, taken
# from the UART baud divider at 0x400024a4 (132000000 / (32*baud)) -- which
# checks out because it also makes the RTOS tick exactly 50.000 Hz and PIT2
# 60.0 Hz.
FRAME_VECTOR = 208
FRAME_HZ = 132_000_000 / ((0x2191 + 1) * 1024)     # 14.9996

# Buttons that latch instead of behaving momentarily. A mouse cannot hold one
# button while clicking another, so a modifier click toggles it and stays
# asserted for the next press -- which is what makes FUNC+SRC reach SRC's
# secondary function rather than its primary. Keyed off the device TOML's
# group name; that grouping was previously editorial only, and this is the
# first thing to read it semantically.
LATCHING_GROUPS = frozenset({'modifiers'})

W, H = 128, 64

# Vertical space the window owes to everything that is not the panel: the
# toolbar, three status lines, and the control surface, which is several rows
# of buttons deep. Without this the auto-zoom happily fills the screen with a
# 128x64 framebuffer and clips the controls off the bottom.
RESERVE_H = 460
MAX_SCALE = 3

# Panel palette: an OLED is emissive, so the lit pixel is the bright thing and
# the ground is genuinely black rather than dark grey.
OFF = b'\x0c\x0e\x12'
ON = b'\xe8\xf6\xff'
# The worker runs under spin(pits=...), which steps to each PIT deadline and
# so delivers the OS heartbeat: the RTOS time slice (PIT0), the software timer
# wheel (PIT2) and the display frame timer (PIT3). Without it the GUI ran the
# firmware with no interrupts at all, which is why it showed none of the
# post-intro progress the harness could already reach -- the display task sat
# blocked on a semaphore only PIT3's handler ever posts.
#
# It is expensive: deadline stepping needs `count=` on emu_start, and that
# makes Unicorn count every instruction, which breaks TB chaining. Measured on
# this machine with every hook installed, 2.0M instructions a second counted
# against 15.5M uncounted -- 7.6x, not the ~1.8x this comment used to claim.
# The cost is in `count` itself and not in how often emu_start is called, so
# there is nothing to win by making BUDGET bigger than responsiveness wants.
# The GUI defaults to spin(fast=True), which drops `count=` entirely; --exact
# puts it back for comparison against bootcheck. See longrun._FastStepper.
BUDGET = 400_000          # instructions per pass, ~0.16s: pause/stop latency,
                          # and how long a panel click waits to be delivered.
                          # The cost is in emu_start's `count` rather than in
                          # how often it is called, so a smaller pass buys
                          # responsiveness almost for free.

# Emulated dwell between panel state changes. _drain_input used to deliver
# everything queued in one feed, so a press and its release reached the
# firmware a few emulated milliseconds apart however slowly the user
# clicked -- and a chord collapsed into an instant. A real press lasts
# 50-200 ms. At BUDGET instructions per chunk and roughly one instruction
# per cycle on a 132 MHz bus, a chunk is about 3 ms, so 16 chunks is around
# 50 ms of dwell.
PANEL_DWELL_CHUNKS = 16


class Emulator(threading.Thread):
    """Runs the firmware and publishes a framebuffer. Owns no widgets."""

    daemon = True

    def __init__(self, snapshot, weakptr=False, slc=False, syx=None,
                 fast=True, realtime=True,
                 patch_machine: bool | tuple[str, ...] = False,
                 patch_eighth=7, patch_machine_spec=None,
                 panel_dwell=PANEL_DWELL_CHUNKS, ips_at=(),
                 post_intro_ips=4 * INSTR_PER_SEC):
        super().__init__()
        self.snapshot = snapshot
        self.weakptr = weakptr
        self.slc = slc
        self.syx = syx
        self.patch_machine = patch_machine
        self.patch_eighth = patch_eighth
        self.patch_machine_spec = patch_machine_spec
        self._pending_ips = sorted(ips_at)
        # Timer rate applied once the intro hands over; see --post-intro-ips.
        # An explicit --ips-at wins, so recorded sessions replay unchanged.
        self._post_intro_ips = 0 if ips_at else post_intro_ips
        # See PANEL_DWELL_CHUNKS. 0 means no pacing: the old coalesce-and-
        # deliver-once-per-chunk behaviour, for an A/B against this one.
        self._dwell_chunks = panel_dwell
        self._chunks_since_delivery = 0
        self._delivered_before = False
        # Interactive running, not measurement. `fast` drops the `count=`
        # argument to emu_start, which costs 7.6x on this machine, in exchange
        # for timers landing on a basic-block boundary rather than an exact
        # instruction -- so it changes the instruction stream and must never be
        # used for a determinism or pass/fail claim. See longrun._FastStepper.
        # `realtime` then paces the worker back down: uncounted it runs several
        # times faster than the hardware, and a sequencer at 3x tempo is worse
        # than one at a third.
        self.fast = fast
        self.realtime = realtime
        self._paced = 0
        self._pace_t0 = None
        self._rate_t = None         # wall-clock instruction rate window
        self._rate_instrs = 0
        self.fb = bytearray(W * H)
        self.pause = threading.Event()
        self.stop_flag = threading.Event()
        self.ready = threading.Event()
        self.stats = {'frames': 0, 'px': 0,
                      'pc': 0, 'tcb': 0, 'tasks': 0, 'prints': 0, 'fps': 0.0,
                      'bmp': 0, 'instrs': 0, 'pit': (0, 0, 0),
                      'status': 'loading snapshot', 'mainloop': 0, 'jobs': 0,
                      'dtim3': 0, 'terminal': False, 'panel_lit': 0,
                      'source': 'setPixel', 'wall_ips': 0.0, 'real': 0.0}
        self._uc = None             # set once the machine is built
        self.error = None
        self._seen = set()
        self.version = 0            # bumped on every pixel, so the UI can
        self._frame_t = time.time()  # skip redrawing an unchanged panel
        self.captured = []          # completed frames, for correct-speed replay
        self.use_panel = False      # False: setPixel (intro). True: the
                                    # firmware's own framebuffer (main OS).
        self._last_panel = None     # last panel buffer drawn, to skip repeats
        self._panel_live = False    # seen the OS draw into it at least once
        self._panel_latch = None    # newest untorn frame, grabbed at diff entry
        self.fb_front = None        # resolved once the image is known -- see run()
        self.profile = None         # the whole symbol profile, same point
        # Panel input. The UI thread must never touch guest memory: the
        # worker sits inside emu_start for a whole BUDGET at a time. So
        # clicks arrive on this queue and are applied between chunks, the
        # same safe point pause already uses.
        self.inbox = collections.deque()
        self.device = None          # which product, identified by firmware hash
        self.held = None            # panelin.Held, once the device is known
        self.button_names = {}      # control code -> the firmware's own name
        self.encoder_names = {}
        self.device_error = None    # why there is no control surface, if so
        self._faulted_pages = set()  # pages already reported by _fault_sink
        self._fault_summary_printed = False  # print the report once, not per chunk
        self._backtrace_printed = False  # print the stack scan once, not per chunk

    def _identify_device(self, m, profile):
        """Work out which product this is and read its control names.

        Degrades rather than fails: an unrecognised firmware means no control
        surface, not a dead emulator. The names are read out of the image, so
        they are this firmware's own rather than a table that can go stale.
        """
        try:
            self.device, _fw = devices.identify(config.firmware(self.syx))
            self.held = panelin.Held(self.device)
            self.button_names = panelin.control_names(m, profile, 'button')
            self.encoder_names = panelin.control_names(m, profile, 'encoder')
        except Exception as exc:                       # noqa: BLE001
            self.device_error = '%s: %s' % (type(exc).__name__, exc)

    def _drain_input(self, m, profile, pc):
        """Apply queued panel input at a chunk boundary. -> the new PC.

        Everything delivered in one pass is encoded into ONE byte stream and
        sent with a single feed, because the firmware's ISR drains the whole
        receive ring: one raised vector covers every message in it. Raising
        once per event would nest exception frames for input the ring
        already holds.

        A single feed used to mean a single drain of the WHOLE queue, once
        per BUDGET chunk -- so a press and its release, however far apart the
        user actually clicked, reached the firmware a few emulated
        milliseconds apart, and a chord collapsed into an instant. See
        PANEL_DWELL_CHUNKS. Now a press/release (a button STATE change) is
        held back until _dwell_chunks have passed since the last one was
        delivered, so it dwells for something like a real press. Encoder
        events are relative and bursty by nature rather than a state that can
        be held, so they are not paced: every queued encoder event is drained
        in the same pass as the one button transition (or on its own, if no
        button transition is pending). Nothing queued is ever dropped, only
        delayed until its dwell elapses. --panel-dwell 0 disables all of
        this and restores the old drain-everything-every-chunk behaviour.

        Returns the PC because delivering input raises a vector, which moves
        it. Dropping the result would strand the run at the old address.
        """
        if self.held is None:
            return pc
        paced = self._dwell_chunks > 0
        if paced and self._delivered_before and (
                self._chunks_since_delivery < self._dwell_chunks):
            self._chunks_since_delivery += 1
            return pc
        out = bytearray()
        took_button = False
        deferred = []
        while self.inbox:
            kind, code, arg = self.inbox.popleft()
            if kind == 'encoder':
                assert self.device is not None
                channel = self.device.encoder_channel(code)
                if channel is not None:
                    out += panelin.encode_encoder(channel, arg)
            elif paced and took_button:
                deferred.append((kind, code, arg))
            else:
                took_button = True
                if kind == 'press':
                    pos = self.held.press(code)
                    if pos is not None:
                        out += panelin.encode_buttons(*pos)
                elif kind == 'release':
                    pos = self.held.release(code)
                    if pos is not None:
                        out += panelin.encode_buttons(*pos)
                elif kind == 'release_all':
                    for pos in self.held.release_all():
                        out += panelin.encode_buttons(*pos)
        for item in reversed(deferred):
            self.inbox.appendleft(item)
        if not out:
            return pc
        self._chunks_since_delivery = 0
        self._delivered_before = True
        try:
            new_pc = panelin.feed(m, profile, bytes(out))
        except Exception as exc:                       # noqa: BLE001
            self.stats['status'] = 'panel input failed: %s' % exc
            return pc
        # Replayable: paste these into tools/guirun.py to reproduce the session.
        # stats['instrs'] is the count at this chunk boundary, before the next
        # spin, which is exactly where guirun delivers a --feed.
        print('[gui] input --feed %d:%s' % (self.stats['instrs'], bytes(out).hex()),
              flush=True)
        return new_pc

    def run(self):
        def on_pixel(x, y, val, bmp):
            self.stats['bmp'] = bmp
            if (x, y) in self._seen and len(self._seen) > W * H // 2:
                now = time.time()
                self.captured.append(bytes(self.fb))    # snapshot the finished frame
                self.stats['frames'] += 1              # coordinate repeat = new frame
                self.stats['fps'] = 1.0 / max(1e-6, now - self._frame_t)
                self._frame_t = now
                self._seen.clear()
                # Deliberately no emu_stop here any more. Under spin() a hook
                # that stops the run early makes the instruction accounting a
                # lie -- emu_start returns having executed fewer than it was
                # asked for, the loop credits itself the full step, and every
                # timer deadline drifts away from the instructions actually
                # executed. The worker regains control every BUDGET
                # instructions instead, which is soon enough for pause and
                # stop to feel immediate.
            self._seen.add((x, y))
            self.fb[y * W + x] = val
            self.stats['px'] += 1
            self.version += 1

        try:
            # NOTE: unblock=True also satisfies the frame semaphore, so the
            # animation runs unpaced -- as fast as the host manages, not at
            # FRAME_HZ. Excluding FRAME_SEM and driving vector 208 instead was
            # tried and does not work on its own: with every other wait
            # satisfied, the prio-6 task never yields, so the scheduler never
            # reschedules and the woken draw task never runs (the semaphore
            # count just climbs). Faithful pacing needs cycle accounting so
            # the RTOS tick can preempt too. Until then the status line
            # reports the shortfall against the real 15 fps rather than
            # pretending.
            # dsp=True backs the 0x8C000000 coprocessor port's ready line.
            # Without it the priority-3 job worker wedges in the ready-bit
            # spin at 0x400cf4ec on its very first transfer and none of the
            # five jobs queued at boot ever runs. See emu/dsp.py.
            extra = {'syx': self.syx} if self.syx else {}
            # sdgate/esdhc are not passed here -- build()'s own defaults
            # (True) supply the SD storage models, so they come along with
            # every call site that does not explicitly override them.
            m, ev, st, pc, inq, at = build(self.snapshot, unblock=True,
                                           softfloat=True, bitmap=True,
                                           dsp=True, on_pixel=on_pixel,
                                           weakptr=self.weakptr, slc=self.slc,
                                           **extra)
            if self.patch_machine:
                sys.path.insert(0, os.path.join(os.path.dirname(
                    os.path.dirname(os.path.abspath(__file__))), 'tools'))
                from machinepatch import (  # type: ignore[reportMissingImports]
                    patch_b, DEFAULT_CAVE_B, spec_from_arg, DEFAULT_SPEC)
                # patch_b (and spec_from_arg) report a failed precondition
                # with SystemExit, which derives from BaseException and so
                # would slip past the handler below -- and a SystemExit on a
                # worker thread kills it silently, leaving this window stuck
                # on "loading snapshot". Convert it into something catchable.
                try:
                    spec = (spec_from_arg(self.patch_machine_spec)
                            if self.patch_machine_spec else DEFAULT_SPEC)
                    patch_b(m, DEFAULT_CAVE_B, parts=self.patch_machine,
                            eighth=self.patch_eighth, spec=spec)
                except SystemExit as exc:
                    raise RuntimeError('machine patch refused: %s' % exc) from exc
                parts = cast(tuple[str, ...], self.patch_machine)
                self.stats['status'] = ('patched: ' + '+'.join(parts)
                                        + ' (8th=%d)' % self.patch_eighth)
            # build() already resolved (and required) this same profile
            # internally -- see emu/symbols.py -- so re-resolving here is a
            # cache hit, not a rescan. fb_front is OPTIONAL: if it did not
            # resolve for this image, _publish_panel below just never has
            # anything to read, which is the documented degrade-gracefully
            # behaviour rather than a crash.
            main_img = open(config.main_image(), 'rb').read()
            profile = symbols.resolve(main_img)
            self.fb_front = profile.fb_front
            self.profile = profile
            self._identify_device(m, profile)
        except Exception as exc:                       # noqa: BLE001
            self.error = '%s: %s' % (type(exc).__name__, exc)
            self.stats['status'] = 'failed to load'
            print('[gui] FAILED TO LOAD: %s' % self.error, flush=True)
            self.ready.set()
            return

        self._uc = m.uc
        self._m = m

        def fault_sink(rec):
            # Called from inside a Unicorn hook on the worker thread: no
            # locks, no Tk calls, no guest memory access, and nothing may
            # raise into the run -- same defensiveness as Machine._fault.
            try:
                page = rec['page']
                if page in self._faulted_pages:
                    return
                self._faulted_pages.add(page)
                kinds = '+'.join(sorted(rec['kinds'])) if rec['kinds'] else '?'
                print('[gui] FAULT page=0x%08x first=0x%08x pc=0x%08x %s'
                      % (page, rec['first_addr'], rec['first_pc'], kinds),
                      flush=True)
            except Exception:
                pass
        m.fault_sink = fault_sink
        self._reported = 0
        # PIT0 time slice, PIT2 wheel, PIT3 display -- but not until the intro
        # has handed over. Delivering into a running intro stops it ever
        # ending (PIT3 double-posts the frame semaphore that unblock is
        # already satisfying) and stops the OS tasks spawning (PIT2). See
        # emu.pit.Pits.
        # ...and DMA timer 3, which is the 30.05 Hz tick whose ISR
        # (0x400c30e4) is the only thing at boot that sends a message to
        # 0x4094ef3c, the queue the main application task blocks on. Without
        # it that task makes exactly one pass through its message loop and
        # waits forever, which is what this window used to show. See
        # emu/dtim.py.
        intro = intro_running(m, profile.intro_pit3_isr)
        pits = Timers(Pits(m, hold=intro),
                      Dtims(m, channels=(3,), hold=intro))
        # `pits.held` is exactly "the intro is still running", so a snapshot
        # taken after it already belongs to the OS and the panel buffer is the
        # screen from the first frame.
        self.use_panel = not pits.held
        if pits.held:
            def handover(uc, a, s_, d):
                pits.release()
                self.use_panel = True
                if self._post_intro_ips:
                    # Applied at the next chunk boundary, like --ips-at.
                    self._pending_ips.append((self.stats['instrs'],
                                              self._post_intro_ips))
            if profile.intro_done is not None:
                at(profile.intro_done, handover)
            else:
                print('[gui] WARNING: intro_done did not resolve for this '
                      'image; timers will stay held and the intro will '
                      'never hand over', flush=True)
        elif self._post_intro_ips:
            self._pending_ips.append((0, self._post_intro_ips))
        print('[gui] timer rate %d, after intro %s'
              % (pits.sources[0].ips, self._post_intro_ips or 'unchanged'),
              flush=True)

        # Progress markers, so the status line can say what the firmware is
        # actually doing rather than only how many pixels it drew. Resolved
        # per build now (profile.mainloop / profile.job_pump); they say
        # whether the OS actually took over after the intro: mainloop is the
        # main application task's message-loop head, jobs is the job-worker
        # pump.
        mark = self.stats
        if profile.mainloop is not None:
            at(profile.mainloop, lambda uc, a, s, d: mark.__setitem__(
                'mainloop', mark['mainloop'] + 1))
        if profile.job_pump is not None:
            at(profile.job_pump, lambda uc, a, s, d: mark.__setitem__(
                'jobs', mark['jobs'] + 1))
        # 0x4012d2fa is `bra.b` to itself -- the loop the abort path lands in.
        at(0x4012d2fa, lambda uc, a, s, d: mark.__setitem__('terminal', True))

        # Latch the frame at the diff's entry, which emu/panel.py documents as
        # the one moment [FRONT] is a complete, just-rendered frame. Reading it
        # at an arbitrary moment instead -- which _publish_panel used to do --
        # is wrong twice over: mid-flush it is torn on a page boundary, and
        # once the diff has swapped, [FRONT] is the buffer being rendered into
        # NEXT rather than the one on the panel. On screen that is a UI that
        # flickers and elements that come and go between frames.
        #
        # This is the same hook emu.panel.Capture installs, and it does change
        # the run it observes -- but this window already hooks intro_done,
        # mainloop, job_pump and the terminal loop, and it is a viewer, not a
        # measurement. Anything comparing totals should not be reading a GUI.
        if profile.panel_diff is not None and profile.fb_front is not None:
            def latch_frame(uc, a, s_, d):
                buf = panel.read(m, profile.fb_front)
                if buf is not None:
                    self._panel_latch = buf
            at(profile.panel_diff, latch_frame)
        else:
            print('[gui] WARNING: panel_diff/fb_front did not resolve for this '
                  'image; falling back to reading the framebuffer at an '
                  'arbitrary moment, which may tear', flush=True)
        self.ready.set()
        self.stats['status'] = 'running'
        self._pace_t0 = time.time()
        while not self.stop_flag.is_set():
            if self.pause.is_set():
                self.stats['status'] = 'paused'
                self._rate_t = None
                time.sleep(0.05)
                continue
            # Work out the status BEFORE blocking, not after: spin sits
            # inside Unicorn for a whole BUDGET, so whatever is set here is
            # what the UI shows for that whole window. Setting it afterwards
            # leaves the stale value on screen for the entire block and the
            # fresh one for microseconds.
            #
            # fps is measured between completed frames, so it holds its last
            # value forever once the firmware stops drawing. Decay it, or the
            # panel sits frozen while the status line claims 15 fps.
            idle = time.time() - self._frame_t
            if idle > 1.0:
                self.stats['fps'] = 0.0
                self.stats['status'] = 'running, no frame for %.0fs' % idle
            else:
                self.stats['status'] = 'running'
            due_ips, self._pending_ips[:] = (
                [e for e in self._pending_ips
                 if e[0] <= self.stats['instrs']],
                [e for e in self._pending_ips
                 if e[0] > self.stats['instrs']])
            for when, n in due_ips:
                for source in pits.sources:
                    source.ips = n
                print('[gui] ips -> %d at %d' % (n, self.stats['instrs']),
                      flush=True)
            pc = self._drain_input(m, profile, pc)
            pc, executed, stop = spin(m, pc, BUDGET, pits=pits, fast=self.fast)
            if stop != 'limit':
                self.stats['status'] = 'halted: %s' % stop
                # Also to stdout: the status label is invisible to anyone
                # watching the terminal, which is where emu.run prints
                # everything else, so a halt there reads as a freeze.
                total = self.stats['instrs'] + executed
                print('[gui] HALTED: %s  at pc=0x%08x after %dM instr'
                      % (stop, pc, total // 1_000_000), flush=True)
                break
            self.stats['instrs'] += executed
            now = time.time()
            if self._rate_t is None:
                self._rate_t, self._rate_instrs = now, self.stats['instrs']
            elif now - self._rate_t >= 1.0:
                rate = ((self.stats['instrs'] - self._rate_instrs)
                        / (now - self._rate_t))
                self.stats['wall_ips'] = rate
                self.stats['real'] = rate / pits.sources[0].ips
                self._rate_t, self._rate_instrs = now, self.stats['instrs']
            if self.realtime:
                # Sleep off whatever we are ahead of the hardware by. _paced
                # accumulates emulated seconds at the timers' live rate, so a
                # rate change from --ips-at is reflected immediately instead
                # of leaving the pacing keyed to the instruction count at the
                # old rate. Capped per sleep so pause and stop stay
                # responsive.
                self._paced += executed / pits.sources[0].ips
                ahead = self._paced - (time.time() - self._pace_t0)
                if ahead > 0.003:
                    time.sleep(min(ahead, 0.05))
            self._publish_panel(m)
            fired = pits.fired
            self.stats['pit'] = (fired.get('PIT0', 0), fired.get('PIT2', 0),
                                 fired.get('PIT3', 0))
            self.stats['dtim3'] = fired.get('DTIM3', 0)
            # Also say it on stdout: the window shows the panel, but the
            # interesting part of a post-intro run is what the OS is doing,
            # and that was previously visible only from emu.uiprobe.
            self._report()
            self.stats['pc'] = pc
            self.stats['tasks'] = len(ev['tasks'])
            self.stats['prints'] = len(ev['prints'])
            if self.profile.current_tcb is not None:
                try:
                    self.stats['tcb'] = struct.unpack(
                        '>I', m.uc.mem_read(self.profile.current_tcb, 4))[0]
                except UcError:
                    pass
        else:
            self.stats['status'] = 'stopped'
        n_seen = len(m.fault_pages)
        n_kept = len(m.faults)
        capped = ' (truncated at max_fault_records)' if n_kept < n_seen else ''
        print('[gui] faults: %d distinct pages touched, %d records kept%s'
              % (n_seen, n_kept, capped), flush=True)

    def _publish_panel(self, m):
        """Once the OS owns the panel, draw the firmware's framebuffer.

        The frame comes from `_panel_latch`, grabbed at the diff's entry where
        emu/panel.py guarantees [FRONT] is complete and untorn. Polling the
        pointer here instead would sample at an arbitrary point in the flush
        and, after a swap, read the buffer being rendered into next -- the
        window flickered for exactly that reason. The fallback read is only
        for an image where panel_diff did not resolve, so no latch exists.

        A frame is counted when the bytes change, which is the firmware's own
        notion of a new frame -- unlike the setPixel path, which has to infer
        one from a repeated coordinate.
        """
        if not self.use_panel:
            return
        buf = self._panel_latch
        if buf is None:
            assert self.fb_front is not None
            buf = panel.read(m, self.fb_front)
        if buf is None or buf == self._last_panel:
            return
        px = panel.lit(buf)
        if not px and not self._panel_live:
            # INTRO_DONE fires tens of millions of instructions before the OS
            # first composes a frame, and the buffer is empty until it does.
            # Blanking the window for that whole stretch would look like a
            # regression, so hold the intro's last frame until there is
            # something real to replace it with. Once the OS has drawn, later
            # blanks are genuine and do get shown.
            return
        self._panel_live = True
        self._last_panel = buf
        fb = self.fb
        for i in range(W * H):
            fb[i] = 0
        for x, y in px:
            fb[y * W + x] = 1
        now = time.time()
        self.captured.append(bytes(fb))
        self.stats['frames'] += 1
        self.stats['fps'] = 1.0 / max(1e-6, now - self._frame_t)
        self.stats['panel_lit'] = len(px)
        self.stats['source'] = 'panel'
        self._frame_t = now
        self.version += 1

    def _stack_backtrace(self, m, depth=64):
        """Scan upward from A7 for values that look like main OS code addresses.

        This is not a real unwound backtrace -- it is a raw scan of `depth`
        longwords above the current stack pointer, reporting every one that
        falls inside the main OS code span. Some of those will be stale data
        left over from earlier calls rather than live return addresses, but
        with 34 call sites funneling into the same 2-byte trap, even a noisy
        list of candidates is more than the bare PC tells us.

        Does NOT read SR -- reg_read(SR) between emu_start calls clobbers
        condition codes and has deadlocked a guest mutex before.
        """
        candidates = []
        a7 = m.uc.reg_read(UC_M68K_REG_A7)
        for i in range(depth):
            offset = i * 4
            try:
                word = struct.unpack('>I', m.uc.mem_read(a7 + offset, 4))[0]
            except Exception:
                break
            if 0x40000400 <= word <= 0x40307f60:
                candidates.append((offset, word))
        return candidates

    def _report(self):
        """One stdout line per ~20M instructions of OS progress.

        The window shows the panel, which after the intro is mostly blank; the
        part worth watching is what the OS is doing behind it. Printing it here
        means `uv run python -m emu.gui` says the same thing
        `python -m emu.uiprobe run` would, without needing a second run.
        """
        s = self.stats
        step = s['instrs'] // 20_000_000
        if step == self._reported:
            return
        self._reported = step
        note = ''
        if s['terminal']:
            note = ('   TERMINAL LOOP at 0x4012d2fa -- the main task is hung '
                    'on a weak pointer; re-run with --weakptr to step over it')
            if not self._fault_summary_printed:
                self._fault_summary_printed = True
                m = self._m
                recs = sorted(m.faults, key=lambda r: r['count'],
                              reverse=True)[:20]
                print('[gui] fault summary at terminal loop: %d distinct '
                      'pages touched, top %d by count'
                      % (len(m.fault_pages), len(recs)), flush=True)
                for rec in recs:
                    kinds = '+'.join('%s:%d' % (k, c)
                                      for k, c in sorted(rec['kinds'].items()))
                    print('[gui]   page=0x%08x first=0x%08x pc=0x%08x '
                          'count=%d %s'
                          % (rec['page'], rec['first_addr'], rec['first_pc'],
                             rec['count'], kinds), flush=True)
            if not self._backtrace_printed:
                self._backtrace_printed = True
                try:
                    assert self._uc is not None
                    a7 = self._uc.reg_read(UC_M68K_REG_A7)
                    frames = self._stack_backtrace(self._m)[:24]
                    print('[gui] stack at terminal loop (A7=0x%08x, '
                          'candidate return addresses from a raw stack '
                          'scan, not a real unwound backtrace -- some will '
                          'be stale data):' % a7, flush=True)
                    for offset, addr in frames:
                        print('[gui]   +0x%03x  0x%08x' % (offset, addr),
                              flush=True)
                except Exception:
                    pass
        print('[gui] %5.0fM instr  PIT0/2/3 %d/%d/%d  DTIM3 %d  '
              'mainloop %d  jobs %d  tasks %d  %s %d  %.2fM instr/s  '
              '%.0f%% of real time%s'
              % (s['instrs'] / 1e6, s['pit'][0], s['pit'][1], s['pit'][2],
                 s['dtim3'], s['mainloop'], s['jobs'], s['tasks'],
                 s['source'], s['panel_lit'] if s['source'] == 'panel'
                 else s['px'], s['wall_ips'] / 1e6, 100.0 * s['real'],
                 note), flush=True)


def encoder_drag_steps(pixels, threshold=8):
    """Return signed relative encoder detents for a vertical drag distance."""
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    return int(pixels / threshold)


def trigger_position(index):
    """Return the fixed 2x8 physical-grid position for a trigger index."""
    return index // 8, index % 8


class CanvasKey(tk.Canvas):
    """A compact, platform-neutral raised key with button semantics."""

    def __init__(self, master, text, on_press=None, on_release=None,
                 command=None, accent='#dcae45', width=48, height=32,
                 bg='#15181d'):
        super().__init__(master, width=width, height=height, bg=bg,
                         highlightthickness=0, bd=0, cursor='hand2')
        self.text = text
        self.on_press = on_press
        self.on_release = on_release
        self.command = command
        self.accent = accent
        self.pressed = False
        self.latched = False
        self._draw()
        self.bind('<ButtonPress-1>', self._press)
        self.bind('<ButtonRelease-1>', self._release)

    def set_text(self, text):
        self.text = text
        self._draw()

    def set_latched(self, latched):
        self.latched = latched
        self._draw()

    def _draw(self):
        self.delete('all')
        # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
        width = int(self.cget('width'))
        # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
        height = int(self.cget('height'))
        inset = 3 if self.pressed else 1
        face = '#403a30' if self.latched else '#242a31'
        self.create_rectangle(2, 3, width - 2, height - 1, fill='#0b0d10', outline='')
        self.create_rectangle(inset, inset, width - 3, height - 4,
                              fill=face, outline=self.accent, width=1)
        self.create_line(inset + 2, inset + 2, width - 5, inset + 2,
                         fill='#59626d')
        self.create_text(width // 2, height // 2 - (1 if self.pressed else 2),
                         text=self.text, fill='#edf0e8' if self.latched else '#c5ccd4',
                         font=('TkFixedFont', 7), width=width - 8)

    def _press(self, _event):
        self.pressed = True
        self._draw()
        if self.on_press:
            self.on_press()

    def _release(self, _event):
        was_pressed = self.pressed
        self.pressed = False
        self._draw()
        if was_pressed and self.on_release:
            self.on_release()
        if was_pressed and self.command:
            self.command()



class Rotary(tk.Canvas):
    """A relative encoder drawn with Tk primitives, including its push switch."""

    SIZE = 54

    def __init__(self, master, label, turn=None, press=None, release=None,
                 accent='#dcae45'):
        super().__init__(master, width=self.SIZE, height=self.SIZE + 16,
                         bg=Controls.BG, highlightthickness=0, bd=0,
                         cursor='hand2')
        self.label = label
        self.turn = turn
        self.press = press
        self.release = release
        self.accent = accent
        self.angle = 0
        self._drag_y = 0
        self._remainder = 0
        self._draw()
        self.bind('<ButtonPress-1>', self._press)
        self.bind('<B1-Motion>', self._drag)
        self.bind('<ButtonRelease-1>', self._release)
        self.bind('<MouseWheel>', self._wheel)
        self.bind('<Button-4>', lambda _event: self._turn(1))
        self.bind('<Button-5>', lambda _event: self._turn(-1))

    def _draw(self):
        self.delete('all')
        self.create_oval(4, 4, 50, 50, fill='#101419', outline='#434c57', width=2)
        self.create_oval(9, 9, 45, 45, fill='#2a3037', outline='#080a0c')
        import math
        radians = math.radians(self.angle - 90)
        self.create_line(27, 27, 27 + 13 * math.cos(radians),
                         27 + 13 * math.sin(radians), fill=self.accent,
                         width=3, capstyle='round')
        self.create_text(27, 60, text=self.label, fill='#bdc7d2',
                         font=('TkFixedFont', 7))

    def _turn(self, step):
        if not step:
            return
        self.angle = (self.angle + step * 12) % 360
        self._draw()
        if self.turn:
            self.turn(step)

    def _press(self, event):
        self._drag_y = event.y
        self._remainder = 0
        if self.press:
            self.press()

    def _drag(self, event):
        self._remainder += self._drag_y - event.y
        step = encoder_drag_steps(self._remainder)
        if step:
            self._remainder -= step * 8
            self._turn(step)
        self._drag_y = event.y

    def _release(self, _event):
        if self.release:
            self.release()

    def _wheel(self, event):
        step = 1 if event.delta > 0 else -1
        if event.state & 0x0001:
            step *= 10
        self._turn(step)


class Controls(tk.Frame):
    """A bounded hardware-style panel built after device identification."""

    BG = '#15181d'
    FACE = '#242a31'
    TEXT = '#d5dbe3'
    AMBER = '#dcae45'
    ROSE = '#d66578'
    LIME = '#b8cf79'

    def __init__(self, master, device, button_names, encoder_names, send, scale):
        super().__init__(master, bg=self.BG, highlightthickness=1,
                         highlightbackground='#343b44')
        self.device = device
        self.button_names = button_names
        self.encoder_names = encoder_names
        self.send = send
        self.scale = scale
        self.panel = None
        self._latching = frozenset(c for g in device.groups
                                   if g.name in LATCHING_GROUPS for c in g.codes)
        self._latched = {}
        self._build()

    def _label(self, code, kind):
        names = self.button_names if kind == 'button' else self.encoder_names
        return names.get(code) or '#%d' % code

    def _group_labels(self, group):
        labels = {c: self._label(c, group.kind) for c in group.codes}
        if len(labels) < 2:
            return {c: t.rsplit(' ', 1)[-1] for c, t in labels.items()}
        cut = os.path.commonprefix(list(labels.values())).rfind(' ') + 1
        return labels if cut <= 0 else {c: (t[cut:] or t) for c, t in labels.items()}

    def _groups(self):
        return {group.name: group for group in self.device.groups}

    def _build(self):
        groups = self._groups()
        upper = tk.Frame(self, bg=self.BG)
        upper.grid(row=0, column=0, padx=10, pady=(9, 3), sticky='n')
        rotary = tk.Frame(upper, bg=self.BG)
        rotary.grid(row=0, column=0, padx=(0, 8), sticky='n')
        # MONITOR is local-only: the mapped device has no second rotary channel.
        Rotary(rotary, 'MONITOR', accent='#77818d').pack(pady=(0, 5))
        level = groups.get('level')
        if level:
            code = level.codes[0]
            Rotary(rotary, 'LEVEL / DATA',
                   turn=lambda step, c=code: self._encoder_turn(c, step),
                   press=lambda: self.send('press', 49, 0),
                   release=lambda: self.send('release', 49, 0)).pack()

        self.panel = Panel(upper, scale=self.scale)
        self.panel.grid(row=0, column=1, padx=5, sticky='n')

        encoders = tk.Frame(upper, bg=self.BG)
        encoders.grid(row=0, column=2, padx=(8, 0), sticky='n')
        encoder_group = groups.get('encoders')
        if encoder_group:
            labels = self._group_labels(encoder_group)
            for index, code in enumerate(encoder_group.codes):
                push_code = 41 + index
                Rotary(encoders, labels[code],
                       turn=lambda step, c=code: self._encoder_turn(c, step),
                       press=lambda c=push_code: self.send('press', c, 0),
                       release=lambda c=push_code: self.send('release', c, 0)).grid(
                           row=index // 4, column=index % 4, padx=2, pady=2)
        # The six page keys are intentionally tied to the encoder bank, not
        # the display: this matches their physical row directly below A--H.
        pages = groups.get('pages')
        if pages:
            page_row = self._keys_row(encoders, pages, self._group_labels(pages),
                                      width=36, height=30)
            page_row.grid(row=2, column=0, columnspan=4, pady=(4, 0))

        lower = tk.Frame(self, bg=self.BG)
        lower.grid(row=1, column=0, padx=10, pady=(2, 9), sticky='n')
        modifiers = groups.get('modifiers')
        select = groups.get('select')
        modifier_labels = self._group_labels(modifiers) if modifiers else {}
        select_labels = self._group_labels(select) if select else {}

        left_stack = tk.Frame(lower, bg=self.BG)
        left_stack.grid(row=0, column=0, rowspan=2, padx=(0, 7), sticky='n')
        # The firmware names identify these controls; arrange their physical
        # stack independently of their numerical wire order.
        modifier_by_name = {label.upper(): code for code, label in modifier_labels.items()}
        stacked_modifiers = set()
        for row, name in enumerate(('FUNC', 'TRK')):
            code = modifier_by_name.get(name)
            if code is not None:
                stacked_modifiers.add(code)
                self._button(left_stack, code, modifier_labels[code]).grid(
                    row=row, column=0, pady=2)
        if select:
            for row, code in enumerate(select.codes[1:], start=2):
                self._button(left_stack, code, select_labels[code]).grid(
                    row=row, column=0, pady=2)

        middle = tk.Frame(lower, bg=self.BG)
        middle.grid(row=0, column=1, padx=4, sticky='nw')
        modes = groups.get('modes')
        if modes:
            self._keys_row(middle, modes, self._group_labels(modes)).grid(
                row=0, column=0, padx=2, pady=2, sticky='w')
        # Keep the third modifier (normally keyboard setup) with the other
        # mode tools after FUNC and TRK have moved into the physical stack.
        remaining_modifiers = [code for code in (modifiers.codes if modifiers else ())
                               if code not in stacked_modifiers]
        if remaining_modifiers and modifiers:
            self._keys_row(middle, modifiers, modifier_labels,
                           codes=remaining_modifiers).grid(row=0, column=1, padx=2, pady=2)
        transport = groups.get('transport')
        if transport:
            self._keys_row(middle, transport, self._group_labels(transport)).grid(
                row=1, column=0, padx=2, pady=2, sticky='w')
        product = groups.get('product')
        if product:
            self._keys_row(middle, product, self._group_labels(product)).grid(
                row=1, column=1, padx=2, pady=2, sticky='w')

        right = tk.Frame(lower, bg=self.BG)
        right.grid(row=0, column=2, padx=(8, 0), sticky='ne')
        confirm = groups.get('confirm')
        if confirm:
            confirm_box = tk.Frame(right, bg=self.BG)
            confirm_box.grid(row=0, column=0, padx=2, sticky='n')
            for row, code in enumerate(confirm.codes):
                self._button(confirm_box, code, self._group_labels(confirm)[code],
                             width=42, height=28).grid(row=row, column=0, pady=1)
        arrows = groups.get('arrows')
        if arrows:
            self._button_group(right, arrows).grid(row=0, column=1, padx=2, sticky='n')
        if select and select.codes:
            self._button(right, select.codes[0], select_labels[select.codes[0]],
                         width=52, height=32).grid(row=0, column=2, padx=2, sticky='n')

        trigs = groups.get('trigs')
        if trigs:
            trig_box = tk.Frame(lower, bg='#1b2026', highlightthickness=1,
                                highlightbackground='#38414b')
            trig_box.grid(row=1, column=1, columnspan=2, padx=(4, 0), pady=(5, 0))
            labels = self._group_labels(trigs)
            for index, code in enumerate(trigs.codes):
                accent = self.ROSE if index < 8 else self.LIME
                row, column = trigger_position(index)
                self._button(trig_box, code, labels[code], accent=accent,
                             width=72, height=40).grid(row=row, column=column,
                                                       padx=2, pady=3)

    def _keys_row(self, parent, group, labels, codes=None, width=48, height=32):
        box = tk.Frame(parent, bg=self.BG)
        for column, code in enumerate(codes if codes is not None else group.codes):
            self._button(box, code, labels[code], width=width, height=height).grid(
                row=0, column=column, padx=1, pady=1)
        return box

    def _button_group(self, parent, group):
        box = tk.Frame(parent, bg=self.BG)
        labels = self._group_labels(group)
        if group.layout == 'dpad':
            places = ((0, 1), (1, 0), (1, 1), (1, 2))
            for code, (row, column) in zip(group.codes, places):
                self._button(box, code, labels[code], width=34, height=28).grid(
                    row=row, column=column, padx=1, pady=1)
        else:
            columns = group.columns or len(group.codes)
            for index, code in enumerate(group.codes):
                self._button(box, code, labels[code]).grid(
                    row=index // columns, column=index % columns, padx=1, pady=1)
        return box

    def _button(self, box, code, text, accent=None, width=48, height=32):
        accent = accent or self.AMBER
        if code in self._latching:
            widget = CanvasKey(box, text, accent=accent, width=width, height=height,
                               on_press=lambda c=code: self._toggle_latch(c, widget))
        else:
            widget = CanvasKey(box, text, accent=accent, width=width, height=height,
                               on_press=lambda c=code: self.send('press', c, 0),
                               on_release=lambda c=code: self.send('release', c, 0))
        return widget

    def _toggle_latch(self, code, widget):
        if code in self._latched:
            self.send('release', code, 0)
            del self._latched[code]
            widget.set_latched(False)
        else:
            self.send('press', code, 0)
            self._latched[code] = widget
            widget.set_latched(True)

    def _consume_latched(self):
        for code, widget in self._latched.items():
            self.send('release', code, 0)
            widget.set_latched(False)
        self._latched.clear()

    def _encoder_turn(self, code, step):
        self.send('encoder', code, step)


class Panel(tk.Frame):
    def __init__(self, master, scale=7):
        super().__init__(master, bg='#0b0d10')
        self.scale = scale
        self.img = tk.PhotoImage(width=W, height=H)
        self.big = tk.PhotoImage(width=W * scale, height=H * scale)
        self.view = tk.Label(self, bd=0, highlightthickness=0, bg='#0b0d10',
                             image=self.big)
        self.view.pack(padx=18, pady=18)
        self._blank()

    def _blank(self):
        self.draw(bytearray(W * H))

    def draw(self, fb):
        body = b''.join(ON if v else OFF for v in fb)
        self.img.put(b'P6\n%d %d\n255\n' % (W, H) + body, to=(0, 0, W, H))
        # copy -zoom writes into the existing image; PhotoImage.zoom would
        # allocate a new one every refresh.
        self.tk.call(self.big, 'copy', self.img, '-zoom', self.scale, self.scale)


class App(tk.Tk):
    def __init__(self, snapshot, weakptr=False, slc=False, scale=None,
                 syx=None, fast=True, realtime=True,
                 patch_machine: bool | tuple[str, ...] = False,
                 patch_eighth=7, patch_machine_spec=None,
                 panel_dwell=PANEL_DWELL_CHUNKS, ips_at=(),
                 post_intro_ips=4 * INSTR_PER_SEC):
        super().__init__()
        self.title('Hardware-style emulator')
        self.configure(bg='#15181d')
        self.snapshot = snapshot
        # Keep the complete front panel bounded even on large Retina displays.
        # Integer scaling preserves the guest's native pixels; three fits the
        # OLED into the center of an approximately 980x740-point instrument.
        if scale is None:
            avail_w = max(1, self.winfo_screenwidth() - 460)
            avail_h = max(1, self.winfo_screenheight() - RESERVE_H)
            scale = max(1, min(MAX_SCALE, avail_w // W, avail_h // H))
        self.scale = scale
        self.geometry('%dx%d' % (min(980, self.winfo_screenwidth() - 80),
                                  min(740, self.winfo_screenheight() - 120)))

        self.surface = tk.Frame(self, bg='#15181d')
        self.surface.pack(padx=12, pady=(10, 4))
        self.panel: Panel | None = None  # constructed after identification
        self.controls = None

        # Emulator tools stay in a narrow service footer below the instrument.
        bar = tk.Frame(self, bg='#15181d')
        bar.pack(fill='x', padx=16, pady=(0, 3))
        self.btn = CanvasKey(bar, 'PAUSE', command=self.toggle, width=58, height=27)
        self.btn.pack(side='left')
        CanvasKey(bar, 'RESTART', command=self.restart, width=62, height=27).pack(
            side='left', padx=3)
        CanvasKey(bar, 'SAVE', command=self.save, width=52, height=27).pack(side='left')
        self.replay_btn = CanvasKey(bar, 'REPLAY', command=self.toggle_replay,
                                    width=62, height=27)
        self.replay_btn.pack(side='left', padx=3)
        self.frames_lbl = tk.Label(bar, text='', bg='#15181d', fg='#7f8b9c',
                                   font=('TkFixedFont', 8))
        self.frames_lbl.pack(side='right')

        self.status = tk.Label(self, text='', bg='#15181d', fg='#8793a1',
                               font=('TkFixedFont', 8), anchor='w', justify='left')
        self.status.pack(fill='x', padx=16, pady=(0, 7))

        self.emu = None
        self.weakptr = weakptr
        self.slc = slc
        self.syx = syx
        self.fast = fast
        self.realtime = realtime
        self.patch_machine = patch_machine
        self.patch_eighth = patch_eighth
        self.patch_machine_spec = patch_machine_spec
        self.panel_dwell = panel_dwell
        self.ips_at = ips_at
        self.post_intro_ips = post_intro_ips
        self.shown = -1
        self.replay = None          # (frames, index, next_due) while replaying
        self.start()
        self.protocol('WM_DELETE_WINDOW', self.quit_all)
        self.after(60, self.tick)

    def start(self):
        self.emu = Emulator(self.snapshot, weakptr=self.weakptr,
                            slc=self.slc, syx=self.syx, fast=self.fast,
                            realtime=self.realtime,
                            patch_machine=self.patch_machine,
                            patch_eighth=self.patch_eighth,
                            patch_machine_spec=self.patch_machine_spec,
                            panel_dwell=self.panel_dwell,
                            ips_at=self.ips_at,
                            post_intro_ips=self.post_intro_ips)
        self.emu.start()

    def send_input(self, kind, code, arg):
        """Hand one panel event to the worker. Never touches guest memory."""
        if self.emu:
            self.emu.inbox.append((kind, code, arg))

    def _ensure_controls(self):
        """Build the control surface once the worker has identified the device."""
        if self.controls is not None or not self.emu or not self.emu.device:
            return
        self.controls = Controls(self.surface, self.emu.device,
                                 self.emu.button_names, self.emu.encoder_names,
                                 self.send_input, self.scale)
        self.controls.pack()
        self.panel = self.controls.panel

    def restart(self):
        if self.controls is not None:
            self.controls.destroy()
            self.controls = None
            self.panel = None
        if self.emu:
            self.emu.stop_flag.set()
            self.emu.pause.clear()
            self.emu.join(timeout=3)
        self.shown = -1
        self.replay = None
        self.replay_btn.set_text('REPLAY')
        self.start()
        self.btn.set_text('PAUSE')

    def toggle_replay(self):
        """Play the captured frames back at the rate the firmware asks for.

        Emulating in real time needs ~3x more throughput than we have, but the
        frames themselves are correct -- so replaying them at FRAME_HZ shows
        the animation at its true speed even though producing it was slower.
        """
        if self.replay is not None:
            self.replay = None
            self.replay_btn.set_text('REPLAY')
            return
        frames = list(self.emu.captured) if self.emu else []
        if not frames:
            self.status.configure(text='nothing captured yet - let it run first')
            return
        if self.emu:
            self.emu.pause.set()
            self.btn.set_text('RESUME')
        self.replay = [frames, 0, time.time()]
        self.replay_btn.set_text('STOP REPLAY')

    def toggle(self):
        if not self.emu:
            return
        if self.emu.pause.is_set():
            self.emu.pause.clear()
            self.btn.set_text('PAUSE')
        else:
            self.emu.pause.set()
            self.btn.set_text('RESUME')

    def save(self):
        if not self.emu:
            return
        os.makedirs('out', exist_ok=True)
        s = 6
        px = bytearray(W * s * H * s)
        for i, v in enumerate(self.emu.fb):
            if v:
                x, y = i % W, i // W
                for dy in range(s):
                    row = (y * s + dy) * W * s + x * s
                    for dx in range(s):
                        px[row + dx] = 255
        open('out/panel.png', 'wb').write(png(px, W * s, H * s))
        self.status.configure(text='wrote out/panel.png')

    def tick(self):
        if self.replay is not None:
            frames, i, due = self.replay
            now = time.time()
            if now >= due:
                if self.panel is not None:
                    self.panel.draw(frames[i])
                i = (i + 1) % len(frames)
                self.replay = [frames, i, max(now, due + 1.0 / FRAME_HZ)]
                self.frames_lbl.configure(
                    text='replay %d/%d at %.2f fps (true speed)'
                         % (i, len(frames), FRAME_HZ))
                self.status.configure(
                    text='replaying captured frames at the firmware\'s own rate\n'
                         'PIT3: (0x2191+1) x 1024 = 8,800,256 bus cycles @ 132 MHz',
                    fg='#9aa7b8')
            self.after(10, self.tick)
            return
        e = self.emu
        if e:
            if e.error:
                self.status.configure(text=e.error, fg='#ff8f8f')
            else:
                self._ensure_controls()
                if self.panel is not None and e.version != self.shown:
                    self.panel.draw(e.fb)      # skip if nothing was drawn
                    self.shown = e.version
                s = e.stats
                self.frames_lbl.configure(
                    text='frame %d   %.1f / %.1f fps   %.2fM instr/s  '
                         '(%.0f%% of real time)'
                         % (s['frames'], s['fps'], FRAME_HZ,
                            s['wall_ips'] / 1e6, 100.0 * s['real']))
                extra = ('  HUNG: terminal loop 0x4012d2fa (try --weakptr)'
                         if s['terminal'] else '')
                self.status.configure(
                    text='%s   %.1f fps   %d frames   %d tasks%s\n'
                         'pc 0x%08x   task 0x%08x   source %s   %s %d\n'
                         'PIT0 %d   PIT2 %d   PIT3 %d   DTIM3 %d   '
                         'mainloop %d   jobs %d   %.1fM instructions'
                         % (s['status'], s['fps'], s['frames'], s['tasks'],
                            extra,
                            s['pc'], s['tcb'], s['source'],
                            'lit' if s['source'] == 'panel' else 'setPixel',
                            s['panel_lit'] if s['source'] == 'panel'
                            else s['px'],
                            s['pit'][0], s['pit'][1], s['pit'][2],
                            s['dtim3'], s['mainloop'], s['jobs'],
                            s['instrs'] / 1e6),
                    fg='#9aa7b8')
        self.after(60, self.tick)

    def quit_all(self):
        # Join before tearing down: the worker is inside Unicorn between
        # chunks, and letting the interpreter kill a daemon thread mid-
        # emu_start crashes the process on exit (SIGBUS).
        if self.emu:
            self.emu.stop_flag.set()
            self.emu.pause.clear()
            self.emu.join(timeout=3)
        self.destroy()


def parse_count(s):
    if s and s[-1] in ('M', 'm'):
        return int(float(s[:-1]) * 1_000_000)
    return int(s, 0)


if __name__ == '__main__':
    # --weakptr steps over the weak-pointer branches that otherwise freeze the
    # main task in the terminal loop after 153 messages. It is a diagnostic,
    # not a fix -- see longrun.build.  --scale N forces the integer panel zoom.
    argv = sys.argv[1:]
    weakptr = '--weakptr' in argv
    slc = '--slc' in argv
    # --exact restores `count=` stepping: slower by about 7.6x, but every
    # timer lands on the instruction it was due at. Use it when comparing a
    # run against bootcheck, never for ordinary interactive use.
    # --unthrottled lets the worker run as fast as it can instead of pacing
    # itself to the hardware's clock.
    fast = '--exact' not in argv
    realtime = '--unthrottled' not in argv
    # --patch-machine installs the experimental eighth machine (PLACEHOLDER)
    # into the machine list. Bare, it applies all nine parts (list, dispatch,
    # group, name, rank, permit, hint, pertype, clone); --patch-machine=list,
    # =dispatch, =group, =name, =rank, =permit, =hint, =pertype, or =clone
    # applies just that part, and a +-separated combination
    # (--patch-machine=list+dispatch) applies exactly those, for bisecting.
    # An optional :N suffix on the parts value (e.g. --patch-machine=list:6)
    # sets the 8th list entry's value, default 7.
    # Unknown part names are refused by machinepatch.patch_b.
    patch_machine = False
    patch_eighth = 7
    patch_machine_spec = None
    for a in argv:
        if a == '--patch-machine':
            patch_machine = ('list', 'dispatch', 'group', 'name', 'rank',
                              'permit', 'hint', 'pertype', 'clone')
        elif a.startswith('--patch-machine='):
            value = a.split('=', 1)[1]
            if ':' in value:
                parts_str, eighth_str = value.split(':', 1)
                patch_eighth = int(eighth_str, 0)
            else:
                parts_str = value
            patch_machine = tuple(parts_str.split('+'))
        elif a.startswith('--machine='):
            patch_machine_spec = a.split('=', 1)[1]
    scale = None
    if '--scale' in argv:
        i = argv.index('--scale')
        scale = max(1, int(argv[i + 1]))
        del argv[i:i + 2]
    # --panel-dwell N overrides PANEL_DWELL_CHUNKS; 0 disables pacing and
    # restores the old coalesce-everything-into-one-feed behaviour, for
    # testing the two against each other.
    panel_dwell = PANEL_DWELL_CHUNKS
    if '--panel-dwell' in argv:
        i = argv.index('--panel-dwell')
        panel_dwell = max(0, int(argv[i + 1]))
        del argv[i:i + 2]
    syx = None
    if '--syx' in argv:
        i = argv.index('--syx')
        syx = argv[i + 1]
        del argv[i:i + 2]
    # --ips-at WHEN:N (repeatable) changes the timer rate to N instructions
    # per emulated second once instruction count WHEN is reached. WHEN and N
    # both accept a plain integer or an M-suffixed count (80M, 18.72M).
    ips_at = []
    while '--ips-at' in argv or any(a.startswith('--ips-at=') for a in argv):
        if '--ips-at' in argv:
            i = argv.index('--ips-at')
            spec = argv[i + 1]
            del argv[i:i + 2]
        else:
            i = next(j for j, a in enumerate(argv)
                     if a.startswith('--ips-at='))
            spec = argv[i].split('=', 1)[1]
            del argv[i:i + 1]
        if ':' not in spec:
            raise SystemExit('--ips-at expects WHEN:N, got %r' % spec)
        when_str, n_str = spec.split(':', 1)
        ips_at.append((parse_count(when_str), parse_count(n_str)))
    ips_at.sort()
    # --post-intro-ips N sets the timer rate applied once the intro hands
    # over (default 4x INSTR_PER_SEC, which keeps the UI queue drained); 0
    # keeps the default rate. Ignored when --ips-at is given.
    post_intro_ips = 4 * INSTR_PER_SEC
    if '--post-intro-ips' in argv:
        i = argv.index('--post-intro-ips')
        post_intro_ips = max(0, parse_count(argv[i + 1]))
        del argv[i:i + 2]
    args = [a for a in argv if not a.startswith('--')]
    snap = args[0] if args else 'snapshots/boot400M.snap'
    if not os.path.exists(snap):
        raise SystemExit('no such snapshot: %s\n'
                         'build one with:  uv run python -m emu.checkpoint make '
                         '60000000,120000000,200000000,280000000,400000000' % snap)
    App(snap, weakptr=weakptr, slc=slc, scale=scale, syx=syx, fast=fast,
        realtime=realtime, patch_machine=patch_machine,
        patch_eighth=patch_eighth, patch_machine_spec=patch_machine_spec,
        panel_dwell=panel_dwell, ips_at=ips_at,
        post_intro_ips=post_intro_ips).mainloop()
