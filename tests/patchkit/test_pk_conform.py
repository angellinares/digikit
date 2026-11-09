"""pk_conform: what the machine conformance suite reads from the manifests (no core runs here)."""

import tomllib

import pytest

from tools.patchkit import pk_conform as c
from tools.patchkit import pk_device


def test_machine_parameters_come_from_the_manifest_and_the_device_frame():
    dt2 = c.spec("test/sine", "dt2")
    dn2 = c.spec("test/sine", "dn2")
    assert dt2 is not None and dn2 is not None
    # Digitakt II: TUNE is the stock row 0xca on knob 0; the module's parameters follow from knob 1
    assert [(p.id, p.knob) for p in dt2.params] == [("tune", 0), ("sine_level", 1)]
    assert [dt2.frame.hw(0, p.knob) - dt2.frame.base for p in dt2.params] == [0, 1]
    # the frame skips two halfwords per group of four knobs
    assert [dt2.frame.hw(0, k) - dt2.frame.base for k in (3, 4, 5)] == [3, 6, 7]
    # Module word 0 sits on knob B on both devices; DN2 has no stock TUNE knob.
    assert [(p.id, p.knob) for p in dn2.params] == [("sine_level", 1)]
    assert dn2.frame.hw(3, 1) == dn2.frame.base + 3 * dn2.frame.stride + 1
    # a run-time machine has no fixed number: the rig takes the first number of the run-time range of the device (core/machines types.toml)
    types = tomllib.loads(
        (c.ROOT / "modules/core/adapters/machines/types.toml").read_text()
    )
    assert dt2.machine_type == types["dt2"]["dynamic_first"]
    assert dn2.machine_type == types["dn2"]["dynamic_first"]
    assert [p.default for p in dn2.params] == [32512]


def test_machines_are_found_and_the_framework_is_not():
    labels = {s.label for s in c.machine_specs()}
    assert {
        "test/sine@dt2",
        "test/sine@dn2",
    } <= labels
    assert not any(label.startswith("core/") for label in labels)
    assert "test/dyn-dsp-hang@dt2" in {s.label for s in c.machine_specs(negative=True)}


def test_the_budget_follows_the_headroom_of_the_device():
    for dev in c.DEVICES:
        b = c.budget(dev)
        voices = pk_device.load(dev)["adapter"]["machine"]["voices"]
        assert b.voice_block == b.headroom // voices > 0
        assert b.note_on_block == b.headroom


def test_the_cases_cover_the_groups_and_are_deterministic():
    sp = c.spec("test/sine", "dt2")
    assert sp is not None
    cases = c.cases(sp)
    assert cases == c.cases(sp)
    by_group = {
        g: [x for x in cases if x.group == g]
        for g in ("limits", "corners", "modulation", "long", "retrigger")
    }
    assert all(by_group.values())
    n = len(sp.params)
    assert len(by_group["corners"]) == 3 * 2**n
    assert {x.note for x in by_group["limits"]} == set(c.NOTES_LIMITS)
    assert max(x.blocks for x in by_group["long"]) == c.LONG_BLOCKS
    assert all(x.blocks == c.WALK_BLOCKS for x in by_group["modulation"])
    assert {x.vel for x in by_group["retrigger"]} == {1, 127}
    # every limits case sets every parameter, inside its declared range
    rng = {p.id: (p.lo, p.hi) for p in sp.params}
    for x in cases:
        for row in x.words:
            for pid, w in row:
                assert rng[pid][0] <= w <= rng[pid][1], (x.name, pid, w)


def test_fx_specs_find_the_fixtures_and_keep_the_negative_apart():
    gain = c.fx_spec("test/fx-gain")
    assert gain is not None
    assert (gain.process, gain.desc, gain.state) == (
        "_fg_process",
        "_fg_desc",
        "_fg_state",
    )
    assert sorted(s.module for s in c.fx_specs(negative=True)) == [
        "test/dyn-fx-nan",
        "test/fx-nan",
    ]
    positive = [s.module for s in c.fx_specs()]
    assert "test/fx-nan" not in positive and "test/dyn-fx-hang" not in positive
    assert c.fx_spec("core/fx") is None  # a framework renders no effect of its own
    assert c.fx_spec("test/sine") is None  # a machine is no effect
    assert not c.is_machine_module("test/fx-gain")


def test_fx_specs_include_run_time_effects_and_turn_case_values_into_knob_floats():
    """A `DK_FX` record (a run-time effect: no state symbol, the page's knobs give the floats of the words) is an effect spec too."""
    import struct

    s = c.fx_spec("test/dyn-fx-gain")
    assert s is not None
    assert (s.process, s.desc, s.state) == ("_dg_process", "_dg_desc", "")
    assert s.knobs == ((0.0, pytest.approx(1.27)),)

    def bits(x: float) -> int:
        return struct.unpack("<I", struct.pack("<f", x))[0]

    assert c.fx_word(s, 0, 0) == bits(0.0)
    assert c.fx_word(s, 0, c.FX_PARAM_MAX) == bits(1.27)
    mid = struct.unpack("<f", struct.pack("<I", c.fx_word(s, 0, 0x4000)))[0]
    assert abs(mid - 1.27 * 0x4000 / c.FX_PARAM_MAX) < 1e-3  # the middle of the knob
    assert c.fx_word(s, 1, 5) == 5  # a word without a knob stays the raw case value
    assert "test/dyn-fx-gain" in [x.module for x in c.fx_specs()]


def test_fx_cases_cover_every_group_and_stay_inside_the_parameter_range():
    for n in (0, 1, 4):
        cases = c.fx_cases(n)
        assert {x.group for x in cases} == {
            "signals",
            "params",
            "modulation",
            "switching",
            "long",
        }
        assert {x.name for x in cases if x.group == "signals"} == {
            "silence", "dc", "square", "noise", "impulse", "sine", "ramp"
        }  # fmt: skip
        for x in cases:
            for row in x.words:
                assert len(row) == n and all(0 <= w <= c.FX_PARAM_MAX for w in row)
    assert max(x.blocks for x in c.fx_cases(1)) == c.LONG_BLOCKS
