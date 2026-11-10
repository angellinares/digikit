"""core/machines (Digitakt II) as a module: the ColdFire routines against a model of the Rust generator's behaviour
(a1cf1df^:native/patchkit/src/machines.rs, removed in a1cf1df), on the native core with hand-built tables (tools/patchkit/pk_cfunit.py).

Skips when the m68k toolchain or the DT2 1.17 image is absent."""

import sys

import pytest
from support.paths import REPO_ROOT
from support.words import be32

ROOT = REPO_ROOT

from tools.firmware import config  # noqa: E402
from tools.patchkit import (  # noqa: E402
    pk_cfunit,  # noqa: E402
    pk_toolchains,  # noqa: E402
)

SYX = config.stock("Digitakt_II_OS1.17.syx")
SRC = (
    ROOT
    / "modules"
    / "core"
    / "adapters"
    / "machines"
    / "src"
    / "cf"
    / "dt2"
    / "machines.s"
)
ROWSIZE = 64
FIRST = 7
ROWS = 0x47EC0000
STOCK_LIST = 0x47EC1000
NAMES = 0x47EC2000
TYPEARG_EXIT = "_mf_x_typearg"

STUBS = [
    "_mf_t_dispatch",
    "_mf_t_list",
    "_mf_t_rank",
    "_mf_t_typearg",
    "_mf_x_typearg",
    "_mf_t_clone5",
    "_mf_c0a",
    ("_mf_c0", 8),
    ("_mf_c1", 6),
    "_mf_c1a",
    ("_mf_c2", 8),
    "_mf_c2a",
    ("_mf_c3", 6),
    "_mf_c3a",
    ("_mf_c4", 6),
    "_mf_c4a",
    ("_mf_c6", 12),
    "_mf_c6a",
    ("_mf_sp", 8),
    "_mf_spa",
]

pytestmark = [
    pytest.mark.skipif(
        pk_toolchains.m68k_bin("gcc") is None, reason="m68k toolchain not installed"
    ),
    pytest.mark.skipif(
        not SYX.exists(), reason="firmware/Digitakt_II_OS1.17.syx not present"
    ),
]


def row(name, short, f, clone_of, position, own_dsp=0):
    """One registry row: descriptor pointers (built at run time), nine ID fields, clone_of, position, own_dsp, two strings."""
    return dict(
        name=name,
        short=short,
        f=f,
        clone_of=clone_of,
        position=position,
        own_dsp=own_dsp,
    )


ENTRIES = [
    row("XSlice", "XSL", [0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0, 0xFE, 0x0A], 6, 7),
    row("Pluck", "PLK", [0xCA, 0x113, 0x114, 0, 0x115, 0x116, 0, 0xD1, 0x0A], 0, 2),
    row("XClone", "XCL", [0xF8, 0xF9, 0, 0xFB, 0xFC, 0xFD, 0, 0xFE, 0x0A], 6, 255),
]
COUNT = len(ENTRIES)
STR_AT = 0x47EC3000
TYPE_FN = 0x47EC4000  # a stand-in for FUN_4005f6bc: `move.l #type,d0; rts`, the type is poked per case
STOCK = [3, 0, 1, 2, 4, 5, 6]  # a stock display order


def tables():
    mem = {STOCK_LIST: b"".join(be32(t) for t in STOCK)}
    rows = b""
    at = STR_AT
    strings = b""
    for e in ENTRIES:
        n, s = at, at + len(e["name"]) + 1
        strings += e["name"].encode() + b"\0" + e["short"].encode() + b"\0"
        at = STR_AT + len(strings)
        rows += be32(0) + be32(0) + b"".join(be32(x) for x in e["f"])
        rows += (
            be32(e["clone_of"])
            + be32(e["position"])
            + be32(e["own_dsp"])
            + be32(n)
            + be32(s)
        )
    assert len(rows) == ROWSIZE * COUNT
    mem[ROWS] = rows
    mem[STR_AT] = strings
    return mem


