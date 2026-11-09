"""core/dsp-voice and a raw sine routine (tests/support/raw_sine.s) on the concrete SHARC+ core.

The wrapper and the sine routine are assembled from the module sources, linked at test bases with the firmware
symbols stood in by test addresses, and run the way the engine calls them: R4 = voice record, R8 = output block, R14
= loop counter, a frame built by the caller's CJUMP. The stock path must fall through with nothing but the displaced
stack adjustment and R0-R2 / I4 changed; a registered type must run the routine (even voice) or copy its block (odd
voice) and leave the callee-saved registers as found. The emulator proof is tools/patchkit/pk_proof_voice.py. The machine of the SDK contract is tested in test_sine.py. Skips when
Selache is not built."""

import re
import struct

import numpy as np
import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

import random  # noqa: E402

from support.dsp_voice_rig import (  # noqa: E402
    CONTEXT_REGS,
    CORE,
    DIAG,
    M6_HOSTILE,
    M13_HOSTILE,
    MASK,
    OUT,
    RECORDS,
    RETURN_SW,
    RING_A,
    RING_B,
    STACK,
    STATE,
    STEP_WORD,
    STOCK_BP,
    TABLE,
    TYPE,
    Rig,
    draw_context,
    nw,
    stops_at,
)

from tools.patchkit import pk_sharc, pk_toolchains  # noqa: E402

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


def u32(x: int) -> int:
    return x & 0xFFFFFFFF


def context_intact(out) -> None:
    """Every context register leaves as it came in (M7 / M14 are the stock epilogue's and hold)."""
    for name in CONTEXT_REGS:
        assert u32(out["regs"][name]) == u32(out["in"][name]), (name, out["in"][name])


def bits(x: float) -> int:
    return struct.unpack("<I", struct.pack("<f", x))[0]


def val(x: int) -> float:
    return struct.unpack("<f", struct.pack("<I", x))[0]


@pytest.fixture(scope="module")
def rig_cls():
    return Rig


def test_every_word_redecodes(rig_cls):
    rig = rig_cls()
    assert rig.core_linked.checked > 120
    assert rig.sine_linked.checked > 90


def test_an_unregistered_type_falls_through_to_the_stock_renderer(rig_cls):
    rig = rig_cls()
    rig.set_type(2, 0)
    out = rig.call(5)
    assert stops_at(out, STOCK_BP), out["halt"]
    # only the displaced instruction's effect (I7 - 104 bytes) and R0-R2 / I4 differ; the callee-saved set is intact
    assert out["regs"]["I7"] == STACK - 104
    assert out["regs"]["I6"] == STACK
    # the context registers are the stock code's own: L4 (zeroed for the scan) is put back, nothing else was touched
    context_intact(out)
    for name in ("R3", "R5", "R15", "I0", "I5", "M1", "M3", "I8", "I9", "M8"):
        assert out["regs"][name] == out["in"][name], name
    assert out["regs"]["R4"] == out["in"]["R4"] and out["regs"]["R8"] == out["in"]["R8"]
    assert out["instructions"] < 60, (
        "a slot whose type no row registers does not walk the table"
    )


def test_with_no_registered_type_a_voice_costs_a_check_and_voice_0_keeps_the_heartbeat(
    rig_cls,
):
    rig = rig_cls()
    for k in range(3 * 24):
        rig.dm[nw(TABLE) + k] = (
            0xFFFFFFFF if k % 24 == 0 and k < 48 else 0
        )  # two free rows, then the end
    rig.set_type(2, TYPE)  # a type that would be registered if a row had it
    reads = {"diag": (nw(DIAG), 16), "mask": (nw(MASK), 1)}
    out = rig.call(5, extra_reads=reads)
    assert stops_at(out, STOCK_BP), out["halt"]
    assert out["regs"]["I7"] == STACK - 104 and out["regs"]["I6"] == STACK
    context_intact(out)
    for name in ("R3", "R5", "R15", "I0", "I5", "M1", "M3", "I8", "I9", "M8"):
        assert out["regs"][name] == out["in"][name], name
    assert out["instructions"] < 15
    assert not any(out["diag"]), "no beat and no breadcrumb from a voice other than 0"
    # voice 0: the heartbeat, and the mask of what is registered (free rows are no type)
    out = rig.call(0, extra_reads=reads)
    assert stops_at(out, STOCK_BP), out["halt"]
    assert out["diag"][0] == 1 and out["mask"][0] == 0
    rig.dm[nw(TABLE) + 2 * 24] = 31
    rig.dm[nw(TABLE) + 3 * 24] = 40
    rig.dm[nw(TABLE) + 4 * 24] = 0
    rig.dm[nw(TABLE) + 24] = 9  # a registered row after a free one
    out = rig.call(0, extra_reads=reads, mask=0)
    assert out["mask"][0] == 1 << 9 | 1 << 31, hex(out["mask"][0])


