"""With the same audio in L and R, R must equal L bit for bit through every twin of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 3).

The stock DT2 engine holds this by construction for a mono sample: both voices of a pair run the same code on the same data. Here it is the whole-chain check on
top of the per-twin tests: each stage runs on a stamped slot over several blocks in which its controls change every block, with a reset of the stage in the
middle of the note (the engine's reset after a filter change; the reset wrappers mirror it for R), starting from equal private state, and the twin's R block
is compared with its L block. A twin that shares a history, copies a control after the L call, or lets a reset reach one channel only, fails. Original firmware is
supplied at runtime, never copied into a fixture."""

import random

import pytest
from support.dn2_stereo_rig import (
    HEARTBEAT,
    STOCK,
    Rig,
    bits,
    call,
    new_session,
    poke_words,
    words,
)
from support.dn2_stereo_stages import (
    LBUF,
    R_RING,
    RING_L,
    RING_WORDS,
    SCRATCH,
    SLOT,
    STAGES,
    block_input,
    init_record,
    ring_pattern,
    run_blend,
    scaled,
    setup_blend,
    stage_regs,
)

# the stages that reset in the middle of a note, and the wrapper that mirrors the reset (None: the stage has no reset)
RESET = {
    "first-order": "_gs_fo_reset",
    "uy": "_gs_uy_reset",
    "case5": "_gs_case5_reset",
    "held": "_gs_held_reset",
    "held-late": "_gs_held_reset",
    "case1": "_gs_case1_reset",
    "case2": "_gs_case2_reset",
    "case3": "_gs_case3_reset",
    "ring4": "_gs_ring_reset",
    "ring6": "_gs_ring_reset",
    "ring-wrap": "_gs_ring_reset",
    "ring-dirty-r": "_gs_ring_reset",
}
BLOCKS = 7
RESET_AT = 3  # the reset comes before this block


@pytest.fixture(scope="module")
def rig():
    return Rig()


def identical_run(rig, stage, reset, seed=4):
    """The stage on a stamped slot with equal L and R input and equal private state; returns (L block, R block) per block."""
    rng = random.Random(seed)
    session = new_session(rig)
    lrec, rrec = stage.l_record(), stage.r_record(rig)
    out = SCRATCH if stage.scratch else LBUF
    regs = stage_regs(stage)
    init_record(session, lrec, stage.size, stage.initial, stage.extra.get("words"))
    setup_blend(session)
    poke_words(session, rrec, [0xFFFFFFFF] * stage.size)
    for i, v in stage.private().items():  # equal private start in both records
        session.poke(rrec + 4 * i, v)
        session.poke(lrec + 4 * i, v)
    if stage.extra.get("prime"):  # equal rings
        poke_words(session, RING_L, ring_pattern(0))
        poke_words(session, R_RING, ring_pattern(0))
    session.poke(rig.data["_gs_stamp"] + 4 * SLOT, HEARTBEAT)
    rbuf = rig.data["_gs_rbuf"] + 128 * SLOT
    blocks = []
    for block in range(BLOCKS):
        if reset and block == RESET_AT:
            call(session, rig.sym[reset], lrec, 0, 0, ())
        for i in stage.controls:
            base, spread = stage.extra["values"][i]
            session.poke(lrec + 4 * i, bits(base + spread * block))
        audio = block_input(rng)
        poke_words(session, LBUF, scaled(audio, 16.0) if stage.r_scale else audio)
        poke_words(session, rbuf, audio)
        call(
            session,
            rig.sym[stage.entry],
            lrec,
            LBUF,
            out,
            stage.args_of(block),
            **regs,
        )
        if stage.scratch:
            run_blend(session, LBUF)
        left = words(session, LBUF, 32)
        blocks.append(
            (
                scaled(left, 1 / 16) if stage.r_scale else left,
                words(session, rbuf, 32),
                audio,
            )
        )
    return blocks


@pytest.mark.parametrize("name", STAGES)
def test_with_equal_input_r_equals_l_bit_for_bit(rig, name):
    stage = STAGES[name]
    blocks = identical_run(rig, stage, RESET.get(name))
    for n, (left, right, _audio) in enumerate(blocks):
        assert left == right, f"block {n}"
        assert any(w & 0x7FFFFFFF for w in left)
        assert all((w & 0x7F800000) != 0x7F800000 for w in left)
    # the stage did something: the mid-note reset and the control changes were not a bypass
    assert any(left != audio for left, _, audio in blocks) or stage.extra.get("dry")


# the held-sample stage reads its input again in the first sample after a reset (its counters are 0), so stale held audio never sounds: its mirror is hygiene
@pytest.mark.parametrize(
    "name", [n for n in STAGES if n in RESET and not n.startswith("held")]
)
def test_a_reset_that_misses_r_breaks_the_identity(rig, name):
    """Negative control: the stock reset on L alone (no mirror) leaves R's history from before the reset."""
    stage = STAGES[name]
    stock = {
        "_gs_fo_reset": "_gs_fo_reset_stock",
        "_gs_uy_reset": "_gs_uy_reset_stock",
        "_gs_held_reset": "_gs_held_reset_stock",
        "_gs_case1_reset": "_gs_case1_reset_stock",
        "_gs_case2_reset": "_gs_case2_reset_stock",
        "_gs_case3_reset": "_gs_case3_reset_stock",
        "_gs_case5_reset": "_gs_case5_reset_stock",
        "_gs_ring_reset": "_gs_ring_reset_stock",
    }[RESET[name]]
    session_rig = rig
    # a stand-in symbol table entry that runs only the stock reset
    original = dict(session_rig.sym)
    session_rig.sym["_gs_stock_only"] = STOCK[stock]
    try:
        blocks = identical_run(session_rig, stage, "_gs_stock_only")
    finally:
        session_rig.sym.clear()
        session_rig.sym.update(original)
    assert any(left != right for left, right, _ in blocks)


def test_every_stage_with_a_reset_is_in_the_table(rig):
    assert set(STAGES) - set(RESET) == {"gain", "shaper-early", "shaper-late"}, (
        "the gain stage restarts through its argument and the shaper has no state; "
        "every other stage has a reset mirror"
    )
    assert RING_WORDS == 8192
