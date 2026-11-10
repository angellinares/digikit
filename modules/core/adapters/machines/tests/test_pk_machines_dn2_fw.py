"""core/machines (Digitone II) as a module: the ColdFire routines against a model of the Rust generator's behaviour
(a1cf1df^:native/patchkit/src/machines/dn2.rs, removed in a1cf1df). The routines touch no firmware code, so the DT2 1.17 ready state runs them.

Skips when the m68k toolchain or the DT2 1.17 image is absent."""

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
    / "dn2"
    / "machines.s"
)
ROWSIZE = 64
FIRST = 5
ROWS = 0x47EC0000
STOCK_LIST = 0x47EC1000
STR_AT = 0x47EC3000
OBJ = 0x47EC4000
RC = [0x1000 + 0x100 * i for i in range(9)]
RC_LEN = [4, 4, 6, 6, 6, 6, 6, 6, 6]
STOCK = [0, 1, 2, 3, 4]
PGROWS = 0x47EC5000
PGSTR = 0x47EC6000
PGSIZE = 84
# (machine type, name, title, nine codes, nine slots): FM Clone has two pages, Sine one, Mid Clone none
PAGES = [
    (
        FIRST,
        "PGA",
        "Pages Test",
        [0x141, 0x142, 0x143, 0x144, 0, 0, 0, 0, 10],
        [25, 26, 27, 30, 0, 0, 0, 0, 0],
    ),
    (
        FIRST,
        "PGB",
        "Pages Test",
        [0x145, 0x146, 0, 0, 0, 0, 0, 0, 10],
        [40, 41, 0, 0, 0, 0, 0, 0, 0],
    ),
    (
        FIRST + 1,
        "SIN",
        "Sine",
        [0xD4, 0, 0, 0, 0, 0, 0, 0, 10],
        [35, 0, 0, 0, 0, 0, 0, 0, 0],
    ),
]

STUBS = [
    "_mf_t_dispatch",
    "_mf_t_list",
    "_mf_t_group",
    "_mf_t_typemap0",
    "_mf_t_typemap1",
    "_mf_t_typemap2",
    ("_mf_ft", 6),
    ("_mf_rd", 8),
    ("_mf_ow", 6),
]

pytestmark = [
    pytest.mark.skipif(
        pk_toolchains.m68k_bin("gcc") is None, reason="m68k toolchain not installed"
    ),
    pytest.mark.skipif(
        not SYX.exists(), reason="firmware/Digitakt_II_OS1.17.syx not present"
    ),
]

# (name, short, clone_of, position, own_dsp)
ENTRIES = [
    ("FM Clone", "FMC", 0, 4, 0),
    ("Sine", "SIN", 0, 4, 1),
    ("Mid Clone", "MCL", 4, 255, 0),
]
COUNT = len(ENTRIES)


def tables():
    rows, strings = b"", b""
    for name, short, clone, pos, own in ENTRIES:
        n = STR_AT + len(strings)
        strings += name.encode() + b"\0"
        s = STR_AT + len(strings)
        strings += short.encode() + b"\0"
        rows += be32(0) + be32(0) + be32(0) * 8 + be32(10)
        rows += be32(clone) + be32(pos) + be32(own) + be32(n) + be32(s)
    assert len(rows) == ROWSIZE * COUNT
    return {
        ROWS: rows,
        STR_AT: strings,
        STOCK_LIST: b"".join(be32(t) for t in STOCK),
    }


@pytest.fixture(scope="module")
def unit():
    sym = {
        "_mf_first": FIRST,
        "_mf_count": COUNT,
        "_mf_entries": COUNT,
        "_mf_rows": ROWS,
        "_mf_list_stock": STOCK_LIST,
        "_mf_pgrows": PGROWS,
        "_mf_pgn": 0,
    }
    sym.update({f"_mf_rc{i}": a for i, a in enumerate(RC)})
    return pk_cfunit.CfUnit(
        SYX,
        [SRC, ROOT / "tests/fixtures/native-page-absent.s"],
        STUBS,
        sym,
        [
            "mf_dispatch",
            "mf_list_entry",
            "mf_group",
            "mf_typemap0",
            "mf_typemap1",
            "mf_typemap2",
            "mf_frametype",
            "mf_reader",
            "mf_owner",
        ],
    )


