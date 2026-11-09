#!/usr/bin/env python3
"""Scenarios for `rt_matrix` generated from the firmware's own tables (the atlas of tools/coldfire/atlas.py).

    uv run python -m tools.devtools.rt_scenarios list [--device dn2|dt2] [--category machine,fx,...]
    uv run python -m tools.devtools.rt_scenarios summary

`rt_matrix run --cells @generated` runs every generated cell of the device, `@generated:CATEGORY[,...]` one or more
categories, `@generated:sample` one cell per category, `@generated:NAME[,NAME]` named cells. Add `--profile PREFIX` to capture a SHARC profile per cell.

What the firmware tables give (atlas `descriptors` and `params`, built per OS version with
`uv run python -m tools.coldfire.atlas build IMAGE`):
- the machines, their type ids and, on the Digitakt II, the eight encoder parameters of each machine's SRC page;
- on the Digitone II the machine parameters (`params` rows owned by the machine's type), 8 per page, in code order;
- the common pages (filter, amplitude, FX, modulation) with the parameter behind each of the 8 encoders and the
  ninth knob, and the stock FX pages (delay, reverb, chorus, compressor) with their parameters, names and ranges.

What is written by hand (each marked here): the key behind a page group (PAGE_KEY), the order of the machine menu
(MENU_ORDER), the FX setup panel events (taken from the send-FX scenarios of tests/regression/scenarios up to
SETUP_CUT, SEND_SETUP), and the encoder step (EVENTS_PAGE, EVENTS_FX). The Digitone II machine page layout (8 codes per page in code
order) is an assumption **[O]**: a cell whose encoders change neither the screen nor the firmware state is INVALID.

Each cell has the proof that `rt_matrix` `judge` applies: the screen and the sound or FX state in firmware memory
change over the sweep, the inputs are recorded as scheduled, and the PCM is not silent. A machine switch is
proved by the machine type on the Digitone II and by a state change on the Digitakt II.
"""

from __future__ import annotations

import argparse
import pathlib
import sqlite3
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
IMAGES = {"dn2": "dn2-1.12", "dt2": "dt2-1.17"}
SLOT = 1_000_000  # instructions per encoder event: the spacing the encoder acceleration was calibrated at
EVENTS_PAGE = 28  # events of +4 detents, then as many of -4, per encoder of a page cell (about 40% of the range)
EVENTS_FX = 56  # the same for the one encoder of an FX cell (the full range, as the FX scenarios do)
NAV_GAP = 20 * SLOT
SETUP_CUT = 95_000_000  # the send-FX scenarios start their parameter sweeps after this instruction (by hand)

# hand: the key behind a page group (descriptor `name2`)
PAGE_KEY = {"Filter": 3, "Amplitude": 4, "FX": 5, "MOD": 6}
# hand: the machine menu order, as machine type ids (docs/findings/02: the DT2 list; DN2 as the menu shows)
MENU_ORDER = {"dn2": [0, 2, 1, 3], "dt2": [0, 1, 2, 3, 6, 4, 5]}
# hand: the first machine type of each device in the descriptor table (type id = descriptor index - base)
MACHINE_BASE = {"dn2": 1, "dt2": 6}
# hand: FX page group name -> (send-FX scenario that selects it, short name)
FX_PAGES = {"DEL": ("sf-del-ref", "del"), "REV": ("sf-rev-ref", "rev"),
            "CHO": ("sf-chr-ref", "cho"), "COMP": ("sf-mst-thr0", "comp")}  # fmt: skip
SEND_SETUP = {"dn2": ("ts", "dn2-sendfx.ts"), "dt2": ("ts", "dt2-sendfx.ts")}
CATEGORIES = ("machine", "page", "fx")


