"""SDK DSP step macros (sdk/dsp/*.inc): each macro in a 32-sample block loop on the concrete core, against the float32
models of tools/patchkit/dsp_ref.py, plus the instruction count of the loop.

recip, div and rsqrt run with the core's measured seed tables (8-bit seeds), so they are compared within the bounds stated in
dsp_ref.py. Skips when Selache is not built."""

import numpy as np
import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.patchkit import (  # noqa: E402
    dsp_ref,
    pk_sharc,
    pk_toolchains,
)

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

BASE = 0xB90000  # short-word (PC) address in L2 code space
BUF = 0x08020000  # normal-word DM address in L2
IN0, IN1, OUT = BUF, BUF + 0x40, BUF + 0x80
N = dsp_ref.BLOCK
f32 = np.float32


def bits(a) -> list[int]:
    return [int(w) for w in np.asarray(a, dtype=f32).view(np.uint32)]


def b(x: float) -> int:
    return int(f32(x).view(np.uint32))


def fill(edges, count=N) -> np.ndarray:
    """The edge values, then a ramp from -4 to 4 up to `count` values (float32)."""
    ramp = np.linspace(-4.0, 4.0, count - len(edges))
    return np.concatenate([np.asarray(edges, dtype=f32), ramp.astype(f32)])


# zero, +-1, +-8, small values, and some in-between values
X = fill([0.0, 1.0, -1.0, 8.0, -8.0, 0.001, -0.001, 0.5, -0.5, 1.5, -1.5, 0.25])
Y = X[::-1].copy()
# 1e-3 .. 1e3, both signs, no zero
WIDE = np.concatenate(
    [np.geomspace(1e-3, 1e3, 16), -np.geomspace(1e-3, 1e3, 16)]
).astype(f32)
POS = np.geomspace(1e-3, 1e3, N).astype(f32)


def kernel(inc, setup, setup_n, macro, macro_n, consts, inputs, out_reg="F8"):
    """Build and run one loop: load the inputs (F0, F1), the macro, the store of `out_reg`; 32 samples.

    Returns (output words, executed instructions, ceiling). The ceiling documents the cost:
      prologue (I0, I1, [I2], M6)  +  setup_n  +  1 (LCNTR)
      + 32 x (loads + macro_n + 1 store + NOPs up to the 4 instructions a loop needs)  +  1 (RTS)."""
    loads = [f"F{i} = DM(I{i + 1}, M6);" for i in range(len(inputs))]
    body_n = len(loads) + macro_n + 1
    pad = ["NOP;"] * max(0, 4 - body_n)
    per_sample = body_n + len(pad)
    pointers = ["I0 = %d;" % OUT, "I1 = %d;" % IN0]
    if len(inputs) > 1:
        pointers.append("I2 = %d;" % IN1)
    text = "\n".join(
        [
            '#include "dsp/%s.inc"' % inc,
            ".SECTION/PM seg_pmco;",
            ".GLOBAL _t;",
            "_t:",
            *("    " + p for p in pointers),
            "    M6 = 1;",
            *("    " + s for s in setup),
            "    LCNTR = 32, DO _t_end UNTIL LCE;",
            *("        " + s for s in loads),
            "        %s;" % macro,
            *("        " + s for s in pad),
            "_t_end: DM(I0, M6) = %s;" % out_reg,
            "    RTS;",
            "",
        ]
    )
    text = pk_sharc.expand_includes(text, [ROOT / "sdk" / "asm", ROOT / "sdk"])
    _, linked = pk_sharc.build(text, BASE)
    dm = {}
    for base, vec in zip((IN0, IN1), inputs, strict=False):
        dm.update({base + i: w for i, w in enumerate(bits(vec))})
    r = pk_sharc.run_call(
        linked,
        "_t",
        regs=consts,
        dm_words=dm,
        reads={"out": (OUT, N)},
    )
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    ceiling = len(pointers) + 1 + setup_n + 1 + N * per_sample + 1
    return np.array(r["out"], dtype=np.uint32).view(f32), r["instructions"], ceiling


def test_gain():
    g = f32(0.7)
    out, n, ceiling = kernel(
        "gain", [], 0, "DK_GAIN_STEP(F8, F0, F4)", 1, {"R4": b(g)}, [X]
    )  # g = 0.7
    assert np.array_equal(out.view(np.uint32), dsp_ref.gain(X, g).view(np.uint32))
    assert n <= ceiling


