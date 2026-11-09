"""Synthetic tests for tools/sharc_dev/sharc_run.py, the concrete single-path runner.

Follows the tests/sharc/trace/ convention: build tiny synthetic loader
streams and Instruction records by hand rather than reading real firmware
bytes (out/, firmware/ and anything derived from them are Elektron's
copyright and must never end up here).
"""

import os
import pathlib
import tempfile
import unittest
from importlib import import_module
from unittest.mock import patch

import pytest
from support.paths import REPO_ROOT
from support.sharc_trace_build import insn, loader_block, loader_memory

sr = import_module("tools.sharc_dev.sharc_run")
T = import_module("tools.sharc_dev.sharc_trace")
L = import_module("tools.sharc_dev.sharcldr")
# Only for the two firmware-backed watchpoint regression tests below
# (WatchpointFirmwareTest): tools/sharc_dev/sharc_harness/ owns voice-record layout
# and setup_voice()/call_render(), which this file must not hand-duplicate
# (see InitSnapshotFirmwareTest's own docstring for why a different, earlier
# firmware test here chose to duplicate rather than import it -- that
# reasoning was about a module edited by a *different* concurrent lane at
# the time; tools/sharc_dev/sharc_harness/ is not being edited by anyone else here).
h = import_module("tools.sharc_dev.sharc_harness")

ROOT = str(REPO_ROOT)
DT2_116_BLOB = pathlib.Path(ROOT) / "out/sections/dt2-1.17/section_7_BLOB.bin"


class ResetUregsTest(unittest.TestCase):
    def test_every_ureg_is_seeded(self):
        uregs = sr.reset_uregs()
        self.assertEqual(len(uregs), len(T.UREG_NAMES))
        for code in range(len(T.UREG_NAMES)):
            self.assertIsInstance(uregs[code], T.Const)

    def test_core_reset_values_are_used_where_documented(self):
        uregs = sr.reset_uregs()
        self.assertEqual(uregs[T.UREG_CODES["MODE1"]], T.Const(0))
        self.assertEqual(uregs[T.UREG_CODES["ASTATX"]], T.Const(0))

    def test_undocumented_uregs_default_to_zero(self):
        uregs = sr.reset_uregs()
        self.assertEqual(uregs[T.UREG_CODES["R0"]], T.Const(0))
        self.assertEqual(uregs[T.UREG_CODES["L7"]], T.Const(0))

    def test_overrides_win_over_reset_values(self):
        uregs = sr.reset_uregs({"R4": 0x1234, "MODE1": 5})
        self.assertEqual(uregs[T.UREG_CODES["R4"]], T.Const(0x1234))
        self.assertEqual(uregs[T.UREG_CODES["MODE1"]], T.Const(5))

    def test_negative_override_matches_seed_value_semantics(self):
        uregs = sr.reset_uregs({"M7": -1})
        self.assertEqual(uregs[T.UREG_CODES["M7"]], T.Const(0xFFFFFFFF))


class MakeStateTest(unittest.TestCase):
    def test_requires_loaded_memory(self):
        with self.assertRaises(ValueError):
            sr.make_state(b"\x00\x00", 0x10)

    def test_default_dag_modify_regs_are_seeded(self):
        state = sr.make_state(loader_memory(), 0x10)
        self.assertEqual(state.uregs[T.UREG_CODES["M6"]], T.Const(1))
        self.assertEqual(state.uregs[T.UREG_CODES["M7"]], T.Const(0xFFFFFFFF))

    def test_record_events_defaults_off(self):
        state = sr.make_state(loader_memory(), 0x10)
        self.assertFalse(state.record_events)

    def test_poke_writes_through_dm_write(self):
        state = sr.make_state(loader_memory(), 0x10, pokes={0x31400: 0xCAFEBABE})
        self.assertEqual(T._dm_read(state, T.Const(0x31400), 4), T.Const(0xCAFEBABE))

    def test_bad_poke_raises(self):
        # Not an MMR and not in the 32-bit-normal-word external range, and
        # nothing loaded to alias against: _dm_write() cannot place it.
        with self.assertRaises(ValueError):
            sr.make_state(
                loader_memory(), 0x10, pokes={0x1000000: 1}, assume_nw32=False
            )


