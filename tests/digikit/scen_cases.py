"""Shared by the tests of tests/digikit: the (scenario, image) cases of a kind at the tier DIGIKIT_TIER."""

import os

import pytest

from tools.firmware import config
from tools.firmware.config import DEVICE_SYX
from tools.patchkit import pk_digikit_scen as S

TIER = os.environ.get("DIGIKIT_TIER", "quick")
# `mise run module:check` runs only the scenarios of the module (comma-separated names)
ONLY = {n for n in os.environ.get("DIGIKIT_SCENARIOS", "").split(",") if n}


def cases(kinds: tuple[str, ...], device: str | None = None):
    out = []
    for scen in S.selected(TIER):
        if scen.kind not in kinds or (ONLY and scen.name not in ONLY):
            continue
        for image in scen.images(TIER):
            if device and image[:3] != device:
                continue
            if image[:3] in scen.devices or scen.data["scenario"].get("images"):
                out.append(pytest.param(scen, image, id=f"{scen.name}-{image}"))
    return out


JOBS = int(os.environ.get("DIGI_JOBS", "3"))
_REPORTS: dict[tuple[str, str], dict] = {}


def run(scen, image, tmp_path_factory, request):
    """Run the case; the first case of a file runs every selected case of the file at once (JOBS processes), the others read their report."""
    if not config.stock(DEVICE_SYX[image]).exists():
        pytest.skip(f"{DEVICE_SYX[image]} missing")
    key = (scen.name, image)
    if key not in _REPORTS:
        mine = [
            (it.callspec.params["scen"], it.callspec.params["image"])
            for it in request.session.items
            if it.module is request.module and hasattr(it, "callspec")
        ]
        mine = [(s, i) for s, i in mine if config.stock(DEVICE_SYX[i]).exists()]
        keep = tmp_path_factory.mktemp("digikit")
        if len(mine) == 1:
            _REPORTS[key] = S.run_scenario(
                scen, image, keep / f"{scen.name}-{image}", TIER
            )
        else:
            _REPORTS.update(S.run_pairs(mine, TIER, JOBS, keep, quiet=True))
    report = _REPORTS[key]
    failed = [c for c in report["checks"] if not c["ok"]]
    assert report["pass"], failed
