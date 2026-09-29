"""Tests for the Group I forms and circular-buffer wrap this session added
to tools/sharc_trace.py's ``_execute``: 1a, 7b, 22c, 26a, 5a_swap, 4d, 3d,
the 8p_undoc48/21p_undoc16/22p_undoc48 "no confirmed semantics" stops, and
the Type19a_scaled/Type7a/Type7b circular-modify wrap.

Field values marked "real SW <addr>" are copied verbatim from
out/sharcdb/dt2-1.16.sqlite (tools/sharc.py dt2-1.16 "SELECT sw, fields
FROM insn WHERE form='<form>'") so each form is exercised with a shape the
firmware actually contains, not just an invented encoding. A few tests
zero out an unrelated "compute" sub-field (noted inline) to stay
independent of the parallel compute-opcode work landing in the same file.
"""

import os
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
T = import_module("sharc_trace")
Instruction = import_module("sharc_disasm").Instruction
from test_sharc_trace import loader_memory  # noqa: E402  (shared fixture)


def insn(name, fields, length=4, kind="confident"):
    return Instruction(0, length, name, fields, kind=kind)


class Type1aTest(unittest.TestCase):
    """real SW 0x16b9c3 (1489107), compute zeroed (an empty Type1a compute
    is architecturally valid -- PRM p.13-3's own syntax row "00000000000
    00000000000 DMACCESS(1a), PMACCESS(1a)" -- so this isolates the dual
    move from _compute's ongoing opcode coverage)."""

    def test_dual_dm_store_pm_load_post_modify_no_compute(self):
        fields = {
            "compute[15:0]": 0,
            "compute[22:16]": 0,
            "dmd": 1,
            "dmdreg[3:0]": 1,
            "dmi[2:0]": 5,
            "dmm[2:0]": 1,
            "pmd": 0,
            "pmdreg[3:0]": 8,
            "pmi[1:0]": 1,
            "pmi[2:2]": 1,
            "pmm[2:0]": 6,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["R1"]: T.Const(0x55),
                T.UREG_CODES["I5"]: T.Const(0x1000),
                T.UREG_CODES["M1"]: T.Const(2),
                T.UREG_CODES["I13"]: T.Const(0x2000),
                T.UREG_CODES["M14"]: T.Const(3),
            },
        )
        [result] = T._execute(state, insn("1a", fields, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.pc_sw, 0x13)
        # DM(I5, M1) = R1, post-modify: I5 addressed the old value, then I5 += M1.
        store = next(e for e in result.trace if e["action"] == "store")
        self.assertEqual(store["space"], "DM")
        self.assertEqual(store["address"], 0x1000)
        self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(0x1002))
        # PM(I13, M14) into R8, post-modify: PM loads are always Unknown here
        # (no PM concrete backing), but the post-modify still advances I13.
        load = next(e for e in result.trace if e["action"] == "load")
        self.assertEqual(load["space"], "PM")
        self.assertIsInstance(result.uregs[T.UREG_CODES["R8"]], T.Unknown)
        self.assertEqual(result.uregs[T.UREG_CODES["I13"]], T.Const(0x2003))


class Type7bTest(unittest.TestCase):
    def test_linear_modify_real_instance(self):
        # real SW 0x120bff (1183327): L1 unset (Unknown, not Const 0), so
        # this exercises the "length not a known nonzero Const" linear path.
        fields = {
            "cond[4:0]": 31,
            "g": 0,
            "idis[2:0]": 2,
            "is[1:0]": 1,
            "is[2:2]": 0,
            "m[2:0]": 4,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I1"]: T.Const(0x100), T.UREG_CODES["M4"]: T.Const(5)},
        )
        [result] = T._execute(state, insn("7b", fields, length=4))
        self.assertIsNone(result.stopped)
        # source I1, destination I1 XOR idis(2) = I3.
        self.assertEqual(result.uregs[T.UREG_CODES["I3"]], T.Const(0x105))
        event = result.trace[-1]
        self.assertEqual(event["action"], "i-modify")
        self.assertNotIn("circular", event)

    def test_circular_wrap_always_active_independent_of_cbufen(self):
        # PRM p.13-49: MODIFY wraps whenever L is nonzero "independent of
        # the state of the CBUFEN bit". MODE1 is left at Unknown (default)
        # here, i.e. CBUFEN's state is deliberately unknown, and the wrap
        # still happens.
        fields = {
            "cond[4:0]": 31,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 0,
            "m[2:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x1008),
                T.UREG_CODES["M0"]: T.Const(4),
                T.UREG_CODES["L0"]: T.Const(0x10),
                T.UREG_CODES["B0"]: T.Const(0x1000),
            },
        )
        [result] = T._execute(state, insn("7b", fields, length=4))
        self.assertIsNone(result.stopped)
        # 0x1008 + 4 = 0x100c, still inside [0x1000, 0x1010): no wrap needed.
        self.assertEqual(result.uregs[T.UREG_CODES["I0"]], T.Const(0x100C))
        self.assertTrue(result.trace[-1]["circular"])

    def test_predicate_false_skips(self):
        fields = {
            "cond[4:0]": 0x03,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 0,
            "m[2:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x100),
                T.UREG_CODES["M0"]: T.Const(4),
                T.UREG_CODES["ASTATX"]: T.Const(0),  # AC_BIT clear -> cond 0x03 false
            },
        )
        [result] = T._execute(state, insn("7b", fields, length=4))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.uregs[T.UREG_CODES["I0"]], T.Const(0x100))
        self.assertEqual(result.trace[-1]["action"], "i-modify-skipped")