class FreshCallStateTest(unittest.TestCase):
    """sr.fresh_call_state(): the fix for the exact bug the frame-render
    survey hit cloning a halted init Runner for a new root (pc/call_stack/
    loops/status_stack alone is not enough -- pending/stopped must also be
    cleared, or the clone's first step() immediately re-fires whatever the
    source Runner halted on)."""

    def _halted_state(self):
        state = sr.make_state(loader_memory(), 0x10, regs={"R0": 5})
        state.pending = T.Pending(target=0x20, call=True, return_sw=0x18)
        state.stopped = "return without followed call"
        state.call_stack = [0x18]
        state.loops = [T.Loop(0x10, 0x14, 3, 0)]
        state.status_stack = [(T.Const(0), T.Const(0), T.Const(0))]
        return state

    def test_clears_pending_stopped_and_per_call_bookkeeping(self):
        fresh = sr.fresh_call_state(self._halted_state(), 0x1000)
        self.assertEqual(fresh.pc_sw, 0x1000)
        self.assertIsNone(fresh.pending)
        self.assertIsNone(fresh.stopped)
        self.assertEqual(fresh.call_stack, [])
        self.assertEqual(fresh.loops, [])
        self.assertEqual(fresh.status_stack, [])

    def test_keeps_scalar_config_and_prior_register_state(self):
        source = self._halted_state()
        fresh = sr.fresh_call_state(source, 0x1000)
        self.assertEqual(fresh.explicit_memory_model, source.explicit_memory_model)
        self.assertEqual(fresh.uregs[T.UREG_CODES["R0"]], T.Const(5))

    def test_regs_override_applied_after_clone(self):
        fresh = sr.fresh_call_state(
            self._halted_state(), 0x1000, regs={"R0": 0x40000000}
        )
        self.assertEqual(fresh.uregs[T.UREG_CODES["R0"]], T.Const(0x40000000))

    def test_return_address_becomes_the_only_call_stack_entry(self):
        fresh = sr.fresh_call_state(self._halted_state(), 0x1000, return_address=0x2000)
        self.assertEqual(fresh.call_stack, [0x2000])

    def test_no_return_address_leaves_call_stack_empty(self):
        fresh = sr.fresh_call_state(self._halted_state(), 0x1000)
        self.assertEqual(fresh.call_stack, [])

    def test_does_not_mutate_the_source_state(self):
        source = self._halted_state()
        fresh = sr.fresh_call_state(source, 0x1000, regs={"R0": 9})
        fresh.overlay[0] = 0xFF
        self.assertNotIn(0, source.overlay)
        self.assertEqual(source.uregs[T.UREG_CODES["R0"]], T.Const(5))
        self.assertEqual(source.pc_sw, 0x10)
        self.assertEqual(source.stopped, "return without followed call")


class RunnerFreshCallTest(unittest.TestCase):
    def test_fresh_call_is_a_new_runner_with_its_own_counters(self):
        runner = sr.Runner(loader_memory(), 0x10, diagnose_unknown=True)
        runner._cache[0x10] = insn("21a", {}, length=4)
        runner.step()
        self.assertEqual(runner.instructions, 1)
        new_runner = runner.fresh_call(0x2000, regs={"R0": 7}, return_address=0x30)
        self.assertIsNot(new_runner, runner)
        self.assertEqual(new_runner.state.pc_sw, 0x2000)
        self.assertEqual(new_runner.state.call_stack, [0x30])
        self.assertEqual(new_runner.state.uregs[T.UREG_CODES["R0"]], T.Const(7))
        self.assertEqual(new_runner.instructions, 0)
        self.assertEqual(new_runner.max_call_depth_reached, 0)
        self.assertEqual(len(new_runner.form_counts), 0)
        # instructions=1 on the source Runner is unaffected.
        self.assertEqual(runner.instructions, 1)

    def test_fresh_call_shares_the_decode_cache(self):
        runner = sr.Runner(loader_memory(), 0x10)
        new_runner = runner.fresh_call(0x2000)
        self.assertIs(new_runner._cache, runner._cache)

    def test_fresh_call_does_not_share_watchpoints(self):
        runner = sr.Runner(loader_memory(), 0x10, watchpoints=[sr.Watchpoint(0, 4)])
        new_runner = runner.fresh_call(0x2000)
        self.assertEqual(new_runner.watch_log, ())
        self.assertIsNone(new_runner._watch)

    def test_fresh_call_defaults_diagnose_unknown_from_source(self):
        runner = sr.Runner(loader_memory(), 0x10, diagnose_unknown=True)
        new_runner = runner.fresh_call(0x2000)
        self.assertTrue(new_runner.diagnose_unknown)
        overridden = runner.fresh_call(0x2000, diagnose_unknown=False)
        self.assertFalse(overridden.diagnose_unknown)