@pytest.mark.parametrize("track", [0, 1, 4, 7, 15])
def test_a_registered_type_selects_by_the_track_and_the_right_half_of_the_cache_word(
    rig_cls, track
):
    rig = rig_cls()
    # the neighbour in the same 32-bit cache word is another type: it must not be mistaken for ours
    rig.set_type(track ^ 1, TYPE + 1)
    rig.set_type(track, TYPE)
    rig.set_note(track, 60)
    v = 2 * track
    rig.rec_word(v, 0x1B8, 1)  # ACTIVE
    out = rig.call(v)
    assert stops_at(out, RETURN_SW), out["halt"]
    assert any(w not in (0, None) for w in out["out"])
    assert out["regs"]["I6"] == 0x20000FF0, "RFRAME restores the caller's frame pointer"
    assert out["regs"]["I7"] == STACK, "RFRAME restores I7"
    # the neighbour track's voice, with the other type, falls through
    rig2 = rig_cls()
    rig2.set_type(track, TYPE)
    rig2.set_type(track ^ 1, TYPE + 1)
    out2 = rig2.call(2 * (track ^ 1))
    assert stops_at(out2, STOCK_BP)


def test_the_callee_saved_registers_survive_a_rendered_voice(rig_cls):
    rig = rig_cls()
    rig.set_type(0, TYPE)
    rig.set_note(0, 60)
    rig.rec_word(0, 0x1B8, 1)
    out = rig.call(0)
    assert stops_at(out, RETURN_SW)
    for name in ("R3", "R5", "R15", "I0", "I5", "M1", "M3", "I8", "I9", "M8"):
        assert out["regs"][name] == out["in"][name], name
    context_intact(out)


def test_the_odd_voice_copies_the_even_voices_block(rig_cls):
    rig = rig_cls()
    rig.set_type(0, TYPE)
    rig.set_note(0, 60)
    rig.rec_word(0, 0x1B8, 1)
    rig.rec_word(1, 0x1B8, 1)
    even = rig.call(0)
    assert stops_at(even, RETURN_SW)
    # the block the even voice wrote is in the output page; the odd voice's call sees it in place
    for k, w in enumerate(even["out"]):
        rig.dm[nw(OUT) + k] = w
    odd = rig.call(1)
    assert stops_at(odd, RETURN_SW)
    assert odd["out"] == even["out"]


def voice_call(rig, v: int, ctx):
    """One registered voice with a note on; the odd voice sees the even voice's block in place."""
    t = v // 2
    rig.set_type(t, TYPE)
    rig.set_note(t, 60)
    rig.rec_word(v, 0x1B8, 1)
    if v % 2:
        for k in range(32):
            rig.dm[nw(OUT) + 32 * v + k] = 0
            rig.dm[nw(OUT) + 32 * (v - 1) + k] = 0x3F000000 + k
    return rig.call(v, ctx=ctx)


