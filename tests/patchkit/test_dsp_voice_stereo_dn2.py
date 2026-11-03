"""core/dsp-voice on the Digitone II and machine contract 1.2 (stereo machines), on the concrete SHARC+ core: the contract adapter's side of
docs/design/dn2-stereo.md.

A stereo machine returns DK_STEREO and writes R through `out_r`, the slot's R buffer (`_gs_rbuf`). After the guard the adapter checks R, multiplies it by the
engine's constant gain (the stock loop at 0x1c9478 does that to L right after the render hook), and stamps the slot with the block counter. The stamp, the clear
of the R stage records at a note-on and after a gap, and the cap of 8 stereo slots per block are the adapter's. The twins and the pan hook are in
test_dsp_stereo_twins.py and test_dsp_stereo_gain_slot.py. A mono 1.1 machine is unchanged. Skips when Selache is not built."""

import dataclasses
import struct

import pytest
from support.machine_rig import MachineRig, nw
from support.paths import REPO_ROOT

from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

FIXTURE = REPO_ROOT / "tests/fixtures/dsp/stereo-machine.s"
ENGINE_GAIN = 0x3FCADE01
NAN = 0xFFFFFFFF
STAGE_RECORDS = (
    ("_gs_fo_rec", 112, 28),
    ("_gs_uy_rec", 32, 8),
    ("_gs_gain_rec", 44, 11),
    ("_gs_c5_rec", 212, 53),
    ("_gs_hd_rec", 20, 5),
    ("_gs_c1_rec", 204, 51),
    ("_gs_c2_rec", 300, 75),
    ("_gs_c3_rec", 236, 59),
    ("_gs_rg_rec", 72, 18),
)


def f32(bits):
    return struct.unpack("<f", struct.pack("<I", bits))[0]


def stereo_rig():
    """The fixture machine on the manifest of test/sine: parameter word 0 selects R (0: 0.5, 1: 16.0 over the bound, 2: NaN); L is 0.25."""
    spec = pk_conform.spec("test/sine", "dn2")
    spec = dataclasses.replace(
        spec,
        sources=(str(FIXTURE),),
        entry="_sm_render",
        note_on="_sm_note_on",
        desc="_sm_desc",
    )
    return MachineRig("test/sine", "dn2", spec=spec)


def data(rig, name):
    return rig.built.data[name]


def heartbeat(rig):
    return rig.sess.peek(nw(data(rig, "_dv_diag"))) or 0


def stamp(rig, v):
    return rig.sess.peek(nw(data(rig, "_gs_stamp")) + v)


def word(rig, name, index=0):
    return rig.sess.peek(nw(data(rig, name)) + index) or 0


def seen(rig, v):
    return word(rig, "_gs_seen", v)


def r_block(rig, v):
    return [rig.sess.peek(nw(data(rig, "_gs_rbuf") + 128 * v) + k) for k in range(32)]


def record(rig, name, stride, size, v):
    return [rig.sess.peek(nw(data(rig, name) + stride * v) + k) for k in range(size)]


def fill_records(rig, v, word):
    for name, stride, size in STAGE_RECORDS:
        for k in range(size):
            rig.sess.poke(nw(data(rig, name) + stride * v) + k, word)


def restarted(name, size):
    """An R record after the clear of a note-on or a gap: zeros, but the ring stage's fill status (word 17) is 1 as the stock reset leaves it."""
    return [1 if name == "_gs_rg_rec" and k == 17 else 0 for k in range(size)]


def play(rig, slots, trig=()):
    """One engine block: every slot in `slots` is called in order (slot 0 first: it counts the block); the slots in `trig` have the note-on edge in it."""
    out = {}
    for v in slots:
        rig.use(v)
        if v in trig:
            rig.trigger()
        out[v] = rig.block("friendly")
    return out


def start(rig, slots, select=0):
    for v in slots:
        rig.select(v)
        rig.set_params({"sine_level": select * 256})
        rig.set_note(60, 100)
    play(rig, slots, trig=set(slots))  # the edge block: silent
    return play(rig, slots)  # the first block of the machine (note_on, render)


