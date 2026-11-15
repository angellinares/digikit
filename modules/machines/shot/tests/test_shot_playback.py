"""digikit/shot through core/dsp-voice: the assertion that no SHARC-rig check (tests/rig/shot.toml) replaces yet, the adapter's latch words.
Kept until T1g phase 5 ports it (docs/plans/ledgers/t1g-retirement.md "Kept until phase 5"). Skips when Selache is not built."""

import numpy as np
import pytest
from shot_model import FWD_L, REV_L, Shot
from support.machine_rig import nw

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


DEVICES = ["dt2", "dn2"]


# kept until T1g phase 5 ports it
@pytest.mark.parametrize("device", DEVICES)
def test_smp_is_latched_but_other_parameter_words_remain_live(device):
    s = Shot(device, index=2)
    s.start(params={"play": FWD_L})
    s.step(1)
    changed = {
        "sample": 5 << 8,
        "start": 12 << 8,
        "end": 100 << 8,
        "play": REV_L,
        "pitch": 72 << 8,
    }
    s.step(1, changed)
    r = s.rig
    live = nw(r.built.data["_dv_params"])
    called = nw(r.built.data["_dv_call_params"])
    assert r.sess.peek(live) == 5 << 8
    assert r.sess.peek(called) == 2 << 8
    for word, pid in enumerate(("start", "end", "play", "pitch"), 1):
        assert r.sess.peek(called + word) == changed[pid]
    # Entry 5 is empty: a new trigger must now use it and finish silently.
    s.index = 5
    s.start()
    left, _ = s.step(1)
    assert not np.any(left)
    assert r.sess.peek(called) == 5 << 8