class Type22cTest(unittest.TestCase):
    def test_idle_real_instance_advances(self):
        # real SW 0x120988 (1181736): emu=0.
        state = T.State(0x10, {0: T.Const(7)})
        [result] = T._execute(state, insn("22c", {"emu": 0}, length=2))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.pc_sw, 0x11)
        self.assertEqual(result.uregs, {0: T.Const(7)})
        self.assertEqual(result.trace[-1]["action"], "idle")
        self.assertEqual(result.trace[-1]["mode"], "idle")

    def test_emuidle_variant(self):
        state = T.State(0x10)
        [result] = T._execute(state, insn("22c", {"emu": 1}, length=2))
        self.assertEqual(result.trace[-1]["mode"], "emuidle")


class Type26aTest(unittest.TestCase):
    def test_sync_real_instance_is_a_pure_advance(self):
        # real SW 0x120724 (1181924): Type26a has no fields at all.
        state = T.State(0x10, {0: T.Const(1)})
        [result] = T._execute(state, insn("26a", {}, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.pc_sw, 0x13)
        self.assertEqual(result.uregs, {0: T.Const(1)})
        self.assertEqual(result.trace[-1]["action"], "sync")


class Type5aSwapTest(unittest.TestCase):
    def test_unconditional_swap_real_instance_no_compute(self):
        # real SW 0x16ba03 (1489092 uses cond 0x14; this one, 1839651, is
        # unconditional). Compute zeroed to isolate the swap from _compute.
        fields = {
            "cdreg[3:0]": 5,
            "compute[15:0]": 0,
            "compute[22:16]": 0,
            "cond[4:0]": 31,
            "dreg[3:0]": 5,
        }
        state = T.State(0x10, {T.UREG_CODES["R5"]: T.Const(0x42)})
        [result] = T._execute(state, insn("5a_swap", fields, length=6))
        self.assertIsNone(result.stopped)
        # R5's new value came from the untracked complementary S5: Unknown,
        # not the pre-swap R5 value and not a stop.
        self.assertIsInstance(result.uregs[T.UREG_CODES["R5"]], T.Unknown)
        self.assertEqual(result.trace[-1]["action"], "dreg-swap")

    def test_predicate_false_skips_and_leaves_dreg_untouched(self):
        fields = {
            "cdreg[3:0]": 9,
            "compute[15:0]": 0,
            "compute[22:16]": 0,
            "cond[4:0]": 0x03,
            "dreg[3:0]": 14,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["R14"]: T.Const(0x99),
                T.UREG_CODES["ASTATX"]: T.Const(0),
            },
        )
        [result] = T._execute(state, insn("5a_swap", fields, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.uregs[T.UREG_CODES["R14"]], T.Const(0x99))
        self.assertEqual(result.trace[-1]["action"], "dreg-swap-skipped")


class Type4bTest(unittest.TestCase):
    """Type4b's (l, x, w) tables (PRM p.13-32, BH/BHSE (Type 4b)) have no
    (lw) row: (1, 1, 1) is the row with no suffix, the plain normal-word
    access (encoding.TYPE4B_ACCESS_WIDTHS). The immediate modifier is
    scaled by the size of the access in byte space (PRM p.6-9)."""

    def test_111_is_normal_word_load_post_modify_real_instance(self):
        # real SW 0x1c6b4c: d=0 load into R4, (l, x, w) = (1, 1, 1), u=1
        # post-modify, I3, data=+7.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "data[4:0]": 7,
            "data[5:5]": 0,
            "dreg[3:0]": 4,
            "g": 0,
            "i[2:0]": 3,
            "l": 1,
            "u": 1,
            "w": 1,
            "x": 1,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I3"]: T.Const(0x30004000)},
            concrete=loader_memory(),
            assume_nw32=True,
        )
        self.assertTrue(T._dm_write(state, 0x30004000, 4, T.Const(0x1234)))
        [result] = T._execute(state, insn("4b", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["action"], "load")
        self.assertEqual(load["access_width"], "normal-word")
        self.assertEqual(load["addressing_mode"], "post-modify")
        self.assertEqual(result.uregs[T.UREG_CODES["R4"]], T.Const(0x1234))
        self.assertNotIn(T.UREG_CODES["R5"], result.uregs)  # no pair load
        # I3 = 0x30004000 + 7 * 4.
        self.assertEqual(result.uregs[T.UREG_CODES["I3"]], T.Const(0x3000401C))

    def test_sw_0x1c336b_reads_the_left_channel_slot(self):
        # sw 0x1c336b "IF SZ R8 = DM(I6 - 12)", (l, x, w) = (1, 1, 1):
        # FUN_1c3289 stored {L, R, frames} at I6-12..I6-10 normal words
        # (sw 0x1c3341-0x1c3349) and read R with "R8 = DM(I6 - 11)"
        # (Type15b, sw 0x1c3363). The (1, 1, 1) load must read L, the word
        # at I6 - 48 bytes, not the one at I6 - 96.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "data[4:0]": 20,
            "data[5:5]": 1,
            "dreg[3:0]": 8,
            "g": 0,
            "i[2:0]": 6,
            "l": 1,
            "u": 0,
            "w": 1,
            "x": 1,
        }
        i6 = 0x30005000
        state = T.State(
            0x1C336B,
            {T.UREG_CODES["I6"]: T.Const(i6)},
            concrete=loader_memory(),
            assume_nw32=True,
        )
        self.assertTrue(T._dm_write(state, i6 - 48, 4, T.Const(0x8422B9E8)))
        self.assertTrue(T._dm_write(state, i6 - 44, 4, T.Const(0x842CC9E8)))
        [result] = T._execute(state, insn("4b", fields, length=4))
        self.assertEqual(result.uregs[T.UREG_CODES["R8"]], T.Const(0x8422B9E8))
        self.assertNotIn(T.UREG_CODES["R9"], result.uregs)
        self.assertEqual(result.uregs[T.UREG_CODES["I6"]], T.Const(i6))

    def test_111_store_writes_one_word(self):
        # sw 0x1c26eb "DM(I5, 27) = R10", (1, 1, 1), post-modify: the
        # record fields after it are addressed as I5 - 26 .. I5 - 7, so
        # the store is one normal word and I5 advances 27 words.
        fields = {
            "cond[4:0]": 31,
            "d": 1,
            "data[4:0]": 27,
            "data[5:5]": 0,
            "dreg[3:0]": 10,
            "g": 0,
            "i[2:0]": 5,
            "l": 1,
            "u": 1,
            "w": 1,
            "x": 1,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I5"]: T.Const(0x30006000),
                T.UREG_CODES["R10"]: T.Const(0xA5A5),
                T.UREG_CODES["R11"]: T.Const(0x5A5A),
            },
            concrete=loader_memory(),
            assume_nw32=True,
        )
        self.assertTrue(T._dm_write(state, 0x30006004, 4, T.Const(0x77)))
        [result] = T._execute(state, insn("4b", fields, length=4))
        self.assertTrue(result.trace[-1]["concrete_write"])
        self.assertEqual(T._dm_read(result, 0x30006000, 4), T.Const(0xA5A5))
        self.assertEqual(T._dm_read(result, 0x30006004, 4), T.Const(0x77))
        self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(0x3000606C))

    def test_011_is_not_a_type4b_encoding(self):
        # (0, 1, 1) is Type3b's normal-word row, absent from Type4b's.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "data[4:0]": 1,
            "data[5:5]": 0,
            "dreg[3:0]": 2,
            "g": 0,
            "i[2:0]": 0,
            "l": 0,
            "u": 0,
            "w": 1,
            "x": 1,
        }
        state = T.State(0x10, {T.UREG_CODES["I0"]: T.Const(0x4000)})
        [result] = T._execute(state, insn("4b", fields))
        self.assertEqual(result.stopped, "unsupported Type4b access width")


