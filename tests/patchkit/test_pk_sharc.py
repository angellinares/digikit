"""SHARC+ patchkit toolchain: a module source built by `patchkit sharc` (Selache as a library, our decoder re-decodes) runs on the core.

The guards, the fix-ups and the decoder checks of the build are tested in packages/patchkit/src/build/sharc_obj.rs. Skips when Selache's
C compiler is not built (uv run python -m tools.patchkit.pk_toolchains selache)."""

import struct

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.patchkit import (  # noqa: E402
    pk_sharc,  # noqa: E402
    pk_toolchains,  # noqa: E402
)

SRC = ROOT / "tests" / "patchkit" / "pk_src"
BASE = 0xB90000  # short-word (PC) address in L2 code space
BUF = 0x08020000  # normal-word DM address in L2
HOOK = 0xB9F000
RET = 0xB9E000
FRAME = 0x08030100  # a C frame: [FRAME] = caller frame pointer, [FRAME-1] = return address - 1


def bits(x: float) -> int:
    return struct.unpack("<I", struct.pack("<f", x))[0]


def val(x: int) -> float:
    return struct.unpack("<f", struct.pack("<I", x))[0]


def test_scale_buffer_runs_and_every_word_redecodes():
    obj, linked = pk_sharc.build(
        (SRC / "scale.s").read_text(), BASE, {"_fw_hook": HOOK}
    )
    assert linked.checked == len(obj.rows) == 12
    vals = [1.0, 2.0, -3.0, 0.5]
    out = pk_sharc.run_call(
        linked,
        "_scale_buf",
        regs={"R4": BUF, "R8": 4},
        dm_words={BUF + i: bits(v) for i, v in enumerate(vals)},
        reads={"out": (BUF, 5)},
        return_sw=HOOK,
    )
    assert out["halt"]["reason"] == "breakpoint" and out["halt"]["pc_sw"] == HOOK
    assert [val(w) for w in out["out"][:4]] == [v * 2.5 for v in vals]
    assert out["out"][4] is None  # one past the buffer untouched
    assert out["regs"]["R0"] == 7  # the local call `_bias` returned


# --- selcc evaluation: its output decodes and runs correctly on the core (byte-address pointers) ---


@pytest.fixture(scope="module")
def selcc_module():
    if pk_toolchains.selache_dir() is None:
        pytest.skip("selache not built (tools/patchkit/pk_toolchains.py selache)")
    if pk_toolchains.selcc_bin() is None:
        pytest.skip("selcc not built (not part of pk_toolchains selache)")
    return pk_sharc.link(pk_sharc.compile_c((SRC / "scale_c.c").read_text()), BASE)


def c_call(linked, entry, regs, dm=None, reads=None):
    mem = {FRAME: 0, FRAME - 1: RET - 1, **(dm or {})}
    regs = {**regs, "I6": FRAME, "I7": FRAME - 2}
    return pk_sharc.run_call(
        linked,
        entry,
        regs=regs,
        dm_words=mem,
        reads=reads,
        return_sw=RET,
        max_steps=20000,
    )


def s32(x):
    return x - (1 << 32) if x & 0x80000000 else x


def test_selcc_integer_functions(selcc_module):
    assert (
        s32(c_call(selcc_module, "add3.", {"R4": 10, "R8": 3, "R12": 7})["regs"]["R0"])
        == 31
    )
    byte_buf = 0x20080000
    r = c_call(
        selcc_module,
        "sum.",
        {"R4": byte_buf, "R8": 5},
        {byte_buf + 4 * i: v & 0xFFFFFFFF for i, v in enumerate([1, 2, 3, 40, -5])},
    )
    assert s32(r["regs"]["R0"]) == 41
    for x, want in ((0, 11), (1, 22), (2, 33), (5, 44), (9, -1)):
        assert s32(c_call(selcc_module, "sw.", {"R4": x})["regs"]["R0"]) == want
    for x in (0, 0xF0F0, 0xFFFFFFFF):
        assert c_call(selcc_module, "popc.", {"R4": x})["regs"]["R0"] == bin(x).count(
            "1"
        )


def test_selcc_float_loop(selcc_module):
    byte_buf = 0x20080000
    vals = [1.0, 2.0, -3.0, 0.5]
    r = c_call(
        selcc_module,
        "scale.",
        {"R4": byte_buf, "R8": 4, "R12": bits(1.5)},
        {byte_buf + 4 * i: bits(v) for i, v in enumerate(vals)},
        {f"o{i}": (byte_buf + 4 * i, 1) for i in range(4)},
    )
    assert [val(r[f"o{i}"][0]) for i in range(4)] == [v * 1.5 for v in vals]


@pytest.mark.parametrize("image", ["dt2-1.17", "dn2-1.12"])
def test_every_stock_shift_immediate_passes_the_check(image, tmp_path):
    """The shift-immediate rule of the build (`patchkit sharc shift-check`) accepts every 48-bit shift instruction in the functions of the
    stock image."""
    import sqlite3

    db = ROOT / "out" / "sharcdb" / f"{image}.sqlite"
    if not db.is_file():
        pytest.skip(f"no {db} (tools/sharc_dev/sharcdb build)")
    with sqlite3.connect(db) as c:
        rows = c.execute(
            "select sw, raw from insn where form in ('6a_mem', '6a_nomem', '6b_shiftimm')"
            " and aligned = 1 and function_sw is not null"
        ).fetchall()
    words = tmp_path / "words.txt"
    words.write_text("".join(f"{raw}\n" for _, raw in rows))
    verdicts = pk_sharc._sharc(["shift-check", str(words)]).splitlines()
    bad = [(hex(sw), v) for (sw, _), v in zip(rows, verdicts, strict=True) if v != "ok"]
    assert len(rows) > 1000 and bad == []
