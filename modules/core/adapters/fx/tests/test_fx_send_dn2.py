"""core/fx, the send route on the Digitone II (modules/core/adapters/fx/src/dsp/fxsend-dn2.s, finding 67): the slot mix on the concrete SHARC+ core.

The Digitone II has no track buses before its master function: the send bus is the sum over the 16 voice slots of the gain of the slot's track times the slot's bus
(the map slot -> track at `dsp.fx.value.send.slot_map`). The model is the routine's own float32 arithmetic, as in tests/patchkit/test_fx_send.py (gain, ramp, return add), slots summed in order."""

import random

import pytest
from support.fx_rig import TRACKS, FxRig
from test_fx_send import (
    AMOUNTS,
    SIGNALS,
    STEP,
    add_return,
    f32,
    gain,
    send_bus,
    send_row,
)

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


# 0x3d042108: the master function's fade of a flagged slot, w_k = 1 - k / 31 (w_0 = 1)
FADE_STEP = f32(1.0 / 31.0)


def faded(x):
    """A flagged slot's bus as the routine scales it (finding 67): each channel times w_k, w_0 = 1.0, w_(k+1) = w_k - 1/31, in float32."""
    out = list(x)
    for ch in (0, 32):
        w = 1.0
        for k in range(32):
            out[ch + k] = f32(x[ch + k] * w)
            w = f32(w - FADE_STEP)
    return out


def slot_bus(amounts, old, slot_track, slots, fade=()):
    """The bus of one block: for each slot in order, g_k * its bus with the gain of its track (g_k = old + (k + 1) * step, added per sample); returns (bus, the new gains per track).
    SLOTS: slot number -> its 64 floats; slots not in it are silent. FADE: the slots whose flag byte is set (their bus is scaled by `faded` first)."""
    new = [gain(b) for b in (tuple(amounts) + (0,) * TRACKS)[:TRACKS]]
    bus = [0.0] * 64
    for v in range(TRACKS):
        t = slot_track[v]
        if new[t] == 0.0 and old[t] == 0.0:
            continue
        step = f32(f32(new[t] - old[t]) * STEP)
        g = old[t]
        x = slots.get(v, [0.0] * 64)
        if v in fade:
            x = faded(x)
        for k in range(32):
            g = f32(g + step)
            for ch in (0, 32):
                bus[ch + k] = f32(bus[ch + k] + f32(x[ch + k] * g))
    return bus, new


def dn2_rig(slot_track, slots, fade=(), **kw) -> FxRig:
    rig = FxRig([send_row(**kw)], device="dn2")
    rig.set_slotmap(slot_track)
    rig.set_fade(fade)
    for v, x in slots.items():
        rig.set_slot(v, x)
    return rig


def test_a_chord_is_the_sum_of_its_slots():
    """Three slots sound track 3 (a chord and a release tail), one track 5, one has no amount: the bus and the return equal the float32 model bit for bit (ramp block, steady blocks, a change of amount)."""
    slot_track = [0] * TRACKS
    for v, t in ((0, 3), (1, 3), (2, 5), (7, 3), (9, 9)):
        slot_track[v] = t
    slots = {v: SIGNALS[v] for v in (0, 1, 2, 7, 9)}
    amounts = tuple({3: 127, 5: 64, 9: 0}.get(t, 0) for t in range(TRACKS))
    rig = dn2_rig(slot_track, slots, amounts=amounts)
    ret = [f32(0.01 * k) for k in range(64)]
    old = [0.0] * TRACKS
    for n, mode in enumerate(("in", "steady", "steady")):
        rig.set_return(ret)
        bus, old = slot_bus(amounts, old, slot_track, slots)
        b = rig.send_block()
        assert b.returned and b.ctx_bad == [] and b.regs_bad == [], (n, b.halt)
        assert rig.bus() == tuple(bus), n
        wet = tuple(f32(x * 0.5) for x in bus)
        assert b.ret == add_return(ret, wet, 1.0, mode), n
        assert rig.prev_gains(0) == tuple(old)
        assert b.master == (0.0,) * 64
    amounts = tuple({3: 32, 5: 127, 9: 100}.get(t, 0) for t in range(TRACKS))
    rig.set_amounts(0, amounts)
    for n in range(2):
        rig.set_return(ret)
        bus, old = slot_bus(amounts, old, slot_track, slots)
        b = rig.send_block()
        assert rig.bus() == tuple(bus), n
        assert b.ret == add_return(
            ret, tuple(f32(x * 0.5) for x in bus), 1.0, "steady"
        ), n
    # the chord is one track: the same sum as one bus with the three slots added in slot order
    assert any(v != 0.0 for v in bus)


def test_with_one_slot_for_each_track_the_slot_mix_is_the_track_mix():
    """The identity map and slot v = track v: the Digitone II object gives the Digitakt II object's bus."""
    slot_track = list(range(TRACKS))
    slots = {t: SIGNALS[t] for t in range(TRACKS)}
    rig = dn2_rig(slot_track, slots)
    old = [0.0] * TRACKS
    for _ in range(3):
        bus, old = send_bus(AMOUNTS, old)
        rig.send_block()
        assert rig.bus() == tuple(bus)


