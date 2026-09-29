"""Tests for tools/dt2gui.py's content key and emu.gui's card-image sidecar
check -- the two pieces of "run the GUI with your own samples in one
command" that don't need firmware: what determines whether a cached +Drive
image/snapshot is reused or rebuilt, and what stops the GUI from silently
running a snapshot against the wrong card image. No DT2_SYX needed: the key
only hashes files (the sample .wav's, tools/plusdrive.py, and whatever path
is passed as "syx"), and the sidecar check only compares hashes recorded in
a `.ladder.json` against a card image file -- neither runs the emulator.
"""

import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import tools.dt2gui as dt2gui  # noqa: E402


def _write(path, data=b"x"):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(data)
    return path


class ContentKeyTest(unittest.TestCase):
    """content_key() is what decides reuse vs. rebuild: same inputs must
    give the same key, and every input that changes the built +Drive image
    must change the key."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.samples = os.path.join(self.tmp.name, "samples")
        os.makedirs(self.samples)
        self.syx = _write(os.path.join(self.tmp.name, "fw.syx"), b"firmware bytes")
        _write(os.path.join(self.samples, "hat.wav"), b"hat pcm data")

    def test_deterministic(self):
        key1, names1 = dt2gui.content_key(self.samples, self.syx)
        key2, names2 = dt2gui.content_key(self.samples, self.syx)
        self.assertEqual(key1, key2)
        self.assertEqual(names1, names2)
        self.assertEqual(len(key1), 64)  # full sha256 hex; callers take [:12]

    def test_names_found(self):
        _write(os.path.join(self.samples, "snare.wav"), b"snare pcm")
        _, names = dt2gui.content_key(self.samples, self.syx)
        self.assertEqual(names, ["hat.wav", "snare.wav"])  # sorted

    def test_changes_with_sample_bytes(self):
        key1, _ = dt2gui.content_key(self.samples, self.syx)
        _write(os.path.join(self.samples, "hat.wav"), b"different pcm data")
        key2, _ = dt2gui.content_key(self.samples, self.syx)
        self.assertNotEqual(key1, key2)

    def test_changes_with_sample_set(self):
        key1, _ = dt2gui.content_key(self.samples, self.syx)
        _write(os.path.join(self.samples, "snare.wav"), b"snare pcm")
        key2, _ = dt2gui.content_key(self.samples, self.syx)
        self.assertNotEqual(key1, key2)

    def test_ignores_non_wav_files(self):
        key1, _ = dt2gui.content_key(self.samples, self.syx)
        _write(os.path.join(self.samples, "README.md"), b"# notes\n")
        key2, _ = dt2gui.content_key(self.samples, self.syx)
        self.assertEqual(key1, key2)

    def test_ignores_wav_case_and_still_hashes_once(self):
        # Both .wav and .WAV are picked up (case-insensitive match, per
        # tools/plusdrive.py's own load_samples); renaming should still
        # change the key exactly like any other sample-set change.
        key1, _ = dt2gui.content_key(self.samples, self.syx)
        os.rename(
            os.path.join(self.samples, "hat.wav"),
            os.path.join(self.samples, "hat.WAV"),
        )
        key2, _ = dt2gui.content_key(self.samples, self.syx)
        self.assertNotEqual(key1, key2)  # different name -> different key
        _, names = dt2gui.content_key(self.samples, self.syx)
        self.assertEqual(names, ["hat.WAV"])

    def test_changes_with_firmware(self):
        key1, _ = dt2gui.content_key(self.samples, self.syx)
        other_syx = _write(os.path.join(self.tmp.name, "fw2.syx"), b"other firmware")
        key2, _ = dt2gui.content_key(self.samples, other_syx)
        self.assertNotEqual(key1, key2)

    def test_no_samples_is_an_error_in_prepare(self):
        empty = os.path.join(self.tmp.name, "empty")
        os.makedirs(empty)
        with self.assertRaises(SystemExit):
            dt2gui.prepare(empty, self.syx)


class CardImageSidecarTest(unittest.TestCase):
    """emu.gui.check_card_image_sidecar: the guard against booting a
    snapshot against the wrong (or no) card image."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        from emu.gui import check_card_image_sidecar

        self.check = check_card_image_sidecar
        self.snap_dir = os.path.join(self.tmp.name, "snap")
        os.makedirs(self.snap_dir)
        self.snapshot = os.path.join(self.snap_dir, "ready.snap")  # need not exist
        self.img = _write(os.path.join(self.tmp.name, "dt2.img"), b"card image bytes")

    def _write_sidecar(self, card_image_sha256, card_image="dt2.img"):
        import json

        with open(os.path.join(self.snap_dir, ".ladder.json"), "w") as fh:
            json.dump(
                {
                    "protocol": 1,
                    "sdgate": True,
                    "esdhc": True,
                    "card_image": card_image,
                    "card_image_sha256": card_image_sha256,
                },
                fh,
            )

    def test_matching_sha_passes_silently(self):
        from emu.checkpoint import sha256_file

        self._write_sidecar(sha256_file(self.img))
        self.check(self.snapshot, self.img)  # must not raise

    def test_a_checked_card_returns_its_hash(self):
        # emu/livesharc.py keys the live DSP's state pack by it.
        from emu.checkpoint import sha256_file

        sha = sha256_file(self.img)
        self._write_sidecar(sha)
        self.assertEqual(self.check(self.snapshot, self.img), sha)

    def test_mismatched_image_refuses(self):
        self._write_sidecar("0" * 64)  # sha of some other image
        with self.assertRaises(SystemExit) as ctx:
            self.check(self.snapshot, self.img)
        msg = str(ctx.exception)
        self.assertIn("dt2gui.py", msg)  # names the fix, per the task
        self.assertIn(self.snapshot, msg)

    def test_missing_card_image_refuses_when_sidecar_expects_one(self):
        from emu.checkpoint import sha256_file

        self._write_sidecar(sha256_file(self.img))
        with self.assertRaises(SystemExit):
            self.check(self.snapshot, None)

    def test_no_sidecar_is_a_warning_not_a_refusal(self):
        # No .ladder.json written in self.snap_dir at all.
        self.check(self.snapshot, self.img)  # must not raise

    def test_no_card_image_and_no_sidecar_expectation_passes(self):
        self._write_sidecar(None, card_image=None)
        self.check(self.snapshot, None)  # both "none" -> match, no raise


