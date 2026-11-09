"""The stage table and the harness of the DN2 R twins (tests/patchkit/test_dsp_stereo_twins.py, test_dsp_stereo_identity.py).

A `Stage` names a twin, the stock stage behind it, the L record, the words the stage shares between the channels (`controls`, copied before the L call) and the
words each channel keeps (`histories`). `run_chain` runs the twin on a stamped slot next to two reference cores that run the stock function alone, one for each
channel; `variant_r` computes R by hand with the stock function (the negative controls). Original firmware is supplied at runtime, never copied into a fixture."""

import dataclasses
import random
import re
from dataclasses import dataclass, field

from support.dn2_stereo_rig import (
    BACK,
    BUFFERS,
    ENGINE,
    FRAME,
    HEARTBEAT,
    REGS,
    STOCK,
    bits,
    call,
    f32,
    new_session,
    poke_words,
    value,
    words,
)
from support.paths import REPO_ROOT

SLOT = 5
# ADM_RING_R_BASE of the generated adapter constants ([adapter.sample_pool] ring_r_base): the 16 R rings of 32 KiB
RING_R_BASE = int(
    re.search(
        r"#define ADM_RING_R_BASE (\d+)",
        (
            REPO_ROOT / "modules/core/adapters/dsp-voice/build/gen/adapter-dn2.inc"
        ).read_text(),
    ).group(1)
)
R_RING = RING_R_BASE + 0x8000 * SLOT
RING_L = (
    0x80100000  # the L ring of the L record in the tests (any DDR address of the model)
)
RING_VARIANT = 0x80200000  # the R ring of a hand-made R pass
RING_WORDS = 8192
LBUF = BUFFERS + 0x80 * SLOT
SCRATCH = BUFFERS + 0x1000  # the L scratch block of case 5


@dataclass
class Stage:
    name: str
    entry: str
    stock: str
    record: int  # L record of slot v: record + stride * v (byte address)
    stride: int
    size: int  # words
    controls: tuple  # shared control words, rewritten before every block
    histories: tuple  # words the stage keeps per channel
    initial: dict  # word -> float, the L record at the start (and the R record's histories are zero)
    args: tuple = (32,)
    scratch: bool = False  # R12 is a scratch output block and a blend follows (case 5)
    blocks: int = 4
    controls_per_block: tuple = ()  # gain: the first stack argument of each block (1 restarts the attack)
    checked: tuple = ()  # words of the R record compared with the reference (default: controls and histories)
    r_scale: bool = False  # the late shaper: the engine scales L by 16 before the stage and by 1/16 after it, the twin does the same on R
    extra: dict = field(default_factory=dict)

    def args_of(self, block):
        if not self.controls_per_block:
            return self.args
        return (
            self.controls_per_block[block % len(self.controls_per_block)],
            *self.args[1:],
        )

    def private(self):
        """The private start of the R record (word -> raw value) and of the right reference: zero histories, or the stage's own `private` words."""
        return self.extra.get("private", dict.fromkeys(self.histories, 0))

    def l_record(self, v=SLOT):
        return self.record + self.stride * v

    def r_record(self, rig, v=SLOT):
        return rig.data[self.extra["rrec"]] + self.stride * v


STAGES = {
    "first-order": Stage(
        "first-order",
        "_gs_fo",
        "_gs_fo_stock",
        ENGINE + 0xE088,
        112,
        28,
        (21, 22, 23),
        (24, 25),
        {21: 0.7, 22: 0.2, 23: 0.1, 24: 0.1, 25: -0.05, 26: 0.3, 27: -0.2},
        extra={
            "rrec": "_gs_fo_rec",
            "values": {21: (0.7, -0.05), 22: (0.2, 0.01), 23: (0.1, 0.02)},
        },
    ),
    "uy": Stage(
        "uy",
        "_gs_uy",
        "_gs_uy_stock",
        ENGINE + 0x12148,
        32,
        8,
        (2, 3, 4, 5),
        (6, 7),
        {2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4, 6: 0.02, 7: -0.03},
        extra={
            "rrec": "_gs_uy_rec",
            "values": {2: (0.6, 0.02), 3: (0.3, -0.01), 4: (0.2, 0.0), 5: (0.4, 0.03)},
        },
    ),
}