class RunnerStepTest(unittest.TestCase):
    def test_advances_pc_and_counts_one_instruction(self):
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn("21a", {}, length=6)
        runner.step()
        self.assertEqual(runner.state.pc_sw, 0x13)
        self.assertEqual(runner.instructions, 1)
        self.assertEqual(runner.form_counts["21a"], 1)

    def test_record_events_off_leaves_trace_empty_or_minimal(self):
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn("21a", {}, length=6)
        runner.step()
        # 21a is a pure NOP-advance with no _event() calls at all either way.
        self.assertEqual(runner.state.trace, [])

    def test_decode_cache_avoids_redecoding(self):
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn("21c", {}, length=2)
        with patch.object(sr.st, "decode_at") as mock_decode:
            first = runner._decode(0x10)
            second = runner._decode(0x10)
        mock_decode.assert_not_called()
        self.assertIs(first, second)

    def test_decode_cache_calls_through_on_miss(self):
        runner = sr.Runner(loader_memory(), 0x10)
        sentinel = insn("21c", {}, length=2)
        with patch.object(sr.st, "decode_at", return_value=sentinel) as mock_decode:
            got = runner._decode(0x10)
            got_again = runner._decode(0x10)
        mock_decode.assert_called_once()
        self.assertIs(got, sentinel)
        self.assertIs(got_again, sentinel)

    def test_invalidate_forces_redecode(self):
        runner = sr.Runner(loader_memory(), 0x10)
        first_insn = insn("21c", {}, length=2)
        second_insn = insn("21a", {}, length=6)
        with patch.object(
            sr.st, "decode_at", side_effect=[first_insn, second_insn]
        ) as mock_decode:
            got = runner._decode(0x10)
            runner.invalidate(0x10)
            got_after = runner._decode(0x10)
        self.assertEqual(mock_decode.call_count, 2)
        self.assertIs(got, first_insn)
        self.assertIs(got_after, second_insn)

    def test_breakpoint_halts_before_executing(self):
        runner = sr.Runner(loader_memory(), 0x10, breakpoints=[0x10])
        runner._cache[0x10] = insn("21a", {}, length=6)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(ctx.exception.reason, "breakpoint")
        self.assertEqual(ctx.exception.pc_sw, 0x10)
        self.assertEqual(runner.instructions, 0)

    def _type9a_abs_fields(self, **changes):
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 31,
            "pmi[2:2]": 1,
            "pmi[1:0]": 0,
            "pmm[2:0]": 5,
            "j": 0,
            "e": 0,
            "ci": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        fields.update(changes)
        return fields

    def test_depth0_return_is_a_clean_halt_not_an_exception_from_advance(self):
        # PGR's delayed-return idiom (pmm==6, j=1); tests/sharc/trace/'s
        # test_type9a_abs_i4_m6_delayed_is_return_with_compute confirms
        # sharc_trace itself reports this as state.stopped ==
        # "return without followed call" for an empty call_stack -- i.e. the
        # call-depth-0 RTS this runner's entry routines are expected to hit.
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn(
            "9a_abs", self._type9a_abs_fields(**{"pmm[2:0]": 6, "j": 1}), length=6
        )
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(ctx.exception.reason, "return without followed call")
        self.assertEqual(ctx.exception.form, "9a_abs")

    def test_unresolved_predicate_is_reported_as_a_fork_halt(self):
        # An 8a_rel conditional branch whose predicate bit (SV, cond 0x07)
        # is not known forks sharc_trace into taken/not_taken -- exactly the
        # case a fully concrete run should never hit, and Runner.step() must
        # turn into a Halt instead of silently taking one branch.
        runner = sr.Runner(
            loader_memory(), 0x10, regs={"ASTATX": T.Unknown("forced for test")}
        )
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x07,
            "j": 1,
            "ci": 0,
            "reladdr[23:16]": 0,
            "reladdr[15:0]": 16,
        }
        runner._cache[0x10] = insn("8a_rel", fields, length=4)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertTrue(ctx.exception.reason.startswith("fork"))
        self.assertEqual(ctx.exception.pc_sw, 0x10)
        self.assertEqual(ctx.exception.form, "8a_rel")
        # No half-executed state was kept as "the" state after a fork.
        self.assertEqual(runner.instructions, 0)

    def test_provisional_interpretations_off_by_default(self):
        # No --provisional given -> Runner threads an empty mapping, and
        # 21p_undoc16 (a confirmed-decode, no-confirmed-semantics form)
        # stops exactly as before this mechanism existed.
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn("21p_undoc16", {"operand[6:0]": 0x25}, length=2)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(
            ctx.exception.reason,
            "undocumented form 21p_undoc16 has no confirmed semantics",
        )
        self.assertEqual(runner.state.provisional_interpreted, ())

    def test_provisional_interpretations_opt_in_nop_advances(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            provisional_interpretations={"21p_undoc16": "nop"},
        )
        runner._cache[0x10] = insn("21p_undoc16", {"operand[6:0]": 0x25}, length=2)
        runner.step()
        self.assertEqual(runner.state.pc_sw, 0x11)
        self.assertEqual(runner.instructions, 1)
        self.assertEqual(runner.state.provisional_interpreted, ("21p_undoc16",))

    def test_diagnose_unknown_off_by_default_leaves_halt_unchanged(self):
        # diagnose_unknown defaults False: Halt.unknowns must stay empty
        # (and to_json() must therefore omit the key) so a caller that
        # never asked for this -- notably
        # tests/sharc/integration/test_sharc_golden.py's run_frame/run_voice cases -- sees
        # byte-identical output to before this feature existed.
        runner = sr.Runner(
            loader_memory(), 0x10, regs={"ASTATX": T.Unknown("forced for test")}
        )
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x07,
            "j": 1,
            "ci": 0,
            "reladdr[23:16]": 0,
            "reladdr[15:0]": 16,
        }
        runner._cache[0x10] = insn("8a_rel", fields, length=4)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(ctx.exception.unknowns, ())
        self.assertNotIn("unknowns", ctx.exception.to_json())

    def test_diagnose_unknown_names_astatx_flag(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"ASTATX": T.Unknown("forced for test")},
            diagnose_unknown=True,
        )
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x07,  # SV flag (SIMPLE_COND_BITS)
            "j": 1,
            "ci": 0,
            "reladdr[23:16]": 0,
            "reladdr[15:0]": 16,
        }
        runner._cache[0x10] = insn("8a_rel", fields, length=4)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        # "cond=SV" (sharcfn.cond_name(0x07)) first, then the exact bit
        # ("ASTATX.SV", not the old coarser "ASTATX flag") -- no "last
        # write to ASTATX" note yet (this is the Runner's first step, so
        # self._last_writer is still empty), but ASTATX itself is a raw
        # Unknown here (this test's own regs= override, not a PartialConst
        # from a forgotten bit), so its .reason surfaces too.
        expected = ("cond=SV", "ASTATX.SV", "ASTATX reason: forced for test")
        self.assertEqual(ctx.exception.unknowns, expected)
        self.assertEqual(ctx.exception.to_json()["unknowns"], list(expected))

    def test_diagnose_unknown_names_mode1_for_eq_cond(self):
        # 8a_rel's predicate goes through sequencer._predicate_simd_branch(),
        # which reads MODE1 (_simd_active()) to decide how to combine PEx's
        # and PEy's conditions for *every* cond, including EQ (0x00) --
        # unlike plain _predicate(), whose EQ/NE branch is the only one
        # that needs MODE1 at all. ASTATY is left at its Const(0) reset
        # value (concretely EQ-false), so PEx's own unresolved ASTATX and
        # the unresolved MODE1 combination are the two real causes here.
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={
                "MODE1": T.Unknown("forced for test"),
                "ASTATX": T.Unknown("forced for test"),
            },
            diagnose_unknown=True,
        )
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x00,
            "j": 1,
            "ci": 0,
            "reladdr[23:16]": 0,
            "reladdr[15:0]": 16,
        }
        runner._cache[0x10] = insn("8a_rel", fields, length=4)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(
            ctx.exception.unknowns,
            (
                "cond=EQ",
                "MODE1",
                "MODE1 reason: forced for test",
                "ASTATX.AZ",
                "ASTATX reason: forced for test",
            ),
        )

    def test_diagnose_unknown_names_last_writer_pc(self):
        # A real prior write to ASTATX (a Type2a ALU "pass" whose source is
        # Unknown, forgetting every ALU_FLAGS_MASK bit including AZ -- see
        # tools/sharc_dev/sharc_core/flags.py's _astatx_alu_logical) followed by an
        # EQ-conditioned branch reading AZ: the fork's diagnosis should name
        # not just "ASTATX.AZ" but the earlier step's own pc_sw as the last
        # writer -- Runner(diagnose_unknown=True)'s self._last_writer,
        # populated by step() itself (see its docstring), not by this test.
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"R0": T.Unknown("forced for test")},
            diagnose_unknown=True,
        )
        # Type2a, unconditional ALU "R1 = pass R0" (cu=0, opcode=0x21,
        # rn=1, rx=0, ry=0 -- PRM Table 18-5; the same compute[22:16]/
        # compute[15:0] split tests/sharc/trace/'s Type2a fixtures
        # use).
        pass_field = (0x21 << 12) | (1 << 8)
        runner._cache[0x10] = insn(
            "2a",
            {
                "cond[4:0]": 0x1F,
                "compute[22:16]": pass_field >> 16,
                "compute[15:0]": pass_field & 0xFFFF,
            },
            length=6,
        )
        runner.step()
        self.assertEqual(runner.state.pc_sw, 0x13)
        self.assertEqual(runner._last_writer, {"ASTATX": 0x10})
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x00,  # EQ: reads AZ
            "j": 1,
            "ci": 0,
            "reladdr[23:16]": 0,
            "reladdr[15:0]": 16,
        }
        runner._cache[0x13] = insn("8a_rel", fields, length=4)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(
            ctx.exception.unknowns,
            ("cond=EQ", "ASTATX.AZ", "last write to ASTATX at 0x10"),
        )


