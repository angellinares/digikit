"""tools/mmio_record.py: argument parsing, and report/dump on a synthetic
trace (no firmware). The firmware check, that a recorded window ends in the
same state as an unrecorded one, is slow and needs a snapshot: set
DT2_MMIO_SNAPSHOT (and DT2_SYX, plus DT2_MMIO_CARD for a +Drive checkpoint)
and pass --slow.
"""

import argparse
import json
import os
import struct
import subprocess
import sys

import pytest
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_SR

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, "tools"))

import mmio_record as mr  # noqa: E402
from emu import mmiotrace  # noqa: E402
from emu.harness import Machine  # noqa: E402

CODE = 0x40100000
PIT0 = 0xFC080000
SSI0 = 0xFC0BC000


def synthetic_trace(path):
    rec = mmiotrace.Recorder(path, state_every=0)
    m = Machine()
    rec.attach(m)
    for base in (CODE, PIT0, SSI0):
        m.ensure(base)
    m.uc.mem_write(
        CODE,
        b"\x33\xfc\x00\x0f"
        + struct.pack(">I", PIT0)  # move.w #$f,PIT0
        + b"\x30\x39"
        + struct.pack(">I", SSI0)  # move.w SSI0,d0
        + b"\x4e\x71",
    )
    m.uc.reg_write(UC_M68K_REG_SR, 0x2700)
    m.uc.reg_write(UC_M68K_REG_A7, CODE + 0x8000)
    rec.start({"label": "synthetic"}, clock=lambda: 7)
    m.uc.emu_start(CODE, 0, count=2)
    rec.stop()


def test_parse_press():
    assert mr.parse_press("TRIG 1@2000000+1000000") == ("TRIG 1", 2000000, 1000000)
    assert mr.parse_press("NO@0x10") == ("NO", 16, 0)
    with pytest.raises(argparse.ArgumentTypeError):
        mr.parse_press("TRIG 1")


def test_report_and_dump(tmp_path, capsys):
    path = str(tmp_path / "s.mmio")
    synthetic_trace(path)
    assert mr.main(["report", path, "--registers", "4"]) == 0
    out = capsys.readouterr().out
    assert "PIT0" in out and "SSI0" in out and "synthetic" in out
    # SSI0's control registers have no Python model in the recorded setup.
    assert "0xfc0bc000 R16" in out
    assert mr.main(["report", path, "--json"]) == 0
    summary = json.loads(capsys.readouterr().out)
    assert summary["peripherals"]["PIT0|timers-intc"]["writes"] == 1
    assert mr.main(["dump", path, "--tag", "WR"]) == 0
    assert "0xfc080000 W16 = 0xf" in capsys.readouterr().out


@pytest.mark.slow
@pytest.mark.skipif(
    not os.environ.get("DT2_MMIO_SNAPSHOT"),
    reason="set DT2_MMIO_SNAPSHOT (and DT2_SYX) to a checkpoint",
)
def test_recording_leaves_state_unchanged(tmp_path):
    snap = os.environ["DT2_MMIO_SNAPSHOT"]
    card = os.environ.get("DT2_MMIO_CARD")
    common = [snap, "--instrs", "2000000"] + (["--card-image", card] if card else [])
    tool = os.path.join(ROOT, "tools", "mmio_record.py")
    a, b = str(tmp_path / "a.snap"), str(tmp_path / "b.snap")
    subprocess.run(
        [sys.executable, tool, "record", *common, "--out", str(tmp_path / "t.mmio")]
        + ["--save-final", a],
        check=True,
        cwd=ROOT,
    )
    subprocess.run(
        [sys.executable, tool, "record", *common, "--no-record", "--save-final", b],
        check=True,
        cwd=ROOT,
    )
    eq = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "snapeq.py"), a, b],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert eq.returncode == 0, eq.stdout + eq.stderr
