#!/usr/bin/env python3
"""How close the ColdFire emulator runs to real time in the audio state.

    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/cfrealtime.py \\
        [--snapshot snapshots/dt2-1.16-drive3/loaded.snap] [--ips N] \\
        [--guest-s 0.3] [--warmup-s 0.1] [--coalesce] [--fast] \\
        [--no-idle-skip] [--no-audio] [--save PATH] [--vectors PATH] \\
        [--json PATH]

Resumes a snapshot with the UI up and runs the audio clock the way
docs/findings/04's Lane J2 does: the SSI0/eDMA48/50 request model at
96 kHz (`emu.ssi`), the RX hand-over marker peer, and the snapshot's own
timers. It then runs `--guest-s` seconds of device time after a
`--warmup-s` warm-up, and prints one JSON line:

  * `rt_factor`: guest seconds per wall second. 1.0 is real time.
  * `executed_per_wall_s`: instructions Unicorn executed per wall second.
    With the idle skip on, the idle loop's own instructions are credited
    to the clock without being executed (`idle_skipped`), so this can be
    lower than `guest_instr_per_wall_s`.
  * `v191_per_gs` and `v191_per_wall_s`: vector-191 deliveries, one per
    audio block and one SPI2 frame each. The device makes 1,500 a second.
  * `timers`: per channel, ticks due, taken (`fired`), lost to a tick
    already pending (`missed`), cleared by the guest before delivery
    (`cleared`) and pending at the end.
  * `ipl_ge5`: the share of `emu_start` boundaries at which the CPU was at
    IPL 5 or above, that is, inside the audio interrupts.

Keep the warm-up at 0.1 s or more when reading `v191_per_gs`. After a
resume, vector 170 runs the generic handler `0x400d2f98`, which does not
force vector 191, until it has seen the RX marker in 64 major loops and
hands the vector over to `0x4002d322` (emu.ssi.RxHandoverPeer): 65 blocks,
43 ms. A window that includes them reads below 1,500.

`--ips` sets the time base of the timers and the SSI0 clock (default
`emu.pit.DEVICE_INSTR_PER_SEC`). Timers a snapshot saved at another rate
are rescaled to it. `--no-audio` leaves the SSI0 model out, as the GUI
does.

`--save PATH` writes the end state (`emu.checkpoint.save_longrun`) and
`--vectors PATH` writes every interrupt and trap the host raised, as
`[boundary, vector, pc, sp]` rows. Runs with and without `--no-idle-skip`
must give identical files: that is the idle skip's equivalence check
(`tools/snapeq.py A B`, then `cmp` on the vector logs).

Timing numbers are only as good as the machine is quiet: alternate A/B
runs and compare medians.
"""

import argparse
import collections
import json
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from unicorn import UC_HOOK_CODE  # noqa: E402
from unicorn.m68k_const import (  # noqa: E402
    UC_M68K_REG_A7,
    UC_M68K_REG_PC,
    UC_M68K_REG_SR,
)

from emu import config, symbols  # noqa: E402
from emu.checkpoint import save_longrun  # noqa: E402
from emu.dtim import build_timers  # noqa: E402
from emu.longrun import build, spin  # noqa: E402
from emu.pit import DEVICE_INSTR_PER_SEC, Pits  # noqa: E402
from emu.ssi import AUDIO_SSI0_REQUEST_HZ, RxHandoverPeer  # noqa: E402

SNAPSHOT = "snapshots/dt2-1.16-drive3/loaded.snap"
CARD_IMAGE = "out/plusdrive/native/dt2.img"


