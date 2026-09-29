"""Tests for tools/sharc_survey.py.

Pure-Python pieces (load_patch_table, apply_patches, category
classification, the ASTAT-group-from-unknowns mapping) are tested against
synthetic loader streams, following tests/test_sharc_run.py's convention.
The end-to-end SurveyStopTest class needs the real dt2-1.16 program
database and firmware blob (out/sharcdb, out/sections -- see the worktree's
symlinks); it is skipped, not marked slow, when they are not available,
matching tests/test_sharc_graph.py.
"""

import contextlib
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest
from importlib import import_module

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
sv = import_module("sharc_survey")
sr = import_module("sharc_run")
T = import_module("sharc_trace")
L = import_module("sharcldr")
sharc = import_module("sharc")

DT2_116_DB = pathlib.Path("out/sharcdb/dt2-1.16.sqlite")
DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")


def loader_memory():
    return L.LoadedMemory.from_stream(b"")


class LoadPatchTableTest(unittest.TestCase):
    def test_no_path_is_an_empty_table(self):
        self.assertEqual(sv.load_patch_table(None), {})

    def test_json_hex_string_keys_and_values(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "patch.json")
            with open(path, "w") as fh:
                json.dump({"0x10": [["reg", "R0", "0x40000000"]]}, fh)
            table = sv.load_patch_table(path)
        self.assertEqual(table, {0x10: [("reg", "R0", 0x40000000)]})

    def test_json_decimal_keys_and_int_values(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "patch.json")
            with open(path, "w") as fh:
                json.dump({"16": [["mem", 32, 255]]}, fh)
            table = sv.load_patch_table(path)
        self.assertEqual(table, {16: [("mem", 32, 255)]})

    def test_py_module_reads_patch_table_dict(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "patch.py")
            with open(path, "w") as fh:
                fh.write("PATCH_TABLE = {0x10: [('reg', 'R0', 5)]}\n")
            table = sv.load_patch_table(path)
        self.assertEqual(table, {0x10: [("reg", "R0", 5)]})

    def test_py_module_without_patch_table_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "patch.py")
            with open(path, "w") as fh:
                fh.write("X = 1\n")
            with self.assertRaises(SystemExit):
                sv.load_patch_table(path)


class ApplyPatchesTest(unittest.TestCase):
    def _runner(self):
        return sr.Runner(loader_memory(), 0x10, regs={"R0": 1})

    def test_reg_patch_by_name(self):
        runner = self._runner()
        applied = sv.apply_patches(runner, {0x10: [("reg", "R0", 0x40000000)]})
        self.assertEqual(runner.state.uregs[T.UREG_CODES["R0"]], T.Const(0x40000000))
        self.assertEqual(applied, ["reg R0 = 0x40000000"])

    def test_reg_patch_by_code(self):
        runner = self._runner()
        code = T.UREG_CODES["R1"]
        sv.apply_patches(runner, {0x10: [("reg", code, 7)]})
        self.assertEqual(runner.state.uregs[code], T.Const(7))

    def test_mem_patch_writes_through_dm_write(self):
        runner = sr.Runner(loader_memory(), 0x10)
        applied = sv.apply_patches(runner, {0x10: [("mem", 0x31400, 0xCAFEBABE)]})
        self.assertEqual(
            T._dm_read(runner.state, T.Const(0x31400), 4), T.Const(0xCAFEBABE)
        )
        self.assertEqual(applied, ["mem[0x31400] = 0xcafebabe"])

    def test_mem_patch_with_non_int_target_raises(self):
        runner = self._runner()
        with self.assertRaises(ValueError):
            sv.apply_patches(runner, {0x10: [("mem", "R0", 1)]})

    def test_unknown_kind_raises(self):
        runner = self._runner()
        with self.assertRaises(ValueError):
            sv.apply_patches(runner, {0x10: [("bogus", "R0", 1)]})

    def test_no_entry_for_this_pc_is_a_noop(self):
        runner = self._runner()
        applied = sv.apply_patches(runner, {0x1000: [("reg", "R0", 9)]})
        self.assertEqual(applied, [])
        self.assertEqual(runner.state.uregs[T.UREG_CODES["R0"]], T.Const(1))

    def test_branch_kind_is_a_noop_here(self):
        # A "branch" entry is only meaningful once run_collect_all() has
        # actually seen a fork at this pc (see its own module note); a
        # plain run_with_patches() caller with no fork there must not have
        # this raise, and must not touch any register.
        runner = self._runner()
        applied = sv.apply_patches(runner, {0x10: [("branch", None, 1)]})
        self.assertEqual(applied, [])
        self.assertEqual(runner.state.uregs[T.UREG_CODES["R0"]], T.Const(1))