class ForkDiagnosisTest(unittest.TestCase):
    """Direct tests of sr._fork_diagnosis(), independent of Runner.step()'s
    diagnose_unknown wiring (covered above)."""

    def _state(self, **regs):
        return sr.make_state(loader_memory(), 0x10, regs=regs)

    def test_true_condition_names_nothing(self):
        state = self._state()
        fields = {"cond[4:0]": 0x1F}
        self.assertEqual(sr._fork_diagnosis(state, insn("8a_rel", fields)), ())

    def test_simple_cond_bit_names_astatx_flag(self):
        # ASTATX here is a raw Unknown (this test's own regs= override), so
        # its .reason ("x") surfaces too -- see note_reason()'s docstring:
        # a PartialConst (the common case after a real ALU/MULT/SHIFT
        # compute merely forgets one bit) carries no such reason at all.
        state = self._state(ASTATX=T.Unknown("x"))
        fields = {"cond[4:0]": 0x03}  # AC
        self.assertEqual(
            sr._fork_diagnosis(state, insn("8a_rel", fields)),
            ("cond=AC", "ASTATX.AC", "ASTATX reason: x"),
        )

    def test_simple_cond_bit_resolved_names_nothing(self):
        state = self._state(ASTATX=0)
        fields = {"cond[4:0]": 0x03}
        self.assertEqual(sr._fork_diagnosis(state, insn("8a_rel", fields)), ())

    def test_lt_ge_le_gt_names_every_unknown_alu_flag(self):
        # 5a_move's predicate is plain sequencer._predicate() (not
        # _predicate_simd_branch() -- see _SIMD_BRANCH_FORMS), so this only
        # reads ASTATX/MODE1, not ASTATY: ASTATX entirely unseeded names
        # each of AF/AN/AZ/AV individually now (the old coarser "ASTATX
        # flag" collapsed all four into one), plus MODE1 -- each register's
        # own .reason note appears once, right after its first bit (later
        # bits of the same register dedup the identical reason text).
        state = self._state(ASTATX=T.Unknown("x"), MODE1=T.Unknown("y"))
        fields = {"cond[4:0]": 0x01}  # LT
        self.assertEqual(
            sr._fork_diagnosis(state, insn("5a_move", fields)),
            (
                "cond=LT",
                "ASTATX.AF",
                "ASTATX reason: x",
                "ASTATX.AN",
                "ASTATX.AZ",
                "ASTATX.AV",
                "MODE1",
                "MODE1 reason: y",
            ),
        )

    def test_simd_branch_form_also_checks_astaty_and_eager_mode1(self):
        # 8a_rel's predicate is _predicate_simd_branch(), which reads MODE1
        # for every cond (not only EQ/NE and LT/GE/LE/GT) and both PEs'
        # flags -- see _fork_diagnosis()'s docstring.
        state = self._state(
            ASTATX=T.Unknown("x"), ASTATY=T.Unknown("y"), MODE1=T.Unknown("z")
        )
        fields = {"cond[4:0]": 0x03}  # AC (a SIMPLE_COND_BITS flag)
        self.assertEqual(
            sr._fork_diagnosis(state, insn("8a_rel", fields)),
            (
                "cond=AC",
                "MODE1",
                "MODE1 reason: z",
                "ASTATX.AC",
                "ASTATX reason: x",
                "ASTATY.AC",
                "ASTATY reason: y",
            ),
        )

    def test_missing_cond_field_reports_nothing(self):
        state = self._state()
        self.assertEqual(sr._fork_diagnosis(state, insn("18a", {})), ())

    def test_last_writer_omitted_when_not_given(self):
        # A direct call with no last_writer (the default) never adds a
        # "last write to ..." note, even though the bit itself is named --
        # only Runner.step() ever has a self._last_writer to pass. The
        # reason note is independent of last_writer and still appears.
        state = self._state(ASTATX=T.Unknown("x"))
        fields = {"cond[4:0]": 0x03}
        self.assertEqual(
            sr._fork_diagnosis(state, insn("8a_rel", fields), None),
            ("cond=AC", "ASTATX.AC", "ASTATX reason: x"),
        )

    def test_last_writer_appended_once_per_register(self):
        state = self._state(ASTATX=T.Unknown("x"), MODE1=T.Unknown("y"))
        fields = {"cond[4:0]": 0x01}  # LT: ASTATX checked four times over
        self.assertEqual(
            sr._fork_diagnosis(
                state,
                insn("5a_move", fields),
                {"ASTATX": 0x1000, "MODE1": 0x1004},
            ),
            (
                "cond=LT",
                "ASTATX.AF",
                "last write to ASTATX at 0x1000",
                "ASTATX reason: x",
                "ASTATX.AN",
                "ASTATX.AZ",
                "ASTATX.AV",
                "MODE1",
                "last write to MODE1 at 0x1004",
                "MODE1 reason: y",
            ),
        )


