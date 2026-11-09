#!/usr/bin/env python3
"""Read the digikit runtime's debug log over USB MIDI SysEx (docs/design/digikit-upload.md commands LOG and PEEK; docs/findings/46-digikit-debug.md).

    uv run python -m tools.device.dklog --emu follow                        # the running emulator (out/native/run/current.json)
    uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt follow
    uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt crash
    uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt peek 0x47e8b000 64

`follow` polls LOG (the device sends nothing unless asked) and writes every new event to the terminal as a readable line and to a file
(default `out/runs/device-logs/<date>.log`): a readable line starting `# ` and, right after it, the same event as one JSON object on a line of
its own. `grep -v '^#' FILE` is a JSON-lines file. It notes a gap (events the ring overwrote before they were read) and a device restart
(the sequence numbers start again). `--once` reads what is there and stops; Ctrl-C stops a follow.

`crash` prints the last crash record the device found at its boot, decoded: the program counter, the vector, the registers, the stack, the
module, and the events before the crash. A record of a stopped DSP (a hang record, version 2) is decoded the same way: the site, the machine type or the
effect row, the module, the core step and the module step. The last fault is also kept on the card as the settings key `dk/fault` (it survives power
cycles); `crash` prints it too (`fault_value`). `peek ADDR LEN` hex-dumps RAM inside the ranges the device file lists (`[digikit] peek`).

The log belongs to the module `digikit/debug`: LOG, PEEK and the levels exist only while it is loaded (INFO then reports the capabilities LOG, PEEK and
LOGCFG). Without it `crash` prints the kept fault record only. The module's ring holds 512 events: a host that polls once a second loses nothing in normal use
(about 1 to 3 events a second at the info level).
The transports are those of `tools.device.dkupload`: `--emu` for the emulator, `--port` for a device through python-rtmidi (not a repository
dependency).
"""

from __future__ import annotations

import argparse
import datetime
import json
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools.device import dkupload as D
from tools.device.dklog_kinds import (
    GUARD_KINDS,
    KINDS,
    LEVEL_MASK,
    LEVEL_SHIFT,
    LEVELS,
    SOURCE_MASK,
    SOURCE_SHIFT,
)

ROOT = Path(__file__).resolve().parents[2]
BLOCKS_PER_SECOND = 1500

# The DSP's breadcrumb (finding 48): stage | voice << 8 | machine type << 16, written by core/dsp-voice before each step of a machine's path.
CRUMB_STAGES = {
    0: "NONE",
    1: "HOOK_ENTERED",
    2: "TYPE_FOUND",
    3: "STOCK_PATH_TAKEN",
    4: "ROUTINE_CALLED",
    5: "ROUTINE_RETURNED",
    6: "ODD_COPY_DONE",
    7: "EPILOGUE_DONE",
    # the fine breadcrumbs of the rung C3 builds (DV_C3_FINE, finding 49 section 9), written inline before the CALL of stage 2
    8: "C3_TYPE_MATCHED",
    9: "C3_FRAME_SAVED",
    10: "C3_CONTEXT_PINNED",
    # the deep breadcrumbs of the rung C3d builds (DV_C3_DEEP, finding 49 section 10), inside the frame save of the registered path
    11: "D_AFTER_MODIFY_I7",
    12: "D_AFTER_R_STORES",
    13: "D_AFTER_I0_STORE",
    14: "D_AFTER_I1_I5_STORES",
    15: "D_AFTER_M1_M3_STORES",
    16: "D_AFTER_B3_I13_STORES",
    17: "D_AFTER_M5_M7_STORES",
    18: "D_AFTER_M13_M15_STORES",
    19: "D_AFTER_L0_L3_STORES",
    20: "D_AFTER_SV_L4_LOAD",
    21: "D_AFTER_L4_L5_STORES",
    22: "D_AFTER_MODE1_READ",
}
# The 32 words of dl_diag (modules/core/adapters/dsp-link/README.md), in order.
DIAG_NAMES = (
    "magic_count",
    "link_beat",
    "voice_beat",
    "M5",
    "M6",
    "M7",
    "M13",
    "M14",
    "M15",
    "I6",
    "I7",
    "B7",
    "L7",
    "MODE1",
    "crumb",
    "dispatches",
    "returns",
    "guard_faults",
    "guard_last",
    "guard_p01",
    "guard_p23",
    "guard_p45",
    "guard_p67",
    "guard_disabled_type",
    "fx_last",
    "module_step",
    "fx_master_calls",
    "fx_send_calls",
    "fx_faults",
    "record_this",
    "record_before",
    "fx_disabled_row",
)

