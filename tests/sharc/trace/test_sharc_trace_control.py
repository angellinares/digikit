"""Symbolic tracer: control flow, loops, system forms and state copies."""

import struct
import unittest
from importlib import import_module

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")
from support.sharc_trace_build import insn  # noqa: E402

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T

from support.sharc_trace_build import TraceRunMixin, f32, full_compute  # noqa: E402


class TraceControlFlowTest(TraceRunMixin, unittest.TestCase):
    """Control flow, loops and system forms."""

    def test_type21_nops_advance_without_changing_state(self):
        for name, length, expected_pc in (("21a", 6, 0x13), ("21c", 2, 0x11)):
            with self.subTest(name=name):
                state = T.State(0x10, {0: T.Const(7)})
                advanced = self.run_one(state, insn(name, {}, length=length))
                self.assertEqual(advanced.pc_sw, expected_pc)
                self.assertEqual(advanced.steps, 1)
                self.assertEqual(advanced.uregs, {0: T.Const(7)})
                self.assertEqual(advanced.trace, [])

    def test_type6b_executes_selected_shift_immediate_operations(self):
        cases = (
            ("bset0", 0x023E00300000, 0, 0, 1),
            ("bclr0", 0x023E00310000, 0, 0xFFFFFFFF, 0xFFFFFFFE),
            ("bset2", 0x023E00300200, 0, 0, 4),
            ("fext30", 0x023E38108022, 2, 0xFFFFFFFF, 0x3FFFFFFF),
            ("fext31", 0x023E3810C022, 2, 0xFFFFFFFF, 0x7FFFFFFF),
            ("lshift-10", 0x023E7800F611, 1, 0xFFFFFFFF, 0x003FFFFF),
            ("ashift-8-frame-reader", 0x023E7801F822, 2, 0x80000000, 0xFF800000),
            ("bset-r2", 0x023E00300022, 2, 0, 1),
            ("fext16", 0x023E20100022, 2, 0xFFFFFFFF, 0xFFFF),
            ("bclr25", 0x023E00311922, 2, 0xFFFFFFFF, 0xFDFFFFFF),
        )
        for label, word, register, initial, expected in cases:
            with self.subTest(label=label):
                words = (word >> 32, (word >> 16) & 0xFFFF, word & 0xFFFF)
                record = T.decode_at(struct.pack("<3H", *words), 0, 0)
                self.assertEqual(
                    (record.type_name, record.length_bytes, record.kind),
                    ("6b_shiftimm", 6, "confident"),
                )
                state = T.State(0x10, {register: T.Const(initial)})
                record.offset = 0
                advanced = self.run_one(state, record)
                self.assertEqual(advanced.pc_sw, 0x13)
                self.assertEqual(advanced.uregs[register], T.Const(expected))
                self.assertEqual(advanced.trace[-1]["action"], "compute")

    def test_type6b_rejects_unresolved_predicates_and_opcodes(self):
        base = {
            "cond[4:0]": 0x1E,
            "dataex[3:0]": 0,
            "shiftimm[22:16]": 0x30,
            "shiftimm[15:0]": 0,
        }
        stopped = self.run_one(T.State(0x10), insn("6b_shiftimm", base, length=6))
        self.assertEqual(stopped.stopped, "unsupported conditional SIMD Type6b")
        base["cond[4:0]"] = 0x1F
        base["shiftimm[22:16]"] = 0x11
        stopped = self.run_one(T.State(0x10), insn("6b_shiftimm", base, length=6))
        self.assertEqual(stopped.stopped, "unsupported ShiftImm opcode 0x11")

    def test_type11c_immediate_conditional_and_delayed_rts(self):
        immediate = {"x": 0, "j": 0, "cond[4:0]": 0x1F, "lr": 0}
        state = T.State(0x10, call_stack=[0x200])
        returned = self.run_one(state, insn("11c", immediate, length=2))
        self.assertEqual((returned.pc_sw, returned.call_stack), (0x200, []))
        self.assertEqual(returned.trace[-1]["action"], "loaded-call-return")

        conditional = dict(immediate, **{"cond[4:0]": 0})
        states = T._execute(
            T.State(0x10, call_stack=[0x200]),
            insn("11c", conditional, length=2),
        )
        self.assertEqual(
            sorted((state.pc_sw, state.call_stack) for state in states),
            [(0x11, [0x200]), (0x200, [])],
        )

        delayed = dict(immediate, j=1)
        state = self.run_one(
            T.State(0x10, call_stack=[0x200]), insn("11c", delayed, length=2)
        )
        self.assertEqual((state.pc_sw, state.pending.slots), (0x11, 2))
        state = self.run_one(state, insn("21c", {}, length=2))
        state = self.run_one(state, insn("21c", {}, length=2))
        self.assertEqual((state.pc_sw, state.call_stack), (0x200, []))

        loop_entry = self.run_one(
            T.State(
                0x10,
                call_stack=[0x13],
                loops=[T.Loop(0x13, 0x20, 2, 1)],
            ),
            insn("11c", immediate, length=2),
        )
        self.assertEqual(loop_entry.stopped, "return reached loop PC-stack entry")

    def test_type11a_rejects_rti_and_loop_reentry(self):
        # PGR p.9-44/9-45: RTI additionally pops the status/loop stacks and
        # clears IRPTL/IMASKP; LR changes loop-PC-stack reentry. Neither is
        # modeled, so both must fail closed, the same as Type11c's own "x".
        base = {
            "x": 0,
            "cond[4:0]": 0x1F,
            "j": 0,
            "e": 0,
            "lr": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        rti = dict(base, x=1)
        stopped = self.run_one(
            T.State(0x10, call_stack=[0x200]), insn("11a", rti, length=6)
        )
        self.assertEqual(stopped.stopped, "unsupported Type11a RTI")
        loop_reentry = dict(base, lr=1)
        stopped = self.run_one(
            T.State(0x10, call_stack=[0x200]), insn("11a", loop_reentry, length=6)
        )
        self.assertEqual(stopped.stopped, "unsupported Type11a loop reentry")

    def test_type11a_unconditional_delayed_return_with_parallel_compute(self):
        # The real byte-backed sw 0x1c0701 instance inside the shared
        # reciprocal helper blk88@0x1c06ba: IF ALWAYS RTS (DB), F8 = mant
        # (F0); (x=0 RTS, cond=0x1F always-true, j=1 delayed, e=0 plain
        # compute, lr=0). PGR p.9-44: "If a compute operation is specified
        # without the ELSE, it is performed in parallel with the return."
        fields = {
            "x": 0,
            "cond[4:0]": 0x1F,
            "j": 1,
            "e": 0,
            "lr": 0,
            **full_compute(0, 0xAD, 8, 0, 0),  # R8 = mant(F0)
        }
        state = self.run_one(
            T.State(0x10, {0: T.Const(f32(3.5))}, call_stack=[0x200]),
            insn("11a", fields, length=6),
        )
        self.assertEqual(state.uregs[8], T.Const(0xE0000000))
        self.assertEqual(state.pending.slots, 2)
        self.assertEqual(
            state.call_stack, [0x200]
        )  # not popped until the delay slots run
        state = self.run_one(state, insn("21c", {}, length=2))
        state = self.run_one(state, insn("21c", {}, length=2))
        self.assertEqual((state.pc_sw, state.call_stack), (0x200, []))

    def test_type11a_else_compute_runs_only_when_return_not_taken(self):
        # cond=0x03 reads ASTATX.AC (PGR Table 10-4); e=1 makes the compute
        # an ELSE clause, so it must run only when the return is NOT taken.
        fields = {
            "x": 0,
            "cond[4:0]": 0x03,
            "j": 0,
            "e": 1,
            "lr": 0,
            **full_compute(0, 0x01, 0, 1, 2),  # R0 = R1 + R2
        }
        astatx = T.UREG_CODES["ASTATX"]
        taken = self.run_one(
            T.State(
                0x10,
                {astatx: T.Const(1 << T.AC_BIT), 1: T.Const(2), 2: T.Const(3)},
                call_stack=[0x200],
            ),
            insn("11a", fields, length=6),
        )
        self.assertEqual((taken.pc_sw, taken.call_stack), (0x200, []))
        self.assertNotIn(0, taken.uregs)  # ELSE compute did not run

        not_taken = self.run_one(
            T.State(
                0x10,
                {astatx: T.Const(0), 1: T.Const(2), 2: T.Const(3)},
                call_stack=[0x200],
            ),
            insn("11a", fields, length=6),
        )
        self.assertEqual(not_taken.uregs[0], T.Const(5))
        self.assertEqual(not_taken.call_stack, [0x200])  # return not taken

    def test_type11a_forks_on_unknown_predicate_applying_compute_per_branch(self):
        fields = {
            "x": 0,
            "cond[4:0]": 0x03,
            "j": 0,
            "e": 0,
            "lr": 0,
            **full_compute(0, 0x01, 0, 1, 2),  # R0 = R1 + R2
        }
        states = T._execute(
            T.State(0x10, {1: T.Const(2), 2: T.Const(3)}, call_stack=[0x200]),
            insn("11a", fields, length=6),
        )
        by_pc = {state.pc_sw: state for state in states}
        self.assertEqual(sorted(by_pc), [0x13, 0x200])
        # e=0: the plain compute runs on the taken (returning) branch only.
        self.assertEqual(by_pc[0x200].uregs[0], T.Const(5))
        self.assertNotIn(0, by_pc[0x13].uregs)

    def test_fixed_pass_drives_eq_return_from_documented_astat_flags(self):
        astatx = T.UREG_CODES["ASTATX"]
        mode1 = T.UREG_CODES["MODE1"]
        pass_zero = {
            "srcureghigh[4:0]": 0,
            "srcureglow[1:1]": 0,
            "srcureglow[0:0]": 0,
            "dstureg[6:0]": 24,
            "cond[4:0]": 0x1F,
            "compute[22:16]": 2,
            "compute[15:0]": 0x1000,
        }
        state = self.run_one(
            T.State(
                0x1C0FB9,
                {0: T.Const(0), astatx: T.Const(0xFFFFFFFF), mode1: T.Const(0)},
                call_stack=[0x200],
            ),
            insn("5a_move", pass_zero, length=6),
        )
        self.assertEqual(state.uregs[24], T.Const(0))
        # AZ (bit0) sets, AC/AV/AN/AS/AI (bits1-5) and AF (bit10) all clear;
        # bits outside the ALU-flags mask are untouched from 0xFFFFFFFF.
        self.assertEqual(state.uregs[astatx], T.Const(0xFFFFFBC1))
        self.assertTrue(T._predicate(state, 0x00))
        self.assertFalse(T._predicate(state, 0x10))

        returned = T._execute(
            state,
            insn(
                "11c",
                {"x": 0, "j": 0, "cond[4:0]": 0x00, "lr": 0},
                length=2,
            ),
        )
        self.assertEqual(len(returned), 1)
        self.assertEqual((returned[0].pc_sw, returned[0].call_stack), (0x200, []))

    def test_fixed_pass_sets_an_and_invalidates_unknown_astat(self):
        astatx = T.UREG_CODES["ASTATX"]
        mode1 = T.UREG_CODES["MODE1"]
        short_pass = {"compute[11:0]": 0x201}

        negative = self.run_one(
            T.State(
                0x10,
                {
                    1: T.Const(0x80000000),
                    astatx: T.Const(0xFFFF0000),
                    mode1: T.Const(0),
                },
            ),
            insn("2c", short_pass, length=2),
        )
        self.assertEqual(negative.uregs[astatx], T.Const(0xFFFF0004))
        self.assertFalse(T._predicate(negative, 0x00))
        self.assertTrue(T._predicate(negative, 0x10))

        unknown = self.run_one(
            T.State(0x10, {astatx: T.Const(0), mode1: T.Const(0)}),
            insn("2c", short_pass, length=2),
        )
        # PASS of an uninitialized register only invalidates the bits PASS
        # itself defines (the ALU-flags group); the other, previously-known
        # bits of ASTATX (all 0 here) are not thrown away with them.
        self.assertEqual(
            unknown.uregs[astatx], T.PartialConst(0xFFFFFFFF & ~T.ALU_FLAGS_MASK, 0)
        )
        self.assertIsNone(T._astatx_known_bit(unknown.uregs[astatx], T.AZ_BIT))
        self.assertEqual(T._astatx_known_bit(unknown.uregs[astatx], T.MN_BIT), False)
        self.assertIsNone(T._predicate(unknown, 0x00))
        self.assertIsNone(T._predicate(unknown, 0x10))

    def test_type11c_rejects_rti_and_loop_reentry(self):
        for fields, reason in (
            ({"x": 1, "j": 0, "cond[4:0]": 0x1F, "lr": 0}, "unsupported Type11c RTI"),
            (
                {"x": 0, "j": 0, "cond[4:0]": 0x1F, "lr": 1},
                "unsupported Type11c loop reentry",
            ),
        ):
            with self.subTest(reason=reason):
                stopped = self.run_one(T.State(0x10), insn("11c", fields, length=2))
                self.assertEqual(stopped.stopped, reason)

    def test_type18a_mutates_documented_system_register_bits(self):
        mode1 = T.UREG_CODES["MODE1"]
        cases = (
            (0, 0x30, 0x0C, 0x3C, "set"),
            (1, 0x3F, 0x0C, 0x33, "clear"),
            (2, 0x30, 0x0C, 0x3C, "toggle"),
        )
        for bop, initial, mask, expected, operation in cases:
            with self.subTest(operation=operation):
                state = T.State(0x10, {mode1: T.Const(initial)})
                advanced = self.run_one(
                    state,
                    insn(
                        "18a",
                        {
                            "bop[2:0]": bop,
                            "sreg[3:0]": 2,
                            "data[31:16]": 0,
                            "data[15:0]": mask,
                        },
                        length=6,
                    ),
                )
                self.assertEqual(advanced.uregs[mode1], T.Const(expected))
                self.assertEqual(advanced.trace[-1]["action"], "system-bit-op")
                self.assertEqual(advanced.trace[-1]["operation"], operation)

    def test_type18a_bit_tests_drive_tf_predicates(self):
        mode1 = T.UREG_CODES["MODE1"]
        astatx = T.UREG_CODES["ASTATX"]
        astaty = T.UREG_CODES["ASTATY"]
        stkyx = T.UREG_CODES["STKYX"]
        stkyy = T.UREG_CODES["STKYY"]

        def bit_test(bop, mask, state):
            return self.run_one(
                state,
                insn(
                    "18a",
                    {
                        "bop[2:0]": bop,
                        "sreg[3:0]": 8,
                        "data[31:16]": mask >> 16,
                        "data[15:0]": mask & 0xFFFF,
                    },
                    length=6,
                ),
            )

        state = bit_test(
            4,
            4,
            T.State(
                0x10,
                {mode1: T.Const(0), stkyx: T.Const(4), astatx: T.Const(0)},
            ),
        )
        self.assertEqual(state.trace[-1]["action"], "system-bit-test")
        self.assertTrue(state.trace[-1]["result"])
        self.assertTrue(T._predicate(state, 0x0D))
        self.assertFalse(T._predicate(state, 0x1D))

        state = bit_test(4, 8, state)
        self.assertFalse(state.trace[-1]["result"])
        self.assertFalse(T._predicate(state, 0x0D))
        self.assertTrue(T._predicate(state, 0x1D))

        state = bit_test(5, 4, state)
        self.assertTrue(state.trace[-1]["result"])
        self.assertTrue(T._predicate(state, 0x0D))

        simd = bit_test(
            4,
            4,
            T.State(
                0x10,
                {
                    mode1: T.Const(1 << 21),
                    stkyx: T.Const(4),
                    stkyy: T.Const(0),
                    astatx: T.Const(0),
                    astaty: T.Const(0),
                },
            ),
        )
        self.assertEqual(simd.uregs[astatx], T.Const(1 << 18))
        self.assertEqual(simd.uregs[astaty], T.Const(0))

        unknown = bit_test(
            4,
            4,
            T.State(0x10, {mode1: T.Const(0), astatx: T.Const(0)}),
        )
        self.assertIsNone(T._predicate(unknown, 0x0D))

    def test_type18a_bit_test_resolves_from_partial_astatx(self):
        # Regression for a frame-render stop at 0x1c088e (dt2-1.16,
        # FUN_1c0874): ASTATX was PartialConst(mask=0xFFDFFF, bits=0x400)
        # -- every bit BIT TST 0x20 needs (just bit 5) known False -- so the
        # bit-test/xor-test branch must resolve a definite result from the
        # PartialConst's own known bits (SHARC+ Core Programming Reference,
        # "Type 18a ISA/VISA (register bit manipulation)", pp.16-2/16-3,
        # extraction pages 400-401: TST is true iff every bit DATA sets is
        # also set in the register), not report Unknown just because the
        # whole 32-bit register isn't a plain Const.
        mode1 = T.UREG_CODES["MODE1"]
        astatx = T.UREG_CODES["ASTATX"]
        # sreg 8 selects STKYX (tested by USTAT1+sreg) as the source; the
        # BTF result always lands in ASTATX regardless of which sreg was
        # tested -- matching test_type18a_bit_tests_drive_tf_predicates
        # above, which uses the same sreg=8/stkyx pairing.
        stkyx = T.UREG_CODES["STKYX"]

        def bit_test(bop, mask, source_value):
            return self.run_one(
                T.State(
                    0x10, {mode1: T.Const(0), stkyx: source_value, astatx: T.Const(0)}
                ),
                insn(
                    "18a",
                    {
                        "bop[2:0]": bop,
                        "sreg[3:0]": 8,
                        "data[31:16]": mask >> 16,
                        "data[15:0]": mask & 0xFFFF,
                    },
                    length=6,
                ),
            )

        # TST 0x20: bit 5 is known (False) even though bit 13 is not --
        # the tested bit alone decides the result, definitely False.
        partial = T.PartialConst(0xFFDFFF, 0x400)
        state = bit_test(4, 0x20, partial)
        self.assertEqual(state.trace[-1]["result"], False)
        self.assertEqual(state.uregs[astatx], T.Const(0))

        # TST needing a bit the PartialConst does not know (bit 13, 0x2000)
        # stays Unknown.
        state = bit_test(4, 0x2000, partial)
        self.assertIsNone(state.trace[-1]["result"])

        # TST over several known-set bits within the PartialConst's mask
        # (0x400 itself, bit 10) resolves True.
        state = bit_test(4, 0x400, partial)
        self.assertEqual(state.trace[-1]["result"], True)

        # XOR (full-register equality) is False as soon as one known bit
        # disagrees with DATA (bit 5 wants 1, is known 0), even though bit
        # 13 stays unknown.
        state = bit_test(5, 0x20, partial)
        self.assertEqual(state.trace[-1]["result"], False)

    def test_type20a_pushes_and_pops_status_registers(self):
        codes = {
            name: T.UREG_CODES[name]
            for name in ("ASTATX", "ASTATY", "MODE1", "MMASK", "STKYX")
        }
        state = T.State(
            0x10,
            {
                codes["ASTATX"]: T.Const(1),
                codes["ASTATY"]: T.Const(2),
                codes["MODE1"]: T.Const(0x3C),
                codes["MMASK"]: T.Const(0x0C),
                codes["STKYX"]: T.Const(1 << 24),
            },
        )
        push_fields = {
            "lpu": 0,
            "lpo": 0,
            "spu": 1,
            "spo": 0,
            "ppu": 0,
            "ppo": 0,
            "fc": 1,
            "llii": 0,
            "lldwb": 0,
            "lldi": 0,
            "llpwb": 0,
            "llpi": 0,
        }
        state = self.run_one(state, insn("20a", push_fields, length=6))
        self.assertEqual(state.uregs[codes["MODE1"]], T.Const(0x30))
        self.assertEqual(state.uregs[codes["STKYX"]], T.Const(0))
        self.assertEqual(len(state.status_stack), 1)
        self.assertTrue(state.trace[-1]["flush_cache"])

        state.uregs[codes["ASTATX"]] = T.Const(9)
        state.uregs[codes["ASTATY"]] = T.Const(10)
        pop_fields = dict(push_fields, spu=0, spo=1, fc=0)
        state = self.run_one(state, insn("20a", pop_fields, length=6))
        self.assertEqual(state.uregs[codes["ASTATX"]], T.Const(1))
        self.assertEqual(state.uregs[codes["ASTATY"]], T.Const(2))
        self.assertEqual(state.uregs[codes["MODE1"]], T.Const(0x3C))
        self.assertEqual(state.uregs[codes["STKYX"]], T.Const(1 << 24))
        self.assertEqual(state.status_stack, [])

        state.uregs[codes["STKYX"]] = T.Const(0)
        empty_pop_fields = dict(pop_fields, lpo=1, ppo=1)
        state = self.run_one(state, insn("20a", empty_pop_fields, length=6))
        self.assertEqual(
            state.uregs[codes["STKYX"]],
            T.Const((1 << 26) | (1 << 24) | (1 << 22)),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["CURLCNTR"]], T.Const(0xFFFFFFFF))
        self.assertEqual(state.uregs[T.UREG_CODES["PCSTK"]], T.Const(0x7FFFFFFF))
        self.assertEqual(state.uregs[T.UREG_CODES["PCSTKP"]], T.Const(0))

        mixed = self.run_one(
            T.State(0x10),
            insn("20a", dict(push_fields, lpo=1), length=6),
        )
        self.assertEqual(mixed.stopped, "invalid Type20a mixed push and pop")

    def test_type12a_immediate_runs_a_counted_single_instruction_loop(self):
        state = T.State(0x10, {T.UREG_CODES["STKYX"]: T.Const(0)})
        setup = insn(
            "12a_imm",
            {
                "data[15:8]": 0,
                "data[7:0]": 4,
                "mode": 1,
                "reladdr[22:16]": 0,
                "reladdr[15:0]": 3,
            },
            length=6,
        )
        state = self.run_one(state, setup)
        self.assertEqual(state.pc_sw, 0x13)
        self.assertEqual(state.loops, [T.Loop(0x13, 0x13, 4, 1)])
        self.assertEqual(state.call_stack, [0x13])
        self.assertEqual(state.uregs[T.UREG_CODES["PCSTK"]], T.Const(0x13))
        self.assertEqual(state.uregs[T.UREG_CODES["LCNTR"]], T.Const(4))
        self.assertEqual(state.uregs[T.UREG_CODES["STKYX"]], T.Const(0))

        nop = insn("21a", {}, length=6)
        for remaining in (3, 2, 1):
            state = self.run_one(state, nop)
            self.assertEqual(state.pc_sw, 0x13)
            self.assertEqual(state.loops[-1].remaining, remaining)
            self.assertEqual(state.uregs[T.UREG_CODES["CURLCNTR"]], T.Const(remaining))
        state = self.run_one(state, nop)
        self.assertEqual(state.pc_sw, 0x16)
        self.assertEqual(state.loops, [])
        self.assertEqual(state.uregs[T.UREG_CODES["CURLCNTR"]], T.Const(0xFFFFFFFF))
        self.assertEqual(
            state.uregs[T.UREG_CODES["STKYX"]], T.Const((1 << 26) | (1 << 22))
        )
        self.assertEqual(state.call_stack, [])
        self.assertEqual(state.trace[-1]["action"], "loop-exit")

        zero = self.run_one(
            T.State(0x10),
            insn(
                "12a_imm",
                {
                    "data[15:8]": 0,
                    "data[7:0]": 0,
                    "mode": 0,
                    "reladdr[22:16]": 0,
                    "reladdr[15:0]": 3,
                },
                length=6,
            ),
        )
        self.assertEqual(zero.stopped, "unsupported zero-count Type12a loop")

        ureg_fields = {
            "ureg[6:0]": 4,
            "mode": 1,
            "reladdr[22:16]": 0,
            "reladdr[15:0]": 3,
        }
        ureg = self.run_one(
            T.State(0x10, {4: T.Const(23)}),
            insn("12a_ureg", ureg_fields, length=6),
        )
        self.assertEqual(ureg.loops, [T.Loop(0x13, 0x13, 23, 1)])
        unknown = self.run_one(T.State(0x10), insn("12a_ureg", ureg_fields, length=6))
        self.assertEqual(unknown.stopped, "nonconcrete Type12a UREG loop count")

    def test_type12a_concrete_ureg_one_and_maximum_immediate(self):
        fields = {
            "ureg[6:0]": 4,
            "mode": 0,
            "reladdr[22:16]": 0,
            "reladdr[15:0]": 3,
        }
        state = self.run_one(
            T.State(0x10, {T.UREG_CODES["STKYX"]: T.Const(0), 4: T.Const(1)}),
            insn("12a_ureg", fields, length=6),
        )
        self.assertEqual(state.loops, [T.Loop(0x13, 0x13, 1, 0)])
        self.assertEqual(state.uregs[T.UREG_CODES["LCNTR"]], T.Const(1))
        self.assertEqual(state.uregs[T.UREG_CODES["CURLCNTR"]], T.Const(1))
        state = self.run_one(state, insn("21a", {}, length=6))
        self.assertEqual(state.pc_sw, 0x16)
        self.assertEqual(state.loops, [])
        self.assertEqual(state.trace[-1]["action"], "loop-exit")

        maximum = self.run_one(
            T.State(0x10),
            insn(
                "12a_imm",
                {
                    "data[15:8]": 0xFF,
                    "data[7:0]": 0xFF,
                    "mode": 1,
                    "reladdr[22:16]": 0,
                    "reladdr[15:0]": 3,
                },
                length=6,
            ),
        )
        self.assertEqual(maximum.loops, [T.Loop(0x13, 0x13, 0xFFFF, 1)])
        self.assertEqual(maximum.uregs[T.UREG_CODES["LCNTR"]], T.Const(0xFFFF))
        self.assertEqual(maximum.uregs[T.UREG_CODES["CURLCNTR"]], T.Const(0xFFFF))

    def test_17_signed_and_assembled(self):
        s = self.run_one(
            T.State(10), insn("17b", {"ureg[6:0]": 2, "data[15:0]": 0xFFFF})
        )
        self.assertEqual(s.uregs[2], T.Const(0xFFFFFFFF))
        s = self.run_one(
            T.State(10),
            insn(
                "17a", {"ureg[6:0]": 2, "data[31:16]": 0x1234, "data[15:0]": 0x5678}, 6
            ),
        )
        self.assertEqual(s.uregs[2], T.Const(0x12345678))


