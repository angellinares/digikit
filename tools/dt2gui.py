#!/usr/bin/env python3
# pyright: reportMissingImports=false
"""Run the GUI with your own samples, in one command.

    uv run python tools/dt2gui.py
    uv run python tools/dt2gui.py --samples my-kit/
    uv run python tools/dt2gui.py --no-gui        # just build and check
    uv run python tools/dt2gui.py --rebuild       # force a clean rebuild
    uv run python tools/dt2gui.py --live-audio    # hear the DSP (emu/gui.py)

Doing this by hand is six separate commands (build the +Drive image with
tools/plusdrive.py, build a boot ladder with emu.checkpoint, run
tools/guirun.py until "Load all samples" finishes, close the two modal
windows a +Drive cold boot opens, then launch emu.gui with the matching
image) and any mismatch between the image and the snapshot silently shows
SampleManager as empty instead of failing loudly. This tool does all of it
under one content key, reusing whatever it can and only rebuilding what the
key says changed.

The key is sha256(firmware sha256, tools/plusdrive.py's own sha256, and
each *.wav directly under --samples with its bytes) -- everything that
actually determines the +Drive image's content. It is truncated to 12 hex
characters to name two directories:

    out/plusdrive/auto/<key12>/dt2.img              the +Drive image
    snapshots/dt2-1.16-auto/<key12>/boot*.snap       the boot ladder
    snapshots/dt2-1.16-auto/<key12>/samples.snap     samples loaded
    snapshots/dt2-1.16-auto/<key12>/ready.snap       + both modals closed
    snapshots/dt2-1.16-auto/<key12>/flexbus.raw      the load's FlexBus log

The FlexBus log is what the ColdFire streamed to the SHARC while loading
the samples; `--live-audio` (passed through to emu/gui.py) feeds it to the
native SHARC core's start state (emu/livesharc.py), so the live DSP holds
the same samples as this card. A directory built before the log was
recorded gets its ready snapshot rebuilt when `--live-audio` needs the log.

A directory that already has these files is reused untouched; changing a
sample, the firmware, or plusdrive.py's own code changes the key and gets a
fresh directory instead of silently resuming a mismatched one. --rebuild
forces a fresh build under the current key regardless.

emu/run.py's `need_snapshot` already knows how to build-or-reuse a boot
ladder against a card image sidecar (emu.checkpoint's `.ladder.json`); this
reuses it rather than re-deriving the same staleness check.
"""

import argparse
import hashlib
import os
import struct
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, "tools"))

import tools.plusdrive as plusdrive  # noqa: E402
import tools.plusdrive_check as plusdrive_check  # noqa: E402
from emu import config, panelin, symbols  # noqa: E402
from emu.checkpoint import save_longrun, sha256_file  # noqa: E402
from emu.dtim import Dtims, Timers  # noqa: E402
from emu.longrun import build as lr_build  # noqa: E402
from emu.longrun import spin  # noqa: E402
from emu.pit import Pits  # noqa: E402
from emu.run import need_snapshot  # noqa: E402

SNAP_ROOT = os.path.join("snapshots", "dt2-1.16-auto")
IMG_ROOT = os.path.join("out", "plusdrive", "auto")

# The proven "Load all samples" budget: docs/findings/07-emulator.md's
# "A +Drive cold boot loads a sample end to end" measured this at 88.5s wall
# (4.52M instr/s) for the one-sample boot project tools/plusdrive.py builds,
# with the queue, its invoker and every FUN_40154540/FUN_400cd638 call
# already done well before 390M. A much larger sample set would need more;
# nothing here detects that automatically, so raise LOAD_LIMIT/LOAD_SAVE_AT
# if a future --samples directory does not finish within this budget --
# --no-gui will simply report guirun stopping at the limit rather than
# lying about it having finished.
LOAD_LIMIT = 400_000_000
LOAD_SAVE_AT = 390_000_000

# The load run's FlexBus log, next to the snapshots (emu/livesharc.py's
# default LP0 feed for --live-audio). The name must end in .raw
# (tools/sharc_lp0.py reads any other extension as records).
FLEXBUS_LOG = "flexbus.raw"

