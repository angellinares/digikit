"""The real runtimeAcceptance records (not the faked routing of test_verify_route.py)."""

from __future__ import annotations

from tools.devtools import verify_route


def test_record_owner_is_packed_only_when_uploadable() -> None:
    records = verify_route.acceptance_records()
    core = records[
        "../../modules/core/adapters/input/tests/input-adapter.device.test.ts"
    ]
    assert (
        "core/input" not in core.modules
    )  # patched into the image, never packed as a .dkm
    shipped = records["../../modules/fx/freqshift/tests/freqshift.device.test.ts"]
    assert "digikit/freqshift" in shipped.modules
