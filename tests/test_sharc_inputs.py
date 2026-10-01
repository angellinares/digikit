"""Tests for tools/sharc_inputs.py.

Pure-Python pieces (ring_label, _is_plausible, init_write_info against a
synthetic loader stream, the to_json() shapes) need neither the real
firmware nor its program database. The end-to-end tests do (out/sharcdb,
out/sections -- see the worktree's symlinks) and are skipped, not marked
slow, when they are not available, matching tests/test_sharc_contract.py.
run_init()/dynamic_view() are additionally marked slow (~1.24M instructions
for init alone), like tests/test_sharc_harness.py's own equivalents.
"""

import os
import pathlib
import sys
import unittest
from importlib import import_module

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
si = import_module("sharc_inputs")
sharc = import_module("sharc")
C = import_module("sharc_contract")
sr = import_module("sharc_run")
st = import_module("sharc_trace")
L = import_module("sharcldr")

DT2_116_DB = pathlib.Path("out/sharcdb/dt2-1.16.sqlite")
DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")
IDLE_CAPTURE = pathlib.Path("out/captures/dt2-1.16-idle.dt2cap")
PLAY_PRETRACKS_CAPTURE = pathlib.Path("out/captures/dt2-1.16-play-pretracks.dt2cap")


def loader_memory():
    return L.LoadedMemory.from_stream(b"")


class RingLabelTest(unittest.TestCase):
    def test_ring_a_buf0(self):
        self.assertEqual(si.ring_label(0x261CC8), "ring A buf0")
        self.assertEqual(si.ring_label(0x261DC7), "ring A buf0")

    def test_ring_a_buf1_starts_right_after_buf0(self):
        self.assertEqual(si.ring_label(0x261DC8), "ring A buf1")

    def test_ring_b_matches_the_finding_addresses(self):
        self.assertEqual(si.ring_label(0x261EC8), "ring B buf0")
        self.assertEqual(si.ring_label(0x261FC8), "ring B buf1")

    def test_ring_c_matches_the_finding_addresses(self):
        self.assertEqual(si.ring_label(0x262138), "ring C buf0")
        self.assertEqual(si.ring_label(0x262938), "ring C buf1")

    def test_ring_d_matches_the_finding_addresses(self):
        self.assertEqual(si.ring_label(0x263138), "ring D buf0")
        self.assertEqual(si.ring_label(0x263938), "ring D buf1")

    def test_outside_every_ring_is_none(self):
        self.assertIsNone(si.ring_label(0x0))
        self.assertIsNone(si.ring_label(0x300000))
        # Between ring A buf1's end and ring B buf0's start there is no gap
        # (0x261dc8 + 0x100 == 0x261ec8), but well past ring D there is.
        self.assertIsNone(si.ring_label(0x264138))
        # The ColdFire TX frame region is not a ring: ring_label() keeps
        # its original, narrower meaning (see frame_label()/
        # coldfire_region() below).
        self.assertIsNone(si.ring_label(0x2558DC))


class FrameLabelTest(unittest.TestCase):
    """docs/findings/06, "The ColdFire frame is mapped into SHARC DM at
    0x2558dc": FRAME_REGION covers the whole 0x802-byte TX frame."""

    def test_frame_start(self):
        self.assertEqual(si.frame_label(0x2558DC), "coldfire TX frame +0x0")

    def test_known_scalar_offsets_from_finding_04(self):
        # docs/findings/04's per-track TX frame map, base 0x2558dc: machine
        # type at 0x94, the two derived flags at 0x73c/0x75c.
        self.assertEqual(si.frame_label(0x2558DC + 0x94), "coldfire TX frame +0x94")
        self.assertEqual(si.frame_label(0x2558DC + 0x73C), "coldfire TX frame +0x73c")

    def test_gain_table_is_inside_the_frame(self):
        # tools/sharc_harness.FRAME_PATCH_TABLE's own hand-patched
        # "per-track mixer/gain parameter mirror" at 0x2560b8.
        self.assertIsNotNone(si.frame_label(0x2560B8))

    def test_end_exclusive(self):
        self.assertIsNotNone(si.frame_label(0x2558DC + 0x801))
        self.assertIsNone(si.frame_label(0x2558DC + 0x802))

    def test_outside_frame_is_none(self):
        self.assertIsNone(si.frame_label(0x0))
        self.assertIsNone(si.frame_label(0x2558DC - 1))


class ColdfireRegionTest(unittest.TestCase):
    def test_ring_takes_priority_and_cites_the_ring_finding(self):
        name, finding = si.coldfire_region(0x261CC8)
        self.assertEqual(name, "ring A buf0")
        self.assertEqual(finding, si.RING_FINDING)

    def test_frame_cites_the_frame_finding(self):
        name, finding = si.coldfire_region(0x2558DC + 0x94)
        self.assertEqual(name, "coldfire TX frame +0x94")
        self.assertEqual(finding, si.FRAME_FINDING)

    def test_outside_both_is_none(self):
        self.assertIsNone(si.coldfire_region(0x0))


