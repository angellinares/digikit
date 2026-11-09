#!/usr/bin/env python3
"""`dkprobe`: read the words of the SHARC probe from the ColdFire collector (a service module) and write a JSON table; compare two tables.

    uv run python -m tools.device.dkprobe capture --emu [--run-dir DIR] --out FILE.json
    uv run python -m tools.device.dkprobe capture --port Digitakt --out FILE.json
    uv run python -m tools.device.dkprobe compare DEVICE.json EMULATOR.json
    uv run --with python-rtmidi python -m tools.device.dkprobe run --port Digitakt --modules test/dsp-facts --capture OUT.json [--compare REF.json] [--keep]

`run` does everything around a capture (pack, upload, select, restart, capture, compare, restore): `tools/device/dkprobe_run.py`.

The DSP probe module and the collector must be loaded already (upload, select, restart: `dk:run`, `dk:store`). `capture` resets the collector's ring, waits until it is
full (about one run and a margin: 24,576 words, about 16 s at 1500 words a second, one per block, on a device; the emulator is stepped), polls STATUS until the collector has
checked every word, reads the ring, rebuilds the frames on the host (header, size, checksum, frame number) and writes one JSON table: the identity of the
device (INFO, MODULES, hashes of the module files in the store), the collector's counters, the host's own counters, and one entry per item of the sequence
(`tools/device/dkprobe_seq.py`) with the input, the result bits, ASTAT after and STKY after. An item is complete when some run in the capture holds it; items
that differ between two runs of the capture are listed.

Frames come in the order the words were captured. A frame is accepted when its header (0xB0, payload size, frame number) and its checksum are right; other words are skipped.
A gap in the frame numbers counts as a sequence error. A run starts at the frame whose first record is the run header (run counter, item count, table checksum).
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tools.device import dkprobe_seq as seq

CMD_STATUS = 0x32
CMD_READ = 0x33
CMD_RESET = 0x34
READ_CHUNK = 200
STATUS_KEYS = (
    "format",
    "blocks",
    "records",
    "drops",
    "repeats",
    "idle",
    "words",
    "overruns",
    "cap",
    "parsed",
    "frames_ok",
    "seq_errors",
    "cks_errors",
    "last_seq",
    "last_beat",
    "synced",
)
FORMAT = 1


class ProbeError(RuntimeError):
    pass


def parse_status(body: bytes) -> dict[str, int]:
    """STATUS reply (format 3: 16 big-endian u32)."""
    n = len(STATUS_KEYS)
    if len(body) < 4 * n:
        raise ProbeError(f"STATUS reply of {len(body)} bytes, expected {4 * n}")
    return dict(zip(STATUS_KEYS, struct.unpack(f">{n}I", body[: 4 * n]), strict=True))


# ---- frames and runs ----------------------------------------------------------------------------------------------------------


@dataclass
class Frames:
    """What the host found in a list of words."""

    frames: list[tuple[int, list[int]]] = field(
        default_factory=list
    )  # (frame number, payload) in capture order
    skipped: int = 0  # words outside accepted frames
    bad_checksum: int = 0  # candidates (a header-looking word with the right size) whose checksum failed, after the first accepted frame
    seq_errors: int = 0  # accepted frames whose number is not the previous one plus 1


def reassemble(words: list[int], sq: Any = seq) -> Frames:
    out = Frames()
    p, n = 0, len(words)
    while p < n:
        h = words[p]
        cnt = (h >> 16) & 0xFF
        if (h >> 24) == sq.HDR_MAGIC and cnt == sq.PAYLOAD_WORDS:
            if p + cnt + 1 >= n:
                break
            if sq.fletcher(words[p : p + cnt + 1]) == words[p + cnt + 1]:
                num = h & 0xFFFF
                if out.frames and ((num - out.frames[-1][0]) & 0xFFFF) != 1:
                    out.seq_errors += 1
                out.frames.append((num, words[p + 1 : p + 1 + cnt]))
                p += cnt + 2
                continue
            if out.frames:
                out.bad_checksum += 1
        out.skipped += 1
        p += 1
    return out


@dataclass
class Item:
    index: int
    op: str
    input: int
    result: int
    astat: int
    stky: int


@dataclass
class Table:
    items: dict[int, Item] = field(default_factory=dict)
    runs_seen: list[int] = field(
        default_factory=list
    )  # run counters whose header was found
    run_info: dict[int, dict[str, int]] = field(
        default_factory=dict
    )  # run counter -> MODE1, ASTAT, STKY as found
    conflicts: list[int] = field(
        default_factory=list
    )  # items whose records differ between two runs
    table_checksum_ok: bool = True
    samples: dict[int, list[tuple[int, int, int]]] = field(
        default_factory=dict
    )  # item index -> the three words of its record in every run that holds it, in capture order


def _records(payload: list[int], sq: Any = seq) -> list[list[int]]:
    return [
        payload[i : i + sq.WORDS_PER_RECORD]
        for i in range(0, len(payload), sq.WORDS_PER_RECORD)
    ]


def build_table(fr: Frames, sq: Any = seq) -> Table:
    """Place every record of every frame in the run it belongs to. A frame whose run header is in the capture is placed from it (forwards or, for the frames
    before the first header, backwards from the next one)."""
    its = sq.items()
    fpr = sq.frames_per_run()
    tab = Table()
    n = len(fr.frames)
    # position of every frame in its run: anchors are the frames that start with the run header
    anchors: list[
        tuple[int, int, int]
    ] = []  # (index in fr.frames, frame number, run counter)
    want_sum = sq.table_checksum()
    for i, (num, pl) in enumerate(fr.frames):
        if pl[1] == len(its) and pl[0] != 0xFFFFFFFF:
            if pl[2] == want_sum:
                anchors.append((i, num, pl[0]))
            else:
                tab.table_checksum_ok = False
    if not anchors:
        return tab
    placed: dict[
        int, tuple[int, int]
    ] = {}  # frame index -> (run counter, position in run)
    for i in range(n):
        # the nearest anchor before, else the first one after
        before = [a for a in anchors if a[0] <= i]
        if before:
            _, anum, arun = before[-1]
            d = (fr.frames[i][0] - anum) & 0xFFFF
            run, pos = arun + d // fpr, d % fpr
        else:
            _, anum, arun = anchors[0]
            d = (anum - fr.frames[i][0]) & 0xFFFF
            back = -(-d // fpr)
            run, pos = arun - back, (-d) % fpr
        placed[i] = (run, pos)
    seen_runs: set[int] = set()
    for i, (_, pl) in enumerate(fr.frames):
        run, pos = placed[i]
        seen_runs.add(run)
        for k, rec in enumerate(_records(pl, sq)):
            r = pos * sq.RECORDS_PER_FRAME + k
            if r == 1:
                tab.run_info[run] = {"mode1": rec[0], "astat": rec[1], "stky": rec[2]}
            if r < 2 or r >= 2 + len(its):
                continue
            idx = r - 2
            op, inp = its[idx]
            item = Item(idx, op, inp, rec[0], rec[1], rec[2])
            tab.samples.setdefault(idx, []).append((rec[0], rec[1], rec[2]))
            old = tab.items.get(idx)
            if old is None:
                tab.items[idx] = item
            elif (old.result, old.astat, old.stky) != (
                item.result,
                item.astat,
                item.stky,
            ) and idx not in tab.conflicts:
                tab.conflicts.append(idx)
    tab.runs_seen = sorted(a[2] for a in anchors)
    return tab


# ---- the device ---------------------------------------------------------------------------------------------------------------


class Probe:
    """The collector's commands over an Uploader. `advance` lets time pass: the emulator is stepped, a device is waited for."""

    def __init__(self, up: Any, emu: Any | None = None, step: int = 100_000_000):
        self.up, self.emu, self.step = up, emu, step

    def advance(self, seconds: float = 0.5) -> None:
        if self.emu is not None:
            self.emu.step(self.step)
        else:
            time.sleep(seconds)

    def status(self) -> dict[str, int]:
        st, body = self.up.request(CMD_STATUS)
        if st != 0:
            raise ProbeError(f"STATUS: status {st} (is digikit/probe loaded?)")
        return parse_status(body)

    def reset(self) -> None:
        st, _ = self.up.request(CMD_RESET)
        if st != 0:
            raise ProbeError(f"RESET: status {st}")

    def read_words(self, count: int) -> list[int]:
        words: list[int] = []
        while len(words) < count:
            want = min(READ_CHUNK, count - len(words))
            st, body = self.up.request(CMD_READ, struct.pack(">IH", len(words), want))
            if st != 0:
                raise ProbeError(f"READ: status {st}")
            total = struct.unpack_from(">I", body, 0)[0]
            got = (len(body) - 8) // 4
            if got == 0:
                break
            words += struct.unpack(f">{got}I", body[8 : 8 + 4 * got])
            if len(words) >= total:
                break
        return words

    def wait_full(
        self, timeout_s: float = 600.0, max_advances: int = 400
    ) -> dict[str, int]:
        """Wait until the ring is full, then until the collector has checked every word."""
        t0 = time.monotonic()
        for _ in range(max_advances):
            s = self.status()
            if s["words"] >= s["cap"]:
                break
            if time.monotonic() - t0 > timeout_s:
                raise ProbeError(f"ring not full after {timeout_s} s: {s}")
            self.advance()
        else:
            raise ProbeError(
                f"ring not full after {max_advances} waits: {self.status()}"
            )
        last = -1
        while True:
            s = self.status()
            if s["parsed"] == last or s["parsed"] >= s["words"] - seq.FRAME_WORDS:
                return s
            last = s["parsed"]


