"""Safe mode scenarios,Digitone II (tests/digikit/scenarios/*.toml): one test per scenario and image, tier DIGIKIT_TIER. One file per group, so that verify runs them in parallel."""

import pytest
from scen_cases import cases, run


@pytest.mark.parametrize(("scen", "image"), cases(("safe",), "dn2"))
def test_scenario(scen, image, tmp_path_factory, request):
    run(scen, image, tmp_path_factory, request)