@pytest.fixture(scope="module")
def unit():
    return pk_cfunit.CfUnit(
        SYX,
        [SRC, ROOT / "tests/fixtures/native-page-absent.s"],
        STUBS,
        {
            "_mf_first": FIRST,
            "_mf_count": COUNT,
            "_mf_entries": COUNT,
            "_mf_rows": ROWS,
            "_mf_list_stock": STOCK_LIST,
            "_mf_spt": TYPE_FN,
        },
        [
            "mf_srcpage",
            "mf_dispatch",
            "mf_list_entry",
            "mf_rank",
            "mf_typearg",
            "mf_clone5",
            *CLONE_SITES,
        ],
    )


def model_order():
    order = list(STOCK)
    for k, e in enumerate(ENTRIES):
        pos = min(e["position"], len(order))
        order.insert(pos, FIRST + k)
    return order


def test_all_instructions_decode_and_use_no_privileged_forms(unit):
    assert sum(unit.hist.values()) > 100


def test_dispatch_returns_the_descriptor_of_an_added_type_and_hands_others_on(unit):
    mem = tables()
    cases = [
        {"routine": "mf_dispatch", "stack": [unit.rts, t]}
        for t in (FIRST, FIRST + 1, FIRST + 2, 6, 0, FIRST + COUNT, 200)
    ]
    cases.append(
        {
            "routine": "mf_dispatch",
            "stack": [unit.rts, FIRST],
            "reads": [(ROWS, ROWSIZE * COUNT)],
        }
    )
    res = unit.run_cases(mem, cases)
    for k in range(COUNT):
        assert res[k].stub == "rts" and res[k].d[0] == ROWS + k * ROWSIZE
    for r in res[3:7]:
        assert r.stub == "_mf_t_dispatch"
    rows = res[7].reads[0]
    for k, e in enumerate(ENTRIES):
        base = k * ROWSIZE
        # the descriptor strings are checked in test_descriptor_strings
        assert rows[base + 8 : base + 44] == b"".join(be32(x) for x in e["f"])


def test_descriptor_strings_are_std_string_representations(unit):
    mem = tables()
    res = unit.run_cases(
        mem,
        [{"routine": "mf_dispatch", "stack": [unit.rts, FIRST], "reads": [(ROWS, 8)]}],
    )[0]
    long_p, short_p = (int.from_bytes(res.reads[0][i : i + 4], "big") for i in (0, 4))
    got = unit.run_cases(
        mem,
        [
            {
                "routine": "mf_dispatch",
                "stack": [unit.rts, FIRST],
                "reads": [(long_p - 12, 12 + 7), (short_p - 12, 12 + 4)],
            }
        ],
    )[0]
    # state is rebuilt per run: the pointers are identical because the bss is
    # zeroed and the build is deterministic
    for blob, text in zip(got.reads, ("XSLICE", "XSL"), strict=True):
        assert blob[:12] == be32(len(text)) + be32(len(text)) + be32(0xFFFFFFFF)
        assert blob[12 : 12 + len(text) + 1] == text.encode() + b"\0"


def test_list_and_rank_tables(unit):
    mem = tables()
    res = unit.run_cases(
        mem,
        [
            {"routine": "mf_list_entry"},
            {
                "routine": "mf_rank",
                "stack": [0x1111, 0x2222, 0x3333],
            },
            {"routine": "mf_dispatch", "stack": [unit.rts, FIRST]},
        ],
    )
    assert res[0].stub == "_mf_t_list"
    assert res[1].stub == "_mf_t_rank"
    order = model_order()
    total = FIRST + COUNT
    assert len(order) == total
    begin, end = res[1].stack[1], res[1].stack[2]
    assert res[1].stack[0] == 0x1111  # the map argument is untouched
    assert end - begin == 8 * total
    # read the tables back with a second pass over the same image
    got = unit.run_cases(
        mem,
        [
            {"routine": "mf_list_entry", "reads": []},
            {
                "routine": "mf_rank",
                "stack": [0, 0, 0],
                "reads": [(begin, 8 * total)],
            },
        ],
    )[1]
    pairs = got.reads[0]
    for i, t in enumerate(order):
        assert pairs[8 * i : 8 * i + 4] == be32(t) and pairs[
            8 * i + 4 : 8 * i + 8
        ] == be32(i)


GAP = row("", "", [0] * 8 + [0x0A], 0, 0xFFFFFFFF)