class LongWordModifierScaleTest(unittest.TestCase):
    """PRM p.6-9: load/store modifiers scale by the size of the access in
    byte space, "except in the case of (lw)"; Table 6-2 (pp.6-10/6-11)
    gives the (lw) rows scaled_mod = mod << 2, and no scaling in word
    space."""

    def test_long_word_scales_like_a_normal_word(self):
        scale = import_module("sharc_core.memory")._access_modifier_scale
        self.assertEqual(scale("long-word", True), 4)
        self.assertEqual(scale("normal-word", True), 4)
        self.assertEqual(scale("long-word", False), 1)
        self.assertEqual(scale("normal-word", False), 1)


class ModifyScaleTest(unittest.TestCase):
    """PRM p.6-10: "Ia = MODIFY(Ib,Mc); /* Add Mc bytes, Ia=Ib+Mc */ Does
    not scale the modifier, whatever the address space"; only (sw)/(nw)
    scale (Table 6-2). Type7b has no (sw)/(nw) bits (p.13-50)."""

    def test_type7b_adds_a_byte_offset(self):
        # sw 0x1c661c "I4 = modify(I4, M0)": I4 = 0x14 (a record offset),
        # M0 = 0x245c4c (a table address).
        fields = {
            "cond[4:0]": 31,
            "g": 0,
            "is[2:2]": 1,
            "is[1:0]": 0,
            "idis[2:0]": 0,
            "m[2:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I4"]: T.Const(0x14),
                T.UREG_CODES["M0"]: T.Const(0x245C4C),
                T.UREG_CODES["L4"]: T.Const(0),
            },
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("7b", fields))
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x245C60))

    def test_type7a_plain_modify_is_unscaled_and_nw_is_scaled(self):
        base = {
            "cond[4:0]": 31,
            "g": 0,
            "is[2:2]": 1,
            "is[1:0]": 1,
            "idis[2:0]": 0,
            "m[2:0]": 6,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        for (w, lbit), expected in (
            ((0, 0), 0x1001),
            ((1, 0), 0x1004),
            ((0, 1), 0x1002),
        ):
            with self.subTest(w=w, l=lbit):
                state = T.State(
                    0x10,
                    {
                        T.UREG_CODES["I5"]: T.Const(0x1000),
                        T.UREG_CODES["M6"]: T.Const(1),
                    },
                    assume_nw32=True,
                )
                [result] = T._execute(state, insn("7a", {**base, "w": w, "l": lbit}, 6))
                self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(expected))