def test_a_stereo_machine_writes_r_scales_it_and_stamps_the_slot():
    rig = stereo_rig()
    blocks = start(rig, [0])
    assert blocks[0].returned and blocks[0].faults == 0
    assert set(blocks[0].samples) == {0.25}
    expected = f32(ENGINE_GAIN) * 0.5
    assert set(r_block(rig, 0)) == {struct.unpack("<I", struct.pack("<f", expected))[0]}
    assert stamp(rig, 0) == heartbeat(rig) != 0
    assert stamp(rig, 1) in (0, None)
    # the stamp expires with the next block, a new one replaces it
    first = stamp(rig, 0)
    play(rig, [0])
    assert stamp(rig, 0) == first + 1 == heartbeat(rig)


def test_a_mono_machine_is_unchanged_and_never_stamps():
    rig = MachineRig("test/sine", "dn2")
    rig.set_params({"sine_level": 0x7F00})
    rig.set_note(60, 100)
    rig.trigger()
    rig.block("friendly")
    for _ in range(3):
        b = rig.block("friendly")
    assert max(abs(x) for x in b.samples) > 0.1
    assert [stamp(rig, v) for v in range(16)] == [0] * 16
    assert rig.sess.peek(nw(data(rig, "_gs_ns"))) in (0, None)


@pytest.mark.parametrize("select", [1, 2], ids=["over-bound", "nan"])
def test_a_bad_r_block_plays_r_equals_l_without_a_fault_and_without_a_stamp(select):
    rig = stereo_rig()
    blocks = start(rig, [0], select)
    assert blocks[0].faults == 0, (
        "L is fine: the machine is not faulted, R is not used for the block"
    )
    assert set(blocks[0].samples) == {0.25}
    assert stamp(rig, 0) in (0, None)
    # the machine goes on: a good R is stamped again
    rig.set_params({"sine_level": 0})
    play(rig, [0])
    assert stamp(rig, 0) == heartbeat(rig)


def test_the_r_stage_records_restart_at_the_note_on_block():
    rig = stereo_rig()
    for v in (0, 3):
        rig.select(v)
        rig.set_params({"sine_level": 0})
        rig.set_note(60, 100)
    fill_records(rig, 0, NAN)
    fill_records(rig, 3, NAN)
    fill_records(rig, 7, NAN)
    play(
        rig, [0, 3], trig={0, 3}
    )  # the edge block: the machines do not run, nothing is stamped, the records are not touched
    assert stamp(rig, 0) in (0, None)
    assert record(rig, "_gs_fo_rec", 112, 28, 0) == [NAN] * 28
    play(rig, [0, 3])  # block 0 of the machines
    for v in (0, 3):
        for name, stride, size in STAGE_RECORDS:
            assert record(rig, name, stride, size, v) == restarted(name, size), (
                f"{name} of slot {v}"
            )
    for name, stride, size in STAGE_RECORDS:
        assert record(rig, name, stride, size, 7) == [NAN] * size, (
            "another slot's records are not touched"
        )


def test_the_r_stage_records_are_kept_in_a_running_voice_and_restart_after_a_gap():
    rig = stereo_rig()
    start(rig, [0])
    fill_records(rig, 0, 0x3F000000)
    play(rig, [0])
    assert record(rig, "_gs_fo_rec", 112, 28, 0) == [0x3F000000] * 28, (
        "a voice that ran in the last block keeps its R history"
    )
    # a block in which the slot is not rendered (a stage without a twin cleared the stamp, a slot over the cap): R restarts from zero
    assert word(rig, "_gs_cnt", 18) == 0
    rig.sess.poke(nw(data(rig, "_gs_stamp")), 0)
    play(rig, [0])
    assert record(rig, "_gs_fo_rec", 112, 28, 0) == [0] * 28
    assert word(rig, "_gs_cnt", 18) == 1, "the restart of a running voice is counted"