class FlexbusLogTest(unittest.TestCase):
    """--live-audio feeds the sample load's FlexBus log to the live DSP
    (emu/livesharc.py): the load run records it next to the snapshots, and
    a directory without it gets its ready snapshot rebuilt when asked."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def test_the_load_run_records_the_flexbus_log(self):
        d = self.tmp.name
        seen = {}

        def fake_run(cmd, log_path):
            seen["cmd"] = cmd
            _write(os.path.join(d, "samples.snap"))
            return 0

        with (
            mock.patch.object(dt2gui, "_run_logged", fake_run),
            mock.patch.object(dt2gui, "close_modals", lambda *a, **k: 0),
        ):
            dt2gui.build_ready_snapshot(d, "dt2.img", "fw.syx")
        cmd = seen["cmd"]
        i = cmd.index("--flexbus-log")
        self.assertEqual(cmd[i + 1], os.path.join(d, dt2gui.FLEXBUS_LOG))
        self.assertTrue(dt2gui.FLEXBUS_LOG.endswith(".raw"))

    def _prepare(self, need_flexbus, with_log):
        samples = os.path.join(self.tmp.name, "samples")
        _write(os.path.join(samples, "hat.wav"), b"hat pcm")
        syx = _write(os.path.join(self.tmp.name, "fw.syx"), b"fw")
        key12 = dt2gui.content_key(samples, syx)[0][:12]
        img_root = os.path.join(self.tmp.name, "img")
        snap_root = os.path.join(self.tmp.name, "snap")
        _write(os.path.join(img_root, key12, "dt2.img"))
        _write(os.path.join(snap_root, key12, "ready.snap"))
        if with_log:
            _write(os.path.join(snap_root, key12, dt2gui.FLEXBUS_LOG))
        built = []
        with (
            mock.patch.object(dt2gui, "IMG_ROOT", img_root),
            mock.patch.object(dt2gui, "SNAP_ROOT", snap_root),
            mock.patch.object(dt2gui, "need_snapshot", lambda *a, **k: False),
            mock.patch.object(
                dt2gui,
                "build_ready_snapshot",
                lambda d, img, syx: built.append(d) or os.path.join(d, "ready.snap"),
            ),
        ):
            dt2gui.prepare(samples, syx, need_flexbus=need_flexbus)
        return built

    def test_a_ready_directory_without_the_log_is_rebuilt_for_live_audio(self):
        self.assertEqual(len(self._prepare(need_flexbus=True, with_log=False)), 1)

    def test_a_ready_directory_with_the_log_is_reused(self):
        self.assertEqual(self._prepare(need_flexbus=True, with_log=True), [])

    def test_without_live_audio_the_log_is_not_needed(self):
        self.assertEqual(self._prepare(need_flexbus=False, with_log=False), [])


if __name__ == "__main__":
    unittest.main()
