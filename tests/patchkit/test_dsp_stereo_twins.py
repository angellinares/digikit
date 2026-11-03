"""The R twins of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 2), in the style of test_dsp_stereo_shadow_replay.py.

A twin wrapper (`_gs_fo`, `_gs_uy`, `_gs_gain`, `_gs_c5`, `_gs_shaper_*`, `_gs_held_*`, `_gs_case1` to `_gs_case3`, `_gs_ring4`, `_gs_ring6`) is called the way the engine
calls the stock stage: a linked call, R4 = the L record, R8 = R12 = the voice buffer, the arguments on the stack. For a stamped slot it runs the stock function on L,
then on a private R record and the R buffer. Reference channels run the stock function on independent concrete cores with private state; the control words of each
block are the same for all three. Negative controls: a shared history, and the stage's control words copied after the L call, give a different R. Original firmware
is supplied at runtime, never copied into a fixture."""

import random

import pytest
from support.dn2_stereo_rig import (
    HEARTBEAT,
    STOCK,
    Rig,
    call,
    new_session,
    poke_words,
    words,
)
from support.dn2_stereo_stages import (
    DIRTY_FILLED,
    LBUF,
    R_RING,
    RING_L,
    RING_WORDS,
    SCRATCH,
    SLOT,
    STAGES,
    block_input,
    init_record,
    run_chain,
    stage_regs,
    variant_r,
)


@pytest.fixture(scope="module")
def rig():
    return Rig()


@pytest.mark.parametrize("name", STAGES)
def test_the_twin_matches_two_isolated_mono_channels(rig, name):
    stage = STAGES[name]
    history, stereo = run_chain(rig, stage)
    for h in history:
        assert h["l"] == h["l_ref"], "L is the stock path"
        assert h["l_rec"] == h["l_rec_ref"]
        assert h["r"] == h["r_ref"], "R is the stock stage on independent state"
        words_checked = (
            range(stage.size)
            if stage.name == "gain"
            else stage.checked or (*stage.controls, *stage.histories)
        )
        for i in words_checked:
            assert h["r_rec"][i] == h["r_rec_ref"][i], f"R record word {i}"
        assert h["l"] != h["r"]
        if not stage.extra.get("dry"):
            assert h["r"] != h["r_in"] and h["l"] != h["l_in"]
        assert all((w & 0x7F800000) != 0x7F800000 for w in h["l"] + h["r"])
    # the histories are independent: both are live and different
    assert not stage.histories or any(
        history[-1]["l_rec"][i] != history[-1]["r_rec"][i] for i in stage.histories
    )


@pytest.mark.parametrize("name", STAGES)
def test_a_slot_that_is_not_stamped_is_the_plain_stock_call(rig, name):
    stage = STAGES[name]
    stereo, plain = new_session(rig), new_session(rig)
    lrec, rrec = stage.l_record(), stage.r_record(rig)
    for session in (stereo, plain):
        init_record(session, lrec, stage.size, stage.initial)
        poke_words(session, LBUF, block_input(random.Random(3)))
    poke_words(stereo, rrec, [0x12345678] * stage.size)
    stereo.poke(rig.data["_gs_stamp"] + 4 * SLOT, HEARTBEAT - 1)
    stereo.poke(rig.data["_gs_rbuf"] + 128 * SLOT, 0x12345678)
    out = SCRATCH if stage.scratch else LBUF
    call(stereo, rig.sym[stage.entry], lrec, LBUF, out, stage.args, **stage_regs(stage))
    call(plain, STOCK[stage.stock], lrec, LBUF, out, stage.args, **stage_regs(stage))
    assert words(stereo, LBUF, 32) == words(plain, LBUF, 32)
    assert words(stereo, lrec, stage.size) == words(plain, lrec, stage.size)
    assert words(stereo, rrec, stage.size) == [0x12345678] * stage.size, (
        "R state untouched"
    )
    assert stereo.peek(rig.data["_gs_rbuf"] + 128 * SLOT) == 0x12345678