@pytest.mark.parametrize("v", [0, 1, 2, 3, 14, 15, 30, 31])
@pytest.mark.parametrize("m6", M6_HOSTILE)
@pytest.mark.parametrize("m13", M13_HOSTILE)
def test_the_machine_path_does_not_depend_on_m6_or_m13(rig_cls, v, m6, m13):
    """The hardware report's two causes: the odd-voice copy and the registry scan used M6, the machine call jumped by M13.
    Any M6 in {0x1b8, 4, 0, -1} and M13 in {4, -2, 0x10}: the same block as with the old friendly constants, and every
    context register intact afterwards."""
    ref = voice_call(rig_cls(), v, "friendly")
    assert stops_at(ref, RETURN_SW), ref["halt"]
    assert any(w not in (0, None) for w in ref["out"])
    out = voice_call(rig_cls(), v, {"M6": m6, "M13": m13})
    assert stops_at(out, RETURN_SW), out["halt"]
    assert out["out"] == ref["out"]
    context_intact(out)
    assert out["regs"]["I6"] == 0x20000FF0 and out["regs"]["I7"] == STACK


@pytest.mark.parametrize("seed", range(12))
def test_a_drawn_context_changes_nothing(rig_cls, seed):
    """Random contexts (M5, M6, M13, M15, L0-L5, MODE1 drawn from hostile values): the same block, the registers intact,
    the callee-saved set as found; the stock path too."""
    rng = random.Random(seed)
    for v in (2 * (seed % 16), 2 * (seed % 16) + 1):
        ctx = draw_context(rng)
        ref = voice_call(rig_cls(), v, "friendly")
        out = voice_call(rig_cls(), v, ctx)
        assert stops_at(out, RETURN_SW), (ctx, out["halt"])
        assert out["out"] == ref["out"], ctx
        context_intact(out)
        for name in ("R3", "R5", "R15", "I0", "I5", "M1", "M3", "I8", "I9", "M8"):
            assert out["regs"][name] == out["in"][name], name
        # the unregistered neighbour falls through with the context untouched
        stock = rig_cls()
        stock.set_type(v // 2, 0)
        so = stock.call(v, ctx=ctx)
        assert stops_at(so, STOCK_BP), (ctx, so["halt"])
        context_intact(so)


PROBE = """
.SECTION/PM seg_pmco;
.GLOBAL _probe;
_probe:
    I0 = 0x2009B000;
    R0 = M5;
    DM(0, I0) = R0;
    R0 = M6;
    DM(1, I0) = R0;
    R0 = M7;
    DM(2, I0) = R0;
    R0 = M13;
    DM(3, I0) = R0;
    R0 = M14;
    DM(4, I0) = R0;
    R0 = M15;
    DM(5, I0) = R0;
    R0 = L0;
    DM(6, I0) = R0;
    R0 = L1;
    DM(7, I0) = R0;
    R0 = L2;
    DM(8, I0) = R0;
    R0 = L3;
    DM(9, I0) = R0;
    R0 = L4;
    DM(10, I0) = R0;
    R0 = L5;
    DM(11, I0) = R0;
    R0 = MODE1;
    DM(12, I0) = R0;
    JUMP (M13, I13);
"""
PROBE_BASE = 0xB93800


class ProbeRig(Rig):
    """The wrapper around a routine that stores the context registers it is entered with."""

    def __init__(self) -> None:
        super().__init__()
        self.sine_linked = pk_sharc.link(pk_sharc.assemble(PROBE), PROBE_BASE, {})
        self.render_pc = self.sine_linked.symbols["_probe"]
        self.reset()


def test_the_context_is_pinned_for_the_routine():
    """The routine runs with M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1, the L registers 0 and MODE1 with SIMD,
    TRUNCATE and ALUSAT clear and RND32 set, whatever the context was (the ABI in the wrapper's header)."""
    rng = random.Random(7)
    for _ in range(8):
        rig = ProbeRig()
        rig.set_type(0, TYPE)
        rig.set_note(0, 60)
        rig.rec_word(0, 0x1B8, 1)
        ctx = draw_context(rng)
        out = rig.call(0, ctx=ctx, extra_reads={"seen": (nw(STATE), 13)})
        assert stops_at(out, RETURN_SW), (ctx, out["halt"])
        seen = out["seen"]
        assert seen[:6] == [0, 1, 0xFFFFFFFF, 0, 1, 0xFFFFFFFF], (ctx, seen)
        assert seen[6:12] == [0] * 6, (ctx, seen)
        assert seen[12] & 0x20A000 == 0 and seen[12] & 0x10000, (ctx, hex(seen[12]))
        context_intact(out)


CRUMB_PROBE = """
.SECTION/PM seg_pmco;
.GLOBAL _crumb_probe;
_crumb_probe:
    I0 = 0x2009B000;
    R0 = DM(0x2009D030);
    DM(0, I0) = R0;
    R0 = DM(0x200A0000 + 0x300);
    DM(1, I0) = R0;
    R0 = DM(0x200A2000 + 0x300);
    DM(2, I0) = R0;
    R0 = DM(0x2009D034);
    DM(3, I0) = R0;
    I0 = R8;
    R0 = 0;
    LCNTR = 32, DO _cp_z UNTIL LCE;
_cp_z: DM(I0, M6) = R0;
    JUMP (M13, I13);
"""
CRUMB_PROBE_BASE = 0xB93C00


class CrumbRig(Rig):
    def __init__(self, defines: frozenset[str] = frozenset()) -> None:
        super().__init__(defines=defines)
        self.sine_linked = pk_sharc.link(
            pk_sharc.assemble(CRUMB_PROBE), CRUMB_PROBE_BASE, {}
        )
        self.render_pc = self.sine_linked.symbols["_crumb_probe"]
        self.reset()


DIAG_CRUMB, DIAG_NDISP = nw(DIAG) + 12, nw(DIAG) + 13


def crumb(stage, v, t):
    return stage | v << 8 | t << 16


def test_breadcrumbs_name_each_step_and_reach_both_reply_records():
    """The wrapper writes stage | voice << 8 | type << 16 before each step (finding 48): into its own diagnostic word 12 and, at once,
    into both reply records with the header and the count of dispatches, so that a core that stops inside the routine leaves them."""
    for v in (0, 4, 30):
        rig = CrumbRig()
        rig.set_rings()
        t = v // 2
        rig.set_type(t, TYPE)
        rig.set_note(t, 60)
        rig.rec_word(v, 0x1B8, 1)
        ring_reads = {
            "ra": (nw(RING_A), 0x400 // 4),
            "rb": (nw(RING_B), 0x400 // 4),
            "diag": (nw(DIAG), 16),
        }
        out = rig.call(v, extra_reads=ring_reads, ctx="friendly")
        assert stops_at(out, RETURN_SW), out["halt"]
        seen = [out["diag"][12], out["diag"][13], out["diag"][14]]
        want = crumb(7, v, TYPE)  # the epilogue is the last step
        assert seen == [want, 1, 1], [hex(x) for x in seen]  # one dispatch, one return
        for ring in ("ra", "rb"):
            r = out[ring]
            assert r[192] == want and r[193] == 1 and r[213] == 1, (ring, v)
            assert r[45] == 0x20D1A6, (ring, v)  # the header: 0xD1A6, 32 words
            # nothing else in the record was written
            assert sum(1 for x in r if x) == 4, (ring, v)


def test_the_module_step_word_is_cleared_before_the_call_and_the_records_are_left_alone():
    """The module step is the static word `_dk_step` (its address is `_dv_rec[2]`, dsp-link.inc); the wrapper clears it right before it calls the routine,
    so a step copied by the block tail belongs to a call of this block. The reply records' word 298 is the tail's, the wrapper does not touch it."""
    rig = CrumbRig()
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    rig.rec_word(2, 0x1B8, 1)
    rig.dm[nw(RING_A) + 298] = 7
    rig.dm[nw(RING_B) + 298] = 9
    rig.dm[nw(STEP_WORD)] = 5
    reads = {
        "ra": (nw(RING_A) + 296, 4),
        "rb": (nw(RING_B) + 296, 4),
        "step": (nw(STEP_WORD), 1),
    }
    out = rig.call(2, extra_reads=reads, ctx="friendly")
    assert stops_at(out, RETURN_SW), out["halt"]
    assert out["step"] == [0], out["step"]
    assert out["ra"][2] == 7 and out["rb"][2] == 9, (out["ra"], out["rb"])


def test_the_routine_sees_the_called_breadcrumb():
    rig = CrumbRig()
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    rig.rec_word(2, 0x1B8, 1)
    out = rig.call(2, extra_reads={"seen": (nw(STATE), 4)}, ctx="friendly")
    assert stops_at(out, RETURN_SW), out["halt"]
    want = crumb(4, 2, TYPE)  # ROUTINE_CALLED: voice 2, the type in play
    assert out["seen"] == [want, want, want, 1], [hex(x) for x in out["seen"]]


def test_the_odd_voice_counts_no_dispatch_and_ends_at_the_epilogue():
    rig = CrumbRig()
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    out = rig.call(3, extra_reads={"diag": (nw(DIAG), 16)}, ctx="friendly")
    assert stops_at(out, RETURN_SW), out["halt"]
    assert out["diag"][12] == crumb(7, 3, TYPE) and out["diag"][13:15] == [0, 0]


def test_the_stock_path_leaves_one_breadcrumb_and_an_unregistered_added_type_two():
    rig = CrumbRig()
    rig.set_rings()
    rig.set_type(2, 0)
    reads = {"diag": (nw(DIAG), 16), "ra": (nw(RING_A), 0x400 // 4)}
    out = rig.call(5, extra_reads=reads, ctx="friendly")
    assert stops_at(out, STOCK_BP), out["halt"]
    assert out["diag"][12] == crumb(1, 5, 0) and out["diag"][13] == 0
    assert not any(out["ra"]), "a stock type writes nothing into the reply records"
    # type 12: no module registered it (the table holds TYPE only); the stock path runs, with a breadcrumb that says so
    rig.set_type(2, 12)
    out = rig.call(5, extra_reads=reads, ctx="friendly")
    assert stops_at(out, STOCK_BP), out["halt"]
    assert out["diag"][12] == crumb(3, 5, 12) and out["ra"][192] == crumb(3, 5, 12)
    assert out["regs"]["R3"] == out["in"]["R3"] and out["regs"]["R5"] == out["in"]["R5"]
    # a type below 7 that is not registered either: the engine's own, no stage 3
    rig.set_type(2, 6)
    out = rig.call(5, extra_reads=reads, ctx="friendly")
    assert out["diag"][12] == crumb(1, 5, 0)


def test_the_bisect_build_takes_the_stock_path_for_added_types_with_no_call():
    """DV_BISECT_C2 (finding 49, docs/PATCHING.md "Rung C2"): an added type, registered or not, runs the stock renderer with the stage 3
    breadcrumb written inline, the same words as the stock build's for an unregistered type. A breakpoint on `_dv_crumb` proves that the
    bisect build never enters it (so no CALL and no RTS run on this path), and a registered type is not dispatched (the dispatch counter counts the stage 3 passes in this build, the returns stay 0)."""
    reads = {"diag": (nw(DIAG), 16), "ra": (nw(RING_A), 0x400 // 4)}
    bisect = frozenset({"DV_BISECT_C2"})
    for t in (12, TYPE):  # 12: no module registered it; TYPE: the table holds it
        rig = CrumbRig(bisect)
        rig.set_rings()
        rig.set_type(2, t)
        crumb_sw = rig.core_linked.symbols["_dv_crumb"]
        out = rig.call(5, extra_reads=reads, ctx="friendly", breaks=(crumb_sw,))
        assert stops_at(out, STOCK_BP), (t, out["halt"])
        # the dispatch counter counts stage 3 passes in this build; the routine is never called, so no return
        assert out["diag"][12] == crumb(3, 5, t) and out["diag"][13:15] == [1, 0]
        assert out["ra"][192] == crumb(3, 5, t) and out["ra"][193] == 1
        assert (
            out["regs"]["R3"] == out["in"]["R3"]
            and out["regs"]["R5"] == out["in"]["R5"]
        )
    # the stock build writes the same words for the unregistered type
    rig = CrumbRig()
    rig.set_rings()
    rig.set_type(2, 12)
    ref = rig.call(5, extra_reads=reads, ctx="friendly")
    assert ref["diag"][12] == crumb(3, 5, 12) and ref["ra"][192] == crumb(3, 5, 12)
    # a type below 7 is the engine's own in both builds: no breadcrumb beyond stage 1
    rig = CrumbRig(bisect)
    rig.set_rings()
    rig.set_type(2, 6)
    out = rig.call(5, extra_reads=reads, ctx="friendly")
    assert out["diag"][12] == crumb(1, 5, 0)


C3_COMMON = ("DV_C3_FINE",)
C3_VARIANTS = {
    "a-delayed": frozenset({*C3_COMMON, "DV_C3_DB"}),
    "b-noirq": frozenset({*C3_COMMON, "DV_C3_NOIRQ"}),
    "c-nosys": frozenset({*C3_COMMON, "DV_C3_NOSYS"}),
    # rung C3d (finding 49 section 10): the deep breadcrumbs 11 to 22 (the DAG1 register stores through R2 are the default build now)
    "d1-deep": frozenset({*C3_COMMON, "DV_C3_DEEP"}),
}


@pytest.mark.parametrize("name", sorted(C3_VARIANTS))
def test_the_c3_variants_keep_the_registered_path_working(name):
    """Rung C3 (finding 49 section 9, docs/PATCHING.md): each variant removes one suspect from the registered path and must still do what
    the rung C build does: dispatch the routine on the even voice, copy on the odd voice, end at the epilogue breadcrumb, leave the
    context as found. The fine breadcrumbs 8, 9 and 10 are written inline in that order before the CALL of stage 2."""
    defines = C3_VARIANTS[name]
    reads = {
        "diag": (nw(DIAG), 16),
        "ra": (nw(RING_A), 0x400 // 4),
        "rb": (nw(RING_B), 0x400 // 4),
    }
    for v in (0, 2, 3, 30):
        t = v // 2
        rig = CrumbRig(defines)
        rig.set_rings()
        rig.set_type(t, TYPE)
        rig.set_note(t, 60)
        rig.rec_word(v, 0x1B8, 1)
        out = rig.call(v, extra_reads=reads, ctx="friendly")
        assert stops_at(out, RETURN_SW), (name, v, out["halt"])
        want = crumb(7, v, TYPE)
        even = v % 2 == 0
        assert out["diag"][12:15] == [want, int(even), int(even)], (name, v)
        for ring in ("ra", "rb"):
            assert out[ring][192] == want and out[ring][45] == 0x20D1A6
        context_intact(out)
        assert (
            out["regs"]["I7"] == out["in"]["I6"]
        )  # RFRAME: I7 = I6, as the stock epilogue
    # the fine breadcrumbs, in order, at the label after each one
    rig = CrumbRig(defines)
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    rig.rec_word(2, 0x1B8, 1)
    for label, stage in (("_dv_f1b", 8), ("_dv_f2b", 9), ("_dv_f3b", 10)):
        out = rig.call(
            2,
            extra_reads=reads,
            ctx="friendly",
            breaks=(rig.core_linked.symbols[label],),
        )
        assert out["diag"][12] == crumb(stage, 2, TYPE), (
            name,
            label,
            hex(out["diag"][12]),
        )
        assert out["ra"][192] == crumb(stage, 2, TYPE) and out["rb"][192] == crumb(
            stage, 2, TYPE
        )
        # none of the fine breadcrumbs counts as a dispatch
        assert out["diag"][13:15] == [0, 0]


DEEP_STAGES = tuple(range(11, 23))


@pytest.mark.parametrize("name", ["d1-deep"])
def test_the_deep_breadcrumbs_come_in_order_inside_the_frame_save(name):
    """Rung C3d: twelve more inline breadcrumbs (stages 11 to 22) between the MODIFY of I7 and the save of MODE1, each at the label after
    it, none counted as a dispatch, none touching the registers the frame save has not stored yet (R0, R2, I4 only)."""
    defines = C3_VARIANTS[name]
    reads = {"diag": (nw(DIAG), 16), "ra": (nw(RING_A), 0x400 // 4)}
    rig = CrumbRig(defines)
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    rig.rec_word(2, 0x1B8, 1)
    for stage in DEEP_STAGES:
        out = rig.call(
            2,
            extra_reads=reads,
            ctx="friendly",
            breaks=(rig.core_linked.symbols[f"_dv_d{stage}b"],),
        )
        assert out["diag"][12] == crumb(stage, 2, TYPE), (
            name,
            stage,
            hex(out["diag"][12]),
        )
        assert out["ra"][192] == crumb(stage, 2, TYPE)
        assert out["diag"][13:15] == [0, 0]
    out = rig.call(2, extra_reads=reads, ctx="friendly")
    assert stops_at(out, RETURN_SW), out["halt"]
    context_intact(out)


INCLUDES = pk_sharc.sharc_include_dirs(
    CORE.parents[2]
)  # the adapter's includes and the SDK's


def test_no_build_of_the_module_stores_a_same_dag_register_through_an_index_register(
    tmp_path,
):
    """Finding 49 section 10: a store through DM(Ia) cannot take a DAG1 register (UREGXDAG1, PRM 15-7); the stock image has none. The
    frame save goes through R2 in every build, and a module build that has such a store fails (pk_sharc.sharc_object)."""

    def stores(defines):
        text = pk_sharc.apply_conditionals(CORE.read_text(), defines)
        return [
            ln.strip()
            for ln in text.splitlines()
            if re.fullmatch(r"\s*DM\(-?\d+, I6\) = [IMLB]([0-7])\b;.*", ln)
            and not ln.strip().startswith("//")
        ]

    for defines in (frozenset(), *C3_VARIANTS.values()):
        assert stores(defines) == []
        pk_sharc.sharc_object(
            [CORE], INCLUDES, defines
        )  # raises BuildError on an excluded store
    # the DN2 copy too
    dn2 = CORE.with_name("dsp-voice-dn2.s")
    pk_sharc.sharc_object([dn2], INCLUDES, frozenset())
    # and a source with one fails, with the parcel address in the message
    bad = tmp_path / "bad.s"
    bad.write_text(".SECTION/PM seg_pmco;\n_x:\n    DM(-16, I6) = I0;\n    RTS;\n")
    with pytest.raises(
        pk_sharc.PkSharcError, match=r"parcel 0x0: DM store of I0 \(DAG1\)"
    ):
        pk_sharc.sharc_object([bad], [], frozenset())


def test_the_c3_noirq_variant_masks_interrupts_inside_and_restores_mode1():
    defines = C3_VARIANTS["b-noirq"]
    rig = CrumbRig(defines)
    rig.set_rings()
    rig.set_type(1, TYPE)
    rig.set_note(1, 60)
    rig.rec_word(2, 0x1B8, 1)
    ctx = {
        "MODE1": 0x1011800 | 0x8000
    }  # IRPTEN set, TRUNCATE set: the pin clears it, the epilogue puts it back
    inside = rig.call(2, ctx=ctx, breaks=(rig.core_linked.symbols["_dv_f3b"],))
    assert inside["regs"]["MODE1"] & 0x1000 == 0, (
        "IRPTEN is clear once the registered path is past its mask"
    )
    assert inside["regs"]["MODE1"] & 0x8000 == 0, "the pins still apply"
    out = rig.call(2, ctx=ctx)
    assert stops_at(out, RETURN_SW), out["halt"]
    assert (
        u32(out["regs"]["MODE1"]) == u32(out["in"]["MODE1"])
        and out["regs"]["MODE1"] & 0x1000
    )
    # the stock path and the engine's own types never touch IRPTEN
    rig.set_type(2, 6)
    out = rig.call(5, ctx=ctx)
    assert stops_at(out, STOCK_BP) and u32(out["regs"]["MODE1"]) == u32(
        out["in"]["MODE1"]
    )


def test_the_c3_nosys_variant_writes_no_system_register_on_the_registered_path():
    text = pk_sharc.apply_conditionals(CORE.read_text(), C3_VARIANTS["c-nosys"])
    path = text.split("_dv_stock:")[0]
    assert (
        "MODE1 =" not in path
        and "BIT SET MODE1" not in path
        and "BIT CLR MODE1" not in path
    )
    assert "R3 = MODE1;" in path  # the read stays (harmless); only the writes are gone


def test_every_build_restores_b3_before_i3():
    """A load of a B register also loads its I register (PRM p.6-24, "Circular Buffer Programming Model"); the stock epilogue restores B3
    first, in every build (the emulator does not model the I load)."""

    def order(defines):
        lines = [
            x.strip()
            for x in pk_sharc.apply_conditionals(CORE.read_text(), defines).splitlines()
        ]

        def at(prefix):
            return next(k for k, x in enumerate(lines) if x.startswith(prefix))

        return at("B3 = DM(-24, I6);"), at("I3 = DM(-19, I6);")

    for defines in (C3_VARIANTS["a-delayed"], frozenset()):
        b, i = order(defines)
        assert b < i


def test_without_reply_records_the_breadcrumb_stays_local():
    rig = CrumbRig()  # core/dsp-link absent or before its first block: _dv_rec holds 0, nothing may be written through it
    rig.set_type(0, TYPE)
    rig.set_note(0, 60)
    rig.rec_word(0, 0x1B8, 1)
    out = rig.call(
        0, extra_reads={"diag": (nw(DIAG), 16), "low": (0, 256)}, ctx="friendly"
    )
    assert stops_at(out, RETURN_SW), out["halt"]
    assert out["diag"][12] == crumb(7, 0, TYPE) and out["diag"][13:15] == [1, 1]
    assert not any(out["low"])


def sine_blocks(rig, note, tune=64.0, blocks=60):
    rig.set_type(0, TYPE)
    rig.set_note(0, note, tune)
    rig.rec_word(0, 0x1B8, 1)
    ys = []
    state = [nw(STATE) + k for k in range(32)]
    for _ in range(blocks):
        out = rig.call(
            0, extra_reads={"state": (nw(STATE), 32), "rec": (nw(RECORDS) + 110, 1)}
        )
        assert stops_at(out, RETURN_SW), out["halt"]
        ys += [val(w) for w in out["out"]]
        for a, w in zip(state, out["state"], strict=True):
            rig.dm[a] = w
        rig.dm[nw(RECORDS) + 110] = out["rec"][0]
    return np.array(ys, dtype=np.float64)


def peak_hz(x, fs=48000.0):
    """Frequency from the interpolated upward zero crossings (works for a few cycles)."""
    idx = np.flatnonzero((x[:-1] < 0) & (x[1:] >= 0))
    t = idx + (-x[idx]) / (x[idx + 1] - x[idx])
    assert len(t) >= 3, "fewer than two periods"
    return fs * (len(t) - 1) / (t[-1] - t[0])


@pytest.mark.parametrize("note", [12, 36, 60, 69, 84, 108, 127])
def test_the_sine_follows_the_note_within_a_cent(rig_cls, note):
    rig = rig_cls()
    want = 440.0 * 2 ** ((note - 69) / 12)
    y = sine_blocks(rig, note, blocks=max(100, int(4.2 * 48000 / want / 32)))
    got = peak_hz(y)
    cents = 1200 * np.log2(got / want)
    assert abs(cents) < 1.0, (note, got, want, cents)
    amp = float(np.max(np.abs(y)))
    assert 0.35 < amp < 0.40, amp  # velocity 100 / 127 * 0.5 = 0.3937


def test_tune_shifts_the_pitch_by_semitones(rig_cls):
    rig = rig_cls()
    y = sine_blocks(rig, 60, tune=65.0, blocks=100)
    want = 440.0 * 2 ** ((61 - 69) / 12)
    assert abs(1200 * np.log2(peak_hz(y) / want)) < 1.0


def test_a_retrigger_restarts_the_oscillator_and_a_stopped_voice_is_silent(rig_cls):
    rig = rig_cls()
    y = sine_blocks(rig, 60, blocks=4)
    assert abs(y[0]) < 1e-6, "the oscillator starts at phase 0"
    # byte +0x1b9 now holds the mark; the arm routine's short store of 1 at +0x1b8 clears it: a new note
    rig.rec_word(0, 0x1B8, 1)
    y2 = []
    out = rig.call(0, extra_reads={"state": (nw(STATE), 32)})
    y2 = [val(w) for w in out["out"]]
    assert abs(y2[0]) < 1e-6
    # ACTIVE = 0: zeros
    rig.rec_word(0, 0x1B8, 0)
    out = rig.call(0)
    assert out["out"] == [0] * 32