class Type4dTest(unittest.TestCase):
    def test_byte_store_pre_modify_real_instance(self):
        # real SW 0x1c4b91 (1856721): d=1 store, l=x=w=0 -> byte, u=0
        # pre-modify, data=29|1<<5 (six-bit signed -3).
        fields = {
            "cond[4:0]": 31,
            "d": 1,
            "data[4:0]": 29,
            "data[5:5]": 1,
            "dreg[3:0]": 4,
            "g": 0,
            "i[2:0]": 4,
            "l": 0,
            "u": 0,
            "w": 0,
            "x": 0,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I4"]: T.Const(0x2000), T.UREG_CODES["R4"]: T.Const(0xAB)},
        )
        [result] = T._execute(state, insn("4d", fields, length=6))
        self.assertIsNone(result.stopped)
        store = result.trace[-1]
        self.assertEqual(store["action"], "store")
        self.assertEqual(store["access_width"], "byte")
        self.assertEqual(store["addressing_mode"], "pre-modify")
        self.assertEqual(store["address"], 0x1FFD)
        # Pre-modify (matching Type3a/4a's own convention): only the
        # address for this access is offset; I4 itself is left unchanged.
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x2000))

    def test_short_word_sign_extended_load_post_modify_real_instance(self):
        # real SW 0x1c5127 (1846396): d=0 load, l=1,x=1,w=0 ->
        # short-word-sign-extended, u=1 post-modify, data=2.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "data[4:0]": 2,
            "data[5:5]": 0,
            "dreg[3:0]": 2,
            "g": 0,
            "i[2:0]": 4,
            "l": 1,
            "u": 1,
            "w": 0,
            "x": 1,
        }
        state = T.State(0x10, {T.UREG_CODES["I4"]: T.Const(0x3000)})
        [result] = T._execute(state, insn("4d", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["action"], "load")
        self.assertEqual(load["access_width"], "short-word-sign-extended")
        self.assertEqual(load["addressing_mode"], "post-modify")
        self.assertEqual(load["address"], 0x3000)
        # short-word scale is fixed at 2 (not gated by assume_nw32).
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x3004))

    def test_unsupported_predicate_stops(self):
        fields = {
            "cond[4:0]": 0x03,
            "d": 1,
            "data[4:0]": 0,
            "data[5:5]": 0,
            "dreg[3:0]": 0,
            "g": 0,
            "i[2:0]": 0,
            "l": 0,
            "u": 0,
            "w": 0,
            "x": 0,
        }
        state = T.State(
            0x10, {T.UREG_CODES["I0"]: T.Const(0), T.UREG_CODES["R0"]: T.Const(0)}
        )
        [result] = T._execute(state, insn("4d", fields, length=6))
        self.assertEqual(result.stopped, "unsupported Type4d predicate")