def test_dispatch_and_list(unit):
    mem = tables()
    res = unit.run_cases(
        mem,
        [
            {"routine": "mf_dispatch", "stack": [unit.rts, FIRST + k]}
            for k in range(COUNT)
        ]
        + [
            {"routine": "mf_dispatch", "stack": [unit.rts, t]}
            for t in (4, FIRST + COUNT)
        ]
        + [{"routine": "mf_list_entry"}],
    )
    for k in range(COUNT):
        assert res[k].stub == "rts" and res[k].d[0] == ROWS + k * ROWSIZE
    assert [r.stub for r in res[COUNT:]] == ["_mf_t_dispatch"] * 2 + ["_mf_t_list"]


def test_group_sends_added_types_to_the_group_one_path(unit):
    mem = tables()
    res = unit.run_cases(
        mem, [{"routine": "mf_group", "d": {0: t, 1: 5}} for t in range(0, 12)]
    )
    for t, r in enumerate(res):
        assert r.stub == "_mf_t_group"
        assert r.d[0] == (0 if FIRST <= t < FIRST + COUNT else t)


@pytest.mark.parametrize(
    "name,off", [("mf_typemap0", 4), ("mf_typemap1", 8), ("mf_typemap2", 0xC)]
)
def test_type_maps_replace_the_argument_by_clone_of(unit, name, off):
    mem = tables()
    ts = [0, 4, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT, 99]
    cases = []
    for t in ts:
        stack = [0xAAAA] * 8
        stack[off // 4] = t
        if name == "mf_typemap2":
            stack[2] = 9  # the slot argument, which the stock code has loaded into a0
        cases.append(
            {
                "routine": name,
                "d": {0: 7, 1: 0x64 if name == "mf_typemap2" else 8},
                "a": {0: 9},
                "stack": stack,
            }
        )
    for t, r in zip(ts, unit.run_cases(mem, cases), strict=True):
        want = ENTRIES[t - FIRST][2] if FIRST <= t < FIRST + COUNT else t
        assert r.stub == "_mf_t_" + name[3:]
        assert r.stack[off // 4] == want
        if (
            name == "mf_typemap2"
        ):  # d1 = 0x64 and a0 = the slot are set before this site and used after it
            assert r.d[1] == 0x64 and r.a[0] == 9
        keep = [0xAAAA] * 8
        if name == "mf_typemap2":
            keep[2] = 9
        assert [x for i, x in enumerate(r.stack[:8]) if i != off // 4] == [
            x for i, x in enumerate(keep) if i != off // 4
        ]


def test_frame_type_sends_clone_unless_own_dsp(unit):
    mem = tables()
    mem[OBJ + 0xD8C] = bytes([0])
    ts = [0, 3, 4, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT, 200]
    res = []
    for t in ts:
        m = dict(mem)
        m[OBJ + 0xD8C] = bytes([t])
        res.append(
            unit.run_cases(
                m,
                [
                    {
                        "routine": "mf_frametype",
                        "d": {0: 0x12345678, 1: 5, 2: 6},
                        "a": {0: OBJ, 1: 7},
                    }
                ],
            )[0]
        )
    for t, r in zip(ts, res, strict=True):
        if FIRST <= t < FIRST + COUNT:
            name, short, clone, pos, own = ENTRIES[t - FIRST]
            want = t if own else clone
        else:
            want = t if t < 128 else t - 256
        assert r.stub == "_mf_ft"
        assert r.d[0] & 0xFFFF == want & 0xFFFF
        assert r.d[1:3] == [5, 6] and r.a[1] == 7


def test_type_reader_maps_except_for_the_identity_callers(unit):
    cases, want = [], []
    for t in [0, 4, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT, -3]:
        mem_t = bytes([t & 0xFF])
        for caller in [0x777, *[RC[i] + RC_LEN[i] for i in range(9)], RC[0]]:
            cases.append(
                (
                    mem_t,
                    {
                        "routine": "mf_reader",
                        "d": {0: 1, 1: 2},
                        "a": {0: OBJ, 1: 0x55},
                        "stack": [0x99, caller],
                    },
                )
            )
            real = caller in [RC[i] + RC_LEN[i] for i in range(9)]
            w = ENTRIES[t - FIRST][2] if FIRST <= t < FIRST + COUNT and not real else t
            want.append(w & 0xFFFFFFFF)
    for (mem_t, case), w in zip(cases, want, strict=True):
        mem = tables()
        mem[OBJ + 0xDE] = mem_t
        r = unit.run_cases(mem, [case])[0]
        assert r.stub == "_mf_rd"
        assert r.d[0] == w, (mem_t, case["stack"], hex(r.d[0]), hex(w))
        assert r.a[1] == 0x55 and r.stack[:2] == [0x99, case["stack"][1]]


def test_owner_test_maps_d3(unit):
    for t in [0, 2, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT]:
        mem = tables()
        mem[OBJ + 0xDE] = bytes([t])
        r = unit.run_cases(
            mem,
            [
                {
                    "routine": "mf_owner",
                    "d": {0: 11, 2: 0x4242, 3: 9},
                    "a": {0: OBJ, 1: 3},
                    "stack": [1, 2],
                }
            ],
        )[0]
        want = ENTRIES[t - FIRST][2] if FIRST <= t < FIRST + COUNT else t
        assert r.stub == "_mf_ow" and r.d[3] == want
        assert r.d[0] == 11 and r.a[1] == 3
        assert r.stack[0] == 0x4242 and r.stack[1:3] == [1, 2]  # d2 pushed


# ---- own parameter pages (registry `pages`)


def page_tables():
    mem = tables()
    rows, strings = b"", b""
    for mach, name, title, codes, slots in PAGES:
        n = PGSTR + len(strings)
        strings += name.encode() + b"\0"
        t = PGSTR + len(strings)
        strings += title.encode() + b"\0"
        rows += be32(n) + be32(t) + b"".join(be32(c) for c in codes) + be32(mach)
        rows += b"".join(be32(x) for x in slots)
    assert len(rows) == PGSIZE * len(PAGES)
    mem[PGROWS] = rows
    mem[PGSTR] = strings
    return mem


@pytest.fixture(scope="module")
def unit_pg():
    sym = {
        "_mf_first": FIRST,
        "_mf_count": COUNT,
        "_mf_entries": COUNT,
        "_mf_rows": ROWS,
        "_mf_list_stock": STOCK_LIST,
        "_mf_pgrows": PGROWS,
        "_mf_pgn": len(PAGES),
    }
    sym.update({f"_mf_rc{i}": a for i, a in enumerate(RC)})
    return pk_cfunit.CfUnit(
        SYX,
        [SRC, ROOT / "tests/fixtures/native-page-absent.s"],
        STUBS,
        sym,
        ["mf_typemap0", "mf_typemap1", "mf_typemap2", "mf_reader"],
    )


def test_page_count(unit_pg):
    mem = page_tables()
    ts = [0, 4, FIRST, FIRST + 1, FIRST + 2, FIRST + COUNT]
    cases = [
        {"routine": "mf_typemap0", "d": {1: 8}, "a": {0: 9}, "stack": [unit_pg.rts, t]}
        for t in ts
    ]
    for t, r in zip(ts, unit_pg.run_cases(mem, cases), strict=True):
        own = {FIRST: 2, FIRST + 1: 1}.get(t)
        if own:
            assert r.stub == "rts" and r.d[0] == own
        else:  # no own pages: the clone mapping and the stock code
            want = ENTRIES[t - FIRST][2] if FIRST <= t < FIRST + COUNT else t
            assert r.stub == "_mf_t_typemap0" and r.stack[1] == want


def test_page_descriptor(unit_pg):
    mem = page_tables()
    cases = []
    for t, page in [
        (FIRST, 0),
        (FIRST, 1),
        (FIRST + 1, 0),
        (FIRST, 2),
        (FIRST, -1),
        (FIRST + 2, 0),
        (0, 0),
    ]:
        cases.append(
            {
                "routine": "mf_typemap1",
                "d": {2: 0xD2D2},
                "stack": [0xD2D2, unit_pg.rts, t, page & 0xFFFFFFFF],
                "reads": [(PGROWS, PGSIZE * len(PAGES))],
            }
        )
    res = unit_pg.run_cases(mem, cases)
    # in range: the descriptor is the row, returned by an rts with d2 restored from the stack
    for r, k in zip(res[:3], (0, 1, 2), strict=True):
        assert r.stub == "rts" and r.d[0] == PGROWS + k * PGSIZE and r.d[2] == 0xD2D2
    # out of range (and not own): the stock code answers (MIDI's row for a type above 4, the clone's page otherwise)
    assert [r.stub for r in res[3:]] == ["_mf_t_typemap1"] * 4
    assert (
        res[3].stack[2] == FIRST and res[4].stack[2] == FIRST
    )  # the own type stays above 4
    assert res[5].stack[2] == ENTRIES[2][2]  # Mid Clone: its clone_of
    # the rows are descriptors: the strings became std::string reps (length, capacity, -1 before the characters)
    rows = res[0].reads[0]
    for k, (_m, name, title, codes, _s) in enumerate(PAGES):
        w = [
            int.from_bytes(rows[PGSIZE * k + 4 * i : PGSIZE * k + 4 * i + 4], "big")
            for i in range(11)
        ]
        assert w[2:11] == codes
        for ptr, text in ((w[0], name), (w[1], title)):
            hdr = unit_pg.run_cases(
                mem,
                [
                    {
                        "routine": "mf_typemap0",
                        "stack": [unit_pg.rts, FIRST],
                        "reads": [(ptr - 12, 12 + len(text) + 1)],
                    }
                ],
            )[0].reads[0]
            assert [int.from_bytes(hdr[i : i + 4], "big") for i in (0, 4, 8)] == [
                len(text),
                len(text),
                0xFFFFFFFF,
            ]
            assert hdr[12:] == text.encode() + b"\0"


def test_slot_to_code(unit_pg):
    mem = page_tables()
    cases = []
    probes = [
        (FIRST, 25),
        (FIRST, 27),
        (FIRST, 30),
        (FIRST, 41),
        (FIRST, 28),
        (FIRST, 64),
        (FIRST + 1, 35),
        (FIRST + 1, 25),
        (FIRST + 2, 25),
    ]
    for t, slot in probes:
        cases.append(
            {
                "routine": "mf_typemap2",
                "d": {2: 0xD2D2},
                "a": {0: slot},
                "stack": [0xD2D2, unit_pg.rts, slot, t, 3],
            }
        )
    # a slot outside 25..64 of a type with pages: the stock code (it ignores the type there)
    for slot in (0x10, 0x42, 65):
        cases.append(
            {
                "routine": "mf_typemap2",
                "a": {0: slot},
                "stack": [0xD2D2, unit_pg.rts, slot, FIRST, 3],
            }
        )
    res = unit_pg.run_cases(mem, cases)
    want = [0x141, 0x143, 0x144, 0x146, 0, 0, 0xD4, 0, None]
    for r, w, (t, slot) in zip(res[:9], want, probes, strict=True):
        if w is None:  # Mid Clone has no page: clone_of mapped, a0 kept
            assert (
                r.stub == "_mf_t_typemap2"
                and r.stack[3] == ENTRIES[2][2]
                and r.a[0] == slot
            )
        else:
            assert r.stub == "rts" and r.d[0] == w and r.d[2] == 0xD2D2, (t, slot)
    for r, slot in zip(res[9:], (0x10, 0x42, 65), strict=True):
        assert (
            r.stub == "_mf_t_typemap2"
            and r.a[0] == slot
            and r.stack[3] == FIRST
            and r.d[1] == 0x64
        )


def test_reader_gives_pages_machines_their_own_type(unit_pg):
    mem = page_tables()
    for t, want in [
        (0, 0),
        (4, 4),
        (FIRST, FIRST),
        (FIRST + 1, FIRST + 1),
        (FIRST + 2, ENTRIES[2][2]),
    ]:
        m = dict(mem)
        m[OBJ + 0xDE] = bytes([t])
        r = unit_pg.run_cases(
            m,
            [
                {
                    "routine": "mf_reader",
                    "d": {1: 2},
                    "a": {0: OBJ, 1: 0x55},
                    "stack": [0x99, 0x777],
                }
            ],
        )[0]
        assert r.stub == "_mf_rd" and r.d[0] == want and r.a[1] == 0x55