# The fault table's sites (the `site` byte of a fault) and the core steps (stage 4 and 5 of the breadcrumb).
SITES = {0: "unknown", 1: "voice", 2: "fx master", 3: "fx send"}
FAULT_KINDS = {1: "cpu crash", 2: "dsp stopped", 3: "cpu panic"}
FAULT_MAGIC = 0x464C5431  # "FLT1": the value of the settings key dk/fault
FAULT_BYTES = 40
FX_ROW_BYTES = 96


def crumb_of(w: int) -> dict[str, Any]:
    """The breadcrumb word by name: the step, the voice (0-31) and the machine type in play."""
    st = w & 0xFF
    return {
        "stage": CRUMB_STAGES.get(st, f"STAGE{st}"),
        "voice": (w >> 8) & 0xFF,
        "type": w >> 16,
    }


def core_step_text(stage: int) -> str:
    return {0: "none", 1: "ENTER", 4: "CALL", 5: "RET"}.get(stage, f"STAGE{stage}")


def fx_last_text(w: int) -> str:
    """The `last` word of core/fx: stage | site << 8 | row byte offset << 16."""
    if w == 0:
        return "NONE"
    return f"{core_step_text(w & 0xFF)} {SITES.get((w >> 8) & 0xFF, '?')} row={(w >> 16) // FX_ROW_BYTES}"


def crumb_text(w: int) -> str:
    c = crumb_of(w)
    return f"{c['stage']} voice={c['voice']} type={c['type']}"


# kind -> (name, argument names): `KINDS` and `GUARD_KINDS` come from sdk/sdk.toml (`dklog_kinds.py`). The argument words are as
# `modules/core/runtime/src/rs/src` writes them; docs/findings/46-digikit-debug.md.
K = {name: kid for kid, (name, _) in KINDS.items()}
CARD_STATES = {
    0: "unknown",
    1: "ours",
    2: "blank",
    3: "foreign",
    4: "unreadable",
    5: "too small",
}
SAFE_REASONS = {1: "counter", 2: "key", 3: "flag", 4: "dsp"}
REFUSAL = {
    1: "outside the store range",
    2: "card read-only",
    3: "claim failed",
    4: "card too small",
}
REGS = [f"d{i}" for i in range(8)] + [f"a{i}" for i in range(8)]
VECTORS = {
    2: "access error",
    3: "address error",
    4: "illegal instruction",
    5: "divide by zero",
    8: "privilege violation",
    9: "trace",
    10: "line A",
    11: "line F",
    14: "format error",
}
EVENT_FLAG_ERROR = 1


def ascii_of(*words: int) -> str:
    raw = b"".join(w.to_bytes(4, "big") for w in words).split(b"\0")[0]
    return "".join(chr(c) if 32 <= c < 127 else "?" for c in raw)


def sx(v: int) -> int:
    return v - (1 << 32) if v >= 1 << 31 else v


