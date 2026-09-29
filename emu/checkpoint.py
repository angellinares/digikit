# pyright: reportMissingImports=false
"""Create a boot snapshot, and resume from one.

make:   uv run python -m emu.checkpoint make 60000000,400000000 [prefix] [syx]
        [--no-sdgate --no-esdhc] [--card-image PATH] [--coverage]
resume: uv run python -m emu.checkpoint resume snapshots/boot400M.snap 5000000
"""

import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from unicorn.m68k_const import UC_M68K_REG_PC

from unicorn import UcError

import emu.dspboot as db
import emu.longrun as lr
from emu import config
from emu.snapshot import save


def sha256_file(path, chunk=1 << 20):
    """-> hex sha256 of a file, streamed (safe for a large sparse card image:
    reading its zero-filled holes costs memory bandwidth, not disk I/O)."""
    h = hashlib.sha256()
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(chunk), b""):
            h.update(block)
    return h.hexdigest()


def _integer(text, label):
    try:
        return int(text)
    except (TypeError, ValueError) as exc:
        raise SystemExit("invalid %s: %r" % (label, text)) from exc


def _points(text):
    return [
        _integer(value, "checkpoint instruction count") for value in text.split(",")
    ]


def save_longrun(machine, ev, timers, path, extra=None):
    """Save stateful longrun state, including timer cadence.

    Example: ``save_longrun(m, ev, timers, '/tmp/run.snap', {'n': n})``.
    On restore, call ``ev['restore_checkpoint_timers']()`` before execution.
    ``make`` and timer-less ``resume`` remain legacy helpers and do not save
    timer cadence.
    """
    components = dict(ev["checkpoint_components"])
    components["timers"] = timers
    return save(
        machine,
        path,
        extra=extra,
        components=components,
        manifest=ev["checkpoint_manifest"],
    )