class WatchpointTest(unittest.TestCase):
    # A Type4a (i=I6, g=DM, cond=always) store/load, matching
    # tests/sharc/trace/'s own Type4a fixtures. ADDR is already at
    # or above SW_ALIAS_BASE (0x28000000) so _dm_write/_dm_read never
    # aliases it -- see sharc_core/memory.py's _canonical_dm_address --
    # keeping the watched address exactly what the test says it is.
    ADDR = 0x28300000

    def _store_fields(self, dreg=0):
        return {
            "i[2:0]": 6,
            "g": 0,
            "d": 1,
            "cond[4:0]": 0x1F,
            "data[5:5]": 0,
            "data[4:0]": 0,
            "dreg[3:0]": dreg,
            "u": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }

    def _load_fields(self, dreg=1):
        fields = self._store_fields(dreg=dreg)
        fields["d"] = 0
        return fields

    def test_stopping_write_watchpoint_halts_with_old_and_new_value(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"I6": self.ADDR, "R0": 0xCAFEBABE},
            watchpoints=[
                sr.Watchpoint(self.ADDR, self.ADDR + 4, on_read=False, label="v")
            ],
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(ctx.exception.reason, "watchpoint")
        self.assertEqual(ctx.exception.pc_sw, 0x10)
        self.assertEqual(len(runner.watch_log), 1)
        event = runner.watch_log[0]
        self.assertEqual(event.access, "write")
        self.assertEqual(event.address, self.ADDR)
        self.assertEqual(event.width, 4)
        self.assertIsNone(event.old_value)
        self.assertEqual(event.new_value, 0xCAFEBABE)
        self.assertEqual(event.label, "v")

    def test_log_only_watchpoint_does_not_stop(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"I6": self.ADDR, "R0": 0xCAFEBABE},
            watchpoints=[
                sr.Watchpoint(self.ADDR, self.ADDR + 4, on_read=False, stop=False)
            ],
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        runner.step()  # does not raise
        self.assertEqual(runner.instructions, 1)
        self.assertEqual(len(runner.watch_log), 1)

    def test_read_watchpoint_reports_bytes_written_earlier(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"I6": self.ADDR, "R0": 0xCAFEBABE},
            watchpoints=[
                sr.Watchpoint(self.ADDR, self.ADDR + 4, on_write=False, stop=False)
            ],
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        runner._cache[0x13] = insn("4a", self._load_fields(), length=6)
        runner.step()
        runner.step()
        reads = [e for e in runner.watch_log if e.access == "read"]
        self.assertEqual(len(reads), 4)  # one per byte -- see Watchpoint's docstring
        self.assertEqual(
            sorted(r.address for r in reads),
            list(range(self.ADDR, self.ADDR + 4)),
        )
        r1 = runner.state.uregs[T.UREG_CODES["R1"]]
        self.assertEqual(r1, T.Const(0xCAFEBABE))

    def test_watchpoint_outside_range_does_not_trigger(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"I6": self.ADDR, "R0": 0xCAFEBABE},
            watchpoints=[sr.Watchpoint(self.ADDR + 100, self.ADDR + 200)],
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        runner.step()
        self.assertEqual(runner.watch_log, ())

    def test_no_watchpoints_means_no_tracker_and_empty_log(self):
        runner = sr.Runner(loader_memory(), 0x10)
        self.assertEqual(runner.watch_log, ())
        runner._cache[0x10] = insn("21a", {}, length=6)
        runner.step()
        self.assertEqual(runner.watch_log, ())

    def test_attach_watchpoints_on_existing_runner(self):
        runner = sr.Runner(loader_memory(), 0x10, regs={"I6": self.ADDR, "R0": 1})
        runner.attach_watchpoints(
            [sr.Watchpoint(self.ADDR, self.ADDR + 4, on_read=False)]
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        with self.assertRaises(sr.Halt) as ctx:
            runner.step()
        self.assertEqual(ctx.exception.reason, "watchpoint")

    def test_run_result_to_json_omits_watch_log_when_empty(self):
        runner = sr.Runner(loader_memory(), 0x10, breakpoints=[0x10])
        runner._cache[0x10] = insn("21a", {}, length=6)
        result = runner.run(max_steps=1)
        self.assertNotIn("watch_log", result.to_json())

    def test_run_result_to_json_includes_watch_log_when_present(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            regs={"I6": self.ADDR, "R0": 5},
            watchpoints=[sr.Watchpoint(self.ADDR, self.ADDR + 4, on_read=False)],
        )
        runner._cache[0x10] = insn("4a", self._store_fields(), length=6)
        result = runner.run(max_steps=10)
        self.assertEqual(result.halt.reason, "watchpoint")
        self.assertEqual(len(result.to_json()["watch_log"]), 1)


class ParseWatchArgTest(unittest.TestCase):
    def test_parses_start_end_and_label(self):
        w = sr._parse_watch_arg(
            "100:200:mylabel", on_read=True, on_write=False, stop=True
        )
        self.assertEqual((w.start, w.end, w.label), (0x100, 0x200, "mylabel"))
        self.assertTrue(w.on_read)
        self.assertFalse(w.on_write)
        self.assertTrue(w.stop)

    def test_parses_without_label(self):
        w = sr._parse_watch_arg("10:20", on_read=False, on_write=True, stop=False)
        self.assertEqual((w.start, w.end, w.label), (0x10, 0x20, ""))

    def test_rejects_malformed_spec(self):
        with self.assertRaises(SystemExit):
            sr._parse_watch_arg("nocolon", on_read=True, on_write=True, stop=True)


class SnapshotTest(unittest.TestCase):
    def _stepped_runner(self):
        runner = sr.Runner(loader_memory(), 0x10, regs={"R0": 0x1234})
        runner._cache[0x10] = insn("21a", {}, length=6)
        runner._cache[0x13] = insn("21c", {}, length=2)
        runner.step()
        runner.step()
        return runner

    def test_round_trip_preserves_state(self):
        data = loader_memory()
        runner = sr.Runner(data, 0x10, regs={"R0": 0x1234})
        runner._cache[0x10] = insn("21a", {}, length=6)
        runner.step()
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "snap.pkl")
            sr.save_snapshot(runner, path)
            restored = sr.load_snapshot(path, data)
        self.assertEqual(restored.state.pc_sw, runner.state.pc_sw)
        self.assertEqual(restored.state.uregs, runner.state.uregs)
        self.assertEqual(dict(restored.state.overlay), dict(runner.state.overlay))
        self.assertIs(type(restored.state.overlay), dict)
        self.assertIs(restored.state.concrete, data)
        self.assertEqual(restored.instructions, runner.instructions)
        self.assertEqual(restored.form_counts, runner.form_counts)
        self.assertEqual(restored.breakpoints, runner.breakpoints)
        self.assertIsNone(restored._watch)
        self.assertFalse(restored.diagnose_unknown)

    def test_overlay_saved_as_plain_dict_even_when_watched(self):
        data = loader_memory()
        runner = sr.Runner(
            data,
            0x10,
            regs={"I6": 0x28300000, "R0": 7},
            watchpoints=[
                sr.Watchpoint(0x28300000, 0x28300004, on_read=False, stop=False)
            ],
        )
        runner._cache[0x10] = insn(
            "4a",
            {
                "i[2:0]": 6,
                "g": 0,
                "d": 1,
                "cond[4:0]": 0x1F,
                "data[5:5]": 0,
                "data[4:0]": 0,
                "dreg[3:0]": 0,
                "u": 0,
                "compute[22:16]": 0,
                "compute[15:0]": 0,
            },
            length=6,
        )
        runner.step()
        self.assertIsInstance(runner.state.overlay, sr._WatchingOverlay)
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "snap.pkl")
            sr.save_snapshot(runner, path)
            restored = sr.load_snapshot(path, data)
        self.assertIs(type(restored.state.overlay), dict)
        self.assertEqual(dict(restored.state.overlay), dict(runner.state.overlay))

    def test_refuses_mismatched_image(self):
        runner = self._stepped_runner()
        other = loader_memory(loader_block(1, 0, 4, payload=b"\x00" * 4))
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "snap.pkl")
            sr.save_snapshot(runner, path)
            with self.assertRaises(ValueError):
                sr.load_snapshot(path, other)

    def test_refuses_unsupported_version(self):
        import pickle

        runner = self._stepped_runner()
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "snap.pkl")
            sr.save_snapshot(runner, path)
            with open(path, "rb") as fh:
                payload = pickle.load(fh)
            payload["version"] = 999
            with open(path, "wb") as fh:
                pickle.dump(payload, fh)
            with self.assertRaises(ValueError):
                sr.load_snapshot(path, runner.data)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 SHARC loader is not available")