# emu/panelin.py's own button code space (read back live from the firmware's
# name table by tools/dt2gui.py's own --check-names, and cross-checked
# against docs/findings/03-ui-and-panel.md): NO = 12.
NO_CODE = 12

# docs/findings/03-ui-and-panel.md, "Modal windows take every key": the 1.16
# key dispatcher FUN_4011c13a walks an intrusive list headed at this fixed
# address (stable across boots -- this firmware has no ASLR), youngest view
# first. Node layout (from FUN_4011c13a's own decompile,
# out/ghidra/dt2-1.16-emac/decomp/4011c13a_FUN_4011c13a.c): next pointer at
# node+4, the view pointer at node+8, walked from the sentinel at
# VIEW_STACK+0x14 until back at its own recorded head.
VIEW_STACK = 0x44F5DE80
CONFIRM_WINDOW_VTABLE = 0x4021C898


def content_key(samples_dir, syx):
    """-> 64-hex sha256 of everything that determines the +Drive image."""
    h = hashlib.sha256()
    h.update(b"firmware\0" + sha256_file(syx).encode() + b"\0")
    h.update(
        b"plusdrive.py\0"
        + sha256_file(os.path.join(ROOT, "tools", "plusdrive.py")).encode()
        + b"\0"
    )
    names = sorted(
        n
        for n in os.listdir(samples_dir)
        if n.lower().endswith(".wav") and os.path.isfile(os.path.join(samples_dir, n))
    )
    for name in names:
        h.update(
            b"sample\0"
            + name.encode()
            + b"\0"
            + sha256_file(os.path.join(samples_dir, name)).encode()
            + b"\0"
        )
    return h.hexdigest(), names


def build_card_image(samples_dir, img_path, syx):
    """Build the +Drive image and validate it with the firmware's own
    mount/directory/load code (tools/plusdrive_check.py)."""
    print("[dt2gui] building %s from %s ..." % (img_path, samples_dir), flush=True)
    t0 = time.time()
    with open(config.main_image(), "rb") as fh:
        main_os = fh.read()
    entries = plusdrive.build(samples_dir, img_path, main_os=main_os)
    for e in entries:
        print("  %-24s %d bytes" % (e["name"], e["size"]))
    print("[dt2gui] validating with the firmware's own checks ...", flush=True)
    plusdrive_check.run_checks(img_path, main_os=config.main_image())
    print(
        "[dt2gui] built and validated %s in %.0fs" % (img_path, time.time() - t0),
        flush=True,
    )


def _r32(m, addr):
    return struct.unpack(">I", bytes(m.uc.mem_read(addr, 4)))[0]


def window_stack(m, base=VIEW_STACK):
    """-> [view_ptr, ...] youngest first, exactly as FUN_4011c13a walks it.

    A literal translation of the decompile cited above, not a
    reimplementation from first principles: whatever that loop does is by
    definition what "the top of the stack" means to the key dispatcher, so
    this follows the same pointers rather than a guessed list layout.
    """
    stack = []
    sentinel = _r32(m, base + 0x14)
    i = base + 0x14
    guard = 0
    while i != sentinel and guard < 64:
        i = _r32(m, i + 4)
        stack.append(_r32(m, i + 8))
        guard += 1
    return stack


def modal_open(m):
    stack = window_stack(m)
    if not stack:
        return False
    return _r32(m, stack[0]) == CONFIRM_WINDOW_VTABLE