def test_mix():
    m = f32(0.3)
    out, n, ceiling = kernel(
        "gain", [], 0, "DK_MIX_STEP(F8, F0, F1, F4, F9)", 3, {"R4": b(m)}, [X, Y]
    )  # m = 0.3
    assert np.array_equal(out.view(np.uint32), dsp_ref.mix(X, Y, m).view(np.uint32))
    assert n <= ceiling


@pytest.mark.parametrize("cur, target", [(0.0, 1.0), (2.5, -3.0), (-1.0, -1.0)])
def test_ramp(cur, target):
    consts = {"R8": b(cur), "R6": b(target), "R7": b(1 / 32)}  # 1/32 = 0.03125
    out, n, ceiling = kernel(
        "ramp",
        ["DK_RAMP_SETUP(F4, F8, F6, F7);"],
        2,
        "DK_RAMP_STEP(F8, F4)",
        1,
        consts,
        [X],
    )
    assert np.array_equal(
        out.view(np.uint32), dsp_ref.ramp(cur, target).view(np.uint32)
    )
    assert n <= ceiling


def test_onepole():
    a, y0 = f32(0.1227), f32(0.5)
    out, n, ceiling = kernel(
        "onepole",
        [],
        0,
        "DK_ONEPOLE_STEP(F8, F0, F4, F9)",
        3,
        {"R4": b(a), "R8": b(y0)},  # a = 0.1227, y0 = 0.5
        [X],
    )
    assert np.array_equal(
        out.view(np.uint32), dsp_ref.onepole(X, a, y0).view(np.uint32)
    )
    assert n <= ceiling


# --- block routines (sdk/dsp/call.inc, biquad.inc): the C convention, called from assembly and from selcc C ---

BQ = BUF + 0x200
BQ_IN = (
    BQ,
    BQ + 0x80,
)  # two input blocks; the two words after each are a marker region the routine must not read
BQ_OUT = (BQ + 0x40, BQ + 0xC0)
BQ_ST = BQ + 0x100
BQ_STACK = BQ + 0x300  # I6 = I7 at the call; the frames grow down from here
BQ_CALL, BQ_ROUTINE = (
    3,
    200,
)  # DK_CALL; _dk_biquad: 25 before the loop, 5 x 32 in and after it, 15 after (counted in biquad.inc)
BQ_SAVED = {f"R{n}": 0x7000 + n for n in (3, 5, 6, 7, 9, 10, 11, 13, 14, 15)}
BQ_SAVED.update(
    {"I0": 0x08021000, "I1": 0x08021010, "I2": 0x08021020, "I3": 0x08021030}
)
BQ_SAVED.update({"I5": 0x08021040, "M1": 11, "M2": 12, "M3": 13, "B3": 0x08021050})
BQ_CONSTS = {"M5": 0, "M6": 1, "M7": -1 & 0xFFFFFFFF, "M14": 1}
BQ_READ_REGS = tuple(
    k
    for k in (*BQ_SAVED, *BQ_CONSTS, "I6", "I7")
    if k not in ("R5", "R6", "R7", "R9", "R10", "R11", "R13", "I0")
)

RNG = np.random.default_rng(7)
BQ_X = RNG.uniform(-1.0, 1.0, 2 * N).astype(f32)


def cookbook_lowpass(fs, f0, q):
    w = 2 * np.pi * f0 / fs
    al = np.sin(w) / (2 * q)
    a0 = 1 + al
    c = np.cos(w)
    return [f32(v / a0) for v in ((1 - c) / 2, 1 - c, (1 - c) / 2, -2 * c, 1 - al)]


def cookbook_bandpass(fs, f0, q):
    """Constant 0 dB peak gain band-pass."""
    w = 2 * np.pi * f0 / fs
    al = np.sin(w) / (2 * q)
    a0 = 1 + al
    return [f32(v / a0) for v in (al, 0.0, -al, -2 * np.cos(w), 1 - al)]


BQ_SETS = {
    "lowpass": (cookbook_lowpass(48000, 1000, 0.7071), (0.0, 0.0)),
    "bandpass": (cookbook_bandpass(48000, 2000, 8.0), (0.1, -0.05)),
}