class InitSnapshotFirmwareTest(unittest.TestCase):
    """Runs the real init routine (FUN_1c15e3, ~1.24M instructions -- see
    docs/findings/06) to its return and checks that a snapshot saved there
    restores byte-for-byte into a fresh Runner. Slow (~15s): run with
    --slow. Talks to sharc_run.py only (not tools/sharc_dev/sharc_harness/, edited
    by another lane concurrently), duplicating its _make_runner() options
    (explicit_memory_model) and the documented init address
    directly.
    """

    INIT_SW = (
        0x1C15E3  # tools/sharc_dev/sharc_symbols.py's REQUIRED_DT2 'init' FuncMatch
    )

    def test_snapshot_round_trip_matches_a_full_init_run(self):
        memory = sr._load_image_memory("dt2-1.17")
        runner = sr.Runner(
            memory,
            self.INIT_SW,
            regs={"I6": 0x300000},
            explicit_memory_model=True,
        )
        result = runner.run(max_steps=2_000_000)
        self.assertEqual(result.halt.reason, "return without followed call")

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "init.pkl")
            sr.save_snapshot(runner, path)
            restored = sr.load_snapshot(path, memory)

        self.assertEqual(restored.state.pc_sw, runner.state.pc_sw)
        self.assertEqual(restored.state.uregs, runner.state.uregs)
        self.assertEqual(dict(restored.state.overlay), dict(runner.state.overlay))
        self.assertEqual(restored.state.mmrs, runner.state.mmrs)
        self.assertEqual(restored.state.call_stack, runner.state.call_stack)
        self.assertEqual(restored.state.loops, runner.state.loops)
        self.assertEqual(restored.instructions, runner.instructions)