BLEND = (
    ENGINE + 0x13E9C + 0x38 * SLOT
)  # the blend's 14-word ramp record (I0 of the engine at the case 5 call)
BLEND_ENTRY, BLEND_STOP = 0x1C9C12, 0x1C98B9
CASE5 = {9: 0.02, 10: -0.03} | {
    11 + 7 * k + d: v
    for k in range(6)
    for d, v in ((0, (0.6, 0.1, 0.15, 0.2, 0.1, 0.08)[k]), (2, 0.0001))
}
CASE5_VALUES = {
    11 + 7 * k + d: (v * (1 + 0.1 * k), v * 0.05)
    for k in range(6)
    for d, v in ((0, (0.6, 0.1, 0.15, 0.2, 0.1, 0.08)[k]), (2, 0.0001))
}
STAGES["gain"] = Stage(
    "gain",
    "_gs_gain",
    "_gs_gain_stock",
    ENGINE + 0x13508,
    44,
    11,
    (),
    (),
    {},
    args=(0, 0, 32),
    blocks=9,
    controls_per_block=(1, 0, 0, 0, 0, 1, 0, 0, 0),
    extra={"rrec": "_gs_gain_rec", "words": (7, 1, 7, bits(0.5), 7, 1, 0, 5, 0, 0, 0)},
)
STAGES["case5"] = Stage(
    "case5",
    "_gs_c5",
    "_gs_c5_stock",
    ENGINE + 0x127C8,
    212,
    53,
    tuple(CASE5_VALUES),
    (9, 10),
    CASE5,
    scratch=True,
    extra={"rrec": "_gs_c5_rec", "values": CASE5_VALUES},
)


def scaled(block, factor):
    """The engine's scaling of a block by a power of two (exact, so Python's float32 product is the core's)."""
    return [bits(f32(value(w) * factor)) for w in block]


SHAPER_VALUES = {0: (0.6, 0.05), 1: (0.2, 0.0), 2: (0.5, 0.04)}
SHAPER_INITIAL = {0: 0.6, 1: 0.2, 2: 0.5, 5: 0.4, 12: 0.3}
for late in (False, True):
    STAGES["shaper-late" if late else "shaper-early"] = Stage(
        "shaper-late" if late else "shaper-early",
        "_gs_shaper_late" if late else "_gs_shaper_early",
        "_gs_shaper_stock",
        ENGINE + 0xE788,
        96,
        24,
        (0, 1, 2),
        (),
        SHAPER_INITIAL,
        checked=(0, 1, 2, 4, 5, 6, 11, 12, 13),
        r_scale=late,
        blocks=6,
        extra={"rrec": "_gs_sh_rec", "values": SHAPER_VALUES},
    )
STAGES["held"] = Stage(
    "held",
    "_gs_held_early",
    "_gs_held_stock",
    ENGINE + 0xEEC8,
    20,
    5,
    (0,),
    (1, 2),
    {0: 0.25},
    checked=(0, 1, 2, 3, 4),
    blocks=5,
    extra={
        "rrec": "_gs_hd_rec",
        "values": {0: (32 / 127, 16 / 127)},
        "words": (0, 0, 0, 1, 1),
    },
)
STAGES["held-late"] = dataclasses.replace(
    STAGES["held"], name="held-late", entry="_gs_held_late"
)