def _db(device: str) -> sqlite3.Connection:
    path = ROOT / "out" / "atlas" / f"{IMAGES[device]}.sqlite"
    if not path.is_file():
        raise SystemExit(
            f"rt_scenarios: {path} is missing: uv run python -m tools.coldfire.atlas build {IMAGES[device]}"
        )
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def tables(device: str) -> dict[str, Any]:
    """Machines, page groups and parameters of the device's image, from the atlas."""
    db = _db(device)
    params = {
        r[0]: {
            "name": r[1],
            "short": r[2],
            "owner": r[3],
            "owner_type": r[4],
            "min": r[5],
            "max": r[6],
        }
        for r in db.execute(
            "select code, long_name, short_name, owner_name, owner_type, min, max from params"
        )
    }
    rows = db.execute(
        "select idx, name1, name2, role, fields from descriptors order by idx"
    ).fetchall()
    import json

    desc = [
        {"idx": i, "name1": a, "name2": b, "role": c, "fields": json.loads(d)}
        for i, a, b, c, d in rows
    ]
    return {"params": params, "descriptors": desc}


def _encoders(fields: list[int | None]) -> list[tuple[int, int]]:
    """(encoder 1..9, parameter code) of the nonzero fields of a descriptor (the ninth is the level knob)."""
    return [(i + 1, f) for i, f in enumerate(fields[:9]) if f]


def _turns(encoders: list[int], n: int) -> tuple[list[dict[str, Any]], int]:
    """Events from instruction 0: each encoder N events of +4 detents then N of -4, one per SLOT."""
    out: list[dict[str, Any]] = []
    t = 0
    for e in encoders:
        for sign in (4, -4):
            for _ in range(n):
                out.append({"at_instr": t, "turn": e, "detents": sign})
                t += SLOT
        t += 3 * SLOT
    return out, t


def _press(key: int, n: int, at: int) -> tuple[list[dict[str, Any]], int]:
    out = []
    for _ in range(n):
        out.append({"at_instr": at, "button": key, "down": True})
        out.append({"at_instr": at + 3 * SLOT, "button": key, "down": False})
        at += NAV_GAP
    return out, at


def machines(device: str, t: dict[str, Any]) -> dict[int, dict[str, Any]]:
    """Sound machines by type id: name and, per page, the (encoder, code) list of its parameters."""
    out: dict[int, dict[str, Any]] = {}
    base = MACHINE_BASE[device]
    for d in t["descriptors"]:
        if d["role"] != "machine" or d["name1"] in ("MIDI", "NONE"):
            continue
        typ = d["idx"] - base
        if typ < 0:
            continue
        if device == "dt2":
            pages = [_encoders(d["fields"])]  # the SRC page
        else:
            codes = sorted(
                c
                for c, p in t["params"].items()
                if p["owner"] == "SYN" and p["owner_type"] == typ
            )
            pages = [
                [(i + 1, c) for i, c in enumerate(codes[k : k + 8])]
                for k in range(0, len(codes), 8)
            ]
        out[typ] = {"name": d["name1"], "pages": pages}
    return out


def common_pages(t: dict[str, Any]) -> list[dict[str, Any]]:
    """Filter, amplitude, FX-track and modulation pages: key, press count, (encoder, code) list."""
    seen: dict[str, list[str]] = {}
    pages = []
    for d in t["descriptors"]:
        key = PAGE_KEY.get(d["name2"])
        if d["role"] != "page_group" or key is None or not any(d["fields"][:9]):
            continue
        names = seen.setdefault(d["name2"], [])
        if d["name1"] in names:  # a second layout of the same page (the amplitude page)
            continue
        names.append(d["name1"])
        pages.append(
            {
                "name": d["name1"],
                "key": key,
                "presses": len(names),
                "encoders": _encoders(d["fields"]),
            }
        )
    return pages


