"""The runtime guard of core/dsp-voice (docs/findings/51-pluck-tune-guard.md) on the concrete SHARC+ core.

The wrapper and a fixture routine are assembled from the module sources and run the way the engine calls them
(tests/support/dsp_voice_rig.py). Fixtures (tests/fixtures/modules/dyn-guard-nan/src/dsp): `clean` (a ramp, inside the bound), `nan_legacy` (NaN while
the voice is active), `loud` (100.0 while active). The guard clamps the machine knob words to the row's declared ranges for the call,
puts the engine's words back, checks the block (finite, at most 8.0 in magnitude), and on a fault zeroes the block, ends the voice,
counts the fault in the row and the diagnostic words, and after 8 faults in 1500 blocks disables the type. Skips when Selache is not built."""

import struct

import numpy as np
import pytest
from support.dsp_voice_rig import (
    DIAG,
    FRAME,
    GUARD,
    REC,
    RECORDS,
    RETURN_SW,
    ROOT,
    ROW,
    STATE,
    TABLE,
    TYPE,
    Rig,
    nw,
    stops_at,
)

from tools.patchkit import pk_sharc, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

FIX = ROOT / "tests" / "fixtures" / "modules" / "dyn-guard-nan" / "src" / "dsp"
BASE = 0xB93800
CL_SEEN = STATE  # the clean fixture's copy of the knob words it saw
RAMP = np.arange(32, dtype=np.float32) * np.float32(0.125)
D_FAULTS, D_TYPEVOICE, D_PARAMS, D_DISABLED = (
    15,
    16,
    17,
    21,
)  # words of _dv_diag (dl_diag words 17, 18, 19 to 22, 23)


def val(x: int) -> float:
    return struct.unpack("<f", struct.pack("<I", x))[0]


