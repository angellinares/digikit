"""Synthetic tests for the deliberately small SHARC delay tracer."""

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
            T.State(1, {0: T.Const(99), 2: T.Const(4), 12: T.Const(4)}),
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

    def test_compute_unknown_and_unsupported_do_not_mutate(self):
        s = self.run_one(T.State(1), insn("2c", {"compute[11:0]": 0x251}, 2))
        self.assertIsInstance(s.uregs[5], T.Unknown)
        s = self.run_one(
            T.State(1, {1: T.Const(2)}), insn("2c", {"compute[11:0]": 0xF12}, 2)
        )
        self.assertIn("unsupported short compute", s.stopped)
        self.assertEqual(s.uregs, {1: T.Const(2)})

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
        self.assertEqual(
            (concrete.pc_sw, concrete.uregs[3]), (13, T.Const(0xFFFFFFFF))
        )
        self.assertEqual(
            concrete.trace[-1],
            {
                "pc_sw": 10,
                "form": "2a",
                "action": "compute",
                "operation": "decrement",
                "result_register": "R3",
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
        unsupported = self.run_one(
            T.State(10, dict(original)),
            insn(
                "2a",
                {"cond[4:0]": 0x1F, "compute[22:16]": 0xF, "compute[15:0]": 0x1234},
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
            (0, 1, 1): "normal-word",
            (0, 0, 0): "byte",
            (0, 1, 0): "byte-sign-extended",
            (1, 0, 0): "short-word",
            (1, 1, 0): "short-word-sign-extended",
            (1, 1, 1): "long-word",
        }
        for (l, x, w), access_width in expected.items():
            event = self.run_one(
                T.State(1, {16: T.Const(0x80), 32: T.Const(3)}),
                insn("3b", {**base, "l": l, "x": x, "w": w}),
            ).trace[0]
            self.assertEqual(event["access_width"], access_width)

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
            (executed.trace[-1]["condition"], executed.trace[-1]["predicate_assumption"]),
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
        self.assertEqual((skipped.uregs[4], skipped.uregs[17]), (T.Const(7), T.Const(0x80)))

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

    def test_type3b_unknown_predicate_second_call_delay_slot_preserves_call_target(self):
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
        self.assertNotIn(2, skipped.uregs)

    def test_19a_constant_and_unknown(self):
        f = {
            "g": 1,
            "idis[2:0]": 2,
            "is[2:0]": 1,
            "data[31:16]": 0xFFFF,
            "data[15:0]": 0xFFFE,
        }
        self.assertEqual(
            self.run_one(T.State(1, {25: T.Const(7)}), insn("19a", f, 6)).uregs[26],
            T.Const(5),
        )
        self.assertIsInstance(
            self.run_one(T.State(1), insn("19a", f, 6)).uregs[26], T.Unknown
        )

    def test_delay_slots_variable_width_and_target(self):
        branch = insn(
            "8a_abs", {"b": 0, "cond[4:0]": 31, "addr[23:16]": 0, "addr[15:0]": 99}, 6
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
            "8a_abs", {"b": 0, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 30}, 6
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
        self.assertEqual([e["pc_sw"] for e in s.trace[:-1]], [10, 12, 14])

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
        self.assertEqual(T._render(state.uregs[18]), "receive_buffer + 0x94")
        seeded = T.trace(b"", 0, 0, {"R1": 7}, max_steps=0)[0]
        self.assertEqual(seeded.uregs[1], T.Const(7))
        symbolic = T.trace(b"", 0, 0, {"R1": "@receive_buffer"}, max_steps=0)[0]
        self.assertEqual(symbolic.uregs[1], T.symbol("receive_buffer"))
        with self.assertRaises(ValueError):
            T.trace(b"", 0, 0, {"R1": "@"}, max_steps=0)

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
            self.assertNotEqual(missing_base.returncode, 0)
            self.assertIn("--base-sw is required", missing_base.stderr.decode())
            self.assertNotEqual(ambiguous.returncode, 0)
            self.assertIn("ambiguous", ambiguous.stderr.decode())
            self.assertNotEqual(no_ranges.returncode, 0)
            self.assertIn("no loaded ranges", no_ranges.stderr.decode())
            self.assertEqual(valid.returncode, 0, valid.stderr.decode())
            self.assertNotIn("Traceback", valid.stderr.decode())
            self.assertNotIn("raw", valid.stdout.decode())
            json.loads(valid.stdout)
        finally:
            os.unlink(stream_path)
            os.unlink(empty_path)

    def test_event_values_are_json_safe(self):
        type3c = {"dmi[2:0]": 0, "dmm[2:0]": 0, "d": 1, "dreg[3:0]": 3}
        s = self.run_one(
            T.State(1, {16: T.Const(0x80), 32: T.Const(3)}), insn("3c", type3c, 2)
        )
        self.assertEqual(s.trace[0]["value"], {"unknown": "uninitialized R3"})
        encoded = json.dumps(s.trace)
        self.assertIn('"unknown": "uninitialized R3"', encoded)
        self.assertNotIn("b'", encoded)

    def test_bounds_and_json_has_no_raw_bytes(self):
        data = b"\x00\x00"
        self.assertEqual(T.trace(data, 0, 0, max_steps=0)[0].stopped, "max-steps")
        branch = insn(
            "8a_abs", {"b": 0, "cond[4:0]": 1, "addr[23:16]": 0, "addr[15:0]": 20}, 6
        )
        with patch("sharc_trace.decode_at", return_value=branch):
            self.assertIn(
                "max-states", [s.stopped for s in T.trace(b"", 0, 0, max_states=1)]
            )
        with tempfile.NamedTemporaryFile("wb", delete=False) as f:
            f.write(data)
            path = f.name
        try:
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


if __name__ == "__main__":
    unittest.main()