def fields_of(kind: int, a: tuple[int, ...]) -> dict[str, Any]:
    """The argument words of an event by name, with the readable forms (names, signed results) the kind has."""
    name, names = KINDS.get(kind, (f"KIND{kind}", ("a0", "a1", "a2", "a3", "a4")))
    f: dict[str, Any] = {n: v for n, v in zip(names, a, strict=True) if n != "-"}
    if name == "CARD_CHECK":
        f["state"] = CARD_STATES.get(a[0], a[0])
        f["first_nonblank"] = None if a[1] == 0xFFFFFFFF else a[1]
        f["hash"] = f"{a[2]:08x}"
    elif name in ("MOD_LOAD", "MOD_LIST", "CRASH_MOD") and kind != 22:
        f = {k: v for k, v in f.items() if not k.startswith("name")}
        f["name"] = ascii_of(a[2], a[3], a[4])
        if name == "MOD_LOAD":
            f["result"] = sx(a[1])
    elif name == "CRASH_MOD":
        if a[0] == 0x100:
            f = {
                "frame_word": f"{a[1]:08x}",
                "faults": a[2],
                "name_tail": ascii_of(a[3]),
            }
        else:
            f = {
                "module": None if a[0] == 0xFFFFFFFF else a[0],
                "slots_entered": a[1] & 0xFFFF,
                "boot_was_stable": bool(a[1] >> 16),
                "name": ascii_of(a[2], a[3], a[4]),
            }
    elif name == "MOD_DSP":
        f["result"] = sx(a[1])
    elif name == "SAFE":
        f["reason"] = SAFE_REASONS.get(a[0], a[0])
    elif name == "UPLOAD":
        f["status"] = sx(a[1])
        f["cmd"] = f"{a[0]:#x}"
    elif name == "CARD_REFUSED":
        f["reason"] = REFUSAL.get(a[2], a[2])
        f["card_state"] = CARD_STATES.get(a[3], a[3])
    elif name == "MOD_LOG":
        f = {"msg": ascii_of(a[0], a[1], a[2], a[3]), "value": a[4]}
    elif name == "CRASH_FOUND":
        vec = a[1] & 0xFF
        f = {
            "pc": f"{a[0]:08x}",
            "vector": vec,
            "vector_name": VECTORS.get(vec, "?"),
            "format": a[1] >> 8,
            "sr": f"{a[2] & 0xFFFF:04x}",
            "a7": f"{a[3]:08x}",
            "crash_time_blocks": a[4],
        }
    elif name == "HEALTH":
        flags, cpu = a[0] >> 16, a[0] & 0xFFFF
        f = {
            "health_flags": flags,
            "cpu_pct": cpu / 10 if flags & 2 else None,
            "dsp_avg_pct": a[1] / 10 if flags & 4 else None,
            "dsp_max_pct": a[2] / 10 if flags & 4 else None,
            "arena_free": a[3],
            "heap_free_kb": None if a[4] == 0xFFFFFFFF else a[4],
        }
    elif name == "DSP_DIAG":
        count, chunk = a[0] >> 8, a[0] & 0xFF
        f = {"count": count, "chunk": chunk}
        for k, w in enumerate(a[1:]):
            j = chunk * 4 + k
            if j >= count or j >= len(DIAG_NAMES):
                break
            if DIAG_NAMES[j] == "crumb":
                f["crumb"] = crumb_text(w)
            elif DIAG_NAMES[j] == "fx_last":
                f["fx_last"] = fx_last_text(w)
            elif DIAG_NAMES[j] in (
                "link_beat",
                "voice_beat",
                "dispatches",
                "returns",
                "guard_disabled_type",
                "module_step",
                "fx_master_calls",
                "fx_send_calls",
            ):
                f[DIAG_NAMES[j]] = w
            else:
                f[DIAG_NAMES[j]] = f"{w:08x}"
    elif name == "DSP_STOPPED":
        f = {
            "heartbeat": a[0],
            "voice_beat": a[1],
            "crumb": crumb_text(a[2]) if a[2] else "NONE",
            "dispatches": a[3],
            "resumed": bool(a[4]),
        }
    elif name == "MACHINE_CHANGE":
        f = {
            "track": a[0] + 1,
            "old_type": None if a[1] == 0xFFFFFFFF else a[1],
            "new_type": None if a[2] == 0xFFFFFFFF else a[2],
        }
    elif name == "MACHINE_FAULT":
        f = {
            "type": a[0] & 0xFFFF,
            "voice": a[0] >> 16,
            "track": (a[0] >> 16) // 2 + 1,
            "kind": GUARD_KINDS.get(a[1] & 0xFF, str(a[1] & 0xFF)),
            "faults": a[1] >> 8,
            "params": [f"{(w >> s) & 0xFFFF:04x}" for w in a[2:] for s in (0, 16)],
        }
    elif name == "MACHINE_DISABLED":
        f = {"type": a[0], "disabled": a[1], "faults": a[2]}
    elif name in ("FX_FAULT", "FX_DISABLED"):
        f = {"row": a[0], "faults": a[1]}
    elif name == "FAULT_FOUND":
        f = {
            "kind": FAULT_KINDS.get(a[0] & 0xFF, a[0] & 0xFF),
            "site": SITES.get((a[0] >> 8) & 0xFF, (a[0] >> 8) & 0xFF),
            "core_step": core_step_text((a[0] >> 16) & 0xFF),
            "id": a[1],
            "module_step": a[2],
            "at": f"{a[3]:08x}",
            "module": None if a[4] == 0xFFFFFFFF else a[4],
        }
    elif name == "CRASH_REG":
        f = {REGS[a[0] + i]: f"{w:08x}" for i, w in enumerate(a[1:]) if a[0] + i < 16}
    elif name == "CRASH_STACK":
        f = {"first_word": a[0], "words": [f"{w:08x}" for w in a[1:]]}
    return f


def event_dict(ev: tuple[int, int, int, int, tuple[int, ...]]) -> dict[str, Any]:
    seq, t, kind, flags, a = ev
    name = KINDS.get(kind, (f"KIND{kind}", ()))[0]
    lv = (flags >> LEVEL_SHIFT) & LEVEL_MASK
    return {
        "seq": seq,
        "blocks": t,
        "t": round(t / BLOCKS_PER_SECOND, 3),
        "kind": name,
        "kind_id": kind,
        "error": bool(flags & EVENT_FLAG_ERROR),
        "level": LEVELS[lv] if lv < len(LEVELS) else str(lv),
        "source": (flags >> SOURCE_SHIFT) & SOURCE_MASK,
        "args": list(a),
        "fields": fields_of(kind, a),
    }


