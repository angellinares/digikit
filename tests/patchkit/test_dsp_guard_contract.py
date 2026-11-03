"""The output guard of core/dsp-voice for a machine of the SDK contract, on both devices (docs/design/module-sdk.md 5.4, docs/findings/51-pluck-tune-guard.md).

The adapter runs the module through the MachineRig (tests/support/machine_rig.py): the NaN fixture of tests/fixtures/modules/dyn-guard-nan (the module's own
source, one body for both devices) and `clamp.s`, a fixture beside it whose block is the first parameter word times 2^-12. The guard zeroes a block
that is not finite or above 8.0 in magnitude, ends the voice, counts the fault in the row and, after 8 faults in 1500 blocks, disables the type. The
adapter clamps the parameter words to the row's declared ranges (0: 0 .. 0x7f00) in a buffer of its own. Skips when Selache is not built."""

import dataclasses
import math

import pytest
from support import dsp_voice_rig as dv
from support.machine_rig import MachineRig, nw

from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = "test/dyn-guard-nan"
FIX = (
    pk_conform.ROOT / "tests" / "fixtures" / "modules" / "dyn-guard-nan" / "src" / "dsp"
)
KNOB = {
    "dt2": 1,
    "dn2": 0,
}  # the knob position of the first module parameter (ADM_PARAM_SLOT0)


def rig(device: str, fixture: str = "nan", prefix: str = "gn") -> MachineRig:
    sp = pk_conform.spec(MODULE, device)
    assert sp is not None
    sp = dataclasses.replace(
        sp,
        sources=(FIX / f"{fixture}.s",),
        entry=f"_{prefix}_render",
        note_on=f"_{prefix}_note_on",
        desc=f"_{prefix}_desc",
    )
    return MachineRig(MODULE, device, spec=sp)


def note(r: MachineRig):
    """One note: the block that renders it (DN2: the edge block is silent, the next one renders)."""
    r.trigger()
    b = r.block("friendly")
    if r.device == "dn2":
        b = r.block("friendly")
    assert b.returned
    return b


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_a_nan_block_is_zeroed_and_counted(device):
    r = rig(device)
    b = note(r)
    assert b.faults == 1
    assert all(x == 0.0 for x in b.samples)
    b = note(r)  # the voice ended: the next note starts again and faults again
    assert b.faults == 2


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_the_type_is_disabled_after_eight_faults(device):
    r = rig(device)
    for k in range(8):
        assert note(r).faults == k + 1
    disabled = r.sess.peek(nw(dv.TABLE) + 10)
    assert disabled == r.spec.machine_type
    b = note(r)  # a disabled type is not called: silence, no new fault
    assert b.faults == 8
    assert all(x == 0.0 for x in b.samples)


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_a_parameter_beyond_the_range_is_clamped_before_the_module(device):
    r = rig(device, "clamp", "cp")
    r.hw(r.spec.frame.hw(r.track if device == "dt2" else r.voice, KNOB[device]), 0xFFFF)
    b = note(r)
    assert b.faults == 0
    assert all(
        x == 0x7F00 * 2.0**-12 for x in b.samples
    )  # 0xffff reached the module as 0x7f00


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_a_declared_wider_range_lets_it_through_to_the_guard(device):
    r = rig(device, "clamp", "cp")
    r.sess.poke(nw(dv.TABLE) + 2 + KNOB[device], 0xFFFF << 16)
    r.hw(r.spec.frame.hw(r.track if device == "dt2" else r.voice, KNOB[device]), 0xFFFF)
    b = note(r)
    assert b.faults == 1  # 0xffff * 2^-12 = 16.0 is above the bound of 8.0
    assert all(x == 0.0 for x in b.samples)
    assert math.isfinite(sum(b.samples))


def observed(r: MachineRig, name: str) -> int:
    return r.sess.peek(nw(r.built.data[f"_tk_{name}"])) or 0


def test_dn2_source_track_latches_at_delayed_note_on_and_retrigger():
    r = rig("dn2", "track", "tk")
    r.select(3)  # physical voice 3, with an independently assigned source track
    r.set_owner(7)
    r.trigger()
    edge = r.block("friendly")
    assert edge.returned and all(x == 0 for x in edge.samples)
    assert observed(r, "notes") == observed(r, "renders") == 0
    r.set_owner(11)  # owner is read at actual note_on, after the edge block
    b = r.block("friendly")
    assert b.returned and not b.context_bad and b.faults == 0
    assert all(x == 11 / 16 for x in b.samples)
    assert observed(r, "note_track") == observed(r, "render_track") == 11
    r.set_owner(0xFFFF)  # no edge: retain the latched owner, even if current is invalid
    b = r.block("friendly")
    assert b.returned and all(x == 11 / 16 for x in b.samples)
    assert observed(r, "notes") == 1 and observed(r, "renders") == 2
    r.set_owner(15)
    b = note(r)
    assert all(x == 15 / 16 for x in b.samples)
    assert observed(r, "notes") == 2 and observed(r, "renders") == 3
    assert observed(r, "note_track") == observed(r, "render_track") == 15


@pytest.mark.parametrize("owner", [16, 17, 0x7FFF, 0x8000, 0xFFFF])
def test_dn2_invalid_source_owner_ends_only_the_slot_without_callbacks(owner):
    r = rig("dn2", "track", "tk")
    r.select(3)
    r.set_owner(owner)
    for _ in range(9):  # would disable a type if treated as a module fault
        b = note(r)
        assert b.returned and not b.context_bad and b.faults == 0
        assert all(x == 0 for x in b.samples)
    r.set_owner(4)  # changing owner without a new edge cannot revive the ended slot
    assert all(x == 0 for x in r.block("friendly").samples)
    assert observed(r, "notes") == observed(r, "renders") == 0
    assert not r.sess.peek(nw(dv.TABLE) + 10)
    r.select(5)
    r.set_owner(9)
    assert all(x == 9 / 16 for x in note(r).samples)
    r.use(3)
    r.set_owner(2)
    assert all(x == 2 / 16 for x in note(r).samples)
    assert observed(r, "notes") == observed(r, "renders") == 2


@pytest.mark.parametrize("track", [0, 3, 15])
def test_dt2_source_track_is_the_actual_track(track):
    r = rig("dt2", "track", "tk")
    r.select(track)
    b = note(r)
    assert b.returned and not b.context_bad and b.faults == 0
    assert all(x == track / 16 for x in b.samples)
    assert observed(r, "note_track") == observed(r, "render_track") == track
    assert all(x == track / 16 for x in r.block("friendly").samples)