def close_modals(
    snapshot_in, card_image, syx, out_path, max_instrs=30_000_000, chunk=1_000_000
):
    """Resume `snapshot_in` (samples already loaded) and press NO until no
    modal window is on top of the view stack (docs/findings/03-ui-and-panel.md,
    "Modal windows take every key"), then save `out_path`.

    Adaptive rather than a fixed press schedule: it presses NO only while
    `modal_open` says one is actually there, and stops once that has been
    false for two consecutive checks (one quiet check could be a torn frame
    mid-transition to the second modal).
    """
    m, ev, st, pc, inq, at = lr_build(
        snapshot_in,
        unblock=True,
        softfloat=True,
        bitmap=True,
        dsp=True,
        syx=syx,
        card_image=card_image,
        deferred_components=("timers",),
    )
    with open(config.main_image(), "rb") as fh:
        main_img = fh.read()
    profile = symbols.resolve(main_img)
    device, _fw = devices_identify(syx)
    held = panelin.Held(device)
    pits = ev["restore_checkpoint_timers"]()
    if pits is None:
        pits = Timers(Pits(m), Dtims(m, channels=(3,)))

    def tap_no():
        nonlocal pc
        pos = held.press(NO_CODE)
        if pos is not None:
            pc = panelin.feed(m, profile, panelin.encode_buttons(*pos))
        pc, _n, _stop = spin(m, pc, chunk, pits=pits, fast=True)
        pos = held.release(NO_CODE)
        if pos is not None:
            pc = panelin.feed(m, profile, panelin.encode_buttons(*pos))
        pc, _n, _stop = spin(m, pc, chunk, pits=pits, fast=True)

    done = 0
    quiet = 0
    closed = 0
    while done < max_instrs and quiet < 2:
        if modal_open(m):
            tap_no()
            closed += 1
            quiet = 0
            done += 2 * chunk
        else:
            quiet += 1
            pc, n, _stop = spin(m, pc, chunk, pits=pits, fast=True)
            done += n
    stack = window_stack(m)
    print(
        "[dt2gui] closed %d modal(s), %d instrs, view stack depth %d, top=0x%08x"
        % (closed, done, len(stack), _r32(m, stack[0]) if stack else 0),
        flush=True,
    )
    if modal_open(m):
        raise RuntimeError(
            "a modal window is still on top of the view stack after %d "
            'instructions -- see docs/findings/03-ui-and-panel.md, "Modal '
            'windows take every key"' % done
        )
    save_longrun(m, ev, pits, out_path, extra={"auto_closed_modals": closed})
    return closed


def devices_identify(syx):
    from emu import device as devices

    return devices.identify(config.firmware(syx))


def _run_logged(cmd, log_path):
    """Run `cmd`, writing every line to `log_path` and echoing progress
    lines to the terminal -- but not guirun's end-of-run per-sector eSDHC
    dump (thousands of numbers on one line), which would otherwise bury the
    "what did this reuse/rebuild" summary this tool exists to give."""
    os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
    with (
        open(log_path, "w") as log,
        subprocess.Popen(
            cmd,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        ) as proc,
    ):
        assert proc.stdout is not None
        for line in proc.stdout:
            log.write(line)
            if "sectors read" in line or "sector runs" in line or len(line) > 300:
                continue
            print(line, end="", flush=True)
        proc.wait()
    return proc.returncode


def build_ready_snapshot(snap_dir, img_path, syx):
    boot400 = os.path.join(snap_dir, "boot400M.snap")
    samples_path = os.path.join(snap_dir, "samples.snap")
    ready_path = os.path.join(snap_dir, "ready.snap")
    log_path = os.path.join(snap_dir, "load.log")
    flexbus_path = os.path.join(snap_dir, FLEXBUS_LOG)
    print(
        '[dt2gui] driving the boot forward: "Load all samples" '
        "(usually under 2 minutes; full log -> %s) ..." % log_path,
        flush=True,
    )
    t0 = time.time()
    cmd = [
        sys.executable,
        os.path.join("tools", "guirun.py"),
        boot400,
        "--card-image",
        img_path,
        "--syx",
        syx,
        "--intro-timers",
        "pit3",
        "--limit",
        str(LOAD_LIMIT),
        "--save-at",
        "%d:%s" % (LOAD_SAVE_AT, samples_path),
        "--flexbus-log",
        flexbus_path,
    ]
    rc = _run_logged(cmd, log_path)
    if rc != 0 or not os.path.exists(samples_path):
        raise SystemExit("loading samples failed; see %s" % log_path)
    print(
        "[dt2gui] samples loaded in %.0fs -> %s" % (time.time() - t0, samples_path),
        flush=True,
    )
    print(
        "[dt2gui] closing the two modal windows a +Drive cold boot opens "
        "(finding 03) ...",
        flush=True,
    )
    t0 = time.time()
    closed = close_modals(samples_path, img_path, syx, ready_path)
    print(
        "[dt2gui] closed %d modal window(s) in %.0fs -> %s"
        % (closed, time.time() - t0, ready_path),
        flush=True,
    )
    return ready_path


