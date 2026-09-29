"""Tests for tools/sharc_diff.py, the Python/native differential test
harness.

Three groups, the same convention tests/test_sharc_replay.py uses:

- Pure state-format tests (pack_state/unpack_state/compare_states/
  ComputeCase generation) need no firmware and always run: they build
  synthetic States by hand or through import_state, never through a real
  Runner over the DT2 1.16 image.
- The self-test (self_test_agree/self_test_mutation) and the frame-lockstep
  test need the real DT2 1.16 SHARC+ image bytes (Elektron's copyright,
  never committed here) and are skipped without them; they are also slow
  (a real Runner run), so they carry @pytest.mark.slow.
- The corpus generator (generate_compute_corpus) needs no firmware either
  (every case is a fabricated instruction over a random register file,
  concrete=None) but does real work over every op table entry, so it is
  marked slow too, to keep the default `pytest tests -q` run fast.
"""

import os
import pathlib
import sys
import unittest

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))

import sharc_diff as sd  # noqa: E402
import sharc_trace as st  # noqa: E402

DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")
IDLE_CAPTURE = pathlib.Path("out/captures/dt2-1.16-idle.dt2cap")


def _synthetic_state(**overrides):
    uregs = {code: st.Const(code) for code in range(sd.UREG_COUNT)}
    special = {"MRF": st.MR((1 << 80) - 1, 0x1234)}
    kwargs = dict(
        pc_sw=0x1000,
        uregs=uregs,
        special=special,
        mmrs={0x30024: st.Const(0)},
        concrete=None,
        record_events=False,
    )
    kwargs.update(overrides)
    return st.State(**kwargs)


class ExportImportRoundTripTest(unittest.TestCase):
    """No firmware needed: export_state/import_state/pack_state/
    unpack_state are pure data transforms over a hand-built State."""

    def test_export_then_import_reproduces_uregs(self):
        state = _synthetic_state()
        fields = sd.export_state(state, page_hash=False)
        imported = sd.import_state(fields, data=None)
        for code in range(sd.UREG_COUNT):
            self.assertEqual(
                st._ureg_raw(state.uregs, code), st._ureg_raw(imported.uregs, code)
            )

    def test_export_then_import_reproduces_special_slots(self):
        state = _synthetic_state()
        fields = sd.export_state(state, page_hash=False)
        imported = sd.import_state(fields, data=None)
        self.assertEqual(state.special["MRF"], imported.special["MRF"])
        for name in ("MRB", "MSF", "MSB", "BFFWRP", "BFF_HI", "BFF_LO"):
            self.assertIsInstance(imported.special[name], st.Unknown)

    def test_pack_unpack_round_trips_byte_identically(self):
        state = _synthetic_state()
        fields = sd.export_state(state, page_hash=True)
        blob = sd.pack_state(fields)
        back = sd.unpack_state(blob)
        blob2 = sd.pack_state(back)
        self.assertEqual(blob, blob2)

    def test_pack_unpack_agrees_under_compare_states(self):
        state = _synthetic_state()
        fields = sd.export_state(state, page_hash=True)
        back = sd.unpack_state(sd.pack_state(fields))
        self.assertEqual(sd.compare_states(fields, back), [])

    def test_blob_starts_with_magic_and_version(self):
        state = _synthetic_state()
        blob = sd.pack_state(sd.export_state(state, page_hash=False))
        self.assertEqual(blob[:4], b"SHRD")

    def test_unpack_rejects_bad_magic(self):
        with self.assertRaises(ValueError):
            sd.unpack_state(b"XXXX" + b"\x00" * 20)

    def test_pending_round_trips_including_after_delay_slots_sentinel(self):
        state = _synthetic_state(
            pending=st.Pending(
                target=0x2000, call=True, slots=1, return_sw=st.AFTER_DELAY_SLOTS
            )
        )
        fields = sd.export_state(state, page_hash=False)
        back = sd.unpack_state(sd.pack_state(fields))
        self.assertEqual(back["pending"]["return_sw"], st.AFTER_DELAY_SLOTS)
        self.assertEqual(back["pending"]["target"], 0x2000)

    def test_loops_and_call_stack_round_trip(self):
        state = _synthetic_state(
            loops=[st.Loop(0x100, 0x200, 3, 0)],
            call_stack=[0x300, 0x400],
        )
        fields = sd.export_state(state, page_hash=False)
        back = sd.unpack_state(sd.pack_state(fields))
        self.assertEqual(
            back["loops"],
            [{"start_sw": 0x100, "end_sw": 0x200, "remaining": 3, "mode": 0}],
        )
        self.assertEqual(back["call_stack"], [0x300, 0x400])

    def test_memory_range_with_no_concrete_byte_is_not_a_false_diff(self):
        state = _synthetic_state()  # concrete=None: no address is readable
        fields = sd.export_state(state, memory_ranges=[(0x100, 4)], page_hash=False)
        self.assertIsNone(fields["memory_ranges"][0x100]["data"])
        back = sd.unpack_state(sd.pack_state(fields))
        self.assertEqual(sd.compare_states(fields, back), [])