class Type3dTest(unittest.TestCase):
    def test_byte_access_load_pre_modify_real_instance(self):
        # real SW 0xb7febf (12059647): d=0 load into R2 (ureg 2), ex=0,
        # w=0, l=0, x=0, u=0 pre-modify. This is dt2-1.16's single most
        # common Type3d encoding (46/72 occurrences, tools/sharc.py's
        # insn table: `select fields, count(*) from insn where form='3d'
        # group by fields`), which rules out "l/x unused, always
        # normal-word" for this (ex, w) = (0, 0) group: a spread of 46
        # (0,0,0,0) + 3 (0,0,0,1) + 5 (0,0,1,0) + 10 (0,0,1,1) instances
        # (a real compiler using every l/x combination) only makes sense
        # if l/x actually select the access width, exactly as
        # ACCESS_WIDTHS[(l, x, 0)] already does for Type3b/Type4d -- see
        # _type_3d's own docstring (tools/sharc_core/forms_move.py) for
        # the PRM citation. l=0, x=0 -> "byte" (ACCESS_WIDTHS[(0, 0, 0)]),
        # not "normal-word" as this test asserted before that fix.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "ex": 0,
            "g": 0,
            "i[2:0]": 5,
            "l": 0,
            "m[2:0]": 5,
            "u": 0,
            "ureg[6:0]": 2,
            "w": 0,
            "x": 0,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I5"]: T.Const(0x4000), T.UREG_CODES["M5"]: T.Const(4)},
        )
        [result] = T._execute(state, insn("3d", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["action"], "load")
        self.assertEqual(load["space"], "DM")
        self.assertEqual(load["access_width"], "byte")
        self.assertEqual(load["addressing_mode"], "pre-modify")
        self.assertEqual(load["address"], 0x4004)
        # Pre-modify (matching Type3a/4a's own convention): I5 itself is
        # left unchanged; only this access's address was offset.
        self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(0x4000))

    def test_short_word_store_pre_modify_real_instance(self):
        # real SW 0x1c29d4: d=1 store of ureg 2 (R2), ex=0, w=0, l=1,
        # x=0 -> ACCESS_WIDTHS[(1, 0, 0)] = "short-word", u=0 pre-modify,
        # I5/M7.
        fields = {
            "cond[4:0]": 31,
            "d": 1,
            "ex": 0,
            "g": 0,
            "i[2:0]": 5,
            "l": 1,
            "m[2:0]": 7,
            "u": 0,
            "ureg[6:0]": 2,
            "w": 0,
            "x": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I5"]: T.Const(0x5000),
                T.UREG_CODES["M7"]: T.Const(1),
                T.UREG_CODES["R2"]: T.Const(0x1234),
            },
        )
        [result] = T._execute(state, insn("3d", fields, length=6))
        self.assertIsNone(result.stopped)
        store = result.trace[-1]
        self.assertEqual(store["action"], "store")
        self.assertEqual(store["access_width"], "short-word")
        self.assertEqual(store["addressing_mode"], "pre-modify")
        # short-word's own scale is fixed at 2, independent of
        # assume_nw32 (sharc_core.memory._access_modifier_scale): M7=1
        # scales to 2, so I5=0x5000 + 2 = 0x5002.
        self.assertEqual(store["address"], 0x5002)
        # Pre-modify: I5 itself is left unchanged.
        self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(0x5000))

    def test_short_word_sign_extended_load_post_modify_real_instance(self):
        # real SW 0x1c419d: d=0 load into ureg 2 (R2), ex=0, w=0, l=1,
        # x=1 -> ACCESS_WIDTHS[(1, 1, 0)] = "short-word-sign-extended",
        # u=1 post-modify, I4/M7.
        fields = {
            "cond[4:0]": 31,
            "d": 0,
            "ex": 0,
            "g": 0,
            "i[2:0]": 4,
            "l": 1,
            "m[2:0]": 7,
            "u": 1,
            "ureg[6:0]": 2,
            "w": 0,
            "x": 1,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I4"]: T.Const(0x6000), T.UREG_CODES["M7"]: T.Const(1)},
        )
        [result] = T._execute(state, insn("3d", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["action"], "load")
        self.assertEqual(load["access_width"], "short-word-sign-extended")
        self.assertEqual(load["addressing_mode"], "post-modify")
        # Post-modify: the access itself uses I4 unchanged.
        self.assertEqual(load["address"], 0x6000)
        # short-word's own scale is fixed at 2 (not gated by
        # assume_nw32): M7=1 scales to 2, so I4 becomes 0x6000 + 2.
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x6002))

    def test_conditional_store_real_instance(self):
        # real SW 0xb7fec8 (12059656): d=1 store of ureg 45 (M13), cond
        # 0x01 (LT); this test only checks the predicate-false skip path.
        fields = {
            "cond[4:0]": 1,
            "d": 1,
            "ex": 0,
            "g": 0,
            "i[2:0]": 5,
            "l": 0,
            "m[2:0]": 5,
            "u": 0,
            "ureg[6:0]": 45,
            "w": 0,
            "x": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I5"]: T.Const(0x4000),
                T.UREG_CODES["M5"]: T.Const(4),
                T.UREG_CODES["M13"]: T.Const(0x77),
                # AN/AV both clear -> LT (cond 0x01) resolves False.
                T.UREG_CODES["ASTATX"]: T.Const(0),
            },
        )
        [result] = T._execute(state, insn("3d", fields, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.trace[-1]["action"], "type3d-skipped")
        self.assertEqual(result.uregs[T.UREG_CODES["I5"]], T.Const(0x4000))

    def test_exclusive_and_waccess_are_unsupported(self):
        base = {
            "cond[4:0]": 31,
            "d": 0,
            "g": 0,
            "i[2:0]": 0,
            "l": 0,
            "m[2:0]": 0,
            "u": 0,
            "ureg[6:0]": 0,
            "w": 0,
            "x": 0,
        }
        state = T.State(0x10, {T.UREG_CODES["I0"]: T.Const(0)})
        [result] = T._execute(state, insn("3d", dict(base, ex=1), length=6))
        self.assertEqual(result.stopped, "unsupported Type3d exclusive access")
        state = T.State(0x10, {T.UREG_CODES["I0"]: T.Const(0)})
        [result] = T._execute(state, insn("3d", dict(base, ex=0, w=1), length=6))
        self.assertEqual(result.stopped, "unsupported Type3d WACCESS")


class Type3aLongWordTest(unittest.TestCase):
    """Type3a's (LW) long-word register-pair option (forms_move.py's
    ``_type_3a``, PRM p.13-15's ACCESS Encode Table + p.2-4 "Data Register
    Neighbor Pairing") -- previously refused outright as "unsupported
    Type3a long-word access"."""

    def test_pair_load_post_modify_real_instance(self):
        # real SW 0x1c2920 (1845536), the frame-render stop this fixed
        # (tools/sharc_harness.py's FRAME_MILESTONE docstring):
        # {"compute": 0, "cond": 31, "d": 0, "g": 0, "i": 1, "l": 1,
        # "m": 4, "u": 1, "ureg": 0} -- an unconditional (cond=31), no-op
        # compute, DM (g=0) load into the R0:R1 pair, post-modify (u=1) by
        # I1/M4.
        fields = {
            "compute": 0,
            "cond": 31,
            "d": 0,
            "g": 0,
            "i": 1,
            "l": 1,
            "m": 4,
            "u": 1,
            "ureg": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I1"]: T.Const(0x30001000),
                # The firmware steps (lw) pairs with M = 2 (sw 0x1c11fe,
                # 0xb80295: "M4 = 0x2" before "R0 = DM(I4, M4) (lw)").
                T.UREG_CODES["M4"]: T.Const(2),
            },
            concrete=loader_memory(),
            assume_nw32=True,
        )
        self.assertTrue(T._dm_write(state, 0x30001000, 4, T.Const(0xAAAA)))
        self.assertTrue(T._dm_write(state, 0x30001004, 4, T.Const(0xBBBB)))
        [result] = T._execute(state, insn("3a", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["action"], "load")
        self.assertEqual(load["space"], "DM")
        self.assertEqual(load["access_width"], "long-word")
        self.assertEqual(load["addressing_mode"], "post-modify")
        self.assertEqual(load["ureg_pair"], ["R0", "R1"])
        self.assertEqual(load["address"], 0x30001000)
        # R0 (low word, the named ureg) <- address; R1 (its neighbor) <-
        # address + 4 (PRM Figure 7-19: the named RX gets the low 32 bits,
        # its neighbor RY the high 32 bits of one 64-bit access).
        self.assertEqual(result.uregs[T.UREG_CODES["R0"]], T.Const(0xAAAA))
        self.assertEqual(result.uregs[T.UREG_CODES["R1"]], T.Const(0xBBBB))
        # Post-modify: an (lw) modifier is scaled like a normal word, by 4
        # in byte space (PRM p.6-9 "except in the case of (lw)"; Table 6-2
        # "Rm = dm(In, mod) (lw)": scaled_mod = mod << 2), so M4 = 2 steps
        # one 64-bit pair.
        self.assertEqual(result.uregs[T.UREG_CODES["I1"]], T.Const(0x30001008))

    def test_pair_store_pre_modify(self):
        fields = {
            "compute": 0,
            "cond": 31,
            "d": 1,
            "g": 0,
            "i": 2,
            "l": 1,
            "m": 3,
            "u": 0,
            "ureg": 4,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I2"]: T.Const(0x30002000),
                T.UREG_CODES["M3"]: T.Const(2),
                T.UREG_CODES["R4"]: T.Const(0x11111111),
                T.UREG_CODES["R5"]: T.Const(0x22222222),
            },
            concrete=loader_memory(),
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("3a", fields, length=6))
        self.assertIsNone(result.stopped)
        store = result.trace[-1]
        self.assertEqual(store["action"], "store")
        self.assertEqual(store["access_width"], "long-word")
        self.assertEqual(store["addressing_mode"], "pre-modify")
        self.assertEqual(store["ureg_pair"], ["R4", "R5"])
        self.assertTrue(store["concrete_write"])
        # Pre-modify: the address for this access is I2 + M3 * 4 = 0x30002008;
        # I2 itself is left unchanged (matching Type3a/3d/4a's own convention).
        self.assertEqual(store["address"], 0x30002008)
        self.assertEqual(result.uregs[T.UREG_CODES["I2"]], T.Const(0x30002000))
        self.assertEqual(T._dm_read(result, 0x30002008, 4), T.Const(0x11111111))
        self.assertEqual(T._dm_read(result, 0x3000200C, 4), T.Const(0x22222222))

    def test_odd_ureg_pairs_with_the_register_below_it(self):
        # R1 (explicit, odd) pairs with R0 (its neighbor), not R2 (PRM
        # p.6-5's odd DAG-register case, mirrored onto R/S/I/M/L/B): the
        # named register is always the low half regardless of parity.
        fields = {
            "compute": 0,
            "cond": 31,
            "d": 0,
            "g": 0,
            "i": 1,
            "l": 1,
            "m": 4,
            "u": 1,
            "ureg": 1,
        }
        state = T.State(
            0x10,
            {T.UREG_CODES["I1"]: T.Const(0x30001000)},
            concrete=loader_memory(),
        )
        self.assertTrue(T._dm_write(state, 0x30001000, 4, T.Const(0xCCCC)))
        self.assertTrue(T._dm_write(state, 0x30001004, 4, T.Const(0xDDDD)))
        [result] = T._execute(state, insn("3a", fields, length=6))
        self.assertIsNone(result.stopped)
        load = result.trace[-1]
        self.assertEqual(load["ureg_pair"], ["R1", "R0"])
        self.assertEqual(result.uregs[T.UREG_CODES["R1"]], T.Const(0xCCCC))
        self.assertEqual(result.uregs[T.UREG_CODES["R0"]], T.Const(0xDDDD))