def bq_model(coef, state):
    y1, s1, s2 = dsp_ref.biquad(BQ_X[:N], *coef, *state)
    y2, s1, s2 = dsp_ref.biquad(BQ_X[N:], *coef, s1, s2)
    return y1, y2, [s1, s2]


def bq_poke(coef, state):
    dm = {BQ_ST + i: b(v) for i, v in enumerate([*coef, *state])}
    for k in range(2):
        for i, w in enumerate(bits(BQ_X[k * N : (k + 1) * N])):
            dm[BQ_IN[k] + i] = w
        dm[BQ_IN[k] + N] = dm[BQ_IN[k] + N + 1] = (
            0x7FC00001  # NaN marker after the block
        )
    return dm


def test_biquad_from_assembly_two_blocks_in_a_row():
    for in_place in (False, True):
        for coef, state in BQ_SETS.values():
            outs = BQ_IN if in_place else BQ_OUT
            text = "\n".join(
                [
                    "#define DK_BLOCK 32",
                    '#include "dsp/call.inc"',
                    '#include "dsp/biquad.inc"',
                    ".SECTION/PM seg_pmco;",
                    ".GLOBAL _bq_h;",
                    "_bq_h:",
                    *(
                        line
                        for k in range(2)
                        for line in (
                            "    R4 = %d;" % BQ_IN[k],
                            "    R8 = %d;" % outs[k],
                            "    R12 = %d;" % BQ_ST,
                            "    DK_CALL(_dk_biquad, _bq_after%d);" % k,
                        )
                    ),
                    "    RTS;",
                    "",
                ]
            )
            _, linked = pk_sharc.build(
                pk_sharc.expand_includes(text, [ROOT / "sdk" / "asm", ROOT / "sdk"]),
                BASE,
            )
            regs = {**BQ_SAVED, **BQ_CONSTS, "I6": BQ_STACK, "I7": BQ_STACK}
            r = pk_sharc.run_call(
                linked,
                "_bq_h",
                regs=regs,
                dm_words=bq_poke(coef, state),
                reads={"y0": (outs[0], N), "y1": (outs[1], N), "st": (BQ_ST + 5, 2)},
                read_regs=BQ_READ_REGS,
            )
            assert r["halt"]["reason"] == "breakpoint", r["halt"]
            y1, y2, st = bq_model(coef, state)
            assert np.array_equal(np.array(r["y0"], dtype=np.uint32), bits(y1))
            assert np.array_equal(np.array(r["y1"], dtype=np.uint32), bits(y2))
            assert r["st"] == bits(st)
            # the cost: 3 loads of R4/R8/R12 and DK_CALL per block, the routine, the final RTS
            assert r["instructions"] <= 2 * (3 + BQ_CALL + BQ_ROUTINE) + 1
            for name, want in {
                **BQ_SAVED,
                **BQ_CONSTS,
                "I6": BQ_STACK,
                "I7": BQ_STACK,
            }.items():
                assert r["regs"][name] == want, name


def test_biquad_never_reads_past_the_input_block():
    coef, state = BQ_SETS["bandpass"]
    text = "\n".join(
        [
            "#define DK_BLOCK 32",
            '#include "dsp/call.inc"',
            '#include "dsp/biquad.inc"',
            ".SECTION/PM seg_pmco;",
            ".GLOBAL _bq_h;",
            "_bq_h:",
            "    R4 = %d;" % BQ_IN[0],
            "    R8 = %d;" % BQ_OUT[0],
            "    R12 = %d;" % BQ_ST,
            "    DK_CALL(_dk_biquad, _bq_after);",
            "    RTS;",
            "",
        ]
    )
    _, linked = pk_sharc.build(
        pk_sharc.expand_includes(text, [ROOT / "sdk" / "asm", ROOT / "sdk"]), BASE
    )
    regs = {**BQ_SAVED, **BQ_CONSTS, "I6": BQ_STACK, "I7": BQ_STACK}
    # the watch is a read watchpoint of the core's driver (CallSession.record_reads); the control range proves that it fires
    for lo, count, must_hit in ((BQ_IN[0] + N, 2, False), (BQ_IN[0] + N - 2, 2, True)):
        sess = pk_sharc.CallSession([linked], 0xB9F000)
        for addr, word in bq_poke(coef, state).items():
            sess.poke(addr, word)
        sess.record_reads(lo, count)
        r = sess.call("_bq_h", regs)
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert bool(sess.stores) == must_hit, (lo, [e.address for e in sess.stores])


