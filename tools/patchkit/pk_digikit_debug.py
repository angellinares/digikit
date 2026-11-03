#!/usr/bin/env python3
"""The `debug` and `crash` scenarios of the digikit runtime (docs/findings/46-digikit-debug.md). Run by `tools/patchkit/pk_digikit_scen.py`.

debug the debug log: LOG and PEEK over SysEx, INFO's sequence numbers, the DEBUG switch (errors only when off, saved in the record's flags
       and kept by the selection), the periodic events, `tools/device/dklog.py` against the emulator.
memaudit  the ColdFire memory audit (pk_digikit_memaudit.py): the write guard armed on the free regions outside the modules' blocks while the UI is driven.
crash  a CPU exception in a module's slot: the runtime's hook records it in RAM that the soft reset keeps and resets; the next boot reports
       the record with the right PC; two in a row lead to safe mode, for a crash before the stable period and for one after it.
"""

from __future__ import annotations

import contextlib
import io
import json
import struct
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools.device import dklog as L
from tools.device import dkupload as DK
from tools.device.dkproto import SERVICE_CMD, SETTINGS_SECTORS
from tools.patchkit import pk_device
from tools.patchkit import pk_digikit_upload as U
from tools.patchkit import pk_dkrt_boot as B
from tools.patchkit import pk_dkrt_machine as M
from tools.patchkit import pk_harness as H

E_PEEK = -63


def uploader(rig: Any) -> DK.Uploader:
    return DK.Uploader(U.transport(rig))


def small_ring(rig: Any) -> list[tuple[int, int, int, int, tuple[int, ...]]]:
    """The runtime's own ring of boot and error events (`dlog.rs`, 32 events), read from guest memory: what exists when the debug module is not loaded."""
    base = M.sym(rig.plan, "core/runtime", "DKRT_LOG")
    count = rig.w(M.sym(rig.plan, "core/runtime", "DKRT_LOG_COUNT"))
    out = []
    for seq in range(max(1, count - 31), count + 1):
        at = base + ((seq - 1) % 32) * 32
        w = [rig.w(at + 4 * k) for k in range(8)]
        if w[0] == seq:
            out.append((seq, w[1], w[2] >> 16, w[2] & 0xFFFF, tuple(w[3:8])))
    return out


def events(
    rig: Any, kind: str | None = None
) -> list[tuple[int, int, int, int, tuple[int, ...]]]:
    """Every event the device's log holds: over SysEx (the way dklog reads it) while the debug module is loaded, else the runtime's own small ring from memory."""
    up = uploader(rig)
    evs = L.read_all(up)[2] if DK.has_cap(up.info(), "LOG") else small_ring(rig)
    return [e for e in evs if kind is None or e[2] == L.K[kind]]


def about_rows(rig: Any, plan: Any) -> list[str]:
    """The rows of the ABOUT page as the menu last built them (`DK_ABOUT`)."""
    base, n = plan.addr("DK_ABOUT"), rig.w(plan.addr("DK_ABOUT_N"))
    rows = []
    for i in range(n):
        raw = b"".join(
            rig.w(base + 24 * i + 4 * k).to_bytes(4, "big") for k in range(6)
        )
        rows.append(raw.split(b"\0")[0].decode("latin-1"))
    return rows


# ---- debug ------------------------------------------------------------------------------------------------------------------