class RunWithPatchesTest(unittest.TestCase):
    def test_patch_applied_before_the_triggering_pc_executes(self):
        # A patch at the CURRENT pc lands before that instruction runs: an
        # unconditional "21a" NOP-family instruction just advances pc, so
        # if the patch had not taken, the fork below (predicated on R0's
        # patched-vs-original value) could not distinguish the two cases.
        runner = sr.Runner(loader_memory(), 0x10, regs={"R0": 1}, diagnose_unknown=True)
        runner._cache[0x10] = _insn("21a", {}, length=4)
        halt = sv.run_with_patches(runner, {0x10: [("reg", "R0", 5)]}, max_steps=1)
        self.assertEqual(halt.reason, "max-steps")
        self.assertEqual(runner.state.uregs[T.UREG_CODES["R0"]], T.Const(5))

    def test_max_steps_budget_produces_a_max_steps_halt(self):
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = _insn("21a", {}, length=4)
        runner._cache[0x12] = _insn("21a", {}, length=4)
        halt = sv.run_with_patches(runner, {}, max_steps=2)
        self.assertEqual(halt.reason, "max-steps")
        self.assertEqual(runner.instructions, 2)


def _insn(name, fields, length=4, kind="confident"):
    Instruction = import_module("sharc_disasm").Instruction
    return Instruction(0, length, name, fields, kind=kind)


class AstatGroupsFromUnknownsTest(unittest.TestCase):
    def test_single_group(self):
        self.assertEqual(
            sv._astat_groups_from_unknowns(("cond=TF", "ASTATX.BTF")), {"BTF"}
        )

    def test_multiple_bits_same_group_collapse_to_one(self):
        self.assertEqual(
            sv._astat_groups_from_unknowns(
                ("cond=LT", "ASTATX.AF", "ASTATX.AN", "ASTATX.AZ", "ASTATX.AV")
            ),
            {"ALU"},
        )

    def test_two_different_registers_same_group(self):
        self.assertEqual(
            sv._astat_groups_from_unknowns(("cond=AC", "ASTATX.AC", "ASTATY.AC")),
            {"ALU"},
        )

    def test_mode1_only_is_empty(self):
        self.assertEqual(sv._astat_groups_from_unknowns(("cond=EQ", "MODE1")), set())

    def test_no_unknowns_is_empty(self):
        self.assertEqual(sv._astat_groups_from_unknowns(()), set())

    def test_reason_and_last_writer_notes_are_not_mistaken_for_bits(self):
        # note_reason()/note_writer() text (tools/sharc_run.py) never
        # matches _UNKNOWN_RE, so it must not spuriously contribute a group.
        unknowns = (
            "cond=AC",
            "ASTATX.AC",
            "last write to ASTATX at 0x10",
            "ASTATX reason: x",
        )
        self.assertEqual(sv._astat_groups_from_unknowns(unknowns), {"ALU"})


