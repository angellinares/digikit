"""RECIPS and RSQRTS seed tables: derive them from a probe table of a real device, and check the SHARC core against such a table.

The probe (`tools/device/dkprobe.py`, `tests/fixtures/modules/sharc-probe`) runs both instructions over every table index and the
boundary and special inputs, and records result, ASTAT and STKY for each item. The seed mantissa comes from a ROM table that the
manual does not publish; the rest of the behaviour (exponent, sign, specials, flags) follows the manual and the data.

  recips  seed mantissa: table[7 mantissa MSBs]; seed exponent (biased) 253 - e; sign of the input.
  rsqrts  seed mantissa: table[exponent LSB, 6 mantissa MSBs]; seed exponent (unbiased) -1 - (e >> 1); sign of the input.
  Both tables hold 8-bit seeds (the low 15 mantissa bits of a result are zero).

    uv run python -m tools.sharc_dev.recip_tables fit out/probe/dt2-device.json            # derive and report
    uv run python -m tools.sharc_dev.recip_tables fit out/probe/dt2-device.json --write    # rewrite sharc_core/seed_tables.py
    uv run python -m tools.sharc_dev.recip_tables verify out/probe/dt2-device.json         # run the core over every item
    uv run python -m tools.sharc_dev.recip_tables fixture out/probe/dt2-device.json        # write the compact test fixture

`verify` and the fixture test compare the result word and the masked flags: ASTAT bits 0-13 (`ASTAT_MASK`) and STKY bits 0-9
(`STKY_MASK`). STKY bits 24 and 26 are stack status (SSEM status stack empty, LSEM loop stack empty) and are set on every item, so they are not an effect
of the instruction; ASTAT bits above 13 are other state.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

ASTAT_MASK = 0x3FFF
STKY_MASK = 0x3FF
FIXTURE = Path("tests/fixtures/sharc/recips-rsqrts-dt2-1.17.json")
TABLES_MODULE = Path("tools/sharc_dev/sharc_core/seed_tables.py")

OPCODE = {"recips": 0xC4, "rsqrts": 0xC5}


def load_items(path: Path) -> list[tuple[str, int, int, int, int]]:
    """(op, input, result, astat, stky) per item of a probe JSON or of the fixture."""
    doc = json.loads(path.read_text())
    if "items_table" in doc:
        return [
            (
                i["op"],
                int(i["input"], 16),
                int(i["result"], 16),
                int(i["astat"], 16),
                int(i["stky"], 16),
            )
            for i in doc["items_table"]
        ]
    return [
        (op, int(i, 16), int(r, 16), int(a, 16), int(s, 16))
        for op, i, r, a, s in (row.split(",") for row in doc["rows"])
    ]


def table_index(op: str, bits: int) -> int:
    if op == "recips":
        return (bits >> 16) & 0x7F
    return (((bits >> 23) & 1) << 6) | ((bits >> 17) & 0x3F)


def fit(items: list[tuple[str, int, int, int, int]]) -> dict[str, Any]:
    """The two 128-entry tables (8-bit seeds) from the ordinary inputs, with the conflicts and the missing indices."""
    seen: dict[str, dict[int, set[int]]] = {
        "recips": defaultdict(set),
        "rsqrts": defaultdict(set),
    }
    for op, bits, result, _, _ in items:
        e = (bits >> 23) & 0xFF
        if e in (0, 0xFF) or (bits >> 31):
            continue
        if op == "recips" and e - 127 > 125:
            continue
        seen[op][table_index(op, bits)].add(result & 0x7FFFFF)
    out: dict[str, Any] = {"conflicts": [], "missing": {}, "low_bits_set": []}
    for op, by_index in seen.items():
        table = []
        for k in range(128):
            values = by_index.get(k, set())
            if len(values) > 1:
                out["conflicts"].append((op, k, sorted(values)))
            if not values:
                out["missing"].setdefault(op, []).append(k)
            value = min(values) if values else 0
            if value & 0x7FFF:
                out["low_bits_set"].append((op, k, value))
            table.append(value >> 15)
        out[op] = tuple(table)
    return out


def render_module(recips: tuple[int, ...], rsqrts: tuple[int, ...]) -> str:
    def rows(table: tuple[int, ...]) -> str:
        return "".join(
            "    " + ", ".join("0x%02X" % v for v in table[i : i + 16]) + ",\n"
            for i in range(0, 128, 16)
        )

    return (
        '"""RECIPS and RSQRTS seed tables: the 8-bit mantissa seeds of the ROM tables, measured on a Digitakt II (OS 1.17).\n'
        "\n"
        "Written by `uv run python -m tools.sharc_dev.recip_tables fit PROBE.json --write`; do not edit by hand.\n"
        "Each entry is the top 8 mantissa bits of the result (the low 15 bits are zero). RECIPS is indexed by the 7 mantissa MSBs of the\n"
        "input; RSQRTS by the exponent LSB (bit 6) and the 6 mantissa MSBs (bits 5-0).\n"
        '"""\n'
        "\n"
        "# fmt: off\n"
        "RECIPS_SEED = (\n" + rows(recips) + ")\n"
        "\n"
        "RSQRTS_SEED = (\n" + rows(rsqrts) + ")\n"
        "# fmt: on\n"
    )