def run_debug(c: Any) -> None:
    if c.table().get("part") == "guard":
        return run_guard(c)
    if c.table().get("part") == "capacity":
        from tools.patchkit import pk_digikit_capacity as CAP

        return CAP.run_capacity(c)
    if c.table().get("part") == "inferred-needs":
        from tools.patchkit import pk_digikit_capacity as CAP

        return CAP.run_inferred_needs(c)
    if c.table().get("part") == "menu-open":
        from tools.patchkit import pk_digikit_capacity as CAP

        return CAP.run_menu_open(c)
    if c.table().get("part") == "fits":
        from tools.patchkit import pk_digikit_capacity as CAP

        return CAP.run_fits(c)
    if c.table().get("part") == "isolation":
        from tools.patchkit import pk_digikit_capacity as CAP

        return CAP.run_isolation(c)
    if c.table().get("part") == "memaudit":
        from tools.patchkit import pk_digikit_memaudit as MA

        return MA.run_memaudit(c)
    img = c.image_for(B.RT, "run")
    # the log (LOG, PEEK, LOGCFG) is the debug module's: a blank card with the module selected, and one without it
    plain = c.card(None)
    dbg_mod = c.pack("digikit/debug", "debug.dkm")
    blank = c.card([dbg_mod])
    has_diag = any(k.endswith(":dl_diag") for k in img.plan["symbols"])
    with c.rig(img, plain, "no-debug", cf_only=True) as rig:
        up = uploader(rig)
        info = up.info()
        ring = small_ring(rig)
        st = [
            up.request(cmd, bytes(6))[0]
            for cmd in (
                SERVICE_CMD["LOG"],
                SERVICE_CMD["PEEK"],
                SERVICE_CMD["LOGCFG"],
            )
        ]
        c.check(
            "without the debug module INFO has no LOG, PEEK or LOGCFG capability, the three commands are E_PROTO, and the debug flag is off",
            not any(DK.has_cap(info, n) for n in ("LOG", "PEEK", "LOGCFG"))
            and DK.has_cap(info, "SERVICE_CMDS")
            and st == [-60] * 3
            and not info["debug_on"],
            f"{info['caps']:#x} {st}",
        )
        kinds = [e[2] for e in ring]
        c.check(
            "without it the runtime's small ring (32 events) holds the boot's events and no periodic or module events",
            0 < len(ring) <= 32
            and L.K["BOOT_START"] in kinds
            and L.K["UI_FIRST"] in kinds
            and not {L.K["HEALTH"], L.K["DSP_DIAG"], L.K["MOD_LOG"]} & set(kinds),
            str(sorted(kinds)),
        )
        c.done(rig)
    with c.rig(img, blank, "debug", cf_only=True) as rig:
        up = uploader(rig)
        evs = events(rig)
        oldest, newest = evs[0][0], evs[-1][0]
        by_kind = {e[2] for e in evs}
        c.check(
            "LOG returns the boot's events in sequence, from 1, with the block counter as time (not going back)",
            [e[0] for e in evs] == list(range(oldest, newest + 1))
            and oldest == 1
            and all(a[1] <= b[1] for a, b in zip(evs, evs[1:], strict=False)),
            f"{len(evs)} events, kinds {sorted(by_kind)}",
        )
        # This boot runs without the DSP: with dl_diag present the first DSP_DIAG needs a running DSP (the coupled part below checks it);
        # without it, DSP_ABSENT is logged at the first poll.
        want = {"BOOT_START", "CARD_CHECK", "RAM_KEEP", "BOOT_RECORD", "UI_FIRST"} | (
            set() if has_diag else {"DSP_ABSENT"}
        )
        c.check(
            "the boot steps are logged (handed over from the runtime's ring): boot start, card check, RAM probe, record, first UI pass, and DSP_ABSENT when the link is absent",
            all(L.K[k] in by_kind for k in want),
            str(sorted(L.KINDS[k][0] for k in by_kind)),
        )
        start = next(e for e in evs if e[2] == L.K["BOOT_START"])
        c.check(
            "BOOT_START carries the runtime version and the device id",
            start[4][0] == B.RUNTIME_VERSION and start[4][1] in (1, 2),
            str(start),
        )
        info = up.info()
        c.check(
            "INFO reports the LOG, PEEK and LOGCFG capabilities and the debug flag while the module is loaded, no crash",
            all(DK.has_cap(info, n) for n in ("LOG", "PEEK", "LOGCFG"))
            and info["debug_on"]
            and not info["crash_found"]
            and not info["card_read_only"],
            str(info),
        )
        # PEEK
        state_sym = M.sym(rig.plan, "core/runtime", "DKRT_STATE")
        got = up.peek(state_sym, 8)
        c.check(
            "PEEK reads the runtime's own region (DKRT_STATE: magic 'DKRT', runtime version)",
            got[:4] == b"DKRT" and int.from_bytes(got[4:8], "big") == B.RUNTIME_VERSION,
            got.hex(),
        )
        big = L.peek_range(up, state_sym, 1300)
        c.check(
            "PEEK of 1300 bytes in three requests (512 at most each)",
            len(big) == 1300 and big[:4] == b"DKRT",
            f"{len(big)}",
        )
        refused = {}
        rs = pk_device.peek_ranges(c.device)
        (lo0, hi0), (_lo1, hi1) = rs[0], rs[-1]
        fw = up.peek(lo0 + 0x400, 16)
        c.check("PEEK reads the firmware's data range", len(fw) == 16, fw.hex())
        for name, (addr, n) in {
            "a peripheral register (the reset controller)": (0xEC090000, 4),
            "just below the first range": (lo0 - 4, 8),
            "a range that runs past the end of the last": (hi1 - 4, 8),
            "the gap between the ranges": (hi0, 16),
            "address wrap": (0xFFFFFFF0, 32),
        }.items():
            st, _ = up.request(SERVICE_CMD["PEEK"], struct.pack(">IH", addr, n))
            refused[name] = st
        st0, _ = up.request(SERVICE_CMD["PEEK"], struct.pack(">IH", state_sym, 0))
        st513, _ = up.request(SERVICE_CMD["PEEK"], struct.pack(">IH", state_sym, 513))
        c.check(
            "PEEK refuses everything outside the ranges of the device file, and lengths 0 and over 512 (E_PEEK)",
            all(v == E_PEEK for v in refused.values())
            and st0 == E_PEEK
            and st513 == E_PEEK,
            f"{refused} {st0} {st513}",
        )
        # LOGCFG: levels (finding 46; docs/design/module-sdk.md 13.2)
        cfg = up.logcfg()
        c.check(
            "LOGCFG get: default level info (2), no module level set, debug on",
            cfg == {"default": 2, "modules": [0xFF] * 8, "debug": True},
            str(cfg),
        )
        m0 = len(events(rig, "UPLOAD"))
        up.logcfg(default=0)
        up.request(DK.CMD_ABORT)  # an info event: dropped at level error
        mid = len(events(rig, "UPLOAD"))
        up.request(DK.CMD_DELETE, b"..\0")  # an error event: kept
        m1 = len(events(rig, "UPLOAD"))
        c.check(
            "LOGCFG default error (0): an info event is dropped, an error event is kept",
            mid == m0 and m1 == mid + 1,
            f"{m0} {mid} {m1}",
        )
        back = up.logcfg(default=2)
        up.request(DK.CMD_ABORT)
        c.check(
            "LOGCFG default back to info: the upload command is logged again",
            back["default"] == 2 and len(events(rig, "UPLOAD")) == m1 + 1,
            str(back),
        )
        # the old debug switch is gone: LOGCFG op 2 is E_PROTO, and the selection record keeps no debug flag
        st_op2, _ = up.request(SERVICE_CMD["LOGCFG"], bytes([2, 1]))
        r_sel = rig.set_selection([dbg_mod])
        rec = B.current(rig.sel_records())
        c.check(
            "LOGCFG op 2 is refused (E_PROTO); a saved selection has the flag bit 1 clear",
            st_op2 == -60 and r_sel == 0 and rec is not None and not rec["flags"] & 2,
            f"{st_op2} {r_sel} {rec}",
        )
        # dklog against the emulator
        out = c.work / "dklog.log"
        n = L.follow(up, out, once=True, echo=False)
        text = out.read_text().splitlines()
        js = [json.loads(x) for x in text if not x.startswith("#")]
        c.check(
            "dklog follow --once writes every event as a readable line and a JSON line",
            n == len(js)
            and n >= 8
            and [j["seq"] for j in js] == sorted(j["seq"] for j in js)
            and any(x.startswith("#") and "CARD_CHECK" in x for x in text),
            f"{n} events, {len(text)} lines",
        )
        c.check(
            "dklog crash: no crash record in this log",
            L.crash_record(L.read_all(up)[2]) is None,
        )
        # the command line, as Em runs it: a second connection to the daemon (`--run-dir`)
        run_dir = rig.s.run_dir
        assert run_dir is not None
        cli = c.work / "dklog-cli.log"
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc_follow = L.main(
                [
                    "--emu",
                    "--run-dir",
                    str(run_dir),
                    "follow",
                    "--once",
                    "--out",
                    str(cli),
                ]
            )
            rc_peek = L.main(
                ["--emu", "--run-dir", str(run_dir), "peek", hex(state_sym), "16"]
            )
            rc_crash = L.main(["--emu", "--run-dir", str(run_dir), "crash"])
        c.check(
            "dklog on the command line (follow --once, peek, crash) works against the daemon",
            (rc_follow, rc_peek, rc_crash) == (0, 0, 0)
            and "BOOT_START" in out.getvalue()
            and "DKRT" in out.getvalue()
            and "no crash record" in out.getvalue()
            and cli.read_text().count("\n{") >= 6,
            out.getvalue()[-300:],
        )
        c.done(rig)
    if c.dt2:
        part_modules(c, img)
    if c.tier == "full":
        periodic(c, img, blank, has_diag)
    c.common_checks_upload(project_writes_ok=True)