@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 SHARC loader is not available")
class WatchpointFirmwareTest(unittest.TestCase):
    """Both watchpoint bugs this lane fixed, reproduced against the real
    DT2 1.16 image via tools/sharc_dev/sharc_harness/'s voice-record layout and
    setup_voice()/call_render() -- sharc_run.py owns none of that record
    layout, and hand-duplicating its Q31/field pokes here would risk
    silently drifting from it. Neither test is slow: both run one bare-state
    voice render (tens to a few hundred instructions), not the ~1.24M-
    instruction init FUN_1c15e3 InitSnapshotFirmwareTest above runs."""

    @classmethod
    def setUpClass(cls):
        cls.memory = h.load_image_memory("dt2-1.17")

    def test_write_watchpoint_on_raw_record_address_fires_through_the_alias(self):
        # Bug 1: a Watchpoint given in the same raw, unaliased terms
        # voice_record_address()/setup_voice() themselves use (well below
        # sharcldr.SW_ALIAS_BASE) used to never fire, because a real store
        # there only ever lands at SW_ALIAS_BASE + address (sharc_core.
        # memory._canonical_dm_address()) while a Watchpoint's own
        # start/end stayed unaliased -- see tests/sharc/semantics/test_sharc_run.py's own
        # WatchpointTest.ADDR comment for the same bug avoided there by
        # picking an address already at/above SW_ALIAS_BASE.
        #
        # A sample_len (40) well under one block's 64 raw interpolation
        # steps makes FUN_1c4f81's own past-limit test clear ACTIVE
        # (record+FIELD_ACTIVE, +0x1b8) partway through this very first
        # call_render() -- a real render write, not a setup poke:
        # setup_voice()'s own ACTIVE=1 poke runs *before* the watchpoint is
        # attached below, so it can never be what this test's watchpoint
        # fires on.
        runner = h.new_runner(self.memory, "dt2-1.17")
        state = runner.state
        h._write_samples(state, 0x310000, [0.5] * 40, "int16")
        record = h.setup_voice(
            state, "dt2-1.17", 0, sample_len=40, sample_base=0x310000
        )
        # A real, still-unmapped low DM address -- the exact case that never
        # fired before this fix.
        self.assertLess(record + h.FIELD_ACTIVE, L.SW_ALIAS_BASE)

        runner.attach_watchpoints(
            [
                sr.Watchpoint(
                    record + h.FIELD_ACTIVE,
                    record + h.FIELD_ACTIVE + 1,
                    on_read=False,
                    label="active",
                )
            ]
        )
        result, _floats = h.call_render(runner, "dt2-1.17", record)

        self.assertEqual(result.halt.reason, "watchpoint")
        writes = [e for e in runner.watch_log if e.access == "write"]
        self.assertEqual(len(writes), 1)
        event = writes[0]
        self.assertEqual(event.label, "active")
        self.assertEqual(event.new_value, 0)
        self.assertEqual(event.address, L.SW_ALIAS_BASE + record + h.FIELD_ACTIVE)

    def test_read_watchpoint_does_not_crash_on_a_followed_call(self):
        # Bug 2: any active read watchpoint used to wrap state.concrete in
        # a duck-typed, non-LoadedMemory proxy; sharc_core.sequencer.
        # _advance() resolving a followed call's target then hit
        # decode_at(state.concrete, None, target)'s
        # isinstance(data, LoadedMemory) check, fell through to the
        # flat-image decode path, and raised ValueError("base_sw is
        # required for flat image decoding") -- on *every* followed call
        # while any read watchpoint was active, not only one that actually
        # touches the watched range. FUN_1c4ecf's own render body reaches a
        # followed call (docs/findings/06: its own call into the decimator
        # at 0xb80000), so a normal voice render already exercises this;
        # the watched range below (0:4) is deliberately far from anything
        # the render touches, to isolate "any read watchpoint exists" from
        # "the watchpoint's own range matters" as the trigger.
        runner = h.new_runner(self.memory, "dt2-1.17")
        state = runner.state
        h._write_samples(state, 0x310000, [0.5] * 256, "int16")
        record = h.setup_voice(
            state, "dt2-1.17", 0, sample_len=256, sample_base=0x310000
        )
        runner.attach_watchpoints([sr.Watchpoint(0, 4, on_write=False, stop=False)])

        result, floats = h.call_render(runner, "dt2-1.17", record)  # must not raise

        self.assertEqual(result.halt.reason, "return without followed call")
        self.assertEqual(len(floats), 64)