class UndocumentedFormsStopWithReasonTest(unittest.TestCase):
    """8p_undoc48/21p_undoc16/22p_undoc48: confirmed real words in places
    but with no known semantics (docs/findings/05-sharc-isa-and-decoding.md
    lines 330, 528-530). These must stop with a specific, informative
    reason rather than silently guessing or falling through to the
    generic "unsupported form" message."""

    def test_stops_name_the_form_and_do_not_touch_state(self):
        cases = (
            # real SW 0x1c32b4 (1847988).
            (
                "8p_undoc48",
                {
                    "a": 0,
                    "b": 0,
                    "ci": 0,
                    "cond[4:0]": 0,
                    "imm[15:0]": 0,
                    "imm[23:16]": 62,
                    "j": 0,
                    "r": 1,
                },
            ),
            # real SW 0x16b8f7 (1489143).
            ("21p_undoc16", {"operand[6:0]": 0}),
            # real SW 0x16b8fb (1489147).
            ("22p_undoc48", {"operand[6:0]": 56}),
        )
        for name, fields in cases:
            with self.subTest(name=name):
                state = T.State(0x10, {0: T.Const(9)})
                [result] = T._execute(state, insn(name, fields, length=6))
                self.assertEqual(
                    result.stopped,
                    "undocumented form %s has no confirmed semantics" % name,
                )
                self.assertEqual(result.pc_sw, 0x10)
                self.assertEqual(result.uregs, {0: T.Const(9)})

    def test_provisional_nop_is_off_by_default_even_with_other_forms_named(self):
        # Naming a *different* form in provisional_interpretations must not
        # change this form's stop -- the opt-in is per exact form name, not
        # "any provisional interpretation is active".
        state = T.State(
            0x10, {0: T.Const(9)}, provisional_interpretations={"22p_undoc48": "nop"}
        )
        fields = {"operand[6:0]": 0}
        [result] = T._execute(state, insn("21p_undoc16", fields, length=2))
        self.assertEqual(
            result.stopped,
            "undocumented form 21p_undoc16 has no confirmed semantics",
        )
        self.assertEqual(result.provisional_interpreted, ())

    def test_provisional_nop_advances_and_touches_no_register(self):
        # real SW 0x1c32b0 (1847984): the form this session's opt-in
        # --provisional 21p_undoc16=nop experiment is about (see
        # docs/findings for what is/is not established -- this is an
        # experiment aid, not a semantics claim).
        state = T.State(
            0x10, {0: T.Const(9)}, provisional_interpretations={"21p_undoc16": "nop"}
        )
        fields = {"operand[6:0]": 0x25}
        [result] = T._execute(state, insn("21p_undoc16", fields, length=2))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.pc_sw, 0x11)
        self.assertEqual(result.uregs, {0: T.Const(9)})
        self.assertEqual(result.provisional_interpreted, ("21p_undoc16",))

    def test_provisional_nop_logs_once_per_execution(self):
        state = T.State(
            0x10, {0: T.Const(9)}, provisional_interpretations={"21p_undoc16": "nop"}
        )
        fields = {"operand[6:0]": 0x25}
        [state] = T._execute(state, insn("21p_undoc16", fields, length=2))
        [state] = T._execute(state, insn("21p_undoc16", fields, length=2))
        self.assertEqual(state.provisional_interpreted, ("21p_undoc16", "21p_undoc16"))

    def test_provisional_mode_other_than_nop_is_not_implemented(self):
        # Only "nop" is a recognised mode; anything else (e.g. a typo'd
        # --provisional value) must fall through to the safe stop, not
        # silently execute as a no-op.
        state = T.State(
            0x10, {0: T.Const(9)}, provisional_interpretations={"21p_undoc16": "bogus"}
        )
        fields = {"operand[6:0]": 0x25}
        [result] = T._execute(state, insn("21p_undoc16", fields, length=2))
        self.assertEqual(
            result.stopped,
            "undocumented form 21p_undoc16 has no confirmed semantics",
        )