def text_of(d: dict[str, Any], names: dict[int, str] | None = None) -> str:
    """One readable line. `names` maps a source (module record index + 1) to the module's name, when known; else `mod#N`."""
    parts = []
    for k, v in d["fields"].items():
        if isinstance(v, list):
            v = ",".join(map(str, v))
        parts.append(f"{k}={v}")
    flag = "!" if d["error"] else " "
    src = d.get("source", 0)
    who = (names or {}).get(src, f"mod#{src - 1}") if src else ""
    return f"{d['seq']:6d} {d['t']:10.3f}s {d.get('level', ''):<5s} {who:<10s}{flag}{d['kind']:<13s} {' '.join(parts)}"


def level_of(v: str | int) -> int:
    """A level by name (`LEVELS`) or number 0 to 4."""
    s = str(v).lower()
    if s in LEVELS:
        return LEVELS.index(s)
    if s.isdigit() and int(s) < len(LEVELS):
        return int(s)
    raise ValueError(
        f"unknown level {v!r} (one of {', '.join(LEVELS)} or 0 to {len(LEVELS) - 1})"
    )


def module_names(
    events: list[tuple[int, int, int, int, tuple[int, ...]]],
) -> dict[int, str]:
    """Source (module record index + 1) -> name, from the MOD_LIST events in `events`."""
    out: dict[int, str] = {}
    for _seq, _t, kind, _fl, a in events:
        if kind == K["MOD_LIST"]:
            out[a[0] + 1] = ascii_of(a[2], a[3], a[4])
    return out


def source_of(module: str, names: dict[int, str]) -> int:
    """The source of the module `module`: a name `module_names` knows, or a record index."""
    for src, n in names.items():
        if n == module:
            return src
    if module.isdigit():
        return int(module) + 1
    raise ValueError(
        f"no module {module!r} (the log names modules only in MOD_LIST events; give the record index 0 to 7)"
    )


def shown(
    d: dict[str, Any], level: int | None = None, source: int | None = None
) -> bool:
    """The host-side display filter: errors always pass the level; `source` limits the events to one module."""
    if source is not None and d["source"] != source:
        return False
    if level is None or d["error"]:
        return True
    return d["level"] not in LEVELS or LEVELS.index(d["level"]) <= level


def read_all(
    up: D.Uploader, first: int = 0
) -> tuple[int, int, list[tuple[int, int, int, int, tuple[int, ...]]]]:
    """Poll LOG from `first` until the host has everything the device has: (oldest, newest, events)."""
    out: list[tuple[int, int, int, int, tuple[int, ...]]] = []
    want = first
    oldest = newest = 0
    while True:
        oldest, newest, evs = up.log_page(want)
        if not evs:
            return oldest, newest, out
        out += evs
        want = evs[-1][0] + 1
        if want > newest:
            return oldest, newest, out


class Follower:
    """The state of a `follow`: the next sequence number to ask for, and what was lost."""

    def __init__(
        self,
        up: D.Uploader,
        emit: Callable[[str, dict[str, Any] | None], None],
        start: int = 0,
        level: int | None = None,
        source: int | None = None,
        names: dict[int, str] | None = None,
    ):
        self.up, self.emit, self.next = up, emit, start
        self.seen_newest = 0
        self.level, self.source = (
            level,
            source,
        )  # display filters (the events are still read)
        self.names = dict(names or {})

    def poll(self) -> int:
        """One round of polling; returns the number of events written."""
        n = 0
        while True:
            oldest, newest, evs = self.up.log_page(self.next)
            if newest and newest < self.seen_newest:
                self.emit(
                    f"the device restarted: the log starts again (was at {self.seen_newest}, now {newest})",
                    None,
                )
                self.next = 0
                self.seen_newest = 0
                continue
            if not evs:
                break
            if evs[0][0] > self.next and self.next:
                self.emit(
                    f"{evs[0][0] - self.next} events lost (the ring held from {oldest})",
                    None,
                )
            for ev in evs:
                d = event_dict(ev)
                self.names.update(module_names([ev]))
                if not shown(d, self.level, self.source):
                    continue
                self.emit(text_of(d, self.names), d)
                n += 1
            self.next = evs[-1][0] + 1
            self.seen_newest = max(self.seen_newest, newest)
            if self.next > newest:
                break
        return n


