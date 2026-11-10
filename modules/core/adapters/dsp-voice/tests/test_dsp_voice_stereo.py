"""core/dsp-voice and machine contract 1.2 (stereo machines) on the concrete SHARC+ core, Digitakt II.

A stereo machine returns DK_STEREO and writes R through `out_r`, the odd voice's block of the pair. The even voice leaves a per-track
flag, the odd voice keeps R instead of copying L, and the guard checks and zeroes both blocks. A mono 1.1 machine is unchanged: the odd
voice copies the even voice's block. Skips when Selache is not built."""

import dataclasses

import numpy as np
import pytest
from support.machine_rig import MachineRig, nw
from support.paths import REPO_ROOT

from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

FIXTURE = REPO_ROOT / "tests/fixtures/dsp/stereo-machine.s"


def stereo_rig():
    """The fixture machine on the manifest of test/sine (one parameter: word 0 selects R)."""
    spec = pk_conform.spec("test/sine", "dt2")
    spec = dataclasses.replace(
        spec,
        sources=(str(FIXTURE),),
        entry="_sm_render",
        note_on="_sm_note_on",
        desc="_sm_desc",
    )
    return MachineRig("test/sine", "dt2", spec=spec)


def flag(rig):
    return rig.sess.peek(nw(rig.built.data["_dv_st"]) + rig.track) or 0


def run(rig, select=0):
    rig.set_params({"sine_level": select * 256})
    rig.set_note(60, 100)
    rig.trigger()
    even = rig.block("friendly")
    mid = flag(rig)
    odd = rig.odd_block()
    return even, odd, mid


def test_a_stereo_machine_writes_r_into_the_odd_voice_and_the_odd_voice_keeps_it():
    rig = stereo_rig()
    even, odd, mid = run(rig)
    assert even.returned and even.faults == 0
    assert set(even.samples) == {0.25}
    assert set(even.right) == {0.5}  # R went to the odd buffer during the even voice
    assert set(odd) == {0.5}, "the odd voice did not copy L over R"
    assert mid != 0 and flag(rig) == 0, "set by the even voice, consumed by the odd one"
    # the next block: same again, the flag does not stick
    even, odd, _ = run(rig)
    assert set(even.samples) == {0.25} and set(odd) == {0.5}


@pytest.mark.parametrize("select", [1, 2], ids=["over-bound", "nan"])
def test_the_guard_checks_r_and_zeroes_both_blocks(select):
    rig = stereo_rig()
    even, odd, _ = run(rig, select)
    assert even.faults == 1
    assert set(even.samples) == {0.0}
    assert set(odd) == {0.0}, "R is zeroed and the odd voice copies the silent L"
    assert flag(rig) == 0
    assert rig.read_rec_word(rig.voice, 0x1B8) & 0xFF == 0, "the voice ends"


def test_a_mono_machine_is_unchanged_the_odd_voice_copies_l():
    rig = MachineRig("test/sine", "dt2")
    rig.set_params({"sine_level": 0x7F00})
    rig.set_note(60, 100)
    rig.trigger()
    rig.block("friendly")
    rig.block("friendly")
    even = rig.block("friendly")
    assert all(np.isnan(x) for x in even.right), (
        "a mono machine leaves the odd buffer alone"
    )
    assert flag(rig) == 0
    odd = rig.odd_block()
    assert odd == even.samples and np.max(np.abs(odd)) > 0.1


def test_the_status_bit_is_not_confused_with_the_voice_end():
    """DK_OK | DK_STEREO continues the voice (DK_DONE | DK_STEREO ending it is in the SHOT tests)."""
    rig = stereo_rig()
    run(rig)
    assert rig.read_rec_word(rig.voice, 0x1B8) & 0xFF != 0, "DK_OK | DK_STEREO: running"


def test_a_flag_left_by_an_even_voice_whose_odd_voice_did_not_run_does_not_leak():
    """The engine runs the odd voice after the even one; a rig that skips it must not see the guard scan a stale odd buffer."""
    rig = stereo_rig()
    rig.set_params({"sine_level": 0})
    rig.set_note(60, 100)
    rig.trigger()
    rig.block("friendly")
    assert flag(rig) != 0
    again = rig.block("friendly")  # no odd block in between
    assert again.faults == 0 and set(again.samples) == {0.25}


# DT2 1.17 stock pan stage: the pair gate is word +0xd8 of the even voice's param record (byte 0x2506ec + 0xdc * 2t + 0xd8).
def gate_word(rig):
    return rig.sess.peek(nw(0x2506EC + 0xDC * 2 * rig.track + 0xD8)) or 0


def test_a_stereo_machine_sets_the_stock_pair_gate_a_mono_machine_leaves_it():
    rig = stereo_rig()
    assert gate_word(rig) == 0
    run(rig)
    assert gate_word(rig) == 1, "stereo pair path of the stock pan stage"
    rig.sess.poke(nw(0x2506EC + 0xDC * 2 * rig.track + 0xD8), 0)
    run(rig)
    assert gate_word(rig) == 1, "every block, a stale 0 cannot stay"

    mono = MachineRig("test/sine", "dt2")
    mono.set_params({"sine_level": 0x7F00})
    mono.set_note(60, 100)
    mono.trigger()
    mono.block("friendly")
    mono.block("friendly")
    assert gate_word(mono) == 0
