"""Synthetic tests for the deliberately small SHARC delay tracer."""

import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from importlib import import_module
from unittest.mock import patch

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
T = import_module("sharc_trace")
Instruction = import_module("sharc_disasm").Instruction
L = import_module("sharcldr")
encode = import_module("test_sharc_disasm").encode


def loader_block(code, address, count, arg=0, payload=b""):
    """Build a checksum-valid synthetic loader block."""
    header = bytearray(struct.pack("<IIII", code | 0xAD000000, address, count, arg))
    header[2] = 0
    checksum = 0
    for byte in header:
        checksum ^= byte
    header[2] = checksum
    return bytes(header) + payload


def loader_memory(*blocks):
    return L.LoadedMemory.from_stream(b"".join(blocks))


def insn(name, fields, length=4, kind="confident"):
    return Instruction(0, length, name, fields, kind=kind)


class TraceTest(unittest.TestCase):
    def run_one(self, state, record):
        return T._execute(state, record)[0]

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

    def test_type6b_rejects_unimplemented_predicates_and_opcodes(self):
        base = {
            "cond[4:0]": 0x1E,
            "dataex[3:0]": 0,
            "shiftimm[22:16]": 0x30,
            "shiftimm[15:0]": 0,
        }
        stopped = self.run_one(T.State(0x10), insn("6b_shiftimm", base, length=6))
        self.assertEqual(stopped.stopped, "unsupported Type6b predicate")
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
        self.assertEqual(state.call_stack, [0x200])  # not popped until the delay slots run
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

    def test_type7a_modify_uses_its_m_register_and_type3a_moves_one_word(self):
        modified = self.run_one(
            T.State(
                1,
                {
                    23: T.Const(0x100),
                    1: T.Const(4),
                    2: T.Const(5),
                    T.UREG_CODES["M7"]: T.Const(1),
                },
            ),
            insn(
                "7a",
                {
                    "g": 0,
                    "cond[4:0]": 31,
                    "is[2:2]": 1,
                    "is[1:0]": 3,
                    "m[2:0]": 7,
                    "idis[2:0]": 0,
                    "compute[22:16]": 0x28,
                    "compute[15:0]": 0x8310,
                },
                6,
            ),
        )
        self.assertEqual(modified.trace[0]["action"], "i-modify")
        self.assertEqual(modified.trace[0]["source"], "I7")
        self.assertEqual(modified.uregs[23], T.Const(0x101))
        self.assertEqual(modified.uregs[3], T.Const(29))

        memory = loader_memory(loader_block(1, 0x80, 4, payload=b"\0" * 4))
        moved = self.run_one(
            T.State(
                1,
                {16: T.Const(0x80), 32: T.Const(1), 2: T.Const(0xAABBCCDD)},
                concrete=memory,
                assume_nw32=True,
            ),
            insn(
                "3a",
                {
                    "u": 1,
                    "i": 0,
                    "m": 0,
                    "cond": 31,
                    "g": 0,
                    "d": 1,
                    "l": 0,
                    "ureg": 2,
                    "compute": 0,
                },
                6,
            ),
        )
        self.assertTrue(moved.trace[0]["concrete_write"])
        self.assertEqual(T._dm_read(moved, 0x80, 4), T.Const(0xAABBCCDD))
        self.assertEqual(moved.uregs[16], T.Const(0x84))

    def test_type7a_not_sv_pex_true_modifies_even_in_simd(self):
        state = self.run_one(
            T.State(
                0x10,
                {
                    T.UREG_CODES["I12"]: T.Const(0x100),
                    T.UREG_CODES["M9"]: T.Const(1),
                    T.UREG_CODES["L12"]: T.Const(0),
                    T.UREG_CODES["ASTATX"]: T.Const(0),  # SV clear: NOT SV is true.
                    T.UREG_CODES["MODE1"]: T.Const(1 << 21),
                },
                assume_nw32=True,
            ),
            insn(
                "7a",
                {
                    "g": 1,
                    "cond[4:0]": 0x17,
                    "is[2:2]": 1,
                    "is[1:0]": 0,
                    "m[2:0]": 1,
                    "idis[2:0]": 0,
                    "compute[22:16]": 0,
                    "compute[15:0]": 0,
                },
                6,
            ),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["I12"]], T.Const(0x104))
        self.assertEqual(state.pc_sw, 0x13)
        self.assertEqual(state.steps, 1)
        self.assertEqual(state.trace[0]["action"], "i-modify")

    def test_type7a_not_sv_false_skips_only_in_known_sisd(self):
        state = self.run_one(
            T.State(
                0x10,
                {
                    T.UREG_CODES["I12"]: T.Const(0x100),
                    T.UREG_CODES["M9"]: T.Const(1),
                    T.UREG_CODES["L12"]: T.Const(0),
                    T.UREG_CODES["ASTATX"]: T.Const(1 << T.SV_BIT),
                    T.UREG_CODES["MODE1"]: T.Const(0),
                },
                assume_nw32=True,
            ),
            insn("7a", {"g": 1, "cond[4:0]": 0x17, "is[2:2]": 1,
                        "is[1:0]": 0, "m[2:0]": 1, "idis[2:0]": 0,
                        "compute[22:16]": 0, "compute[15:0]": 0}, 6),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["I12"]], T.Const(0x100))
        self.assertEqual(state.trace[0]["action"], "i-modify-skipped")
        self.assertEqual((state.pc_sw, state.steps), (0x13, 1))

    def test_type7a_not_sv_uncertain_outcomes_taint_destination_and_store(self):
        fields = {"g": 1, "cond[4:0]": 0x17, "is[2:2]": 1,
                  "is[1:0]": 0, "m[2:0]": 1, "idis[2:0]": 0,
                  "compute[22:16]": 0, "compute[15:0]": 0}
        base = {
            T.UREG_CODES["I12"]: T.Const(0x100),
            T.UREG_CODES["M9"]: T.Const(1),
            T.UREG_CODES["L12"]: T.Const(0),
        }
        for label, registers in (
            ("PEx false SIMD", {**base, T.UREG_CODES["ASTATX"]: T.Const(1 << T.SV_BIT),
                                 T.UREG_CODES["MODE1"]: T.Const(1 << 21)}),
            ("PEx false unknown MODE1", {**base, T.UREG_CODES["ASTATX"]: T.Const(1 << T.SV_BIT)}),
            ("unknown ASTATX", {**base, T.UREG_CODES["MODE1"]: T.Const(0)}),
        ):
            with self.subTest(label=label):
                state = self.run_one(T.State(0x10, registers, assume_nw32=True), insn("7a", fields, 6))
                self.assertEqual(
                    state.uregs[T.UREG_CODES["I12"]],
                    T.Unknown("conditional Type7a modify outcome"),
                )
                self.assertEqual((state.pc_sw, state.steps), (0x13, 1))
                self.assertEqual(state.trace[0]["action"], "i-modify-uncertain")
                # A downstream DM store through the conditional I12 is not definite.
                address = T._json_value(state.uregs[T.UREG_CODES["I12"]])
                W = import_module("sharcwriters")
                classification, _ = W.classify_store_address(address, 4, 0x100, None, None)
                self.assertEqual(classification, "UNRESOLVED")

    def test_type7a_not_sv_fails_closed_for_circular_or_compute_forms(self):
        fields = {"g": 1, "cond[4:0]": 0x17, "is[2:2]": 1,
                  "is[1:0]": 0, "m[2:0]": 1, "idis[2:0]": 0,
                  "compute[22:16]": 0, "compute[15:0]": 0}
        base = {T.UREG_CODES["I12"]: T.Const(0x100), T.UREG_CODES["M9"]: T.Const(1),
                T.UREG_CODES["ASTATX"]: T.Const(0), T.UREG_CODES["MODE1"]: T.Const(0)}
        for label, registers, altered, reason in (
            ("nonzero L", {**base, T.UREG_CODES["L12"]: T.Const(1)}, {}, "unsupported Type7a circular modify"),
            ("missing L", base, {}, "unsupported Type7a circular modify"),
            ("nonzero compute", {**base, T.UREG_CODES["L12"]: T.Const(0)}, {"compute[15:0]": 1}, "unsupported Type7a conditional compute"),
            ("other condition", {**base, T.UREG_CODES["L12"]: T.Const(0)}, {"cond[4:0]": 0x07}, "unsupported Type7a predicate"),
        ):
            with self.subTest(label=label):
                state = self.run_one(T.State(0x10, registers, assume_nw32=True), insn("7a", {**fields, **altered}, 6))
                self.assertEqual(state.stopped, reason)

    def test_type14a_direct_load_and_store(self):
        fields = {
            "addr[31:16]": 0x310C,
            "addr[15:0]": 0x90C0,
            "g": 0,
            "d": 1,
            "l": 0,
            "ureg[6:0]": 16,
        }
        stored = self.run_one(
            T.State(10, {16: T.Const(0x3DEF7B9C)}),
            insn("14a", fields, 6),
        )
        self.assertEqual(
            stored.trace[0],
            {
                "pc_sw": 10,
                "form": "14a",
                "action": "store",
                "space": "DM",
                "ureg": "I0",
                "value": 0x3DEF7B9C,
                "address": 0x310C90C0,
                "expression": "0x310c90c0",
                "simd_companion_possible": True,
            },
        )

        loaded = self.run_one(
            T.State(10, {16: T.Const(7)}),
            insn("14a", {**fields, "g": 1, "d": 0}, 6),
        )
        self.assertEqual(
            (loaded.trace[0]["space"], loaded.trace[0]["simd_companion_possible"]),
            ("PM", True),
        )
        self.assertEqual(loaded.trace[0]["address"], 0x310C90C0)
        self.assertEqual(loaded.uregs[16], T.Unknown("memory-address 0x310c90c0"))

        memory = loader_memory(
            loader_block(0, 0x200, 8, payload=b"\0" * 8),
        )
        long_fields = {
            **fields,
            "addr[31:16]": 0,
            "addr[15:0]": 0x200,
            "ureg[6:0]": 6,
            "l": 1,
        }
        long_word = self.run_one(
            T.State(
                10,
                {6: T.Const(0x12345678), 7: T.Const(0x9ABCDEF0)},
                concrete=memory,
                assume_nw32=True,
            ),
            insn("14a", long_fields, 6),
        )
        self.assertEqual(T._dm_read(long_word, 0x200, 4), T.Const(0x12345678))
        self.assertEqual(T._dm_read(long_word, 0x204, 4), T.Const(0x9ABCDEF0))
        self.assertEqual(long_word.trace[0]["ureg_pair"], ["R6", "R7"])
        self.assertEqual(long_word.trace[0]["access_width"], "long-word")

        long_word.pc_sw = 10
        loaded_pair = self.run_one(
            long_word,
            insn("14a", {**long_fields, "d": 0, "ureg[6:0]": 10}, 6),
        )
        self.assertEqual(loaded_pair.uregs[10], T.Const(0x12345678))
        self.assertEqual(loaded_pair.uregs[11], T.Const(0x9ABCDEF0))

        odd_pair = self.run_one(
            T.State(10),
            insn("14a", {**long_fields, "ureg[6:0]": 7}, 6),
        )
        self.assertEqual(odd_pair.stopped, "unsupported Type14a odd UREG pair")

    def test_type7d_aconv_matches_prm_worked_example(self):
        # out/refs/sharc-plus-prm p.351 ACONV Example: "I0 = B2W(I2);" and
        # "IF AV B4 = W2B(B1);" -- is is the source register number within
        # its class; the destination is is XOR idis, as in Type7a/Type19a.
        b2w = self.run_one(
            T.State(1, {T.UREG_CODES["I2"]: T.Const(0x400)}),
            insn(
                "7d",
                {
                    "g": 0,
                    "is[2:2]": 0,
                    "is[1:0]": 2,
                    "breg": 0,
                    "toby": 0,
                    "idis[2:0]": 2,
                },
                6,
            ),
        )
        self.assertEqual(b2w.uregs[T.UREG_CODES["I0"]], T.Const(0x100))
        self.assertEqual(
            b2w.trace[0],
            {
                "pc_sw": 1,
                "form": "7d",
                "action": "aconv",
                "direction": "b2w",
                "source": "I2",
                "destination": "I0",
                "value": 0x100,
                "semantics": "prm-likely",
            },
        )

        w2b = self.run_one(
            T.State(1, {T.UREG_CODES["B1"]: T.Const(0x40)}),
            insn(
                "7d",
                {
                    "g": 0,
                    "is[2:2]": 0,
                    "is[1:0]": 1,
                    "breg": 1,
                    "toby": 1,
                    "idis[2:0]": 5,
                },
                6,
            ),
        )
        self.assertEqual(w2b.uregs[T.UREG_CODES["B4"]], T.Const(0x100))
        self.assertEqual(w2b.trace[0]["direction"], "w2b")
        self.assertEqual(w2b.trace[0]["source"], "B1")
        self.assertEqual(w2b.trace[0]["destination"], "B4")

    def test_type7d_exact_bytes_propagate_symbolic_addresses_conservatively(self):
        # Hand-built pure-ACONV byte fixtures. The first is I7 = B2W(I7);
        # the second exercises g=1, B class, source B9 and XOR destination
        # B12: B12 = W2B(B9). They are decoder fixtures, not firmware claims.
        b2w = T.decode_at(bytes.fromhex("bf0480c00000"), 0, 0)
        w2b = T.decode_at(bytes.fromhex("fe04805d0000"), 0, 0)
        compute_bearing = T.decode_at(bytes.fromhex("bf0480c00100"), 0, 0)
        self.assertEqual(
            (b2w.raw, b2w.type_name, b2w.fields),
            (0x04BFC0800000, "7d", {
                "g": 0, "is[2:2]": 1, "is[1:0]": 3, "breg": 0,
                "toby": 0, "idis[2:0]": 0,
            }),
        )
        self.assertEqual(
            (w2b.raw, w2b.type_name, w2b.fields),
            (0x04FE5D800000, "7d", {
                "g": 1, "is[2:2]": 0, "is[1:0]": 1, "breg": 1,
                "toby": 1, "idis[2:0]": 5,
            }),
        )
        # PIN_FIELDS admits only pure ACONV here; PRM optional-compute Type7d
        # rows must not silently execute through this bounded handler.
        self.assertNotEqual(compute_bearing.type_name, "7d")

        # A divisible affine B2W has an exact affine result.  An unaligned
        # affine source instead becomes an opaque symbol: B2W is not linear
        # over unknown low bits and the address map may raise ILAD.
        exact = self.run_one(
            T.State(1, {T.UREG_CODES["I7"]: T.Affine(0x20, (("byte_base", 4),))}),
            b2w,
        )
        self.assertEqual(
            exact.uregs[T.UREG_CODES["I7"]], T.Affine(8, (("byte_base", 1),)))
        symbolic = self.run_one(
            T.State(1, {T.UREG_CODES["I7"]: T.symbol("B7e")}), b2w
        )
        self.assertEqual(
            symbolic.uregs[T.UREG_CODES["I7"]],
            T.symbol("aconv_b2w_23_1_0_B7e_1"),
        )
        self.assertEqual(symbolic.trace[0]["semantics"], "prm-likely")

        banked = self.run_one(
            T.State(1, {T.UREG_CODES["B9"]: T.Affine(3, (("word_base", 1),))}),
            w2b,
        )
        self.assertEqual(
            banked.uregs[T.UREG_CODES["B12"]], T.Affine(12, (("word_base", 4),)))
        self.assertEqual(banked.trace[0]["direction"], "w2b")

    def test_type7d_firmware_instance_stops_without_a_concrete_source(self):
        # Unknown remains a hard stop: neither address-map conversion nor its
        # ILAD outcome is modeled by the bounded tracer.
        fields = {
            "g": 0, "is[2:2]": 1, "is[1:0]": 3, "breg": 0,
            "toby": 0, "idis[2:0]": 0,
        }
        stopped = self.run_one(T.State(1), insn("7d", fields, 6))
        self.assertEqual(stopped.stopped, "Type7d B2W(I7) source is not concrete")

    def test_type14d_short_word_store_and_zero_extended_load(self):
        # out/refs/sharc-plus-prm pp.384-386: w=0,ex=0,l=1 is (sw)/(sw) BH
        # (store) / BHSE with x=0 (zero-extend load). Firmware instances
        # 0x1c8119 (store) and 0x1c811c (load) share DM 0x269454.
        fields = {
            "ex": 0,
            "l": 1,
            "w": 0,
            "x": 0,
            "dreg[3:0]": 2,
            "addr[31:16]": 0x26,
            "addr[15:0]": 0x9454,
        }
        memory = loader_memory(loader_block(1, 0x269454, 4, payload=b"\0" * 4))
        stored = self.run_one(
            T.State(1, {2: T.Const(0x1234ABCD)}, concrete=memory),
            insn("14d", {**fields, "d": 1}, 6),
        )
        self.assertEqual(stored.trace[0]["access_width"], "short-word")
        self.assertTrue(stored.trace[0]["concrete_write"])
        self.assertEqual(T._dm_read(stored, 0x269454, 2), T.Const(0xABCD))

        memory = loader_memory(
            loader_block(1, 0x269454, 2, payload=struct.pack("<H", 0xBEEF))
        )
        loaded = self.run_one(
            T.State(1, concrete=memory), insn("14d", {**fields, "d": 0}, 6)
        )
        self.assertEqual(loaded.uregs[2], T.Const(0xBEEF))
        self.assertEqual(loaded.trace[0]["access_width"], "short-word")

    def test_type14d_byte_sign_and_zero_extended_loads(self):
        # Firmware instance 0xb8cdaf: w=0,ex=0,l=0,x=1 is (bwse), a
        # sign-extended byte load (p.386 BHSE Encode Table).
        fields = {
            "ex": 0,
            "l": 0,
            "w": 0,
            "d": 0,
            "dreg[3:0]": 2,
            "addr[31:16]": 0x2D,
            "addr[15:0]": 0x722C,
        }
        memory = loader_memory(loader_block(1, 0x2D722C, 1, payload=bytes([0x80])))
        signed = self.run_one(
            T.State(1, concrete=memory), insn("14d", {**fields, "x": 1}, 6)
        )
        self.assertEqual(signed.uregs[2], T.Const(0xFFFFFF80))
        self.assertEqual(signed.trace[0]["access_width"], "byte-sign-extended")

        memory = loader_memory(loader_block(1, 0x2D722C, 1, payload=bytes([0x80])))
        unsigned = self.run_one(
            T.State(1, concrete=memory), insn("14d", {**fields, "x": 0}, 6)
        )
        self.assertEqual(unsigned.uregs[2], T.Const(0x80))
        self.assertEqual(unsigned.trace[0]["access_width"], "byte")

    def test_type14d_stops_on_exclusive_access_and_undocumented_encodings(self):
        base = {
            "d": 0,
            "l": 0,
            "x": 0,
            "dreg[3:0]": 0,
            "addr[31:16]": 0,
            "addr[15:0]": 0,
        }
        exclusive = self.run_one(T.State(1), insn("14d", {**base, "ex": 1, "w": 1}, 6))
        self.assertEqual(exclusive.stopped, "unsupported Type14d exclusive access")

        undocumented_w = self.run_one(
            T.State(1), insn("14d", {**base, "ex": 0, "w": 1}, 6)
        )
        self.assertEqual(
            undocumented_w.stopped,
            "undocumented Type14d encoding (w=1, ex=0)",
        )

        undocumented_store_x = self.run_one(
            T.State(1),
            insn("14d", {**base, "ex": 0, "w": 0, "d": 1, "x": 1}, 6),
        )
        self.assertEqual(
            undocumented_store_x.stopped,
            "undocumented Type14d store encoding (x=1)",
        )

    def test_type15a_symbolic_load_leaves_the_i_register_unmodified(self):
        # out/refs/sharc-plus-prm p.388: "The I register is pre-modified
        # with an immediate value ... The I register is not updated,"
        # unlike Type19a's post-modify MODIFY.
        loaded = self.run_one(
            T.State(1, {16: T.symbol("buf")}),
            insn(
                "15a",
                {
                    "g": 0,
                    "i[2:0]": 0,
                    "d": 0,
                    "l": 0,
                    "ureg[6:0]": 7,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x18,
                },
                6,
            ),
        )
        self.assertEqual(loaded.trace[0]["expression"], "buf + 0x18")
        self.assertEqual(loaded.uregs[7], T.Unknown("memory-address buf + 0x18"))
        self.assertEqual(loaded.uregs[16], T.symbol("buf"))

    def test_type15a_dm_store_and_pm_dag2_load(self):
        # Under --assume-32bit-normal-words the <data32> displacement is in
        # byte-addressed-space normal-word units, so it is scaled by 4 (out/
        # refs/sharc-plus-prm Table 6-2, p.6-10/6-11) -- the same scaling
        # Type4a/4b/15b already apply to their own immediate modifiers. The
        # store lands at I0 + 0x1000*4 = 0x6000, not I0 + 0x1000.
        memory = loader_memory(loader_block(1, 0x6000, 4, payload=b"\0" * 4))
        stored = self.run_one(
            T.State(
                1,
                {16: T.Const(0x2000), 5: T.Const(0xCAFEBABE)},
                concrete=memory,
                assume_nw32=True,
            ),
            insn(
                "15a",
                {
                    "g": 0,
                    "i[2:0]": 0,
                    "d": 1,
                    "l": 0,
                    "ureg[6:0]": 5,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x1000,
                },
                6,
            ),
        )
        self.assertTrue(stored.trace[0]["concrete_write"])
        self.assertEqual(T._dm_read(stored, 0x6000, 4), T.Const(0xCAFEBABE))
        # Pre-modify only: I0 keeps its value.
        self.assertEqual(stored.uregs[16], T.Const(0x2000))

        loaded_pm = self.run_one(
            T.State(1, {T.UREG_CODES["I11"]: T.Const(5)}),
            insn(
                "15a",
                {
                    "g": 1,
                    "i[2:0]": 3,
                    "d": 0,
                    "l": 0,
                    "ureg[6:0]": 0,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x10,
                },
                6,
            ),
        )
        self.assertEqual(loaded_pm.trace[0]["space"], "PM")
        self.assertIsInstance(loaded_pm.uregs[0], T.Unknown)

    def test_type15a_forced_long_word_register_pair(self):
        # Scaled the same way as the plain-register case above: I0 + 0x100*4
        # = 0x700, not I0 + 0x100. The (lw) pair still steps its second word
        # by a fixed 4 bytes (out/refs/sharc-plus-prm p.6-15's (lw) row scales
        # the same as an unqualified access, not by 8), independent of this
        # displacement scaling.
        memory = loader_memory(loader_block(1, 0x700, 8, payload=b"\0" * 8))
        long_word = self.run_one(
            T.State(
                1,
                {16: T.Const(0x300), 6: T.Const(0x11111111), 7: T.Const(0x22222222)},
                concrete=memory,
                assume_nw32=True,
            ),
            insn(
                "15a",
                {
                    "g": 0,
                    "i[2:0]": 0,
                    "d": 1,
                    "l": 1,
                    "ureg[6:0]": 6,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x100,
                },
                6,
            ),
        )
        self.assertEqual(T._dm_read(long_word, 0x700, 4), T.Const(0x11111111))
        self.assertEqual(T._dm_read(long_word, 0x704, 4), T.Const(0x22222222))
        self.assertEqual(long_word.trace[0]["access_width"], "long-word")
        self.assertEqual(long_word.trace[0]["simd_companion_possible"], False)

        long_word.pc_sw = 1
        loaded_pair = self.run_one(
            long_word,
            insn(
                "15a",
                {
                    "g": 0,
                    "i[2:0]": 0,
                    "d": 0,
                    "l": 1,
                    "ureg[6:0]": 10,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x100,
                },
                6,
            ),
        )
        self.assertEqual(loaded_pair.uregs[10], T.Const(0x11111111))
        self.assertEqual(loaded_pair.uregs[11], T.Const(0x22222222))

        odd_pair = self.run_one(
            T.State(1, {16: T.Const(0x300)}),
            insn(
                "15a",
                {
                    "g": 0,
                    "i[2:0]": 0,
                    "d": 0,
                    "l": 1,
                    "ureg[6:0]": 7,
                    "addr[31:16]": 0,
                    "addr[15:0]": 0x100,
                },
                6,
            ),
        )
        self.assertEqual(odd_pair.stopped, "unsupported Type15a odd UREG pair")

    def test_type4a_store_and_type15a_load_of_the_same_normal_word_slot(self):
        """A Type4a store and a Type15a load of the same architectural
        word offset from the same I register must hit the same byte
        address once --assume-32bit-normal-words is in effect.

        This reproduces the DT2 1.16 wavetable stage-6 kernel's own local-
        variable re-read: Type4a stores DM(I6-4)=R0 at sw 0x1cbf72 (a 6-bit
        signed word offset of -4), and Type15a later loads
        R12=DM(0xfffffffc,I6) at sw 0x1cbff1 (a 32-bit displacement that
        also decodes to -4). Per out/refs/sharc-plus-prm's "Enhanced Modify
        Instruction for Address Scaling" (pp.6-9-6-10) and Table 6-2
        (p.6-10/6-11), both are normal-word displacements into byte-
        addressed space and both must be scaled by 4 -- so both
        instructions must resolve to the same byte address, I6-16.
        """
        i6 = T.UREG_CODES["I6"]
        state = T.State(
            1,
            {i6: T.Const(0x90001000), 0: T.Const(0xCAFEBABE)},
            concrete=loader_memory(),
            assume_nw32=True,
        )
        store_fields = {
            "i[2:0]": 6,
            "g": 0,
            "d": 1,
            "cond[4:0]": 0x1F,
            "data[5:5]": 1,
            "data[4:0]": 0x1C,  # 6-bit signed -4
            "dreg[3:0]": 0,
            "u": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        stored = self.run_one(state, insn("4a", store_fields, 6))
        self.assertEqual(stored.trace[0]["address"], 0x90001000 - 16)
        self.assertTrue(stored.trace[0]["concrete_write"])
        # I6 is unchanged: the frame pointer stays valid for the later load.
        self.assertEqual(stored.uregs[i6], T.Const(0x90001000))

        load_fields = {
            "g": 0,
            "i[2:0]": 6,
            "d": 0,
            "l": 0,
            "ureg[6:0]": 12,
            "addr[31:16]": 0xFFFF,
            "addr[15:0]": 0xFFFC,  # 32-bit displacement, also decodes to -4
        }
        loaded = self.run_one(stored, insn("15a", load_fields, 6))
        self.assertEqual(loaded.trace[0]["address"], 0x90001000 - 16)
        self.assertEqual(loaded.uregs[12], T.Const(0xCAFEBABE))

    def test_type4a_store_and_type15b_load_of_the_same_normal_word_slot(self):
        """Type15b's own (non-lw) immediate modifier was already scaled the
        same way as Type4a's (tools/sharc_trace.py's existing assume_nw32
        handling for name == "15b"); this guards that the two stay in
        agreement now that Type15a's data32 is scaled too."""
        i6 = T.UREG_CODES["I6"]
        state = T.State(
            1,
            {i6: T.Const(0x2000), 3: T.Const(0x11223344)},
            concrete=loader_memory(),
            assume_nw32=True,
        )
        store_fields = {
            "i[2:0]": 6,
            "g": 0,
            "d": 1,
            "cond[4:0]": 0x1F,
            "data[5:5]": 0,
            "data[4:0]": 5,  # +5 words
            "dreg[3:0]": 3,
            "u": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        stored = self.run_one(state, insn("4a", store_fields, 6))
        self.assertEqual(stored.trace[0]["address"], 0x2000 + 20)

        load_fields = {
            "i[2:0]": 6,
            "g": 0,
            "d": 0,
            "l": 0,
            "ureg[6:0]": 4,
            "data[6:0]": 5,  # same +5 word offset, Type15b's own field name
        }
        loaded = self.run_one(stored, insn("15b", load_fields, 4))
        self.assertEqual(loaded.trace[0]["address"], 0x2000 + 20)
        self.assertEqual(loaded.uregs[4], T.Const(0x11223344))

    def test_pm_normal_word_load_into_px_splits_loader_backed_48_bits(self):
        address = T.L1_BLOCK3_NW_BASE + 0x20
        byte_address = L.sw_to_byte(T.L1_BLOCK3_SW_BASE) + 6 * 0x20
        payload = struct.pack("<3H", 0x1234, 0x5678, 0x9ABC)
        memory = loader_memory(
            loader_block(1, byte_address, len(payload), payload=payload)
        )
        fields = {
            "addr[31:16]": address >> 16,
            "addr[15:0]": address & 0xFFFF,
            "g": 1,
            "d": 0,
            "l": 0,
            "ureg[6:0]": T.UREG_CODES["PX"],
        }
        loaded = self.run_one(T.State(10, concrete=memory), insn("14a", fields, 6))
        self.assertEqual(loaded.uregs[T.UREG_CODES["PX1"]], T.Const(0x9ABC0000))
        self.assertEqual(loaded.uregs[T.UREG_CODES["PX2"]], T.Const(0x12345678))
        self.assertIsInstance(loaded.uregs[T.UREG_CODES["PX"]], T.Unknown)
        self.assertEqual(
            loaded.trace[-1]["concrete_value"],
            {"PX1": 0x9ABC0000, "PX2": 0x12345678},
        )

        missing = self.run_one(
            T.State(
                10,
                {
                    T.UREG_CODES["PX"]: T.Const(1),
                    T.UREG_CODES["PX1"]: T.Const(2),
                    T.UREG_CODES["PX2"]: T.Const(3),
                },
                concrete=memory,
            ),
            insn(
                "14a",
                {**fields, "addr[15:0]": (address + 1) & 0xFFFF},
                6,
            ),
        )
        self.assertIsInstance(missing.uregs[T.UREG_CODES["PX"]], T.Unknown)
        self.assertIsInstance(missing.uregs[T.UREG_CODES["PX1"]], T.Unknown)
        self.assertIsInstance(missing.uregs[T.UREG_CODES["PX2"]], T.Unknown)

        dm_loaded = self.run_one(
            T.State(10, concrete=memory),
            insn("14a", {**fields, "g": 0}, 6),
        )
        self.assertEqual(dm_loaded.uregs[T.UREG_CODES["PX1"]], T.Const(0x9ABC0000))
        self.assertEqual(dm_loaded.uregs[T.UREG_CODES["PX2"]], T.Const(0x12345678))

    def test_type3b_pm_px_load_uses_normal_word_alias_and_post_modifies(self):
        address = T.L1_BLOCK3_NW_BASE + 0x21
        byte_address = L.sw_to_byte(T.L1_BLOCK3_SW_BASE) + 6 * 0x21
        payload = struct.pack("<3H", 0xABCD, 0x0123, 0x4567)
        memory = loader_memory(
            loader_block(1, byte_address, len(payload), payload=payload)
        )
        fields = {
            "u": 1,
            "i[2:0]": 0,
            "m[2:0]": 6,
            "cond[4:0]": 31,
            "g": 1,
            "d": 0,
            "l": 0,
            "ureg[6:0]": T.UREG_CODES["PX"],
            "w": 1,
            "x": 1,
        }
        state = T.State(
            10,
            {24: T.Const(address), 46: T.Const(1)},
            concrete=memory,
        )
        loaded = self.run_one(state, insn("3b", fields))
        self.assertEqual(loaded.uregs[24], T.Const(address + 1))
        self.assertEqual(loaded.uregs[T.UREG_CODES["PX1"]], T.Const(0x45670000))
        self.assertEqual(loaded.uregs[T.UREG_CODES["PX2"]], T.Const(0xABCD0123))
        self.assertEqual(loaded.trace[-1]["access_width"], "normal-word")

    def test_type25_negative_pcrel_target_wraps_24bit_current_sw_address(self):
        call = insn(
            "25a_pcrel",
            {"reladdr[23:16]": 0x80, "reladdr[15:0]": 0},
            6,
        )
        state = self.run_one(T.State(0x100), call)
        state = self.run_one(state, insn("21c", {}, length=2))
        state = self.run_one(state, insn("21c", {}, length=2))
        self.assertEqual(state.stopped, "external-call")
        self.assertEqual(state.trace[-1]["target_sw"], 0x800100)

        dt2_116 = insn(
            "25a_pcrel",
            {"reladdr[23:16]": 0x9C, "reladdr[15:0]": 0x843D},
            6,
        )
        state = self.run_one(T.State(0x1C0FA5), dt2_116)
        state = self.run_one(state, insn("21c", {}, length=2))
        state = self.run_one(state, insn("21c", {}, length=2))
        self.assertEqual(state.trace[-1]["target_sw"], 0xB893E2)

    def test_ureg_copy_and_compute_rejection(self):
        f = {
            "srcureghigh[4:0]": 4,
            "srcureglow[1:1]": 1,
            "srcureglow[0:0]": 0,
            "dstureg[6:0]": 3,
            "cond[4:0]": 31,
        }
        s = self.run_one(T.State(1, {18: T.Const(9)}), insn("5b_move", f))
        self.assertEqual(s.uregs[3], T.Const(9))
        f.update({"compute[22:16]": 1, "compute[15:0]": 0})
        self.assertIn(
            "unsupported full compute",
            self.run_one(T.State(1), insn("5a_move", f, 6)).stopped,
        )

    def test_ureg_move_unknown_predicate_forks_copy_and_skip(self):
        fields = {
            "cond[4:0]": 1,
            "srcureghigh[4:0]": 0,
            "srcureglow[1:1]": 0,
            "srcureglow[0:0]": 1,
            "dstureg[6:0]": 2,
        }
        executed, skipped = T._execute(
            T.State(10, {1: T.Const(0x1234), 2: T.Const(7)}),
            insn("5b_move", fields),
        )
        self.assertEqual(executed.uregs[2], T.Const(0x1234))
        self.assertEqual(skipped.uregs[2], T.Const(7))
        self.assertEqual(executed.trace[-1]["predicate_assumption"], True)
        self.assertEqual(skipped.trace[-1]["predicate_assumption"], False)

    def test_computes_and_old_value_parallel_move(self):
        short = lambda opcode, rn, rx: {"compute[11:0]": (opcode << 8) | (rn << 4) | rx}
        s = self.run_one(T.State(1, {3: T.Const(9)}), insn("2c", short(2, 1, 3), 2))
        self.assertEqual(s.uregs[1], T.Const(9))
        full = lambda cu, op, rn, rx, ry: {
            "compute[22:16]": ((cu << 4) | (op >> 4)),
            "compute[15:0]": ((op & 15) << 12) | (rn << 8) | (rx << 4) | ry,
        }
        s = self.run_one(
            T.State(1, {1: T.Const(99), 4: T.Const(7)}),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 1,
                    "dstureg[6:0]": 4,
                    "cond[4:0]": 31,
                    **full(0, 0x02, 3, 4, 4),
                },
                6,
            ),
        )
        self.assertEqual((s.uregs[3], s.uregs[4]), (T.Const(0), T.Const(99)))
        for opcode, expected, operation in (
            (0x40, 0x0A00, "and"),
            (0x41, 0xAFAF, "or"),
            (0x42, 0xA5AF, "xor"),
        ):
            s = self.run_one(
                T.State(1, {1: T.Const(0x0F0F), 2: T.Const(0xAAA0)}),
                insn("2a", {"cond[4:0]": 31, **full(0, opcode, 3, 1, 2)}, 6),
            )
            self.assertEqual(s.uregs[3], T.Const(expected))
            self.assertEqual(s.trace[-1]["operation"], operation)
        s = self.run_one(
            T.State(1, {5: T.Const(11)}),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 6,
                    "cond[4:0]": 31,
                    **full(0, 0x21, 6, 5, 0),
                },
                6,
            ),
        )
        self.assertEqual(s.uregs[6], T.Const(11))
        s = self.run_one(
            T.State(1, {1: T.Const(6), 2: T.Const(7)}),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 3,
                    "cond[4:0]": 31,
                    **full(1, 0x70, 2, 1, 2),
                },
                6,
            ),
        )
        self.assertEqual(s.uregs[2], T.Const(42))
        self.assertEqual(s.trace[0]["action"], "compute")

        s = self.run_one(
            T.State(
                1,
                {
                    0: T.Const(99),
                    2: T.Const(4),
                    12: T.Const(4),
                    T.UREG_CODES["ASTATX"]: T.Const(1),
                },
            ),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 3,
                    "cond[4:0]": 31,
                    **full(0, 0x0A, 0, 12, 2),
                },
                6,
            ),
        )
        self.assertEqual(s.uregs[0], T.Const(99))
        self.assertEqual(s.trace[0]["operation"], "compare")
        self.assertTrue(s.trace[0]["status_only"])
        # comp(R12=4, R2=4) with ASTATX=1: AZ set, AN and CACC MSB clear.
        self.assertEqual(s.uregs[T.UREG_CODES["ASTATX"]], T.Const(1))

        s = self.run_one(
            T.State(1, {0: T.Const(0x10), 2: T.Const(4)}),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 2,
                    "cond[4:0]": 31,
                    **full(2, 0xCC, 0, 0, 2),
                },
                6,
            ),
        )
        self.assertEqual(s.uregs[0], T.Const(0x10))
        self.assertEqual(s.uregs[2], T.Const(0x10))
        self.assertEqual(
            (s.trace[0]["operation"], s.trace[0]["status_only"]),
            ("bit-test", True),
        )

        s = self.run_one(
            T.State(1, {1: T.Const(0x10), 2: T.Const(4)}),
            insn(
                "5a_move",
                {
                    "srcureghigh[4:0]": 0,
                    "srcureglow[1:1]": 0,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 3,
                    "cond[4:0]": 31,
                    **full(2, 0xC8, 0, 1, 2),
                },
                6,
            ),
        )
        self.assertEqual(s.uregs[0], T.Const(0))
        self.assertEqual(s.trace[0]["operation"], "bit-toggle")

        s = self.run_one(
            T.State(1, {1: T.Const(0x10), 2: T.Const(32)}),
            insn("2a", {"cond[4:0]": 31, **full(2, 0xC8, 0, 1, 2)}, 6),
        )
        self.assertEqual(s.uregs[0], T.Const(0x10))

        s = self.run_one(
            T.State(1, {1: T.Const(0x80000001), 2: T.Const(4)}),
            insn("2a", {"cond[4:0]": 31, **full(2, 0x00, 0, 1, 2)}, 6),
        )
        self.assertEqual(s.uregs[0], T.Const(0x10))
        self.assertEqual(s.trace[0]["operation"], "logical-shift")

        for opcode, expected, operation in (
            (0xC0, 0x80000001, "bit-set"),
            (0xC4, 0x00000001, "bit-clear"),
        ):
            s = self.run_one(
                T.State(1, {1: T.Const(0x80000001), 2: T.Const(31)}),
                insn("2a", {"cond[4:0]": 31, **full(2, opcode, 0, 1, 2)}, 6),
            )
            self.assertEqual(s.uregs[0], T.Const(expected))
            self.assertEqual(s.trace[-1]["operation"], operation)

        s = self.run_one(
            T.State(1, {1: T.Const(0x80000000), 2: T.Const(0xFFFFFFFC)}),
            insn("2a", {"cond[4:0]": 31, **full(2, 0x00, 0, 1, 2)}, 6),
        )
        self.assertEqual(s.uregs[0], T.Const(0x08000000))

    def test_compute_unknown_and_unsupported_do_not_mutate(self):
        s = self.run_one(T.State(1), insn("2c", {"compute[11:0]": 0x251}, 2))
        self.assertIsInstance(s.uregs[5], T.Unknown)
        # ShortCompute's opcode field (PRM Table 18-2, p.423-425) is now
        # fully enumerated across all 16 values (0-7/0xC-0xE fixed-point,
        # 8/9/0xA/0xB/0xF float), so "unsupported short compute" can no
        # longer be reached through a 2c instruction; exercise the same
        # do-not-mutate-on-stop guarantee through a still-unsupported full
        # compute instead (cu=11, reserved -- PRM Table 18-1 p.423).
        s = self.run_one(
            T.State(10, {1: T.Const(2)}),
            insn(
                "2a",
                {"cond[4:0]": 0x1F, "compute[22:16]": 0x30, "compute[15:0]": 0},
                6,
            ),
        )
        self.assertIn("unsupported full compute", s.stopped)
        self.assertEqual(s.uregs, {1: T.Const(2)})

    def test_type6a_shift_store_and_post_modify_use_old_values(self):
        address = 0x1000
        fields = {
            "cond[4:0]": 31,
            "g": 0,
            "i[2:0]": 5,
            "m[2:0]": 5,
            "d": 1,
            "dreg[3:0]": 9,
            "dataex[3:0]": 0,
            # R2 = BCLR R2 BY 11
            "shiftimm[22:16]": 0x31,
            "shiftimm[15:0]": 0x0B22,
        }
        state = T.State(
            1,
            {
                T.UREG_CODES["R2"]: T.Const(0xFFFF),
                T.UREG_CODES["R9"]: T.Const(0x12345678),
                T.UREG_CODES["I5"]: T.Const(address),
                T.UREG_CODES["M5"]: T.Const(4),
            },
        )
        state = self.run_one(state, insn("6a_mem", fields, 6))
        self.assertEqual(state.uregs[T.UREG_CODES["R2"]], T.Const(0xF7FF))
        self.assertEqual(state.uregs[T.UREG_CODES["I5"]], T.Const(address + 4))
        self.assertEqual(
            (
                state.trace[0]["action"],
                state.trace[0]["address"],
                state.trace[0]["value"],
            ),
            ("store", address, 0x12345678),
        )
        self.assertEqual(state.trace[1]["operation"], "bit-clear-immediate")

        toggled = self.run_one(
            T.State(1, {T.UREG_CODES["R1"]: T.Const(1)}),
            insn(
                "6b_shiftimm",
                {
                    "cond[4:0]": 31,
                    "dataex[3:0]": 0,
                    "shiftimm[22:16]": 0x32,
                    "shiftimm[15:0]": 0x1F01,
                },
                6,
            ),
        )
        self.assertEqual(toggled.uregs[T.UREG_CODES["R0"]], T.Const(0x80000001))
        self.assertEqual(toggled.trace[-1]["operation"], "bit-toggle-immediate")

    def test_type2a_short_executes_unconditionally(self):
        state = T.State(1, {T.UREG_CODES["R8"]: T.Const(0x00FFFFFF)})
        results = T._execute(
            state,
            insn("2a_short", {"compute[22:16]": 0x28, "compute[15:0]": 0x8280}),
        )
        self.assertEqual(len(results), 1)
        advanced = results[0]
        self.assertEqual(advanced.pc_sw, 3)
        self.assertEqual(advanced.uregs[T.UREG_CODES["R2"]], T.Const(8))
        self.assertEqual(advanced.trace[-1]["operation"], "leftz")

    def test_full_compute_compu_is_status_only(self):
        state = self.run_one(
            T.State(
                1,
                {T.UREG_CODES["R4"]: T.Const(5), T.UREG_CODES["R2"]: T.Const(7)},
            ),
            insn("2a_short", {"compute[22:16]": 0x00, "compute[15:0]": 0xB042}),
        )
        self.assertEqual(state.trace[-1]["operation"], "compare")
        self.assertTrue(state.trace[-1]["status_only"])
        # Compare's own bits (AC/AI/AS/AV/AF clear, AZ/AN from the operands)
        # become known even starting from a wholly-uninitialized ASTATX; only
        # CACC (not modelled from an unknown starting shift register) stays
        # unknown.
        astatx = state.uregs[T.UREG_CODES["ASTATX"]]
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertIsNone(T._astatx_known_bit(astatx, 24))

    def test_type6b_or_shift_immediate_ors_into_destination(self):
        cases = (
            ("or-lshift-pos", 0x08, 3, 5, 4, 0x1, 0x1, 0x11),
            ("or-lshift-neg", 0x08, 2, 4, 0xF8, 0x1, 0xFF00, 0xFF),
            ("or-ashift-neg", 0x09, 6, 7, 0xFC, 0x0, 0x80000000, 0xF8000000),
        )
        for label, opcode, rn, rx, data8, rn_init, rx_init, expected in cases:
            with self.subTest(label=label):
                fields = {
                    "cond[4:0]": 31,
                    "dataex[3:0]": 0,
                    "shiftimm[22:16]": opcode,
                    "shiftimm[15:0]": (data8 << 8) | (rn << 4) | rx,
                }
                state = T.State(1, {rn: T.Const(rn_init), rx: T.Const(rx_init)})
                advanced = self.run_one(state, insn("6b_shiftimm", fields, 6))
                self.assertEqual(advanced.uregs[rn], T.Const(expected))

    def test_full_compute_compare_sets_flags_and_shifts_cacc(self):
        astatx = T.UREG_CODES["ASTATX"]
        compu_r4_r2 = {"compute[22:16]": 0x00, "compute[15:0]": 0xB042}
        state = self.run_one(
            T.State(
                1,
                {
                    T.UREG_CODES["R4"]: T.Const(5),
                    T.UREG_CODES["R2"]: T.Const(7),
                    astatx: T.Const((1 << 18) | 0x3A),
                },
            ),
            insn("2a_short", compu_r4_r2),
        )
        self.assertEqual(state.uregs[astatx], T.Const((1 << 18) | (1 << 2)))
        comp_r1_r2 = {"compute[22:16]": 0x00, "compute[15:0]": 0xA012}
        equal = self.run_one(
            T.State(
                1,
                {
                    T.UREG_CODES["R1"]: T.Const(3),
                    T.UREG_CODES["R2"]: T.Const(3),
                    astatx: T.Const(0xFF000000),
                },
            ),
            insn("2a_short", comp_r1_r2),
        )
        self.assertEqual(equal.uregs[astatx], T.Const(0x7F000001))

    def test_full_compute_compare_signed_vs_unsigned(self):
        astatx = T.UREG_CODES["ASTATX"]
        regs = {
            T.UREG_CODES["R1"]: T.Const(0xFFFFFFFF),
            T.UREG_CODES["R2"]: T.Const(1),
            astatx: T.Const(0),
        }
        signed = self.run_one(
            T.State(1, dict(regs)),
            insn("2a_short", {"compute[22:16]": 0x00, "compute[15:0]": 0xA012}),
        )
        self.assertEqual(signed.uregs[astatx], T.Const(1 << 2))
        unsigned = self.run_one(
            T.State(1, dict(regs)),
            insn("2a_short", {"compute[22:16]": 0x00, "compute[15:0]": 0xB012}),
        )
        self.assertEqual(unsigned.uregs[astatx], T.Const(0x80000000))

    def test_short_compute_compare_sets_flags(self):
        fields = {"compute[11:0]": (3 << 8) | (1 << 4) | 2}
        state = self.run_one(
            T.State(
                1, {1: T.Const(3), 2: T.Const(5), T.UREG_CODES["ASTATX"]: T.Const(0)}
            ),
            insn("2c", fields, 2),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["ASTATX"]], T.Const(1 << 2))

    def test_concrete_compare_resolves_eq_branch(self):
        equal = self.run_one(
            T.State(
                1,
                {
                    T.UREG_CODES["R1"]: T.Const(1),
                    T.UREG_CODES["R2"]: T.Const(1),
                    T.UREG_CODES["ASTATX"]: T.Const(0),
                    T.UREG_CODES["MODE1"]: T.Const(0),
                },
            ),
            insn("2a_short", {"compute[22:16]": 0x00, "compute[15:0]": 0xA012}),
        )
        self.assertEqual(equal.uregs[T.UREG_CODES["ASTATX"]], T.Const(1))
        branch = insn(
            "8a_rel",
            {
                "b": 0,
                "j": 0,
                "cond[4:0]": 0x00,
                "reladdr[23:16]": 0,
                "reladdr[15:0]": 30,
            },
            6,
        )
        results = T._execute(equal, branch)
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0].pc_sw, 33)
        unresolved = T.State(1, {T.UREG_CODES["MODE1"]: T.Const(0)})
        self.assertEqual(len(T._execute(unresolved, branch)), 2)

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

    def test_type9a_abs_rejects_la_and_ci_modifiers(self):
        stopped = self.run_one(
            T.State(10), insn("9a_abs", self._type9a_abs_fields(a=1), 6)
        )
        self.assertEqual(stopped.stopped, "unsupported Type9a control modifier")

    def test_type9a_abs_stops_on_unknown_indirect_target(self):
        stopped = self.run_one(
            T.State(10), insn("9a_abs", self._type9a_abs_fields(), 6)
        )
        self.assertEqual(
            stopped.stopped, "unknown 9a_abs indirect target through I12/M13"
        )

    def test_type9a_abs_immediate_jump_with_known_registers(self):
        state = T.State(
            10, {T.UREG_CODES["I12"]: T.Const(0x2000), T.UREG_CODES["M13"]: T.Const(4)}
        )
        advanced = self.run_one(state, insn("9a_abs", self._type9a_abs_fields(), 6))
        self.assertEqual(advanced.pc_sw, 0x2004)
        self.assertIsNone(advanced.pending)

    def test_type9a_abs_delayed_jump_with_compute(self):
        fields = self._type9a_abs_fields(
            **{"pmm[2:0]": 7, "j": 1, "compute[22:16]": 0x02, "compute[15:0]": 0x9220}
        )
        state = T.State(
            10,
            {
                T.UREG_CODES["I12"]: T.Const(0x30000),
                T.UREG_CODES["M15"]: T.Const(0),
                T.UREG_CODES["R2"]: T.Const(4),
            },
        )
        advanced = self.run_one(state, insn("9a_abs", fields, 6))
        self.assertEqual(advanced.uregs[T.UREG_CODES["R2"]], T.Const(5))
        self.assertEqual(advanced.pending.target, 0x30000)

    def test_type9a_abs_i4_m6_delayed_is_return_with_compute(self):
        fields = self._type9a_abs_fields(
            **{"pmm[2:0]": 6, "j": 1, "compute[22:16]": 0x02, "compute[15:0]": 0x9220}
        )
        state = T.State(0x10, {T.UREG_CODES["R2"]: T.Const(4)}, call_stack=[0x200])
        returned = self.run_one(state, insn("9a_abs", fields, 6))
        self.assertEqual(returned.uregs[T.UREG_CODES["R2"]], T.Const(5))
        self.assertTrue(returned.pending.return_from_call)
        stopped = self.run_one(T.State(0x10), insn("9a_abs", fields, 6))
        self.assertEqual(stopped.stopped, "return without followed call")

    def test_delayed_call_returns_after_variable_width_delay_slots(self):
        for push_length, expected in ((2, 17), (6, 19)):
            with self.subTest(push_length=push_length):
                state = T._transfer(
                    T.State(10), insn("25a_direct", {}, 6), 99, True, True
                )[0]
                state = T._advance(state, insn("3a", {}, push_length))[0]
                state = T._advance(state, insn("16a", {}, 6))[0]
                self.assertEqual(state.stopped, "external-call")
                self.assertEqual(state.trace[-1]["return_sw"], expected)

    def test_dedupe_key_ignores_history_but_not_future_state(self):
        a = T.State(5, {1: T.Const(2)}, steps=3)
        b = T.State(5, {1: T.Const(2)}, steps=9)
        b.trace.append({"action": "compute"})
        self.assertEqual(T._dedupe_key(a), T._dedupe_key(b))
        c = T.State(5, {1: T.Const(2)}, at_loaded_entry=True)
        self.assertNotEqual(T._dedupe_key(a), T._dedupe_key(c))
        d = T.State(5, {1: T.Const(3)})
        self.assertNotEqual(T._dedupe_key(a), T._dedupe_key(d))

    def test_return_idiom_checks_known_i12_m14_target(self):
        fields = {
            "b": 0,
            "cond[4:0]": 0x1F,
            "pmi[2:2]": 1,
            "pmi[1:0]": 0,
            "pmm[2:0]": 6,
            "j": 1,
        }
        regs = {T.UREG_CODES["I12"]: T.Const(0x1FF), T.UREG_CODES["M14"]: T.Const(1)}
        ok = self.run_one(
            T.State(0x10, dict(regs), call_stack=[0x200]), insn("9b_abs", fields)
        )
        self.assertIsNone(ok.stopped)
        self.assertTrue(ok.pending.return_from_call)
        regs[T.UREG_CODES["I12"]] = T.Const(0x2FF)
        bad = self.run_one(
            T.State(0x10, regs, call_stack=[0x200]), insn("9b_abs", fields)
        )
        self.assertEqual(
            bad.stopped, "return target 0x300 differs from recorded return 0x200"
        )

    def test_type3a_post_modify_scales_normal_word_modifier(self):
        fields = {
            "u": 1,
            "i": 7,
            "m": 7,
            "g": 0,
            "d": 1,
            "l": 0,
            "ureg": 2,
            "cond": 31,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        state = T.State(
            1,
            {
                T.UREG_CODES["I7"]: T.Const(0x1000),
                T.UREG_CODES["M7"]: T.Const(0xFFFFFFFF),
                2: T.Const(5),
            },
            assume_nw32=True,
        )
        stored = self.run_one(state, insn("3a", fields, 6))
        self.assertEqual(stored.uregs[T.UREG_CODES["I7"]], T.Const(0x1000 - 4))

    # PRM Table 13-1: "IF cond compute, DM(Ia,Mb) = Ureg" -- cond gates the
    # whole Type3a instruction, not just its compute half, mirroring the
    # True/False/None predicate handling Type2a and Type5a already exercise.

    def _type3a_fields(self, cond):
        return {
            "u": 1,
            "i": 0,
            "m": 0,
            "cond": cond,
            "g": 0,
            "d": 1,
            "l": 0,
            "ureg": 2,
            "compute": 0,
        }

    def test_type3a_predicate_false_skips_transfer_and_compute(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        memory = loader_memory(loader_block(1, 0x80, 4, payload=b"\0" * 4))
        state = T.State(
            1,
            {
                16: T.Const(0x80),
                32: T.Const(1),
                2: T.Const(0xAABBCCDD),
                astatx_code: T.Const(0),  # AC clear -> cond 0x03 (AC) is False
            },
            concrete=memory,
            assume_nw32=True,
        )
        result = self.run_one(state, insn("3a", self._type3a_fields(0x03), 6))
        self.assertEqual(result.pc_sw, 4)  # PC still advances on a skip
        self.assertEqual(result.uregs[16], T.Const(0x80))  # no post-modify
        self.assertEqual(T._dm_read(result, 0x80, 4), T.Const(0))  # store never ran
        self.assertEqual(
            (result.trace[-1]["action"], result.trace[-1]["predicate_assumption"]),
            ("type3a-skipped", False),
        )

    def test_type3a_predicate_true_performs_transfer_and_annotates_trace(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        memory = loader_memory(loader_block(1, 0x80, 4, payload=b"\0" * 4))
        state = T.State(
            1,
            {
                16: T.Const(0x80),
                32: T.Const(1),
                2: T.Const(0xAABBCCDD),
                astatx_code: T.Const(1 << T.AC_BIT),  # AC set -> cond 0x03 True
            },
            concrete=memory,
            assume_nw32=True,
        )
        result = self.run_one(state, insn("3a", self._type3a_fields(0x03), 6))
        self.assertTrue(result.trace[-1]["concrete_write"])
        self.assertEqual(T._dm_read(result, 0x80, 4), T.Const(0xAABBCCDD))
        self.assertEqual(result.uregs[16], T.Const(0x84))
        self.assertEqual(
            (result.trace[-1]["condition"], result.trace[-1]["predicate_assumption"]),
            (0x03, True),
        )

    def test_type3a_unknown_predicate_forks_execute_and_skip(self):
        memory = loader_memory(loader_block(1, 0x80, 4, payload=b"\0" * 4))
        state = T.State(
            1,
            {16: T.Const(0x80), 32: T.Const(1), 2: T.Const(0xAABBCCDD)},
            concrete=memory,
            assume_nw32=True,
        )
        executed, skipped = T._execute(state, insn("3a", self._type3a_fields(0x03), 6))
        self.assertEqual((executed.pc_sw, skipped.pc_sw), (4, 4))
        self.assertEqual(executed.uregs[16], T.Const(0x84))
        self.assertEqual(skipped.uregs[16], T.Const(0x80))
        self.assertEqual(T._dm_read(executed, 0x80, 4), T.Const(0xAABBCCDD))
        self.assertEqual(T._dm_read(skipped, 0x80, 4), T.Const(0))
        self.assertEqual(
            (
                executed.trace[-1]["condition"],
                executed.trace[-1]["predicate_assumption"],
            ),
            (0x03, True),
        )
        self.assertEqual(
            (skipped.trace[-1]["action"], skipped.trace[-1]["predicate_assumption"]),
            ("type3a-skipped", False),
        )

    def test_type9b_abs_indirect_jump_uses_dag2_registers(self):
        fields = {
            "b": 0,
            "a": 0,
            "cond[4:0]": 0x1F,
            "pmi[2:2]": 1,
            "pmi[1:0]": 1,
            "pmm[2:0]": 5,
            "j": 1,
            "ci": 0,
        }
        state = T.State(
            0x10, {T.UREG_CODES["I13"]: T.Const(0x300), T.UREG_CODES["M13"]: T.Const(0)}
        )
        jumped = self.run_one(state, insn("9b_abs", fields))
        self.assertEqual(jumped.pending.target, 0x300)
        stopped = self.run_one(T.State(0x10), insn("9b_abs", fields))
        self.assertEqual(
            stopped.stopped, "unknown 9b_abs indirect target through I13/M13"
        )

    def test_full_compute_negate(self):
        state = self.run_one(
            T.State(1, {2: T.Const(5), T.UREG_CODES["ASTATX"]: T.Const(0)}),
            insn("2a_short", {"compute[22:16]": 0x02, "compute[15:0]": 0x2120}),
        )
        self.assertEqual(state.uregs[1], T.Const(0xFFFFFFFB))
        self.assertEqual(state.trace[-1]["operation"], "negate")

    def test_provisional_form_runs_only_when_named(self):
        word = T.Instruction(
            0, 6, "14d", {"ex": 1}, kind="uncertain", note="source: prm"
        )
        stopped = self.run_one(T.State(1), word)
        self.assertEqual(stopped.stopped, "uncertain or undecodable form: source: prm")
        self.assertEqual(stopped.provisional_used, ())
        allowed = self.run_one(T.State(1, provisional_forms=("14d",)), word)
        self.assertEqual(allowed.provisional_used, ("14d",))
        # The form now reaches its own handler, which stops for its own reason.
        self.assertEqual(allowed.stopped, "unsupported Type14d exclusive access")

    def test_type6b_bit_test_immediate_is_status_only(self):
        astatx = T.UREG_CODES["ASTATX"]
        for label, source_value, expected_sz in (
            ("bit set", 0b1000, False),
            ("bit clear", 0b0100, True),
        ):
            with self.subTest(label=label):
                fields = {
                    "cond[4:0]": 31,
                    "dataex[3:0]": 0,
                    "shiftimm[22:16]": 0x33,
                    "shiftimm[15:0]": (3 << 8) | (1 << 4) | 2,
                }
                state = self.run_one(
                    T.State(1, {2: T.Const(source_value), astatx: T.Const(0)}),
                    insn("6b_shiftimm", fields, 6),
                )
                self.assertNotIn(1, state.uregs)
                self.assertTrue(state.trace[-1]["status_only"])
                self.assertEqual(
                    state.uregs[astatx], T.Const((1 << 12) if expected_sz else 0)
                )

    def test_full_compute_register_to_mr_move_is_recorded(self):
        state = self.run_one(
            T.State(1, {T.UREG_CODES["R2"]: T.Const(0x1234)}),
            insn(
                "2a",
                {
                    "cond[4:0]": 31,
                    "compute[22:16]": 0x41,
                    "compute[15:0]": 0x0200,
                },
                6,
            ),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["R2"]], T.Const(0x1234))
        self.assertEqual(
            (state.trace[-1]["operation"], state.trace[-1]["result_register"]),
            ("mr-data-move", "MR0F"),
        )
        state.uregs[T.UREG_CODES["R10"]] = T.Const(3)
        state = self.run_one(
            state,
            insn(
                "2a",
                {
                    "cond[4:0]": 31,
                    "compute[22:16]": 0x1B,
                    "compute[15:0]": 0x40A2,
                },
                6,
            ),
        )
        self.assertEqual(state.special["MRF"], T.Const(0x1234 + 3 * 0x1234))
        self.assertEqual(state.trace[-1]["operation"], "multiply-accumulate")
        state.uregs[T.UREG_CODES["R6"]] = T.Const(2)
        state.uregs[T.UREG_CODES["R11"]] = T.Const(4)
        state = self.run_one(
            state,
            insn(
                "2a",
                {
                    "cond[4:0]": 31,
                    "compute[22:16]": 0x1B,
                    "compute[15:0]": 0x07B6,
                },
                6,
            ),
        )
        self.assertEqual(state.uregs[T.UREG_CODES["R7"]], T.Const(0x48D8))
        self.assertEqual(state.trace[-1]["operation"], "multiply-add-mrf")

    def test_type2a_increment_unconditional_and_affine(self):
        def full(opcode, rn, rx, ry=0):
            return {
                "compute[22:16]": opcode >> 4,
                "compute[15:0]": ((opcode & 0xF) << 12) | (rn << 8) | (rx << 4) | ry,
            }

        concrete = self.run_one(
            T.State(10, {4: T.Const(0x41)}),
            insn("2a", {"cond[4:0]": 0x1F, **full(0x29, 3, 4)}, 6),
        )
        self.assertEqual((concrete.pc_sw, concrete.uregs[3]), (13, T.Const(0x42)))
        self.assertEqual(
            concrete.trace[-1],
            {
                "pc_sw": 10,
                "form": "2a",
                "action": "compute",
                "operation": "increment",
                "result_register": "R3",
                "value": 0x42,
                "condition": 0x1F,
                "predicate_assumption": True,
            },
        )
        affine = self.run_one(
            T.State(10, {4: T.symbol("counter")}),
            insn("2a", {"cond[4:0]": 0x1F, **full(0x29, 3, 4)}, 6),
        )
        self.assertEqual(affine.uregs[3], T.Affine(1, (("counter", 1),)))

    def test_type2a_decrement_unconditional_and_affine(self):
        def full(opcode, rn, rx, ry=0):
            return {
                "compute[22:16]": opcode >> 4,
                "compute[15:0]": ((opcode & 0xF) << 12) | (rn << 8) | (rx << 4) | ry,
            }

        concrete = self.run_one(
            T.State(10, {4: T.Const(0)}),
            insn("2a", {"cond[4:0]": 0x1F, **full(0x2A, 3, 4)}, 6),
        )
        self.assertEqual((concrete.pc_sw, concrete.uregs[3]), (13, T.Const(0xFFFFFFFF)))
        self.assertEqual(
            concrete.trace[-1],
            {
                "pc_sw": 10,
                "form": "2a",
                "action": "compute",
                "operation": "decrement",
                "result_register": "R3",
                "value": 0xFFFFFFFF,
                "condition": 0x1F,
                "predicate_assumption": True,
            },
        )
        affine = self.run_one(
            T.State(10, {4: T.symbol("counter")}),
            insn("2a", {"cond[4:0]": 0x1F, **full(0x2A, 3, 4)}, 6),
        )
        self.assertEqual(affine.uregs[3], T.Affine(-1, (("counter", 1),)))

    def test_type2a_unknown_predicate_forks_execute_and_skip(self):
        fields = {
            "cond[4:0]": 1,
            "compute[22:16]": 2,
            "compute[15:0]": 0x9340,
        }
        executed, skipped = T._execute(
            T.State(10, {4: T.Const(7)}), insn("2a", fields, 6)
        )
        self.assertEqual((executed.pc_sw, skipped.pc_sw), (13, 13))
        self.assertEqual(executed.uregs[3], T.Const(8))
        self.assertNotIn(3, skipped.uregs)
        self.assertEqual(
            (
                executed.trace[-1]["condition"],
                executed.trace[-1]["predicate_assumption"],
            ),
            (1, True),
        )
        self.assertEqual(
            (
                skipped.trace[-1]["action"],
                skipped.trace[-1]["condition"],
                skipped.trace[-1]["predicate_assumption"],
            ),
            ("compute-skipped", 1, False),
        )
        executed.uregs[3] = T.Const(0)
        self.assertNotIn(3, skipped.uregs)

    def test_type2a_status_only_and_unsupported_do_not_write(self):
        compare = self.run_one(
            T.State(10, {0: T.Const(0x55), 2: T.Const(4), 12: T.Const(4)}),
            insn(
                "2a",
                {
                    "cond[4:0]": 0x1F,
                    "compute[22:16]": 0,
                    "compute[15:0]": 0xA0C2,
                },
                6,
            ),
        )
        self.assertEqual(compare.uregs[0], T.Const(0x55))
        self.assertEqual(
            (compare.trace[-1]["operation"], compare.trace[-1]["status_only"]),
            ("compare", True),
        )
        original = {1: T.Const(2)}
        # cu=11 (reserved, "not used by SINGLEFN" -- PRM Table 18-1 p.423)
        # is never matched by any compute branch, fixed-point or float,
        # so it still reaches the final unsupported-opcode raise. (0xF/0x1234
        # used to serve as this test's "definitely unsupported" filler value,
        # but cu=00 opcode-top-nibble-0xF is now the documented float dual
        # add/subtract encoding -- see test_dual_add_subtract_* below.)
        unsupported = self.run_one(
            T.State(10, dict(original)),
            insn(
                "2a",
                {"cond[4:0]": 0x1F, "compute[22:16]": 0x30, "compute[15:0]": 0},
                6,
            ),
        )
        self.assertIn("unsupported full compute", unsupported.stopped)
        self.assertEqual(unsupported.uregs, original)
        empty = self.run_one(
            T.State(10, dict(original)),
            insn(
                "2a",
                {"cond[4:0]": 0x1F, "compute[22:16]": 0, "compute[15:0]": 0},
                6,
            ),
        )
        self.assertEqual(empty.stopped, "empty full compute")
        self.assertEqual(empty.uregs, original)

    def test_type2a_second_call_delay_slot_forks_to_external_call(self):
        call = insn("25a_direct", {"addr[23:16]": 0, "addr[15:0]": 99}, 4)
        first_slot = insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4)
        type2a = insn(
            "2a",
            {"cond[4:0]": 1, "compute[22:16]": 2, "compute[15:0]": 0x9340},
            6,
        )
        state = self.run_one(T.State(10, {4: T.Const(7)}), call)
        state = self.run_one(state, first_slot)
        executed, skipped = T._execute(state, type2a)
        for result, assumed in ((executed, True), (skipped, False)):
            self.assertEqual(result.stopped, "external-call")
            self.assertEqual(
                (result.trace[-1]["return_sw"], result.trace[-1]["target_sw"]), (17, 99)
            )
            self.assertEqual(result.trace[-2]["predicate_assumption"], assumed)
        self.assertEqual(executed.uregs[3], T.Const(8))
        self.assertNotIn(3, skipped.uregs)

    def test_type4a_pre_post_and_type3c(self):
        base = {
            "i[2:0]": 1,
            "g": 0,
            "d": 0,
            "cond[4:0]": 31,
            "data[5:5]": 1,
            "data[4:0]": 0x1F,
            "dreg[3:0]": 2,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        s = self.run_one(
            T.State(1, {17: T.Const(0x100)}), insn("4a", {**base, "u": 0}, 6)
        )
        self.assertEqual(s.trace[0]["address"], 0xFF)
        self.assertEqual(s.uregs[17], T.Const(0x100))
        # The full compute reads R2 before this postmodify load overwrites it.
        computed = {"compute[22:16]": 0x02, "compute[15:0]": 0x1320}
        s = self.run_one(
            T.State(1, {17: T.Const(0x100), 2: T.Const(4)}),
            insn("4a", {**base, **computed, "u": 1}, 6),
        )
        self.assertEqual(s.uregs[3], T.Const(4))
        s = self.run_one(
            T.State(1, {17: T.Const(0x100), 2: T.Const(4)}),
            insn("4a", {**base, "u": 1, "d": 1}, 6),
        )
        self.assertEqual(s.trace[0]["address"], 0x100)
        self.assertEqual(s.trace[0]["value"], 4)
        self.assertEqual(s.uregs[17], T.Const(0xFF))
        self.assertEqual((T.UREG_CODES["I0"], T.UREG_CODES["M0"]), (16, 32))
        s = self.run_one(
            T.State(1, {16: T.Const(0x80), 32: T.Const(3)}),
            insn("3c", {"dmi[2:0]": 0, "dmm[2:0]": 0, "d": 0, "dreg[3:0]": 3}, 2),
        )
        self.assertEqual(
            (s.trace[0]["space"], s.trace[0]["address"], s.uregs[16]),
            ("DM", 0x80, T.Const(0x83)),
        )
        # The Type3c call-slot case selects DAG1 I7 and M7 directly.
        self.assertEqual((T.UREG_NAMES[23], T.UREG_NAMES[39]), ("I7", "M7"))
        s = self.run_one(
            T.State(1, {23: T.Const(0x90), 39: T.Const(4)}),
            insn("3c", {"dmi[2:0]": 7, "dmm[2:0]": 7, "d": 0, "dreg[3:0]": 3}, 2),
        )
        self.assertEqual((s.trace[0]["address"], s.uregs[23]), (0x90, T.Const(0x94)))
        s = self.run_one(
            T.State(
                1,
                {23: T.Const(0x90), 39: T.Const(1)},
                assume_nw32=True,
            ),
            insn("3c", {"dmi[2:0]": 7, "dmm[2:0]": 7, "d": 0, "dreg[3:0]": 3}, 2),
        )
        self.assertEqual((s.trace[0]["address"], s.uregs[23]), (0x90, T.Const(0x94)))

    def test_type16a_store_and_unknown_postmodify(self):
        f = {
            "i[2:0]": 2,
            "m[2:0]": 3,
            "g": 1,
            "sl": 0,
            "by": 0,
            "data[31:16]": 0x1234,
            "data[15:0]": 0x5678,
        }
        s = self.run_one(
            T.State(1, {26: T.Const(0x90), 43: T.Const(4)}), insn("16a", f, 6)
        )
        self.assertEqual(
            (s.trace[0]["space"], s.trace[0]["value"], s.uregs[26]),
            ("PM", 0x12345678, T.Const(0x94)),
        )
        pm_with_nw32_dm = self.run_one(
            T.State(
                1,
                {26: T.Const(0x90), 43: T.Const(4)},
                assume_nw32=True,
            ),
            insn("16a", f, 6),
        )
        self.assertEqual(pm_with_nw32_dm.uregs[26], T.Const(0x94))
        s = self.run_one(T.State(1), insn("16a", f, 6))
        self.assertIsInstance(s.uregs[26], T.Unknown)

    def test_synthetic_prefix_forms(self):
        full_mul = {"compute[22:16]": 0x17, "compute[15:0]": 0x0212}
        move = {
            "srcureghigh[4:0]": 1,
            "srcureglow[1:1]": 0,
            "srcureglow[0:0]": 0,
            "dstureg[6:0]": 13,
            "cond[4:0]": 31,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        records = [
            insn("5a_move", move, 6),
            insn(
                "15b",
                {"i[2:0]": 0, "g": 0, "d": 1, "l": 1, "ureg[6:0]": 4, "data[6:0]": 0},
            ),
            insn(
                "15b",
                {"i[2:0]": 0, "g": 0, "d": 0, "l": 1, "ureg[6:0]": 14, "data[6:0]": 0},
            ),
            insn("2c", {"compute[11:0]": 0x202}, 2),
            insn("17b", {"ureg[6:0]": 7, "data[15:0]": 1}),
            insn(
                "4a",
                {
                    "i[2:0]": 1,
                    "g": 0,
                    "d": 0,
                    "u": 0,
                    "cond[4:0]": 31,
                    "data[5:5]": 0,
                    "data[4:0]": 0,
                    "dreg[3:0]": 14,
                    "compute[22:16]": 0,
                    "compute[15:0]": 0,
                },
                6,
            ),
            insn(
                "4a",
                {
                    "i[2:0]": 1,
                    "g": 0,
                    "d": 0,
                    "u": 0,
                    "cond[4:0]": 31,
                    "data[5:5]": 0,
                    "data[4:0]": 0,
                    "dreg[3:0]": 4,
                    "compute[22:16]": 0,
                    "compute[15:0]": 0,
                },
                6,
            ),
            insn(
                "5a_move",
                {
                    **move,
                    "srcureghigh[4:0]": 3,
                    "srcureglow[1:1]": 1,
                    "srcureglow[0:0]": 0,
                    "dstureg[6:0]": 4,
                    **full_mul,
                },
                6,
            ),
        ]
        s = T.State(
            1,
            {
                4: T.Const(0x55),
                1: T.Const(6),
                2: T.Const(7),
                16: T.Const(0),
                17: T.Const(0),
            },
        )
        for record in records:
            s = self.run_one(s, record)
        self.assertEqual(s.uregs[13], T.Const(0x55))
        self.assertEqual(s.uregs[2], T.Const(42))
        self.assertIsInstance(s.uregs[14], T.Unknown)
        self.assertEqual(s.uregs[4], s.uregs[14])

    def test_15b_concrete_symbolic_load_and_store(self):
        f = {"i[2:0]": 1, "g": 0, "d": 0, "l": 1, "ureg[6:0]": 2, "data[6:0]": 0x7F}
        s = self.run_one(T.State(1, {17: T.Const(0x100)}), insn("15b", f))
        self.assertEqual(s.trace[0]["address"], 0xFF)
        self.assertIsInstance(s.uregs[2], T.Unknown)
        f["d"] = 1
        s = self.run_one(T.State(1, {2: T.Const(5)}), insn("15b", f))
        self.assertEqual(s.trace[0]["action"], "store")
        self.assertEqual(s.trace[0]["expression"], "I1 + -1")

    def test_normal_word_immediate_offsets_scale_only_when_opted_in(self):
        type4 = {
            "i[2:0]": 1,
            "g": 0,
            "d": 0,
            "cond[4:0]": 31,
            "data[5:5]": 0,
            "data[4:0]": 3,
            "dreg[3:0]": 2,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
            "u": 0,
        }
        state = T.State(1, {17: T.Const(0x100)}, assume_nw32=True)
        s = self.run_one(state, insn("4a", type4, 6))
        self.assertEqual(s.trace[0]["address"], 0x10C)

        type15 = {
            "i[2:0]": 1,
            "g": 0,
            "d": 0,
            "l": 0,
            "ureg[6:0]": 2,
            "data[6:0]": 3,
        }
        state = T.State(1, {17: T.Const(0x100)}, assume_nw32=True)
        s = self.run_one(state, insn("15b", type15))
        self.assertEqual(s.trace[0]["address"], 0x10C)

    def test_type3b_dm_premodify_load_and_pm_postmodify_store(self):
        load = {
            "u": 0,
            "i[2:0]": 1,
            "m[2:0]": 2,
            "g": 0,
            "d": 0,
            "l": 0,
            "x": 1,
            "w": 1,
            "ureg[6:0]": 7,
            "cond[4:0]": 31,
        }
        s = self.run_one(
            T.State(1, {17: T.symbol("buffer"), 34: T.Const(4)}), insn("3b", load)
        )
        event = s.trace[0]
        self.assertEqual(
            (
                event["space"],
                event["expression"],
                event["addressing_mode"],
                event["access_width"],
            ),
            ("DM", "buffer + 0x4", "pre-modify", "normal-word"),
        )
        self.assertEqual(s.uregs[17], T.symbol("buffer"))
        self.assertEqual(s.uregs[7], T.Unknown("memory-address buffer + 0x4"))

        store = {
            **load,
            "u": 1,
            "i[2:0]": 2,
            "m[2:0]": 3,
            "g": 1,
            "d": 1,
            "ureg[6:0]": 4,
        }
        s = self.run_one(
            T.State(1, {26: T.Const(0x90), 43: T.Const(4), 4: T.Const(0x55)}),
            insn("3b", store),
        )
        event = s.trace[0]
        self.assertEqual(
            (
                event["space"],
                event["address"],
                event["value"],
                event["addressing_mode"],
            ),
            ("PM", 0x90, 0x55, "post-modify"),
        )
        self.assertEqual(s.uregs[26], T.Const(0x94))

    def test_type3b_widths_and_rejections_do_not_mutate(self):
        base = {
            "u": 0,
            "i[2:0]": 0,
            "m[2:0]": 0,
            "g": 0,
            "d": 0,
            "ureg[6:0]": 2,
            "cond[4:0]": 31,
        }
        expected = {
            (0, 1, 1): ("normal-word", 0x83),
            (0, 0, 0): ("byte", 0x83),
            (0, 1, 0): ("byte-sign-extended", 0x83),
            (1, 0, 0): ("short-word", 0x86),
            (1, 1, 0): ("short-word-sign-extended", 0x86),
            (1, 1, 1): ("long-word", 0x98),
        }
        for (l, x, w), (access_width, address) in expected.items():
            event = self.run_one(
                T.State(1, {16: T.Const(0x80), 32: T.Const(3)}),
                insn("3b", {**base, "l": l, "x": x, "w": w}),
            ).trace[0]
            self.assertEqual(event["access_width"], access_width)
            self.assertEqual(event["address"], address)

        for fields, reason in (
            ({"l": 0, "x": 0, "w": 1}, "unsupported Type3b access width"),
            (
                {"d": 1, "l": 0, "x": 1, "w": 0},
                "unsupported Type3b sign-extended store",
            ),
        ):
            state = self.run_one(
                T.State(1, {16: T.Const(0x80), 32: T.Const(3), 2: T.Const(9)}),
                insn("3b", {**base, **fields}),
            )
            self.assertEqual(state.stopped, reason)
            self.assertEqual(
                state.uregs, {16: T.Const(0x80), 32: T.Const(3), 2: T.Const(9)}
            )

        invalid = T._execute(
            T.State(1, {16: T.Const(0x80), 32: T.Const(3), 2: T.Const(9)}),
            insn("3b", {**base, "cond[4:0]": 1, "l": 0, "x": 0, "w": 1}),
        )
        self.assertEqual(len(invalid), 1)
        self.assertEqual(invalid[0].stopped, "unsupported Type3b access width")
        self.assertEqual(
            invalid[0].uregs, {16: T.Const(0x80), 32: T.Const(3), 2: T.Const(9)}
        )

    def test_type3b_unknown_predicate_forks_premodify_load_and_postmodify_store(self):
        load = {
            "u": 0,
            "i[2:0]": 1,
            "m[2:0]": 2,
            "g": 0,
            "d": 0,
            "l": 0,
            "x": 1,
            "w": 1,
            "ureg[6:0]": 7,
            "cond[4:0]": 1,
        }
        executed, skipped = T._execute(
            T.State(10, {17: T.symbol("buffer"), 34: T.Const(4)}),
            insn("3b", load),
        )
        self.assertEqual((executed.pc_sw, skipped.pc_sw), (12, 12))
        self.assertEqual(executed.uregs[7], T.Unknown("memory-address buffer + 0x4"))
        self.assertNotIn(7, skipped.uregs)
        self.assertEqual(executed.uregs[17], T.symbol("buffer"))
        self.assertEqual(skipped.uregs[17], T.symbol("buffer"))
        self.assertEqual(
            (
                executed.trace[-1]["condition"],
                executed.trace[-1]["predicate_assumption"],
            ),
            (1, True),
        )
        self.assertEqual(
            skipped.trace[-1],
            {
                "pc_sw": 10,
                "form": "3b",
                "action": "memory-access-skipped",
                "space": "DM",
                "ureg": "R7",
                "addressing_mode": "pre-modify",
                "access_width": "normal-word",
                "condition": 1,
                "predicate_assumption": False,
            },
        )

        store = {**load, "u": 1, "d": 1, "ureg[6:0]": 4}
        executed, skipped = T._execute(
            T.State(10, {17: T.Const(0x80), 34: T.Const(4), 4: T.Const(7)}),
            insn("3b", store),
        )
        self.assertEqual(executed.uregs[17], T.Const(0x84))
        self.assertEqual(skipped.uregs[17], T.Const(0x80))
        self.assertEqual(executed.trace[-1]["value"], 7)
        self.assertEqual(skipped.trace[-1]["addressing_mode"], "post-modify")
        executed.uregs[4] = T.Const(99)
        executed.uregs[17] = T.Const(0)
        self.assertEqual(
            (skipped.uregs[4], skipped.uregs[17]), (T.Const(7), T.Const(0x80))
        )

    def test_type3b_dm_postmodify_and_pm_premodify(self):
        store = {
            "i[2:0]": 1,
            "m[2:0]": 2,
            "d": 1,
            "l": 0,
            "x": 1,
            "w": 1,
            "ureg[6:0]": 4,
            "cond[4:0]": 31,
        }
        dm = self.run_one(
            T.State(1, {17: T.symbol("dm"), 34: T.Const(4), 4: T.Const(7)}),
            insn("3b", {**store, "u": 1, "g": 0}),
        )
        self.assertEqual(dm.trace[0]["expression"], "dm")
        self.assertEqual(dm.uregs[17], T.Affine(4, (("dm", 1),)))

        pm = self.run_one(
            T.State(1, {25: T.symbol("pm"), 42: T.Const(4), 4: T.Const(7)}),
            insn("3b", {**store, "u": 0, "g": 1}),
        )
        self.assertEqual(pm.trace[0]["expression"], "pm + 0x4")
        self.assertEqual(pm.uregs[25], T.symbol("pm"))

    def test_type4b_immediate_width_and_postmodify(self):
        memory = loader_memory(
            loader_block(1, L.SW_ALIAS_BASE + 0x80, 4, payload=b"\x80\x7f\0\0")
        )
        fields = {
            "i[2:0]": 1,
            "g": 0,
            "d": 0,
            "u": 1,
            "cond[4:0]": 31,
            "data[5:5]": 0,
            "data[4:0]": 2,
            "dreg[3:0]": 3,
            "l": 0,
            "x": 1,
            "w": 0,
        }
        state = self.run_one(
            T.State(1, {17: T.Const(0x80)}, concrete=memory),
            insn("4b", fields, 4),
        )
        self.assertEqual(state.uregs[3], T.Const(0xFFFFFF80))
        self.assertEqual(state.uregs[17], T.Const(0x82))
        self.assertEqual(state.trace[0]["access_width"], "byte-sign-extended")

        short = self.run_one(
            T.State(1, {17: T.Const(0x80)}),
            insn("4b", {**fields, "u": 0, "l": 1, "x": 1, "w": 0}, 4),
        )
        self.assertEqual(short.trace[0]["address"], 0x84)
        self.assertEqual(short.trace[0]["access_width"], "short-word-sign-extended")

    def test_type3b_unknown_predicate_second_call_delay_slot_preserves_call_target(
        self,
    ):
        call = insn("25a_direct", {"addr[23:16]": 0, "addr[15:0]": 99}, 4)
        type3b = insn(
            "3b",
            {
                "u": 1,
                "i[2:0]": 0,
                "m[2:0]": 0,
                "g": 0,
                "d": 0,
                "l": 0,
                "x": 1,
                "w": 1,
                "ureg[6:0]": 2,
                "cond[4:0]": 1,
            },
        )
        s = self.run_one(T.State(10, {16: T.Const(0x80), 32: T.Const(4)}), call)
        s = self.run_one(s, insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4))
        executed, skipped = T._execute(s, type3b)
        for result, assumed in ((executed, True), (skipped, False)):
            self.assertEqual(result.stopped, "external-call")
            self.assertEqual(
                (result.trace[-1]["return_sw"], result.trace[-1]["target_sw"]), (16, 99)
            )
            self.assertEqual(result.trace[-2]["predicate_assumption"], assumed)
        self.assertIn(2, executed.uregs)
        self.assertIsInstance(skipped.uregs[2], T.Unknown)

    def test_19a_constant_and_unknown(self):
        f = {
            "g": 1,
            "idis[2:0]": 2,
            "is[2:0]": 1,
            "data[31:16]": 0xFFFF,
            "data[15:0]": 0xFFFE,
        }
        self.assertEqual(
            self.run_one(T.State(1, {25: T.Const(7)}), insn("19a", f, 6)).uregs[27],
            T.Const(5),
        )
        self.assertIsInstance(
            self.run_one(T.State(1), insn("19a", f, 6)).uregs[27], T.Unknown
        )

    def test_delay_slots_variable_width_and_target(self):
        branch = insn(
            "8a_abs",
            {"b": 0, "j": 1, "cond[4:0]": 31, "addr[23:16]": 0, "addr[15:0]": 99},
            6,
        )
        s = self.run_one(T.State(10), branch)
        s = self.run_one(s, insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4))
        s = self.run_one(
            s, insn("17a", {"ureg[6:0]": 1, "data[31:16]": 0, "data[15:0]": 2}, 6)
        )
        self.assertEqual(s.pc_sw, 99)
        self.assertEqual([e["pc_sw"] for e in s.trace], [10, 13, 15])

    def test_conditional_forks_have_independent_two_slot_delays(self):
        branch = insn(
            "8a_abs",
            {"b": 0, "j": 1, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 30},
            6,
        )
        taken, not_taken = T._execute(T.State(10), branch)
        self.assertEqual(taken.trace[-1]["action"], "branch")
        self.assertEqual(not_taken.trace[-1]["action"], "branch-not-taken")
        not_taken.trace[-1]["action"] = "changed-not-taken"
        self.assertEqual(taken.trace[-1]["action"], "branch")

        # The not-taken state retains its delay marker and rejects transfers
        # in both delay slots.
        self.assertEqual(
            self.run_one(not_taken, branch).stopped, "nested delayed transfer"
        )
        _, not_taken = T._execute(T.State(10), branch)
        not_taken = self.run_one(
            not_taken, insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4)
        )
        self.assertEqual(
            self.run_one(not_taken, branch).stopped, "nested delayed transfer"
        )

        taken, not_taken = T._execute(T.State(10), branch)
        slot32 = insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4)
        slot48 = insn("17a", {"ureg[6:0]": 1, "data[31:16]": 0, "data[15:0]": 2}, 6)
        taken = self.run_one(self.run_one(taken, slot32), slot48)
        not_taken = self.run_one(self.run_one(not_taken, slot32), slot48)
        self.assertEqual(taken.pc_sw, 30)
        self.assertEqual(not_taken.pc_sw, 18)
        self.assertEqual([e["pc_sw"] for e in taken.trace], [10, 13, 15])
        self.assertEqual([e["pc_sw"] for e in not_taken.trace], [10, 13, 15])

    def test_type8_without_db_transfers_immediately(self):
        branch = insn(
            "8a_abs",
            {"b": 0, "j": 0, "cond[4:0]": 31, "addr[23:16]": 0, "addr[15:0]": 30},
            6,
        )
        state = self.run_one(T.State(10), branch)
        self.assertEqual(state.pc_sw, 30)
        self.assertIsNone(state.pending)
        self.assertEqual(state.steps, 1)

        conditional = insn(
            "8a_abs",
            {"b": 0, "j": 0, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 30},
            6,
        )
        taken, not_taken = T._execute(T.State(10), conditional)
        self.assertEqual(taken.pc_sw, 30)
        self.assertEqual(not_taken.pc_sw, 13)
        self.assertEqual(not_taken.trace[-1]["action"], "branch-not-taken")

    def test_type8_call_without_db_uses_immediate_return_address(self):
        call = insn(
            "8a_abs",
            {"b": 1, "j": 0, "cond[4:0]": 31, "addr[23:16]": 0, "addr[15:0]": 99},
            6,
        )
        taken = self.run_one(T.State(10), call)
        self.assertEqual(taken.stopped, "external-call")
        self.assertEqual(
            (taken.trace[-1]["target_sw"], taken.trace[-1]["return_sw"]), (99, 13)
        )

        conditional = insn(
            "8a_abs",
            {"b": 1, "j": 0, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 99},
            6,
        )
        taken, not_taken = T._execute(T.State(10), conditional)
        self.assertEqual(taken.stopped, "external-call")
        self.assertEqual(taken.trace[-1]["return_sw"], 13)
        self.assertEqual(not_taken.pc_sw, 13)
        self.assertIsNone(not_taken.stopped)
        self.assertEqual(not_taken.trace[-1]["action"], "branch-not-taken")

        negative = insn(
            "8a_rel",
            {
                "b": 1,
                "j": 0,
                "cond[4:0]": 31,
                "reladdr[23:16]": 0x80,
                "reladdr[15:0]": 0,
            },
            6,
        )
        guarded = self.run_one(
            T.State(
                0,
                concrete=loader_memory(loader_block(0, 0, 4, payload=b"\0" * 4)),
                follow_loaded_calls=True,
            ),
            negative,
        )
        self.assertEqual(guarded.stopped, "external-call")
        self.assertEqual(guarded.trace[-1]["target_sw"], 0x800000)

    def test_delayed_call_returns_after_variable_width_slots(self):
        call = insn("25a_direct", {"addr[23:16]": 0, "addr[15:0]": 99}, 4)
        s = self.run_one(T.State(10), call)
        s = self.run_one(s, insn("17b", {"ureg[6:0]": 0, "data[15:0]": 1}, 4))
        s = self.run_one(
            s, insn("17a", {"ureg[6:0]": 1, "data[31:16]": 0, "data[15:0]": 2}, 6)
        )
        self.assertEqual(s.stopped, "external-call")
        self.assertEqual(s.trace[-1]["return_sw"], 17)
        self.assertEqual(s.trace[-1]["target_sw"], 99)
        self.assertEqual([e["pc_sw"] for e in s.trace[:-1]], [10, 10, 12, 14])

    def test_type9a_relative_branch_applies_compute_on_true_bit_test(self):
        branch = insn(
            "9a_rel",
            {
                "b": 0,
                "a": 0,
                "cond[4:0]": 0x0D,
                "j": 1,
                "e": 0,
                "ci": 0,
                "compute[22:16]": 0x02,
                "compute[15:0]": 0x9220,
                "reladdr[5:5]": 0,
                "reladdr[4:0]": 7,
            },
            6,
        )
        true_state = self.run_one(
            T.State(
                10,
                {
                    T.UREG_CODES["R2"]: T.Const(4),
                    T.UREG_CODES["ASTATX"]: T.Const(1 << 18),
                },
            ),
            branch,
        )
        self.assertEqual(true_state.uregs[T.UREG_CODES["R2"]], T.Const(5))
        self.assertEqual(true_state.pending.target, 17)

        false_state = self.run_one(
            T.State(
                10,
                {
                    T.UREG_CODES["R2"]: T.Const(4),
                    T.UREG_CODES["ASTATX"]: T.Const(0),
                },
            ),
            branch,
        )
        self.assertEqual(false_state.uregs[T.UREG_CODES["R2"]], T.Const(4))
        self.assertIsNone(false_state.pending)
        self.assertEqual(false_state.pc_sw, 13)

        taken, not_taken = T._execute(
            T.State(10, {T.UREG_CODES["R2"]: T.Const(4)}), branch
        )
        self.assertEqual(taken.uregs[T.UREG_CODES["R2"]], T.Const(5))
        self.assertEqual(taken.pending.target, 17)
        self.assertEqual(not_taken.uregs[T.UREG_CODES["R2"]], T.Const(4))
        self.assertIsNone(not_taken.pending)

    def test_type2a_not_av_saturate_mrf_preserves_unknown_dependency(self):
        saturate = insn(
            "2a",
            {
                "cond[4:0]": 0x14,
                "compute[22:16]": 0x10,
                "compute[15:0]": 0x072E,
            },
            6,
        )
        executed = self.run_one(
            T.State(10, {T.UREG_CODES["ASTATX"]: T.Const(0)}), saturate
        )
        self.assertIsInstance(executed.uregs[T.UREG_CODES["R7"]], T.Unknown)
        self.assertEqual(executed.trace[-1]["operation"], "saturate-mrf")
        skipped = self.run_one(
            T.State(10, {T.UREG_CODES["ASTATX"]: T.Const(1 << 1)}), saturate
        )
        self.assertNotIn(T.UREG_CODES["R7"], skipped.uregs)
        self.assertEqual(skipped.trace[-1]["action"], "compute-skipped")

    def test_cjump_frame_and_rframe_restore_compiler_frame(self):
        call = insn("25a_direct", {"addr[23:16]": 0, "addr[15:0]": 99}, 4)
        state = self.run_one(
            T.State(
                10,
                {
                    T.UREG_CODES["I6"]: T.Const(0x200),
                    T.UREG_CODES["I7"]: T.Const(0x100),
                },
            ),
            call,
        )
        self.assertEqual(state.uregs[T.UREG_CODES["R2"]], T.Const(0x200))
        self.assertEqual(state.uregs[T.UREG_CODES["I6"]], T.Const(0x100))
        self.assertEqual(state.trace[-2]["action"], "cjump-frame")

        restoring = T.State(
            30,
            {
                T.UREG_CODES["I6"]: T.Const(0x100),
                T.UREG_CODES["I7"]: T.Const(0x80),
            },
            pending=T.Pending(None, slots=2, return_from_call=True),
            call_stack=[40],
            concrete=loader_memory(loader_block(0, 0x100, 4, payload=b"\0" * 4)),
            assume_nw32=True,
        )
        self.assertTrue(T._dm_write(restoring, 0x100, 4, T.Const(0x200)))
        restored = self.run_one(restoring, insn("25c_rframe", {}, 2))
        self.assertEqual(restored.uregs[T.UREG_CODES["I7"]], T.Const(0x100))
        self.assertEqual(restored.uregs[T.UREG_CODES["I6"]], T.Const(0x200))
        self.assertEqual(restored.trace[-1]["action"], "rframe")

    def test_type16_immediate_stores_scale_normal_word_postmodify(self):
        memory = loader_memory(loader_block(0, 0xF8, 16, payload=b"\0" * 16))
        state = T.State(
            1,
            {
                T.UREG_CODES["I7"]: T.Const(0x100),
                T.UREG_CODES["M7"]: T.Const(0xFFFFFFFF),
            },
            concrete=memory,
            assume_nw32=True,
        )
        stored16 = self.run_one(
            state,
            insn(
                "16b",
                {"i[2:0]": 7, "m[2:0]": 7, "g": 0, "data[15:0]": 2},
                4,
            ),
        )
        self.assertEqual(T._dm_read(stored16, 0x100, 4), T.Const(2))
        self.assertEqual(stored16.uregs[T.UREG_CODES["I7"]], T.Const(0xFC))
        stored32 = self.run_one(
            stored16,
            insn(
                "16a",
                {
                    "i[2:0]": 7,
                    "m[2:0]": 7,
                    "g": 0,
                    "sl": 0,
                    "by": 0,
                    "data[31:16]": 0x1234,
                    "data[15:0]": 0x5678,
                },
                6,
            ),
        )
        self.assertEqual(T._dm_read(stored32, 0xFC, 4), T.Const(0x12345678))
        self.assertEqual(stored32.uregs[T.UREG_CODES["I7"]], T.Const(0xF8))

    def test_provisional_stop(self):
        self.assertIn(
            "uncertain",
            self.run_one(T.State(1), insn("19p", {}, kind="uncertain")).stopped,
        )

    def test_affine_canonicalization_and_arithmetic(self):
        value = T.Affine(0x1_0000_0001, (("z", 2), ("a", 1), ("z", -2), ("a", -1)))
        self.assertEqual((value.constant, value.terms), (1, ()))
        self.assertEqual(T._affine(3, (("z", 1),)), T.Affine(3, (("z", 1),)))
        self.assertEqual(T._affine(3, ()), T.Const(3))
        receive = T.symbol("receive_buffer")
        self.assertEqual(
            T._subtract(T._add(receive, T.Const(4), "ignored"), T.Const(5), "ignored"),
            T.Affine(0xFFFFFFFF, (("receive_buffer", 1),)),
        )
        self.assertEqual(
            T._multiply(T.Const(2), T.symbol("track_index"), "ignored"),
            T.Affine(0, (("track_index", 2),)),
        )
        rejected = T._multiply(
            receive, T.symbol("track_index"), "receive_buffer * track_index"
        )
        self.assertIsInstance(rejected, T.Unknown)
        self.assertIn("non-affine", rejected.reason)
        with self.assertRaises(ValueError):
            T.symbol("not-valid")

    def test_affine_compute_and_memory_events(self):
        short = lambda opcode, rn, rx: {"compute[11:0]": (opcode << 8) | (rn << 4) | rx}
        state = T.State(1, {1: T.symbol("receive_buffer"), 2: T.Const(0x94)})
        state = self.run_one(state, insn("2c", short(0, 1, 2), 2))
        self.assertEqual(state.uregs[1], T.Affine(0x94, (("receive_buffer", 1),)))
        self.assertEqual(T._render(state.uregs[1]), "receive_buffer + 0x94")
        state.uregs[3] = T.symbol("track_index")
        state.uregs[4] = T.Const(2)
        full = {"compute[22:16]": 0x17, "compute[15:0]": 0x0534}
        state.uregs[5] = T._compute(full, False, state.uregs)[1]
        state = self.run_one(state, insn("2c", short(0, 1, 5), 2))
        self.assertEqual(
            T._render(state.uregs[1]),
            "receive_buffer + 2*track_index + 0x94",
        )
        store = {"i[2:0]": 1, "g": 0, "d": 1, "l": 1, "ureg[6:0]": 2, "data[6:0]": 0x14}
        event = self.run_one(
            T.State(1, {17: state.uregs[1], 2: T.Const(5)}), insn("15b", store)
        ).trace[0]
        self.assertEqual(event["expression"], "receive_buffer + 2*track_index + 0xa8")
        self.assertEqual(
            event["address"],
            {
                "affine": {
                    "constant": 0xA8,
                    "terms": [["receive_buffer", 1], ["track_index", 2]],
                }
            },
        )
        self.assertEqual(json.loads(json.dumps(event))["address"], event["address"])

    def test_affine_type19_and_seed_handling(self):
        f = {"g": 0, "idis[2:0]": 2, "is[2:0]": 1, "data[31:16]": 0, "data[15:0]": 0x94}
        state = self.run_one(
            T.State(1, {17: T.symbol("receive_buffer")}), insn("19a", f, 6)
        )
        self.assertEqual(T._render(state.uregs[19]), "receive_buffer + 0x94")
        seeded = T.trace(b"", 0, 0, {"R1": 7}, max_steps=1)[0]
        self.assertEqual(seeded.uregs[1], T.Const(7))
        symbolic = T.trace(b"", 0, 0, {"R1": "@receive_buffer"}, max_steps=1)[0]
        self.assertEqual(symbolic.uregs[1], T.symbol("receive_buffer"))
        with self.assertRaises(ValueError):
            T.trace(b"", 0, 0, {"R1": "@"}, max_steps=1)

    def test_scaled_type19_circular_modify_of_bare_symbol_yields_fresh_symbol(self):
        # B7 not concrete (the ordinary case when a function is traced from
        # its own entry with every register seeded as its own named
        # symbol), L7 concrete (tools/sharcwriters.py's own
        # GLOBAL_CONSTANT_SEEDS): the old behaviour collapsed straight to
        # Unknown("scaled circular modify I7"). It should now recognise
        # I7's bare entry symbol and mint a FRESH symbol for the result --
        # never reuse "I7e" itself, which would falsely assert the result
        # equals I7's own entry value (see the "different sites" test
        # below for why that would be unsound).
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        state = self.run_one(
            T.State(0x10, {i7: T.symbol("I7e"), l7: T.Const(0x1FD)}),
            insn("19a_scaled", fields, length=6),
        )
        result = state.uregs[i7]
        self.assertIsInstance(result, T.Affine)
        self.assertEqual(result.constant, 0)
        self.assertEqual(len(result.terms), 1)
        name, coefficient = result.terms[0]
        self.assertEqual(coefficient, 1)
        self.assertTrue(name.startswith(T.CIRC_SYMBOL_PREFIX))
        self.assertNotEqual(name, "I7e")  # never the input symbol itself
        self.assertTrue(state.trace[-1]["circular"])

    def test_scaled_type19_different_modify_sites_yield_different_symbols(self):
        # Two circular MODIFYs of I7's bare entry symbol at two different
        # program counters must NOT be asserted equal: reusing one name for
        # both (the bug this replaces) would let the Affine algebra cancel
        # "site A's result - site B's result" to a spurious 0, aliasing two
        # provably-different stack frames. Distinct PCs must mint distinct
        # symbol names.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        seed = {i7: T.symbol("I7e"), l7: T.Const(0x1FD)}
        at_a = self.run_one(T.State(0x10, dict(seed)), insn("19a_scaled", fields, length=6))
        at_b = self.run_one(T.State(0x20, dict(seed)), insn("19a_scaled", fields, length=6))
        self.assertNotEqual(at_a.uregs[i7], at_b.uregs[i7])
        # Re-running the SAME site is deterministic (same name each time),
        # which is what lets a store reached along two predicate-fork paths
        # from the same PC still compare equal -- only different sites
        # differ.
        again_a = self.run_one(T.State(0x10, dict(seed)), insn("19a_scaled", fields, length=6))
        self.assertEqual(at_a.uregs[i7], again_a.uregs[i7])

    def test_scaled_type19_circular_modify_chains_through_a_prior_circ_symbol(self):
        # A value already bounded by an EARLIER circular MODIFY (a
        # CIRC_SYMBOL_PREFIX symbol, not a raw entry seed) also qualifies:
        # the second MODIFY still yields a fresh symbol, not Unknown.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        prior = T.Affine(0, ((T.CIRC_SYMBOL_PREFIX + "7_10", 1),))
        state = self.run_one(
            T.State(0x20, {i7: prior, l7: T.Const(0x1FD)}),
            insn("19a_scaled", fields, length=6),
        )
        self.assertIsInstance(state.uregs[i7], T.Affine)
        self.assertTrue(state.trace[-1]["circular"])

    def test_scaled_type19_circular_modify_of_symbol_with_small_offset_still_bounded(self):
        # PRM p.6-23's single +-byte_length correction still recovers a
        # value within one buffer length of a symbol's own bound when the
        # input carries a small existing offset (e.g. after a push/pop),
        # not just when it is perfectly bare.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        adjusted = T._add(T.symbol("I7e"), T.Const(-8), "I7e - 8")
        state = self.run_one(
            T.State(0x10, {i7: adjusted, l7: T.Const(0x1FD)}),
            insn("19a_scaled", fields, length=6),
        )
        self.assertIsInstance(state.uregs[i7], T.Affine)
        self.assertTrue(state.trace[-1]["circular"])

    def test_scaled_type19_circular_modify_of_symbol_with_large_offset_is_unknown(self):
        # An offset at or beyond one buffer length (0x1fd * 4 = 0x7f4) is
        # no longer provably "within one buffer length" of the symbol's
        # own bound, so the conservative Unknown fallback still applies.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        adjusted = T._add(T.symbol("I7e"), T.Const(0x800), "I7e + 0x800")
        state = self.run_one(
            T.State(0x10, {i7: adjusted, l7: T.Const(0x1FD)}),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(state.uregs[i7], T.Unknown("scaled circular modify I7"))

    def test_scaled_type19_circular_modify_of_unrecognized_symbol_is_unknown(self):
        # A bare symbol that does not match the recognised entry-seed or
        # circ_ naming convention (e.g. a loop-count or unrelated symbol
        # that happens to reach this register) is not assumed bounded.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        l7 = T.UREG_CODES["L7"]
        state = self.run_one(
            T.State(0x10, {i7: T.symbol("loop_count"), l7: T.Const(0x1FD)}),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(state.uregs[i7], T.Unknown("scaled circular modify I7"))

    def test_scaled_type19_circular_modify_without_concrete_length_is_unknown(self):
        # L7 itself not concrete: no numeric byte_length is available to
        # bound the accepted input offset against, so this falls back to
        # Unknown even for a bare, recognised symbol.
        fields = {
            "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
            "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        state = self.run_one(
            T.State(0x10, {i7: T.symbol("I7e")}),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(state.uregs[i7], T.Unknown("scaled circular modify I7"))

    def test_stack_bounded_symbol_helper(self):
        self.assertEqual(T._stack_bounded_symbol(T.symbol("I7e")), ("I7e", 0))
        self.assertEqual(
            T._stack_bounded_symbol(T._add(T.symbol("I7e"), T.Const(4), "x")),
            ("I7e", 4),
        )
        self.assertEqual(
            T._stack_bounded_symbol(T.symbol(T.CIRC_SYMBOL_PREFIX + "7_10")),
            (T.CIRC_SYMBOL_PREFIX + "7_10", 0),
        )
        self.assertIsNone(T._stack_bounded_symbol(T.Const(5)))
        self.assertIsNone(T._stack_bounded_symbol(T.symbol("loop_count")))
        two_terms = T.Affine(0, (("I7e", 1), ("M7e", 1)))
        self.assertIsNone(T._stack_bounded_symbol(two_terms))
        scaled = T.Affine(0, (("I7e", 2),))
        self.assertIsNone(T._stack_bounded_symbol(scaled))

    def test_scaled_type19_normal_word_modify_and_circular_wrap(self):
        fields = {
            "w": 1,
            "g": 0,
            "idis[2:0]": 0,
            "is[2:0]": 7,
            "data[31:16]": 0xFFFF,
            "data[15:0]": 0xFFFE,
        }
        i7 = T.UREG_CODES["I7"]
        b7 = T.UREG_CODES["B7"]
        l7 = T.UREG_CODES["L7"]

        byte_space = self.run_one(
            T.State(
                0xB893E2,
                {
                    i7: T.Const(0x26F7EE),
                    b7: T.Const(0x26F000),
                    l7: T.Const(0x1FD),
                },
                assume_nw32=True,
            ),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(byte_space.uregs[i7], T.Const(0x26F7E6))
        self.assertEqual(byte_space.uregs[b7], T.Const(0x26F000))
        self.assertEqual(byte_space.uregs[l7], T.Const(0x1FD))
        self.assertEqual(byte_space.trace[-1]["offset"], -8)

        normal_space = self.run_one(
            T.State(
                0x10,
                {i7: T.Const(0x100), b7: T.Const(0), l7: T.Const(0)},
            ),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(normal_space.uregs[i7], T.Const(0xFE))

        wrapped = self.run_one(
            T.State(
                0x10,
                {i7: T.Const(0x104), b7: T.Const(0x100), l7: T.Const(4)},
                assume_nw32=True,
            ),
            insn("19a_scaled", fields, length=6),
        )
        self.assertEqual(wrapped.uregs[i7], T.Const(0x10C))
        self.assertTrue(wrapped.trace[-1]["circular"])

    def test_cli_symbolic_seed(self):
        with tempfile.NamedTemporaryFile("wb", delete=False) as f:
            path = f.name
        try:
            valid = subprocess.run(
                [
                    sys.executable,
                    "tools/sharc_trace.py",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "R1=@receive_buffer",
                    "--json",
                ],
                capture_output=True,
            )
            invalid = subprocess.run(
                [
                    sys.executable,
                    "tools/sharc_trace.py",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "R1=@not-valid",
                ],
                capture_output=True,
            )
            invalid_register = subprocess.run(
                [
                    sys.executable,
                    "tools/sharc_trace.py",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "BOGUS=1",
                ],
                capture_output=True,
            )
            self.assertEqual(valid.returncode, 0, valid.stderr.decode())
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn("NAME=@symbol", invalid.stderr.decode())
            self.assertNotEqual(invalid_register.returncode, 0)
            self.assertNotIn("Traceback", invalid_register.stderr.decode())
        finally:
            os.unlink(path)

    def test_blob_backed_exact_pc_decode(self):
        pc = 0x20
        address = L.sw_to_byte(pc)
        rframe = bytes.fromhex("0119")
        full = bytes.fromhex("000f00000000")  # Confident 48-bit Type 17a.

        # The established flat-image API remains unchanged.
        self.assertEqual(T.decode_at(rframe, pc, pc).type_name, "25c_rframe")
        self.assertEqual(
            T.decode_at(
                loader_memory(loader_block(0, address, 6, payload=full)), None, pc
            ).type_name,
            "17a",
        )
        # Adjacent loader blocks are one contiguous decode window.
        self.assertEqual(
            T.decode_at(
                loader_memory(
                    loader_block(0, address, 4, payload=full[:4]),
                    loader_block(0, address + 4, 2, payload=full[4:]),
                ),
                None,
                pc,
            ).type_name,
            "17a",
        )
        # Trying smaller windows permits a valid 16-bit form at range end.
        self.assertEqual(
            T.decode_at(
                loader_memory(loader_block(0, address, 2, payload=rframe)), None, pc
            ).type_name,
            "25c_rframe",
        )

    def test_blob_backed_gap_truncation_and_overlap(self):
        pc = 0x30
        address = L.sw_to_byte(pc)
        truncated = T.decode_at(
            loader_memory(
                loader_block(0, address, 4, payload=bytes.fromhex("000f0000"))
            ),
            None,
            pc,
        )
        self.assertEqual(truncated.kind, "unknown")
        self.assertIn("17a (48 bits) but only 4 bytes remain", truncated.note)
        unmapped = T.decode_at(
            loader_memory(
                loader_block(0, address + 2, 2, payload=bytes.fromhex("0119"))
            ),
            None,
            pc,
        )
        self.assertEqual(unmapped.note, "PC unmapped in loader memory")
        # LoadedMemory's stream-order last-write rule is visible to decoding.
        overwritten = loader_memory(
            loader_block(0, address, 2, payload=bytes.fromhex("0119")),
            loader_block(0, address, 2, payload=bytes.fromhex("800a")),
        )
        self.assertEqual(T.decode_at(overwritten, None, pc).type_name, "11c")

    def test_blob_cli_validation_and_json(self):
        pc = 0x40
        address = L.sw_to_byte(pc)
        with tempfile.NamedTemporaryFile("wb", delete=False) as stream:
            stream.write(loader_block(0, address, 2, payload=bytes.fromhex("0119")))
            stream.write(loader_block(1 << L.BFLAGS["FINAL"], 0, 0))
            stream_path = stream.name
        with tempfile.NamedTemporaryFile("wb", delete=False) as empty:
            empty.write(loader_block(1 << L.BFLAGS["FINAL"], 0, 0))
            empty_path = empty.name
        with tempfile.NamedTemporaryFile("w", delete=False) as trace_file:
            trace_path = trace_file.name
        try:
            command = [sys.executable, "tools/sharc_trace.py"]
            missing_base = subprocess.run(
                command + [stream_path, "--start", hex(pc)], capture_output=True
            )
            ambiguous = subprocess.run(
                command + [stream_path, "--blob", "--base-sw", "0", "--start", hex(pc)],
                capture_output=True,
            )
            no_ranges = subprocess.run(
                command + [empty_path, "--blob", "--start", hex(pc)],
                capture_output=True,
            )
            valid = subprocess.run(
                command + [stream_path, "--blob", "--start", hex(pc), "--json"],
                capture_output=True,
            )
            concrete = subprocess.run(
                command
                + [
                    stream_path,
                    "--blob",
                    "--start",
                    hex(pc),
                    "--concrete-memory",
                    "--dossier-bytes",
                    "4",
                    "--json",
                ],
                capture_output=True,
            )
            summary = subprocess.run(
                command
                + [
                    stream_path,
                    "--blob",
                    "--start",
                    hex(pc),
                    "--summary",
                    "--trace-json",
                    trace_path,
                ],
                capture_output=True,
            )
            unsafe = subprocess.run(
                command
                + [
                    stream_path,
                    "--base-sw",
                    "0",
                    "--start",
                    hex(pc),
                    "--concrete-memory",
                ],
                capture_output=True,
            )
            self.assertNotEqual(missing_base.returncode, 0)
            self.assertIn("--base-sw is required", missing_base.stderr.decode())
            self.assertNotEqual(ambiguous.returncode, 0)
            self.assertIn("ambiguous", ambiguous.stderr.decode())
            self.assertNotEqual(no_ranges.returncode, 0)
            self.assertIn("no loaded ranges", no_ranges.stderr.decode())
            self.assertEqual(valid.returncode, 0, valid.stderr.decode())
            self.assertEqual(concrete.returncode, 0, concrete.stderr.decode())
            self.assertEqual(summary.returncode, 0, summary.stderr.decode())
            self.assertNotEqual(unsafe.returncode, 0)
            self.assertIn("requires --blob", unsafe.stderr.decode())
            self.assertNotIn("Traceback", valid.stderr.decode())
            self.assertNotIn("raw", valid.stdout.decode())
            json.loads(valid.stdout)
            summary_result = json.loads(summary.stdout)
            self.assertEqual(summary_result["start_sw"], pc)
            self.assertEqual(len(summary_result["states"]), 1)
            with open(trace_path) as trace_input:
                self.assertEqual(len(json.load(trace_input)), 1)
        finally:
            os.unlink(stream_path)
            os.unlink(empty_path)
            os.unlink(trace_path)

    def test_concrete_loader_reads_and_forked_write_overlays(self):
        # Loader memory is keyed at the alias, while application DM code uses
        # the unaliased address.  Later overlapping loader data still wins.
        address = L.SW_ALIAS_BASE + 0x100
        memory = loader_memory(
            loader_block(1, address, 4, payload=b"\x11\x22\x33\x44"),
            loader_block(1, address + 2, 2, payload=b"\xaa\xbb"),
        )
        self.assertIsNone(T._dm_read(T.State(1, concrete=memory), 0x100, 4))
        self.assertEqual(
            T._dm_read(T.State(1, concrete=memory), 0x100, 1), T.Const(0x11)
        )
        state = T.State(1, {0: T.Const(0x100)}, concrete=memory, assume_nw32=True)
        load = insn(
            "14a",
            {
                "l": 0,
                "addr[31:16]": 0,
                "addr[15:0]": 0x100,
                "ureg[6:0]": 1,
                "g": 0,
                "d": 0,
            },
            6,
        )
        loaded = self.run_one(state, load)
        self.assertEqual(loaded.uregs[1], T.Const(0xBBAA2211))
        left, right = T._copy(loaded), T._copy(loaded)
        self.assertTrue(T._dm_write(left, 0x100, 4, T.Const(0x01020304)))
        self.assertEqual(T._dm_read(left, 0x100, 4), T.Const(0x01020304))
        self.assertEqual(T._dm_read(right, 0x100, 4), T.Const(0xBBAA2211))
        self.assertTrue(T._dm_write(left, 0x200, 4, T.Const(0x55667788)))
        self.assertEqual(T._dm_read(left, 0x200, 4), T.Const(0x55667788))
        self.assertTrue(T._dm_write(left, 0x310CA300, 4, T.Const(0x11223344)))
        self.assertEqual(T._dm_read(left, 0x310CA300, 4), T.Const(0x11223344))

    def test_trace_real_return_idiom_and_external_dossier_modes(self):
        # The return is real decoder-backed firmware syntax: Type9b_abs raw
        # 0x083f343f, a normal delay slot, then Type25c_rframe raw 0x1901.
        start, target = 0x100, 0x110
        call = encode("25a_direct", target)
        # Two 16-bit slots end at start+5, and the call returns to the
        # instruction after its second delay slot.
        slot = bytes.fromhex("f29f")
        return_branch = struct.pack("<HH", 0x083F, 0x343F)
        rframe = struct.pack("<H", 0x1901)
        payload = bytearray((target - start + 5) * 2)
        payload[: len(call)] = call
        payload[len(call) : len(call) + len(slot)] = slot
        payload[len(call) + len(slot) : len(call) + 2 * len(slot)] = slot
        offset = (target - start) * 2
        payload[offset : offset + 4] = return_branch
        payload[offset + 4 : offset + 4 + len(slot)] = slot
        payload[offset + 4 + len(slot) : offset + 4 + len(slot) + 2] = rframe
        memory = loader_memory(
            loader_block(1, L.sw_to_byte(start), len(payload), payload=bytes(payload)),
            loader_block(1, L.SW_ALIAS_BASE + 0x300, 4, payload=b"\0" * 4),
        )
        self.assertEqual(T.decode_at(memory, None, target).type_name, "9b_abs")
        self.assertEqual(T.decode_at(memory, None, target + 3).type_name, "25c_rframe")
        returned = T.trace(
            memory,
            None,
            start,
            max_steps=6,
            concrete_memory=True,
            follow_loaded_calls=True,
        )[0]
        actions = [event["action"] for event in returned.trace]
        self.assertIn("loaded-call-enter", actions)
        self.assertIn("return-branch", actions)
        self.assertIn("loaded-call-return", actions)
        self.assertEqual(returned.pc_sw, start + 5)

        stopped = T.trace(
            memory,
            None,
            start,
            {"I0": 0x300},
            max_steps=10,
            concrete_memory=True,
            dossier_bytes=4,
        )[0]
        continued = T.trace(
            memory,
            None,
            start,
            {"I0": 0x300},
            max_steps=10,
            concrete_memory=True,
            continue_external_calls=True,
            dossier_bytes=4,
        )[0]
        self.assertEqual(stopped.stopped, "external-call")
        self.assertEqual(stopped.trace[-1]["objects"][0]["bytes"], [0, 0, 0, 0])
        self.assertIn(
            "opaque-external-call", [event["action"] for event in continued.trace]
        )
        self.assertIsInstance(continued.uregs[0], T.Unknown)

    def test_trace_executes_scaled_type19_loaded_entry(self):
        start = 0x10
        payload = bytes.fromhex("8715fffffeff")
        memory = loader_memory(
            loader_block(1, L.sw_to_byte(start), len(payload), payload=payload)
        )
        state = T.trace(
            memory,
            None,
            start,
            {"I7": 0x26F7EE, "B7": 0x26F000, "L7": 0x1FD},
            max_steps=1,
            concrete_memory=True,
            assume_nw32=True,
        )[0]
        self.assertEqual(state.uregs[T.UREG_CODES["I7"]], T.Const(0x26F7E6))
        self.assertEqual(state.trace[-2]["action"], "i-add")

    def test_trace_can_seed_documented_core_reset_state(self):
        start = 0x10
        memory = loader_memory(
            loader_block(1, L.sw_to_byte(start), 2, payload=bytes.fromhex("f29f"))
        )
        state = T.trace(
            memory,
            None,
            start,
            {"MODE1": 5},
            max_steps=0,
            concrete_memory=True,
            core_reset_state=True,
        )[0]
        self.assertEqual(state.uregs[T.UREG_CODES["MODE1"]], T.Const(5))
        self.assertEqual(state.uregs[T.UREG_CODES["MMASK"]], T.Const(0))
        self.assertEqual(T._dm_read(state, T.Const(0x31400), 4), T.Const(0))
        self.assertTrue(state.core_reset_state)

    def test_event_values_are_json_safe(self):
        type3c = {"dmi[2:0]": 0, "dmm[2:0]": 0, "d": 1, "dreg[3:0]": 3}
        s = self.run_one(
            T.State(1, {16: T.Const(0x80), 32: T.Const(3)}), insn("3c", type3c, 2)
        )
        self.assertEqual(s.trace[0]["value"], {"unknown": "uninitialized R3"})
        encoded = json.dumps(s.trace)
        self.assertIn('"unknown": "uninitialized R3"', encoded)
        self.assertNotIn("b'", encoded)

    def test_runtime_summary_keeps_only_named_peripheral_accesses(self):
        state = T.State(0x20, steps=7, stopped="test-stop")
        state.trace = [
            {
                "pc_sw": 0x10,
                "form": "14a",
                "action": "store",
                "address": 0x100,
                "value": 1,
            },
            {
                "pc_sw": 0x13,
                "form": "12a_imm",
                "action": "loop-setup",
                "start_sw": 0x16,
                "end_sw": 0x16,
                "count": 4,
                "mode": 1,
            },
            {
                "pc_sw": 0x16,
                "form": "14a",
                "action": "store",
                "address": 0x310CA300,
                "value": 0x1234,
                "access_width": "normal-word",
            },
            {
                "pc_sw": 0x19,
                "form": "test_form",
                "action": "stop",
                "reason": "test-stop",
            },
        ]
        summary = T.summarize([state], 0x10)
        item = summary["states"][0]
        self.assertEqual(summary["start_sw"], 0x10)
        self.assertEqual(item["stop_pc_sw"], 0x19)
        self.assertEqual(item["stop_form"], "test_form")
        self.assertEqual(item["loop_setups"][0]["count"], 4)
        self.assertEqual(
            item["peripheral_accesses"],
            [
                {
                    "pc_sw": 0x16,
                    "action": "store",
                    "address": 0x310CA300,
                    "peripheral": "PCG0_CTLC0",
                    "value": 0x1234,
                    "access_width": "normal-word",
                }
            ],
        )

    def test_breakpoint_captures_registers_and_watched_dm_before_execution(self):
        memory = loader_memory(
            loader_block(0, T.sw_to_byte(0x100), 2, payload=b"\x01\x00"),
            loader_block(1, 0x200, 4, payload=b"\x78\x56\x34\x12"),
        )
        states = T.trace(
            memory,
            None,
            0x100,
            sets={"I6": 0x261BE4},
            concrete_memory=True,
            assume_nw32=True,
            breakpoints=[0x100],
        )
        self.assertEqual(len(states), 1)
        self.assertEqual((states[0].stopped, states[0].steps), ("breakpoint", 0))
        summary = T.summarize(states, 0x100, [0x200])["states"][0]
        self.assertEqual(summary["stop_pc_sw"], 0x100)
        self.assertEqual(summary["registers"]["I6"], 0x261BE4)
        self.assertEqual(summary["watched_dm"]["0x200"], 0x12345678)

    def test_bounds_and_json_has_no_raw_bytes(self):
        data = b"\x00\x00"
        self.assertEqual(T.trace(data, 0, 0, max_steps=0)[0].stopped, "max-steps")
        with self.assertRaisesRegex(ValueError, "max_states must be between"):
            T.trace(data, 0, 0, max_states=0)
        with self.assertRaisesRegex(ValueError, "24-bit short-word"):
            T.trace(data, 0, 0, breakpoints=[0x1000000])
        branch = insn(
            "8a_abs",
            {"b": 0, "j": 0, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 20},
            6,
        )
        with patch("sharc_trace.decode_at", return_value=branch):
            self.assertIn(
                "max-states", [s.stopped for s in T.trace(b"", 0, 0, max_states=1)]
            )
        with tempfile.NamedTemporaryFile("wb", delete=False) as f:
            f.write(data)
            path = f.name
        try:
            for option, value, message in (
                ("--break-pc", "0x1000000", "24-bit short-word"),
                ("--watch-dm", "0x100000000", "32-bit address"),
            ):
                invalid = subprocess.run(
                    [
                        sys.executable,
                        "tools/sharc_trace.py",
                        path,
                        "--base-sw",
                        "0",
                        "--start",
                        "0",
                        option,
                        value,
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(invalid.returncode, 2)
                self.assertIn(message, invalid.stderr)
            out = subprocess.check_output(
                [
                    sys.executable,
                    "tools/sharc_trace.py",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--json",
                ]
            )
            self.assertNotIn("raw", out.decode())
            json.loads(out)
        finally:
            os.unlink(path)


class PokeDmTest(unittest.TestCase):
    """--poke-dm / --poke-dm-file: seed the concrete-memory overlay up front."""

    def make_program(self):
        # A single Type21c NOP is enough: max_steps=0 stops before it runs,
        # so only the seeded overlay (not any instruction) is under test.
        return loader_memory(
            loader_block(1, L.sw_to_byte(0x10), 2, payload=b"\x01\x00"),
        )

    def test_poke_dm_requires_concrete_memory(self):
        with self.assertRaisesRegex(ValueError, "requires --concrete-memory"):
            T.trace(self.make_program(), None, 0x10, max_steps=0, pokes={0x200: 1})

    def test_poke_dm_address_must_be_32_bit(self):
        with self.assertRaisesRegex(ValueError, "32-bit address"):
            T.trace(
                self.make_program(),
                None,
                0x10,
                max_steps=0,
                concrete_memory=True,
                assume_nw32=True,
                pokes={0x1_0000_0000: 1},
            )

    def test_poke_dm_that_cannot_land_raises(self):
        # Without --assume-32bit-normal-words a plain internal DM address
        # cannot take a 4-byte write, exactly like a real store instruction;
        # the poke must fail closed instead of silently doing nothing.
        with self.assertRaisesRegex(ValueError, "did not take effect"):
            T.trace(
                self.make_program(),
                None,
                0x10,
                max_steps=0,
                concrete_memory=True,
                pokes={0x200: 0x12345678},
            )

    def test_poke_dm_lands_in_overlay_and_is_readable(self):
        state = T.trace(
            self.make_program(),
            None,
            0x10,
            max_steps=0,
            concrete_memory=True,
            assume_nw32=True,
            pokes={0x200: 0x12345678, 0x204: 7},
        )[0]
        self.assertEqual(T._dm_read(state, 0x200, 4), T.Const(0x12345678))
        self.assertEqual(T._dm_read(state, 0x204, 4), T.Const(7))

    def test_poke_dm_at_external_address_needs_no_assume_nw32(self):
        state = T.trace(
            self.make_program(),
            None,
            0x10,
            max_steps=0,
            concrete_memory=True,
            pokes={0x310CA300: 0xDEADBEEF},
        )[0]
        self.assertEqual(T._dm_read(state, 0x310CA300, 4), T.Const(0xDEADBEEF))

    def test_poke_dm_negative_value_is_masked_to_32_bits(self):
        state = T.trace(
            self.make_program(),
            None,
            0x10,
            max_steps=0,
            concrete_memory=True,
            assume_nw32=True,
            pokes={0x200: -1},
        )[0]
        self.assertEqual(T._dm_read(state, 0x200, 4), T.Const(0xFFFFFFFF))

    def test_poke_dm_does_not_leak_into_a_sibling_path(self):
        # pokes seed the shared start state before the fork point, so every
        # forked path should see them -- but a later concrete write on one
        # path must still stay confined to that path's own overlay.
        states = T.trace(
            self.make_program(),
            None,
            0x10,
            max_steps=0,
            concrete_memory=True,
            assume_nw32=True,
            pokes={0x200: 42},
        )
        self.assertEqual(len(states), 1)
        left, right = T._copy(states[0]), T._copy(states[0])
        T._dm_write(left, 0x200, 4, T.Const(99))
        self.assertEqual(T._dm_read(left, 0x200, 4), T.Const(99))
        self.assertEqual(T._dm_read(right, 0x200, 4), T.Const(42))

    def _run_cli(self, program_path, *extra_args):
        return subprocess.run(
            [
                sys.executable,
                "tools/sharc_trace.py",
                program_path,
                "--blob",
                "--start",
                "0x10",
                "--max-steps",
                "0",
                "--json",
                *extra_args,
            ],
            capture_output=True,
            text=True,
        )

    def test_cli_poke_dm_and_poke_dm_file_round_trip(self):
        with tempfile.NamedTemporaryFile("wb", delete=False) as stream:
            stream.write(
                loader_block(1, L.sw_to_byte(0x10), 2, payload=b"\x01\x00")
            )
            stream.write(loader_block(1 << L.BFLAGS["FINAL"], 0, 0))
            program_path = stream.name
        poke_file = tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False
        )
        try:
            json.dump({"0x300000": 291, "0x310ca300": [1, 2, 3]}, poke_file)
            poke_file.close()
            missing_concrete = self._run_cli(
                program_path, "--poke-dm", "0x300000=1"
            )
            self.assertEqual(missing_concrete.returncode, 2)
            self.assertIn(
                "requires --concrete-memory", missing_concrete.stderr
            )
            landed = self._run_cli(
                program_path,
                "--concrete-memory",
                "--assume-32bit-normal-words",
                "--poke-dm-file",
                poke_file.name,
                "--poke-dm",
                "0x300004=999",
                "--watch-dm",
                "0x300000",
                "--watch-dm",
                "0x300004",
                "--watch-dm",
                "0x310ca300",
                "--watch-dm",
                "0x310ca304",
                "--watch-dm",
                "0x310ca308",
            )
            self.assertEqual(landed.returncode, 0, landed.stderr)
            watched = json.loads(landed.stdout)[0]["watched_dm"]
            self.assertEqual(watched["0x300000"], 291)
            self.assertEqual(watched["0x300004"], 999)
            self.assertEqual(watched["0x310ca300"], 1)
            self.assertEqual(watched["0x310ca304"], 2)
            self.assertEqual(watched["0x310ca308"], 3)
        finally:
            os.unlink(program_path)
            os.unlink(poke_file.name)


class ApproxRecipsCliTest(unittest.TestCase):
    """--approx-recips wiring: flag -> trace() -> State, end to end via the CLI."""

    def test_approx_recips_flag_parses_and_is_visible_only_when_requested(self):
        data = b"\x00\x00"  # a single Type21c NOP; nothing here executes recips
        with tempfile.NamedTemporaryFile("wb", delete=False) as f:
            f.write(data)
            path = f.name
        try:
            base_command = [
                sys.executable,
                "tools/sharc_trace.py",
                path,
                "--base-sw",
                "0",
                "--start",
                "0",
                "--json",
            ]
            without_flag = subprocess.run(
                base_command, capture_output=True, text=True
            )
            with_flag = subprocess.run(
                base_command + ["--approx-recips"], capture_output=True, text=True
            )
            self.assertEqual(without_flag.returncode, 0, without_flag.stderr)
            self.assertEqual(with_flag.returncode, 0, with_flag.stderr)
            without_state = json.loads(without_flag.stdout)[0]
            with_state = json.loads(with_flag.stdout)[0]
            # --approx-recips is usage-gated (state.approx_recips_used), not
            # request-gated: neither run executes recips, so neither JSON
            # carries the assumption, even the one that passed the flag.
            self.assertNotIn(
                "approximate RECIPS seed", without_state["assumptions"]
            )
            self.assertNotIn("approximate RECIPS seed", with_state["assumptions"])
            self.assertNotIn("Traceback", with_flag.stderr)
        finally:
            os.unlink(path)


def full_compute(cu, opcode, rn, rx, ry):
    """A full-compute field dict for _compute(f, short=False, ...)."""
    field = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def short_compute(opcode, rn, rx):
    """A short-compute field dict for _compute(f, short=True, ...)."""
    return {"compute[11:0]": (opcode << 8) | (rn << 4) | rx}


def dual_add_subtract_fields(float_form, rs, ra, rx, ry):
    """PRM Table 18-10 (p.433) / Table 18-13 (p.434): opcode[19:16]=0111
    (fixed) or 1111 (float); Rs[15:12], Ra[11:8], Rx[7:4], Ry[3:0]."""
    top_nibble = 0xF if float_form else 0x7
    field = (top_nibble << 16) | (rs << 12) | (ra << 8) | (rx << 4) | ry
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def mulalu_fields(category, rm, ra, rxm, rym, rxa, rya):
    """PRM Figure 18-1 mf bit (p.423) + Table 18-16/18-17 (p.434): mf=1,
    opcode[21:16]=CATEGORY, then Rm[15:12]/Ra[11:8]/Rxm[7:6]/Rym[5:4]/
    Rxa[3:2]/Rya[1:0] (Rxm/Rym/Rxa/Rya are each a 2-bit code within their
    fixed quad: R/F 0-3, 4-7, 8-11, 12-15 respectively)."""
    field = (
        (1 << 22)
        | (category << 16)
        | (rm << 12)
        | (ra << 8)
        | (rxm << 6)
        | (rym << 4)
        | (rxa << 2)
        | rya
    )
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def f32(value):
    """The IEEE-754 single-precision bit pattern for a Python float."""
    return struct.unpack("<I", struct.pack("<f", value))[0]


def shiftimm_fields(opcode, data8, rn, rx, dataex=0):
    """A ShiftImm field dict for _shift_immediate."""
    field = (opcode << 16) | (data8 << 8) | (rn << 4) | rx
    return {
        "shiftimm[22:16]": field >> 16,
        "shiftimm[15:0]": field & 0xFFFF,
        "dataex[3:0]": dataex,
    }


class AstatxFlagsTest(unittest.TestCase):
    """Focused tests for each op's ASTATX flags and the LT/GE/LE/GT/etc.
    condition predicates, from the verified per-instruction PRM table
    (docs/findings/-style citations inline)."""

    def astatx_after(self, fields, short, values, old_astatx, special=None):
        rn, value, operation, update = T._compute(fields, short, values, special)
        return rn, value, operation, update(old_astatx)

    # -- add/subtract/increment/decrement (PRM pp.439-440, 446-447) --------

    def test_add_flags_plain_no_carry_no_overflow(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "add")
        self.assertEqual(value, T.Const(12))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 0))

    def test_add_flags_carry_and_zero(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(0xFFFFFFFF), 2: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_add_flags_signed_overflow(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(0x7FFFFFFF), 2: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x80000000))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AV_BIT) | (1 << T.AN_BIT))
        )

    def test_subtract_flags_negative_result_no_carry(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x02, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFE))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))

    def test_subtract_flags_equal_sets_az_and_ac(self):
        _, _, _, astatx = self.astatx_after(
            full_compute(0, 0x02, 0, 1, 2),
            False,
            {1: T.Const(7), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_increment_matches_add_by_one_flags(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x29, 0, 1, 0),
            False,
            {1: T.Const(0x7FFFFFFF)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "increment")
        self.assertEqual(value, T.Const(0x80000000))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AV_BIT) | (1 << T.AN_BIT))
        )

    def test_decrement_is_rx_minus_1_flags(self):
        # RX=0: 0 - 1 = -1, AN set, no carry (borrow), no AZ.
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x2A, 0, 1, 0), False, {1: T.Const(0)}, T.Unknown("start")
        )
        self.assertEqual(op, "decrement")
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))
        # RX=1: 1 - 1 = 0, AZ and AC (no borrow) set.
        _, _, _, astatx = self.astatx_after(
            full_compute(0, 0x2A, 0, 1, 0), False, {1: T.Const(1)}, T.Unknown("start")
        )
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_arith_flags_unknown_when_operand_not_const(self):
        old = T.Const(0xFFFFFFFF)
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.symbol("x"), 2: T.Const(1)},
            old,
        )
        self.assertIsInstance(value, T.Affine)
        # Only the ALU-flags bits are forgotten; everything else in OLD
        # (here, all 1s) stays known at its old value.
        kept = 0xFFFFFFFF & ~T.ALU_FLAGS_MASK
        self.assertEqual(astatx, T.PartialConst(kept, kept))

    # -- pass/not/and/or/xor (PRM pp.449-452) --------------------------------

    def test_pass_not_and_or_xor_flags_from_result(self):
        cases = (
            (0x21, 1, 2, {1: T.Const(0)}, T.Const(0), 1 << T.AZ_BIT),  # pass
            (0x40, 1, 2, {1: T.Const(5), 2: T.Const(3)}, T.Const(1), 0),  # and
            (
                0x41,
                1,
                2,
                {1: T.Const(0x80000000), 2: T.Const(0)},
                T.Const(0x80000000),
                1 << T.AN_BIT,
            ),  # or
            (
                0x42,
                1,
                2,
                {1: T.Const(5), 2: T.Const(5)},
                T.Const(0),
                1 << T.AZ_BIT,
            ),  # xor
        )
        for opcode, rx, ry, values, expected_value, expected_bits in cases:
            with self.subTest(opcode=hex(opcode)):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, opcode, 0, rx, ry),
                    False,
                    values,
                    T.Const(0xFFFFFFFF),
                )
                self.assertEqual(value, expected_value)
                self.assertEqual(
                    astatx,
                    T.Const((0xFFFFFFFF & ~T.ALU_FLAGS_MASK) | expected_bits),
                )

    def test_not_flags_unknown_operand_forgets_only_alu_bits(self):
        old = T.Const(0)  # every bit known, all clear
        _, value, op, astatx = self.astatx_after(
            short_compute(4, 0, 1), True, {1: T.Unknown("uninit")}, old
        )
        self.assertEqual(op, "not")
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(astatx, T.PartialConst(0xFFFFFFFF & ~T.ALU_FLAGS_MASK, 0))
        self.assertIsNone(T._astatx_known_bit(astatx, T.AZ_BIT))
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), False)

    # -- comp/compu (PRM pp.18-5,18-6; CACC shift) ---------------------------

    def test_compare_signed_sets_an_when_less_and_clears_af(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x0A, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "compare")
        preserve = 0x00FFFFC0 & ~(1 << T.AF_BIT)
        cacc = (0xFFFFFFFF >> 1) & 0x7F000000  # CACC (bits 31:24) shifts in from old
        expected = (0xFFFFFFFF & preserve) | cacc | (1 << T.AN_BIT)
        self.assertEqual(astatx, T.Const(expected))
        self.assertFalse(bool(expected & (1 << T.AF_BIT)))

    def test_compare_cacc_shifts_and_new_msb_enters_bit31(self):
        # Old CACC (bits 31:24) = 0b10101010; compu(7,5) -> x>y -> new MSB=1.
        old = T.Const(0b10101010 << 24)
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x0B, 0, 1, 2), False, {1: T.Const(7), 2: T.Const(5)}, old
        )
        expected_cacc = (0b10101010 >> 1) | 0b10000000
        self.assertEqual((astatx.value >> 24) & 0xFF, expected_cacc)

    def test_compare_partial_when_old_astatx_not_fully_known(self):
        # Old ASTATX unknown entirely: AZ/AN/AC/AV/AS/AI/AF become known (the
        # compare's own bits), but CACC cannot be shifted without the old
        # CACC bits, so it stays unknown.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x0A, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))
        self.assertIsNone(T._astatx_known_bit(astatx, 31))

    # -- multiplier ops (PRM p.493 for mr-data-move) -------------------------

    def test_multiply_family_forgets_multiplier_flags(self):
        old = T.Const(0xFFFFFFFF)
        for opcode, cu in ((0x70, 1),):
            _, _, op, astatx = self.astatx_after(
                full_compute(cu, opcode, 0, 1, 2),
                False,
                {1: T.Const(3), 2: T.Const(4)},
                old,
            )
            self.assertEqual(op, "multiply")
            self.assertEqual(
                astatx,
                T.PartialConst(
                    0xFFFFFFFF & ~T.MULT_FLAGS_MASK, 0xFFFFFFFF & ~T.MULT_FLAGS_MASK
                ),
            )

    def test_mr_data_move_clears_multiplier_flags(self):
        field = (0b100000 << 17) | (1 << 16) | (0 << 12) | (3 << 8)
        fields = {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}
        _, _, op, astatx = self.astatx_after(
            fields, False, {3: T.Const(5)}, T.Const(0xFFFFFFFF)
        )
        self.assertEqual(op, "mr-data-move")
        self.assertEqual(astatx, T.Const(0xFFFFFFFF & ~T.MULT_FLAGS_MASK))

    # -- leftz (PRM p.521) ----------------------------------------------------

    def test_leftz_flags(self):
        cases = (
            (0, 32, False, True),
            (1, 31, False, False),
            (0x80000000, 0, True, False),
        )
        for rx_value, expected_result, expected_sz, expected_sv in cases:
            with self.subTest(rx=hex(rx_value)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(2, 0x88, 0, 1, 0),
                    False,
                    {1: T.Const(rx_value)},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "leftz")
                self.assertEqual(value, T.Const(expected_result))
                self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
                self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), expected_sz)
                self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), expected_sv)

    # -- btst reg (PRM p.513) -------------------------------------------------

    def test_btst_flags_tested_bit_and_out_of_range(self):
        # Bit 0 of 0b1 is 1 -> SZ cleared.
        _, _, op, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0b1), 2: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "bit-test")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # Bit 1 of 0b1 is 0 -> SZ set.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0b1), 2: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

        # Position 32 is out of range -> SZ and SV both set.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(32)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    def test_btst_sv_known_even_when_source_unknown(self):
        # SV only needs the position, so it is known even when the tested
        # register is not; SZ needs the source too, so it stays unknown.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Unknown("uninit"), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertIsNone(T._astatx_known_bit(astatx, T.SZ_BIT))

    # -- bset/bclr/btgl reg and immediate (PRM pp.511-513) -------------------

    def test_bit_field_reg_flags(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(2, 0xC0, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "bit-set")
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # Position > 31: value passes through unchanged, SV set, SZ from the
        # (unchanged) value.
        _, value, _, astatx = self.astatx_after(
            full_compute(2, 0xC4, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(99)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    def test_bit_set_clear_toggle_immediate_flags(self):
        # bset RX by 0 -> output nonzero, SZ cleared, SV cleared (0 <= 31).
        rn, value, op, astatx4 = T._shift_immediate(
            shiftimm_fields(0x30, 0, 0, 1), {1: T.Const(0)}
        )
        update = astatx4
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "bit-set-immediate")
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # bclr position 40 (> 31): passthrough, SV set.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x31, 40, 0, 1), {1: T.Const(0xFF)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(value, T.Const(0xFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    # -- fext immediate (PRM pp.518-519) -------------------------------------

    def test_fext_immediate_sv_set_when_span_exceeds_32(self):
        # position=30, length=8 (encoded via dataex/data8) -> span=38 > 32.
        position, length = 30, 8
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x10, data8, 0, 1, dataex), {1: T.Const(0xFFFFFFFF)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "field-extract-immediate")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)

    def test_fext_immediate_sv_clear_when_span_within_32(self):
        position, length = 0, 8
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x10, data8, 0, 1, dataex), {1: T.Const(0)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(value, T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    # -- lshift/ashift reg and immediate, OR-forms (PRM pp.508-510) ----------

    def test_logical_shift_reg_flags(self):
        # RY encodes amount=4 in its low byte; RX=1 -> shifted=16, SV set
        # (left shift), SZ from the shifted value (nonzero).
        rn, value, op, update = T._compute(
            full_compute(2, 0x00, 0, 1, 2), False, {1: T.Const(1), 2: T.Const(4)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "logical-shift")
        self.assertEqual(value, T.Const(16))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)

    def test_shift_immediate_or_forms_sz_uses_pre_or_value(self):
        # RN already has bit0 set; OR-lshift RX=0b10 by 1 -> shifted=0b100
        # (nonzero) even though it is OR'd into a nonzero RN. SZ must reflect
        # the shifted value alone, per the PRM text.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x08, 1, 0, 1), {0: T.Const(1), 1: T.Const(0b10)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "logical-shift-or-immediate")
        self.assertEqual(value, T.Const(0b101))  # 1 | (0b10 << 1)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)

    def test_or_ashift_immediate_leaves_ss_unknown(self):
        # PRM p.510: OR-ashift's ASTATx/y block omits the SS line (its
        # OR-lshift sibling repeats "SS Cleared"); model that gap as SS
        # becoming unknown rather than guessing it is cleared.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x09, 1, 0, 1), {0: T.Const(0), 1: T.Const(1)}
        )
        astatx = update(T.Const(0xFFFFFFFF))  # SS previously known (=1)
        self.assertEqual(op, "arithmetic-shift-or-immediate")
        self.assertIsNone(T._astatx_known_bit(astatx, T.SS_BIT))
        # SZ/SV are still defined for OR-ashift.
        self.assertIsNotNone(T._astatx_known_bit(astatx, T.SZ_BIT))
        self.assertIsNotNone(T._astatx_known_bit(astatx, T.SV_BIT))

    def test_ashift_immediate_plain_still_clears_ss(self):
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x01, 1, 0, 1), {1: T.Const(1)}
        )
        astatx = update(T.Const(0xFFFFFFFF))
        self.assertEqual(op, "arithmetic-shift-immediate")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)