def follow(
    up: D.Uploader,
    out_path: Path | None,
    interval: float = 1.0,
    once: bool = False,
    start: int = 0,
    echo: bool = True,
    sleep: Callable[[float], None] = time.sleep,
    level: int | None = None,
    source: int | None = None,
    names: dict[int, str] | None = None,
) -> int:
    """Poll the log and write it; `once` reads what is there and returns. Returns the number of events written."""
    f = None
    if out_path is not None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        f = out_path.open("a")
        f.write(
            f"# dklog follow, started {datetime.datetime.now().isoformat(timespec='seconds')}\n"
        )

    def emit(text: str, d: dict[str, Any] | None) -> None:
        if echo:
            print(text if d is not None else f"# {text}", flush=True)
        if f is not None:
            f.write(f"# {text}\n")
            if d is not None:
                f.write(
                    json.dumps(
                        {
                            **d,
                            "host_time": datetime.datetime.now().isoformat(
                                timespec="milliseconds"
                            ),
                        }
                    )
                    + "\n"
                )
            f.flush()

    fol = Follower(up, emit, start, level, source, names)
    total = 0
    try:
        while True:
            total += fol.poll()
            if once:
                return total
            sleep(interval)
    except KeyboardInterrupt:
        return total
    finally:
        if f is not None:
            f.close()


# ---- the crash record ---------------------------------------------------------------------------------------------------------


def crash_record(
    events: list[tuple[int, int, int, int, tuple[int, ...]]],
) -> dict[str, Any] | None:
    """The last crash record in a list of events, decoded: the events from the last CRASH_FOUND on that belong to it."""
    starts = [
        i for i, e in enumerate(events) if e[2] == K["CRASH_FOUND"] and e[4][4] != 0xBAD
    ]
    bad = [e for e in events if e[2] == K["CRASH_FOUND"] and e[4][4] == 0xBAD]
    if not starts:
        return {"corrupt": True, "seq": bad[-1][0]} if bad else None
    i = starts[-1]
    pc, vf, sr, a7, tcrash = events[i][4]
    rec: dict[str, Any] = {
        "seq": events[i][0],
        "boot_blocks": events[i][1],
        "pc": pc,
        "vector": vf & 0xFF,
        "vector_name": VECTORS.get(vf & 0xFF, "?"),
        "format": vf >> 8,
        "sr": sr & 0xFFFF,
        "a7": a7,
        "crash_blocks": tcrash,
        "module": None,
        "regs": {},
        "stack": {},
        "tail": [],
    }
    pending: tuple[int, ...] | None = None
    for _seq, _t, kind, _fl, a in events[i + 1 :]:
        if kind == K["CRASH_MOD"]:
            if a[0] == 0x100:
                rec["frame_word"] = a[1]
                rec["faults_in_handler"] = a[2]
            else:
                rec["module"] = {
                    "index": None if a[0] == 0xFFFFFFFF else a[0],
                    "name": ascii_of(a[2], a[3], a[4]),
                    "slots_entered": a[1] & 0xFFFF,
                    "boot_was_stable": bool(a[1] >> 16),
                }
        elif kind == K["CRASH_REG"]:
            for j, w in enumerate(a[1:]):
                if a[0] + j < 16:
                    rec["regs"][REGS[a[0] + j]] = w
        elif kind == K["CRASH_STACK"]:
            for j, w in enumerate(a[1:]):
                rec["stack"][a[0] + j] = w
        elif kind == K["CRASH_TAIL"]:
            # two events per logged event: (seq, time, kind << 16 | flags, arg0, arg1), then (seq, arg2, arg3, arg4, 1)
            if pending is None:
                pending = a
            else:
                rec["tail"].append(
                    (
                        pending[0],
                        pending[1],
                        pending[2] >> 16,
                        pending[2] & 0xFFFF,
                        (pending[3], pending[4], a[1], a[2], a[3]),
                    )
                )
                pending = None
        else:
            break
    return rec


def fault_value(raw: bytes) -> dict[str, Any] | None:
    """The settings value of the key `dk/fault` (40 bytes, big-endian): `FLT1`, kind | site << 8 | core step << 16, id, module step, PC (CPU crash) or the DSP site's
    `last` word, the runtime version, the module name (16 bytes). None when it is not a fault record."""
    if len(raw) < FAULT_BYTES or int.from_bytes(raw[0:4], "big") != FAULT_MAGIC:
        return None
    w = [int.from_bytes(raw[4 * i : 4 * i + 4], "big") for i in range(6)]
    kind = w[1] & 0xFF
    if kind not in FAULT_KINDS:
        return None
    return {
        "kind": FAULT_KINDS[kind],
        "site": SITES.get((w[1] >> 8) & 0xFF, "?"),
        "core_step": core_step_text((w[1] >> 16) & 0xFF),
        "id": w[2],
        "module_step": w[3],
        "pc" if kind == 1 else "last": w[4],
        "runtime": D.fmt_runtime(w[5]),
        "module": ascii_of(
            *(int.from_bytes(raw[24 + 4 * i : 28 + 4 * i], "big") for i in range(4))
        ),
    }


