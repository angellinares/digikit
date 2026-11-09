"""The pure parts of tools/device/dkrun.py: the runtime check and the restart detection from the device's log (no emulator)."""

import pytest

from tests.support import synth_spec as P
from tools.device import dklog as L
from tools.device import dkproto
from tools.device import dkrun as R
from tools.device import dkupload as D
from tools.patchkit import patchkit as PK

UP = L.K["UPLOAD"]


def ev(kind, a0=0, a1=0, flags=0, seq=1):
    return (seq, 0, kind, flags, (a0, a1, 0, 0, 0))


def test_an_old_runtime_is_refused_with_the_way_out():
    info = {"runtime": 3, "card_read_only": False}
    with pytest.raises(R.RunError, match="flash a runtime-4 image"):
        R.require_runtime(info)
    R.require_runtime({"runtime": 4, "card_read_only": False})
    with pytest.raises(R.RunError, match="writes nothing"):
        R.require_runtime({"runtime": 4, "card_read_only": True})


def test_restart_detection_reads_the_log():
    boot = ev(L.K["BOOT_START"], 4)
    asked = ev(UP, D.CMD_RESTART, 0)
    assert (
        R.restarted([boot, ev(UP, D.CMD_SELECT), asked]) is False
    )  # the old boot is still running
    assert (
        R.restarted([boot, ev(UP, D.CMD_SELECT)]) is True
    )  # a new boot never saw the request
    assert R.restarted([]) is False  # nothing booted yet
    refused = ev(UP, D.CMD_RESTART, 0xFFFFFFD5, flags=1)
    with pytest.raises(R.RunError, match="refused"):
        R.restarted([boot, asked, refused])


def _dkm(tmp_path, dsp_area=0):
    """A real packed module (patchkit's synthetic module, the packer's own writer) whose DSP section takes `dsp_area` bytes of the DSP area (a multiple
    of 64) and nothing else of it; its needs are inferred from the file by `patchkit needs`, not declared."""
    spec = tmp_path / f"fit-{dsp_area}.toml"
    spec.write_text(
        P.spec(
            "fit/x",
            dsp={"image": 64, "bss": dsp_area - 64, "voices": [{"type": 16}]}
            if dsp_area
            else None,
        )
    )
    out = tmp_path / f"fit-{dsp_area}.dkm"
    assert (
        PK.run(["synth-dkm", str(spec), "--device", "dt2", "-o", str(out)]).returncode
        == 0
    )
    return out.read_bytes()


CAPS = {
    "MODULES": {"limit": 8, "used": 5, "largest": 0},
    "SELECTION": {"limit": 8, "used": 5, "largest": 0},
    "ARENA": {"limit": 524288, "used": 400000, "largest": 100000},
    "STAGE": {"limit": 65536, "used": 0, "largest": 65536},
    "DSP_AREA": {"limit": 524288, "used": 400000, "largest": 100000},
}


class FitUp:
    """The reads `check_fit` and the start of `run` make."""

    def __init__(self, caps=CAPS, with_cap=True):
        self.caps = caps
        self.info_ = {
            "runtime": 0x000300,
            "card_read_only": False,
            "port": "dt2",
            "free_bytes": 1,
            "safe": False,
        }
        self.info_["caps"] = (1 << dkproto.CAP["CAPACITY"]) if with_cap else 0
        self.sent = []

    def info(self):
        return self.info_

    def capacity(self):
        return self.caps

    def select(self, paths):
        self.sent.append(("select", paths))


def test_a_module_that_will_not_fit_the_empty_device_is_named(tmp_path):
    up = FitUp()
    # the next boot is empty: what is loaded now does not count, so 400000 bytes of DSP area used is no reason
    assert R.check_fit(up, up.info(), _dkm(tmp_path, 300_032)) is None
    why = R.check_fit(up, up.info(), _dkm(tmp_path, 600_064))
    assert why == "DSP memory 586 KiB needed, 511 KiB free"
    # a file the loader would refuse (not a .dkm) is named too, whatever the device says
    assert "not a .dkm" in R.check_fit(up, up.info(), b"nope" * 100)
    # no CAPACITY on the device: no verdict
    assert (
        R.check_fit(
            FitUp(with_cap=False), FitUp(with_cap=False).info(), _dkm(tmp_path, 600_064)
        )
        is None
    )


def test_run_refuses_a_module_that_will_not_fit_unless_forced(
    tmp_path, monkeypatch, capsys
):
    f = tmp_path / "big.dkm"
    f.write_bytes(_dkm(tmp_path, 600_064))
    up = FitUp()
    monkeypatch.setattr(
        R,
        "upload_if_changed",
        lambda up, path, data: up.sent.append(("put", path)) or True,
    )
    with pytest.raises(
        R.RunError,
        match=r"will not fit this device: DSP memory 586 KiB needed, 511 KiB free \(--force uploads it anyway\)",
    ):
        R.run(up, "test/big", dkm=f, restart=False)
    assert up.sent == []  # nothing was uploaded, nothing selected
    debug = tmp_path / "debug.dkm"
    debug.write_bytes(b"debug")
    monkeypatch.setattr(R, "build_pack", lambda module, device: debug)
    R.run(up, "test/big", dkm=f, restart=False, force=True)
    # the log needs the debug module: it is uploaded and selected first, next to the module
    assert up.sent == [
        ("put", "modules/big.dkm"),
        ("put", "modules/debug.dkm"),
        ("select", ["modules/debug.dkm", "modules/big.dkm"]),
    ]
    assert "WARNING test/big will not fit this device" in capsys.readouterr().out
