"""emu/livesharc.py (the GUI's --live-audio wiring) and the state pack's
key, without firmware: the DSPI2 peer, the forced-frame pacing, the input
checks, and what keys a state pack. The end-to-end run (the GUI worker, a
TRIG press, the native engine) is tests/test_live_gui.py (slow)."""

import os
import sys
import tempfile
from types import SimpleNamespace

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, "tools"))

from emu import livesharc  # noqa: E402

PROFILE = {
    "name": livesharc.PROFILE_NAME,
    "vector": 191,
    "counter": 0x1000,
    "gate": 0x2000,
}


class FakeAudio:
    def __init__(self):
        self.pushed = []

    def push_frame(self, frame):
        self.pushed.append(frame)


def test_live_pack_init_limit_is_enforced_before_loading_an_image():
    import sharc_transpile_run as tr

    with pytest.raises(ValueError, match="init instruction limit must be positive"):
        tr.armed_start("absent-image", None, init_limit=0)


def test_the_peer_queues_wire_bytes_unchanged_and_replies_zeros():
    audio = FakeAudio()
    peer = livesharc.LiveFramePeer(audio)
    tx = bytearray(range(256)) * 11  # 2816 bytes, any content
    tx[0x22:0x24] = b"\x00\x00"
    assert peer.exchange(bytes(tx)) == bytes(len(tx))
    tx[0x22:0x24] = b"\x00\x01"  # TRIG 1
    peer.exchange(bytes(tx))
    assert audio.pushed[1] == bytes(tx), "no byte swap on the ColdFire side"
    assert (peer.frames, peer.trig_frames) == (2, 1)


class FakeUc:
    def __init__(self, sr):
        self.sr = sr
        self.writes = []

    def mem_write(self, addr, data):
        self.writes.append((addr, bytes(data)))

    def reg_read(self, _reg):
        return self.sr


class FakeMachine:
    def __init__(self, sr=0x2000):
        self.uc = FakeUc(sr)
        self.raised = []

    def raise_vector(self, vec, level=None):
        self.raised.append((vec, level))
        return True


@pytest.fixture
def forcer_env(monkeypatch):
    monkeypatch.setattr(livesharc, "frame_profile", lambda image=None: PROFILE)
    monkeypatch.setattr(livesharc, "interrupt_level", lambda m, v, respect_mask: 5)


def test_the_forcer_opens_the_gate_and_forces_once_per_period(forcer_env):
    m = FakeMachine()
    pits = SimpleNamespace(now=1_000)
    f = livesharc.FrameForcer(m, pits, period=100)
    assert m.uc.writes == [(0x2000, bytes(4))], "frame-build gate opened"
    for now in (1_000, 1_050, 1_099, 1_100, 1_150, 1_230):
        pits.now = now
        f.on_chunk(0, 0)
    assert m.raised == [(191, 5)] * 3  # at 1000, 1100 and 1230
    assert f.forced == 3
    assert m.uc.writes[1:] == [(0x1000, bytes(4))] * 3, "pacing counter cleared"


def test_the_forcer_never_nests_inside_its_own_level(forcer_env):
    m = FakeMachine(sr=0x2500)  # IPL 5: inside a level-5 handler
    pits = SimpleNamespace(now=0)
    f = livesharc.FrameForcer(m, pits, period=100)
    f.on_chunk(0, 0)
    assert (m.raised, f.deferred) == ([], 1)
    m.uc.sr = 0x2400  # returned: the next boundary forces
    f.on_chunk(0, 0)
    assert (len(m.raised), f.forced) == (1, 1)


def test_prepare_needs_a_card_image_and_the_flexbus_log(monkeypatch):
    monkeypatch.setattr(livesharc, "frame_profile", lambda image=None: PROFILE)
    with tempfile.TemporaryDirectory() as d:
        snap = os.path.join(d, "ready.snap")
        with pytest.raises(SystemExit, match="--card-image"):
            livesharc.prepare(snap, None)
        with pytest.raises(SystemExit) as e:
            livesharc.prepare(snap, os.path.join(d, "dt2.img"), "ab" * 32)
        msg = str(e.value)
        assert livesharc.default_lp0(snap) in msg
        assert "dt2gui.py --live-audio" in msg and "--live-lp0" in msg


def test_other_firmware_is_refused(monkeypatch):
    fake = SimpleNamespace(
        profile_for=lambda image: ("sha", {"name": "Digitone II 1.11"})
    )
    monkeypatch.setitem(sys.modules, "framelink", fake)
    with pytest.raises(SystemExit, match="supports Digitakt II 1.16 only"):
        livesharc.frame_profile("any.bin")


def test_a_state_pack_key_names_the_card_and_a_capture_key_is_unchanged():
    import sharc_transpile_run as tr

    with tempfile.TemporaryDirectory() as d:
        cap = os.path.join(d, "c.dt2cap")
        lp0 = os.path.join(d, "f.raw")
        for p in (cap, lp0):
            with open(p, "wb") as fh:
                fh.write(p.encode())
        blob = b"image blob"
        plain = tr.live_key(blob, cap, lp0, 74)
        assert tr.live_key(blob, cap, lp0, 74, None) == plain
        a = tr.live_key(blob, None, lp0, 0, "aa" * 32)
        b = tr.live_key(blob, None, lp0, 0, "bb" * 32)
        assert a != b, "another card image gets another pack"
        assert a == tr.live_key(blob, None, lp0, 0, "AA" * 32)
        assert a != tr.live_key(blob, None, None, 0, "aa" * 32)