def fault_rows(f: dict[str, Any]) -> list[str]:
    """The ABOUT rows of a fault (the same text as `dkrt_sdk::fault::rows`)."""
    head = {"cpu crash": "FAULT: CPU CRASH", "cpu panic": "FAULT: CPU PANIC"}
    rows = [head.get(f["kind"], "FAULT: DSP STOP")]
    name = f["module"].rsplit("/", 1)[-1].upper()[:15]
    rows.append(
        f"IN {name}"
        if name
        else ("IN FIRMWARE" if f["kind"] == "cpu crash" else "IN UNKNOWN")
    )
    if f["kind"] == "cpu panic":
        pass
    elif f["kind"] == "cpu crash":
        rows.append(f"PC {f['pc']:08X}")
    elif f["site"] == "unknown":
        rows.append("SITE UNKNOWN")
    else:
        step = f"STEP {f['module_step']}" if f["module_step"] else f["core_step"]
        rows.append(f"{f['site'].upper()} {step}")
    return [r[:18] for r in rows]


def hang_record(
    events: list[tuple[int, int, int, int, tuple[int, ...]]],
) -> dict[str, Any] | None:
    """The last hang record (a stopped DSP) in a list of events: the FAULT_FOUND event of kind 2 and the module, the diagnostic words and the events that follow it."""
    starts = [
        i
        for i, e in enumerate(events)
        if e[2] == K["FAULT_FOUND"] and (e[4][0] & 0xFF) == 2
    ]
    if not starts:
        return None
    i = starts[-1]
    a = events[i][4]
    rec: dict[str, Any] = {
        "seq": events[i][0],
        "kind": "dsp stopped",
        "site": SITES.get((a[0] >> 8) & 0xFF, "?"),
        "core_step": core_step_text((a[0] >> 16) & 0xFF),
        "id": a[1],
        "module_step": a[2],
        "last": a[3],
        "module": None,
        "diag": {},
        "tail": [],
    }
    pending: tuple[int, ...] | None = None
    for _seq, _t, kind, _fl, b in events[i + 1 :]:
        if kind == K["CRASH_MOD"] and b[0] != 0x100:
            rec["module"] = {
                "index": None if b[0] == 0xFFFFFFFF else b[0],
                "name": ascii_of(b[2], b[3], b[4]),
            }
        elif kind == K["DSP_DIAG"]:
            chunk = b[0] & 0xFF
            for j, w in enumerate(b[1:]):
                if chunk * 4 + j < len(DIAG_NAMES):
                    rec["diag"][DIAG_NAMES[chunk * 4 + j]] = w
        elif kind == K["CRASH_TAIL"]:
            if pending is None:
                pending = b
            else:
                rec["tail"].append(
                    (
                        pending[0],
                        pending[1],
                        pending[2] >> 16,
                        pending[2] & 0xFFFF,
                        (pending[3], pending[4], b[1], b[2], b[3]),
                    )
                )
                pending = None
        else:
            break
    return rec


def hang_text(rec: dict[str, Any]) -> str:
    m = rec["module"]
    where = f"{m['name']!r}" if m and m["name"] else "an unknown module"
    what = (
        f"machine type {rec['id']}"
        if rec["site"] == "voice"
        else f"effect table row {rec['id'] // FX_ROW_BYTES}"
        if rec["site"].startswith("fx")
        else "no site"
    )
    lines = [
        f"DSP stop found at boot (log event {rec['seq']}): in {where}, site {rec['site']}, {what}",
        f"  core step {rec['core_step']}, module step {rec['module_step']}, last word {rec['last']:08x}",
    ]
    if rec["diag"]:
        lines.append(
            "  DSP words: " + " ".join(f"{k}={v:08x}" for k, v in rec["diag"].items())
        )
    if rec["tail"]:
        lines.append("  events before the stop (oldest first):")
        for seq, t, kind, flags, a in rec["tail"]:
            lines.append("    " + text_of(event_dict((seq, t, kind, flags, a))))
    return "\n".join(lines)


