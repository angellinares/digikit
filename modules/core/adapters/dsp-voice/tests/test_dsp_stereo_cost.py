"""The cost of the R twins of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 3), in instructions on the concrete core (1 instruction = 1 cycle, no stalls).

A twin costs its stock call again plus the copy of the shared words and its own bookkeeping, so a stamped call stays below the stock call plus the unstamped wrapper
(13 instructions) plus the ceiling of the stage; an unstamped slot costs the stock call and 13 instructions. The ceilings are the measured extras rounded up by about 10 %;
a twin that grows past them changes the budget of section 3 and the cap of eight stereo slots. Original firmware is supplied at runtime."""

import random

import pytest
from support.dn2_stereo_rig import HEARTBEAT, Rig, call, new_session, poke_words
from support.dn2_stereo_stages import (
    LBUF,
    R_RING,
    RING_L,
    SCRATCH,
    SLOT,
    STAGES,
    block_input,
    init_record,
    setup_blend,
    stage_regs,
)

# stage -> ceiling of (stamped twin call - unstamped wrapper call); the unstamped call is the stock call and 13 instructions
CEILING = {
    "first-order": 800,
    "uy": 300,
    "gain": 420,
    "case5": 1160,
    "shaper-early": 650,
    "shaper-late": 870,
    "held": 640,
    "case1": 750,
    "case2": 1400,
    "case3": 2520,
    "ring4": 1750,
    "ring-wrap": 1680,
}
WRAPPER_CHECK = 13


@pytest.fixture(scope="module")
def rig():
    return Rig()


def instructions(rig, stage, stamped):
    session = new_session(rig)
    lrec, rrec = stage.l_record(), stage.r_record(rig)
    init_record(session, lrec, stage.size, stage.initial, stage.extra.get("words"))
    setup_blend(session)
    for i, v in stage.private().items():
        session.poke(rrec + 4 * i, v)
    if stage.extra.get("prime"):
        stage.extra["prime"](session, "stereo-l", RING_L)
        stage.extra["prime"](session, "stereo-r", R_RING)
    session.poke(
        rig.data["_gs_stamp"] + 4 * SLOT, HEARTBEAT if stamped else HEARTBEAT - 1
    )
    poke_words(session, LBUF, block_input(random.Random(1)))
    poke_words(
        session, rig.data["_gs_rbuf"] + 128 * SLOT, block_input(random.Random(2))
    )
    out = SCRATCH if stage.scratch else LBUF
    result = call(
        session,
        rig.sym[stage.entry],
        lrec,
        LBUF,
        out,
        stage.args_of(1),
        **stage_regs(stage),
    )
    return result["instructions"]


@pytest.mark.parametrize("name", CEILING)
def test_a_twin_costs_the_stock_call_again_within_its_ceiling(rig, name):
    stage = STAGES[name]
    unstamped = instructions(rig, stage, False)
    stamped = instructions(rig, stage, True)
    assert stamped > 2 * (unstamped - WRAPPER_CHECK), "the R pass runs the stock stage"
    assert stamped - unstamped <= CEILING[name]