# dispatch cases 1, 2 and 3: the controls and values of the replay fixtures (test_dsp_stereo_shadow_replay.py: small finite coefficients, a nonzero step each block)
C1_CONTROLS = (5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42)
C1_BASE = (0.2, 0.3, 0.1, 0.4, 0.0002, 0.2, 0.0001, 0.1, 0.0001, 0.1, 0.0001)
STAGES["case1"] = Stage(
    "case1",
    "_gs_case1",
    "_gs_case1_stock",
    ENGINE + 0xF308,
    204,
    51,
    C1_CONTROLS,
    (3, 4, 48, 49, 50),
    dict(zip(C1_CONTROLS, C1_BASE, strict=True)) | {3: 0.01, 4: -0.02},
    extra={
        "rrec": "_gs_c1_rec",
        "values": {i: (v, v / 16) for i, v in zip(C1_CONTROLS, C1_BASE, strict=True)},
    },
)
C2_CONTROLS = tuple(12 + 7 * k + d for k in range(9) for d in (0, 2))
C2_BASE = tuple(
    v for v0 in (0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1) for v in (v0, 0.0001)
)
STAGES["case2"] = Stage(
    "case2",
    "_gs_case2",
    "_gs_case2_stock",
    ENGINE + 0xFFC8,
    300,
    75,
    C2_CONTROLS,
    (10, 11),
    dict(zip(C2_CONTROLS, C2_BASE, strict=True)) | {10: 0.01, 11: -0.02},
    extra={
        "rrec": "_gs_c2_rec",
        "values": {i: (v, v / 16) for i, v in zip(C2_CONTROLS, C2_BASE, strict=True)},
    },
)
C3_CONTROLS = {0: 0.12, 1: 0.32, 2: 0.1, 3: 0.12, 4: 0.65, 14: 0.22}
C3_WRITES = tuple(17 + 7 * k + d for k in range(6) for d in (0, 1, 2))
C3_CURRENT = (0.32, 0.12, 0.1 / 4, 0.12, 0.65, -0.22 / 8)
STAGES["case3"] = Stage(
    "case3",
    "_gs_case3",
    "_gs_case3_stock",
    ENGINE + 0x11288,
    236,
    59,
    tuple(C3_CONTROLS),
    (5, 9, 10, 11, 12),
    C3_CONTROLS
    | {17 + 7 * k + d: v for k, v in enumerate(C3_CURRENT) for d in (0, 1)}
    | {5: 0.004, 9: 0.003, 10: -0.002, 11: 0.001, 12: -0.003},
    checked=(*C3_CONTROLS, 5, 9, 10, 11, 12, *C3_WRITES),
    blocks=3,
    extra={
        "rrec": "_gs_c3_rec",
        "values": {i: (v, v / 16) for i, v in C3_CONTROLS.items()},
    },
)


# the ring stage (cases 4 and 6): the records of a ring after the stock reset (history and cursor 0, fill status 1) or of a filled ring near its wrap (the fixtures
# of test_dsp_stereo_shadow_replay.py). Word 16 is the byte pointer of the ring; the L ring is at RING_L, the twin's R ring at R_RING.
def ring_pattern(channel):
    return [bits(((i % 97) - 48) / (512 + 97 * channel)) for i in range(RING_WORDS)]


def ring_prime(l_fill="pattern", r_fill="pattern", reference_fill=None):
    """Prime function of a ring stage: who is stereo-l, stereo-r, left, right (the reference cores) or variant-l, variant-r (a hand-made pass); a ring is a
    pattern (unequal for the two channels) or zeros. `reference_fill` is the right reference's ring when it differs from the twin's R ring."""

    def prime(session, who, address):
        right_side = who in ("stereo-r", "right", "variant-r")
        fill = (
            r_fill
            if who == "stereo-r"
            else (reference_fill or r_fill)
            if right_side
            else l_fill
        )
        poke_words(
            session,
            address,
            ring_pattern(int(right_side)) if fill == "pattern" else [0] * RING_WORDS,
        )

    return prime


def ring_words(cursor, coordinate, status):
    w = [0] * 18
    w[6], w[7], w[8], w[9] = 0x3F7EB852, 0x20, 0x20, 0x3D000000
    w[11], w[12] = 0x44FA0000, 0x473B8000
    w[13], w[14], w[15] = bits(coordinate), cursor, bits(32.0)
    w[16], w[17] = RING_L, status
    return tuple(w)


def ring_stage(name, entry, cursor, coordinate, status, prime):
    return Stage(
        name,
        entry,
        "_gs_ring_stock",
        ENGINE + 0x12348,
        72,
        18,
        (2, 10),
        (0, 13, 14, 16, 17),
        {2: 0.5, 10: 0.5, 3: 0.5, 4: 0.5},
        checked=(0, 2, 3, 4, 5, 10, 13, 14, 17),
        blocks=4,
        extra={
            "rrec": "_gs_rg_rec",
            "values": {2: (0.5, 0.0), 10: (0.5, 1 / 32)},
            "words": ring_words(cursor, coordinate, status),
            "private": {0: 0, 13: bits(coordinate), 14: cursor, 17: status},
            "prime": prime,
            "ring_word": 16,
            # a ring with fill status 1 has not been read yet: the block passes dry
            "dry": status == 1,
        },
    )


