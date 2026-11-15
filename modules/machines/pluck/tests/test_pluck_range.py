"""digikit/pluck, model only: the loop period is servable for every note and TUNE word, and the string stays finite and bounded for 200 blocks.

On the Digitakt II an LFO on TUNE silenced every track: the loop period P = fs / f0 left what the delay line can serve
(docs/findings/51-pluck-tune-guard.md). The routine's corners run on the kit (tests/rig/pluck.toml). Kept until T1g phase 5 ports it
(docs/plans/ledgers/t1g-retirement.md "Kept until phase 5")."""

import numpy as np
import pluck_ref
import pytest

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