def test_biquad_from_selcc_c():
    c = """
void dk_biquad(int x, int y, int st);
void run(int x, int y, int st) { dk_biquad(x, y, st); }
"""
    kernel_text = "\n".join(
        [
            "#define DK_BLOCK 32",
            '#include "dsp/call.inc"',
            '#include "dsp/biquad.inc"',
            "",
        ]
    )
    kbase = BASE + 0x1000
    _, kernel = pk_sharc.build(
        pk_sharc.expand_includes(kernel_text, [ROOT / "sdk" / "asm", ROOT / "sdk"]),
        kbase,
    )
    caller = pk_sharc.link(
        pk_sharc.compile_c(c), BASE, {"dk_biquad.": kernel.symbols["dk_biquad."]}
    )
    ret, frame = 0xB9E000, BQ_STACK
    for coef, state in BQ_SETS.values():
        y1, _, _ = dsp_ref.biquad(BQ_X[:N], *coef, *state)
        dm = {frame: 0, frame - 1: ret - 1, **bq_poke(coef, state)}
        r = pk_sharc.run_call(
            caller,
            "run.",
            regs={
                **BQ_CONSTS,
                "R4": BQ_IN[0],
                "R8": BQ_OUT[0],
                "R12": BQ_ST,
                "I6": frame,
                "I7": frame - 2,
            },
            dm_words=dm,
            reads={"y": (BQ_OUT[0], N)},
            return_sw=ret,
            others=(kernel,),
            max_steps=20000,
        )
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert np.array_equal(np.array(r["y"], dtype=np.uint32), bits(y1))


# --- delay and half-band block routines (sdk/dsp/delay.inc, halfband.inc): circular buffers on the C convention ---

KR = (
    BUF + 0x600
)  # the buffers of these routines; the frames grow down from BQ_STACK below it
NAN_WORD = 0x7FC00001  # marker words around every buffer; a routine that reads one makes the output NaN
L_REGS = tuple(f"L{i}" for i in range(6))
# MODE1 as the firmware start-up sets it (0x1011800, includes CBUFEN, bit 24): load/store post-modify wraps only with CBUFEN set
SAVED_REGS = {
    **BQ_SAVED,
    **BQ_CONSTS,
    "I6": BQ_STACK,
    "I7": BQ_STACK,
    "MODE1": 0x1011800,
}


def marks(dm, base, count):
    """NaN markers on the two words before and the two after [base, base + count)."""
    for a in (base - 2, base - 1, base + count, base + count + 1):
        dm[a] = NAN_WORD


def block_calls(inc, name, args, entry):
    """A harness that calls `name` once for every (R4, R8, R12) of `args`, then returns."""
    lines = [
        "#define DK_BLOCK 32",
        '#include "dsp/call.inc"',
        '#include "dsp/%s.inc"' % inc,
        ".SECTION/PM seg_pmco;",
        ".GLOBAL %s;" % entry,
        "%s:" % entry,
    ]
    for call_index, (x, y, st) in enumerate(args):
        lines += [
            "    R4 = %d;" % x,
            "    R8 = %d;" % y,
            "    R12 = %d;" % st,
            "    DK_CALL(%s, _h_after%d);" % (name, call_index),
        ]
    return "\n".join([*lines, "    RTS;", ""])


def build_calls(inc, name, args, externs=None):
    text = block_calls(inc, name, args, "_h")
    return pk_sharc.build(
        pk_sharc.expand_includes(text, [ROOT / "sdk" / "asm", ROOT / "sdk"]),
        BASE,
        externs,
    )[1]


def check_saved(r):
    for name, want in SAVED_REGS.items():
        assert r["regs"][name] == want, name
    for name in L_REGS:  # circular buffering is off again: every L register is 0
        assert r["regs"][name] == 0, name


