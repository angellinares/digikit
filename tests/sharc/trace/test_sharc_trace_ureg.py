"""Symbolic tracer: universal-register moves, computes and compares."""

import unittest
from importlib import import_module

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")
from support.sharc_trace_build import insn  # noqa: E402

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T

from support.sharc_trace_build import TraceRunMixin  # noqa: E402


class TraceUregComputeTest(TraceRunMixin, unittest.TestCase):
    """Universal-register moves, computes and compares."""

    def test_combined_px_copy_updates_halves_and_restores(self):
        px = T.UREG_CODES["PX"]
        px1 = T.UREG_CODES["PX1"]
        px2 = T.UREG_CODES["PX2"]

        def move(state, src, dst):
            return self.run_one(
                state,
                insn(
                    "5b_move",
                    {
                        "srcureghigh[4:0]": src >> 2,
                        "srcureglow[1:1]": (src >> 1) & 1,
                        "srcureglow[0:0]": src & 1,
                        "dstureg[6:0]": dst,
                        "cond[4:0]": 31,
                    },
                ),
            )

        state = T.State(
            1,
            {
                0: T.Const(0x12345678),
                2: T.Const(0x9ABC0000),
                3: T.Const(0x87654321),
                px: T.Const(0),
                px1: T.Const(0x9ABC0000),
                px2: T.Const(0x87654321),
            },
        )
        state = move(state, 0, px)
        self.assertEqual(
            (state.uregs[px1], state.uregs[px2]), (T.Const(0), T.Const(0x12345678))
        )
        state = move(state, px, 1)
        self.assertEqual(state.uregs[1], T.Const(0x12345678))
        state = move(move(state, 2, px1), 3, px2)
        self.assertEqual(
            (state.uregs[px1], state.uregs[px2]),
            (T.Const(0x9ABC0000), T.Const(0x87654321)),
        )
        self.assertEqual(state.uregs[px], T.Const(0))

    def test_b_write_also_loads_i(self):
        """PRM p.6-25: a write of Bn loads In with the same value; a write of In leaves Bn.

        Issue #201: stock DN2 code restores B7 and then reloads I7. A program that writes I7 before B7 must lose its
        I7, as on the instrument."""
        b7, i7, b3, i3 = (T.UREG_CODES[n] for n in ("B7", "I7", "B3", "I3"))

        def move(state, src, dst):
            return self.run_one(
                state,
                insn(
                    "5b_move",
                    {
                        "srcureghigh[4:0]": src >> 2,
                        "srcureglow[1:1]": (src >> 1) & 1,
                        "srcureglow[0:0]": src & 1,
                        "dstureg[6:0]": dst,
                        "cond[4:0]": 31,
                    },
                ),
            )

        def immediate(state, dst, value):
            return self.run_one(
                state,
                insn(
                    "17a",
                    {
                        "ureg[6:0]": dst,
                        "data[31:16]": value >> 16,
                        "data[15:0]": value & 0xFFFF,
                    },
                    8,
                ),
            )

        def regs(state, *codes):
            return tuple(state.uregs[c] for c in codes)

        start = {0: T.Const(0x2D08C0), 1: T.Const(0x2D0900)}
        start |= {i7: T.Const(0x2D0A00), b7: T.Const(0x2D0000)}
        # I7 then B7: the B7 write overwrites I7.
        state = move(move(T.State(1, dict(start)), 1, i7), 0, b7)
        self.assertEqual(regs(state, b7, i7), (T.Const(0x2D08C0), T.Const(0x2D08C0)))
        # B7 then I7 (the control): I7 keeps its own value and B7 does not change.
        state = move(move(T.State(1, dict(start)), 0, b7), 1, i7)
        self.assertEqual(regs(state, b7, i7), (T.Const(0x2D08C0), T.Const(0x2D0900)))
        # An immediate write of B7 (Type 17).
        state = immediate(T.State(1, dict(start)), b7, 0x12345)
        self.assertEqual(regs(state, b7, i7), (T.Const(0x12345), T.Const(0x12345)))
        # Bd = W2B(Bs) (Type 7d) writes B3 but not I3 (PRM p.6-15: "the corresponding I
        # register is not implicitly updated"). The DN2 interrupt dispatcher relies on it.
        state = self.run_one(
            T.State(1, {b7: T.Const(0x2D08C0), i3: T.Const(0x5), b3: T.Const(0x6)}),
            insn(
                "7d",
                {"g": 0, "is[2:2]": 1, "is[1:0]": 3, "idis": 4, "breg": 1, "toby": 1},
            ),
        )
        self.assertNotEqual(state.uregs[b3], T.Const(0x6))
        self.assertEqual(state.uregs[i3], T.Const(0x5))

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
        short = lambda opcode, rn, rx: {"compute[11:0]": (opcode << 8) | (rn << 4) | rx}  # noqa: E731
        s = self.run_one(T.State(1, {3: T.Const(9)}), insn("2c", short(2, 1, 3), 2))
        self.assertEqual(s.uregs[1], T.Const(9))
        full = lambda cu, op, rn, rx, ry: {  # noqa: E731
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


if __name__ == "__main__":
    unittest.main()
