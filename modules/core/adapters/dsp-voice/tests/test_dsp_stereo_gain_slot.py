"""The pan hook of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 1), on the concrete SHARC+ core.

The stock engine bytes run the block from the block ramp to the end of the pan loop (0x1c99de to 0x1c9b33: ramp, conditional fade, metadata clear, 16 slots
of pan) with `_gs_slot` patched in at 0x1c9af6, and the same block unpatched. A slot with no stamp is bit-identical to the stock block; a stamped slot has
plane A = stock(L).A and plane B = stock(R).B, where stock(X) is the unpatched block run on X: the pan gains, the block ramp and the fade of R are
the engine's own. Original firmware is supplied at runtime, never copied into a fixture."""

import random

import pytest
from support.dn2_stereo_rig import (
    HEARTBEAT,
    PAN_END,
    PLANE_TABLE,
    PLANES,
    TABLE,
    Rig,
    bits,
    buffers,
    build_engine,
    engine_regs,
    planes,
    run_block,
    value,
    words,
)

ALL = range(16)


@pytest.fixture(scope="module")
def rig():
    return Rig()


def block_of(rng, scale=0.5):
    return [bits(rng.uniform(-scale, scale)) for _ in range(32)]


def run_triple(rig, stereo, seed, ramp=None, fade=None, gain_words=None):
    """Stock block on L, stock block on R, patched block with R in the R buffers of the `stereo` slots."""
    rng = random.Random(seed)
    left = {v: block_of(rng) for v in ALL}
    right = {v: block_of(rng, 0.3) for v in ALL}
    out = {}
    for name, data, hooked in (
        ("L", left, False),
        ("R", right, False),
        ("H", left, True),
    ):
        session = rig.session(hooked=hooked)
        build_engine(session, data, ramp=ramp, fade=fade, gain_words=gain_words)
        if hooked:
            for v in ALL:
                base = rig.data["_gs_rbuf"] + 128 * v
                for i, w in enumerate(right[v]):
                    session.poke(base + 4 * i, w)
                session.poke(
                    rig.data["_gs_stamp"] + 4 * v, HEARTBEAT if v in stereo else 0
                )
        result = run_block(session)
        out[name] = (planes(session), buffers(session), result["instructions"])
    return out, left, right


def check(out, stereo):
    (pl_l, buf_l, _), (pl_r, _, _), (pl_h, buf_h, _) = out["L"], out["R"], out["H"]
    for v in ALL:
        assert pl_h[v][0] == pl_l[v][0], f"plane A of slot {v}"
        expected = pl_r[v][1] if v in stereo else pl_l[v][1]
        assert pl_h[v][1] == expected, f"plane B of slot {v}"
    assert buf_h == buf_l, "L stays the stock path"


def test_no_stamp_is_bit_identical_to_the_stock_block():
    rig = Rig()
    out, _, _ = run_triple(rig, set(), 1)
    check(out, set())
    # the price of the hooks for 16 mono slots: the stamp check of each slot and of the pre-pan loop, about 26 instructions per slot
    assert out["H"][2] - out["L"][2] < 16 * 30


@pytest.mark.parametrize(
    "stereo", [{0}, {5}, {15}, {1, 2, 3}, set(range(0, 16, 2)), set(ALL)]
)
def test_a_stamped_slot_pans_r_into_plane_b_and_l_into_plane_a(rig, stereo):
    out, left, right = run_triple(rig, stereo, 7)
    check(out, stereo)
    # the planes differ: L != R is audible in B
    for v in stereo:
        assert out["H"][0][v][1] != out["L"][0][v][1]
        assert out["H"][0][v][0] == out["L"][0][v][0]


def test_a_stamped_slot_with_r_equal_to_l_reproduces_the_stock_pair(rig):
    rng = random.Random(3)
    left = {v: block_of(rng) for v in ALL}
    ref = rig.session(hooked=False)
    build_engine(ref, left)
    run_block(ref)
    session = rig.session()
    build_engine(session, left)
    for v in ALL:
        for i, w in enumerate(left[v]):
            session.poke(rig.data["_gs_rbuf"] + 128 * v + 4 * i, w)
        session.poke(rig.data["_gs_stamp"] + 4 * v, HEARTBEAT)
    run_block(session)
    assert planes(session) == planes(ref)


@pytest.mark.parametrize(
    "stamp",
    [0, HEARTBEAT - 1, HEARTBEAT + 1],
    ids=["unstamped", "last-block", "future-block"],
)
def test_a_stamp_of_another_block_is_a_mono_slot(rig, stamp):
    out, left, right = run_triple(rig, set(), 11)
    session = rig.session()
    build_engine(session, left)
    for v in ALL:
        session.poke(rig.data["_gs_stamp"] + 4 * v, stamp)
        for i, w in enumerate(right[v]):
            session.poke(rig.data["_gs_rbuf"] + 128 * v + 4 * i, w)
    run_block(session)
    assert planes(session) == out["L"][0]


