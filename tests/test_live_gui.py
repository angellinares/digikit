"""The GUI's live DSP end to end (tools/live_gui_check.py): emu.gui's own
worker from a tools/dt2gui.py ready snapshot, TRIG 1 pressed, the native
SHARC engine (no output device) rendering at the real-time rate.

Slow (about 10 s, plus about 1.5 min the first time the state pack is
built). Needs DT2_SYX, a ready snapshot with the load's FlexBus log
(`uv run python tools/dt2gui.py --live-audio --no-gui` records it) and its
card image (found under snapshots/dt2-1.16-auto/, or named by
LIVE_GUI_SNAPSHOT and LIVE_GUI_CARD_IMAGE), native/live built and the
native SHARC core library.
"""

import os
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tools"))

pytestmark = pytest.mark.slow


def _ready_snapshot() -> tuple[str, str]:
    """$LIVE_GUI_SNAPSHOT and $LIVE_GUI_CARD_IMAGE when set, else the newest
    dt2gui ready snapshot that has a FlexBus log, and its card image; or
    skip."""
    if os.environ.get("LIVE_GUI_SNAPSHOT"):
        return os.environ["LIVE_GUI_SNAPSHOT"], os.environ["LIVE_GUI_CARD_IMAGE"]
    found = []
    for d in (ROOT / "snapshots/dt2-1.16-auto").glob("*"):
        img = ROOT / "out/plusdrive/auto" / d.name / "dt2.img"
        ready = d / "ready.snap"
        if ready.is_file() and (d / "flexbus.raw").is_file() and img.is_file():
            found.append((ready.stat().st_mtime, str(ready), str(img)))
    if not found:
        pytest.skip("no snapshots/dt2-1.16-auto/*/ready.snap with flexbus.raw")
    _, ready, img = max(found)
    return ready, img


def test_a_trig_pad_sounds_through_the_live_engine():
    from live_audio import DEFAULT_SHARC_LIB, _default_library_path

    if not os.environ.get("DT2_SYX"):
        pytest.skip("set DT2_SYX")
    for path, what in (
        (_default_library_path(), "native/live (cargo build --release)"),
        (DEFAULT_SHARC_LIB, "the native SHARC core library"),
    ):
        if not Path(path).is_file():
            pytest.skip("missing %s: %s" % (what, path))
    ready, img = _ready_snapshot()
    import live_gui_check

    report = live_gui_check.run(ready, img, instrs=12_000_000, tail=0.5)
    assert report["queue"]["trig_pushed"] >= 1, report
    assert report["queue"]["trig_taken"] == report["queue"]["trig_pushed"]
    assert report["queue"]["merged"] == 0
    assert report["render"]["stopped"] == 0
    assert report["render"]["nonzero_frames"] > 0
    assert report["latency_s"] is not None and report["latency_s"] < 2.0
    assert report["ok"]
