"""pk_conform: what the rigs read from the manifests (no core runs here)."""

import tomllib

from tools.patchkit import pk_conform as c
from tools.patchkit import pk_device


def test_machine_parameters_come_from_the_manifest_and_the_device_frame():
    dt2 = c.spec("test/sine", "dt2")
    dn2 = c.spec("test/sine", "dn2")
    assert dt2 is not None and dn2 is not None
    # Digitakt II: TUNE is the stock row 0xca on knob 0; the module's parameters follow from knob 1
    assert [(p.id, p.knob) for p in dt2.params] == [("tune", 0), ("level", 1)]
    assert [dt2.frame.hw(0, p.knob) - dt2.frame.base for p in dt2.params] == [0, 1]
    # the frame skips two halfwords per group of four knobs
    assert [dt2.frame.hw(0, k) - dt2.frame.base for k in (3, 4, 5)] == [3, 6, 7]
    # Module word 0 sits on knob B on both devices; DN2 has no stock TUNE knob.
    assert [(p.id, p.knob) for p in dn2.params] == [("level", 1)]
    assert dn2.frame.hw(3, 1) == dn2.frame.base + 3 * dn2.frame.stride + 1
    # a run-time machine has no fixed number: the rig takes the first number of the run-time range of the device (core/machines types.toml)
    types = tomllib.loads(
        (c.ROOT / "modules/core/adapters/machines/types.toml").read_text()
    )
    assert dt2.machine_type == types["dt2"]["dynamic_first"]
    assert dn2.machine_type == types["dn2"]["dynamic_first"]
    assert [p.default for p in dn2.params] == [32512]


def test_the_budget_follows_the_headroom_of_the_device():
    for dev in c.DEVICES:
        b = c.budget(dev)
        voices = pk_device.load(dev)["adapter"]["machine"]["voices"]
        assert b.voice_block == b.headroom // voices > 0
        assert b.note_on_block == b.headroom


def test_run_time_effects_are_effect_specs_too():
    """A `DK_FX` record (a run-time effect: no state symbol) is an effect spec."""
    s = c.fx_spec("test/dyn-fx-gain")
    assert s is not None
    assert (s.process, s.desc, s.state) == ("_dg_process", "_dg_desc", "")
    assert c.fx_spec("core/fx") is None  # a framework renders no effect of its own
    assert c.fx_spec("test/sine") is None  # a machine is no effect
    assert not c.is_machine_module("test/dyn-fx-gain")