def identity(up: Any) -> dict[str, Any]:
    info = up.info()
    mods = up.modules()
    store = [
        {"path": p, "size": sz, "hash": h}
        for p, sz, h in up.ls()
        if p.startswith("modules/")
    ]
    return {"info": info, "modules": mods.get("modules", []), "store": store}


def capture(
    up: Any, emu: Any | None = None, step: int = 100_000_000, sq: Any = seq
) -> dict[str, Any]:
    pr = Probe(up, emu, step)
    ident = identity(up)
    t0 = time.monotonic()
    pr.reset()
    s = pr.wait_full()
    words = pr.read_words(s["words"])
    elapsed = time.monotonic() - t0
    fr = reassemble(words, sq)
    tab = build_table(fr, sq)
    total = len(sq.items())
    items = [
        {
            "index": it.index,
            "op": it.op,
            "input": f"0x{it.input:08x}",
            "result": f"0x{it.result:08x}",
            "astat": f"0x{it.astat:08x}",
            "stky": f"0x{it.stky:08x}",
        }
        for it in sorted(tab.items.values(), key=lambda x: x.index)
    ]
    doc = {
        "format": FORMAT,
        "identity": ident,
        "sequence": {
            "kind": getattr(sq, "KIND", "sharc-recips"),
            "items": total,
            "records_per_run": sq.records_per_run(),
            "frames_per_run": sq.frames_per_run(),
            "table_checksum": f"0x{sq.table_checksum():08x}",
        },
        "collector": s,
        "host": {
            "words": len(words),
            "frames": len(fr.frames),
            "skipped_words": fr.skipped,
            "seq_errors": fr.seq_errors,
            "checksum_errors": fr.bad_checksum,
            "runs": tab.runs_seen,
            "table_checksum_ok": tab.table_checksum_ok,
            "items_present": len(tab.items),
            "items_missing": total - len(tab.items),
            "conflicts": tab.conflicts,
            "complete": len(tab.items) == total
            and fr.seq_errors == 0
            and fr.bad_checksum == 0
            and tab.table_checksum_ok,
            # every item is there and the module is the one the host expects; a lost word costs one frame of one run (the sequence repeats)
            "usable": len(tab.items) == total and tab.table_checksum_ok,
            "capture_seconds": round(elapsed, 1),
        },
        "run_info": {str(k): v for k, v in sorted(tab.run_info.items())},
        "items_table": items,
    }
    if hasattr(sq, "derive"):
        doc["facts"] = sq.derive(tab.samples)
    return doc