DL_L = 64
DL_RING = KR + 0x10
DL_IN = (KR + 0x100, KR + 0x140)
DL_OUT = (KR + 0x180, KR + 0x1C0)
DL_ST = KR + 0x200
DL_CALL, DL_ROUTINE = (
    3,
    172,
)  # DK_CALL; _dk_delay: 30 before the loop, 8 x 16 in it, 14 after (measured)
DL_RNG = np.random.default_rng(11)
DL_RING0 = DL_RNG.uniform(-1.0, 1.0, DL_L).astype(f32)
DL_X = DL_RNG.uniform(-1.0, 1.0, 2 * N).astype(f32)
DL_W = 60  # near the end of the ring: the writes and the reads wrap inside a block


def dl_poke(d, w):
    dm = {DL_ST: DL_RING, DL_ST + 1: DL_L, DL_ST + 2: w, DL_ST + 3: b(d)}
    dm.update({DL_RING + i: x for i, x in enumerate(bits(DL_RING0))})
    marks(dm, DL_RING, DL_L)
    for k in range(2):
        dm.update({DL_IN[k] + i: x for i, x in enumerate(bits(DL_X[k * N :][:N]))})
        marks(dm, DL_IN[k], N)
    return dm


def dl_model(d, w):
    y1, ring, w = dsp_ref.delay(DL_X[:N], DL_RING0, w, d)
    y2, ring, w = dsp_ref.delay(DL_X[N:], ring, w, d)
    return y1, y2, ring, w


@pytest.mark.parametrize("d", [1.0, 2.5, 7.25, DL_L - 2.0])
def test_delay_from_assembly_two_blocks_in_a_row(d):
    for in_place in (False, True):
        outs = DL_IN if in_place else DL_OUT
        linked = build_calls(
            "delay", "_dk_delay", [(DL_IN[k], outs[k], DL_ST) for k in range(2)]
        )
        r = pk_sharc.run_call(
            linked,
            "_h",
            regs=SAVED_REGS,
            dm_words=dl_poke(d, DL_W),
            reads={
                "y0": (outs[0], N),
                "y1": (outs[1], N),
                "w": (DL_ST + 2, 1),
                "ring": (DL_RING, DL_L),
            },
            read_regs=(*BQ_READ_REGS, *L_REGS, "MODE1"),
        )
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        y1, y2, ring, w = dl_model(d, DL_W)
        assert np.array_equal(np.array(r["y0"], dtype=np.uint32), bits(y1))
        assert np.array_equal(np.array(r["y1"], dtype=np.uint32), bits(y2))
        assert r["w"] == [w]
        assert np.array_equal(np.array(r["ring"], dtype=np.uint32), bits(ring))
        # the cost: 3 loads of R4/R8/R12 and DK_CALL per block, the routine, the final RTS
        assert r["instructions"] <= 2 * (3 + DL_CALL + DL_ROUTINE) + 1
        check_saved(r)


def test_delay_d_one_is_the_previous_input():
    y1, _, _ = dsp_ref.delay(DL_X[:N], DL_RING0, DL_W, 1.0)
    want = np.concatenate([[DL_RING0[DL_W - 1]], DL_X[: N - 1]]).astype(f32)
    assert np.array_equal(y1.view(np.uint32), want.view(np.uint32))


def test_delay_never_reads_outside_the_buffers():
    linked = build_calls("delay", "_dk_delay", [(DL_IN[0], DL_OUT[0], DL_ST)])
    watches = (
        (DL_IN[0] + N, 2, False),  # after the input block
        (DL_IN[0] + N - 2, 2, True),  # control: the last input words
        (DL_RING - 2, 2, False),  # before the ring
        (DL_RING + DL_L, 2, False),  # after the ring
        (DL_RING + DL_L - 2, 2, True),  # control: the last ring words
    )
    for lo, count, must_hit in watches:
        sess = pk_sharc.CallSession([linked], 0xB9F000)
        for addr, word in dl_poke(7.25, DL_W).items():
            sess.poke(addr, word)
        sess.record_reads(lo, count)
        r = sess.call("_h", SAVED_REGS)
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert bool(sess.stores) == must_hit, (lo, [e.address for e in sess.stores])