class SurveyStopCategoryTest(unittest.TestCase):
    def _stop(self, reason, text=""):
        halt = sr.Halt(reason, 0x10, "8a_rel", text)
        return sv.SurveyStop(
            halt=halt, runner=None, img=None, instructions=0, elapsed=0.0
        )

    def test_fork(self):
        self.assertEqual(self._stop("fork (2 successors): ...").category, "fork")

    def test_mmr(self):
        self.assertEqual(self._stop("mmr").category, "mmr")

    def test_watchpoint(self):
        self.assertEqual(self._stop("watchpoint").category, "watchpoint")

    def test_frame_returned(self):
        self.assertEqual(
            self._stop("return without followed call").category, "frame-returned"
        )

    def test_max_steps(self):
        self.assertEqual(self._stop("max-steps").category, "max-steps")

    def test_return_mismatch(self):
        reason = "return target 0x20 differs from recorded return 0x30"
        self.assertEqual(self._stop(reason).category, "return-mismatch")

    def test_breakpoint_falls_back_to_the_raw_reason(self):
        self.assertEqual(self._stop("breakpoint").category, "breakpoint")


class CollectStopJsonTest(unittest.TestCase):
    def test_to_json_shape(self):
        stop = sv.CollectStop(
            index=1,
            category="fork",
            pc=0x10,
            form="8a_rel",
            text="note",
            unknowns=("cond=EQ",),
            last_writer={"ASTATX": 0x8},
            slice_summary="0x10 ...",
            call_stack=["0x20"],
            resolution="not-taken (default)",
            guess_number=1,
            downstream_of=(),
        )
        self.assertEqual(
            stop.to_json(),
            {
                "index": 1,
                "category": "fork",
                "pc": 0x10,
                "form": "8a_rel",
                "text": "note",
                "unknowns": ["cond=EQ"],
                "last_writer": {"ASTATX": 0x8},
                "slice_summary": "0x10 ...",
                "call_stack": ["0x20"],
                "resolution": "not-taken (default)",
                "guess_number": 1,
                "downstream_of": [],
            },
        )


# Fixture for SurveyEndToEndTest/CollectAllEndToEndTest below: FUN_1c2b24
# (the frame render root) called bare, i.e. with no run_init()/snapshot, so
# every register and internal-RAM cell starts unwritten. Commit bcc0b9b
# (the FIX/TRUNC NaN/Inf saturation fix) resolved the fork this fixture used
# to pin (lane-L7's original survey, docs/findings/06 -- see git history for
# that version of this file): with explicit_memory_model's default (True,
# unwritten internal RAM reads as concrete Const(0)) the bare frame render
# no longer forks at all, and instead runs cleanly to a "return without
# followed call" at 0x1c3284.
#
# --no-explicit-memory-model (an existing sv.survey()/collect_all() option,
# documented in tools/sharc_survey.py's own --help; not a new tool
# behaviour) leaves an unwritten cell Unknown instead, which reintroduces a
# real fork: FUN_1c2b24+0x1e7 (0x1c2d08, "R1 = leftz(R1, R0)") computes its
# SV (shift overflow) flag from R1 = DM(I4, M0), a read of a cell this bare
# run never wrote, so the JUMP IF NOT SV at 0x1c2d0b cannot be resolved.
# Verified live on out/sharcdb/dt2-1.16.sqlite + out/sections/dt2-1.16
# (2026-09-26, branch work/sharc-emulator @ 34a8d38): deterministic across
# repeated runs, same pc and instruction count every time.
_ROOT = 0x1C2B24
_FIRST_FORK_PC = 0x1C2D0B
_FIRST_FORK_INSTRUCTIONS = 13895
# The very next fork downstream, once the first is resolved either way
# (guessed not-taken, or forced taken): FUN_1c2b24+0x1f4 (0x1c2d18, "IF NOT
# SV R13 = M6"), the same unresolved SV flag read by a second, nearby
# instruction.
_SECOND_FORK_PC = 0x1C2D18
# A concrete "reg" patch at the instruction that CONSUMES the unresolved
# read (0x1c2d08 itself, before "R1 = leftz(R1, R0)" executes) rather than
# at its own upstream write: pins R1 to an arbitrary concrete value, which
# makes leftz's SV flag concrete too, so the fork at 0x1c2d0b never happens.
_REG_PATCH_PC = 0x1C2D08


