"""digikit/pluck on the concrete SHARC+ core, against the float32 model of pluck_ref.py.

The wrapper (core/dsp-voice) and the routine are assembled from the module sources and run block by block the way
the engine calls them (see modules/core/adapters/dsp-voice/tests/test_dsp_voice.py). The routine's delay line (reserved, unloaded L2 on the
device) is carried from call to call as DM words. Skips when Selache is not built."""

import numpy as np
import pytest
from pluck_rig import SKIP_NO_SELACHE, PluckRig, model_blocks, run_blocks

pytestmark = SKIP_NO_SELACHE


@pytest.mark.parametrize(
    "kw",
    [
        {},
        {
            "note": 48,
            "decay": 0x7000,
            "pick": 0x2000,
            "bright": 0x2000,
            "color": 0x3000,
        },
        {"note": 84, "tune": 66.0, "vel": 127, "bright": 0x7F00, "color": 0x7F00},
        {"note": 33, "pick": 0x7F00, "decay": 0x1000, "bright": 0},
    ],
    ids=["default", "low", "high-tuned", "pick-max"],
)
def test_the_routine_matches_the_float32_model(kw):
    rig = PluckRig()
    n = 40
    got, _ = run_blocks(rig, n, **kw)
    want = model_blocks(n, **kw)
    assert np.all(np.isfinite(got))
    assert np.max(np.abs(want)) > 0.05
    err = float(np.max(np.abs(got - want)))
    # one rounding differs now and then (FLOAT of the xorshift word, TRUNC of the delay); the audio agrees to 1e-5
    assert err < 2e-5 * max(1.0, float(np.max(np.abs(want)))), err


def test_instruction_counts_per_block():
    rig = PluckRig()
    rig.setup(note=48)
    _, first = rig.block()
    _, steady = rig.block()
    # note-on block includes the excitation (N + 1 samples of noise, mean, comb); the steady block is 32 samples
    # 1362 before the runtime guard (finding 51), +414 with it (the knob clamp and the output check of core/dsp-voice).
    # Measured 1939: the parameter views (docs/design/param-views.md) explain about 40 of the rise; the rest is not attributed.
    # The limit is the bound pluck declares for itself (max_cycles_block, _pk_desc6 in pluck.s), not a DSP budget.
    assert steady["instructions"] < 2200, steady["instructions"]
    assert first["instructions"] < 60000, first["instructions"]
    rig.rec_word(0, 0x1B8, 0)
    idle = rig.call(0)
    # wrapper (about 270: it saves, pins and restores the context registers, finding 45, and writes four breadcrumbs, finding 48; the
    # breadcrumbs go into the reply records as well on a device, about 70 more) + 32 stores + the guard (about 400, finding 51)
    assert idle["instructions"] < 750


def test_the_voice_ends_when_it_is_quiet_and_clears_its_flags():
    rig = PluckRig()
    # fast decay, dark: the string dies within a few hundred blocks
    rig.setup(note=84, decay=0x0000, bright=0x0000)
    ended = None
    for i in range(1500):
        _, out = rig.block()
        if (out["flags"][0] & 0xFF) == 0:
            ended = i
            break
    assert ended is not None and ended > 5
    assert out["flags"][0] & 0xFF00 == 0, "the mark is cleared"
    # the next block is silent
    rig.rec_word(0, 0x1B8, 0)
    y, _ = rig.block()
    assert not np.any(y)


def test_a_new_note_restarts_the_string():
    rig = PluckRig()
    y1, _ = run_blocks(rig, 6, note=60)
    rig.rec_word(0, 0x1B8, 1)  # the arm routine: ACTIVE = 1, mark cleared
    y2, _ = rig.block()
    assert np.max(np.abs(y2)) > 0.05
    assert not np.allclose(y2, y1[:32])  # a new noise burst