def test_a_retrigger_of_a_running_slot_restarts_the_r_history():
    rig = stereo_rig()
    start(rig, [0])
    play(rig, [0])
    fill_records(rig, 0, 0x3F000000)
    play(rig, [0], trig={0})  # the edge block of the new note
    play(rig, [0])  # its block 0
    assert record(rig, "_gs_fo_rec", 112, 28, 0) == [0] * 28


def test_the_cap_is_eight_stereo_slots_per_block_the_others_play_r_equals_l():
    rig = stereo_rig()
    slots = list(range(10))
    start(rig, slots)
    stamped = [v for v in slots if stamp(rig, v) == heartbeat(rig)]
    assert stamped == list(range(8)), "the first eight stereo slots of the block"
    assert rig.sess.peek(nw(data(rig, "_gs_ns"))) == 8
    # the next block counts again from zero
    play(rig, slots)
    assert [v for v in slots if stamp(rig, v) == heartbeat(rig)] == list(range(8))
    # a slot that stops being stereo frees its place at once: stop slot 2, slot 8 is stamped
    rig.use(2)
    rig.hw(
        74 + 2, 0
    )  # the slot's machine type is no longer the machine's: the stock engine renders it
    play(rig, slots)
    assert [v for v in slots if stamp(rig, v) == heartbeat(rig)] == [
        0,
        1,
        3,
        4,
        5,
        6,
        7,
        8,
    ]


def test_every_stereo_slot_is_latched_and_the_cap_is_counted_and_published():
    rig = stereo_rig()
    slots = list(range(10))
    start(rig, slots)
    assert [seen(rig, v) for v in slots] == [heartbeat(rig)] * 10, (
        "stereo at adapter time, stamped or not"
    )
    assert [seen(rig, v) for v in range(10, 16)] == [0] * 6
    assert word(rig, "_gs_cnt", 17) == 2, "slots 8 and 9 are over the cap"
    assert word(rig, "_gs_cnt", 16) == 0
    play(rig, slots)
    assert word(rig, "_gs_health") == 0b1001, (
        "the last block: some slot played R = L, reason cap"
    )
    play(rig, slots[:3])
    play(rig, slots[:3])
    assert word(rig, "_gs_health") == 0, "a clean block clears the word"


def test_a_bad_r_block_is_counted_and_raised_in_the_health_word():
    rig = stereo_rig()
    start(rig, [0], select=1)
    assert seen(rig, 0) == heartbeat(rig), "stereo, though not stamped"
    assert word(rig, "_gs_cnt", 16) == 1
    play(rig, [0])
    assert word(rig, "_gs_health") == 0b0101
    rig.set_params({"sine_level": 0})
    play(rig, [0])
    play(rig, [0])
    assert word(rig, "_gs_health") == 0


def test_a_mono_machine_leaves_the_latch_the_counters_and_the_health_word_alone():
    rig = MachineRig("test/sine", "dn2")
    rig.set_params({"sine_level": 0x7F00})
    rig.set_note(60, 100)
    rig.trigger()
    for _ in range(4):
        rig.block("friendly")
    assert [seen(rig, v) for v in range(16)] == [0] * 16
    assert [word(rig, "_gs_cnt", i) for i in range(32)] == [0] * 32
    assert word(rig, "_gs_health") == 0


def test_the_private_observer_cannot_be_installed_with_the_adapter(tmp_path):
    """core/dsp-stereo-proof stays the private observer: both modules claim the first-order and U/Y call sites, so patchkit refuses the pair."""
    import subprocess
    import sys

    import tools

    syx = REPO_ROOT / "firmware/Digitone_II_OS1.12.syx"
    if not syx.exists():
        pytest.skip("requires the DN2 1.12 firmware")
    out = tmp_path / "both.syx"
    both = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.patchkit.patchkit",
            "patch",
            "--firmware",
            str(syx),
            "--modules",
            "core/dsp-voice,core/dsp-stereo-proof",
            "-o",
            str(out),
        ],
        cwd=REPO_ROOT,
        env=tools.python_env(),
        capture_output=True,
        text=True,
    )
    assert both.returncode != 0 and "both claim" in both.stderr + both.stdout
    assert not out.exists()