def part_modules(c: Any, img: Any) -> None:
    """MODULES (protocol command 3, docs/design/digikit-upload.md): after a boot with two modules with DSP code (dyn-params and sine),
    the reply lists both with ranges inside the arena and the DSP area, the contract bytes and version of the file, the assigned machine types and
    the voice rows; paging by start index; it is not logged as an UPLOAD event."""
    files = {
        m: c.pack(m, m.split("/")[-1] + ".dkm")
        for m in ("test/dyn-params", "test/sine")
    }
    card = c.card(list(files.values()))
    with c.rig(img, card, "modules") as rig:
        up = uploader(rig)
        info = up.info()
        c.check(
            "INFO sets the MODULES capability",
            DK.has_cap(info, "MODULES"),
            str(info["caps"]),
        )
        n_up = len(events(rig, "UPLOAD"))
        m = up.modules()
        names = [x["name"] for x in m["modules"]]
        pl = next(x for x in m["modules"] if x["name"] == "test/dyn-params")
        xs = next(x for x in m["modules"] if x["name"] == "test/sine")
        st = M.sym(rig.plan, "core/runtime", "DKRT_STATE")
        arena_lo, arena_hi = rig.w(st + 28), rig.w(st + 32)
        dkm = {
            k: (Path(v).read_bytes())
            for k, v in ((k, c.env.folder / "digikit" / v) for k, v in files.items())
        }

        def hdr(name: str) -> tuple[int, bytes]:
            raw = dkm[name]
            return int.from_bytes(raw[0x34:0x38], "big"), raw[0x38:0x40]

        c.check(
            "MODULES lists the two loaded modules by record index, with the file's version and contract bytes",
            m["total"] == 2
            and names == ["test/dyn-params", "test/sine"]
            and [x["index"] for x in m["modules"]] == [0, 1]
            and all(
                x["version"] == hdr(x["name"])[0]
                and bytes(b for pair in x["contracts"] for b in pair)
                == hdr(x["name"])[1]
                for x in m["modules"]
            ),
            str(m["modules"]),
        )
        c.check(
            "the ColdFire ranges lie inside the arena, do not overlap, and with the free figure stay inside its size",
            all(
                arena_lo <= x["cf_base"]
                and x["cf_base"] + x["cf_size"] <= arena_hi
                and x["cf_size"] > 0
                for x in m["modules"]
            )
            and (
                pl["cf_base"] + pl["cf_size"] <= xs["cf_base"]
                or xs["cf_base"] + xs["cf_size"] <= pl["cf_base"]
            )
            and m["arena_free"] + pl["cf_size"] + xs["cf_size"] <= arena_hi - arena_lo
            and 0 < m["arena_largest"] <= m["arena_free"],
            f"arena {arena_lo:#x}..{arena_hi:#x}, {m['arena_free']} free, {[(hex(x['cf_base']), x['cf_size']) for x in m['modules']]}",
        )
        c.check(
            "the DSP part: dyn-params and sine each have a range inside the DSP area (and the free figure leaves room), the ranges do not overlap",
            pl["dsp_len"] > 0
            and xs["dsp_len"] > 0
            and m["dsp_free"] > 0
            and 0 < m["dsp_largest"] <= m["dsp_free"]
            and (
                pl["dsp_off"] + pl["dsp_len"] <= xs["dsp_off"]
                or xs["dsp_off"] + xs["dsp_len"] <= pl["dsp_off"]
            ),
            f"dsp free {m['dsp_free']}, dyn-params {pl['dsp_off']:#x}+{pl['dsp_len']:#x}, sine {xs['dsp_off']:#x}+{xs['dsp_len']:#x}",
        )
        t_pl = rig.dyn_type("test/dyn-params", "dynp")
        t_xs = rig.dyn_type("test/sine", "sine")
        rows = {k: r[0] for k, r in enumerate(rig.voice_rows())}
        pl_rows = [k for k in range(8) if pl["rows"] >> k & 1]
        xs_rows = [k for k in range(8) if xs["rows"] >> k & 1]
        c.check(
            "the assigned machine types are the ones in the boot's table; dyn-params and sine each own one voice row, with their type",
            pl["types"] == [t_pl]
            and xs["types"] == [t_xs]
            and (t_pl, t_xs) == (16, 17)  # lowest free, in load order
            and len(pl_rows) == 1
            and len(xs_rows) == 1
            and rows[pl_rows[0]] == t_pl
            and rows[xs_rows[0]] == t_xs,
            f"{pl['types']} {xs['types']} rows {pl['rows']:#x} {xs['rows']:#x}",
        )
        c.check(
            "no guard fault so far, and the guard has switched nothing off",
            pl["faults"] == 0
            and xs["faults"] == 0
            and not pl["guard_off"]
            and not xs["guard_off"],
            "",
        )
        # CAPACITY (runtime 0.2.2): limits from the device file, use from the same tables as MODULES
        lim = pk_device.limits(c.device)
        cap = up.capacity()
        arena_total = arena_hi - arena_lo
        want_limits = {
            "MODULES": lim.modules,
            "SELECTION": pk_device.contract_max()["sel_max"],
            "ARENA": arena_total,
            "STAGE": lim.stage_bytes,
            "DSP_AREA": lim.dsp_area_bytes,
            "MACHINE_ROWS": lim.machine_rows,
            "VOICE_ROWS": lim.machine_rows,
            "PARAM_ROWS": lim.param_rows,
            "PAGE_ROWS": lim.page_rows,
            "FX_SLOTS": pk_device.effective_fx_slots(lim, rig.plan),
            "SEND_SLOTS": pk_device.effective_send_slots(c.device, rig.plan),
            "MAP_ENTRIES": pk_device.contract_max()["map_entries"],
            "STORE": c.store.limit // 2,  # KiB
            "SETTINGS": SETTINGS_SECTORS
            * 512,  # bytes of one copy (sdk/sdk.toml [settings])
        }
        c.check(
            "INFO sets the CAPACITY capability, and CAPACITY lists every kind with the device file's limit",
            DK.has_cap(info, "CAPACITY")
            and {k: v["limit"] for k, v in cap.items()} == want_limits,
            str(cap),
        )
        c.check(
            "CAPACITY use agrees with MODULES: two modules, two voice rows, two machine types, the arena and DSP figures equal",
            cap["MODULES"]["used"] == 2
            and cap["SELECTION"]["used"] == 2
            and cap["VOICE_ROWS"]["used"] == 2
            and cap["MACHINE_ROWS"]["used"] == 2
            and cap["MAP_ENTRIES"]["used"] == 2
            and cap["PARAM_ROWS"]["used"] > 0
            and cap["ARENA"]["used"] == arena_total - m["arena_free"]
            and cap["ARENA"]["largest"] == m["arena_largest"]
            and cap["DSP_AREA"]["used"] == lim.dsp_area_bytes - m["dsp_free"]
            and cap["DSP_AREA"]["largest"] == m["dsp_largest"],
            str(cap),
        )
        # paging: from record index 1 only sine; past the end nothing
        b1 = up._ok(DK.CMD_MODULES, "modules", bytes((1,)))
        b9 = up._ok(DK.CMD_MODULES, "modules", bytes((9,)))
        c.check(
            "MODULES pages by start index: from 1 one record (index 1), from 9 none; the total stays 2",
            (b1[16], b1[17], b1[18]) == (2, 1, 1) and (b9[16], b9[17]) == (2, 0),
            f"{b1[16:19].hex()} {b9[16:18].hex()}",
        )
        c.check(
            "MODULES is not logged as an UPLOAD event",
            len(events(rig, "UPLOAD")) == n_up,
            f"{n_up} -> {len(events(rig, 'UPLOAD'))}",
        )
        c.report["modules"] = m
        c.done(rig)