class CircularWrapConstTest(unittest.TestCase):
    """Direct tests of the new _circular_wrap_const helper (PRM Sec. 6,
    "Circular Buffering"): I in [B, B+L), modifier magnitude up to and
    including L, both directions."""

    def test_forward_wrap_crosses_the_top(self):
        # base=0x1000, length=0x10 -> [0x1000, 0x1010). index=0x100c,
        # delta=+8 would land at 0x1014, past the top: wrap to 0x1004.
        self.assertEqual(T._circular_wrap_const(0x100C, 0x1000, 0x10, 8), 0x1004)

    def test_backward_wrap_crosses_the_bottom(self):
        # index=0x1004, delta=-8 would land at 0x0FFC, below the bottom:
        # wrap to 0x100C.
        self.assertEqual(T._circular_wrap_const(0x1004, 0x1000, 0x10, -8), 0x100C)

    def test_modifier_magnitude_equal_to_length_is_the_identity(self):
        # A full lap (|delta| == L) returns exactly where it started, from
        # any position in the buffer -- this is the off-by-one this
        # session fixed (Type19a_scaled used to reject delta == L).
        self.assertEqual(T._circular_wrap_const(0x1003, 0x1000, 0x10, 0x10), 0x1003)
        self.assertEqual(T._circular_wrap_const(0x1003, 0x1000, 0x10, -0x10), 0x1003)

    def test_no_wrap_needed_inside_the_buffer(self):
        self.assertEqual(T._circular_wrap_const(0x1004, 0x1000, 0x10, 2), 0x1006)

    def test_magnitude_greater_than_length_is_unsupported(self):
        self.assertIsNone(T._circular_wrap_const(0x1004, 0x1000, 0x10, 0x11))


