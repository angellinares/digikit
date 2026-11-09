"""The value items of the `test/dsp-facts` probe: check the SHARC core against a Digitakt II capture.

`tools/device/dkprobe_facts.py` defines the items (one instruction on given operands, ASTAT bits 0-13 and STKY cleared before it, result,
ASTAT and STKY read after it). This tool runs the same instructions through the Python core and compares the result word, ASTAT[13:0] and
STKY[9:0]. STKY bits 24 and 26 are stack status (SSEM, LSEM), not an effect of the instruction, so they are masked (as in
`tools/sharc_dev/recip_tables.py`).

    uv run python -m tools.sharc_dev.dsp_facts_values verify out/probe/facts-dt2-device.json     # or the fixture
    uv run python -m tools.sharc_dev.dsp_facts_values fixture out/probe/facts-dt2-device.json    # write the compact fixture

The core runs with MODE1 as the effect adapter sets it on the device (RND32 set; TRUNCATE, ALUSAT and PEYEN clear), plus ALUSAT for the
`*_sat` items. ASTATX and STKYX start at zero.
"""

from __future__ import annotations

import argparse
import json
import sys
from importlib import import_module
from pathlib import Path
from typing import Any

ASTAT_MASK = 0x3FFF
STKY_MASK = 0x3FF
FIXTURE = Path("tests/fixtures/sharc/dsp-facts-values-dt2-1.17.json")
MODE1_RND32 = 1 << 14
MODE1_ALUSAT = 1 << 13

# item op (without the `_sat` suffix) -> the compute fields of its instruction(s): (cu, opcode, rn, rx, ry); a second entry is a
# following instruction. cu 0 is the ALU, 1 the multiplier; `mr1f` is `R0 = MR1F;` (an MR data move).
ALU, MUL = 0, 1
INSTRUCTIONS: dict[str, tuple[tuple[int, int, int, int, int], ...]] = {
    "fix": ((ALU, 0xC9, 0, 1, 0),),
    "fadd": ((ALU, 0x81, 0, 1, 2),),
    "fmul": ((MUL, 0x30, 0, 1, 2),),
    "fmax": ((ALU, 0xE2, 0, 1, 2),),
    "fmin": ((ALU, 0xE1, 0, 1, 2),),
    "fcomp": ((ALU, 0x8A, 0, 1, 2),),
    "fabs": ((ALU, 0xB0, 0, 1, 0),),
    "add": ((ALU, 0x01, 0, 1, 2),),
    "sub": ((ALU, 0x02, 0, 1, 2),),
    "neg": ((ALU, 0x22, 0, 1, 0),),
    "abs": ((ALU, 0x30, 0, 1, 0),),
    "mul_ssf": ((MUL, 0x78, 0, 1, 2),),
    "mul_ssfr_mrf": ((MUL, 0x7D, 0, 1, 2), (-1, 1, 0, 0, 0)),
    "mul_ssi": ((MUL, 0x70, 0, 1, 2),),
}


def base_op(op: str) -> str:
    return op[:-4] if op.endswith("_sat") else op


def load_items(path: Path) -> list[dict[str, Any]]:
    """The value items of a facts capture or of the fixture: op, args, result, astat, stky (ints)."""
    doc = json.loads(path.read_text())
    out = []
    if "facts" in doc:
        for row in doc["facts"]["items"]:
            if row["kind"] != "val" or "result" not in row:
                continue
            out.append(
                {
                    "index": row["index"],
                    "op": row["op"],
                    "args": [int(a, 16) for a in row.get("args", [])],
                    "result": int(row["result"], 16),
                    "astat": int(row["astat"], 16),
                    "stky": int(row["stky"], 16),
                }
            )
        return out
    for row in doc["rows"]:
        index, op, args, result, astat, stky = row.split(",")
        out.append(
            {
                "index": int(index),
                "op": op,
                "args": [int(a, 16) for a in args.split(" ") if a],
                "result": int(result, 16),
                "astat": int(astat, 16),
                "stky": int(stky, 16),
            }
        )
    return out


