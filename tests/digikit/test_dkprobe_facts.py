"""The dsp-facts sequence (tools/device/dkprobe_facts.py): the generated module include, the item list, and the table of facts the host derives."""

import re

from support.paths import REPO_ROOT

from tools.device import dkprobe as P
from tools.device import dkprobe_facts as F

INC = REPO_ROOT / "tests/fixtures/modules/dsp-facts/src/facts_seq.inc"


def test_the_module_include_is_the_generated_one():
    assert INC.read_text() == F.inc_text(), (
        "run: uv run python -m tools.device.dkprobe_facts --write " + str(INC)
    )


def test_the_items_are_distinct_and_the_run_is_whole_frames():
    keys = [(x.op, x.param, x.args, x.alusat) for x in F.facts()]
    assert len(set(keys)) == len(keys) and len(set(F.table_words())) == len(keys)
    assert F.records_per_run() % F.RECORDS_PER_FRAME == 0
    assert F.records_per_run() >= len(keys) + 2
    # a capture holds many runs: the ring of the collector is 24,576 words
    assert 24576 // F.words_per_run() >= 20
    # the slow items come last, the DDR reads at the very end
    ops = [x.op for x in F.facts()]
    assert ops[-1].startswith("dm_ld_ddr") and ops[0] == "block"


def test_generated_code_avoids_the_forms_the_build_refuses():
    inc = F.inc_text()
    # a two-register compute whose destination is a source assembles to a 16-bit form that differs on silicon (docs/MODULE-AUTHORING.md)
    assert not re.search(r"\b(R\d+) = \1 [-+&|^] R\d+;", inc)
    # the registers a module may change: R0-R15, I0-I5, M1-M4 (M5 and M6 are pinned and only used as modifiers)
    assert not re.search(r"\b(I(6|7|8|9|1\d)|L\d|B[0-9]+|M(0|7|8|9|1\d)) =", inc)
    assert "(SSFR)" not in inc.replace(
        "MRF = R1 * R2 (SSFR)", ""
    )  # Selache turns `Rn = Rx * Ry (SSFR)` into a symbol


def test_the_saturation_items_restore_mode1_and_keep_interrupts_off_meanwhile():
    sat = [x for x in F.facts() if x.alusat]
    assert sat and all(x.kind == F.VAL for x in sat)
    code = "\n".join(F._emit_val(sat[0]))
    assert (
        code.index("DM(_fc_mode1) = R0;")
        < code.index("MODE1 = R5;")
        < code.index("R5 = DM(_fc_mode1);")
    )
    assert code.rindex("MODE1 = R5;") > code.index("R3 = ASTAT;")
    assert (
        f"{0xFFFFFFFF ^ (1 << F.MODE1_IRPTEN):#x}" in code
    )  # interrupts off while ALUSAT is set


def _samples(cyc_scale=1, jitter=0):
    """What a capture of 3 runs would hold for the facts, from a core that takes `cyc_scale` cycles per instruction."""
    out: dict[int, list[tuple[int, int, int]]] = {}
    for i, f in enumerate(F.facts()):
        n = f.n if f.n >= 0 else f.param
        if f.kind == F.CYC:
            base = (2 + n * cyc_scale) if f.op != "empty" else 2
            out[i] = [(base + 0 * r, base + 5, base + jitter) for r in range(3)]
        elif f.kind == F.TIME:
            out[i] = [(1000 * r, 666_667 + r, 666_667 - r) for r in range(3)]
        elif f.kind == F.VAL:
            out[i] = [(0x80000000, 0x75000006, 0)] * 3
        else:
            out[i] = [(0x10101, 0x10101, 2)] * 3
    return out


def test_derive_gives_per_unit_cycles_net_of_the_overhead():
    d = F.derive(_samples(cyc_scale=2))
    cyc = d["derived"]["cyc"]
    assert d["derived"]["overhead_cycles"] == 2
    assert cyc["nop"]["per_unit"] == 2.0 and cyc["nop"]["slope"] == 2.0
    assert cyc["nop"]["cold_extra"]["16"] == 5
    assert d["derived"]["block_cycles"]["median"] == 666_667
    row = next(r for r in d["items"] if r["op"] == "fix")
    assert row["result"] == "0x80000000" and row["stable"]


def test_a_capture_table_compares_cycles_as_ratios_and_values_as_equal():
    def mk(scale):
        return {"sequence": {"kind": "dsp-facts"}, "facts": F.derive(_samples(scale))}

    res = P.compare(mk(2), mk(1))
    assert res["same"] and res["different_values"] == []
    nop = next(c for c in res["cycles"] if c["op"] == "nop" and c["param"] == 64)
    assert nop["ratio"] > 1.9
    b = mk(1)
    row = next(r for r in b["facts"]["items"] if r["op"] == "add_sat")
    row["result"] = "0x7fffffff"
    res = P.compare(mk(1), b)
    assert not res["same"] and res["different_values"][0]["op"] == "add_sat"
    assert any("differs: add_sat" in x for x in F.compare_lines(res))
