"""Record, report and dump peripheral traces (emu/mmiotrace.py) from the
Python emulator, the specification the Rust peripheral models replay against.

    # DT2: resume the audio-ready snapshot, force DSP frames, press TRIG 1
    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/mmio_record.py record \\
        snapshots/dt2-1.16-auto/<key>/ready.snap --out out/mmio/dt2-trig.mmio \\
        --card-image out/plusdrive/auto/<key>/dt2.img --audio \\
        --press "TRIG 1@2000000+1000000" --instrs 20000000

    # DN2: set the Digitone firmware and sections
    DT2_SYX=Digitone_II_OS1.11.syx DT2_SECTIONS=out/sections/dn2-1.11 \\
        uv run python tools/mmio_record.py record snapshots/dn2-1.11/boot400M.snap \\
        --out out/mmio/dn2-boot.mmio --slc --instrs 100000000

    uv run python tools/mmio_record.py report out/mmio/dt2-trig.mmio [--json]
    uv run python tools/mmio_record.py dump out/mmio/dt2-trig.mmio --limit 50

The run follows tools/guirun.py's configuration (unblock, softfloat and
bitmap HLEs, the DSP port, a checkpoint's own timers or guirun's intro
topology, the post-intro rate at intro handover) but steps exactly (no
`fast`), so two runs of the same command execute the same instruction
stream. `--save-final` writes the end state; run once with and once with
`--no-record` and compare with tools/snapeq.py to show recording changes
nothing.

Traces are firmware-derived (they hold register values and DMA payloads):
keep them under out/, never commit them.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu import config, mmiotrace, panelin, symbols  # noqa: E402

TIMER_KINDS = ("pit3", "held")


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(1 << 22):
            h.update(chunk)
    return h.hexdigest()


def parse_press(spec: str) -> tuple[str, int, int]:
    """'NAME@INSTR[+HOLD]' -> (name, at, hold). INSTR counts from the start of
    the window; HOLD 0 releases in the same delivery."""
    if "@" not in spec:
        raise argparse.ArgumentTypeError("want NAME@INSTR[+HOLD], got %r" % spec)
    name, when = spec.rsplit("@", 1)
    hold = 0
    if "+" in when:
        when, hold_s = when.split("+", 1)
        hold = int(hold_s, 0)
    return name, int(when, 0), hold


def button(m: Any, profile: Any, name: str) -> tuple[int, int]:
    """-> (channel, bit) of the panel button the running image calls NAME
    (tools/sharc_capture_run.py's button_channel_bit)."""
    names = panelin.control_names(m, profile, "button")
    for code, text in names.items():
        if text == name and 1 <= code <= 48:
            return divmod(code - 1, 8)
    raise SystemExit(
        "no panel button named %r (known: %s)" % (name, sorted(names.values()))
    )


def construct_timers(m: Any, intro: bool, intro_timers: str) -> Any:
    """tools/guirun.py's construct_timers at the default rate."""
    from emu.dtim import Dtims, Timers
    from emu.pit import Pits

    pit3_only = intro and intro_timers == "pit3"
    return Timers(
        Pits(
            m,
            channels=(3,) if pit3_only else (3, 2, 0),
            hold=intro and intro_timers == "held",
        ),
        Dtims(m, channels=(3,), hold=intro),
    )


def release_intro_timers(timers: Any) -> None:
    """tools/guirun.py's release_intro_timers."""
    pit_source = timers.sources[0]
    if tuple(pit_source.channels) == (3,):
        pit_source.channels = (3, 2, 0)
    timers.release()


def record(args: argparse.Namespace) -> int:
    from emu.checkpoint import save_longrun
    from emu.longrun import build, spin
    from emu.pit import DEVICE_INSTR_PER_SEC, INSTR_PER_SEC, intro_running

    if args.sections:
        os.environ["DT2_SECTIONS"] = args.sections
    syx = config.firmware(args.syx)
    main_path = config.main_image()
    with open(main_path, "rb") as fh:
        main_img = fh.read()
    profile = symbols.resolve(main_img)

    rec = None
    if not args.no_record:
        rec = mmiotrace.Recorder(
            args.out, icount=args.icount, state_every=args.state_every
        )
    kwargs: dict[str, Any] = dict(
        syx=syx,
        unblock=args.unblock,
        softfloat=True,
        bitmap=True,
        dsp=True,
        slc=args.slc,
        deferred_components=("timers",),
        mmio_recorder=rec,
    )
    if args.card_image:
        kwargs["card_image"] = args.card_image
    if args.audio:
        from emu.dspi2 import ZeroPeer

        kwargs["dspi2_peer"] = ZeroPeer()
    t0 = time.time()
    m, ev, _st, pc, _inq, at = build(args.snapshot, **kwargs)

    intro = intro_running(m, profile.intro_pit3_isr)
    # A checkpoint's own timers are restored before recording starts (the
    # restore writes no guest state); anything set up after that, fresh
    # timers (DTIM construction clears stale DTMRs), the frame forcer's gate
    # write, is host activity on guest state and is recorded.
    holder: dict[str, Any] = {"timers": ev["restore_checkpoint_timers"]()}
    restored = holder["timers"] is not None

    # Resolve buttons before recording: it only reads the image's name table.
    plan: list[tuple[int, str, bytes]] = []  # (at, what, wire bytes)
    for name, when, hold in args.press:
        channel, bit = button(m, profile, name)
        down = panelin.encode_buttons(channel, 1 << bit)
        up = panelin.encode_buttons(channel, 0)
        if hold:
            plan.append((when, name + " down", down))
            plan.append((when + hold, name + " up", up))
        else:
            plan.append((when, name + " down+up", down + up))
    plan.sort(key=lambda e: e[0])

    header: dict[str, Any] = (
        {}
        if rec is None
        else {
            "tool": "tools/mmio_record.py",
            "label": args.label,
            "argv": sys.argv[1:],
            "syx": {"path": os.path.basename(syx), "sha256": sha256_file(syx)},
            "main_image": {
                "path": main_path,
                "sha256": hashlib.sha256(main_img).hexdigest(),
                "profile": getattr(profile, "name", None),
            },
            "snapshot": {"path": args.snapshot, "sha256": sha256_file(args.snapshot)},
            "card_image": (
                {"path": args.card_image, "sha256": sha256_file(args.card_image)}
                if args.card_image
                else None
            ),
            "manifest": ev["checkpoint_manifest"],
            "build": {
                k: v
                for k, v in kwargs.items()
                if k not in ("mmio_recorder", "dspi2_peer", "syx")
            },
            "dspi2_peer": "ZeroPeer" if args.audio else None,
            "timers": {
                "restored": restored,
                "intro": intro,
                "intro_timers": args.intro_timers if intro and not restored else None,
                "post_intro_ips": args.post_intro_ips,
            },
            "rates": {
                "INSTR_PER_SEC": INSTR_PER_SEC,
                "DEVICE_INSTR_PER_SEC": DEVICE_INSTR_PER_SEC,
            },
            "audio": {"frame_period": args.frame_period} if args.audio else None,
            "presses": [[when, what, data.hex()] for when, what, data in plan],
            "instrs": args.instrs,
            "mmio_forced": {"%#010x" % a: v for a, v in m.mmio.items()},
            "slots": {"%#010x" % base: list(v) for base, v in mmiotrace.SLOTS.items()},
            "edma_channels": mmiotrace.EDMA_CHANNELS,
            "modelled": [[lo, hi, what] for lo, hi, what in mmiotrace.MODELLED],
        }
    )

    def state() -> dict:
        timers = holder["timers"]
        extra: dict[str, Any] = {"timers_now": timers.now if timers else None}
        if holder.get("forcer") is not None:
            f = holder["forcer"]
            extra["forcer"] = {"forced": f.forced, "deferred": f.deferred, "due": f.due}
        return mmiotrace.machine_state(m, ev, extra)

    def clock() -> int:
        timers = holder["timers"]
        return timers.now if timers is not None else 0

    def rate() -> int:
        timers = holder["timers"]
        return timers.sources[0].ips if timers is not None else INSTR_PER_SEC

    if rec is not None:
        rec.start(header, clock=clock, rate=rate, state=state)

    timers = holder["timers"]
    if timers is None:
        timers = construct_timers(m, intro, args.intro_timers)
        ev["checkpoint_components"]["timers"] = timers
        holder["timers"] = timers
    pending_ips: list[int] = []
    handovers: list[int] = []
    if not intro and timers.sources[0].ips != args.post_intro_ips:
        pending_ips.append(args.post_intro_ips)  # past the intro: guirun does this
    if intro and profile.intro_done is not None:

        def handover(uc: Any, a: int, s: int, d: Any) -> None:
            release_intro_timers(timers)
            handovers.append(timers.now)
            pending_ips.append(args.post_intro_ips)
            if rec is not None:
                rec.mark({"event": "intro handover", "clock": timers.now})

        at(profile.intro_done, handover)

    forcer = None
    if args.audio:
        from emu.livesharc import FrameForcer

        forcer = FrameForcer(m, timers, args.frame_period)
        holder["forcer"] = forcer
    setup = {
        "event": "setup",
        "timers": [
            {"type": type(src).__name__, "channels": list(src.channels), "ips": src.ips}
            for src in timers.sources
        ],
        "start_clock": timers.now,
        "forcer": {
            "vector": forcer.vector,
            "level": forcer.level,
            "period": forcer.period,
        }
        if forcer is not None
        else None,
    }
    if rec is not None:
        rec.mark(setup)
    print(
        "[mmio] %s from %s: clock %d, ips %d, intro %s, timers %s"
        % (
            "recording" if rec else "running (no recording)",
            args.snapshot,
            timers.now,
            timers.sources[0].ips,
            intro,
            "restored" if restored else "constructed",
        ),
        flush=True,
    )

    step = {"i": 0}

    def on_chunk(pc_: int, done: int) -> None:
        while pending_ips:
            ips = pending_ips.pop(0)
            if timers.sources[0].ips != ips:
                timers.rescale(ips)
        while step["i"] < len(plan) and done >= plan[step["i"]][0]:
            when, what, data = plan[step["i"]]
            step["i"] += 1
            if rec is not None:
                rec.mark(
                    {
                        "event": "panel",
                        "what": what,
                        "at": when,
                        "done": done,
                        "bytes": data.hex(),
                    }
                )
            panelin.feed(m, profile, data)
        if forcer is not None:
            forcer.on_chunk(pc_, done)

    # The recorder-only sampler runs before PIT and all other async services
    # at each exact boundary. It only reads SR, so the no-record oracle path
    # remains byte-for-byte the existing configuration.
    sampler = (rec.boundary_sampler(),) if rec is not None else ()
    pc, done, stop = spin(
        m, pc, args.instrs, pits=timers, async_events=sampler, on_chunk=on_chunk
    )
    wall = time.time() - t0
    info: dict[str, Any] = {
        "done": done,
        "stop": stop,
        "end_clock": timers.now,
        "wall_s": round(wall, 1),
        "faults": m.fault_report(),
        "handovers": handovers,
        "presses_delivered": step["i"],
        "tasks": [list(t) for t in ev["tasks"]],
        "satisfied": ev["satisfied"],
    }
    if forcer is not None:
        info["forcer"] = {"forced": forcer.forced, "deferred": forcer.deferred}
    esdhc = ev.get("esdhc")
    if esdhc is not None:
        info["esdhc_dma_bytes"] = esdhc.dma_bytes
    if rec is not None:
        summary = rec.stop(info)
        print(
            "[mmio] %s: %d bytes on disk, %d raw, errors %d"
            % (args.out, os.path.getsize(args.out), rec.bytes_raw, summary["errors"])
        )
    print(
        "[mmio] done %d instrs in %.1fs (%.2fM/s), stop=%s, end clock %d, faults %d pages"
        % (
            done,
            wall,
            done / max(wall, 1e-9) / 1e6,
            stop,
            timers.now,
            len(info["faults"]),
        ),
        flush=True,
    )
    if args.save_final:
        save_longrun(m, ev, timers, args.save_final, extra={"mmio_record_done": done})
        print("[mmio] saved %s" % args.save_final)
    m.close()
    return 0 if stop in ("limit",) else 1


def report(args: argparse.Namespace) -> int:
    s = mmiotrace.summarize(args.trace)
    if args.json:
        json.dump(s, sys.stdout, indent=1, sort_keys=True, default=str)
        print()
        return 0
    h = s["header"]
    size = os.path.getsize(args.trace)
    print(
        "trace %s: %d bytes, label %r, clock %s..%s (%s)"
        % (
            args.trace,
            size,
            h.get("label"),
            s["clock"][0],
            s["clock"][1],
            h["clock_resolution"],
        )
    )
    print(
        "records: "
        + ", ".join(
            "%s %d (%d B)" % (k, v, s["tag_bytes"][k])
            for k, v in sorted(s["tags"].items())
        )
    )
    print(
        "\nguest accesses by peripheral (reads, writes, raw trace bytes, lane, unmodelled regs):"
    )
    rows = sorted(
        s["peripherals"].items(),
        key=lambda kv: -(kv[1].get("reads", 0) + kv[1].get("writes", 0)),
    )
    for key, v in rows:
        name, lane = key.split("|")
        un = s["unmodelled"].get(key, {})
        print(
            "  %-34s %-16s R %9d  W %9d  %10d B  unmodelled %d"
            % (
                name,
                lane,
                v.get("reads", 0),
                v.get("writes", 0),
                v.get("bytes", 0),
                len(un),
            )
        )
    print("\nhost operations by source (records, payload bytes):")
    for key, v in sorted(s["host"].items(), key=lambda kv: -kv[1]["n"]):
        regions = s["host_regions"].get(key, {})
        where = ", ".join(
            "%s %d" % r for r in sorted(regions.items(), key=lambda r: -r[1])[:3]
        )
        print("  %-62s %8d %10d  %s" % (key, v["n"], v["bytes"], where))
    print("\ninterrupts (vector source: count, untaken):")
    for key, n in sorted(s["irqs"].items(), key=lambda kv: -kv[1]):
        print("  %-60s %7d %s" % (key, n, s["irq_untaken"].get(key, "")))
    if args.registers:
        print("\nregisters:")
        for key, regs in sorted(s["registers"].items()):
            print("  " + key.split("|")[0])
            for reg, n in list(regs.items())[: args.registers]:
                print("      %-24s %d" % (reg, n))
    end = s["end"] or {}
    print("\nunmodelled registers touched (plain RAM in the Python emulator):")
    from emu import hwref

    for key, regs in sorted(s["unmodelled"].items()):
        print("  " + key.split("|")[0])
        for reg, n in regs.items():
            name = hwref.resolve_address(int(reg.split()[0], 16)).get("name") or ""
            print("      %-18s %-22s %d" % (reg, name, n))
    print("\ncount-only host sources: %s" % end.get("count_only_sources"))
    print("recorder errors: %s %s" % (end.get("errors"), end.get("first_error") or ""))
    print("faults (new pages): %s" % end.get("faults"))
    return 0


def dump(args: argparse.Namespace) -> int:
    rd = mmiotrace.Reader(args.trace)
    tags = {t.upper() for t in args.tag} if args.tag else None
    n = 0
    for rec in rd:
        if tags and rec.name not in tags:
            continue
        if rec.tag in (mmiotrace.RD, mmiotrace.WR):
            addr, value, pc, size = rec.fields
            text = "%#010x %s%d = %#x pc=%#010x %s" % (
                addr,
                "R" if rec.tag == mmiotrace.RD else "W",
                size * 8,
                value,
                pc,
                mmiotrace.peripheral_of(addr)[0],
            )
        elif rec.tag in (mmiotrace.HWR, mmiotrace.HRD):
            sid, addr, ln = rec.fields
            text = "%s %#010x len %d %s%s" % (
                rd.sources.get(sid, sid),
                addr,
                ln,
                rec.data[:16].hex(),
                "..." if ln > 16 else "",
            )
        elif rec.tag in (mmiotrace.STATE, mmiotrace.MARK, mmiotrace.END, mmiotrace.SRC):
            text = rec.data[:200].decode(errors="replace")
        elif rec.tag == mmiotrace.PAGE:
            text = "%#010x len %d" % rec.fields[:2]
        elif rec.tag == mmiotrace.IRQ:
            vec, lvl, flags, sid, pc0, pc1, sr = rec.fields
            text = "vec %d level %s %s%s from %s pc %#010x -> %#010x sr %#06x" % (
                vec,
                lvl,
                "taken" if flags & 1 else "NOT taken",
                " sync" if flags & 2 else "",
                rd.sources.get(sid, sid),
                pc0,
                pc1,
                sr,
            )
        elif rec.tag == mmiotrace.HREG:
            sid, reg, value = rec.fields
            text = "%s reg %d = %#x" % (rd.sources.get(sid, sid), reg, value)
        else:
            text = " ".join(hex(f) for f in rec.fields)
        print("%12d %-5s %s" % (rec.clock, rec.name, text))
        n += 1
        if args.limit and n >= args.limit:
            break
    return 0


def main(argv: list[str] | None = None) -> int:
    from emu.livesharc import FRAME_PERIOD
    from emu.pit import DEVICE_INSTR_PER_SEC

    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("record", help="run a bounded window and record it")
    r.add_argument("snapshot")
    r.add_argument("--out", help="trace path (required unless --no-record)")
    r.add_argument(
        "--instrs",
        type=lambda s: int(s, 0),
        required=True,
        help="window length in guest instructions (a floor: the last timer step completes)",
    )
    r.add_argument("--syx", default=None)
    r.add_argument(
        "--sections",
        default=None,
        help="sets DT2_SECTIONS (e.g. out/sections/dn2-1.11)",
    )
    r.add_argument("--card-image", default=None)
    r.add_argument("--slc", action="store_true")
    r.add_argument("--unblock", action=argparse.BooleanOptionalAction, default=True)
    r.add_argument(
        "--audio",
        action="store_true",
        help="DSPI2 link (ZeroPeer) plus forced vector-191 frames, as emu/gui.py --live-audio (DT2 1.16)",
    )
    r.add_argument("--frame-period", type=lambda s: int(s, 0), default=FRAME_PERIOD)
    r.add_argument(
        "--press",
        action="append",
        default=[],
        type=parse_press,
        help="NAME@INSTR[+HOLD]: panel button by its firmware name, window-relative",
    )
    r.add_argument("--intro-timers", choices=TIMER_KINDS, default="pit3")
    r.add_argument(
        "--post-intro-ips", type=lambda s: int(s, 0), default=DEVICE_INSTR_PER_SEC
    )
    r.add_argument(
        "--icount", action="store_true", help="exact per-instruction clock (slow)"
    )
    r.add_argument("--state-every", type=lambda s: int(s, 0), default=10_000_000)
    r.add_argument(
        "--no-record",
        action="store_true",
        help="same run, no recorder (the snapeq control)",
    )
    r.add_argument(
        "--save-final", default=None, help="save the end state (for tools/snapeq.py)"
    )
    r.add_argument("--label", default=None)
    rp = sub.add_parser("report", help="per-peripheral traffic of a trace")
    rp.add_argument("trace")
    rp.add_argument("--json", action="store_true")
    rp.add_argument(
        "--registers",
        type=int,
        default=0,
        help="list the top N registers per peripheral",
    )
    d = sub.add_parser("dump", help="print records")
    d.add_argument("trace")
    d.add_argument("--limit", type=int, default=100)
    d.add_argument("--tag", action="append", default=[])
    args = p.parse_args(argv)
    if args.cmd == "record":
        if not args.no_record and not args.out:
            p.error("record needs --out (or --no-record)")
        return record(args)
    if args.cmd == "report":
        return report(args)
    return dump(args)


if __name__ == "__main__":
    sys.exit(main())