class IsPlausibleTest(unittest.TestCase):
    """Pure address-range logic (sharc.py's own _KNOWN_STRIDED/_KNOWN_RANGES
    tables and sharc_contract._PLAUSIBLE_DM_BANDS): no program database
    needed."""

    def test_known_structure_address_is_always_plausible(self):
        # sharc.py's _KNOWN_STRIDED: the per-track record array.
        self.assertTrue(si._is_plausible(0x2506EC))

    def test_workspace_band_is_plausible(self):
        self.assertTrue(si._is_plausible(0x241000))

    def test_small_address_is_not_plausible(self):
        self.assertFalse(si._is_plausible(0x4))
        self.assertFalse(si._is_plausible(0x14))

    def test_coldfire_load_address_is_not_plausible(self):
        self.assertFalse(si._is_plausible(0x40000000))


class InitWriteInfoTest(unittest.TestCase):
    """A synthetic loader stream, like tests/test_sharc_survey.py's
    ApplyPatchesTest -- no real firmware needed to check overlay-membership
    logic in isolation."""

    def test_unwritten_address_is_not_written(self):
        runner = sr.Runner(loader_memory(), 0x10)
        written, value = si.init_write_info(runner.state, 0x241000)
        self.assertFalse(written)
        self.assertIsNone(value)

    def test_written_address_reports_its_value(self):
        runner = sr.Runner(loader_memory(), 0x10)
        st._dm_write(runner.state, 0x241000, 4, st.Const(0x3F800000))
        written, value = si.init_write_info(runner.state, 0x241000)
        self.assertTrue(written)
        self.assertEqual(value, 0x3F800000)

    def test_written_zero_is_still_written(self):
        # A read alone cannot tell "init wrote 0" from "nothing ever wrote
        # here and this reads as 0 anyway" -- this is the whole point of
        # checking the overlay rather than inferring from a read.
        runner = sr.Runner(loader_memory(), 0x10)
        st._dm_write(runner.state, 0x241000, 4, st.Const(0))
        written, value = si.init_write_info(runner.state, 0x241000)
        self.assertTrue(written)
        self.assertEqual(value, 0)

    def test_partial_write_narrower_than_width_does_not_count(self):
        runner = sr.Runner(loader_memory(), 0x10)
        st._dm_write(runner.state, 0x241000, 1, st.Const(0xFF))
        written, _value = si.init_write_info(runner.state, 0x241000, width=4)
        self.assertFalse(written)


class JsonShapeTest(unittest.TestCase):
    def test_input_label_to_json(self):
        lbl = si.InputLabel(0x241000, "no_writer", "unknown", extra={"refs": []})
        self.assertEqual(
            lbl.to_json(),
            {
                "address": "0x241000",
                "label": "no_writer",
                "detail": "unknown",
                "refs": [],
            },
        )

    def test_inputs_report_counts_and_json(self):
        report = si.InputsReport(
            image="dt2-1.16",
            root=0x1C2B24,
            n_inputs=2,
            labels=[
                si.InputLabel(0x1, "init", "written by run_init (zero, value=0x0)"),
                si.InputLabel(0x2, "no_writer", "unknown"),
            ],
            init_checked=True,
            low_confidence=[0x4],
        )
        self.assertEqual(
            report.counts, {"init": 1, "coldfire": 0, "sharc_root": 0, "no_writer": 1}
        )
        payload = report.to_json()
        self.assertEqual(payload["root"], "0x1c2b24")
        self.assertEqual(payload["low_confidence"], ["0x4"])
        self.assertEqual(len(payload["labels"]), 2)

    def test_dynamic_view_to_json(self):
        view = si.DynamicView(
            instructions=10,
            halt={"reason": "max-steps"},
            n_read_addrs=3,
            n_written_addrs=1,
            unexplained_reads=[0x241000],
        )
        self.assertEqual(
            view.to_json(),
            {
                "instructions": 10,
                "halt": {"reason": "max-steps"},
                "n_read_addrs": 3,
                "n_written_addrs": 1,
                "unexplained_reads": ["0x241000"],
            },
        )