def _make_with_coverage(syx, img, points, prefix, sdgate, esdhc, card_image):
    """Old ladder path: one db.run() call driven by the global per-instruction
    `cover` hook (see emu.dspboot.prepare's docstring), which calls this
    `hook` as its extra_hook every instruction. See make()'s `coverage=True`.
    """
    todo = list(points)
    box = {"m": None, "saved": []}

    def hook(uc, addr, size, st):
        if todo and st["n"] >= todo[0]:
            at = todo.pop(0)
            path = "%s%dM.snap" % (prefix, at // 1_000_000)
            info = save(
                box["m"],
                path,
                extra={
                    "n": st["n"],
                    "seen": sorted(st["seen"]),
                    "tasks": {hex(k): v for k, v in st["task_create_hits"].items()},
                },
            )
            box["saved"].append(
                (
                    at,
                    path,
                    info,
                    len(st["seen"]),
                    len(st["task_create_hits"]),
                    uc.reg_read(UC_M68K_REG_PC),
                )
            )
            print(
                "  [%dM] %s  %d addrs, %d tasks, pc=0x%08x, %d B"
                % (
                    at // 1_000_000,
                    path,
                    len(st["seen"]),
                    len(st["task_create_hits"]),
                    uc.reg_read(UC_M68K_REG_PC),
                    info["bytes_on_disk"],
                ),
                flush=True,
            )

    db.run(
        syx,
        img,
        limit=points[-1] + 1_000_000,
        extra_hook=hook,
        fast=True,
        verbose=False,
        machine_out=box,
        sdgate=sdgate,
        esdhc=esdhc,
        card_image=card_image,
        coverage=True,
    )
    return box["saved"]


def _make_fast(syx, img, points, prefix, sdgate, esdhc, card_image):
    """New default ladder path: NO global per-instruction hook.

    `db.prepare()` builds exactly the Machine `db.run()` would for the same
    arguments -- every scoped (begin==end) hook stays installed: flash HLE,
    the completion-semaphore patch, the depack-length clamp, task_create/
    task_start tracking, and the idle-spin `do_halt` tick. The only thing
    skipped is the GLOBAL per-instruction `cover` hook, which is what made
    the old path ~1.94x slower (see docs/findings/07-emulator.md, "cold-boot
    cover-hook cost").

    Guest tick/timer delivery is unaffected by removing it: dspboot's only
    scheduler tick is `do_halt`, fired by a scoped hook bound to each
    idle-spin PC (`bra.b $self`) -- it counts hits and calls
    `m.raise_vector(tick_vec)` every `tick_every`-th one, entirely independent
    of how many `emu_start` calls the run is split into. There is no
    chunk-boundary tick injection here (unlike `emu.longrun.spin`'s opt-in
    `tick=True`, which this deliberately never passes) -- exactly dspboot's
    existing behaviour, coverage or not.

    Execution itself is driven the same way `emu/longrun.py`'s `extend()`
    drives a resumed run: one exact `uc.emu_start(pc, 0, count=delta)` per
    rung, so it stops precisely there rather than on some chunk grid, and no
    Python callback runs on any instruction in between. Splitting one long
    run into several exact-count `emu_start` calls on the same Machine
    changes nothing the guest can observe -- Unicorn resumes with the same
    registers, memory and hooks each time; this is exactly what a resumed
    `longrun` run already relies on being true.

    The old cover-hook path saves a rung's state INSIDE the hook call that
    first observes `st['n'] == target` -- i.e. BEFORE the target-th
    instruction executes, with only `target - 1` instructions actually
    retired (UC_HOOK_CODE fires before the instruction at its address runs).
    This path matches that exactly: it runs `target - 1` instructions
    (cumulative from boot) before saving, then continues from there. Like the
    old path, `extra['n']` still records the nominal `target`, not the true
    retired count.

    `st['seen']`/`st['stall_pcs']`/`st['curve']` stay empty in this mode
    (nothing populates them without the global hook), so a saved rung's
    `extra['seen']` is `[]` rather than the coverage set the old path
    recorded; `resume()`'s `st['seen']` carry-through still works, it just
    starts (and stays) empty. `task_create_hits` keeps working -- it is
    populated by a scoped hook, not the global one -- but its recorded `n`
    is only accurate to the rung it was reached within, not the exact
    instruction, since nothing counts instructions between rungs.

    A `UcError` (a guest fault) part-way through stops the ladder early and
    returns whatever rungs were already saved, the same graceful-degradation
    behaviour `_make_with_coverage` gets for free from `db.run()`'s own
    try/except.
    """
    m, st, pc = db.prepare(syx, img, fast=True, sdgate=sdgate, esdhc=esdhc,
                           card_image=card_image, coverage=False)
    saved = []
    pos = 0
    try:
        for target in points:
            want = target - 1     # see docstring: matches the old hook's timing
            delta = want - pos
            if delta < 0:
                raise ValueError(
                    "checkpoint points must be sorted and strictly increasing, "
                    "got %r" % (points,))
            if delta > 0:
                m.uc.emu_start(pc, 0, count=delta)
                pos += delta
                pc = m.uc.reg_read(UC_M68K_REG_PC)
            st["n"] = target
            path = "%s%dM.snap" % (prefix, target // 1_000_000)
            info = save(
                m,
                path,
                extra={
                    "n": target,
                    "seen": sorted(st["seen"]),
                    "tasks": {hex(k): v for k, v in st["task_create_hits"].items()},
                },
            )
            saved.append(
                (
                    target,
                    path,
                    info,
                    len(st["seen"]),
                    len(st["task_create_hits"]),
                    pc,
                )
            )
            print(
                "  [%dM] %s  %d addrs, %d tasks, pc=0x%08x, %d B"
                % (
                    target // 1_000_000,
                    path,
                    len(st["seen"]),
                    len(st["task_create_hits"]),
                    pc,
                    info["bytes_on_disk"],
                ),
                flush=True,
            )
        # Same trailing margin _make_with_coverage's `limit` runs past the
        # final rung, so a caller inspecting `m`/`st` afterwards sees a
        # machine that ran exactly as far as the old path's did.
        remaining = (points[-1] + 1_000_000) - pos
        if remaining > 0:
            m.uc.emu_start(pc, 0, count=remaining)
    except UcError:
        pass
    return saved


def make(points, prefix="snapshots/boot", syx=None, img_path=None,
         sdgate=True, esdhc=True, card_image=None, coverage=False):
    """Save a LADDER of checkpoints in one pass.

    `points` is a list of instruction counts. Saving mid-run is safe because
    save() only reads state; emulation continues afterwards. One slow pass
    yields several resume points, so later blocker work can start deep.

    `sdgate`/`esdhc` install the board loopback of emu/gpio.py and the
    controller of emu/esdhc.py for the cold boot itself. They belong here
    rather than only on the longrun resume path because the continuity check
    that decides whether storage comes up at all runs at instruction
    64,164,269 on Digitone -- before the first rung at 60M -- so a ladder
    built without them bakes "no storage" into every rung, and no amount of
    enabling them at resume time can undo that.

    Both default to True now: without them the firmware's SD bring-up never
    runs, the storage-ready flag stays 0, and every block-storage read
    returns -1. With them on, both builds reach MAIN_OS_RUNNING under
    tools/bootcheck.py --verify -- Digitone's display module initialises for
    the first time, and Digitakt's cold boot creates 9 tasks instead of 5,
    including the priority-6 Main OS task at entry 0x40032f5a. Digitakt
    reaches MAIN_OS_RUNNING both with and without them, so turning them on
    does not regress the previously-working build. Pass sdgate=False and/or
    esdhc=False to get the old unmodelled-storage behaviour back.

    `card_image`: path to a +Drive image built by tools/plusdrive.py, cold-
    booted with the card in place from PC=entry (esdhc's continuity check
    runs before the first rung at 60M, so a ladder built without the image
    from the start bakes "blank card" into every rung -- see emu.dspboot.run's
    own docstring). Only meaningful with esdhc=True. Its sha256 is recorded
    in the ladder's sidecar (see below) so a later run of this same prefix
    with a different (or no) image is caught as stale by
    emu.run.need_snapshot rather than silently mixed with rungs built from
    a different card.

    `coverage`: EXPERIMENTAL, opt-in. Default False: build the ladder via
    `_make_fast` -- no global per-instruction hook, chunked exact-count
    `emu_start` calls stopping precisely at each rung instead (see that
    function's docstring for why this is behaviourally identical to the old
    path and docs/findings/07-emulator.md for the measured 1.94x). Pass
    coverage=True for the old `_make_with_coverage` path, which additionally
    populates each rung's `seen`/`stall_pcs`/`curve` coverage stats at that
    cost. The ladder's `.ladder.json` sidecar records which mode built it
    (`"coverage"`); `emu.run.need_snapshot`'s own staleness check does not
    look at that key, so it does not rebuild a ladder merely because this
    flag's default changed.
    """
    syx = config.firmware(syx)
    image_path = config.main_image(img_path)
    try:
        with open(image_path, "rb") as image:
            img = image.read()
    except OSError as exc:
        raise RuntimeError("cannot read MAIN OS image %r" % image_path) from exc
    points = sorted(points)
    build = _make_with_coverage if coverage else _make_fast
    saved = build(syx, img, points, prefix, sdgate, esdhc, card_image)
    if saved:
        # A snapshot carries no manifest on the cold-boot path -- save() is
        # called above without one -- so this sidecar is what lets a later
        # run (emu/run.py's need_snapshot) tell a ladder built with these
        # storage models (and this card image) from one built without them,
        # instead of silently resuming a mismatched configuration.
        from emu.run import ladder_config_path
        cfg_path = ladder_config_path(prefix)
        card_sha256 = sha256_file(card_image) if card_image else None
        # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
        os.makedirs(os.path.dirname(cfg_path) or ".", exist_ok=True)
        # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
        with open(cfg_path, "w") as fh:
            json.dump({
                "protocol": 1,
                "sdgate": bool(sdgate),
                "esdhc": bool(esdhc),
                "points": points,
                "main_sha256": hashlib.sha256(img).hexdigest(),
                "card_image": os.path.basename(card_image) if card_image else None,
                "card_image_sha256": card_sha256,
                "coverage": bool(coverage),
            }, fh)
    return saved


def resume(path, extra_instrs, hook=None, chunk=500_000):
    """Restore and run forward. Returns (machine, new_addrs, stop_reason, n).

    Resuming has to happen onto an ALREADY-hooked Machine: restoring onto a
    bare one drops the flash HLE, the completion-semaphore patch and the
    scheduler tick, and the run then diverges while still looking plausible
    (docs/NEXT.md trap 4). longrun.build does the hooking, so go through it
    rather than snapshot.restore().
    """
    from unicorn import UC_HOOK_CODE

    m, ev, st, pc, inq, at = lr.build(path)
    carried = set(st["seen"])
    if hook:
        m.uc.hook_add(UC_HOOK_CODE, hook)
    pc, done, stop = lr.spin(m, pc, extra_instrs, chunk)
    st["n"] += done
    return m, st["seen"] - carried, stop, done, st, ev


def extend(path, points, prefix="snapshots/ext", chunk=500_000):
    """Resume `path` and save a ladder of further checkpoints.

    `points` are instruction counts measured FROM the resume point, so
    extend('snapshots/boot280M.snap', [200e6, 400e6]) writes checkpoints at an
    absolute 480M and 680M. Coverage (`seen`) is carried through unchanged --
    tracking new coverage needs a global per-instruction hook, which costs ~3x
    and is not worth paying just to keep a statistic warm.
    """
    m, ev, st, pc, inq, at = lr.build(path)
    base_n = st["n"]
    todo, saved = sorted(points), []

    def on_chunk(p, done):
        while todo and done >= todo[0]:
            todo.pop(0)
            out = "%s%dM.snap" % (prefix, (base_n + done) // 1_000_000)
            extra = {
                "n": base_n + done,
                "seen": sorted(st["seen"]),
                "tasks": {hex(k): v for k, v in st["task_create_hits"].items()},
                "seen_stale": True,
            }
            info = save(m, out, extra=extra)
            saved.append((out, base_n + done))
            print(
                "  [%dM] %s  pc=0x%08x  %d B  tasks_seen_since=%d"
                % (
                    (base_n + done) // 1_000_000,
                    out,
                    p,
                    info["bytes_on_disk"],
                    len(ev["tasks"]),
                ),
                flush=True,
            )

    pc, done, stop = lr.spin(m, pc, max(points), chunk, on_chunk=on_chunk)
    return saved, m, ev, stop


if __name__ == "__main__":
    import time

    # Pulled out before the positional parse so they can be passed in any
    # position without disturbing the existing `make POINTS [prefix] [syx]`
    # argument order that emu/run.py relies on.
    argv = [a for a in sys.argv
            if a not in ("--sdgate", "--esdhc", "--no-sdgate", "--no-esdhc",
                         "--coverage", "--no-coverage")]
    want_sdgate = "--no-sdgate" not in sys.argv
    want_esdhc = "--no-esdhc" not in sys.argv
    # See make()'s `coverage` docstring: default False (the fast, no-global-
    # hook ladder path) unless --coverage is passed.
    want_coverage = "--coverage" in sys.argv
    card_image = None
    if "--card-image" in argv:
        i = argv.index("--card-image")
        card_image = argv[i + 1]
        del argv[i:i + 2]

    cmd = argv[1]
    if cmd == "make":
        pts = _points(argv[2])
        prefix = argv[3] if len(argv) > 3 else "snapshots/boot"
        syx = argv[4] if len(argv) > 4 else None
        make(pts, prefix, syx, sdgate=want_sdgate, esdhc=want_esdhc,
             card_image=card_image, coverage=want_coverage)
    elif cmd == "extend":
        snap = argv[2]
        pts = _points(argv[3])
        prefix = argv[4] if len(argv) > 4 else "snapshots/ext"
        t0 = time.time()
        saved, m, ev, stop = extend(snap, pts, prefix)
        print(
            "extended %s by %dM in %.0fs, stop=%s"
            % (snap, max(pts) // 1_000_000, time.time() - t0, stop)
        )
        print("new tasks: %s" % ["0x%08x/p%d" % (e, p) for e, p, _ in ev["tasks"]])
        print("prints   : %r" % ev["prints"][:20])
    else:
        path = argv[2]
        extra = (
            _integer(argv[3], "instruction count")
            if len(argv) > 3
            else 2_000_000
        )
        t0 = time.time()
        m, fresh, stop, n, st, ev = resume(path, extra)
        print(
            "resumed: ran %d instrs in %.1fs, %d NEW addrs, stop=%s pc=0x%08x"
            % (n, time.time() - t0, len(fresh), stop, m.uc.reg_read(UC_M68K_REG_PC))
        )
