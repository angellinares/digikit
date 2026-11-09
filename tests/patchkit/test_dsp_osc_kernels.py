"""Exact extracted noise/sine kernels on the concrete SHARC core, plus numerical bounds."""

import numpy as np
import pytest
from support.paths import REPO_ROOT

from tools.patchkit import dsp_osc_ref, pk_sharc, pk_toolchains

BASE = 0xB90000
IN, OUT = 0x08020000, 0x08020400
assembled = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None, reason="selache not built"
)


def run(source, *, regs=None, dm=None, count=32):
    source = pk_sharc.expand_includes(
        source, [REPO_ROOT / "sdk" / "asm", REPO_ROOT / "sdk"]
    )
    _, linked = pk_sharc.build(source, BASE)
    result = pk_sharc.run_call(
        linked, "_t", regs=regs or {}, dm_words=dm or {}, reads={"out": (OUT, count)}
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    return result


def test_xorshift_known_sequence():
    # Marsaglia's xorshift32 recurrence, seed 1, independent published-style integer vector.
    state = 1
    actual = []
    for _ in range(5):
        state = dsp_osc_ref.xorshift32(state)
        actual.append(state)
    assert actual == [270369, 67634689, 2647435461, 307599695, 2398689233]
    assert dsp_osc_ref.xorshift32(0) == 0


@assembled
@pytest.mark.parametrize("seed", [0, 1, 0x2545F491, 0xFFFFFFFF, 0x80000000])
@pytest.mark.parametrize("load_shift", [False, True])
def test_xorshift_concrete(seed, load_shift):
    macro = (
        "DK_XORSHIFT32_LOAD_STEP(R9, R1)"
        if load_shift
        else "DK_XORSHIFT32_STEP(R9, R1, R10)"
    )
    result = run(
        f"""
#include "dsp/noise.inc"
.SECTION/PM seg_pmco;
.GLOBAL _t;
_t:
    I0 = {OUT};
    M6 = 1;
    R10 = -17;
    LCNTR = 32, DO _last UNTIL LCE;
        {macro};
_last: DM(I0, M6) = R9;
    RTS;
""",
        regs={"R9": seed},
    )
    state, expected = seed, []
    for _ in range(32):
        state = dsp_osc_ref.xorshift32(state)
        expected.append(state)
    assert result["out"] == expected
    assert result["instructions"] <= 5 + 32 * (8 if load_shift else 7)


def folded_phases():
    # Endpoints, signed zero, adjacent floats, and an asymmetric dense interior.
    edges = np.array([-0.25, -0.0, 0.0, 0.25], dtype=np.float32)
    return np.concatenate(
        [
            edges,
            np.nextafter(edges[[0, 3]], np.float32(0)),
            np.linspace(-0.249, 0.247, 122, dtype=np.float32),
        ]
    )


def test_sine_accuracy():
    x = np.linspace(-0.25, 0.25, 65537, dtype=np.float32)
    error = np.abs(
        dsp_osc_ref.sine_poly(x).astype(float) + np.sin(2 * np.pi * x.astype(float))
    )
    assert np.max(error) < 4e-6
    y = dsp_osc_ref.sine_poly(np.array([-0.25, -0.0, 0.0, 0.25], dtype=np.float32))
    np.testing.assert_allclose(y, [1, 0, 0, -1], atol=4e-6, rtol=0)
    assert np.signbit(y[1]) == np.bool_(False)
    assert np.signbit(y[2]) == np.bool_(True)


@assembled
def test_sine_concrete_operation_order():
    x = folded_phases()
    dm = {IN + i: int(w) for i, w in enumerate(x.view(np.uint32))}
    result = run(
        f"""
#include "dsp/oscillator.inc"
.SECTION/PM seg_pmco;
.GLOBAL _t;
_t:
    I0 = {OUT};
    I1 = {IN};
    M6 = 1;
    LCNTR = {len(x)}, DO _last UNTIL LCE;
        F0 = DM(I1, M6);
        DK_SINE_POLY_STEP(F2, F0, F1, F3, R2, R3);
_last: DM(I0, M6) = F2;
    RTS;
""",
        dm=dm,
        count=len(x),
    )
    assert result["out"] == dsp_osc_ref.sine_poly(x).view(np.uint32).tolist()
    assert result["instructions"] <= 5 + len(x) * 18