class GuardRig(Rig):
    """The wrapper around one of the fixture routines."""

    def __init__(self, fixture: str, sym: str) -> None:
        super().__init__()
        obj = pk_sharc.assemble((FIX / f"{fixture}.s").read_text())
        core = self.core_linked.symbols
        ext = {
            "_dv_frame_hw": core["_dv_frame_hw"],
            "_dv_exp2": core["_dv_exp2"],
            "_cl_seen": CL_SEEN,
        }
        self.sine_linked = pk_sharc.link(obj, BASE, ext)
        self.render_pc = self.sine_linked.symbols[sym]
        self.reset()

    def ranges(self, **knobs: tuple[int, int]) -> None:
        """Declare the range of knob j (`k1=(lo, hi)`) in the registered type's row."""
        for name, (lo, hi) in knobs.items():
            self.dm[nw(TABLE) + 2 + int(name[1:])] = lo | (hi << 16)

    def knobs(self, track: int, words: list[int]) -> None:
        for j, w in enumerate(words):
            self.hw(0x6D + 0x30 * track + j + 2 * (j // 4), w)

    def arm(self, track: int = 0) -> None:
        self.rec_word(2 * track, 0x1B8, 1)

    def step(self, track: int = 0, blocks_before: int = 0) -> tuple[np.ndarray, dict]:
        """One call of the even voice of `track`; the state the next call needs is carried over."""
        v = 2 * track
        if blocks_before:
            self.dm[nw(DIAG)] = self.dm.get(nw(DIAG), 0) + blocks_before
        out = self.call(
            v,
            extra_reads={
                "diag": (nw(DIAG), 32),
                "row": (nw(TABLE), ROW),
                "g": (nw(GUARD), 32),
                "frame": (nw(FRAME), 256),
                "flags": (nw(self.rec(v) + 0x1B8), 1),
                "seen": (nw(CL_SEEN), 8),
            },
            max_steps=60000,
            ctx="friendly",
        )
        assert stops_at(out, RETURN_SW), out["halt"]
        for key, base in (
            ("diag", DIAG),
            ("row", TABLE),
            ("g", GUARD),
            ("frame", FRAME),
        ):
            for k, w in enumerate(out[key]):
                self.dm[nw(base) + k] = w
        self.dm[nw(self.rec(v) + 0x1B8)] = out["flags"][0]
        return np.array([val(w) for w in out["out"]], dtype=np.float32), out

    @staticmethod
    def rec(v: int) -> int:
        return RECORDS + v * REC


@pytest.fixture(scope="module")
def clean_cls():
    return lambda: GuardRig("clean", "_cl_render")


def test_a_clean_routine_is_untouched(clean_cls):
    rig = clean_cls()
    words = [0x4000, 0x1000, 0x2000, 0x1234, 0x7F00, 0x0, 0x0ABC, 0x7F00]
    rig.set_type(0, TYPE)
    rig.knobs(0, words)
    rig.arm()
    frame0 = [rig.dm.get(nw(FRAME) + k, 0) for k in range(256)]
    y, out = rig.step()
    assert np.array_equal(y, RAMP)
    assert out["seen"] == words, "in range: the routine sees the engine's words"
    assert out["frame"] == frame0, "the frame copy is as the engine left it"
    assert out["flags"][0] & 0xFFFF == 0x5A01, (
        "ACTIVE and the mark are the routine's own"
    )
    assert out["diag"][D_FAULTS] == 0 and out["diag"][D_TYPEVOICE] == 0
    assert out["g"][6] == 0, "no word was changed"


def test_the_knob_words_are_clamped_to_the_declared_ranges_for_the_call_only(clean_cls):
    rig = clean_cls()
    rig.set_type(3, TYPE)
    # knob 1 declares 0x1000 .. 0x2000, knob 5 1 .. 5; the others are undeclared (0 .. 0x7f00: 0x2001 passes)
    rig.ranges(k1=(0x1000, 0x2000), k5=(1, 5))
    words = [0xFFFF, 0x0800, 0x2001, 0x0000, 0x8000, 0x0000, 0x7F01, 0x7F00]
    rig.knobs(3, words)
    rig.arm(3)
    frame0 = [rig.dm.get(nw(FRAME) + k, 0) for k in range(256)]
    y, out = rig.step(3)
    assert out["seen"] == [0x7F00, 0x1000, 0x2001, 0, 0x7F00, 1, 0x7F00, 0x7F00]
    assert out["frame"] == frame0, (
        "the engine's words are put back, neighbours untouched"
    )
    assert out["g"][6] == 0b01110011, "the knobs the clamp changed"
    assert np.array_equal(y, RAMP)


def test_a_nan_block_is_zeroed_the_voice_ends_and_the_fault_is_counted():
    rig = GuardRig("nan_legacy", "_gn_render")
    rig.set_type(0, TYPE)
    words = [0x4000, 0x1000, 0x2000, 0, 0x7000, 0x0100, 0, 0x7F00]
    rig.knobs(0, words)
    rig.arm()
    y, out = rig.step()
    assert not np.any(y) and np.all(np.isfinite(y))
    assert out["flags"][0] & 0xFFFF == 0, (
        "ACTIVE and the mark are clear: the voice ended"
    )
    d = out["diag"]
    assert d[D_FAULTS] & 0xFFFF == 1 and (d[D_FAULTS] >> 16) & 0xFF == 1, (
        "one fault, kind 1 (Inf or NaN)"
    )
    assert d[D_TYPEVOICE] == TYPE | (0 << 16)
    got = []
    for m in range(4):
        got += [d[D_PARAMS + m] & 0xFFFF, d[D_PARAMS + m] >> 16]
    assert got == words, "the 8 words of the faulting call"
    assert out["row"][13] == 1, "the type's own count"
    # an idle voice renders silence: no fault
    y2, out2 = rig.step()
    assert not np.any(y2) and out2["diag"][D_FAULTS] == d[D_FAULTS]


def test_an_overlevel_block_is_a_fault_of_kind_2():
    rig = GuardRig("loud", "_ld_render")
    rig.set_type(0, TYPE)
    rig.arm()
    y, out = rig.step()
    assert not np.any(y)
    assert (out["diag"][D_FAULTS] >> 16) & 0xFF == 2


def test_the_8th_fault_within_a_second_disables_the_type_and_a_slow_trickle_does_not():
    rig = GuardRig("nan_legacy", "_gn_render")
    rig.set_type(0, TYPE)
    # a fault every 1600 blocks (> 1500): the window restarts each time, the type stays on
    for _ in range(10):
        rig.arm()
        rig.step(blocks_before=1600)
    assert rig.dm[nw(TABLE) + 10] == 0 and (rig.dm[nw(DIAG) + D_FAULTS] >> 24) == 0
    assert rig.dm[nw(TABLE) + 13] == 10
    # eight in a row, one block apart
    for _ in range(8):
        rig.arm()
        y, out = rig.step(blocks_before=1)
        assert not np.any(y)
    d = out["diag"]
    assert out["row"][10] == TYPE, "the row names the disabled type"
    assert d[D_FAULTS] >> 24 == 1 and d[D_DISABLED] == TYPE
    faults = d[D_FAULTS] & 0xFFFF
    assert faults == 17, (
        "the last slow fault opened the window: the 8th in it is the 7th of the quick run"
    )
    # the disabled type is not called any more: a silent block, ACTIVE as the engine set it, no new fault
    rig.arm()
    y, out = rig.step(blocks_before=1)
    assert not np.any(y)
    assert out["flags"][0] & 0xFFFF == 1
    assert out["diag"][D_FAULTS] & 0xFFFF == faults
    # the odd voice copies the (silent) block
    out_odd = rig.call(1, ctx="friendly")
    assert not any(w for w in out_odd["out"])