# list positions of the two run-time machines in the guard scenario (the nan is the first entry of the run-time machines, the fixture the second);
# their type numbers are the ones the boot assigned (card order: nan, then the fixture)
GUARD_DN2_HINT = 5  # Digitone II: the run-time machine follows MIDI (as in the equivalence scenarios)
GUARD_NAN_HINT = 8
GUARD_SINE_HINT = 7


def run_guard_dn2(c: Any) -> None:
    """The guard on the Digitone II (the same contract body, one `own_dsp` machine with a one-knob page on track 1): ten presses of TRIG 1 in a second give
    8 or more MACHINE_FAULT (the count in the event) and one MACHINE_DISABLED, and the master output holds no NaN or Inf. (The Digitakt II part
    also keeps a second machine audible and sends the faulting track into the delay: the stock sound there needs a sample.)"""
    import numpy as np

    from tools.patchkit import hook_ab

    nan = c.pack("test/dyn-guard-nan", "nan.dkm")
    dbg_mod = c.pack(
        "digikit/debug", "debug.dkm"
    )  # last: the machine rows stay where they were; the log needs it (MACHINE_CHANGE is its)
    img = c.image_for(B.RT, "guard")
    card = c.card([nan, dbg_mod])
    with c.rig(img, card, "guard") as rig:
        info = rig.boot_info()
        c.check(
            "the fixture and the debug module loaded at boot (CF part, DSP part, one voice row)",
            info["count"] == 2
            and info["results"] == [0, 0]
            and len([r for r in rig.voice_rows() if r[0] != B.FREE_TYPE]) == 1,
            str(info["results"]),
        )
        ui_first = next(e for e in events(rig) if e[2] == L.K["UI_FIRST"])[4][0]
        for _ in range(60):
            if int(rig.emu.call("status")["dsp_frames"]) >= ui_first + 3300:
                break
            rig.s.run_ms(500)
        t_nan = rig.dyn_type("test/dyn-guard-nan", "guardnan")
        rig.select(GUARD_DN2_HINT)
        rig.s.run_ms(800)
        chg = [e for e in events(rig, "MACHINE_CHANGE") if e[4][2] == t_nan]
        c.check(
            "the machine list shows the fixture and track 1 takes its type (MACHINE_CHANGE)",
            bool(chg),
            str([e[4][:3] for e in events(rig, "MACHINE_CHANGE")]),
        )
        wav = c.work / "guard.wav"
        rig.emu.call("pcm.tap", action="capture", path=str(wav))
        rig.s.run_ms(400)
        for _ in range(10):
            rig.s.do_tap("TRIG 1")
            rig.s.run_ms(100)
        rig.s.run_ms(1500)  # a poll of the guard's words is a second apart
        rig.emu.call("pcm.tap", action="capture_stop")
        rig.s.run_ms(2500)
        evs = events(rig)
        faults = [e for e in evs if e[2] == L.K["MACHINE_FAULT"]]
        disabled = [e for e in evs if e[2] == L.K["MACHINE_DISABLED"]]
        last = L.event_dict(faults[-1])["fields"] if faults else {}
        c.check(
            "MACHINE_FAULT is logged (an error event): the fixture's type, kind nonfinite, count 8 or more",
            bool(faults)
            and all(e[3] & 1 for e in faults)
            and last.get("type") == t_nan
            and last.get("kind") == "nonfinite"
            and last.get("faults", 0) >= 8,
            str([L.event_dict(e)["fields"] for e in faults]),
        )
        c.check(
            "MACHINE_DISABLED is logged once, for the fixture's type",
            len(disabled) == 1 and disabled[0][4][0] == t_nan and disabled[0][3] & 1,
            str([L.event_dict(e)["fields"] for e in disabled]),
        )
        n_before = len(faults)
        rig.s.do_tap("TRIG 1")
        rig.s.run_ms(2500)
        c.check(
            "a press after the switch-off causes no new fault",
            len(events(rig, "MACHINE_FAULT")) == n_before,
            f"{n_before} -> {len(events(rig, 'MACHINE_FAULT'))}",
        )
        pcm = hook_ab.read_wav(wav)
        c.check(
            "the master output stays finite (no NaN or Inf in the capture)",
            bool(np.all(np.isfinite(pcm))) and len(pcm) > 48000,
            f"{len(pcm)} samples",
        )
        m = uploader(rig).modules()
        x = m["modules"][0]
        c.check(
            "MODULES shows the fixture's faults and that the guard switched it off",
            x["types"] == [t_nan] and x["faults"] >= 8 and x["guard_off"],
            str(x),
        )
        c.report["guard_wav"] = str(wav)
        c.done(rig)
    c.common_checks_upload(project_writes_ok=True)