@pytest.fixture(scope="module")
def gap_unit():
    # rows: XSlice (7), a gap (8: nobody selected it), Pluck (9)
    return pk_cfunit.CfUnit(
        SYX,
        [SRC, ROOT / "tests/fixtures/native-page-absent.s"],
        STUBS,
        {
            "_mf_first": FIRST,
            "_mf_count": 3,
            "_mf_entries": 2,
            "_mf_rows": ROWS,
            "_mf_list_stock": STOCK_LIST,
            "_mf_spt": TYPE_FN,
        },
        ["mf_list_entry", "mf_rank"],
    )


def test_a_gap_row_is_neither_listed_nor_ranked(gap_unit, monkeypatch):
    monkeypatch.setattr(sys.modules[__name__], "ENTRIES", [ENTRIES[0], GAP, ENTRIES[1]])
    mem = tables()
    res = gap_unit.run_cases(
        mem,
        [
            {"routine": "mf_rank", "stack": [0x1111, 0x2222, 0x3333]},
        ],
    )[0]
    assert res.stub == "_mf_t_rank"
    begin, end = res.stack[1], res.stack[2]
    assert end - begin == 8 * (FIRST + 2), "the pairs cover the listed machines only"
    # XSlice at position 7 (the end), Pluck at 2: the gap type 8 appears nowhere
    expect = list(STOCK)
    expect.insert(min(7, len(expect)), FIRST)
    expect.insert(2, FIRST + 2)
    got = gap_unit.run_cases(
        mem,
        [
            {
                "routine": "mf_rank",
                "stack": [0, 0, 0],
                "reads": [(begin, 8 * len(expect))],
            },
        ],
    )[0]
    pairs = got.reads[0]
    assert [
        int.from_bytes(pairs[8 * i : 8 * i + 4], "big") for i in range(len(expect))
    ] == expect
    assert FIRST + 1 not in expect


# site -> (type register, stub that stands for "equal", for "not equal"); stubs named `_mf_cN` are the site address,
# so `_mf_cN + k` is the end of the site
CLONE_SITES = {
    "mf_clone0": (0, "_mf_c0a", "_mf_c0"),
    "mf_clone1": (0, "_mf_c1", "_mf_c1a"),
    "mf_clone2": (0, "_mf_c2", "_mf_c2a"),
    "mf_clone3": (0, "_mf_c3", "_mf_c3a"),
    "mf_clone4": (2, "_mf_c4", "_mf_c4a"),
    "mf_clone6": (0, "_mf_c6", "_mf_c6a"),
}
OBJ6 = 0x47EC5000


def test_clone_tests_take_the_stock_branch_and_also_for_added_clones_of_slice(unit):
    mem = tables()
    types_added = [FIRST + k for k in range(COUNT)]
    cases, want = [], []
    for name, (reg, eq, neq) in CLONE_SITES.items():
        for t in [6, 5, *types_added, FIRST + COUNT, 255]:
            d = {i: 0x1000 + i for i in range(8)}
            d[reg] = t
            if name == "mf_clone4":
                d[0] = 0x77  # the stock prefix loads 6 into d0
            if name == "mf_clone3":
                d[6] = 6
            a = {i: 0x2000 + i for i in range(7)}
            m = dict(mem)
            if name == "mf_clone6":
                a[0] = OBJ6 - 0x990
                m[OBJ6] = bytes([t])
            clone = (
                ENTRIES[t - FIRST]["clone_of"] if FIRST <= t < FIRST + COUNT else None
            )
            equal = t == 6 or clone == 6
            cases.append((m, {"routine": name, "d": d, "a": a, "stack": [0x11, 0x22]}))
            want.append((name, t, eq if equal else neq, d))
    for (m, case), (name, t, stub, _d) in zip(cases, want, strict=True):
        r = unit.run_cases(m, [case])[0]
        assert r.stub == stub, (name, t)
        exp = dict(enumerate(case["d"][i] for i in range(8)))
        if name in ("mf_clone0", "mf_clone1", "mf_clone2"):
            exp[1] = 6
        if name == "mf_clone4":
            exp[0] = 6
        if name == "mf_clone6":
            exp[1] = 6
            exp[0] = t if t < 128 else t - 256
            exp[0] &= 0xFFFFFFFF
        assert r.d == [exp[i] for i in range(8)], (name, t, r.d)
        assert r.a[1:7] == [0x2000 + i for i in range(1, 7)]
        assert r.a[0] == case["a"][0]
        if (
            name == "mf_clone3"
        ):  # addq.l #4,sp ran: the stack words are seen from 4 bytes up
            assert r.stack[:1] == [0x22]


