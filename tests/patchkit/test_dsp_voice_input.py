"""What core/dsp-voice hands a machine of the SDK contract, through the probe `probe.s` beside the dyn-guard-nan fixture (samples 0 to 7: the
parameter words, the rest: the gate).

The gate of `dk_voice_in` (sdk/sdk.toml [machine]) is 1 from a voice's note-on until the firmware's release byte of that voice
(dsp.voice.value.release_latch, a pulse of about one block on the Digitakt II) has been seen, then 0. The Digitone II has no release source yet:
its gate stays 1. On the Digitakt II, parameter words 4 and 5 come from knobs 6 and 7 when the row declares their ranges. Skips when Selache
is not built."""

import dataclasses

import pytest
from support.machine_rig import MachineRig, nw

from tools.patchkit import pk_conform, pk_toolchains
from tools.patchkit.pk_conform import Param

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = "test/dyn-guard-nan"
PROBE = (
    pk_conform.ROOT
    / "tests"
    / "fixtures"
    / "modules"
    / "dyn-guard-nan"
    / "src"
    / "dsp"
    / "probe.s"
)


def rig(device: str, params: tuple[Param, ...] | None = None) -> MachineRig:
    sp = pk_conform.spec(MODULE, device)
    assert sp is not None
    sp = dataclasses.replace(
        sp,
        sources=(PROBE,),
        entry="_pr_render",
        note_on="_pr_note_on",
        desc="_pr_desc",
        params=sp.params if params is None else params,
    )
    return MachineRig(MODULE, device, spec=sp)


def gate(r: MachineRig) -> float:
    b = r.block("friendly")
    assert b.returned
    return b.samples[8]


def words(r: MachineRig) -> list[int]:
    b = r.block("friendly")
    assert b.returned
    return [round(x * 4096) for x in b.samples[:8]]


def knobs(*positions: int) -> tuple[Param, ...]:
    return tuple(Param(f"p{k}", k, 0, 0x7F00, 0x100 * (k + 1)) for k in positions)


def test_a_machine_with_parameters_on_knobs_6_and_7_gets_six_words():
    r = rig("dt2", knobs(1, 2, 4, 5, 6, 7))
    r.trigger()
    assert words(r) == [0x200, 0x300, 0x500, 0x600, 0x700, 0x800, 0, 0]


def test_a_machine_of_four_parameters_gets_four_words():
    r = rig("dt2", knobs(1, 2, 4, 5))
    r.hw(
        r.spec.frame.hw(0, 6), 0x1234
    )  # knobs 6 and 7 hold words the machine did not declare
    r.hw(r.spec.frame.hw(0, 7), 0x6400)
    r.trigger()
    assert words(r) == [0x200, 0x300, 0x500, 0x600, 0, 0, 0, 0]


def test_the_gate_is_held_from_the_note_on_until_the_release_pulse():
    r = rig("dt2")
    r.select(3)  # voice 6: byte 2 of the release word at +4
    r.trigger()
    assert [gate(r) for _ in range(3)] == [1.0, 1.0, 1.0]
    r.release()
    assert [gate(r) for _ in range(3)] == [
        0.0,
        0.0,
        0.0,
    ]  # the pulse lasts one block, the gate stays down
    r.trigger()
    assert gate(r) == 1.0  # a new note


def test_the_release_of_another_voice_leaves_the_gate():
    r = rig("dt2")
    r.select(3)
    r.trigger()
    gate(r)
    for track in (
        2,
        1,
    ):  # voice 4 (the same word, byte 0) and voice 2 (the word before)
        r.use(track)
        r._release_byte(1)
    r.use(3)
    assert gate(r) == 1.0


def test_the_digitone_gate_stays_1():
    r = rig("dn2")
    r.trigger()
    r.block("friendly")
    r.release()
    assert [gate(r) for _ in range(2)] == [1.0, 1.0]


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_a_tracks_state_is_zeroed_at_its_first_note_on_and_then_only_the_clear_bytes(
    device,
):
    r = rig(device)
    b = r.built
    at = nw(b.state + b.stride * r.track)
    words_ = b.stride // 4
    assert [r.sess.peek(at + k) for k in range(words_)] != [0] * words_  # garbage
    for _ in range(2):  # the Digitone II rig sees the trigger edge from the second call on
        r.trigger()
        assert r.block("friendly").returned
    assert [r.sess.peek(at + k) for k in range(words_)] == [0] * words_
    r.sess.poke(at, 0x1234)  # a note-on again: the clear bytes (0) only
    r.trigger()
    assert r.block("friendly").returned
    assert r.sess.peek(at) == 0x1234