def generate(device: str) -> dict[str, dict[str, Any]]:
    """Scenario data by cell name for `rt_matrix` (see SCENARIOS there)."""
    t = tables(device)
    names = {c: p for c, p in t["params"].items()}
    cells: dict[str, dict[str, Any]] = {}
    mach = machines(device, t)
    order = MENU_ORDER[device]
    syn_key = 2

    def pname(code: int) -> str:
        p = names.get(code) or {}
        return f"{p.get('name')}[{p.get('min')}..{p.get('max')}]"

    def add(cell: str, category: str, sc: dict[str, Any], what: str) -> None:
        cells[cell] = {"route": "stock", "devices": (device,), "category": category, "sweep": True,
                       "generated": what, **sc}  # fmt: skip

    for typ, m in mach.items():
        slug = m["name"].lower().replace(" ", "")
        menu = ("menu", order.index(typ), typ if device == "dn2" else None)
        for k, enc in enumerate(m["pages"]):
            ev, sweep_at = _press(syn_key, k + 1, 3 * SLOT)
            turns, _ = _turns([e for e, _ in enc], EVENTS_PAGE)
            ev += [{**x, "at_instr": x["at_instr"] + sweep_at} for x in turns]
            add(f"g-{slug}-src{k + 1}", "machine",
                {"machine": menu, "setup_events": ev, "sweep_at": sweep_at},
                f"{m['name']} source page {k + 1}: " + ", ".join(pname(c) for _, c in enc))  # fmt: skip
        for p in common_pages(t):
            ev, sweep_at = _press(p["key"], p["presses"], 3 * SLOT)
            turns, _ = _turns([e for e, _ in p["encoders"]], EVENTS_PAGE)
            ev += [{**x, "at_instr": x["at_instr"] + sweep_at} for x in turns]
            add(f"g-{slug}-{p['name'].lower()}", "page",
                {"machine": menu, "setup_events": ev, "sweep_at": sweep_at},
                f"{m['name']} with page {p['name']}: " + ", ".join(pname(c) for _, c in p["encoders"]))  # fmt: skip
    # FX: each parameter of each stock FX, with the send of track 1 up (the FX setup events of the send scenarios)
    from tools.devtools import rt_matrix

    kind, file = SEND_SETUP[device]
    seen_fx: set[str] = set()
    for d in t["descriptors"]:
        if (
            d["role"] != "page_group"
            or d["name1"] not in FX_PAGES
            or d["name1"] in seen_fx
        ):
            continue
        seen_fx.add(d["name1"])
        scenario, short = FX_PAGES[d["name1"]]
        setup = [
            e
            for e in rt_matrix.load_setup(kind, file, scenario)
            if e["at_instr"] < SETUP_CUT
        ]
        for enc, code in _encoders(d["fields"]):
            turns, _ = _turns([enc], EVENTS_FX)
            ev = setup + [
                {**x, "at_instr": x["at_instr"] + SETUP_CUT + SLOT} for x in turns
            ]
            machine = (
                ("menu", order.index(0), 0) if device == "dn2" else ("none", 0, None)
            )
            slug = (names.get(code) or {}).get("short", str(code)).lower()
            add(f"g-fx-{short}-{slug}{code}", "fx",
                {"machine": machine, "setup_events": ev, "sweep_at": SETUP_CUT + SLOT, "fx": short},
                f"{d['name2']} encoder {enc}: {pname(code)}")  # fmt: skip
    return cells


def select(device: str, spec: str) -> dict[str, dict[str, Any]]:
    """The cells of `@generated[:category[,category]|:sample]`."""
    cells = generate(device)
    arg = spec.partition(":")[2]
    if not arg:
        return cells
    if arg == "sample":
        first: dict[str, str] = {}
        for name, sc in cells.items():
            first.setdefault(sc["category"], name)
        # one more machine cell on another page kind, to see a common page
        extra = next(
            (
                n
                for n, sc in cells.items()
                if sc["category"] == "machine" and "-src" not in n
            ),
            None,
        )
        keep = set(first.values()) | ({extra} if extra else set())
        return {n: sc for n, sc in cells.items() if n in keep}
    want = set(arg.split(","))
    if want <= set(cells):  # cell names
        return {n: sc for n, sc in cells.items() if n in want}
    if not want <= set(CATEGORIES):
        raise SystemExit(
            f"rt_scenarios: unknown category in {spec!r}; categories: {', '.join(CATEGORIES)}"
        )
    return {n: sc for n, sc in cells.items() if sc["category"] in want}


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("cmd", choices=("list", "summary"))
    p.add_argument("--device", choices=sorted(IMAGES))
    p.add_argument("--category", default="")
    a = p.parse_args(argv)
    for device in [a.device] if a.device else sorted(IMAGES):
        cells = select(
            device, "@generated:" + a.category if a.category else "@generated"
        )
        counts: dict[str, int] = {}
        for name, sc in cells.items():
            counts[sc["category"]] = counts.get(sc["category"], 0) + 1
            if a.cmd == "list":
                print(f"{device} {name:<28} {sc['generated']}")
        print(f"{device}: {len(cells)} cells {counts}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