HB_RING = KR + 0x310
HB_ST = KR + 0x300
HB_TABLE = (
    KR + 0x340
)  # the coefficient words the include's .VAR table stands for (see test_halfband_table_is_the_model)
HB_IN = tuple(KR + 0x400 + 0x80 * k for k in range(3))
HB_OUT = tuple(KR + 0x600 + 0x40 * k for k in range(3))
HB_L = 32  # history ring cells
HB_OFF = 25  # the history offset: the writes and reads wrap inside the first calls
HB_CALL, HB_ROUTINE = (
    3,
    698,
)  # DK_CALL; _dk_halfband_down: 39 before the loop, 20 x 31 in it, 21 for the last output, 18 after (measured)
HB_RNG = np.random.default_rng(13)
HB_RING0 = HB_RNG.uniform(-1.0, 1.0, HB_L).astype(f32)
_T = np.arange(3 * 2 * N)
# a low sine (passband), a high sine (stopband), a sine at 0.2 fs and an impulse early in the third block
HB_X = (
    0.5 * np.sin(2 * np.pi * 0.03 * _T)
    + 0.25 * np.sin(2 * np.pi * 0.41 * _T)
    + 0.125 * np.sin(2 * np.pi * 0.2 * _T)
).astype(f32)
HB_X[2 * N + 6] += f32(1.0)


def hb_poke():
    dm = {HB_ST: HB_RING, HB_ST + 1: HB_OFF}
    dm.update({HB_TABLE + i: w for i, w in enumerate(bits(dsp_ref.HALFBAND_C))})
    dm.update({HB_RING + i: x for i, x in enumerate(bits(HB_RING0))})
    marks(dm, HB_RING, HB_L)
    for k in range(3):
        dm.update(
            {HB_IN[k] + i: x for i, x in enumerate(bits(HB_X[k * 2 * N :][: 2 * N]))}
        )
        marks(dm, HB_IN[k], 2 * N)
    return dm


def hb_model():
    ring, off, ys = HB_RING0, HB_OFF, []
    for k in range(3):
        y, ring, off = dsp_ref.halfband_down(HB_X[k * 2 * N :][: 2 * N], ring, off)
        ys.append(y)
    return ys, ring, off


def hb_build(calls):
    return build_calls(
        "halfband",
        "_dk_halfband_down",
        [(HB_IN[k], HB_OUT[k], HB_ST) for k in range(calls)],
        {"_dk_halfband_c1": HB_TABLE},
    )


def test_halfband_down_from_assembly_three_blocks_in_a_row():
    r = pk_sharc.run_call(
        hb_build(3),
        "_h",
        regs=SAVED_REGS,
        dm_words=hb_poke(),
        reads={
            "y0": (HB_OUT[0], N),
            "y1": (HB_OUT[1], N),
            "y2": (HB_OUT[2], N),
            "off": (HB_ST + 1, 1),
            "ring": (HB_RING, HB_L),
        },
        read_regs=(*BQ_READ_REGS, *L_REGS, "MODE1"),
        max_steps=200000,
    )
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    ys, ring, off = hb_model()
    for k in range(3):
        assert np.array_equal(np.array(r["y%d" % k], dtype=np.uint32), bits(ys[k])), k
    assert (
        r["off"] == [off] == [HB_OFF]
    )  # 64 inputs are 2 turns of the 32-cell ring: the offset does not change
    assert np.array_equal(np.array(r["ring"], dtype=np.uint32), bits(ring))
    # the cost: 3 loads of R4/R8/R12 and DK_CALL per block, the routine, the final RTS
    assert r["instructions"] <= 3 * (3 + HB_CALL + HB_ROUTINE) + 1
    check_saved(r)


def test_halfband_down_never_reads_outside_the_buffers():
    linked = hb_build(1)
    watches = (
        (HB_IN[0] + 2 * N, 2, False),  # after the input block
        (HB_IN[0] + 2 * N - 2, 2, True),  # control: the last input words
        (HB_RING - 2, 2, False),  # before the history ring
        (HB_RING + HB_L, 2, False),  # after it
        (HB_RING + HB_L - 2, 2, True),  # control: the last ring words
    )
    for lo, count, must_hit in watches:
        sess = pk_sharc.CallSession([linked], 0xB9F000)
        for addr, word in hb_poke().items():
            sess.poke(addr, word)
        sess.record_reads(lo, count)
        r = sess.call("_h", SAVED_REGS, max_steps=200000)
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert bool(sess.stores) == must_hit, (lo, [e.address for e in sess.stores])