class CompareStatesTest(unittest.TestCase):
    """No firmware needed."""

    def test_identical_states_have_no_diff(self):
        fields = sd.export_state(_synthetic_state(), page_hash=True)
        self.assertEqual(sd.compare_states(fields, fields), [])

    def test_a_differing_register_is_reported_by_name(self):
        a = sd.export_state(_synthetic_state(), page_hash=False)
        b = sd.export_state(
            _synthetic_state(
                uregs={
                    **{c: st.Const(c) for c in range(sd.UREG_COUNT)},
                    5: st.Const(0xFF),
                }
            ),
            page_hash=False,
        )
        diff = sd.compare_states(a, b)
        self.assertTrue(any(line.startswith("R5:") for line in diff), diff)

    def test_a_differing_pc_is_reported(self):
        a = sd.export_state(_synthetic_state(pc_sw=0x10), page_hash=False)
        b = sd.export_state(_synthetic_state(pc_sw=0x20), page_hash=False)
        diff = sd.compare_states(a, b)
        self.assertTrue(any(line.startswith("pc_sw:") for line in diff), diff)

    def test_a_differing_special_slot_is_reported(self):
        a = sd.export_state(_synthetic_state(), page_hash=False)
        mutated = _synthetic_state()
        mutated.special["MRF"] = st.MR((1 << 80) - 1, 0x9999)
        b = sd.export_state(mutated, page_hash=False)
        diff = sd.compare_states(a, b)
        self.assertTrue(any("special[MRF]" in line for line in diff), diff)


class ComputeCorpusTest(unittest.TestCase):
    """No firmware needed: every case is a fabricated instruction over a
    random register file (concrete=None). Marked slow because it walks
    every op-table entry."""

    @pytest.mark.slow
    def test_generates_cases_with_no_errors(self):
        with __import__("tempfile").TemporaryDirectory() as tmp:
            summary = sd.generate_compute_corpus(tmp, seed=42, cases_per_op=2)
            self.assertGreater(summary["cases"], 100)
            self.assertEqual(summary["errors"], 0)
            self.assertTrue(os.path.exists(summary["manifest_path"]))

    def test_a_single_alu_case_is_deterministic_given_a_seed(self):
        import random

        from sharc_core.compute_alu import ALU_OPS

        opcode = sorted(ALU_OPS)[0]
        fields = sd._full_compute_fields(0, opcode, 1, 2, 3)
        case_a = sd._run_one_compute_case("alu", opcode, "2a", fields, random.Random(7))
        case_b = sd._run_one_compute_case("alu", opcode, "2a", fields, random.Random(7))
        self.assertEqual(case_a.state_after, case_b.state_after)
        self.assertIsNone(case_a.error)

    def test_short_compute_case_runs_via_form_2c(self):
        from sharc_core.compute_multi import SHORT_OPS

        opcode = sorted(SHORT_OPS)[0]
        fields = sd._short_compute_fields(opcode, 1, 2)
        case = sd._run_one_compute_case(
            "short", opcode, "2c", fields, __import__("random").Random(1)
        )
        self.assertIsNone(case.error)
        self.assertIsNotNone(case.state_after)


class NativeEngineStubTest(unittest.TestCase):
    """No firmware, no native library: NativeEngine must fail fast and
    clearly when the dylib is absent."""

    def test_raises_file_not_found_when_library_absent(self):
        with self.assertRaises(FileNotFoundError):
            sd.NativeEngine("/nonexistent/libsharc_dt2_116.dylib", b"")


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class SelfTestTest(unittest.TestCase):
    """Real image, real Runner: PythonEngine vs PythonEngine must agree,
    and a deliberately mutated engine must be caught."""

    def test_two_fresh_engines_from_the_same_seed_agree(self):
        result = sd.self_test_agree("dt2-1.16", steps=300)
        self.assertFalse(result.diverged, result.diff)
        self.assertGreater(result.steps_agreed, 0)

    def test_a_mutated_astatx_bit_is_caught(self):
        result = sd.self_test_mutation("dt2-1.16", steps=300, mutate_after=20)
        self.assertTrue(result.diverged)
        self.assertTrue(any("ASTATX" in line for line in result.diff), result.diff)
        self.assertEqual(result.steps_agreed, 21)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
@unittest.skipUnless(
    IDLE_CAPTURE.exists(), "dt2-1.16-idle.dt2cap capture is not available"
)
class FrameLockstepTest(unittest.TestCase):
    """Two independently-driven Runners, seeded from one run_init(), must
    agree frame-by-frame on a few real captured DSPI2 frames -- the same
    write_dma_transfer/drive_dma_completion/call_frame_collect_all
    primitives sharc_replay.replay()'s own frame loop uses."""

    def test_two_engines_agree_over_two_real_frames(self):
        import sharc_harness as h
        from emu import sharc_capture

        image = "dt2-1.16"
        memory = h.load_image_memory(image)
        init = h.run_init(memory, image)
        if not init.ran:
            self.skipTest("run_init failed: %s" % init.error)
        runner_a = h.new_runner(memory, image, init=init)
        runner_b = h.new_runner(memory, image, init=init)
        for runner in (runner_a, runner_b):
            h.setup_frame_dma(runner.state, image, ring_flag=0)
        cap = sharc_capture.load(str(IDLE_CAPTURE))
        frames = cap.dspi2_frames[:2]
        result = sd.run_frame_lockstep(runner_a, runner_b, image, frames)
        self.assertFalse(result.diverged, result.diff)
        self.assertEqual(result.steps_agreed, len(frames))


if __name__ == "__main__":
    unittest.main()