@unittest.skipUnless(
    DT2_116_DB.exists() and DT2_116_BLOB.exists(),
    "out/sharcdb/dt2-1.16.sqlite or out/sections/dt2-1.16/section_7_BLOB.bin "
    "is not available",
)
class BuildStaticTest(unittest.TestCase):
    """build() without a live init overlay: labels (b)/(c)/(d) only, from
    the program database alone -- fast, no emulator run."""

    @classmethod
    def setUpClass(cls):
        cls.img = sharc.load("dt2-1.16")

    def test_boot_writer_is_labelled_sharc_root_without_init(self):
        # docs/findings/06 / test_sharc_contract.py's own
        # test_boot_fills_the_mix_table_base: FUN_1c15e3 (boot/init) is the
        # sole resolved writer of the mix table base (0x252d78).
        report = si.build(self.img, 0x1C2B24)
        self.assertFalse(report.init_checked)
        by_addr = {lbl.address: lbl for lbl in report.labels}
        self.assertIn(0x252D78, by_addr)
        lbl = by_addr[0x252D78]
        self.assertEqual(lbl.label, "sharc_root")
        self.assertIn("boot/init", lbl.detail)

    def test_low_confidence_addresses_are_not_labelled(self):
        report = si.build(self.img, 0x1C2B24)
        labelled_addrs = {lbl.address for lbl in report.labels}
        self.assertNotIn(0x4, labelled_addrs)
        self.assertIn(0x4, report.low_confidence)

    def test_labels_are_sorted_by_the_documented_order(self):
        report = si.build(self.img, 0x1C2B24)
        order = [si._LABEL_ORDER[lbl.label] for lbl in report.labels]
        self.assertEqual(order, sorted(order))

    def test_counts_match_the_label_list(self):
        report = si.build(self.img, 0x1C2B24)
        self.assertEqual(sum(report.counts.values()), len(report.labels))
        self.assertEqual(report.n_inputs, len(report.labels))

    def test_report_json_round_trips(self):
        report = si.build(self.img, 0x1C2B24)
        payload = report.to_json()
        self.assertEqual(payload["root"], "0x1c2b24")
        self.assertEqual(len(payload["labels"]), report.n_inputs)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class BuildWithInitTest(unittest.TestCase):
    """build() against a real post-init overlay (~1.24M instructions, several
    seconds -- see tools/sharc_harness.py's own InitStateRenderTest)."""

    @classmethod
    def setUpClass(cls):
        cls.img = sharc.load("dt2-1.16")
        cls.init_state = si.run_init_state("dt2-1.16")

    def test_boot_writer_moves_to_init_once_checked_against_a_live_overlay(self):
        # Same address as BuildStaticTest's own case: FUN_1c15e3 is boot/init
        # AND run_init() actually reaches it, so with a live overlay this
        # resolves to label (a), not (c).
        report = si.build(self.img, 0x1C2B24, init_state=self.init_state)
        self.assertTrue(report.init_checked)
        by_addr = {lbl.address: lbl for lbl in report.labels}
        self.assertIn(0x252D78, by_addr)
        self.assertEqual(by_addr[0x252D78].label, "init")

    def test_per_track_record_defaults_are_init_written(self):
        # docs/findings/06's "Init writes" / tools/sharc_harness.py's own
        # notes on FUN_1c15e3 filling every voice record with defaults.
        # 2026-09-28 re-pin, 0x3f800000 -> 0x3f000000 (0.5f): the only
        # init writer of 0x2506ec is now sw 0x1c60cc (0.5f). The old 1.0f
        # came from FUN_1c6154's per-voice init stores (sw 0x1c6340/
        # 0x1c6353) landing on this word: the core scaled the plain
        # `modify(I3, M3)` at sw 0x1c6248 (M3 = voice * 0x38) by 4, so
        # voice 8's 0x38-byte record was written at 0x24ffec + 8 * 0xe0 =
        # 0x2506ec. MODIFY does not scale (PRM p.6-10).
        report = si.build(self.img, 0x1C2B24, init_state=self.init_state)
        by_addr = {lbl.address: lbl for lbl in report.labels}
        self.assertIn(0x2506EC, by_addr)
        self.assertEqual(by_addr[0x2506EC].label, "init")
        self.assertIn("0x3f000000", by_addr[0x2506EC].detail)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class DynamicViewTest(unittest.TestCase):
    """dynamic_view() against the real frame call (setup_voice() +
    setup_frame() + fresh_call(block_handler), tools/sharc_harness.py's own
    call chain), with FRAME_PATCH_TABLE so it reaches the same known
    stop tools/sharc_harness.py pins as FRAME_MILESTONE -- run_init() alone is several
    seconds; a Watchpoint-logged run is slower still, so this is its own
    slow test rather than folded into BuildWithInitTest."""

    def test_reaches_the_known_return_mismatch_and_logs_reads(self):
        import sharc_harness as h

        view = si.dynamic_view("dt2-1.16", patch_table=h.FRAME_PATCH_TABLE)
        self.assertEqual(view.instructions, h.FRAME_MILESTONE["instructions"])
        self.assertEqual(view.halt["reason"], h.FRAME_MILESTONE["reason"])
        self.assertGreater(view.n_read_addrs, 0)
        self.assertGreater(view.n_written_addrs, 0)
        # Addresses are un-aliased back to the plain application DM pointer
        # (see dynamic_view()'s own note on SW_ALIAS_BASE), matching the
        # static table's own addressing -- not the loader-relative alias.
        self.assertTrue(all(a < L.SW_ALIAS_BASE for a in view.unexplained_reads))

    def test_unexplained_reads_cross_check_a_known_static_no_writer_address(self):
        import sharc_harness as h

        img = sharc.load("dt2-1.16")
        static = si.build(img, 0x1C2B24)
        by_addr = {lbl.address: lbl for lbl in static.labels}
        no_writer_addrs = {a for a, lbl in by_addr.items() if lbl.label == "no_writer"}
        view = si.dynamic_view("dt2-1.16", patch_table=h.FRAME_PATCH_TABLE)
        unexplained = set(view.unexplained_reads)
        # At least one address the static pass could not find a writer for
        # is genuinely read at runtime with nothing in the run (or init)
        # ever writing it -- the static "no writer found" bucket is not
        # just an artifact of an incomplete reach walk.
        self.assertTrue(no_writer_addrs & unexplained)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
