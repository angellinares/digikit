"""`firmware.load {runtime: true}` against a real coupled daemon: the controller patches the stock image with the device's patch
set, boots it, and the digikit runtime answers an INFO request over `midi.usb.*`. A patched input is loaded as it is.

Slow (a coupled cold boot per device, cached under out/runs/.cache). Skips without the stock
firmware, its ready inputs (`tools/emulator/ready.py`, paths in tools/patchkit/pk_harness.py `FIRMWARE`) or a buildable
digi-emud (DIGI_EMUD names one outright). The runs are bounded: INFO steps at most 2 billion instructions.
"""

from __future__ import annotations

import time

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT
from tools.control.digi_client import Emu, EmuError  # noqa: E402
from tools.device.dkupload import EmuTransport, Uploader  # noqa: E402
from tools.emulator import card_check  # noqa: E402
from tools.patchkit import pk_harness  # noqa: E402


@pytest.mark.parametrize(("device", "version"), [("dt2", "1.17"), ("dn2", "1.12")])
def test_the_runtime_boots_and_answers_info(device, version):
    cell = pk_harness.Cell(device, version)
    try:
        syx, ready = pk_harness.firmware_inputs(cell)
        exe = pk_harness.daemon_binary(device, syx)
    except pk_harness.HarnessError as e:
        pytest.skip(str(e))
    with (
        pk_harness.run_dir_scope(f"runtime-load-{device}") as run_dir,
        Emu.spawn_coupled(syx, ready, timeout=300, binary=exe, run_dir=run_dir) as emu,
    ):
        params: dict[str, object] = {
            "syx": str(syx),
            "runtime": True,
            "force": True,
            "auto_no": True,
        }
        if device == "dt2":
            samples = pk_harness.OUT / "samples"
            card_check.make_samples(samples)
            params["card"] = emu.call("card.load", dir=str(samples))["card"]
        started = time.monotonic()
        report = emu.call("firmware.load", **params)
        load_s = time.monotonic() - started
        runtime = report["runtime"]
        assert runtime["patched"] is True and runtime["set"] == "emulator"
        # the emulator set: 12 modules, core/sample-pool and the run-time effects (their chain, its free rows and their pages) on both devices
        assert len(runtime["modules"]) == 16
        info = Uploader(EmuTransport(emu, limit=2_000_000_000)).info()
        print(
            device,
            version,
            "INFO",
            info,
            "runtime patch",
            {k: runtime[k] for k in ("cache_hit", "seconds")},
            f"firmware.load {load_s:.1f} s, start_used {report.get('start_used')}",
        )
        assert info["runtime"] > 0 and info["port"]
        # The patched image has a patch record: a second runtime load takes it as it is.
        again = emu.call("firmware.load", **{**params, "syx": runtime["cache_file"]})
        assert again["runtime"]["patched"] is False
        # What the desktop's Open... and Reset do: `replace` boots the file as the first firmware of a new session (the controller
        # takes the token), `restart` resets it under the next token; the runtime answers again, and the stale token is refused.
        reopened = emu.call(
            "firmware.load", **{**params, "replace": True, "session": 5}
        )
        assert (
            reopened["runtime"]["patched"] is True
            and reopened["runtime"]["cache_hit"] is True
        )
        emu.call("restart", session=6, auto_no=True)
        with pytest.raises(EmuError, match="stale emulator session"):
            emu.call("status", session=5)
        assert emu.call("status", session=6)["firmware"]["runtime"]["set"] == "emulator"
        assert Uploader(EmuTransport(emu, limit=2_000_000_000)).info()["runtime"] > 0