def run_guard(c: Any) -> None:
    """The runtime guard of core/dsp-voice (docs/findings/51-pluck-tune-guard.md), on the coupled emulator. A run-time sine on track 1 and the NaN
    fixture (test/dyn-guard-nan) on track 2, whose delay send is up: the fixture's block is NaN while its voice is active, which without the
    guard reaches the send FX and silences every track. Ten TRIG 2 presses in a second: the guard zeroes each block, counts it, logs MACHINE_FAULT
    and switches the type off after the 8th (MACHINE_DISABLED); the sine on track 1 stays audible and finite, and the master output holds no
    NaN or Inf. (The stock sample machine has no sample on this card, so the audible track is a second run-time machine; the guard treats both alike.)"""
    import numpy as np

    from tools.patchkit import hook_ab

    if not c.dt2:
        return run_guard_dn2(c)
    nan = c.pack("test/dyn-guard-nan", "nan.dkm")
    plk = c.pack("test/sine", "sine.dkm")
    dbg_mod = c.pack(
        "digikit/debug", "debug.dkm"
    )  # last: the machine rows stay where they were; the log needs it (MACHINE_CHANGE is its)
    img = c.image_for(B.RT, "guard")
    card = c.card([plk, nan, dbg_mod])
    with c.rig(img, card, "guard") as rig:
        info = rig.boot_info()
        c.check(
            "both modules and the debug module loaded at boot (CF part, DSP part, two voice rows)",
            info["count"] == 3
            and info["results"] == [0, 0, 0]
            and len([r for r in rig.voice_rows() if r[0] != B.FREE_TYPE]) == 2,
            str(info["results"]),
        )
        ui_first = next(e for e in events(rig) if e[2] == L.K["UI_FIRST"])[4][0]
        for _ in range(60):
            if int(rig.emu.call("status")["dsp_frames"]) >= ui_first + 3300:
                break
            rig.s.run_ms(500)

        def pick(track: int, want: int, hint: int) -> None:
            if track > 1:
                rig.s.do_tap(["TRK", f"TRIG {track}"])
                rig.s.run_ms(300)
            for downs in (hint, hint + 1, hint - 1):
                rig.select(downs)
                rig.s.run_ms(800)
                chg = [
                    e
                    for e in events(rig, "MACHINE_CHANGE")
                    if e[4][0] == track - 1 and e[4][2] == want
                ]
                if chg:
                    return
            raise M.Fail(f"no list position gives type {want} on track {track}")

        t_sine = rig.dyn_type("test/sine", "sine")
        t_nan = rig.dyn_type("test/dyn-guard-nan", "guardnan")
        pick(1, t_sine, GUARD_SINE_HINT)
        pick(2, t_nan, GUARD_NAN_HINT)
        # track 2's delay send up: FX page, encoder E (docs/findings: the panel of the DT2 FX pages)
        rig.s.do_tap("FX")
        rig.s.run_ms(300)
        rig.s.turn_burst("ENC E", 4, 30)
        wav = c.work / "guard.wav"
        rig.emu.call("pcm.tap", action="capture", path=str(wav))
        rig.emu.button(H.button("TRIG 1"), True)  # the sine, held
        rig.s.run_ms(400)
        for _ in range(10):  # ten presses of the NaN machine in about a second
            rig.s.do_tap("TRIG 2")
            rig.s.run_ms(100)
        rig.s.run_ms(1500)  # a poll of the guard's words is a second apart
        rig.emu.call("pcm.tap", action="capture_stop")
        rig.emu.button(H.button("TRIG 1"), False)
        rig.s.run_ms(2500)
        evs = events(rig)
        faults = [e for e in evs if e[2] == L.K["MACHINE_FAULT"]]
        disabled = [e for e in evs if e[2] == L.K["MACHINE_DISABLED"]]
        pcm = hook_ab.read_wav(wav)
        c.check(
            "MACHINE_FAULT is logged (an error event): the fixture's type, voice 2 (track 2), kind nonfinite, the parameter words of the call",
            len(faults) >= 1
            and all(e[3] & 1 for e in faults)
            and L.event_dict(faults[-1])["fields"]["type"] == t_nan
            and L.event_dict(faults[-1])["fields"]["track"] == 2
            and L.event_dict(faults[-1])["fields"]["kind"] == "nonfinite",
            str([L.event_dict(e)["fields"] for e in faults]),
        )
        last = L.event_dict(faults[-1])["fields"] if faults else {}
        c.check(
            "the faults are counted: at least 8 (the type is switched off at the 8th within a second)",
            bool(faults) and last.get("faults", 0) >= 8,
            str(last),
        )
        c.check(
            "MACHINE_DISABLED is logged once for the fixture's type",
            len(disabled) == 1 and disabled[0][4][0] == t_nan and disabled[0][3] & 1,
            str([L.event_dict(e)["fields"] for e in disabled]),
        )
        # no fault after the switch-off: a press now changes nothing
        n_before = len(faults)
        rig.s.do_tap("TRIG 2")
        rig.s.run_ms(2500)
        faults2 = events(rig, "MACHINE_FAULT")
        c.check(
            "a press after the switch-off causes no new fault",
            len(faults2) == n_before,
            f"{n_before} -> {len(faults2)}",
        )
        left = pcm if pcm.ndim == 1 else pcm[:, 0]
        win = 4800  # 100 ms
        rms = [
            float(np.sqrt(np.mean(left[i : i + win] ** 2)))
            for i in range(0, len(left) - win, win)
        ]
        c.check(
            "the master output stays finite (no NaN or Inf in the capture)",
            bool(np.all(np.isfinite(pcm))) and len(pcm) > 48000,
            f"{len(pcm)} samples",
        )
        c.check(
            "the sine on track 1 stays audible while the fixture faults (every 100 ms window of the first 1.1 s above -70 dBFS)",
            len(rms) >= 20 and min(rms[0:11]) > 10 ** (-70 / 20),
            f"{[round(20 * np.log10(max(r, 1e-9)), 1) for r in rms[:24]]}",
        )
        c.report["guard_wav"] = str(wav)
        text = c.work / "guard.log"
        L.follow(uploader(rig), text, once=True, echo=False)
        c.report["guard_log"] = str(text)
        c.done(rig)
    c.common_checks_upload(project_writes_ok=True)


