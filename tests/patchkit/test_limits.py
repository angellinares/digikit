"""The capacity limits come from the device files (docs/plans/capacity-isolation.md): the Python side reads `[limits]` and the contract
maxima of sdk/sdk.toml, and the tools keep no copy of a number."""

import pytest

from tools.patchkit import pk_device

DEVICES = pk_device.digikit_devices()


@pytest.mark.parametrize("dev", DEVICES)
def test_the_limits_are_the_device_files_and_within_the_contract_maxima(dev):
    lim = pk_device.limits(dev)
    mx = pk_device.contract_max()
    for key in ("modules", "fx_slots", "param_rows", "page_rows", "dsp_area_bytes"):
        assert getattr(lim, key) <= mx[key], (dev, key)
    assert lim.modules <= mx["sel_max"]
    # a Digitone II has page rows; the Digitakt II none
    assert (lim.page_rows > 0) == (dev == "dn2")