def core_item(op: str, args: list[int]) -> tuple[int, int, int]:
    """Run the item's instruction(s) through the core: (R0, ASTAT, STKY) from ASTAT = STKY = 0, R0 = 0."""
    t = import_module("tools.sharc_dev.sharc_trace")
    disasm = import_module("tools.sharc_dev.sharc_disasm")
    codes = import_module("tools.sharc_dev.sharc_core.encoding").UREG_CODES
    mode1 = MODE1_RND32 | (MODE1_ALUSAT if op.endswith("_sat") else 0)
    regs: dict[int, Any] = {
        0: t.Const(0),
        codes["ASTATX"]: t.Const(0),
        codes["STKYX"]: t.Const(0),
        codes["MODE1"]: t.Const(mode1),
    }
    for n, a in enumerate(args, start=1):
        regs[n] = t.Const(a)
    state = t.State(1, regs)
    for cu, opcode, rn, rx, ry in INSTRUCTIONS[base_op(op)]:
        if cu < 0:  # R0 = MR1F
            field = (0b100000 << 17) | (0 << 16) | (1 << 12) | (rn << 8)
        else:
            field = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
        fields = {
            "cond[4:0]": 0x1F,
            "compute[22:16]": field >> 16,
            "compute[15:0]": field & 0xFFFF,
        }
        record = disasm.Instruction(0, 6, "2a", fields, kind="confident")
        state = t._execute(state, record)[0]
    return (
        state.uregs[0].value,
        state.uregs[codes["ASTATX"]].value,
        state.uregs[codes["STKYX"]].value,
    )


def check(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The items the core does not reproduce (result or masked flags)."""
    bad = []
    for n, it in enumerate(items):
        got = core_item(it["op"], it["args"])
        want = (it["result"], it["astat"] & ASTAT_MASK, it["stky"] & STKY_MASK)
        have = (got[0], got[1] & ASTAT_MASK, got[2] & STKY_MASK)
        if want != have:
            bad.append(
                {
                    "index": n,
                    "op": it["op"],
                    "args": ["0x%x" % a for a in it["args"]],
                    "device": ["0x%x" % v for v in want],
                    "core": ["0x%x" % v for v in have],
                }
            )
    return bad


def write_fixture(items: list[dict[str, Any]], path: Path, item_count: int = 0) -> None:
    rows = [
        "%d,%s,%s,%08x,%x,%x"
        % (
            it["index"],
            it["op"],
            " ".join("%08x" % a for a in it["args"]),
            it["result"],
            it["astat"] & ASTAT_MASK,
            it["stky"] & STKY_MASK,
        )
        for it in items
    ]
    head = json.dumps(
        {
            "about": "Value items of test/dsp-facts measured on a Digitakt II (OS 1.17, 2026-10-31): item index,op,operands,result,ASTAT[13:0],STKY[9:0]; "
            "the index is decimal, the rest hex; MODE1 RND32 set, ALUSAT set for the _sat ops; ASTATX and STKYX cleared before the instruction",
            "items": item_count,
            "masks": {"astat": ASTAT_MASK, "stky": STKY_MASK},
        },
        indent=1,
    )[:-2]
    path.parent.mkdir(parents=True, exist_ok=True)
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
    v = sub.add_parser(
        "verify", help="run the core over every value item of a capture or fixture"
    )
    v.add_argument("probe", type=Path)
    x = sub.add_parser("fixture", help=f"write the compact fixture ({FIXTURE})")
    x.add_argument("probe", type=Path)
    x.add_argument("--out", type=Path, default=FIXTURE)
    args = ap.parse_args(argv)
    items = load_items(args.probe)
    if args.cmd == "fixture":
        write_fixture(
            items, args.out, len(json.loads(args.probe.read_text())["items_table"])
        )
        print("wrote", args.out, len(items), "items")
        return 0
    bad = check(items)
    print("%d of %d items match" % (len(items) - len(bad), len(items)))
    for b in bad[:30]:
        print(json.dumps(b))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