@pytest.mark.parametrize("name", STAGES)
def test_the_harness_reproduces_the_twin_with_the_early_capture(rig, name):
    """Positive control for the two negative controls below."""
    stage = STAGES[name]
    history, _ = run_chain(rig, stage)
    assert variant_r(rig, stage, history, "early") == [h["r"] for h in history]


@pytest.mark.parametrize("name", STAGES)
def test_a_shared_history_gives_a_different_r(rig, name):
    stage = STAGES[name]
    history, _ = run_chain(rig, stage)
    assert variant_r(rig, stage, history, "shared") != [h["r"] for h in history]


def test_controls_copied_after_the_l_call_give_a_different_r_for_the_gain_stage(rig):
    """The L call advances the stage's control words once per block; an R pass that starts from the advanced words is a different gain trajectory."""
    stage = STAGES["gain"]
    history, _ = run_chain(rig, stage)
    assert variant_r(rig, stage, history, "late") != [h["r"] for h in history]


def test_the_late_shaper_without_the_scalings_on_r_gives_a_different_r(rig):
    """The engine scales L by 16 before the late stage and by 1/16 after it; R without that is another point on the curve."""
    stage = STAGES["shaper-late"]
    history, _ = run_chain(rig, stage)
    assert variant_r(rig, stage, history, "early", omit_scale=True) != [
        h["r"] for h in history
    ]


def test_the_callee_saved_registers_survive_the_stereo_path(rig):
    """`call` checks them for every stage above; this one holds the identity under hostile context registers too."""
    stage = STAGES["first-order"]
    session = new_session(rig)
    init_record(session, stage.l_record(), stage.size, stage.initial)
    session.poke(rig.data["_gs_stamp"] + 4 * SLOT, HEARTBEAT)
    for i in stage.histories:
        session.poke(stage.r_record(rig) + 4 * i, 0)
    poke_words(session, LBUF, block_input(random.Random(2)))
    call(
        session,
        rig.sym[stage.entry],
        stage.l_record(),
        LBUF,
        LBUF,
        stage.args,
        M6=1,
        M5=0,
        M14=1,
        M7=0xFFFFFFFF,
    )


# ---- the ring stage: its own ring in the DSP's DDR ----


def test_the_r_pass_of_the_ring_stage_uses_its_own_ring_at_the_profile_base(rig):
    stage = STAGES["ring4"]
    history, stereo = run_chain(rig, stage)
    rrec = stage.r_record(rig)
    assert stereo.peek(rrec + 4 * 16) == R_RING, "word 16 of the R record, not L's"
    assert stereo.peek(stage.l_record() + 4 * 16) == RING_L
    for h in history:
        assert h["ring_l"] == h["ring_l_ref"], "the R pass never writes the L ring"
        assert h["ring_r"] == h["ring_r_ref"], "the R ring is the mono channel's ring"
    first = history[0]
    pattern_l = [
        i for i in range(RING_WORDS) if first["ring_l"][i] != history[-1]["ring_l"][i]
    ]
    pattern_r = [
        i for i in range(RING_WORDS) if first["ring_r"][i] != history[-1]["ring_r"][i]
    ]
    assert pattern_l and pattern_r, "both rings were written"


def test_a_dirty_r_ring_is_hidden_by_the_fill_status_of_a_fresh_note(rig):
    """The stock reset leaves the backing of a ring as it is and sets the fill status; R starts the same way, so its ring needs no clear: a ring full of old audio
    gives the same R as a ring of zeros until the write cursor has wrapped once."""
    history, _ = run_chain(rig, STAGES["ring-dirty-r"])
    assert all(h["r"] == h["r_ref"] for h in history)
    assert any(w != 0 for w in history[0]["ring_r"][:4000]), "the R ring was dirty"
    assert any(h["r"] != h["l"] for h in history)


def test_a_dirty_r_ring_is_audible_once_the_ring_is_filled(rig):
    """Negative control for the test above: with the fill status of a filled ring the backing matters, so the test can tell."""
    history, _ = run_chain(rig, DIRTY_FILLED)
    assert any(h["r"] != h["r_ref"] for h in history)