def prepare(samples_dir, syx, rebuild=False, need_flexbus=False):
    """Build or reuse everything under one content key. -> (ready_snap, img_path).

    NEED_FLEXBUS: the ready snapshot's directory must also hold the load's
    FlexBus log (FLEXBUS_LOG); an older directory without it has its ready
    snapshot rebuilt, which records the log.
    """
    key, names = content_key(samples_dir, syx)
    key12 = key[:12]
    if not names:
        raise SystemExit("no .wav files directly under %s" % samples_dir)
    print(
        "[dt2gui] key %s from %d sample(s): %s" % (key12, len(names), ", ".join(names))
    )

    img_dir = os.path.join(IMG_ROOT, key12)
    img_path = os.path.join(img_dir, "dt2.img")
    snap_dir = os.path.join(SNAP_ROOT, key12)
    ready_path = os.path.join(snap_dir, "ready.snap")

    if rebuild:
        import shutil

        for d in (img_dir, snap_dir):
            if os.path.exists(d):
                shutil.rmtree(d)

    if os.path.exists(img_path):
        print("[dt2gui] reusing %s" % img_path)
    else:
        build_card_image(samples_dir, img_path, syx)

    boot400 = os.path.join(snap_dir, "boot400M.snap")
    prefix = os.path.join(snap_dir, "boot")
    print("[dt2gui] boot ladder: %s" % boot400)
    rebuilt = need_snapshot(boot400, prefix, syx, card_image=img_path)
    flexbus_missing = need_flexbus and not os.path.exists(
        os.path.join(snap_dir, FLEXBUS_LOG)
    )
    if not rebuilt and os.path.exists(ready_path) and not flexbus_missing:
        print("[dt2gui] reusing %s" % ready_path)
        return ready_path, img_path
    if not rebuilt and flexbus_missing and os.path.exists(ready_path):
        print(
            "[dt2gui] %s has no %s (needed by --live-audio) -- rebuilding "
            "the ready snapshot to record it" % (snap_dir, FLEXBUS_LOG)
        )

    if rebuilt and os.path.exists(ready_path):
        print("[dt2gui] boot ladder changed -- rebuilding %s too" % ready_path)
    ready_path = build_ready_snapshot(snap_dir, img_path, syx)
    return ready_path, img_path


def main(argv):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument(
        "--samples",
        default="samples",
        help="directory of .wav files to put on the +Drive (default: samples)",
    )
    p.add_argument(
        "--no-gui",
        action="store_true",
        help="build/reuse everything and stop, without opening the GUI",
    )
    p.add_argument(
        "--rebuild",
        action="store_true",
        help="force a clean rebuild under the current content key",
    )
    p.add_argument("--syx", default=None)
    args, gui_extra = p.parse_known_args(argv)

    syx = config.firmware(args.syx)
    samples_dir = args.samples
    if not os.path.isdir(samples_dir):
        raise SystemExit("no such samples directory: %s" % samples_dir)

    t0 = time.time()
    # emu/gui.py --live-audio feeds the load's FlexBus log to the live DSP
    # unless --live-lp0 names one.
    need_flexbus = "--live-audio" in gui_extra and "--live-lp0" not in gui_extra
    ready_path, img_path = prepare(
        samples_dir, syx, rebuild=args.rebuild, need_flexbus=need_flexbus
    )
    print(
        "[dt2gui] ready in %.0fs total: %s + %s"
        % (time.time() - t0, ready_path, img_path)
    )

    if args.no_gui:
        return 0
    cmd = [
        sys.executable,
        "-m",
        "emu.gui",
        ready_path,
        "--syx",
        syx,
        "--card-image",
        img_path,
    ] + gui_extra
    print("$ %s" % " ".join(cmd), flush=True)
    return subprocess.call(cmd, cwd=ROOT)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