class StateCopyPreservesEveryFieldTest(unittest.TestCase):
    """sharc_core.state._copy() used to build its State(...) with positional
    args that stopped before explicit_memory_model (and any field added
    after it), so a fork silently reset that field to its default -- see
    tools/sharc_dev/sharc_harness/'s _clone_state docstring, which worked around
    this by not calling _copy() at all. Every conditional/predicated fork
    inside forms_move.py/sequencer.py/forms_flow.py/forms_compute.py/
    forms_dag.py goes through _copy(), so this was live on any run that set
    explicit_memory_model=True and then hit one of those forks."""

    def test_explicit_memory_model_survives_a_copy(self):
        state = T.State(0x10, {}, explicit_memory_model=True)
        copied = T._copy(state)
        self.assertTrue(copied.explicit_memory_model)

    def test_mutable_containers_are_independent_copies(self):
        state = T.State(0x10, {T.UREG_CODES["R0"]: T.Const(1)})
        state.overlay[0x1000] = 0xAB
        state.call_stack.append(0x20)
        copied = T._copy(state)
        copied.overlay[0x1000] = 0xCD
        copied.call_stack.append(0x30)
        copied.uregs[T.UREG_CODES["R0"]] = T.Const(2)
        self.assertEqual(state.overlay[0x1000], 0xAB)
        self.assertEqual(state.call_stack, [0x20])
        self.assertEqual(state.uregs[T.UREG_CODES["R0"]], T.Const(1))


if __name__ == "__main__":
    unittest.main()
