"""Tests for tools/live_audio.py, the ctypes wrapper around native/live.

Skipped entirely if native/live hasn't been built (`cd native/live && cargo
build --release`) -- these tests never build the crate themselves, matching
"Python wrapper tests skipped without the built library". A device-opening
test additionally needs LIVE_AUDIO_DEVICE_TESTS=1 (see test_open_and_close),
same convention as native/live's own ignored Rust tests.
"""

import os
import struct
import sys
import wave

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from tools.live_audio import LiveAudio, LiveStats, _default_library_path  # noqa: E402

LIBRARY_PATH = _default_library_path()

pytestmark = pytest.mark.skipif(
    not LIBRARY_PATH.is_file(),
    reason="native/live not built: cd native/live && cargo build --release",
)

DEVICE_TESTS_ENABLED = os.environ.get("LIVE_AUDIO_DEVICE_TESTS") == "1"
needs_device = pytest.mark.skipif(
    not DEVICE_TESTS_ENABLED,
    reason="set LIVE_AUDIO_DEVICE_TESTS=1 to open a real output device",
)


def _write_pcm16_wav(path, sample_rate=48000, frames=((16384, -16384), (0, 32767))):
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        data = b"".join(struct.pack("<hh", left, right) for left, right in frames)
        wav.writeframes(data)


class TestLibraryDiscovery:
    def test_default_library_path_is_under_native_live_target_release(self):
        assert LIBRARY_PATH.parent.name == "release"
        assert LIBRARY_PATH.parent.parent.name == "target"
        assert LIBRARY_PATH.parent.parent.parent.name == "live"

    def test_missing_library_raises_file_not_found(self):
        with pytest.raises(FileNotFoundError):
            LiveAudio(library_path="/nonexistent/liblive_audio.dylib")


@needs_device
class TestOpenPlayStatsClose:
    """The full round trip against a real output device."""

    def test_open_and_close(self):
        audio = LiveAudio(target_latency_frames=512)
        try:
            assert audio.device_name()
            stats = audio.stats()
            assert isinstance(stats, LiveStats)
            assert stats.sample_rate > 0
            assert stats.channels >= 1
            assert stats.ring_capacity_frames >= 512
        finally:
            audio.close()

    def test_context_manager_closes_on_exit(self):
        with LiveAudio() as audio:
            assert audio.device_name()
        # A second close() (via __del__ or an explicit call) must not
        # raise -- close() is idempotent.
        audio.close()

    def test_play_tone_does_not_raise_and_advances_frames_rendered(self):
        import time

        with LiveAudio(target_latency_frames=512) as audio:
            audio.play(tone_hz=440.0, amplitude=0.2)
            before = audio.stats().frames_rendered
            time.sleep(0.2)
            after = audio.stats().frames_rendered
            assert after > before

    def test_play_wav_reports_missing_file(self, tmp_path):
        from tools.live_audio import LiveAudioError

        with LiveAudio() as audio, pytest.raises(LiveAudioError):
            audio.play(str(tmp_path / "does-not-exist.wav"))

    def test_play_wav_plays_a_real_file(self, tmp_path):
        path = tmp_path / "tone.wav"
        _write_pcm16_wav(path)
        with LiveAudio(target_latency_frames=512) as audio:
            audio.play(str(path))
            # Should not raise; stats should reflect an open device.
            stats = audio.stats()
            assert stats.sample_rate > 0

    def test_push_frame_does_not_raise(self):
        with LiveAudio() as audio:
            frame = bytes(0xABC)
            audio.push_frame(frame)

    def test_operations_after_close_raise(self):
        audio = LiveAudio()
        audio.close()
        from tools.live_audio import LiveAudioError

        with pytest.raises(LiveAudioError):
            audio.stats()
        with pytest.raises(LiveAudioError):
            audio.push_frame(b"\x00")
        with pytest.raises(LiveAudioError):
            audio.play(tone_hz=440.0)