def test_masked_clone_test_maps_added_types_to_their_clone(unit):
    mem = tables()
    ts = [0, 4, 6, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT, 77]
    res = unit.run_cases(
        mem,
        [{"routine": "mf_clone5", "d": {0: t, 1: 9, 3: 33}, "a": {0: 5}} for t in ts],
    )
    for t, r in zip(ts, res, strict=True):
        want = ENTRIES[t - FIRST]["clone_of"] if FIRST <= t < FIRST + COUNT else t
        assert r.stub == "_mf_t_clone5" and r.d[0] == want
        assert r.d[1] == 9 and r.d[3] == 33 and r.a[0] == 5


SLOTMAP = [8, 12, 16, 20, None, None, 24, 28, 32, 36]


def test_typearg_answers_fields_clone_types_and_hands_the_rest_on(unit):
    mem = tables()
    cases, want = [], []
    for k in range(COUNT):
        for idx in range(20, 40):
            stack = [0] * 4 + [FIRST + k, 0xBBBB]
            cases.append(
                {
                    "routine": "mf_typearg",
                    "d": {1: 0x77, 2: 0x88},
                    "a": {0: idx, 2: 0x99},
                    "stack": stack,
                }
            )
            want.append((k, idx, stack))
    for t in (0, 6, FIRST + COUNT):
        cases.append(
            {
                "routine": "mf_typearg",
                "d": {1: 0x77, 2: 0x88},
                "a": {0: 27, 2: 0x99},
                "stack": [0] * 4 + [t, 0xBBBB],
            }
        )
    res = unit.run_cases(mem, cases)
    for (k, idx, _stack), r in zip(want, res, strict=False):
        e = ENTRIES[k]
        assert r.d[1] == 0x77 and r.d[2] == 0x88 and r.a[0] == idx and r.a[2] == 0x99
        if 25 <= idx <= 34 and SLOTMAP[idx - 25] is not None:
            assert r.stub == TYPEARG_EXIT
            assert r.d[0] == e["f"][(SLOTMAP[idx - 25] - 8) // 4]
        elif 25 <= idx <= 34:  # slots 29 and 30: the clone's type replaces the argument
            assert r.stub == "_mf_t_typearg" and r.stack[4] == e["clone_of"]
        else:
            assert r.stub == "_mf_t_typearg" and r.stack[4] == FIRST + k
    for r in res[len(want) :]:
        assert r.stub == "_mf_t_typearg"


def test_stock_table_copies_append_the_clones_row():
    """module.toml's [[section]] rows against machines.rs: the clone's name hint, permission mask, per-type byte and
    DSP selector word."""
    import tomllib

    from tools.patchkit.pk_expr import evaluate

    m = tomllib.loads(
        (
            ROOT / "modules" / "core" / "adapters" / "machines" / "module.toml"
        ).read_text()
    )
    secs = {s["name"]: s for s in m["section"]}
    # a fake image: the anchors' tables
    anchors = {
        "cf.machine.data.name.table": 0x1000,
        "cf.machine.data.permit.table": 0x2000,
        "cf.machine.data.pertype.table": 0x3000,
        "dsp.machine.data.selector.table": 0x4000,
        "cf.machine.fn.dispatch": 0x9000,
    }
    img = {}
    for t in range(FIRST):
        img[0x1000 + 12 * t + 8] = be32(0xA0 + t)
        img[0x2000 + 4 * t] = be32(0xB0 + t)
        img[0x3000 + t] = bytes([0xC0 + t])
        img[0x4000 + 4 * t] = be32(0xD0 + t)

    img[0x9001] = bytes([FIRST - 1])  # the dispatch's `moveq #last,D1`

    def image(a, n):
        base = max(k for k in img if k <= a)
        return (img[base] + b"\0" * 8)[a - base : a - base + n]

    syms = {**anchors, "machines.first": FIRST, "machines.count": COUNT}
    for e in ENTRIES:
        f = {"clone_of": e["clone_of"], "name": 0x500, "short": 0x600}
        c = e["clone_of"]
        assert [
            evaluate(x, syms, f, image) for x in secs["mf_names"]["append"]["row"]
        ] == [0x500, 0x600, 0xA0 + c]
        assert (
            evaluate(secs["mf_permit"]["append"]["row"][0], syms, f, image) == 0xB0 + c
        )
        assert (
            evaluate(secs["mf_pertype"]["append"]["row"][0], syms, f, image) == 0xC0 + c
        )
        assert (
            evaluate(secs["mf_selector"]["append"]["row"][0], syms, f, image)
            == 0xD0 + c
        )
    assert evaluate(secs["mf_names"]["from"]["len"], syms, image=image) == FIRST * 12


def test_row_size_of_the_manifest_matches_the_assembly():
    """Every registry field is one 32-bit word (u32 or cstr pointer): the asm hardcodes the offsets."""
    import tomllib

    m = tomllib.loads(
        (
            ROOT / "modules" / "core" / "adapters" / "machines" / "module.toml"
        ).read_text()
    )
    fields = m["registry"][0]["field"]
    assert all(f["kind"] in ("u32", "cstr") for f in fields)
    assert len(fields) * 4 == ROWSIZE
    names = [f["name"] for f in fields]
    assert names.index("clone_of") * 4 == 44 and names.index("position") * 4 == 48
    assert names.index("own_dsp") * 4 == 52 and names.index("name") * 4 == 56
    assert names.index("short") * 4 == 60 and names.index("f0") * 4 == 8


def type_fn(t):
    return {TYPE_FN: b"\x20\x3c" + be32(t & 0xFFFFFFFF) + b"\x4e\x75"}


VIEW = 0x47EC5000  # a stand-in SourcePageView: +0x7c begin, +0x80 end of the page vector, +0x90 the page index
PAGES = VIEW + 0x200  # the vector's storage: two 4-byte entries


def view_mem(end_entries, idx):
    mem = bytearray(0x100)
    mem[0x7C:0x80] = be32(PAGES)
    mem[0x80:0x84] = be32(PAGES + 4 * end_entries)
    mem[0x84:0x88] = be32(PAGES + 8)
    mem[0x90:0x94] = be32(idx)
    return {VIEW: bytes(mem)}


def run_src_page(unit, typ, end_entries, idx):
    r = unit.run_cases(
        {**type_fn(typ), **view_mem(end_entries, idx)},
        [
            {
                "routine": "mf_srcpage",
                "a": {2: VIEW},
                "stack": [0xAAAA, 0xBBBB],
                "reads": [(VIEW + 0x7C, 0x18)],
            }
        ],
    )[0]
    blob = r.reads[0]
    begin, end, cap = (int.from_bytes(blob[i : i + 4], "big") for i in (0, 4, 8))
    idx_after = int.from_bytes(blob[0x14:0x18], "big")
    assert begin == PAGES and cap == PAGES + 8  # begin and capacity are never touched
    assert r.a[2] == VIEW  # the view register is kept
    assert r.stack[0] == 0xAAAA  # nothing is left on the stack
    return r.stub, (end - begin) // 4, idx_after


def test_src_page_hook_gives_added_types_one_page_and_stock_types_two(unit):
    """SourcePageView::vfunc_4, first test `tst.l 0x90(A2); beq.w T`: an added type (>= machines.first) gets page 0 and a one-entry
    page vector (no page dots, the page key has nothing to flip to) and takes the knob page; every other type, including -1, gets the
    two-entry vector back and the stock path (page 1 = the sample view stays)."""
    for typ in (FIRST, 9, 18, 23):  # the first added type, the pluck, run-time rows
        for end_entries, idx in ((2, 0), (2, 1), (1, 0)):
            assert run_src_page(unit, typ, end_entries, idx) == ("_mf_spa", 1, 0), (
                typ,
                end_entries,
                idx,
            )
    for typ in (0, 3, 6, -1):  # SAMPLE, STRETCH, MANUAL SLICE, no track
        # page 0: the knob page; page 1: the sample view (stock continues at site + 8); a vector cut by an added type is restored
        assert run_src_page(unit, typ, 2, 0) == ("_mf_spa", 2, 0), typ
        assert run_src_page(unit, typ, 2, 1) == ("_mf_sp", 2, 1), typ
        assert run_src_page(unit, typ, 1, 0) == ("_mf_spa", 2, 0), typ