def test_halfband_table_is_the_model():
    """The words of the .VAR table in halfband.inc are the float32 table of the design, 2 c_j (the harness pokes the model's
    words at HB_TABLE, so this is the only check of the include's own table)."""
    import re

    text = (ROOT / "sdk" / "dsp" / "halfband.inc").read_text()
    words = [
        int(m.group(2), 16)
        for m in re.finditer(
            r"^\.VAR _dk_halfband_c(\d+) = (0x[0-9a-fA-F]+);", text, re.M
        )
    ]
    assert words == bits(dsp_ref.HALFBAND_C)


def halfband_response(c, f):
    """H(f) of the 23-tap half-band with odd taps c (f in cycles per input sample): 0.5 + 2 sum c_j cos(2 pi j f)."""
    c = np.asarray(c, dtype=np.float64)
    f = np.asarray(f, dtype=np.float64)
    j = np.array([1.0, 3.0, 5.0, 7.0, 9.0, 11.0])
    return 0.5 + 2.0 * np.sum(c * np.cos(2 * np.pi * np.outer(f, j)), axis=1)


def test_halfband_design():
    """Measured on the float32 table (Kaiser window, beta 3.4, 23 taps): the DC gain is 1.0 (error 2.4e-8); the passband
    (0 .. 0.2 fs) ripple is 0.01227 (0.106 dB); the stopband (0.3 .. 0.5 fs) maximum is 0.01227 (-38.2 dB). The bounds are
    the ripple x 1.2 and the stopband + 1 dB. A half-band has H(f) + H(0.5 - f) = 1, so the two ripples are equal."""
    taps = np.array(dsp_ref.HALFBAND_C, dtype=np.float64) / 2  # the table holds 2 c_j
    assert np.array_equal(
        np.array(dsp_ref.HALFBAND_C, dtype=np.float32),
        (2.0 * dsp_ref.halfband_design()).astype(np.float32),
    )
    pass_f = np.linspace(0.0, 0.2, 401)
    stop_f = np.linspace(0.3, 0.5, 401)
    hp = halfband_response(taps, pass_f)
    hs = halfband_response(taps, stop_f)
    assert abs(hp[0] - 1.0) <= 1e-6
    assert np.max(np.abs(hp - 1.0)) <= 0.0148
    assert np.max(np.abs(hs)) <= 0.0138
    assert np.allclose(hp + halfband_response(taps, 0.5 - pass_f), 1.0, atol=1e-6)


def test_sat():
    consts = {"R4": b(1.0), "R5": b(0.5), "R6": b(1.5)}  # one, half, k15
    out, n, ceiling = kernel(
        "sat", [], 0, "DK_SAT_STEP(F8, F0, F4, F5, F6, F9)", 5, consts, [X]
    )
    assert np.array_equal(out.view(np.uint32), dsp_ref.sat(X).view(np.uint32))
    assert np.max(np.abs(out)) <= 1.0
    assert n <= ceiling


def test_recip():
    out, n, ceiling = kernel(
        "recip",
        [],
        0,
        "DK_RECIP(F8, F0, F4, F9)",
        7,
        {"R4": b(2.0)},  # two
        [WIDE],
    )
    assert dsp_ref.max_rel_error(out, dsp_ref.recip(WIDE)) <= dsp_ref.REL_RECIP
    assert n <= ceiling


def test_div():
    out, n, ceiling = kernel(
        "recip",
        [],
        0,
        "DK_DIV(F8, F0, F1, F4, F9, F10)",
        8,
        {"R4": b(2.0)},  # two
        [X[::-1].copy() + f32(0.125), WIDE],
    )
    num = X[::-1].copy() + f32(0.125)
    got, want = out.astype(np.float64), dsp_ref.div(num, WIDE)
    assert np.max(np.abs(got - want) / np.abs(want)) <= dsp_ref.REL_DIV
    assert n <= ceiling


def test_rsqrt():
    out, n, ceiling = kernel(
        "recip",
        [],
        0,
        "DK_RSQRT(F8, F0, F4, F5, F9, F10)",
        16,
        {"R4": b(0.5), "R5": b(1.5)},  # half, k15
        [POS],
    )
    assert dsp_ref.max_rel_error(out, dsp_ref.rsqrt(POS)) <= dsp_ref.REL_RSQRT
    assert n <= ceiling
