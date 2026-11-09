"""test/sine (a machine of the SDK contract, one DSP body) through core/dsp-voice on the concrete SHARC+ core, both devices.

The pitch follows the note within a cent, the DT2 tune knob shifts it by semitones, the level follows the velocity and the level
parameter, and the voice ends with DK_DONE after 2 s (Digitakt II). Skips when Selache is not built."""

import numpy as np
import pytest
import support.dsp_voice_rig as dv
from support.machine_rig import MachineRig

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = "test/sine"
DEVICES = ["dt2", "dn2"]


def render(device: str, note: float, blocks: int, params=None, vel: int = 100):
    rig = MachineRig(MODULE, device)
    rig.set_params({"sine_level": 0x7F00, **(params or {})})
    rig.set_note(note, vel)
    rig.trigger()
    ys = []
    for _ in range(blocks):
        b = rig.block("friendly")
        assert b.returned and b.faults == 0
        ys += list(b.samples)
    return np.array(ys, dtype=np.float64)


def peak_hz(x, fs=48000.0):
    """Frequency from the interpolated upward zero crossings (works for a few cycles)."""
    idx = np.flatnonzero((x[:-1] < 0) & (x[1:] >= 0))
    t = idx + (-x[idx]) / (x[idx + 1] - x[idx])
    assert len(t) >= 3, "fewer than two periods"
    return fs * (len(t) - 1) / (t[-1] - t[0])


@pytest.mark.parametrize("device", DEVICES)
@pytest.mark.parametrize("note", [12, 36, 60, 69, 84, 108, 127])
def test_the_sine_follows_the_note_within_a_cent(device, note):
    want = 440.0 * 2 ** ((note - 69) / 12)
    y = render(device, note, max(100, int(4.2 * 48000 / want / 32)) + 1)
    cents = 1200 * np.log2(peak_hz(y) / want)
    assert abs(cents) < 1.0, (note, want, cents)
    amp = float(np.max(np.abs(y)))
    assert 0.35 < amp < 0.40, amp  # velocity 100 / 127 * 0.5 = 0.3937


def test_tune_shifts_the_pitch_by_semitones():
    y = render("dt2", 60, 101, {"tune": 0x4100})
    want = 440.0 * 2 ** ((61 - 69) / 12)
    assert abs(1200 * np.log2(peak_hz(y) / want)) < 1.0


@pytest.mark.parametrize("device", DEVICES)
def test_the_level_parameter_scales_the_amplitude(device):
    full = np.max(np.abs(render(device, 60, 101)))
    half = np.max(np.abs(render(device, 60, 101, {"sine_level": 0x3F80})))
    assert abs(half / full - 0.5) < 0.01


@pytest.mark.parametrize("device", DEVICES)
def test_a_retrigger_restarts_the_oscillator_and_the_voice_ends_after_two_seconds(
    device,
):
    rig = MachineRig(MODULE, device)
    rig.set_params({"sine_level": 0x7F00})
    rig.set_note(60, 100)
    rig.trigger()
    first = rig.block("friendly")
    second = rig.block("friendly")
    assert abs(second.samples[0] if device == "dn2" else first.samples[0]) < 1e-6
    for _ in range(10):
        rig.block("friendly")
    rig.trigger()
    again = [rig.block("friendly") for _ in range(2)]
    assert abs(again[-1 if device == "dn2" else 0].samples[0]) < 1e-6
    if device == "dt2":  # (slow: 3000 blocks)
        for _ in range(3000):  # 96,000 samples: DK_DONE, then silence
            b = rig.block("friendly")
        assert all(x == 0.0 for x in b.samples)


@pytest.mark.parametrize("device", DEVICES)
def test_a_slot_with_an_unregistered_type_goes_back_to_the_stock_code_without_the_table_walk(
    device,
):
    rig = MachineRig(MODULE, device)
    rig.select(1)
    if device == "dt2":
        rig._set_type_dt2(1, 0)
    else:
        rig.hw(74 + 1, 0)
    b = rig.block()  # a drawn context: the stock code gets its own registers back
    assert b.halt["reason"] == "breakpoint" and b.out["final_pc_sw"] == dv.STOCK_BP, (
        b.halt
    )
    ctx = rig.last_ctx
    assert [
        n
        for n in dv.CONTEXT_REGS
        if (b.out["regs"][n] or 0) & 0xFFFFFFFF != ctx[n] & 0xFFFFFFFF
    ] == []
    assert b.instructions < 70, b.instructions