@unittest.skipUnless(
    IDLE_CAPTURE.exists(), "dt2-1.16-idle.dt2cap capture is not available"
)
class DynamicViewCaptureTest(unittest.TestCase):
    """dynamic_view_capture() (lane C2, 2026-09-25) against three frames of
    the idle capture, real per-frame command dispatch and all: frame 0's
    own header is command 1 (a trivial clear, 192 instructions), frame 1 is
    the first real render (95982 instructions, the same "frame-returned"
    count tools/sharc_replay.py's own milestone test pins for its own
    frame 1), and frame 2 -- the second real render -- hits the same
    unconfirmed opcode at 0x1c32b0 that module's milestone test also pins.
    Instruction counts across the two tools agree exactly (178723 =
    192 + 95982 + 82549), an end-to-end cross-check that both drive the
    same real call chain the same way."""

    def test_three_frame_capture_replay_milestone(self):
        # 2026-10-01 re-pin 407044 -> 407022: the SIMD companion correction
        # skips 22 PCs; capture data and halt state are otherwise unchanged.
        # 2026-09-28 re-pin, 178723 -> 407044: the fixed core (Type4b
        # (1,1,1) normal word, (lw) and MODIFY scaling, ShiftImm 0x19
        # or-fdep, SIMD companions and PEy compute) takes the real
        # per-voice paths, which roughly doubles a render (FRAME_MILESTONE
        # went 96,044 -> 213,504 the same way). The run still stops at the
        # same 0x1c32b0 opcode in the third frame.
        view = si.dynamic_view_capture("dt2-1.16", str(IDLE_CAPTURE), n_frames=3)
        self.assertEqual(view.instructions, 407022)
        self.assertEqual(
            view.halt["category"],
            "uncertain or undecodable form: source: firmware (undocumented; unconfirmed)",
        )
        self.assertEqual(view.halt["pc"], 0x1C32B0)
        self.assertGreater(view.n_read_addrs, 0)
        self.assertGreater(view.n_written_addrs, 0)

    @unittest.skipUnless(
        PLAY_PRETRACKS_CAPTURE.exists(),
        "dt2-1.16-play-pretracks.dt2cap capture is not available",
    )
    def test_static_no_writer_count_shrinks_against_a_real_capture_replay(self):
        img = sharc.load("dt2-1.16")
        init_state = si.run_init_state("dt2-1.16")
        static = si.build(img, 0x1C2B24, init_state=init_state)
        no_writer_addrs = {
            lbl.address for lbl in static.labels if lbl.label == "no_writer"
        }
        view = si.dynamic_view_capture(
            "dt2-1.16",
            str(PLAY_PRETRACKS_CAPTURE),
            n_frames=3,
        )
        unexplained = set(view.unexplained_reads)
        # A real, per-frame-dispatched, real-kit-data (docs/findings/06's
        # own "4 tracks with nonzero machine type" pretracks capture)
        # replay retires SOME of the static no_writer bucket (never read on
        # this path, or a concrete dynamic writer found) without retiring
        # all of it -- see the lane's own report for the exact count and
        # which addresses remain.
        still_unexplained = {
            a for a in no_writer_addrs if any((a + i) in unexplained for i in range(4))
        }
        self.assertTrue(still_unexplained)
        self.assertLess(len(still_unexplained), len(no_writer_addrs))


if __name__ == "__main__":
    unittest.main()