def parse_args(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--snapshot", default=SNAPSHOT)
    p.add_argument("--card-image", default=CARD_IMAGE)
    p.add_argument("--syx", default=None)
    p.add_argument("--ips", type=int, default=DEVICE_INSTR_PER_SEC)
    p.add_argument("--guest-s", type=float, default=0.3)
    p.add_argument("--warmup-s", type=float, default=0.1)
    p.add_argument("--coalesce", action="store_true")
    p.add_argument("--fast", action="store_true")
    p.add_argument("--no-idle-skip", action="store_true")
    p.add_argument("--no-audio", action="store_true")
    p.add_argument("--save")
    p.add_argument("--vectors")
    p.add_argument("--json")
    return p.parse_args(argv)


def setup(args):
    """-> (m, ev, pc, timers, ssi0) resumed from `args.snapshot`; `ssi0` is
    None with `--no-audio`."""
    audio = not args.no_audio
    m, ev, _st, pc, _inq, _at = build(
        args.snapshot,
        syx=args.syx,
        unblock=True,
        softfloat=True,
        bitmap=True,
        dsp=True,
        card_image=args.card_image,
        deferred_components=("timers",),
        ssi0_request_hz=AUDIO_SSI0_REQUEST_HZ if audio else None,
        ssi0_legacy_upgrade=audio,
    )
    timers = ev["restore_checkpoint_timers"]()
    if timers is None:
        timers = build_timers(m, instr_per_sec=args.ips)
    rescale = getattr(timers, "rescale", None)
    if rescale is not None:
        rescale(args.ips)
    else:
        for source in timers.sources:
            source.ips = args.ips
    ssi0 = ev.get("ssi0_dma")
    if ssi0 is not None:
        ssi0.ips = args.ips
        ssi0.align(timers.now)
        ssi0.peer = RxHandoverPeer(m)
        ssi0.coalesce = args.coalesce
    idle = ev.get("idle")
    if idle is not None:
        idle.skip_enabled = not args.no_idle_skip
    return m, ev, pc, timers, ssi0


def timer_counts(timers):
    out = {}
    for source in timers.sources:
        prefix = "PIT" if isinstance(source, Pits) else "DTIM"
        pending = getattr(source, "pending", set())
        cleared = getattr(source, "cleared", collections.Counter())
        for ch in source.channels:
            out["%s%d" % (prefix, ch)] = {
                "fired": source.fired[ch],
                "missed": source.missed[ch],
                "cleared": cleared[ch],
                "pending": int(ch in pending),
            }
    return out


def timer_delta(before, after):
    out = {}
    for name, now in after.items():
        then = before[name]
        row = {k: now[k] - then[k] for k in ("fired", "missed", "cleared")}
        row["pending"] = now["pending"]
        row["due"] = row["fired"] + row["missed"] + row["cleared"]
        row["due"] += now["pending"] - then["pending"]
        if row["due"] or row["pending"]:
            out[name] = row
    return out


def audio_counts(ssi0):
    if ssi0 is None:
        return {"v191": 0, "v170": 0, "req": 0}
    return {"v191": ssi0.vector191, "v170": ssi0.vector170, "req": ssi0.requests}


def main(argv=None):
    args = parse_args(argv)
    m, ev, pc, timers, ssi0 = setup(args)
    idle = ev.get("idle")

    # IPL at each boundary, read before anything is delivered there.
    ipl = collections.Counter()
    first = ssi0 if ssi0 is not None else timers
    service = first.service

    def sampled_service(done):
        ipl[(m.uc.reg_read(UC_M68K_REG_SR) >> 8) & 7] += 1
        return service(done)

    first.service = sampled_service

    starts = [0]
    emu_start = m.uc.emu_start

    def counted_start(*a, **kw):
        starts[0] += 1
        return emu_start(*a, **kw)

    m.uc.emu_start = counted_start

    vectors = []
    if args.vectors:
        raise_vector = m.raise_vector

        def logged_raise(vec, *a, **kw):
            taken = raise_vector(vec, *a, **kw)
            if taken:
                vectors.append(
                    [
                        timers.now,
                        vec,
                        m.uc.reg_read(UC_M68K_REG_PC),
                        m.uc.reg_read(UC_M68K_REG_A7),
                    ]
                )
            return taken

        m.raise_vector = logged_raise

    with open(config.main_image(), "rb") as f:
        profile = symbols.resolve(f.read())
    mainloop = [0]
    if profile.mainloop is not None:

        def on_mainloop(uc, a, s, d):
            mainloop[0] += 1

        m.uc.hook_add(
            UC_HOOK_CODE,
            on_mainloop,
            begin=profile.mainloop,
            end=profile.mainloop,
        )

    events = (ssi0,) if ssi0 is not None else ()
    spin_kw = dict(pits=timers, fast=args.fast, async_events=events)
    if args.warmup_s > 0:
        pc, _done, stop = spin(m, pc, int(args.warmup_s * args.ips), **spin_kw)
        if stop != "limit":
            raise SystemExit("warm-up stopped: %s" % stop)

    t0_timers = timer_counts(timers)
    t0 = dict(
        audio_counts(ssi0),
        spins=ev["idle_spins"]["n"],
        skipped=getattr(idle, "skipped", 0),
        starts=starts[0],
        mainloop=mainloop[0],
    )
    ipl.clear()
    vectors.clear()
    n = int(args.guest_s * args.ips)
    wall0 = time.perf_counter()
    pc, done, stop = spin(m, pc, n, **spin_kw)
    wall = time.perf_counter() - wall0
    gs = done / args.ips
    skipped = getattr(idle, "skipped", 0) - t0["skipped"]
    boundaries = sum(ipl.values())
    t1 = audio_counts(ssi0)
    out = {
        "snapshot": args.snapshot,
        "ips": args.ips,
        "audio": ssi0 is not None,
        "coalesce": args.coalesce,
        "fast": args.fast,
        "idle_skip": idle is not None and not args.no_idle_skip,
        "guest_instr": done,
        "stop": stop,
        "pc": "%#x" % pc,
        "wall_s": round(wall, 3),
        "guest_s": round(gs, 5),
        "rt_factor": round(gs / wall, 5),
        "guest_instr_per_wall_s": round(done / wall),
        "executed_per_wall_s": round((done - skipped) / wall),
        "idle_skipped": skipped,
        "idle_spins": ev["idle_spins"]["n"] - t0["spins"],
        "v191_per_gs": round((t1["v191"] - t0["v191"]) / gs, 1),
        "v191_per_wall_s": round((t1["v191"] - t0["v191"]) / wall, 2),
        "v170_per_gs": round((t1["v170"] - t0["v170"]) / gs, 1),
        "ssi_requests_per_gs": round((t1["req"] - t0["req"]) / gs, 1),
        "mainloop_per_gs": round((mainloop[0] - t0["mainloop"]) / gs, 1),
        "emu_starts": starts[0] - t0["starts"],
        "boundaries": boundaries,
        "ipl_ge5": round(
            sum(v for k, v in ipl.items() if k >= 5) / max(1, boundaries), 4
        ),
        "timers": timer_delta(t0_timers, timer_counts(timers)),
        "tx_crc32": ssi0.tx_crc32 if ssi0 is not None else None,
    }
    if args.save:
        save_longrun(m, ev, timers, args.save, {"guest_instr": done})
    if args.vectors:
        with open(args.vectors, "w") as f:
            json.dump(vectors, f)
    m.close()
    line = json.dumps(out)
    print(line)
    if args.json:
        with open(args.json, "a") as f:
            f.write(line + "\n")
    return out


if __name__ == "__main__":
    main()
