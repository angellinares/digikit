"""The gate of `dk_voice_in` (sdk/sdk.toml [machine]): core/dsp-voice holds it at 1 from a voice's note-on until the firmware's release
byte of that voice (dsp.voice.value.release_latch, a pulse of about one block on the Digitakt II) has been seen, then 0. The Digitone II has
no release source yet: its gate stays 1. The probe `gate.s` (beside the dyn-guard-nan fixture) writes the gate into its block. Skips when
Selache is not built."""

import dataclasses

import pytest
from support.machine_rig import MachineRig

from tools.patchkit import pk_conform, pk_toolchains

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
    / "gate.s"
)


def rig(device: str) -> MachineRig:
    sp = pk_conform.spec(MODULE, device)
    assert sp is not None
    sp = dataclasses.replace(
        sp,
        sources=(PROBE,),
        entry="_gp_render",
        note_on="_gp_note_on",
        desc="_gp_desc",
    )
    return MachineRig(MODULE, device, spec=sp)


def gate(r: MachineRig) -> float:
    b = r.block("friendly")
    assert b.returned
    return b.samples[0]


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
