"""The rig of the digikit/pluck tests (test_pluck.py, test_pluck_range.py, test_pluck_tune.py): the routine on the concrete SHARC+ core.

The wrapper (core/dsp-voice) and the routine are assembled from the module sources and run block by block the way
the engine calls them (see modules/core/adapters/dsp-voice/tests/test_dsp_voice.py). The generic machine rig keeps the core's memory (the routine's
delay line, reserved unloaded L2 on the device) from call to call. Skips when Selache is not built."""

import struct

import numpy as np
import pluck_ref
import pytest
from support.machine_rig import MachineRig, nw
from support.paths import REPO_ROOT

from tools.patchkit import pk_toolchains

ROOT = REPO_ROOT

SKIP_NO_SELACHE = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


def val(x: int) -> float:
    """The float32 value of a DM word."""
    return struct.unpack("<f", struct.pack("<I", x & 0xFFFFFFFF))[0]


class PluckRig(MachineRig):
    """digikit/pluck on the Digitakt II wrapper: the generic machine rig (tests/support/machine_rig.py) with the pluck test API."""

    def __init__(self, seed: int = 0xD1617) -> None:
        super().__init__("digikit/pluck", "dt2", seed=seed)

    def setup(
        self,
        note=60.0,
        tune=64.0,
        vel=100,
        decay=0x4000,
        pick=0,
        bright=0x4000,
        color=0,
        track=0,
    ) -> None:
        self.set_note(note, vel)
        self.set_params(
            {
                "tune": int(tune * 256),
                "pluck_decay": decay,
                "pluck_pick": pick,
                "pluck_bright": bright,
                "pluck_color": color,
            }
        )
        self.rec_word(2 * track, 0x1B8, 1)  # the arm routine: ACTIVE = 1, mark cleared

    def call(self, v: int) -> dict:
        """One call without a block's samples (the idle voice); the instruction count and the final state."""
        b = MachineRig.block(self)
        assert b.returned, b.halt
        return {"instructions": b.instructions, "out": b.samples}

    def block(self, track: int = 0):  # type: ignore[override]
        b = MachineRig.block(self)
        assert b.returned, b.halt
        b.out["flags"] = [self.read_rec_word(2 * track, 0x1B8)]
        # the routine's state row of this track (the first 32 words of its state block: the adapter's `state_base` + track * stride)
        row = nw(self.built.state + track * self.built.stride)
        b.out["state"] = [self.sess.peek(row + k) or 0 for k in range(32)]
        return np.array(b.samples, dtype=np.float32), b.out | {
            "instructions": b.instructions
        }


def run_blocks(rig: PluckRig, n: int, **kw) -> tuple[np.ndarray, list]:
    rig.setup(**kw)
    ys, outs = [], []
    for _ in range(n):
        y, out = rig.block(kw.get("track", 0))
        ys.append(y)
        outs.append(out)
    return np.concatenate(ys), outs


def model_blocks(
    n: int, note=60.0, tune=64.0, vel=100, decay=0x4000, pick=0, bright=0x4000, color=0
) -> np.ndarray:
    q = pluck_ref.Params(
        int(note * 256), int(tune * 256), decay, pick, bright, color, vel << 8
    )
    s = pluck_ref.Pluck()
    s.note_on(q)
    return np.concatenate([s.block(q) for _ in range(n)])