def panic_record(
    events: list[tuple[int, int, int, int, tuple[int, ...]]],
) -> dict[str, Any] | None:
    """The last panic record in a list of events: the FAULT_FOUND event of kind 3 (its second argument is the stack pointer), the stack words and the last events."""
    starts = [
        i
        for i, e in enumerate(events)
        if e[2] == K["FAULT_FOUND"] and (e[4][0] & 0xFF) == 3
    ]
    if not starts:
        return None
    i = starts[-1]
    rec: dict[str, Any] = {
        "seq": events[i][0],
        "panic": True,
        "a7": events[i][4][1],
        "module": None,
        "regs": {},
        "stack": {},
        "tail": [],
    }
    pending: tuple[int, ...] | None = None
    for _seq, _t, kind, _fl, a in events[i + 1 :]:
        if kind == K["CRASH_STACK"]:
            for j, w in enumerate(a[1:]):
                rec["stack"][a[0] + j] = w
        elif kind == K["CRASH_TAIL"]:
            if pending is None:
                pending = a
            else:
                rec["tail"].append(
                    (
                        pending[0],
                        pending[1],
                        pending[2] >> 16,
                        pending[2] & 0xFFFF,
                        (pending[3], pending[4], a[1], a[2], a[3]),
                    )
                )
                pending = None
        else:
            break
    return rec


def crash_text(rec: dict[str, Any]) -> str:
    if rec.get("corrupt"):
        return f"a crash record was found at the boot (event {rec['seq']}) but it failed its CRC"
    m = rec["module"]
    if rec.get("panic"):
        lines = [
            f"CPU PANIC in runtime found at boot (log event {rec['seq']}); the file and line are not kept, the stack holds the way to it",
            f"  a7 {rec['a7']:08x}",
        ]
    else:
        lines = [
            f"crash found at boot (log event {rec['seq']}), crashed {rec['crash_blocks'] / BLOCKS_PER_SECOND:.1f} s after the boot before started",
            f"  vector {rec['vector']} ({rec['vector_name']}), format {rec['format']}, sr {rec['sr']:04x}",
            f"  pc {rec['pc']:08x}   a7 {rec['a7']:08x}",
        ]
    if m is not None:
        where = (
            f"module {m['name']!r} (index {m['index']})"
            if m["index"] is not None
            else f"outside any module, {m['name']!r}"
        )
        lines.append(
            f"  in {where}; slots entered mask {m['slots_entered']:#x}; the boot had {'already' if m['boot_was_stable'] else 'not yet'} reached its stable period"
        )
    if rec["regs"]:
        for i in range(0, 16, 4):
            lines.append(
                "  "
                + "  ".join(f"{r} {rec['regs'].get(r, 0):08x}" for r in REGS[i : i + 4])
            )
    if rec["stack"]:
        words = [rec["stack"].get(i, 0) for i in range(max(rec["stack"]) + 1)]
        lines.append("  stack from a7:")
        for i in range(0, len(words), 8):
            lines.append(
                f"    +{4 * i:03x}  " + " ".join(f"{w:08x}" for w in words[i : i + 8])
            )
    if rec["tail"]:
        lines.append("  events before the crash (oldest first):")
        for seq, t, kind, flags, a in rec["tail"]:
            d = event_dict((seq, t, kind, flags, a))
            lines.append("    " + text_of(d))
    return "\n".join(lines)


# ---- command line --------------------------------------------------------------------------------------------------------------


def hexdump(addr: int, data: bytes) -> str:
    lines = []
    for i in range(0, len(data), 16):
        chunk = data[i : i + 16]
        hexs = " ".join(f"{b:02x}" for b in chunk)
        asc = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        lines.append(f"{addr + i:08x}  {hexs:<47s}  {asc}")
    return "\n".join(lines)


def peek_range(up: D.Uploader, addr: int, length: int) -> bytes:
    out = b""
    while length > 0:
        n = min(512, length)
        out += up.peek(addr, n)
        addr += n
        length -= n
    return out