def periodic(c: Any, img: Any, card: Path, has_diag: bool) -> None:
    """The periodic events need the firmware's block counter, which only a coupled daemon (with the DSP) advances: a health snapshot every ten
    seconds, the stable-period event after 30, and the DSP poll's events."""
    with c.rig(img, card, "periodic") as rig:
        start = rig.frames()
        rig.s.run_ms(32_000)
        health = events(rig, "HEALTH")
        stable = events(rig, "STABLE")
        diag = events(rig, "DSP_DIAG")
        c.check(
            "a HEALTH snapshot every ten seconds (15,000 blocks)",
            len(health) >= 3
            and all(
                b[1] - a[1] in range(14_000, 16_500)
                for a, b in zip(health, health[1:], strict=False)
            ),
            f"{len(health)} snapshots at {[h[1] for h in health]}",
        )
        c.check(
            "the stable period (30 s) is logged once",
            len(stable) == 1,
            f"{len(stable)} events, {rig.frames() - start} frames run",
        )
        if has_diag:
            starts = [e[1] for e in diag if (e[4][0] & 0xFF) == 0]
            c.check(
                "DSP diagnostics are logged quietly: a full set (24 words, six events) at the first poll, then only every ten seconds while just the beats move",
                len(diag) == 6 * len(starts)
                and 3 <= len(starts) <= 5
                and all(
                    b - a in range(14_000, 16_500)
                    for a, b in zip(starts[1:], starts[2:], strict=False)
                ),
                f"{len(diag)} events in {len(starts)} sets at blocks {starts}",
            )
            stopped = events(rig, "DSP_STOPPED")
            c.check(
                "a running DSP is never reported stopped (the heartbeat, dl_diag word 1, moves)",
                not stopped,
                f"{len(stopped)} DSP_STOPPED events",
            )
        else:
            c.check(
                "without dl_diag in the image the DSP poll logs DSP_ABSENT once and nothing else",
                len(events(rig, "DSP_ABSENT")) == 1 and not diag,
                f"{len(diag)}",
            )
        c.done(rig)