def test_a_free_slot_adds_nothing_whatever_its_map_entry_says():
    """Every map entry names track 3 (amount 127), only slot 4 sounds: the bus is that slot's, and with no sounding slot the bus is silent and the return is not touched."""
    slot_track = [3] * TRACKS
    amounts = tuple(127 if t == 3 else 0 for t in range(TRACKS))
    rig = dn2_rig(slot_track, {4: SIGNALS[4]}, amounts=amounts)
    old = [0.0] * TRACKS
    for _ in range(3):
        bus, old = slot_bus(amounts, old, slot_track, {4: SIGNALS[4]})
        rig.send_block()
        assert rig.bus() == tuple(bus)
    assert rig.bus() == tuple(f32(x * gain(127)) for x in SIGNALS[4])
    rng = random.Random(3)
    silent = dn2_rig(slot_track, {}, amounts=amounts)
    for _ in range(4):
        ret = [f32(rng.uniform(-1.0, 1.0)) for _ in range(64)]
        silent.set_return(ret)
        b = silent.send_block()
        assert b.ret == tuple(ret)
        assert silent.bus() == (0.0,) * 64


def test_a_sound_on_a_track_without_an_amount_adds_nothing():
    slot_track = [0] * TRACKS
    slot_track[6] = 8
    amounts = tuple(127 if t == 2 else 0 for t in range(TRACKS))
    rig = dn2_rig(slot_track, {6: SIGNALS[6]}, amounts=amounts)
    assert rig.send_block().returned
    for _ in range(2):
        rig.send_block()
        assert rig.bus() == (0.0,) * 64


def test_a_fading_slot_is_scaled_like_the_master_function_scales_it():
    """The flag byte of a slot is set for one block (the DSP gives the slot to a voice on another track; the map follows a block later): the slot enters the bus with the weights 1 - k / 31, bit for bit
    the float32 model, in the ramp block and in a steady block; the slot next to it, the same track and not flagged, is added whole; a flag on a slot whose track has no amount adds nothing."""
    slot_track = [0] * TRACKS
    for v, t in ((0, 3), (1, 3), (2, 5), (7, 3)):
        slot_track[v] = t
    slots = {v: SIGNALS[v] for v in (0, 1, 2, 7)}
    amounts = tuple({3: 127, 5: 64}.get(t, 0) for t in range(TRACKS))
    for fade in ({1}, {0, 7}, {2}, {4, 9, 11}, set(range(TRACKS))):
        rig = dn2_rig(slot_track, slots, fade=fade, amounts=amounts)
        ret = [f32(0.01 * k) for k in range(64)]
        old = [0.0] * TRACKS
        for n, mode in enumerate(("in", "steady")):
            rig.set_return(ret)
            bus, old = slot_bus(amounts, old, slot_track, slots, fade)
            b = rig.send_block()
            assert b.returned and b.ctx_bad == [] and b.regs_bad == [], (
                fade,
                n,
                b.halt,
            )
            assert rig.bus() == tuple(bus), (fade, n)
            assert b.ret == add_return(
                ret, tuple(f32(x * 0.5) for x in bus), 1.0, mode
            ), (fade, n)
    # the fade is not a gain: a flagged slot differs from the whole one, the whole one is what the other tests pin
    whole, _ = slot_bus(amounts, [0.0] * TRACKS, slot_track, slots)
    flagged, _ = slot_bus(amounts, [0.0] * TRACKS, slot_track, slots, {1})
    assert whole != flagged
    # the next block, the flag back to 0: the bus is the whole slots again
    rig = dn2_rig(slot_track, slots, fade={1}, amounts=amounts)
    rig.send_block()
    rig.set_fade(())
    old = slot_bus(amounts, [0.0] * TRACKS, slot_track, slots, {1})[1]
    bus, _ = slot_bus(amounts, old, slot_track, slots)
    rig.send_block()
    assert rig.bus() == tuple(bus)


def test_the_slot_mix_keeps_the_registers_and_the_stores_of_the_hook():
    """24 hostile contexts give every register back (the send hook's contract on this device too)."""
    slot_track = [v % 5 for v in range(TRACKS)]
    slots = {v: SIGNALS[v] for v in range(TRACKS)}
    for seed in range(24):
        rig = FxRig([send_row()], device="dn2", seed=seed)
        rig.set_slotmap(slot_track)
        rig.set_fade({1, 4} if seed % 2 else ())
        for v, x in slots.items():
            rig.set_slot(v, x)
        b = rig.send_block()
        assert b.returned and b.ctx_bad == [] and b.regs_bad == [], (seed, b.halt)


def test_the_slot_mix_costs():
    """Printed for the finding: instructions of the send hook with one row and 1 slot, 4 slots and 16 slots sounding."""
    slot_track = list(range(TRACKS))
    for n in (1, 4, 16):
        slots = {v: SIGNALS[v] for v in range(n)}
        amounts = tuple(127 if t < n else 0 for t in range(TRACKS))
        rig = dn2_rig(slot_track, slots, amounts=amounts)
        rig.send_block(ctx="friendly")
        b = rig.send_block(ctx="friendly")
        print(f"DN2 send hook, one row, {n} slots with an amount: {b.instructions}")
        assert b.returned