STAGES["ring4"] = ring_stage(
    "ring4", "_gs_ring4", 0, 8160.0, 1, ring_prime("pattern", "pattern")
)
STAGES["ring6"] = dataclasses.replace(STAGES["ring4"], name="ring6", entry="_gs_ring6")
STAGES["ring-wrap"] = ring_stage(
    "ring-wrap", "_gs_ring4", 8190, 8158.0, 0, ring_prime("pattern", "pattern")
)
# a dirty R ring against a clean reference ring: only the fill status can hide the difference
STAGES["ring-dirty-r"] = ring_stage(
    "ring-dirty-r", "_gs_ring4", 0, 8160.0, 1, ring_prime("pattern", "pattern", "zero")
)
# the same with a ring that has been written once (fill status 0): the dirty backing is audible, so R differs from the clean reference
DIRTY_FILLED = ring_stage(
    "ring-dirty-r-filled",
    "_gs_ring4",
    8190,
    8158.0,
    0,
    ring_prime("pattern", "pattern", "zero"),
)


def init_record(session, address, size, initial, words_=None):
    poke_words(session, address, list(words_) if words_ else [0] * size)
    for i, v in initial.items():
        session.poke(address + 4 * i, bits(v))


def run_blend(session, buffer, scratch=SCRATCH):
    """The caller's continuation after the case 5 call: the stock blend 0x1c9c12 to 0x1c98b9 on the buffer and the scratch block."""
    session.poke(ENGINE + 0x13808, scratch)
    session.symbols["_blend"] = BLEND_ENTRY
    session.runner.breakpoints = frozenset((BACK, BLEND_STOP))
    regs = {
        **REGS,
        "I0": BLEND,
        "I1": ENGINE,
        "I5": buffer,
        "R9": 32,
        "I6": FRAME,
        "I7": FRAME - 8,
    }
    result = session.call("_blend", regs, max_steps=4000)
    assert (
        result["halt"]["reason"] == "breakpoint" and result["final_pc_sw"] == BLEND_STOP
    ), result


def block_input(rng, scale=0.4):
    return [bits(rng.uniform(-scale, scale)) for _ in range(32)]


def stage_regs(stage):
    """The registers of the caller that the stage's call keeps (case 5: the engine's I0, I1, I5 and R9 at the call, read by the blend that follows)."""
    return {"I0": BLEND, "I1": ENGINE, "I5": LBUF, "R9": 32} if stage.scratch else {}


def setup_blend(session):
    poke_words(session, BLEND, [0] * 14)
    for i, v in {0: 1.0, 2: -0.01, 7: 0.0, 9: 0.01}.items():
        session.poke(BLEND + 4 * i, bits(v))