@unittest.skipUnless(
    DT2_116_DB.exists() and DT2_116_BLOB.exists(),
    "out/sharcdb/dt2-1.16.sqlite or out/sections/dt2-1.16/section_7_BLOB.bin "
    "is not available",
)
class SurveyEndToEndTest(unittest.TestCase):
    """FUN_1c2b24 (the frame render root) from a bare state, with unwritten
    internal RAM left Unknown (--no-explicit-memory-model) so it still
    forks after commit bcc0b9b -- see the fixture comment above. A real,
    stable, deterministic execution of the actual firmware, not a synthetic
    one."""

    START = _ROOT
    EXPECTED_HALT_PC = _FIRST_FORK_PC
    EXPECTED_INSTRUCTIONS = _FIRST_FORK_INSTRUCTIONS

    def _survey(self, **kwargs):
        kwargs.setdefault("explicit_memory_model", False)
        return sv.survey(image="dt2-1.16", root=self.START, **kwargs)

    def test_bare_survey_reaches_the_known_first_fork(self):
        stop = self._survey()
        self.assertEqual(stop.category, "fork")
        self.assertEqual(stop.halt.pc_sw, self.EXPECTED_HALT_PC)
        self.assertEqual(stop.instructions, self.EXPECTED_INSTRUCTIONS)
        self.assertIn("cond=NOT SV", stop.halt.unknowns)
        self.assertIn("ASTATX.SV", stop.halt.unknowns)
        self.assertTrue(
            any(u.startswith("last write to ASTATX at ") for u in stop.halt.unknowns)
        )

    def test_patch_table_advances_past_the_known_fork(self):
        patch_table = {_REG_PATCH_PC: [("reg", "R1", 0x40000000)]}
        stop = self._survey(patch_table=patch_table)
        self.assertGreater(stop.instructions, self.EXPECTED_INSTRUCTIONS)
        self.assertNotEqual(stop.halt.pc_sw, self.EXPECTED_HALT_PC)

    def test_report_stop_prints_the_documented_fields(self):
        stop = self._survey()
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            sv.report_stop(stop)
        text = buf.getvalue()
        self.assertIn("pc=0x%x" % self.EXPECTED_HALT_PC, text)
        self.assertIn("form=8a_rel", text)
        self.assertIn("ASTATX.SV", text)
        self.assertIn("last flag writer:", text)
        self.assertIn("slice (all inputs):", text)
        self.assertIn("call stack (return addrs, innermost last):", text)
        self.assertIn("FUN_", text)  # some function name resolved somewhere

    def test_json_output_round_trips_through_main(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            sv.main(
                [
                    "dt2-1.16",
                    "--root",
                    hex(self.START),
                    "--max-steps",
                    str(self.EXPECTED_INSTRUCTIONS + 10),
                    "--no-explicit-memory-model",
                    "--json",
                ]
            )
        payload = json.loads(buf.getvalue())
        self.assertEqual(payload["reason"].startswith("fork"), True)
        self.assertEqual(payload["pc_sw"], self.EXPECTED_HALT_PC)
        self.assertEqual(payload["category"], "fork")
        self.assertIn("ASTATX", payload["last_writer"])
        self.assertIsInstance(payload["call_stack"], list)


@unittest.skipUnless(
    DT2_116_DB.exists() and DT2_116_BLOB.exists(),
    "out/sharcdb/dt2-1.16.sqlite or out/sections/dt2-1.16/section_7_BLOB.bin "
    "is not available",
)
class CollectAllEndToEndTest(unittest.TestCase):
    """--collect-all against the same bare FUN_1c2b24 root SurveyEndToEndTest
    uses (see the fixture comment above this module's SurveyEndToEndTest):
    it must find the exact same first fork survey() does (same pc, same
    instruction count -- collect_all() is not a different execution, only a
    different stopping rule), then keep going with a default not-taken
    guess rather than stopping there."""

    START = _ROOT
    FIRST_FORK_PC = _FIRST_FORK_PC
    FIRST_FORK_INSTRUCTIONS = _FIRST_FORK_INSTRUCTIONS
    SECOND_FORK_PC = _SECOND_FORK_PC
    # A max_steps that finds exactly the first two forks (guesses == 2) and
    # nothing past them: the third fork (0x1c2d33) only appears once steps
    # reach 13913, so 13910 leaves margin on both sides (verified live,
    # 2026-09-26).
    TWO_FORKS_MAX_STEPS = _FIRST_FORK_INSTRUCTIONS + 15

    def _collect_all(self, **kwargs):
        kwargs.setdefault("explicit_memory_model", False)
        return sv.collect_all(image="dt2-1.16", root=self.START, **kwargs)

    def test_continues_past_the_first_fork_with_a_default_guess(self):
        # Just past the first fork: the second known one (0x1c2d18) sits
        # only a handful of instructions later, so a slightly larger budget
        # would already find it too -- this only pins the FIRST stop.
        result = self._collect_all(max_steps=self.FIRST_FORK_INSTRUCTIONS + 1)
        first = result.stops[0]
        self.assertEqual(first.category, "fork")
        self.assertEqual(first.pc, self.FIRST_FORK_PC)
        self.assertEqual(first.resolution, "not-taken (default)")
        self.assertEqual(first.guess_number, 1)
        self.assertEqual(first.downstream_of, ())
        self.assertIn("cond=NOT SV", first.unknowns)
        # The chosen (not-taken) branch keeps running past the fork instead
        # of stopping the whole survey there.
        self.assertGreater(result.instructions, self.FIRST_FORK_INSTRUCTIONS)

    def test_finds_a_second_distinct_fork_downstream_of_the_first_guess(self):
        result = self._collect_all(max_steps=self.TWO_FORKS_MAX_STEPS)
        self.assertGreaterEqual(len(result.stops), 2)
        first, second = result.stops[0], result.stops[1]
        self.assertEqual(first.pc, self.FIRST_FORK_PC)
        self.assertEqual(second.pc, self.SECOND_FORK_PC)
        self.assertEqual(second.category, "fork")
        self.assertEqual(second.guess_number, 2)
        self.assertEqual(second.downstream_of, (1,))
        self.assertEqual(result.guesses, 2)

    def test_a_branch_patch_table_entry_overrides_the_default_and_is_not_a_guess(self):
        default = self._collect_all(max_steps=self.FIRST_FORK_INSTRUCTIONS + 1)
        forced = self._collect_all(
            max_steps=self.FIRST_FORK_INSTRUCTIONS + 1,
            patch_table={self.FIRST_FORK_PC: [("branch", None, 1)]},
        )
        self.assertEqual(default.stops[0].pc, forced.stops[0].pc)
        self.assertEqual(default.stops[0].resolution, "not-taken (default)")
        self.assertEqual(forced.stops[0].resolution, "taken (patch-table)")
        self.assertEqual(default.stops[0].guess_number, 1)
        self.assertIsNone(forced.stops[0].guess_number)

    def test_a_reg_patch_can_still_avoid_a_fork_entirely(self):
        # The existing "reg"/"mem" PatchTable kind (applied unconditionally
        # before the instruction runs, via apply_patches()) still works
        # under collect_all(): a good enough register value means the fork
        # this lane's fixture comment already explains never happens, so it
        # is not in the stop list at all.
        result = self._collect_all(
            max_steps=self.FIRST_FORK_INSTRUCTIONS + 1000,
            patch_table={_REG_PATCH_PC: [("reg", "R1", 0x40000000)]},
        )
        pcs = [s.pc for s in result.stops]
        self.assertNotIn(self.FIRST_FORK_PC, pcs)

    def test_max_steps_budget_is_recorded_as_a_terminal_stop(self):
        result = self._collect_all(max_steps=10)
        self.assertEqual(len(result.stops), 1)
        self.assertEqual(result.stops[0].category, "max-steps")
        self.assertEqual(result.instructions, 10)

    def test_report_collect_all_prints_a_row_per_stop(self):
        result = self._collect_all(max_steps=self.TWO_FORKS_MAX_STEPS)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            sv.report_collect_all(result)
        text = buf.getvalue()
        self.assertIn("%#x" % self.FIRST_FORK_PC, text)
        self.assertIn("%#x" % self.SECOND_FORK_PC, text)
        self.assertIn("guess #1", text)
        self.assertIn("terminal stop detail:", text)

    def test_json_round_trips_through_main(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            sv.main(
                [
                    "dt2-1.16",
                    "--root",
                    hex(self.START),
                    "--max-steps",
                    str(self.FIRST_FORK_INSTRUCTIONS + 1),
                    "--no-explicit-memory-model",
                    "--collect-all",
                    "--json",
                ]
            )
        payload = json.loads(buf.getvalue())
        self.assertEqual(payload["stops"][0]["pc"], self.FIRST_FORK_PC)
        self.assertEqual(payload["stops"][0]["category"], "fork")
        self.assertEqual(payload["guesses"], 1)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class CollectAllFrameRunTest(unittest.TestCase):
    """run_collect_all() against the real block_handler dispatch (setup_frame()
    + fresh_call(), like tools/sharc_harness.py's own FrameRenderFromInitTest)
    -- this lane's own validation that --collect-all finds every stop on the
    frame-render path a single survey()/run_with_patches() run only ever
    reports one of at a time. run_init() is ~1.24M instructions (several
    seconds); shared across both methods, like InitStateRenderTest."""

    @classmethod
    def setUpClass(cls):
        import sharc_harness as h  # noqa: PLC0415 -- only this slow test needs it

        cls.h = h
        cls.img = sharc.load("dt2-1.16")
        memory = h.load_image_memory("dt2-1.16")
        init = h.run_init(memory, "dt2-1.16")
        if not init.ran:  # pragma: no cover - defensive, see run_init
            raise RuntimeError("run_init did not complete: %s" % init.error)
        cls.base_runner = init.runner
        h.setup_voice(cls.base_runner.state, "dt2-1.16", 0, sample_len=4096)
        cls.block_handler = h.setup_frame(cls.base_runner.state, "dt2-1.16")

    def _fresh_call(self):
        return self.base_runner.fresh_call(self.block_handler, diagnose_unknown=True)

    def test_with_frame_patch_table_is_a_single_stop(self):
        # FRAME_PATCH_TABLE's two register hypotheses already avoid every
        # fork on this path (docs/findings/06, this lane's handover): the
        # only stop collect-all records is the same FRAME_MILESTONE stop
        # tools/sharc_harness.py's own FrameRenderFromInitTest pins -- a
        # genuine RETURN since the Y1 lane's Type3a (LW) fix (2026-09-25):
        # sv._categorize maps "return without followed call" to its own
        # "frame-returned" category (see that function), not the raw halt
        # reason string.
        result = sv.run_collect_all(
            self._fresh_call(),
            self.h.FRAME_PATCH_TABLE,
            max_steps=4_000_000,
            img=self.img,
        )
        self.assertEqual(len(result.stops), 1)
        milestone = self.h.FRAME_MILESTONE
        self.assertEqual(result.instructions, milestone["instructions"])
        last = result.stops[0]
        self.assertEqual(last.category, "frame-returned")
        self.assertEqual(last.pc, milestone["pc_sw"])

    def test_without_patch_table_is_the_same_single_stop(self):
        result = sv.run_collect_all(
            self._fresh_call(), {}, max_steps=4_000_000, img=self.img
        )
        # 2026-09-28 re-pin, "more than 5 guesses" -> none: every fork this
        # run used to guess through came from tools/sharc_core bugs fixed
        # since (ShiftImm 0x19 read as BITEXT gave Unknown operands to
        # FUN_1c4914's fcomp, the reason FRAME_PATCH_TABLE had its last
        # entry; Type4b/(lw)/MODIFY addressing). FRAME_PATCH_TABLE is empty
        # and the bare run is the same single FRAME_MILESTONE stop.
        self.assertEqual(result.guesses, 0)
        milestone = self.h.FRAME_MILESTONE
        self.assertEqual(len(result.stops), 1)
        self.assertEqual(result.instructions, milestone["instructions"])
        self.assertEqual(result.stops[-1].category, "frame-returned")
        self.assertEqual(result.stops[-1].pc, milestone["pc_sw"])


if __name__ == "__main__":
    unittest.main()