class RunnerRunTest(unittest.TestCase):
    def test_stops_at_max_steps(self):
        runner = sr.Runner(loader_memory(), 0x10)
        runner._cache[0x10] = insn("21c", {}, length=2)
        # 21c always advances by one short word, so pc_sw cycles through a
        # small range of addresses this test never has to pre-populate the
        # cache for beyond the first -- decode_at() on an empty LoadedMemory
        # returns kind="unknown", so cap steps at 1 and assert on that.
        result = runner.run(max_steps=1)
        self.assertEqual(result.halt.reason, "max-steps")
        self.assertEqual(result.instructions, 1)
        self.assertEqual(result.form_counts["21c"], 1)
        self.assertGreaterEqual(result.instructions_per_second, 0)

    def test_run_reports_halt_from_step(self):
        runner = sr.Runner(loader_memory(), 0x10, breakpoints=[0x10])
        runner._cache[0x10] = insn("21a", {}, length=6)
        result = runner.run(max_steps=100)
        self.assertEqual(result.halt.reason, "breakpoint")
        self.assertEqual(result.instructions, 0)
        self.assertEqual(result.final_pc_sw, 0x10)

    def test_to_json_shapes(self):
        runner = sr.Runner(loader_memory(), 0x10, breakpoints=[0x10])
        runner._cache[0x10] = insn("21a", {}, length=6)
        result = runner.run(max_steps=100)
        payload = result.to_json()
        self.assertEqual(payload["halt"]["reason"], "breakpoint")
        self.assertIn("instructions_per_second", payload)
        self.assertIn("form_counts", payload)

    def test_to_json_omits_provisional_key_by_default(self):
        # Byte-identical JSON shape to before this field existed for a
        # caller that never opted in (same convention as watch_log above).
        runner = sr.Runner(loader_memory(), 0x10, breakpoints=[0x10])
        runner._cache[0x10] = insn("21a", {}, length=6)
        result = runner.run(max_steps=100)
        self.assertNotIn("provisional", result.to_json())

    def test_provisional_reports_form_mode_and_count_in_result(self):
        runner = sr.Runner(
            loader_memory(),
            0x10,
            provisional_interpretations={"21p_undoc16": "nop"},
        )
        runner._cache[0x10] = insn("21p_undoc16", {"operand[6:0]": 0x25}, length=2)
        runner._cache[0x11] = insn("21p_undoc16", {"operand[6:0]": 0x25}, length=2)
        result = runner.run(max_steps=2)
        self.assertEqual(result.halt.reason, "max-steps")
        self.assertEqual(result.instructions, 2)
        self.assertEqual(result.provisional, (("21p_undoc16", "nop", 2),))
        payload = result.to_json()
        self.assertEqual(
            payload["provisional"],
            [{"form": "21p_undoc16", "mode": "nop", "count": 2}],
        )


class ParseKvTest(unittest.TestCase):
    def test_parses_name_value_pairs(self):
        self.assertEqual(
            sr._parse_kv(["R4=0x10", "I0=5"], "--reg"), {"R4": "0x10", "I0": "5"}
        )

    def test_rejects_missing_equals(self):
        with self.assertRaises(SystemExit):
            sr._parse_kv(["R4"], "--reg")


if __name__ == "__main__":
    unittest.main()