# ---- compare ------------------------------------------------------------------------------------------------------------------


def _by_index(doc: dict[str, Any]) -> dict[int, dict[str, Any]]:
    return {int(i["index"]): i for i in doc["items_table"]}


def compare(a: dict[str, Any], b: dict[str, Any]) -> dict[str, Any]:
    """Differences of two tables: per field counts, per op, and the first few items that differ. Tables of the dsp-facts sequence are
    compared by `dkprobe_facts.compare` (cycle counts differ between a device and the emulator by design)."""
    kind = a["sequence"].get("kind", "sharc-recips")
    if kind != b["sequence"].get("kind", "sharc-recips"):
        raise ProbeError(
            f"tables of different sequences: {kind} and {b['sequence'].get('kind')}"
        )
    if kind == "dsp-facts":
        from tools.device import dkprobe_facts

        return dkprobe_facts.compare(a, b)
    ia, ib = _by_index(a), _by_index(b)
    common = sorted(set(ia) & set(ib))
    diffs: list[dict[str, Any]] = []
    fields = ("result", "astat", "stky")
    counts = dict.fromkeys(fields, 0)
    per_op: dict[str, int] = {}
    for i in common:
        d = [f for f in fields if ia[i][f] != ib[i][f]]
        for f in d:
            counts[f] += 1
        if d:
            per_op[ia[i]["op"]] = per_op.get(ia[i]["op"], 0) + 1
            diffs.append(
                {
                    "index": i,
                    "op": ia[i]["op"],
                    "input": ia[i]["input"],
                    "fields": d,
                    "a": [ia[i][f] for f in d],
                    "b": [ib[i][f] for f in d],
                }
            )
    return {
        "compared": len(common),
        "only_a": sorted(set(ia) - set(ib))[:20],
        "only_b": sorted(set(ib) - set(ia))[:20],
        "differing_items": len(diffs),
        "field_counts": counts,
        "per_op": per_op,
        "same_sequence": a["sequence"] == b["sequence"],
        "same": not diffs and set(ia) == set(ib),
        "first_differences": diffs[:20],
        "identity": {
            "a": a["identity"]["info"].get("image_id"),
            "b": b["identity"]["info"].get("image_id"),
        },
    }


