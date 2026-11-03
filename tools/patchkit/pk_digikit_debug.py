#!/usr/bin/env python3
"""The `debug` scenarios of the digikit runtime that are left in Python: the ColdFire memory audit (pk_digikit_memaudit.py). Run by
`tools/patchkit/pk_digikit_scen.py`. The log, guard and crash scenarios are TypeScript device tests (modules/tools/debug/tests,
modules/core/runtime/tests/{debug-guard,crash-capture}.device.test.ts)."""

from __future__ import annotations

from typing import Any

from tools.device import dkupload as DK
from tools.patchkit import pk_digikit_upload as U


def uploader(rig: Any) -> DK.Uploader:
    return DK.Uploader(U.transport(rig))


def run_debug(c: Any) -> None:
    from tools.patchkit import pk_digikit_memaudit as MA

    MA.run_memaudit(c)
