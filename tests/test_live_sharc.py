"""The native SHARC core played live (native/live's capture source) against
the Python replay.

Needs firmware-derived files under out/ that these tests never build
themselves (each is minutes of Python):

    uv run python tools/sharc_transpile_run.py live-pack dt2-1.16 CAPTURE --lp0 LOG
    uv run python tools/sharc_transpile_run.py live-ref dt2-1.16 CAPTURE --lp0 LOG --frames 24

plus the native core library (out/native/opt/target-final, or
$SHARC_NATIVE_LIB) and ``cd native/live && cargo build --release``. The
device test also needs LIVE_AUDIO_DEVICE_TESTS=1.
"""

import json
import os
import struct
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tools"))

import sharc_transpile_run as tr  # noqa: E402
from tools.live_audio import (  # noqa: E402
    DEFAULT_SHARC_LIB,
    LiveAudio,
    _default_library_path,
)

IMAGE = "dt2-1.16"
CAPTURE = ROOT / "out/captures/drive3/dt2-1.16-drive3-trig1-emac.dt2cap"
LP0 = ROOT / "out/captures/drive3/flexbus-drive3.raw"
START = 74
LIVE_PLAY = ROOT / "native/live/target/release/live_play"


def _cached_pack_and_ref() -> tuple[Path, Path]:
    """The drive3 pack and a Python reference for it, or skip."""
    for path, what in (
        (CAPTURE, "the drive3 capture"),
        (LP0, "the drive3 FlexBus log"),
        (DEFAULT_SHARC_LIB, "the native core library"),
        (LIVE_PLAY, "live_play (cd native/live && cargo build --release)"),
    ):
        if not path.is_file():
            pytest.skip("missing %s: %s" % (what, path))
    import sharc_harness as h

    blob = tr.pack_image(h.load_image_memory(IMAGE))
    key = tr.live_key(blob, str(CAPTURE), str(LP0), START)
    pack = Path(tr.LIVE_DIR) / ("%s.pack" % key)
    refs = sorted(Path(tr.LIVE_DIR).glob("ref-%s-*.json" % key))
    if not pack.is_file() or not refs:
        pytest.skip("no cached live pack/reference for key %s (see module docs)" % key)
    return pack, refs[-1]


def test_live_voices_equal_the_python_replay():
    """live_play's frame loop (no device) against sharc_replay's voice
    outputs, from the arm frame on: every sample bit for bit."""
    pack, ref_path = _cached_pack_and_ref()
    ref = json.loads(ref_path.read_text())
    arm = ref["arm_frame"]
    frames = len(ref["voice_outputs"]["0"]) // 32
    n = arm - START + frames
    dump = Path(tr.LIVE_DIR) / ("test-dump-%d.f64" % os.getpid())
    try:
        out = subprocess.run(
            [
                str(LIVE_PLAY),
                "--pack",
                str(pack),
                "--bench",
                str(n),
                "--dump",
                str(dump),
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        raw = dump.read_bytes()
    finally:
        dump.unlink(missing_ok=True)
    assert "stopped 0" in out, out
    values = struct.unpack("<%dd" % (len(raw) // 8), raw)
    assert len(values) == n * 64
    for v in (0, 1):
        want = ref["voice_outputs"][str(v)]
        for j in range(frames):
            k = arm - START + j
            got = list(values[k * 64 + v * 32 : k * 64 + (v + 1) * 32])
            assert got == want[j * 32 : (j + 1) * 32], "frame %d voice %d" % (
                arm + j,
                v,
            )
    # Before the arm frame both voices are silent.
    assert not any(values[: (arm - START) * 64])


@pytest.mark.skipif(
    os.environ.get("LIVE_AUDIO_DEVICE_TESTS") != "1",
    reason="set LIVE_AUDIO_DEVICE_TESTS=1 to open a real output device",
)
def test_capture_plays_live_on_the_device():
    pack, _ = _cached_pack_and_ref()
    if not _default_library_path().is_file():
        pytest.skip("native/live not built")
    with LiveAudio(pack=pack) as audio:
        time.sleep(2.0)
        stats = audio.stats()
        render = audio.render_stats()
    assert stats.frames_rendered > 0
    assert render.frames > 0 and render.clean == render.frames
    assert stats.underruns == 0