# ---- command line -------------------------------------------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser(
        "capture", help="read one run from the collector and write the JSON table"
    )
    where = c.add_mutually_exclusive_group(required=True)
    where.add_argument(
        "--emu", action="store_true", help="the running emulator (stepped)"
    )
    where.add_argument(
        "--port", help="a MIDI port name substring (a real device; python-rtmidi)"
    )
    c.add_argument(
        "--run-dir", type=Path, help="with --emu: the run directory of the daemon"
    )
    c.add_argument("--out", type=Path, required=True)
    c.add_argument(
        "--seq",
        choices=("sharc", "facts"),
        default="sharc",
        help="the sequence the probe module sends: sharc (test/sharc-probe) or facts (test/dsp-facts)",
    )
    c.add_argument(
        "--step",
        type=int,
        default=100_000_000,
        help="with --emu: instructions per wait",
    )
    r = sub.add_parser(
        "run",
        help="upload, select and restart the probe modules, capture, compare, restore (see tools/device/dkprobe_run.py)",
    )
    where = r.add_mutually_exclusive_group(required=True)
    where.add_argument("--emu", action="store_true", help="the running emulator")
    where.add_argument("--port", help="a MIDI port name substring (a real device)")
    where.add_argument(
        "--spawn",
        choices=("dt2", "dn2"),
        help="start a private emulator daemon of this device (stock firmware with the digikit runtime) and stop it at the end",
    )
    r.add_argument(
        "--run-dir", type=Path, help="with --emu: the daemon's run directory"
    )
    r.add_argument(
        "--modules",
        required=True,
        help="probe module ids, comma separated (test/sharc-probe, test/dsp-facts); digikit/remote and digikit/probe are added",
    )
    r.add_argument(
        "--capture", type=Path, required=True, help="the JSON table to write"
    )
    r.add_argument(
        "--compare", type=Path, help="a JSON table to compare the capture with"
    )
    r.add_argument(
        "--keep",
        action="store_true",
        help="leave the probe modules selected and loaded",
    )
    r.add_argument(
        "--seq", choices=("sharc", "facts"), help="default: by the probe module"
    )
    r.add_argument("--device", help="pack target (default: the device id INFO reports)")
    r.add_argument(
        "--step",
        type=int,
        default=100_000_000,
        help="with --emu: instructions per wait",
    )
    d = sub.add_parser(
        "compare",
        help="differences of two JSON tables (for example device and emulator)",
    )
    d.add_argument("a", type=Path)
    d.add_argument("b", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "compare":
        res = compare(json.loads(args.a.read_text()), json.loads(args.b.read_text()))
        print(json.dumps(res, indent=1))
        return 0 if res["same"] else 1
    if args.cmd == "run":
        from tools.device import dkprobe_run

        return dkprobe_run.main(args)
    from tools.device import dkupload as D

    emu = D._emu(args.run_dir) if args.emu else None
    transport: Any = D.EmuTransport(emu) if args.emu else D.RtMidiTransport(args.port)
    from tools.device import dkprobe_run

    doc = capture(
        D.Uploader(transport, skip_foreign=not args.emu),
        emu,
        args.step,
        dkprobe_run.SEQUENCES[args.seq],
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(doc, indent=1) + "\n")
    h = doc["host"]
    print(
        f"{args.out}: {h['items_present']}/{doc['sequence']['items']} items, {h['frames']} frames, "
        f"seq errors {h['seq_errors']}, checksum errors {h['checksum_errors']}, runs {h['runs']}, complete {h['complete']}"
    )
    return 0 if h["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