def run_chain(rig, stage, blocks=None, seed=1):
    """Stereo core with the wrapper, left and right reference cores with the stock function. Returns the three cores' observations per block."""
    blocks = blocks or stage.blocks
    rng = random.Random(seed)
    stereo, left, right = (new_session(rig) for _ in range(3))
    lrec, rrec = stage.l_record(), stage.r_record(rig)
    stock_pc = STOCK[stage.stock]
    out = SCRATCH if stage.scratch else LBUF
    regs = stage_regs(stage)
    for session in (stereo, left, right):
        init_record(session, lrec, stage.size, stage.initial, stage.extra.get("words"))
        setup_blend(session)
    # R state of the stereo core: what the adapter's clear leaves (histories), unwritten (NaN) words elsewhere
    poke_words(stereo, rrec, [0xFFFFFFFF] * stage.size)
    private = stage.private()
    for i, v in private.items():
        stereo.poke(rrec + 4 * i, v)
    # the right reference's own record: the same private start, controls as the left ones
    for i, v in private.items():
        right.poke(lrec + 4 * i, v)
    prime = stage.extra.get("prime")
    if prime:
        prime(stereo, "stereo-l", RING_L)
        prime(stereo, "stereo-r", R_RING)
        prime(left, "left", RING_L)
        prime(right, "right", RING_L)
    stereo.poke(rig.data["_gs_stamp"] + 4 * SLOT, HEARTBEAT)
    rbuf = rig.data["_gs_rbuf"] + 128 * SLOT
    history = []
    for block in range(blocks):
        l_in, r_in = block_input(rng), block_input(rng, 0.3)
        for session in (stereo, left, right):
            for i in stage.controls:
                base, spread = stage.extra["values"][i]
                session.poke(lrec + 4 * i, bits(base + spread * block))
        poke_words(stereo, LBUF, l_in)
        poke_words(left, LBUF, l_in)
        poke_words(stereo, rbuf, r_in)
        poke_words(right, LBUF, scaled(r_in, 16.0) if stage.r_scale else r_in)
        before_l_ctrl = words(stereo, lrec, stage.size)
        args = stage.args_of(block)
        call(stereo, rig.sym[stage.entry], lrec, LBUF, out, args, **regs)
        call(left, stock_pc, lrec, LBUF, out, args, **regs)
        call(right, stock_pc, lrec, LBUF, out, args, **regs)
        if stage.scratch:
            for session in (stereo, left, right):
                run_blend(session, LBUF)
        history.append(
            {
                "l": words(stereo, LBUF, 32),
                "l_ref": words(left, LBUF, 32),
                "r": words(stereo, rbuf, 32),
                "r_ref": scaled(words(right, LBUF, 32), 1 / 16)
                if stage.r_scale
                else words(right, LBUF, 32),
                "l_rec": words(stereo, lrec, stage.size),
                "l_rec_ref": words(left, lrec, stage.size),
                "r_rec": words(stereo, rrec, stage.size),
                "r_rec_ref": words(right, lrec, stage.size),
                "l_in": l_in,
                "r_in": r_in,
                "ctrl": before_l_ctrl,
            }
        )
        if prime:
            history[-1] |= {
                "ring_l": words(stereo, RING_L, RING_WORDS),
                "ring_l_ref": words(left, RING_L, RING_WORDS),
                "ring_r": words(stereo, R_RING, RING_WORDS),
                "ring_r_ref": words(right, RING_L, RING_WORDS),
            }
    return history, stereo


SCRATCH_R = SCRATCH + 0x200
RREC = 0x20190000  # a separate record for the variants


def variant_r(rig, stage, history, mode, omit_scale=False):
    """R by hand with the stock function: `early` copies the control words from the L record before the L call (what the twin does, so it must give the twin's R),
    `late` copies them after the L call, `shared` runs R on the L record."""
    session = new_session(rig)
    lrec = stage.l_record()
    rrec = lrec if mode == "shared" else RREC
    regs = stage_regs(stage)
    r_scale = stage.r_scale and not omit_scale
    init_record(session, lrec, stage.size, stage.initial, stage.extra.get("words"))
    prime = stage.extra.get("prime")
    if mode != "shared":
        init_record(session, rrec, stage.size, {}, [0] * stage.size)
        for i, v in stage.private().items():
            session.poke(rrec + 4 * i, v)
        if prime:
            session.poke(rrec + 4 * stage.extra["ring_word"], RING_VARIANT)
    if prime:
        prime(session, "variant-l", RING_L)
        prime(session, "variant-r", RING_VARIANT)
    setup_blend(session)
    out = SCRATCH if stage.scratch else LBUF
    outputs = []
    for block, h in enumerate(history):
        for i in stage.controls:
            base, spread = stage.extra["values"][i]
            session.poke(lrec + 4 * i, bits(base + spread * block))
        poke_words(session, LBUF, h["l_in"])
        copy = [i for i in range(stage.size) if i not in stage.histories]
        if stage.size == 5:  # the held stage: the counters are advanced by the call
            copy = [0, 3, 4]
        if mode == "early":
            for i in copy:
                session.poke(rrec + 4 * i, session.peek(lrec + 4 * i))
        call(session, STOCK[stage.stock], lrec, LBUF, out, stage.args_of(block), **regs)
        if stage.scratch:
            run_blend(session, LBUF)
        if mode == "late":
            for i in copy:
                session.poke(rrec + 4 * i, session.peek(lrec + 4 * i))
        poke_words(session, LBUF, scaled(h["r_in"], 16.0) if r_scale else h["r_in"])
        call(
            session,
            STOCK[stage.stock],
            rrec,
            LBUF,
            SCRATCH_R if stage.scratch else LBUF,
            stage.args_of(block),
            **regs,
        )
        if stage.scratch:
            run_blend(session, LBUF, SCRATCH_R)
        out_r = words(session, LBUF, 32)
        outputs.append(scaled(out_r, 1 / 16) if r_scale else out_r)
    return outputs
