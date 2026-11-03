"""Card safety, debug log and crash capture scenarios, Digitone II (tests/digikit/scenarios/*.toml, runner tools/patchkit/pk_digikit_debug.py): one test per scenario and image, tier DIGIKIT_TIER."""

import pytest
from scen_cases import cases, run


@pytest.mark.parametrize(("scen", "image"), cases(("debug", "crash"), "dn2"))
def test_scenario(scen, image, tmp_path_factory, request):
    run(scen, image, tmp_path_factory, request)