class PredicateTruthTableTest(unittest.TestCase):
    """LT/GE/LE/GT truth tables (PGR Table 4-37 p.4-93 / PRM p.4-53)."""

    def astatx_state(self, af, an, az, av=None):
        bits = 0
        mask = (1 << T.AF_BIT) | (1 << T.AN_BIT) | (1 << T.AZ_BIT)
        if af:
            bits |= 1 << T.AF_BIT
        if an:
            bits |= 1 << T.AN_BIT
        if az:
            bits |= 1 << T.AZ_BIT
        if av is not None:
            mask |= 1 << T.AV_BIT
            if av:
                bits |= 1 << T.AV_BIT
        return T.PartialConst(mask, bits)

    def test_af0_alusat0_truth_table(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        mode1_code = T.UREG_CODES["MODE1"]
        # AF=0, ALUSAT=0: cross = AN xor AV. Enumerate AN, AV, AZ.
        for an in (False, True):
            for av in (False, True):
                for az in (False, True):
                    state = T.State(
                        0,
                        {
                            astatx_code: self.astatx_state(False, an, az, av),
                            mode1_code: T.Const(0),
                        },
                    )
                    cross = an != av
                    expect_lt = cross
                    expect_le = cross or az
                    with self.subTest(an=an, av=av, az=az):
                        self.assertEqual(T._predicate(state, 0x01), expect_lt)
                        self.assertEqual(T._predicate(state, 0x11), not expect_lt)
                        self.assertEqual(T._predicate(state, 0x02), expect_le)
                        self.assertEqual(T._predicate(state, 0x12), not expect_le)

    def test_af0_alusat1_flips_the_av_term(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        mode1_code = T.UREG_CODES["MODE1"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=False, az=False, av=True),
                mode1_code: T.Const(1 << T.ALUSAT_BIT),
            },
        )
        # AV and not ALUSAT = False (ALUSAT set) -> cross = AN xor False = AN = False.
        self.assertFalse(T._predicate(state, 0x01))  # LT
        self.assertTrue(T._predicate(state, 0x11))  # GE

    def test_af0_av_unknown_returns_none(self):
        # AV itself must be known to answer LT/GE/LE/GT at all.
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=True, az=False, av=None),
                T.UREG_CODES["MODE1"]: T.Unknown("mode1"),
            },
        )
        self.assertIsNone(T._predicate(state, 0x01))

    def test_af0_av_false_does_not_need_alusat(self):
        # AV and not ALUSAT is 0 regardless of ALUSAT when AV=0, so MODE1
        # need not be known to resolve the predicate.
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=True, az=False, av=False),
                T.UREG_CODES["MODE1"]: T.Unknown("mode1"),
            },
        )
        self.assertTrue(T._predicate(state, 0x01))  # LT: cross = AN xor 0 = True
        self.assertFalse(T._predicate(state, 0x11))  # GE

    def test_af1_truth_table(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        for an in (False, True):
            for az in (False, True):
                state = T.State(0, {astatx_code: self.astatx_state(True, an, az)})
                expect_le = an or az
                expect_lt = an and not az
                with self.subTest(an=an, az=az):
                    self.assertEqual(T._predicate(state, 0x02), expect_le)
                    self.assertEqual(T._predicate(state, 0x12), not expect_le)
                    self.assertEqual(T._predicate(state, 0x01), expect_lt)
                    self.assertEqual(T._predicate(state, 0x11), not expect_lt)

    def test_unknown_bits_return_none(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(0, {astatx_code: T.Unknown("uninitialized")})
        for cond in (0x01, 0x02, 0x11, 0x12):
            self.assertIsNone(T._predicate(state, cond))

    def test_simple_single_bit_conditions(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        for cond, bit in T.SIMPLE_COND_BITS.items():
            bit_pos, negate = bit
            with self.subTest(cond=hex(cond)):
                state_true = T.State(
                    0, {astatx_code: T.PartialConst(1 << bit_pos, 1 << bit_pos)}
                )
                state_false = T.State(0, {astatx_code: T.PartialConst(1 << bit_pos, 0)})
                self.assertEqual(T._predicate(state_true, cond), not negate)
                self.assertEqual(T._predicate(state_false, cond), negate)


class FloatComputeTest(unittest.TestCase):
    """Float ALU/multiplier/multifunction compute forms (PRM Table 18-5
    p.425-427, Table 18-7 p.428-429, Table 18-10 p.433, and the
    multi_function_selector p.423), cross-checked against the classic PGR's
    per-instruction pages for exact flag formulas the SHARC+ PRM only marks
    '*'/data-dependent."""

    def astatx_after(self, fields, values, old_astatx):
        rn, value, operation, update = T._compute(fields, False, values)
        return rn, value, operation, update(old_astatx)

    # -- Fn = Fx + Fy / Fx - Fy (PGR p.11-24/11-25) --------------------------

    def test_float_add(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-add")
        self.assertEqual(value, T.Const(f32(4.0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AC_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_float_subtract_negative_result(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x82, 0, 1, 2),
            {1: T.Const(f32(1.0)), 2: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-subtract")
        self.assertEqual(value, T.Const(f32(-1.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_float_add_overflow_rounds_to_infinity_and_sets_av(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(3.4e38)), 2: T.Const(f32(3.4e38))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))  # +infinity
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_float_add_nan_input_returns_all_ones_and_sets_ai(self):
        # PGR p.11-24: "A NAN input returns an all 1s result."
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(f32(1.0))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = -Fx / Fn = abs Fx (PGR p.11-30/11-31) --------------------------

    def test_float_negate(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xA2, 0, 1, 0),
            {1: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-negate")
        self.assertEqual(value, T.Const(f32(-2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_float_abs_an_fixed_zero_as_from_input_sign(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xB0, 0, 1, 0),
            {1: T.Const(f32(-3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-abs")
        self.assertEqual(value, T.Const(f32(3.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), True)
        _, _, _, astatx_pos = self.astatx_after(
            full_compute(0, 0xB0, 0, 1, 0),
            {1: T.Const(f32(3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx_pos, T.AS_BIT), False)

    # -- Fn = rnd Fx (PRM Table 18-5 opcode 0xA5, p.20-8; PGR Table 12-4 --
    # opcode 0xA5, pp.12-3/12-4, and p.11-33) --------------------------------

    def test_rnd_passes_through_an_already_32bit_finite_value(self):
        # Every UREG this tracer tracks is already stored at IEEE-754
        # single precision (see the module's "Floating-point compute
        # support" header comment), so re-rounding a finite, normal,
        # already-32-bit value to the same boundary is a no-op.
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-round32")
        self.assertEqual(value, T.Const(f32(2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AC_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_rnd_negative_result_sets_an(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(-2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(f32(-2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_rnd_denormal_input_flushes_to_signed_zero(self):
        for source, expected in ((1e-40, 0), (-1e-40, 0x80000000)):
            with self.subTest(source=source):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, 0xA5, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(value, T.Const(expected))
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_rnd_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(float("nan")))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_rnd_infinity_input_passes_through_without_setting_av(self):
        # The "post-rounded overflow" corner this op documents only arises
        # from rounding away extended-precision mantissa bits this tracer
        # never carries between ops; passing an already-infinite value
        # through must not fabricate an overflow.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(float("inf")))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    # -- Rn = mant Fx (PRM Table 18-5 p.427, opcode 0xAD; PGR p.11-34/11-35) -

    def test_mant_extracts_hidden_bit_and_fraction_left_justified(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "mant")
        # 3.5 = 1.75 * 2^1; hidden bit + 23-bit fraction of 1.75 (0x600000)
        # left-justified into a 32-bit 1.31 word.
        self.assertEqual(value, T.Const(0xE0000000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_mant_negative_input_sets_as_not_an(self):
        # PGR p.11-34: "unsigned-magnitude result" -- AS carries the
        # input's sign, AN stays fixed 0 even though the input is negative.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(-3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xE0000000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), True)

    def test_mant_zero_and_denormal_inputs_flush_to_zero_mantissa(self):
        for source, label in ((0.0, "zero"), (1e-40, "denormal")):
            with self.subTest(label=label):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, 0xAD, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(value, T.Const(0))
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_mant_nan_and_infinity_both_return_all_ones(self):
        # PGR p.11-34: "A NAN or an infinity input returns an all 1s
        # result" -- unlike the arithmetic float ALU ops, MANT overrides
        # infinity too, not just NAN.
        _, nan_value, _, nan_astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(float("nan")))},
            T.Unknown("start"),
        )
        self.assertEqual(nan_value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(nan_astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(nan_astatx, T.AV_BIT), False)
        _, inf_value, _, inf_astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(float("inf")))},
            T.Unknown("start"),
        )
        self.assertEqual(inf_value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(inf_astatx, T.AV_BIT), True)
        self.assertEqual(T._astatx_known_bit(inf_astatx, T.AI_BIT), False)

    # -- Fn = scalb Fx by Ry (PRM Table 18-5 p.427, opcode 0xBD; PGR p.11-33) -

    def test_scalb_scales_the_exponent_by_the_fixed_point_integer(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-scalb")
        self.assertEqual(value, T.Const(f32(12.0)))  # 1.5 * 2**3
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_scalb_negative_shift_scales_down(self):
        _, value, _, _ = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(12.0)), 2: T.Const(0xFFFFFFFD)},  # Ry = -3
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(f32(1.5)))

    def test_scalb_overflow_rounds_to_infinity_and_sets_av(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(3.0e38)), 2: T.Const(10)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))  # +infinity
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_scalb_underflow_flushes_to_zero_not_subnormal(self):
        # PGR p.11-33: "Denormal returns +-zero" -- an explicit override of
        # the ordinary IEEE subnormal result struct would otherwise round to.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(-1.0)), 2: T.Const(0xFFFFFF78)},  # Ry = -136
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x80000000))  # -0.0
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_scalb_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = pass Fx (PGR p.11-32) ------------------------------------------

    def test_float_pass_roundtrips_value(self):
        _, value, op, _ = self.astatx_after(
            full_compute(0, 0xA1, 0, 1, 0),
            {1: T.Const(f32(-7.25))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-pass")
        self.assertEqual(value, T.Const(f32(-7.25)))

    # -- Fn = min/max(Fx, Fy) (PGR p.11-46/11-47) ----------------------------

    def test_float_min_max(self):
        values = {1: T.Const(f32(3.0)), 2: T.Const(f32(-1.0))}
        _, min_value, min_op, _ = self.astatx_after(
            full_compute(0, 0xE1, 0, 1, 2), values, T.Unknown("start")
        )
        _, max_value, max_op, _ = self.astatx_after(
            full_compute(0, 0xE2, 0, 1, 2), values, T.Unknown("start")
        )
        self.assertEqual((min_op, min_value), ("float-min", T.Const(f32(-1.0))))
        self.assertEqual((max_op, max_value), ("float-max", T.Const(f32(3.0))))

    # -- Rn = min/max(Rx, Ry) (PRM Table 18-5 p.425, opcode 0x61/0x62; -------
    # -- PGR p.11-20/11-21, fixed-point, distinct from the float form above) -

    def test_fixed_min_max_pick_the_signed_extreme(self):
        values = {1: T.Const(3), 2: T.Const(0xFFFFFFFF)}  # Rx=3, Ry=-1
        _, min_value, min_op, min_astatx = self.astatx_after(
            full_compute(0, 0x61, 0, 1, 2), values, T.Unknown("start")
        )
        _, max_value, max_op, _ = self.astatx_after(
            full_compute(0, 0x62, 0, 1, 2), values, T.Unknown("start")
        )
        self.assertEqual((min_op, min_value), ("min", T.Const(0xFFFFFFFF)))
        self.assertEqual((max_op, max_value), ("max", T.Const(3)))
        # PRM Table 3-2 ("AF Flag = 0"): AV/AC/AS/AI fixed 0; AZ/AN from
        # the chosen (negative) result.
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AI_BIT), False)

    def test_fixed_min_max_unknown_operand_forgets_the_result(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0x61, 0, 1, 2),
            {1: T.Unknown("uninitialized R1"), 2: T.Const(5)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "min")
        self.assertIsInstance(value, T.Unknown)
        self.assertIsNone(T._astatx_known_bit(astatx, T.AZ_BIT))
        self.assertIsNone(T._astatx_known_bit(astatx, T.AN_BIT))

    # -- Fn = clip Fx by Fy (PGR p.11-48) ------------------------------------

    def test_float_clip(self):
        for fx, fy, expected, label in (
            (1.0, 5.0, 1.0, "within range passes through"),
            (7.0, 5.0, 5.0, "positive out-of-range clips to +|Fy|"),
            (-7.0, 5.0, -5.0, "negative out-of-range clips to -|Fy|"),
        ):
            with self.subTest(label=label):
                _, value, op, _ = self.astatx_after(
                    full_compute(0, 0xE3, 0, 1, 2),
                    {1: T.Const(f32(fx)), 2: T.Const(f32(fy))},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "float-clip")
                self.assertEqual(value, T.Const(f32(expected)))

    # -- comp(Fx, Fy) (PGR p.11-29) -------------------------------------------

    def test_float_compare_equal_sets_az(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x8A, 0, 1, 2),
            {1: T.Const(f32(2.0)), 2: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-compare")
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_float_compare_nan_is_unordered_and_sets_ai(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x8A, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = float Rx / Rn = trunc Fx (PRM p.427, PGR p.11-37/11-39) -------

    def test_float_convert_from_signed_int(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xCA, 0, 1, 0),
            {1: T.Const(0xFFFFFFFD)},  # -3 two's complement
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-convert")
        self.assertEqual(value, T.Const(f32(-3.0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_trunc_toward_zero_both_signs(self):
        for source, expected in ((3.7, 3), (-3.7, -3)):
            with self.subTest(source=source):
                _, value, op, _ = self.astatx_after(
                    full_compute(0, 0xCD, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "trunc")
                self.assertEqual(value, T.Const(expected & 0xFFFFFFFF))

    def test_trunc_overflow_saturates_when_alusat_set(self):
        mode1 = T.Const(1 << T.ALUSAT_BIT)
        result = T._float_to_fixed_trunc(T.Const(f32(1e10)), mode1, "trunc F1")
        self.assertEqual(result, (T.Const(0x7FFFFFFF), True, False))

    def test_trunc_overflow_unknown_when_alusat_clear(self):
        mode1 = T.Const(0)
        result = T._float_to_fixed_trunc(T.Const(f32(1e10)), mode1, "trunc F1")
        value, overflow, invalid = result
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(overflow, True)

    # -- Fn = recips/rsqrts Fx seeds (PRM p.427, PGR p.11-44/11-45) ---------

    def test_recips_rsqrts_decode_without_a_numeric_seed(self):
        for opcode, name in ((0xC4, "float-recips-seed"), (0xC5, "float-rsqrts-seed")):
            with self.subTest(opcode=hex(opcode)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(0, opcode, 0, 1, 0),
                    {1: T.Const(f32(2.0))},
                    T.Unknown("start"),
                )
                self.assertEqual(op, name)
                self.assertIsInstance(value, T.Unknown)
                self.assertIsNone(T._astatx_known_bit(astatx, T.AV_BIT))

    # -- --approx-recips: opt-in documented-formula seed approximation ------

    def approx_after(self, opcode, rx_value, old_astatx=None):
        rn, value, operation, update = T._compute(
            full_compute(0, opcode, 0, 1, 0),
            False,
            {1: rx_value},
            approx_recips=True,
        )
        return rn, value, operation, update(old_astatx or T.Unknown("start"))

    def test_approx_recips_default_off_still_unknown(self):
        # No approx_recips kwarg at all (the CLI's default): unchanged from
        # test_recips_rsqrts_decode_without_a_numeric_seed above.
        _, value, op, _ = self.astatx_after(
            full_compute(0, 0xC4, 0, 1, 0),
            {1: T.Const(f32(4.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-recips-seed")
        self.assertIsInstance(value, T.Unknown)

    def test_approx_recips_ordinary_positive_value_within_documented_accuracy(self):
        _, value, op, astatx = self.approx_after(0xC4, T.Const(f32(4.0)))
        self.assertEqual(op, "float-recips-seed-approx")
        approx = struct.unpack("<f", struct.pack("<I", value.value))[0]
        # PRM p.19-17: "an 8-bit accurate seed" -- require the relative
        # error against the true reciprocal to be within that bound.
        self.assertLess(abs(approx - 0.25) / 0.25, 2**-8)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_approx_recips_negative_value_sets_an_and_keeps_sign(self):
        _, value, op, astatx = self.approx_after(0xC4, T.Const(f32(-4.0)))
        approx = struct.unpack("<f", struct.pack("<I", value.value))[0]
        self.assertLess(approx, 0.0)
        self.assertLess(abs(approx - (-0.25)) / 0.25, 2**-8)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_approx_recips_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, op, astatx = self.approx_after(0xC4, T.Const(0x7FC00000))
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    def test_approx_recips_zero_input_returns_signed_infinity_and_sets_av(self):
        for zero_bits, expected in ((0x00000000, 0x7F800000), (0x80000000, 0xFF800000)):
            with self.subTest(zero_bits=hex(zero_bits)):
                _, value, _, astatx = self.approx_after(0xC4, T.Const(zero_bits))
                self.assertEqual(value, T.Const(expected))
                self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_approx_recips_denormal_input_is_flushed_to_zero_first(self):
        # PRM p.417-418 (IEEE-754 compatibility bullet, general to every
        # computational unit): "Denormal operands ... flush to zero when
        # input to a computational unit." A flushed +denormal therefore
        # takes recips's own +-zero -> +-infinity path.
        _, value, _, astatx = self.approx_after(0xC4, T.Const(0x00000001))
        self.assertEqual(value, T.Const(0x7F800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_approx_recips_large_exponent_underflows_to_signed_zero(self):
        # PRM p.19-17: "If the unbiased exponent of Fx is greater than
        # +125, the result is +-zero." biased 253 -> unbiased 126 > 125.
        _, value, _, astatx = self.approx_after(0xC4, T.Const((253 << 23) | (1 << 31)))
        self.assertEqual(value, T.Const(1 << 31))  # -zero
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_approx_recips_symbolic_input_stays_unknown_even_when_enabled(self):
        _, value, op, _ = self.approx_after(0xC4, T.Unknown("uninitialized R1"))
        self.assertEqual(op, "float-recips-seed-approx")
        self.assertIsInstance(value, T.Unknown)

    def test_approx_recips_does_not_extend_to_rsqrts(self):
        # Decision: rsqrts's seed exponent rule is not a trivial mirror of
        # recips's, so --approx-recips leaves it exactly as undocumented.
        _, value, op, _ = self.approx_after(0xC5, T.Const(f32(4.0)))
        self.assertEqual(op, "float-rsqrts-seed")
        self.assertIsInstance(value, T.Unknown)

    # -- Fn = Fx * Fy (PRM Table 18-7 p.428-429, PGR p.11-57) ----------------

    def test_float_multiply(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(1, 0x30, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(f32(2.0))},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "float-multiply")
        self.assertEqual(value, T.Const(f32(3.0)))
        # Multiplier result flags (MN/MV/MU/MI) are unmodeled -> forgotten,
        # same convention the fixed-point multiply already uses.
        for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
            self.assertIsNone(T._astatx_known_bit(astatx, bit))

    # -- Dual Add/Subtract: Ra = Rx+Ry, Rs = Rx-Ry (PRM Table 18-10 p.433) --

    def test_dual_add_subtract_fixed_writes_both_registers(self):
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        rn, value, op, astatx = self.astatx_after(
            fields, {1: T.Const(10), 2: T.Const(3)}, T.Unknown("start")
        )
        self.assertEqual(op, "dual-add-subtract")
        self.assertEqual(rn, (0, 3))
        self.assertEqual(value, (T.Const(13), T.Const(7)))

    def test_dual_add_subtract_ors_an_from_the_subtract_half(self):
        # Rx=3, Ry=10: Ra=Rx+Ry=13 (AN clear), Rs=Rx-Ry=-7 (AN set). A naive
        # "flags follow Ra only" implementation would miss AN; PRM
        # p.3-21/3-22 says both halves' ALU flags are ORed together.
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        _, value, _, astatx = self.astatx_after(
            fields, {1: T.Const(3), 2: T.Const(10)}, T.Unknown("start")
        )
        self.assertEqual(value, (T.Const(13), T.Const(0xFFFFFFF9)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_dual_add_subtract_ors_az_from_the_add_half(self):
        # Rx=5, Ry=-5: Ra=Rx+Ry=0 (AZ set), Rs=Rx-Ry=10 (AZ clear).
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        _, value, _, astatx = self.astatx_after(
            fields, {1: T.Const(5), 2: T.Const(0xFFFFFFFB)}, T.Unknown("start")
        )
        self.assertEqual(value, (T.Const(0), T.Const(10)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_dual_add_subtract_float_ors_an_from_the_subtract_half(self):
        fields = dual_add_subtract_fields(True, rs=3, ra=0, rx=1, ry=2)
        rn, value, op, astatx = self.astatx_after(
            fields,
            {1: T.Const(f32(1.0)), 2: T.Const(f32(3.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-dual-add-subtract")
        self.assertEqual(rn, (0, 3))
        self.assertEqual(value, (T.Const(f32(4.0)), T.Const(f32(-2.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    # -- MUL/ALU multifunction: Fm=Fxm*Fym, Fa=Fxa+-Fya (PGR Table 12-12) ---

    def test_mulalu_add_variant_writes_multiply_and_add_results(self):
        fields = mulalu_fields(0x18, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        values = {
            0: T.Const(f32(2.0)),  # Rxm (R0-3 quad, code 0)
            4: T.Const(f32(3.0)),  # Rym (R4-7 quad, code 0)
            8: T.Const(f32(5.0)),  # Rxa (R8-11 quad, code 0)
            12: T.Const(f32(1.0)),  # Rya (R12-15 quad, code 0)
        }
        rn, value, op, astatx = T._compute(fields, False, values)
        astatx = astatx(T.Unknown("start"))
        self.assertEqual(op, "float-mulalu-add")
        self.assertEqual(rn, (1, 2))
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(6.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)
        self.assertIsNone(T._astatx_known_bit(astatx, T.MN_BIT))

    def test_mulalu_subtract_variant(self):
        fields = mulalu_fields(0x19, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(5.0)),
            12: T.Const(f32(1.0)),
        }
        rn, value, op, _ = T._compute(fields, False, values)
        self.assertEqual(op, "float-mulalu-subtract")
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(4.0))))

    def test_unsupported_multifunction_category_raises(self):
        # category=0x04 (SSFR fixed-point RM=R3-0*R7-4, RA=RXA+RYA, PGR
        # Table 12-12) is a real MUL/ALU multifunction row this tracer does
        # not implement (only the float rows 0x18/0x19 are); it must still
        # raise rather than being silently misparsed as something else.
        fields = mulalu_fields(0x04, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        with self.assertRaises(ValueError) as raised:
            T._compute(fields, False, {})
        self.assertIn("multifunction category", str(raised.exception))

    # -- ShortCompute float rows (PRM Table 18-2 p.423-425, opcodes 8/9/A/B/F) -

    def short_astatx_after(self, fields, values, old_astatx):
        rn, value, operation, update = T._compute(fields, True, values)
        return rn, value, operation, update(old_astatx)

    def test_short_float_add_and_subtract(self):
        # RN doubles as the Y input and result (Table 18-22): FN = FN +- FX.
        values = {0: T.Const(f32(1.5)), 1: T.Const(f32(2.5))}
        _, add_value, add_op, _ = self.short_astatx_after(
            short_compute(0x8, 0, 1), values, T.Unknown("start")
        )
        _, sub_value, sub_op, _ = self.short_astatx_after(
            short_compute(0x9, 0, 1), values, T.Unknown("start")
        )
        self.assertEqual((add_op, add_value), ("float-add", T.Const(f32(4.0))))
        self.assertEqual((sub_op, sub_value), ("float-subtract", T.Const(f32(-1.0))))

    def test_short_float_convert_reads_only_rx(self):
        # FN = float RX: RN is the destination only, not a second input.
        rn, value, op, _ = self.short_astatx_after(
            short_compute(0xA, 0, 1),
            {0: T.Const(0xDEAD), 1: T.Const(0xFFFFFFFD)},  # RX=-3
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-convert")
        self.assertEqual(value, T.Const(f32(-3.0)))

    def test_short_float_compare_is_status_only(self):
        rn, value, op, astatx = self.short_astatx_after(
            short_compute(0xB, 0, 1),
            {0: T.Const(f32(2.0)), 1: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-compare")
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_short_float_multiply(self):
        rn, value, op, astatx = self.short_astatx_after(
            short_compute(0xF, 0, 1),
            {0: T.Const(f32(1.5)), 1: T.Const(f32(2.0))},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "float-multiply")
        self.assertEqual(value, T.Const(f32(3.0)))
        self.assertIsNone(T._astatx_known_bit(astatx, T.MN_BIT))

    def test_short_compute_opcode_space_is_fully_enumerated(self):
        # All 16 ShortCompute opcodes (PRM Table 18-2) are now implemented;
        # none should reach the "unsupported short compute" fallback.
        for opcode in range(16):
            with self.subTest(opcode=hex(opcode)):
                try:
                    T._compute(
                        short_compute(opcode, 0, 1),
                        True,
                        {0: T.Const(1), 1: T.Const(2)},
                    )
                except ValueError as exc:
                    self.fail("opcode %#x unexpectedly unsupported: %s" % (opcode, exc))

    # -- End-to-end wiring through _execute/_apply_compute -------------------

    def test_dual_result_reaches_both_uregs_via_execute(self):
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        state = T.State(1, {1: T.Const(10), 2: T.Const(3)})
        record = insn(
            "2a",
            {
                "cond[4:0]": 0x1F,
                "compute[22:16]": fields["compute[22:16]"],
                "compute[15:0]": fields["compute[15:0]"],
            },
            6,
        )
        executed = T._execute(state, record)[0]
        self.assertEqual(executed.uregs[0], T.Const(13))
        self.assertEqual(executed.uregs[3], T.Const(7))
        self.assertEqual(executed.trace[-1]["result_register"], ["R0", "R3"])
        self.assertEqual(executed.trace[-1]["value"], [13, 7])

    def recips_record(self):
        fields = full_compute(0, 0xC4, 0, 1, 0)
        return insn("2a", {"cond[4:0]": 0x1F, **fields}, 6)

    def test_approx_recips_end_to_end_tags_the_path_and_emits_an_event(self):
        state = T.State(1, {1: T.Const(f32(4.0))}, approx_recips=True)
        executed = T._execute(state, self.recips_record())[0]
        self.assertTrue(executed.approx_recips_used)
        approx_events = [
            event
            for event in executed.trace
            if event["action"] == "approximate-recips"
        ]
        self.assertEqual(len(approx_events), 1)
        self.assertEqual(approx_events[0]["value"], f32(0.25))
        self.assertEqual(executed.uregs[0], T.Const(f32(0.25)))

    def test_approx_recips_disabled_by_default_leaves_no_trace_and_flag_unset(self):
        state = T.State(1, {1: T.Const(f32(4.0))})
        self.assertFalse(state.approx_recips)
        executed = T._execute(state, self.recips_record())[0]
        self.assertFalse(executed.approx_recips_used)
        self.assertNotIn(
            "approximate-recips", [event["action"] for event in executed.trace]
        )
        self.assertIsInstance(executed.uregs[0], T.Unknown)


class PhaseAOpcodeRegressionTest(unittest.TestCase):
    """Literal Phase A vectors from the public PRM tables in SOURCES.md."""

    def test_type10a_relative_decodes_its_split_fields(self):
        # Type10a_rel's VISA prefix and split reladdr fields are from the PRM
        # Figure 15-4-derived decode table, not an implementation round-trip.
        compute = (0x01 << 12) | (2 << 8) | (3 << 4) | 4
        extra = (
            (1 << 44)
            | (3 << 41)
            | (5 << 38)
            | (0x1F << 33)
            | (1 << 32)
            | (0x15 << 27)
            | (9 << 23)
            | compute
        )
        record = T.decode_at(encode("10a_rel", extra), 0, 0)
        self.assertEqual(
            (record.type_name, record.length_bytes, record.kind),
            ("10a_rel", 6, "confident"),
        )
        self.assertEqual(
            record.fields,
            {
                "d": 1,
                "dmi[2:0]": 3,
                "dmm[2:0]": 5,
                "cond[4:0]": 0x1F,
                "dreg[3:0]": 9,
                "compute[22:16]": 0,
                "compute[15:0]": compute,
                "reladdr[5:5]": 1,
                "reladdr[4:0]": 0x15,
            },
        )

    def test_type2b_decodes_and_executes_an_unconditional_full_compute(self):
        # Type2b is the PRM Figure 14-4 32-bit 110000000 prefix; its sole
        # payload is a full compute field (decode_table.json).
        compute = (0x01 << 12) | (2 << 8) | (3 << 4) | 4  # R2 = R3 + R4
        record = T.decode_at(encode("2b", compute), 0, 0)
        self.assertEqual(
            (record.type_name, record.length_bytes, record.kind), ("2b", 4, "confident")
        )
        self.assertEqual(record.fields, {"compute[22:16]": 0, "compute[15:0]": compute})
        state = self.run_one(T.State(0x10, {3: T.Const(7), 4: T.Const(9)}), record)
        self.assertEqual((state.pc_sw, state.uregs[2]), (0x12, T.Const(16)))

    def run_one(self, state, record):
        return T._execute(state, record)[0]

    def test_alu_copysign_uses_magnitude_and_sign_operands(self):
        # ALUOP 0xe0: FN = FX copysign FY (PRM Table 18-5).
        rn, value, operation, _ = T._compute(
            full_compute(0, 0xE0, 0, 1, 2),
            False,
            {1: T.Const(f32(3.5)), 2: T.Const(f32(-2.0))},
        )
        self.assertEqual(
            (rn, operation, value), (0, "float-copysign", T.Const(f32(-3.5)))
        )

    def test_multifunction_convert_max_and_min_literal_vectors(self):
        # PRM Table 18-16 rows 011010, 011110, and 011111 respectively.
        common = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(3),
            12: T.Const(2),
        }
        for category, expected, operation in (
            (0x1A, T.Const(f32(12.0)), "float-mulalu-convert"),
            (0x1E, T.Const(f32(3.0)), "float-mulmax"),
            (0x1F, T.Const(f32(2.0)), "float-mulmin"),
        ):
            with self.subTest(category=hex(category)):
                # The multiplier half is 2.0 * 3.0; the ALU half selects
                # R8/R12, which are fixed inputs for 1a and float inputs for 1e/f.
                values = (
                    common
                    if category == 0x1A
                    else {**common, 8: T.Const(f32(3.0)), 12: T.Const(f32(2.0))}
                )
                rn, value, actual, _ = T._compute(
                    mulalu_fields(category, 1, 2, 0, 0, 0, 0), False, values
                )
                self.assertEqual((rn, actual), ((1, 2), operation))
                self.assertEqual(value, (T.Const(f32(6.0)), expected))

    def test_scaled_fixed_float_convert_forms(self):
        # The Table 18-5 forms ending "by RY" apply RY as a base-two scale.
        mode1 = T.UREG_CODES["MODE1"]
        cases = (
            (
                0xD9,
                {1: T.Const(f32(3.5)), 2: T.Const(1), mode1: T.Const(0)},
                "fix-scaled",
                T.Const(7),
            ),
            (
                0xDA,
                {1: T.Const(3), 2: T.Const(2)},
                "float-convert-scaled",
                T.Const(f32(12.0)),
            ),
            (
                0xDD,
                {1: T.Const(f32(-3.75)), 2: T.Const(1), mode1: T.Const(0)},
                "trunc-scaled",
                T.Const(0xFFFFFFF9),
            ),
        )
        for opcode, values, operation, expected in cases:
            with self.subTest(opcode=hex(opcode)):
                _, value, actual, _ = T._compute(
                    full_compute(0, opcode, 0, 1, 2), False, values
                )
                self.assertEqual((actual, value), (operation, expected))

    def test_alu_carry_and_borrow_consume_astatx_ac(self):
        # Table 18-5 ALUOP 05: RX+RY+ci; 06: RX-RY+ci-1.  AC supplies ci.
        astatx = T.UREG_CODES["ASTATX"]
        cases = (
            (0x05, 0xFFFFFFFF, 0, 1, "add-with-carry", 0, True),
            (0x06, 0, 0, 0, "subtract-with-borrow", 0xFFFFFFFF, False),
            (0x06, 0, 0, 1, "subtract-with-borrow", 0, True),
        )
        for opcode, rx, ry, carry, operation, expected, ac in cases:
            with self.subTest(opcode=hex(opcode), carry=carry):
                _, value, actual, update = T._compute(
                    full_compute(0, opcode, 0, 1, 2),
                    False,
                    {
                        1: T.Const(rx),
                        2: T.Const(ry),
                        astatx: T.Const(carry << T.AC_BIT),
                    },
                )
                self.assertEqual((actual, value), (operation, T.Const(expected)))
                self.assertEqual(
                    T._astatx_known_bit(update(T.Unknown("start")), T.AC_BIT), ac
                )


class UregMovePartialConstTest(unittest.TestCase):
    """A PartialConst must never leak out of ASTATX/ASTATY into a general
    register: `R0 = ASTATX` (a Type5a UREG move) has to downgrade it, since
    generic consumers (_add/_negate/_multiply/_terms, DM stores, dossiers)
    only understand Const/Affine/Unknown. `_ureg` does this downgrade for
    every caller except the flag/predicate code, which reads the raw
    register through `_ureg_raw`."""

    def move_astatx_to_r0(self, astatx_value, extra_uregs=None):
        astatx_code = T.UREG_CODES["ASTATX"]
        fields = {
            "srcureghigh[4:0]": astatx_code >> 2,
            "srcureglow[1:1]": (astatx_code >> 1) & 1,
            "srcureglow[0:0]": astatx_code & 1,
            "dstureg[6:0]": 0,
            "cond[4:0]": 0x1F,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        uregs = {astatx_code: astatx_value}
        uregs.update(extra_uregs or {})
        state = T.State(0x10, uregs)
        record = insn("5a_move", fields, length=6)
        return T._execute(state, record)[0]

    def test_partially_known_astatx_move_gives_unknown_in_r0_without_crashing(self):
        partial = T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AZ_BIT)
        moved = self.move_astatx_to_r0(partial)
        self.assertIsInstance(moved.uregs[0], T.Unknown)
        # Using the downgraded value generically (arithmetic, the path the
        # coordinator flagged) must not crash and must stay Unknown.
        incremented = T._add(moved.uregs[0], T.Const(1), "R0 + 1")
        self.assertIsInstance(incremented, T.Unknown)
        negated = T._negate(moved.uregs[0], "-R0")
        self.assertIsInstance(negated, T.Unknown)
        multiplied = T._multiply(moved.uregs[0], T.Const(2), "R0 * 2")
        self.assertIsInstance(multiplied, T.Unknown)

    def test_fully_known_astatx_move_gives_const(self):
        moved = self.move_astatx_to_r0(T.Const(0x00000001))
        self.assertEqual(moved.uregs[0], T.Const(1))
        self.assertEqual(T._add(moved.uregs[0], T.Const(1), "R0 + 1"), T.Const(2))

    def test_astatx_register_and_its_predicate_are_unaffected_by_the_move(self):
        partial = T.PartialConst(1 << T.AZ_BIT, 1 << T.AZ_BIT)  # AZ known set
        moved = self.move_astatx_to_r0(
            partial, extra_uregs={T.UREG_CODES["MODE1"]: T.Const(0)}
        )
        # The move reads ASTATX out; it does not consume or clear it.
        self.assertEqual(moved.uregs[T.UREG_CODES["ASTATX"]], partial)
        self.assertTrue(T._predicate(moved, 0x00))  # EQ still resolves True
        self.assertIsInstance(moved.uregs[0], T.Unknown)


class RealBlobStage6NormalWordAddressingTest(unittest.TestCase):
    """DT2 1.16's wavetable stage 6 (blk93@0x1cbf07) stores a compiled
    local with Type4a and re-reads it with Type15a. Skips cleanly when the
    firmware isn't present, per CLAUDE.md ("Firmware ... is Elektron's
    copyright: never commit it") and this repo's skip-not-fail convention
    for optional fixtures (see tests/test_sharcfn.py's
    RealBlobType19AndType6aRenderingTest and tests/test_sharc_interface_probe.py).
    """

    BLOB = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "out",
        "sections",
        "dt2-1.16",
        "section_7_BLOB.bin",
    )
    # CLAUDE.md / the handover's own record of this exact loader-final blob.
    BLOB_SHA256 = (
        "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2"
    )

    # Synthetic addresses for the caller-side frame/struct/table, matching
    # the wavetable6 scratch fixture's layout.py: word offsets from I4 are
    # scaled by 4 for --assume-32bit-normal-words, exactly like Type4a's own
    # modifier (tools/sharc_trace.py's "4a" handling).
    I6 = 0x90001000
    I7 = I6 - 8
    R4_I4 = 0x90002000
    R8_I5 = 0x90010000
    R12_I3 = 0x90010000
    WAVETABLE = 0x90020000
    I4_FIELD15 = 15 * 4

    @staticmethod
    def _f32(value):
        return struct.unpack("<I", struct.pack("<f", value))[0]

    def _pokes(self):
        """The minimum concrete DM state for a full run of blk93 (BLK=4
        iterations): the caller's trip count, the persisted-scalar struct
        fields Type2a/Type3a read every iteration, and a wavetable so the
        per-iteration table reads (Type3b DM(I1,M...)) are concrete too --
        the same shape as the wavetable6 scratch fixture's build_pokes.py."""
        pokes = {
            self.I6 + 4: 4,  # DM(I6+1 word) = DM(I6+4 bytes) = BLK trip count
            self.R4_I4 + 0: 0,
            self.R4_I4 + 2 * 4: 0,
            self.R4_I4 + 4 * 4: 0,
            self.R4_I4 + 10 * 4: 1,
            self.R4_I4 + 13 * 4: self._f32(100.5),  # INDEX_IN
            self.R4_I4 + 14 * 4: 0,  # PHASE_IN
            self.R4_I4 + self.I4_FIELD15: self._f32(2.0),
            self.R4_I4 + 16 * 4: self.WAVETABLE + 4 * 100,  # I1 = table + index_in
            self.R4_I4 + 0x44: 0,
            self.R8_I5: self._f32(0.0),
        }
        table = [self._f32(i / 8192) for i in range(8192 + 32)]
        for offset, word in enumerate(table):
            pokes[self.WAVETABLE + 4 * offset] = word
        return pokes

    @unittest.skipUnless(
        os.path.exists(BLOB), "out/sections/dt2-1.16/section_7_BLOB.bin is not available"
    )
    def test_type4a_store_and_type15a_load_agree_on_the_persisted_local(self):
        with open(self.BLOB, "rb") as fh:
            data = fh.read()
        self.assertEqual(hashlib.sha256(data).hexdigest(), self.BLOB_SHA256)
        memory = L.LoadedMemory.from_stream(data)
        states = T.trace(
            memory,
            None,
            0x1CBF07,
            {
                "I6": self.I6,
                "I7": self.I7,
                "R4": self.R4_I4,
                "R8": self.R8_I5,
                "R12": self.R12_I3,
                "M5": 0,
                "M6": 1,
                "M7": -1,
                "M13": 0,
                "M14": 1,
                "M15": -1,
                "R0": 0,
                "R11": 0x40000000,
                "R15": 0x3F800000,
                "R3": 0,
                "R5": 1,
            },
            max_steps=400,
            max_states=32,
            concrete_memory=True,
            follow_loaded_calls=True,
            assume_nw32=True,
            approx_recips=True,
            pokes=self._pokes(),
        )
        self.assertTrue(states)
        checked_any = False
        for state in states:
            stores = [
                event
                for event in state.trace
                if event.get("pc_sw") == 0x1CBF72 and event.get("action") == "store"
            ]
            loads = [
                event
                for event in state.trace
                if event.get("pc_sw") == 0x1CBFF1 and event.get("action") == "load"
            ]
            if not stores or not loads:
                continue
            checked_any = True
            stored = stores[0]
            self.assertEqual(stored["dreg"], "R0")
            self.assertIsNotNone(stored["value"])
            # Type4a's store and Type15a's load must resolve to the same
            # byte address for the same architectural word offset from I6
            # (the bug this fix corrects: Type15a's <data32> was previously
            # added to I6 unscaled, landing 12 bytes from Type4a's I6-16).
            for loaded in loads:
                self.assertEqual(loaded["ureg"], "R12")
                self.assertEqual(loaded["address"], stored["address"])
                self.assertEqual(loaded["concrete_value"], stored["value"])
        self.assertTrue(
            checked_any,
            "no traced path reached both the Type4a store at 0x1cbf72 and "
            "the Type15a load at 0x1cbff1",
        )


if __name__ == "__main__":
    unittest.main()