def core_item(op: str, bits: int) -> tuple[int, int, int]:
    """Run one instruction through the core: (result, ASTAT, STKY) from ASTAT = STKY = 0."""
    from importlib import import_module

    t = import_module("tools.sharc_dev.sharc_trace")
    disasm = import_module("tools.sharc_dev.sharc_disasm")
    field = (OPCODE[op] << 12) | (0 << 8) | (1 << 4)
    fields = {
        "cond[4:0]": 0x1F,
        "compute[22:16]": field >> 16,
        "compute[15:0]": field & 0xFFFF,
    }
    record = disasm.Instruction(0, 6, "2a", fields, kind="confident")
    codes = import_module("tools.sharc_dev.sharc_core.encoding").UREG_CODES
    state = t.State(
        1,
        {1: t.Const(bits), codes["ASTATX"]: t.Const(0), codes["STKYX"]: t.Const(0)},
    )
    after = t._execute(state, record)[0]
    result = after.uregs[0]
    astat = after.uregs[codes["ASTATX"]]
    stky = after.uregs[codes["STKYX"]]
    return result.value, astat.value, stky.value


def check(items: list[tuple[str, int, int, int, int]]) -> list[dict[str, Any]]:
    """The items the core does not reproduce (result or masked flags)."""
    bad = []
    for n, (op, bits, result, astat, stky) in enumerate(items):
        got = core_item(op, bits)
        want = (result, astat & ASTAT_MASK, stky & STKY_MASK)
        have = (got[0], got[1] & ASTAT_MASK, got[2] & STKY_MASK)
        if want != have:
            bad.append(
                {
                    "index": n,
                    "op": op,
                    "input": "0x%08x" % bits,
                    "device": ["0x%x" % v for v in want],
                    "core": ["0x%x" % v for v in have],
                }
            )
    return bad


def write_fixture(items: list[tuple[str, int, int, int, int]], path: Path) -> None:
    rows = [
        "%s,%08x,%08x,%x,%x" % (op, bits, result, astat & ASTAT_MASK, stky & STKY_MASK)
        for op, bits, result, astat, stky in items
    ]
    doc = {
        "about": "RECIPS and RSQRTS measured on a Digitakt II (OS 1.17, 2026-10-31): op,input,result,ASTAT[13:0],STKY[9:0], all hex",
        "masks": {"astat": ASTAT_MASK, "stky": STKY_MASK},
        "rows": rows,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    head = json.dumps({k: v for k, v in doc.items() if k != "rows"}, indent=1)[:-2]
    path.write_text(
        head
        + ',\n "rows": [\n  '
        + ",\n  ".join(json.dumps(r) for r in rows)
        + "\n ]\n}\n"
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fit", help="derive the tables from a probe JSON")
    f.add_argument("probe", type=Path)
    f.add_argument("--write", action="store_true", help=f"rewrite {TABLES_MODULE}")
    v = sub.add_parser(
        "verify", help="run the core over every item of a probe JSON or fixture"
    )
    v.add_argument("probe", type=Path)
    x = sub.add_parser("fixture", help=f"write the compact fixture ({FIXTURE})")
    x.add_argument("probe", type=Path)
    x.add_argument("--out", type=Path, default=FIXTURE)
    args = ap.parse_args(argv)
    items = load_items(args.probe)
    if args.cmd == "fit":
        res = fit(items)
        print(
            "items %d; conflicts %d; missing %s; indices with low bits set %d"
            % (
                len(items),
                len(res["conflicts"]),
                res["missing"],
                len(res["low_bits_set"]),
            )
        )
        if args.write:
            TABLES_MODULE.write_text(render_module(res["recips"], res["rsqrts"]))
            print("wrote", TABLES_MODULE)
        return 1 if res["conflicts"] or res["missing"] or res["low_bits_set"] else 0
    if args.cmd == "fixture":
        write_fixture(items, args.out)
        print("wrote", args.out)
        return 0
    bad = check(items)
    print("%d of %d items match" % (len(items) - len(bad), len(items)))
    for b in bad[:20]:
        print(json.dumps(b))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