@pytest.mark.parametrize("shape", ["ramp", "fade", "ramp+fade"])
def test_r_takes_the_block_ramp_and_the_fade_of_the_engine(rig, shape):
    rng = random.Random(21)
    ramp = (
        {v: (rng.uniform(0.2, 1.2), rng.uniform(-0.01, 0.01)) for v in ALL}
        if "ramp" in shape
        else None
    )
    fade = {1, 4, 5, 9} if "fade" in shape else None
    gain_words = {v: [0] * 5 + [1] + [0] * 5 for v in (fade or ())}
    stereo = {1, 2, 4, 9, 12}
    out, _, _ = run_triple(rig, stereo, 5, ramp=ramp, fade=fade, gain_words=gain_words)
    check(out, stereo)
    if fade:
        # the fade is a real one: the last sample of a faded slot is (nearly) zero, the stock block says so for R as for L
        assert (
            abs(value(out["R"][0][1][1][31])) < 1e-3 * 0.1
            and abs(value(out["L"][0][1][1][31])) < 1e-3 * 0.1
        )


def test_a_fade_flag_of_another_slot_does_not_fade_this_slot(rig):
    """Negative control: the flags are per slot (word 4 v and the gain record 44 v apart)."""
    rng = random.Random(8)
    left = {v: block_of(rng) for v in ALL}
    right = {v: block_of(rng) for v in ALL}
    results = []
    for fade in ({2}, {3}):
        session = rig.session()
        build_engine(
            session,
            left,
            fade=fade,
            gain_words={v: [0] * 5 + [1] + [0] * 5 for v in fade},
        )
        for v in ALL:
            for i, w in enumerate(right[v]):
                session.poke(rig.data["_gs_rbuf"] + 128 * v + 4 * i, w)
            session.poke(rig.data["_gs_stamp"] + 4 * v, HEARTBEAT if v == 2 else 0)
        run_block(session)
        results.append(planes(session)[2][1])
    assert results[0] != results[1], (
        "slot 2's R is faded only when slot 2 has the fade flags"
    )


# ---- the one-slot body: registers and the loop's own flag use ----


def run_slot(rig, hooked, slot, stamped, block=32, **regs):
    rng = random.Random(slot + 40)
    left = {v: block_of(rng) for v in ALL}
    session = rig.session(hooked=hooked)
    build_engine(session, left)
    for i in range(32):
        session.poke(rig.data["_gs_rbuf"] + 128 * slot + 4 * i, bits(0.125 * (i % 5)))
    session.poke(rig.data["_gs_stamp"] + 4 * slot, HEARTBEAT if stamped else 0)
    session.runner.breakpoints = frozenset((0xB9F000, PAN_END))
    r = engine_regs(
        I3=TABLE + 4 * slot,
        I5=PLANE_TABLE + 8 * slot,
        I4=TABLE + 0x354 + 28 * slot,
        R9=block,
        I12=0x1234,
        **regs,
    )
    result = session.call(
        "_pan_body",
        r,
        max_steps=4000,
        read_regs=("I2", "I3", "I4", "I5", "I12", "I6", "I7", "M4", "ASTATX"),
    )
    assert (
        result["halt"]["reason"] == "breakpoint" and result["final_pc_sw"] == PAN_END
    ), result
    return session, result


LIVE_OUT = ("R9", "I3", "I4", "I5", "I12", "I6", "I7")


@pytest.mark.parametrize("slot", [0, 7, 15])
@pytest.mark.parametrize("block", [32, 1])
def test_a_mono_slot_runs_the_stock_instructions_flags_included(rig, slot, block):
    """block 1: the stock loop leaves after the first sample on the AZ of `R0 = dec(R9)`; the hook has to hand that flag back."""
    stock, rs = run_slot(rig, False, slot, False, block)
    hooked, rh = run_slot(rig, True, slot, False, block)
    for name in LIVE_OUT:
        assert rh["regs"][name] == rs["regs"][name], name
    base = PLANES + 0x100 * slot
    assert words(hooked, base, 64) == words(stock, base, 64)
    assert hooked.peek(base + 4 * 1) == (stock.peek(base + 4 * 1))


@pytest.mark.parametrize("slot", [0, 9])
def test_a_stereo_slot_leaves_the_loop_state_as_the_stock_slot_does(rig, slot):
    stock, rs = run_slot(rig, False, slot, False)
    hooked, rh = run_slot(rig, True, slot, True)
    for name in LIVE_OUT:
        assert rh["regs"][name] == rs["regs"][name], name


def test_a_stamped_slot_of_a_one_sample_block_is_a_mono_slot(rig):
    stock, _ = run_slot(rig, False, 3, False, 1)
    hooked, _ = run_slot(rig, True, 3, True, 1)
    base = PLANES + 0x100 * 3
    assert words(hooked, base, 64) == words(stock, base, 64)