def level_config(
    up: D.Uploader,
    default: str | None = None,
    modules: list[str] | None = None,
    names: dict[int, str] | None = None,
) -> str:
    """The `level` subcommand: set the default level and/or module levels (`NAME=LEVEL` or `NAME=default`), then show the configuration."""
    mods: dict[int, int | None] = {}
    for item in modules or []:
        name, _, lv = item.partition("=")
        if not lv:
            raise ValueError(f"--module {item!r}: expected NAME=LEVEL")
        mods[source_of(name, names or {}) - 1] = (
            None if lv == "default" else level_of(lv)
        )
    cfg = up.logcfg(None if default is None else level_of(default), mods or None)
    names = names or {}

    def lv(v: int) -> str:
        return "default" if v == 0xFF else (LEVELS[v] if v < len(LEVELS) else str(v))

    lines = [
        f"default: {lv(cfg['default'])}",
        f"debug module: {'loaded' if cfg['debug'] else 'not loaded'}",
    ]
    for i, v in enumerate(cfg["modules"]):
        if v != 0xFF:
            lines.append(f"module {i} ({names.get(i + 1, f'mod#{i}')}): {lv(v)}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument(
        "--emu",
        action="store_true",
        help="the running emulator (the desktop app or `mise run emu:serve`)",
    )
    where.add_argument(
        "--port", help="a MIDI port name substring (a real device; python-rtmidi)"
    )
    ap.add_argument(
        "--run-dir",
        type=Path,
        help="with --emu: the run directory of the emulator to talk to (default: discover)",
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("follow")
    p.add_argument(
        "--out", type=Path, help="the file (default out/runs/device-logs/<date>.log)"
    )
    p.add_argument(
        "--interval", type=float, default=1.0, help="seconds between polls (default 1)"
    )
    p.add_argument(
        "--once", action="store_true", help="read what the device has and stop"
    )
    p.add_argument(
        "--from",
        dest="start",
        type=int,
        default=0,
        help="first sequence number to read (default: the oldest)",
    )
    p.add_argument(
        "--level",
        help="show events at or below this level (error, warn, info, debug, trace or 0 to 4); errors always",
    )
    p.add_argument(
        "--module",
        help="show only this module's events (its name from MOD_LIST, or its record index)",
    )
    sub.add_parser("crash")
    p = sub.add_parser("level", help="show or set the log levels")
    p.add_argument(
        "level",
        nargs="?",
        help="the default level (error, warn, info, debug, trace or 0 to 4)",
    )
    p.add_argument(
        "--module",
        action="append",
        default=[],
        help="NAME=LEVEL (or NAME=default); repeatable; NAME is a name from MOD_LIST or a record index",
    )
    p = sub.add_parser("peek")
    p.add_argument("addr", type=lambda s: int(s, 0))
    p.add_argument("length", type=lambda s: int(s, 0))
    sub.add_parser("info")
    args = ap.parse_args(argv)
    t: Any = (
        D.EmuTransport(D._emu(args.run_dir))
        if args.emu
        else D.RtMidiTransport(args.port)
    )
    up = D.Uploader(t)
    try:
        if args.cmd in ("follow", "peek") and not D.has_cap(up.info(), "LOG"):
            raise ValueError(
                "the debug module (digikit/debug) is not loaded: no log, no peek"
            )
        if args.cmd == "follow":
            out = (
                args.out
                or ROOT
                / "out"
                / "runs"
                / "device-logs"
                / f"{datetime.date.today().isoformat()}.log"
            )
            print(f"# writing {out}", file=sys.stderr)
            names = module_names(read_all(up)[2]) if args.module else {}
            follow(
                up,
                out,
                args.interval,
                args.once,
                args.start,
                level=None if args.level is None else level_of(args.level),
                source=None if args.module is None else source_of(args.module, names),
                names=names,
            )
        elif args.cmd == "level":
            if not D.has_cap(up.info(), "LOGCFG"):
                raise ValueError("the debug module (digikit/debug) is not loaded")
            names = module_names(read_all(up)[2]) if args.module else {}
            print(level_config(up, args.level, args.module, names))
        elif args.cmd == "crash":
            has_log = D.has_cap(up.info(), "LOG")
            evs = read_all(up)[2] if has_log else []
            rec = crash_record(evs)
            hang = hang_record(evs)
            panic = panic_record(evs)
            if panic and panic["seq"] > max(
                rec.get("seq", 0) if rec else 0, hang["seq"] if hang else 0
            ):
                print(crash_text(panic))
            elif hang and (not rec or hang["seq"] > rec.get("seq", 0)):
                print(hang_text(hang))
            else:
                print(
                    crash_text(rec)
                    if rec
                    else "no crash record in the log (the last boot did not follow a crash, or the ring has overwritten it)"
                    if has_log
                    else "the debug module (digikit/debug) is not loaded: only the kept fault record is available"
                )
            try:
                from tools.device import dksettings

                raw = dksettings.decode(up.settings_record()[3])
                kept = fault_value(raw.get("dk/fault") or b"") if raw else None
            except (D.UploadError, TimeoutError, ValueError):
                kept = None
            if kept:
                print("kept on the card (settings key dk/fault):")
                print("  " + " | ".join(fault_rows(kept)))
                print("  " + json.dumps(kept))
        elif args.cmd == "peek":
            print(hexdump(args.addr, peek_range(up, args.addr, args.length)))
        elif args.cmd == "info":
            for k, v in up.info().items():
                print(f"{k}: {D.fmt_runtime(v) if k == 'runtime' else v}")
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except D.UploadError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except TimeoutError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