class Type19aScaledCircularFixTest(unittest.TestCase):
    """End-to-end regression for the Type19a_scaled off-by-one this
    session fixed: a modifier whose magnitude equals L used to be rejected
    (Unknown) even though the PRM allows it (a full lap)."""

    def test_modifier_equal_to_length_now_wraps_instead_of_unknown(self):
        fields = {
            "g": 0,
            "is": 0,
            "idis": 0,
            "data[31:16]": 0,
            "data[15:0]": 0x10,  # delta = +0x10
            "w": 1,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x1003),
                T.UREG_CODES["B0"]: T.Const(0x1000),
                T.UREG_CODES["L0"]: T.Const(0x10),
            },
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("19a_scaled", fields, length=4))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.uregs[T.UREG_CODES["I0"]], T.Const(0x1003))
        event = result.trace[-1]
        self.assertTrue(event["circular"])

    def test_modifier_one_more_than_length_is_still_unknown(self):
        fields = {
            "g": 0,
            "is": 0,
            "idis": 0,
            "data[31:16]": 0xFFFF,
            "data[15:0]": 0xFFFC,  # delta = -1 word; scaled below
            "w": 1,
        }
        # Use a length of 4 bytes (L=1 word * scale 4) and a delta whose
        # scaled magnitude (8 bytes) exceeds it, forcing the "still
        # unsupported" branch.
        fields["data[31:16]"] = 0
        fields["data[15:0]"] = 2  # +2 words -> scaled +8 bytes with w=1(nw32 scale 4)
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x1000),
                T.UREG_CODES["B0"]: T.Const(0x1000),
                T.UREG_CODES["L0"]: T.Const(1),  # 1 word == 4 bytes with assume_nw32
            },
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("19a_scaled", fields, length=4))
        self.assertIsNone(result.stopped)
        self.assertIsInstance(result.uregs[T.UREG_CODES["I0"]], T.Unknown)


class Type7aCircularModifyTest(unittest.TestCase):
    """Type7a's conditional (cond=0x17) MODIFY circular-wrap fix: L!=0 used
    to always stop; a Const case should now wrap."""

    def test_conditional_modify_wraps_when_predicate_true(self):
        fields = {
            "cond[4:0]": 0x17,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 0,
            "m[2:0]": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x100C),
                T.UREG_CODES["M0"]: T.Const(4),
                T.UREG_CODES["L0"]: T.Const(0x10),
                T.UREG_CODES["B0"]: T.Const(0x1000),
                # SV_BIT clear -> cond 0x17 (NOT SV) resolves True.
                T.UREG_CODES["ASTATX"]: T.Const(0),
            },
        )
        [result] = T._execute(state, insn("7a", fields, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.uregs[T.UREG_CODES["I0"]], T.Const(0x1000))
        self.assertTrue(result.trace[-1]["circular"])

    def test_nonconcrete_bound_with_nonzero_length_still_stops(self):
        fields = {
            "cond[4:0]": 0x17,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 0,
            "m[2:0]": 0,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I0"]: T.Const(0x100C),
                T.UREG_CODES["M0"]: T.Const(4),
                T.UREG_CODES["L0"]: T.Const(0x10),
                # B0 left unset (Unknown): cannot compute a concrete wrap.
                T.UREG_CODES["ASTATX"]: T.Const(0),
            },
        )
        [result] = T._execute(state, insn("7a", fields, length=6))
        self.assertEqual(result.stopped, "unsupported Type7a circular modify")


class Type7aShortWordScaleTest(unittest.TestCase):
    """MODIFY's (sw)/(nw) address-scale selector (PRM p.348's "BH (Type 7a)"
    encode table, bits 39/23 -- decode_table.json's "w"/"l" fields): real SW
    0x1c50af (dt2-1.16, the voice-render inner loop's ``modify(I4, M3)``
    that seeks I4 to the current sample before this loop's short-word
    reads) decodes w=0, l=1 -- "(sw)" -- so M3 must scale by 2 (short-word),
    not 4 (normal-word, this tracer's behaviour before l was decoded)."""

    def test_short_word_modify_scales_by_2_not_4(self):
        fields = {
            "w": 0,
            "l": 1,
            "cond[4:0]": 31,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 1,
            "m[2:0]": 3,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I4"]: T.Const(0x2000),
                T.UREG_CODES["M3"]: T.Const(6),
            },
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("7a", fields, length=6))
        self.assertIsNone(result.stopped)
        # source is[2:2],is[1:0] = 0b100 -> I4; idis=0 -> destination I4 too.
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x2000 + 6 * 2))

    def test_normal_word_modify_unaffected_by_l_absent(self):
        # Same shape with l omitted: (w, l) = (1, 0) is (nw) and scales by
        # 4 (the blank (0, 0) row is the unscaled plain MODIFY, see
        # ModifyScaleTest).
        fields = {
            "w": 1,
            "cond[4:0]": 31,
            "g": 0,
            "idis[2:0]": 0,
            "is[1:0]": 0,
            "is[2:2]": 1,
            "m[2:0]": 3,
            "compute[22:16]": 0,
            "compute[15:0]": 0,
        }
        state = T.State(
            0x10,
            {
                T.UREG_CODES["I4"]: T.Const(0x2000),
                T.UREG_CODES["M3"]: T.Const(6),
            },
            assume_nw32=True,
        )
        [result] = T._execute(state, insn("7a", fields, length=6))
        self.assertIsNone(result.stopped)
        self.assertEqual(result.uregs[T.UREG_CODES["I4"]], T.Const(0x2000 + 6 * 4))


if __name__ == "__main__":
    unittest.main()
