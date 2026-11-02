"""The no-AOT scenario subset and tier name selection of tools/devtools/fx_tier.py."""

from __future__ import annotations

import pytest
from support.paths import REPO_ROOT

from tools.devtools import fx_tier


@pytest.mark.parametrize("path", sorted(fx_tier.NO_AOT_SUBSET))
def test_no_aot_subset_names_exist(path: str) -> None:
    from tools.emulator.coupled import scenario_document

    doc = scenario_document(REPO_ROOT / path)
    names = {s["name"] for s in doc["scenarios"]}
    assert set(fx_tier.NO_AOT_SUBSET[path]) <= names
    assert len(fx_tier.NO_AOT_SUBSET[path]) == len(set(fx_tier.NO_AOT_SUBSET[path]))
    seconds = {s["name"]: float(s.get("seconds") or 0) for s in doc["scenarios"]}
    assert all(seconds[n] > 0 for n in fx_tier.NO_AOT_SUBSET[path])


def test_tier_names_subset_and_full() -> None:
    path = "tests/regression/scenarios/dn2-sendfx.ts"
    seconds = {f"s{i:03d}": 1.0 for i in range(20)}
    with pytest.raises(SystemExit):  # a subset name that the file lacks
        fx_tier.tier_names(path, seconds, 7, False)
    assert fx_tier.tier_names(path, seconds, 7, True) == sorted(seconds)[::7]
    assert fx_tier.tier_names(path, seconds, 1, True) == sorted(seconds)
    real = {n: 1.0 for n in fx_tier.NO_AOT_SUBSET[path]}
    assert fx_tier.tier_names(path, real, 1, False) == list(fx_tier.NO_AOT_SUBSET[path])