# ---- crash ------------------------------------------------------------------------------------------------------------------


def run_crash(c: Any) -> None:
    t = c.table()
    mod = c.pack(t["crash"], "crash.dkm")
    dbg_mod = c.pack(
        "digikit/debug", "debug.dkm"
    )  # the log: the crash events are read over LOG, and the safe boot loads no module
    img = c.image_for(B.RT, "run")
    cfg = c.env.folder / "digikit" / "cfg"
    cfg.mkdir(parents=True, exist_ok=True)
    cards: dict[str, Path] = {}
    for kind in ("early", "late"):
        (cfg / "crash-after.bin").write_bytes(
            int(t[f"{kind}_blocks"]).to_bytes(4, "big")
            + bytes([int(t[f"{kind}_mode"])])
        )
        cards[kind] = c.card([dbg_mod, mod], stable_blocks=int(t["stable_blocks"]))
    want_vec = {0: 4, 1: 5}  # illegal instruction, divide by zero

    def sequence(kind: str) -> Callable[[], None]:
        def run() -> None:
            mode = int(t[f"{kind}_mode"])
            seen: list[dict[str, Any]] = []
            with c.rig(img, cards[kind], f"crash-{kind}") as rig:
                rig.deliver_exceptions(True)
                for boot in range(3):
                    if boot:
                        rig.wait_reset(settle_ms=0)
                    evs = events(rig)
                    pcs = [
                        e[4][4]
                        for e in evs
                        if e[2] == L.K["MOD_LOG"]
                        and L.ascii_of(*e[4][:4]) == "crash-pc"
                    ]
                    has_log = DK.has_cap(uploader(rig).info(), "LOG")
                    rec = L.crash_record(evs) if has_log else None
                    seen.append(
                        {
                            "info": rig.boot_info(),
                            "crash": rig.crash_info(),
                            "card": rig.card_info(),
                            "rec": rec,
                            "evs": evs,
                            "has_log": has_log,
                            "diag_info": uploader(rig).info(),
                            "sel": B.current(rig.sel_records()),
                        }
                    )
                    # the crash-pc the module logged before it crashed, from the record's own tail (the previous boot's events)
                    seen[-1]["tail_pcs"] = (
                        [
                            e[4][4]
                            for e in rec["tail"]
                            if e[2] == L.K["MOD_LOG"]
                            and L.ascii_of(*e[4][:2]) == "crash-pc"
                        ]
                        if rec and not rec.get("corrupt")
                        else []
                    )
                    seen[-1]["pcs"] = pcs
                    # let the boot run on to its crash (not after the last boot: it is safe mode)
                c.done(rig)
            b0, b1, b2 = seen
            c.check(
                f"{kind}: boot 1 sees no crash record, the exception hook is installed, RAM from before is not there (power-on)",
                b0["crash"]["found"] == 0
                and b0["crash"]["installed"] == 1
                and b0["crash"]["ram_kept"] == 0
                and b0["rec"] is None,
                str(b0["crash"]),
            )
            # boot 2 loads the debug module again, so its log has the whole crash record (the events of the replay); boot 3 is safe mode with no module
            tail_n = [b["crash"]["tail"] for b in (b1, b2)]
            rec = b1["rec"]
            c.check(
                f"{kind}: boot 2 finds the crash record of the boot before, with the module's own PC (vector {want_vec[mode]}, in the module)",
                rec is not None
                and not rec.get("corrupt")
                and b1["crash"]["found"] == 1
                and b1["crash"]["pc"] == rec["pc"]
                and rec["vector"] == want_vec[mode]
                and rec["module"] is not None
                and rec["module"]["index"] == 1
                and bool(rec["module"]["name"])
                and b1["crash"]["ram_kept"] == 1,
                f"found {b1['crash']} rec pc {rec and rec.get('pc')} vec {rec and rec.get('vector')} module {rec and rec.get('module')}",
            )
            c.check(
                f"{kind}: boot 2: INFO says a crash was found; the crash events decode (registers, stack, events before the crash from the runtime's small ring)",
                b1["diag_info"]["crash_found"]
                and rec is not None
                and len(rec["regs"]) == 16
                and len(rec["stack"]) == 32
                and len(rec["tail"]) >= 2
                and rec["a7"] % 4 == 0,
                str(
                    {
                        k: v
                        for k, v in (rec or {}).items()
                        if k in ("regs", "a7", "sr", "frame_word")
                    }
                ),
            )
            c.check(
                f"{kind}: boot 3 (safe mode, no debug module) found the record of the boot before: same PC, tail kept, INFO says a crash was found, no LOG capability",
                b2["crash"]["found"] == 1
                and b2["crash"]["pc"] == b1["crash"]["pc"]
                and all(n > 0 for n in tail_n)
                and b2["diag_info"]["crash_found"]
                and not b2["has_log"],
                f"{b2['crash']} tails {tail_n}",
            )
            c.report[f"crash_{kind}_text"] = (
                L.crash_text(b1["rec"]) if b1["rec"] else ""
            )
            if kind == "early":
                c.check(
                    "early: the crash came before the stable period: boot 2 starts with counter 1 and boot 3 with 2; boot 3 is safe mode (counter), no module loaded",
                    [
                        b0["info"]["counter"],
                        b1["info"]["counter"],
                        b2["info"]["counter"],
                    ]
                    == [0, 1, 2]
                    and b1["info"]["safe"] == 0
                    and b1["info"]["loaded"] == 2
                    and b2["info"]["safe"] == 1
                    and b2["info"]["reason"] == 1
                    and b2["info"]["loaded"] == 0
                    and b1["crash"]["stable"] == 0,
                    str([b["info"]["counter"] for b in seen]),
                )
            else:
                c.check(
                    "late: the crash came after the stable period (counter cleared): the crashed boot is counted bad at the next boot; two such crashes in a row make boot 3 safe mode",
                    b0["info"]["counter"] == 0
                    and b1["info"]["counter"] == 1
                    and b1["info"]["safe"] == 0
                    and b1["info"]["loaded"] == 2
                    and b1["crash"]["stable"] == 1
                    and b2["info"]["counter"] == 2
                    and b2["info"]["safe"] == 1
                    and b2["info"]["reason"] == 1
                    and b2["info"]["loaded"] == 0,
                    str(
                        [
                            (
                                b["info"]["counter"],
                                b["info"]["safe"],
                                b["crash"]["stable"],
                            )
                            for b in seen
                        ]
                    ),
                )
            c.check(
                f"{kind}: the safe boot clears the counter at its first UI-loop pass and keeps the selection",
                b2["info"]["ui_reached"] == 1
                and b2["sel"] is not None
                and b2["sel"]["paths"] == [dbg_mod, mod],
                str(b2["sel"]),
            )

        return run

    c.parallel(sequence("early"), sequence("late"))
    c.common_checks()
