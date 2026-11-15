"""digikit/pluck, model only: the loop period is servable for every note and TUNE word, and the string stays finite and bounded for 200 blocks.

On the Digitakt II an LFO on TUNE silenced every track: the loop period P = fs / f0 left what the delay line can serve
(docs/findings/51-pluck-tune-guard.md). The routine's corners (output against the model) run on the kit (tests/rig/pluck.toml); the finite state words after them are kept here. Kept until T1g phase 5 ports it
(docs/plans/ledgers/t1g-retirement.md "Kept until phase 5")."""

import numpy as np
import pluck_ref
import pytest
from pluck_rig import SKIP_NO_SELACHE, PluckRig, val

TUNES = (0, 0x1000, 0x4000, 0x7C00, 0x7F00, 0xFFFF)
BOUND = 2.0  # the excitation peaks near 1.5 at velocity 127, color 0x7f00; a string only loses energy


# kept until T1g phase 5 ports it
def test_the_loop_period_is_servable_for_every_note_and_tune_word():
    for bright in (0, 0x7F00):
        for note in range(128):
            for tune in (*range(0, 0x10000, 0x100), 0x7F00, 0xFFFF):
                q = pluck_ref.Params(note << 8, tune, 0x4000, 0, bright, 0, 100 << 8)
                assert 2 <= q.N <= 4090, (note, tune, q.N)
                r = float(q.P - q.tau)
                r = min(max(r, 2.501), 4091.49) - q.N
                assert 0.5 <= r < 1.5, (note, tune, r)
                assert abs(float(q.a)) <= 0.9, (note, tune, q.a)
                assert np.isfinite(q.g) and 0.0 <= float(q.g) <= 1.0, (note, tune, q.g)
                assert np.isfinite(q.inc) and np.isfinite(q.w2)


# kept until T1g phase 5 ports it
@pytest.mark.parametrize("tune", TUNES)
def test_the_string_stays_finite_and_bounded_for_200_blocks(tune):
    for note in range(128):
        q = pluck_ref.Params(note << 8, tune, 0x7F00, 0, 0x7F00, 0x7F00, 127 << 8)
        s = pluck_ref.Pluck()
        s.note_on(q)
        y = np.concatenate([s.block(q) for _ in range(200)])
        assert np.all(np.isfinite(y)), (note, tune)
        assert float(np.max(np.abs(y))) < BOUND, (note, tune)
        assert np.all(np.isfinite(s.line)) and np.isfinite(s.lp) and np.isfinite(s.ap)


# kept until T1g phase 5 ports it: the final string state words (lp', ap') are finite after the corner cases, so no later block inherits a bad state
@SKIP_NO_SELACHE
@pytest.mark.parametrize(
    "note,tune",
    [(0, 0), (60, 0x0400), (100, 0x7C00), (127, 0x7F00), (127, 0xFFFF)],
    ids=["note0", "low-tune", "note100-high-tune", "note127", "note127-max-word"],
)
def test_the_string_state_is_finite_after_the_corner_cases(note, tune):
    rig = PluckRig()
    rig.setup(note=note, tune=tune / 256.0)
    for _ in range(8):
        y, out = rig.block()
        assert np.all(np.isfinite(y))
        state = [val(w) for w in out["state"][3:5]]
        assert np.all(np.isfinite(state)), state
