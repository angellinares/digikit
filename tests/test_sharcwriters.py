"""Synthetic tests for tools/sharcwriters.py -- no firmware required for the
pure logic (census form rules, width tables, address classifier). A couple
of integration tests exercise the real DT2 1.16 SHARC image and skip when
it is not present (out/sections/... is never committed, see CLAUDE.md)."""

import os
import pathlib
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))

W = import_module("sharcwriters")
Instruction = import_module("sharc_disasm").Instruction
trace_mod = import_module("sharc_trace")

BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")


def insn(form, fields, length=4, kind="confident"):
    return Instruction(0, length, form, fields, kind=kind)


class StoreSpaceAndDirectionTest(unittest.TestCase):
    def test_simple_form_stores_dm_when_d1_g0(self):
        self.assertEqual(W.store_space_and_direction("15b", {"d": 1, "g": 0}), (True, True))

    def test_simple_form_stores_pm_when_d1_g1(self):
        self.assertEqual(W.store_space_and_direction("15b", {"d": 1, "g": 1}), (True, False))

    def test_simple_form_load_when_d0(self):
        self.assertEqual(W.store_space_and_direction("15b", {"d": 0, "g": 0}), (False, False))

    def test_type3c_d_is_a_real_variable_bit(self):
        self.assertEqual(W.store_space_and_direction("3c", {"d": 1}), (True, True))
        self.assertEqual(W.store_space_and_direction("3c", {"d": 0}), (False, False))

    def test_missing_g_defaults_to_dm(self):
        # A form whose g bit happens to be absent from a merged field dict
        # (should not occur in practice) is treated as DM, not silently
        # dropped.
        self.assertEqual(W.store_space_and_direction("3a", {"d": 1}), (True, True))

    def test_immediate_forms_always_store_dm_iff_g0(self):
        self.assertEqual(W.store_space_and_direction("16a", {"g": 0}), (True, True))
        self.assertEqual(W.store_space_and_direction("16b", {"g": 1}), (True, False))

    def test_dual_forms_store_dm_side_on_dmd(self):
        self.assertEqual(W.store_space_and_direction("1a", {"dmd": 1, "pmd": 0}), (True, True))
        self.assertEqual(W.store_space_and_direction("1a", {"dmd": 0, "pmd": 1}), (False, False))
        # The PM-side pmd bit does not affect the DM side's own classification.
        self.assertEqual(W.store_space_and_direction("1b", {"dmd": 1, "pmd": 1}), (True, True))

    def test_unknown_form_is_not_a_store(self):
        self.assertEqual(W.store_space_and_direction("21a", {}), (False, False))


class StaticStoreWidthTest(unittest.TestCase):
    def test_14a_15a_scalar_vs_pair(self):
        for form in ("14a", "15a"):
            self.assertEqual(W.static_store_width(form, {"l": 0}), 4)
            self.assertEqual(W.static_store_width(form, {"l": 1}), 8)

    def test_15b_long_word_flag(self):
        self.assertEqual(W.static_store_width("15b", {"l": 0}), 4)
        self.assertEqual(W.static_store_width("15b", {"l": 1}), 8)

    def test_fixed_normal_word_forms(self):
        for form in ("3a", "4a", "6a_mem", "3c", "16a", "16b", "1a", "1b"):
            self.assertEqual(W.static_store_width(form, {}), 4)

    def test_14d_byte_or_short_word(self):
        self.assertEqual(W.static_store_width("14d", {"l": 0}), 1)
        self.assertEqual(W.static_store_width("14d", {"l": 1}), 2)

    def test_3b_lxw_table(self):
        cases = {(0, 1, 1): 4, (0, 0, 0): 1, (0, 1, 0): 1, (1, 0, 0): 2, (1, 1, 0): 2, (1, 1, 1): 8}
        for (l, x, w), width in cases.items():
            self.assertEqual(W.static_store_width("3b", {"l": l, "x": x, "w": w}), width)

    def test_3d_mirrors_3b_table(self):
        self.assertEqual(W.static_store_width("3d", {"l": 1, "x": 1, "w": 1}), 8)

    def test_4b_lxw_table(self):
        cases = {(1, 1, 1): 4, (0, 0, 0): 1, (1, 0, 0): 2, (0, 1, 0): 1, (1, 1, 0): 2}
        for (l, x, w), width in cases.items():
            self.assertEqual(W.static_store_width("4b", {"l": l, "x": x, "w": w}), width)

    def test_4d_mirrors_4b_table(self):
        self.assertEqual(W.static_store_width("4d", {"l": 1, "x": 1, "w": 1}), 4)

    def test_undocumented_lxw_combination_is_none(self):
        self.assertIsNone(W.static_store_width("3b", {"l": 1, "x": 0, "w": 1}))

    def test_unknown_form_is_none(self):
        self.assertIsNone(W.static_store_width("21a", {}))


class EventStoreWidthTest(unittest.TestCase):
    def test_prefers_access_width(self):
        self.assertEqual(W.event_store_width("3b", {"access_width": "byte"}), 1)
        self.assertEqual(W.event_store_width("3b", {"access_width": "long-word"}), 8)

    def test_15b_long_word_flag_fallback(self):
        self.assertEqual(W.event_store_width("15b", {"long_word": True}), 8)
        self.assertEqual(W.event_store_width("15b", {"long_word": False}), 4)

    def test_forms_without_an_access_width_key_default_normal_word(self):
        for form in ("3a", "4a", "6a_mem", "3c", "16a", "16b", "14a"):
            self.assertEqual(W.event_store_width(form, {}), 4)

    def test_unrecognized_access_width_is_none(self):
        self.assertIsNone(W.event_store_width("3b", {"access_width": "bogus"}))


class CensusInstructionsTest(unittest.TestCase):
    def test_collects_only_dm_stores(self):
        rows = W.census_instructions([
            (0x10, insn("15b", {"d": 1, "g": 0, "l": 0})),   # DM store
            (0x14, insn("15b", {"d": 1, "g": 1, "l": 0})),   # PM store -- excluded from is_dm
            (0x18, insn("15b", {"d": 0, "g": 0, "l": 0})),   # load -- not a store at all
            (0x1c, insn("2a", {})),                          # not a store-capable form
        ])
        self.assertEqual([r["pc"] for r in rows], [0x10, 0x14])
        self.assertTrue(rows[0]["is_dm"])
        self.assertFalse(rows[1]["is_dm"])

    def test_width_is_attached_per_row(self):
        rows = W.census_instructions([(0x10, insn("14a", {"d": 1, "g": 0, "l": 1}))])
        self.assertEqual(rows[0]["width"], 8)


class ChooseStoreEventTest(unittest.TestCase):
    def test_empty_is_none(self):
        self.assertIsNone(W.choose_store_event([]))

    def test_prefers_const_over_affine_over_unresolved(self):
        const_ev = {"address": 0x100}
        affine_ev = {"address": {"affine": {"constant": 0, "terms": [["I6e", 1]]}}}
        unknown_ev = {"address": {"unknown": "uninitialized I3"}}
        self.assertIs(W.choose_store_event([affine_ev, unknown_ev, const_ev]), const_ev)
        self.assertIs(W.choose_store_event([unknown_ev, affine_ev]), affine_ev)

    def test_deterministic_among_equal_rank(self):
        a = {"address": 0x200}
        b = {"address": 0x100}
        self.assertIs(W.choose_store_event([a, b]), b)
        self.assertIs(W.choose_store_event([b, a]), b)


class AffineRangeTest(unittest.TestCase):
    def test_single_positive_term(self):
        lo, hi = W.affine_range(0, [("I7e", 1)], 0x100, 0x200)
        self.assertEqual((lo, hi), (0x100, 0x1FF))

    def test_constant_offset(self):
        lo, hi = W.affine_range(0xFFFFFFF4, [("I6e", 1)], 0x100, 0x200)  # constant == -12
        self.assertEqual((lo, hi), (0x100 - 12, 0x1FF - 12))

    def test_negative_coefficient_flips_bounds(self):
        # A coefficient of -1 (stored mod 2**32) negates the term's range;
        # the result is itself reduced mod 2**32, like Affine's own values.
        minus_one = 0xFFFFFFFF
        lo, hi = W.affine_range(0, [("I7e", minus_one)], 0x100, 0x200)
        self.assertEqual((lo, hi), (0x100000000 - 0x1FF, 0x100000000 - 0x100))

    def test_two_terms_sum(self):
        lo, hi = W.affine_range(0, [("I6e", 1), ("I7e", 1)], 0x100, 0x200)
        self.assertEqual((lo, hi), (0x200, 0x3FE))


class RangesOverlapTest(unittest.TestCase):
    def test_no_overlap_below(self):
        self.assertFalse(W.ranges_overlap(0x300, 0x400, 4, 0x100))

    def test_no_overlap_above(self):
        self.assertFalse(W.ranges_overlap(0x300, 0x400, 4, 0x500))

    def test_overlap_at_low_edge(self):
        self.assertTrue(W.ranges_overlap(0x100, 0x200, 4, 0x100))

    def test_requested_range_later_bytes_and_adjacent_boundary(self):
        # A possible store at 0x10c hits only the final four bytes of the
        # requested [0x100, 0x110) range; 0x110 is exactly adjacent.
        self.assertTrue(W.ranges_overlap(0x10C, 0x10C, 4, 0x100, 16))
        self.assertFalse(W.ranges_overlap(0x110, 0x110, 4, 0x100, 16))

    def test_overlap_at_high_edge_needs_width(self):
        # target sits just past range_hi, but a `width`-byte store starting
        # at range_hi still reaches it.
        self.assertTrue(W.ranges_overlap(0x100, 0x200, 4, 0x203))
        self.assertFalse(W.ranges_overlap(0x100, 0x200, 4, 0x204))


class ClassifyStoreAddressTest(unittest.TestCase):
    TARGET = 0x252658

    def test_hit(self):
        cls, detail = W.classify_store_address(self.TARGET, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "HIT")
        self.assertEqual(detail["address"], self.TARGET)

    def test_hit_covers_the_whole_width(self):
        cls, _ = W.classify_store_address(self.TARGET - 2, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "HIT")

    def test_requested_range_width_is_used_for_overlap(self):
        # A store into the later bytes of a requested range is still a hit.
        cls, _ = W.classify_store_address(self.TARGET + 4, 4, self.TARGET, 0x26F000, 0x2C0000, 16)
        self.assertEqual(cls, "HIT")

    def test_excluded_const(self):
        cls, _ = W.classify_store_address(0x254D9C, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "EXCLUDED-CONST")

    def test_not_reached_is_unresolved(self):
        cls, detail = W.classify_store_address(None, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertIn("not reached", detail["reason"])

    def test_unknown_width_is_unresolved(self):
        cls, detail = W.classify_store_address(0x100, None, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertIn("width", detail["reason"])

    def test_pure_stack_term_excluded_when_out_of_range(self):
        addr = {"affine": {"constant": 0, "terms": [["I7e", 1]]}}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "EXCLUDED-STACK")
        self.assertEqual(detail["range"], [0x26F000, 0x2BFFFF])

    def test_pure_stack_term_without_bounds_is_stack_relative(self):
        addr = {"affine": {"constant": 0, "terms": [["I6e", 1]]}}
        cls, _ = W.classify_store_address(addr, 4, self.TARGET, None, None)
        self.assertEqual(cls, "STACK-RELATIVE")

    def test_stack_term_overlapping_target_is_unresolved_not_excluded(self):
        # Construct a target inside the stack-relative range so exclusion
        # would be wrong: never silently call this HIT either (it is not a
        # resolved Const).
        addr = {"affine": {"constant": 0, "terms": [["I7e", 1]]}}
        cls, detail = W.classify_store_address(addr, 4, 0x26F050, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertIn("overlaps", detail["reason"])

    def test_loaded_pointer(self):
        addr = {"unknown": "memory-address I8 + 0x10"}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "LOADED-POINTER")
        self.assertEqual(detail["load_expression"], "I8 + 0x10")

    def test_generic_unknown_is_unresolved(self):
        addr = {"unknown": "uninitialized I3"}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertEqual(detail["reason"], "uninitialized I3")

    def test_partial_const_is_unresolved(self):
        addr = {"partial": {"known_mask": 0xFF, "known_bits": 0x12}}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertEqual(detail["known_mask"], 0xFF)

    def test_entry_relative_register_other_than_frame(self):
        addr = {"affine": {"constant": 0x10, "terms": [["I3e", 1]]}}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "ENTRY-RELATIVE")
        self.assertEqual(detail["registers"], ["I3"])

    def test_entry_relative_lists_frame_register_too_when_mixed(self):
        # DM(I7, M7) -- the ordinary push/pop idiom -- is not excludable by
        # the stack bounds alone (M7 is unconstrained), but the report must
        # not hide that I7 (the frame/stack pointer) drives the address.
        addr = {"affine": {"constant": 0, "terms": [["I7e", 1], ["M7e", 8]]}}
        cls, detail = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "ENTRY-RELATIVE")
        self.assertEqual(detail["registers"], ["I7", "M7"])

    def test_zero_terms_affine_collapses_like_const(self):
        addr = {"affine": {"constant": self.TARGET, "terms": []}}
        cls, _ = W.classify_store_address(addr, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "HIT")

    def test_unrecognized_representation_is_unresolved(self):
        cls, detail = W.classify_store_address([1, 2, 3], 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertIn("unrecognized", detail["reason"])

    def test_bool_address_is_unresolved(self):
        # bool is a subclass of int in Python; must not be treated as Const.
        cls, _ = W.classify_store_address(True, 4, self.TARGET, 0x26F000, 0x2C0000)
        self.assertEqual(cls, "UNRESOLVED")


class SeedSetsTest(unittest.TestCase):
    def test_every_i_m_b_register_is_a_distinct_symbol_without_global_constants(self):
        sets = W.seed_sets(seed_global_constants=False)
        symbols = {v for k, v in sets.items() if k.startswith(("I", "M", "B"))}
        self.assertEqual(len(symbols), 48)  # 16 each of I/M/B, all distinct

    def test_l_registers_are_concrete_zero_without_global_constants(self):
        sets = W.seed_sets(seed_global_constants=False)
        for n in range(16):
            self.assertEqual(sets["L%d" % n], 0)

    def test_stack_symbols_are_seeded_from_i6_i7(self):
        sets = W.seed_sets(seed_global_constants=False)
        self.assertEqual(sets["I6"], "@I6e")
        self.assertEqual(sets["I7"], "@I7e")
        self.assertEqual(set(W.STACK_SYMBOLS), {"I6e", "I7e"})

    def test_default_seeds_global_constants(self):
        sets = W.seed_sets()
        for reg, value in W.GLOBAL_CONSTANT_SEEDS.items():
            self.assertEqual(sets[reg], value)

    def test_global_constants_are_concrete_ints_not_symbols(self):
        sets = W.seed_sets()
        for reg in W.GLOBAL_CONSTANT_SEEDS:
            self.assertIsInstance(sets[reg], int)

    def test_disabling_global_constants_restores_symbolic_seeding(self):
        with_consts = W.seed_sets(seed_global_constants=True)
        without = W.seed_sets(seed_global_constants=False)
        # Every M register GLOBAL_CONSTANT_SEEDS covers is symbolic when the
        # option is off, concrete when it is on.
        for reg in W.GLOBAL_CONSTANT_SEEDS:
            if reg.startswith("M"):
                self.assertTrue(str(without[reg]).startswith("@"))
                self.assertNotEqual(with_consts[reg], without[reg])

    def test_non_qualifying_l_registers_keep_the_zero_default_either_way(self):
        # L3/L4/L15 did not qualify in Task 1 (see the GLOBAL_CONSTANT_SEEDS
        # docstring): they keep the pragmatic L=0 default regardless of the
        # option, rather than becoming newly symbolic.
        with_consts = W.seed_sets(seed_global_constants=True)
        without = W.seed_sets(seed_global_constants=False)
        for reg in ("L3", "L4", "L15"):
            self.assertNotIn(reg, W.GLOBAL_CONSTANT_SEEDS)
            self.assertEqual(with_consts[reg], 0)
            self.assertEqual(without[reg], 0)

    def test_qualifying_l_registers_are_corrected_from_zero(self):
        # L6/L7's real value (0x1fd) differs from the L=0 default the
        # option overrides -- this is a correction, not just a narrowing.
        without = W.seed_sets(seed_global_constants=False)
        with_consts = W.seed_sets(seed_global_constants=True)
        for reg in ("L6", "L7"):
            self.assertEqual(without[reg], 0)
            self.assertEqual(with_consts[reg], 0x1FD)


@unittest.skipUnless(BLOB.exists(), "DT2 1.16 SHARC loader is not available")
class IntegrationTest(unittest.TestCase):
    """The tool end-to-end against the real image, bounded to the single
    function containing the control target instead of the whole image (the
    full W.run() over sharcinv.CODE_BLOCKS took the suite from ~3s to
    ~107s -- see CLAUDE.md/the task brief -- and re-tracing the other
    ~1200 functions proves nothing this test does not already check with
    just the one). This is the control described in the task: DM(0x254d9c)
    = R2 at sw 0x1c191d (a direct Type14a store), in the function at entry
    sw 0x1c18a6 (blk93@0x1c18a6), must classify as HIT."""

    @classmethod
    def setUpClass(cls):
        sharcfn = import_module("sharcfn")
        sharcinv = import_module("sharcinv")

        cls.target = 0x254D9C
        cls.ctx = sharcfn.load_context(str(BLOB), (93,), 8)
        cls.fn = cls.ctx["by_id"]["blk93@0x1c18a6"]
        block = cls.ctx["analyzed"][cls.fn["block"]]
        sw_insns = sharcinv.instructions_in(block, cls.fn["entry"], cls.fn["exit"])
        cls.rows = W.census_instructions(sw_insns)
        dm_rows = [row for row in cls.rows if row["is_dm"]]
        chosen, stop_reasons = W.resolve_function(
            cls.ctx, cls.fn, dm_rows, max_steps=4000, max_states=128)
        cls.classified = []
        for row in dm_rows:
            cls_, detail, width = W.classify_row(
                row, chosen.get(row["pc"]), stop_reasons, cls.target,
                fallback_width=4, stack_lo=W.DEFAULT_STACK_LO, stack_hi=W.DEFAULT_STACK_HI)
            cls.classified.append({"pc": row["pc"], "form": row["form"], "class": cls_, **detail})

    def test_control_target_is_a_hit_at_the_known_pc(self):
        hits = [row for row in self.classified if row["class"] == "HIT"]
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]["pc"], 0x1C191D)
        self.assertEqual(hits[0]["form"], "14a")

    def test_class_totals_sum_to_census_total(self):
        dm_total = sum(1 for row in self.rows if row["is_dm"])
        self.assertEqual(len(self.classified), dm_total)

    def test_census_form_set_matches_the_documented_forms(self):
        forms = {row["form"] for row in self.rows if row["is_dm"]}
        self.assertTrue(forms <= set(W.ALL_STORE_FORMS))


class CircularModifyEndToEndTest(unittest.TestCase):
    """No firmware required: runs the two real instructions -- a circular
    Type19a_scaled MODIFY of I7 seeded exactly as seed_sets() seeds it (a
    bare entry symbol, B7/L7 not concrete), then a Type16a store through
    the modified I7 -- via tools/sharc_trace.py's own single-instruction
    executor, and checks the resulting store event classifies as
    EXCLUDED-STACK, over the CIRC_WRAP_SLACK-widened range. This is the
    out/sharcwriters/stack-invariant.md fix end to end: before it, the
    MODIFY produced Unknown('scaled circular modify I7') and the store
    below would classify UNRESOLVED."""

    MODIFY_FIELDS = {
        "w": 1, "g": 0, "idis[2:0]": 0, "is[2:0]": 7,
        "data[31:16]": 0xFFFF, "data[15:0]": 0xFFFE,
    }
    STORE_FIELDS = {"i[2:0]": 7, "m[2:0]": 0, "g": 0, "sl": 0, "by": 0,
                    "data[31:16]": 0, "data[15:0]": 0}

    def _modify_then_store(self, pc):
        i7 = trace_mod.UREG_CODES["I7"]
        seeds = W.seed_sets()
        self.assertEqual(seeds["I7"], "@I7e")  # sanity: still a bare @-seed
        state = trace_mod.State(pc, {i7: trace_mod.symbol("I7e"),
                                      trace_mod.UREG_CODES["L7"]: trace_mod.Const(seeds["L7"])})
        modified = trace_mod._execute(
            state, Instruction(0, 6, "19a_scaled", self.MODIFY_FIELDS, kind="confident")
        )[0]
        # Never the old bug's behaviour (reusing "I7e" itself -- a false
        # claim that the wrapped value equals the pre-modify entry value).
        self.assertNotEqual(modified.uregs[i7], trace_mod.symbol("I7e"))
        stored = trace_mod._execute(
            modified, Instruction(0, 6, "16a", self.STORE_FIELDS, kind="confident")
        )[0]
        event = stored.trace[-1]
        self.assertEqual(event["action"], "store")
        return event

    def test_store_through_circularly_modified_i7_is_excluded_stack_over_widened_range(self):
        event = self._modify_then_store(pc=0)
        cls, detail, _width = W.classify_row(
            {"pc": 0, "form": "16a", "width": 4}, event, set(),
            target=0x252658, fallback_width=4,
            stack_lo=W.DEFAULT_STACK_LO, stack_hi=W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "EXCLUDED-STACK")
        self.assertEqual(detail["range"], [
            W.DEFAULT_STACK_LO - W.CIRC_WRAP_SLACK,
            W.DEFAULT_STACK_HI + W.CIRC_WRAP_SLACK - 1,
        ])
        self.assertTrue(detail["via_circular_modify"])

    def test_two_different_modify_sites_both_excluded_but_not_asserted_equal(self):
        # Two different program points, each doing its own circular MODIFY
        # of I7's bare entry symbol, then a store through the result: both
        # must classify EXCLUDED-STACK (same evidenced bound), but the
        # underlying addresses must not be asserted equal to each other --
        # that would alias two provably-different stack frames.
        event_a = self._modify_then_store(pc=0x10)
        event_b = self._modify_then_store(pc=0x20)
        self.assertNotEqual(event_a["address"], event_b["address"])
        for event in (event_a, event_b):
            cls, detail, _width = W.classify_row(
                {"pc": 0, "form": "16a", "width": 4}, event, set(),
                target=0x252658, fallback_width=4,
                stack_lo=W.DEFAULT_STACK_LO, stack_hi=W.DEFAULT_STACK_HI)
            self.assertEqual(cls, "EXCLUDED-STACK")


class CircSymbolClassifierTest(unittest.TestCase):
    """Pure classifier tests for the circ_-tagged symbol family -- no
    firmware, no tracer execution, synthetic addresses only."""

    TARGET = 0x252658

    def test_is_circ_symbol(self):
        self.assertTrue(W.is_circ_symbol(trace_mod.CIRC_SYMBOL_PREFIX + "7_10"))
        self.assertFalse(W.is_circ_symbol("I7e"))
        self.assertFalse(W.is_circ_symbol("M7e"))

    def test_bare_circ_symbol_is_excluded_stack_over_widened_range(self):
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1c1676"
        addr = {"affine": {"constant": 0, "terms": [[name, 1]]}}
        cls, detail = W.classify_store_address(
            addr, 4, self.TARGET, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "EXCLUDED-STACK")
        self.assertEqual(detail["range"], [
            W.DEFAULT_STACK_LO - W.CIRC_WRAP_SLACK,
            W.DEFAULT_STACK_HI + W.CIRC_WRAP_SLACK - 1,
        ])
        self.assertTrue(detail["via_circular_modify"])

    def test_plain_stack_symbol_does_not_set_via_circular_modify(self):
        addr = {"affine": {"constant": 0, "terms": [["I7e", 1]]}}
        cls, detail = W.classify_store_address(
            addr, 4, self.TARGET, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "EXCLUDED-STACK")
        self.assertNotIn("via_circular_modify", detail)

    def test_every_excluded_stack_result_carries_the_entry_seed_assumption(self):
        # Item 2: the entry-seed assumption is not proven closed (see
        # ENTRY_SEED_ASSUMPTION / out/sharcwriters/stack-invariant.md), so
        # every EXCLUDED-STACK classification -- plain or circ_ -- must say
        # so explicitly rather than assert an unqualified bound.
        plain = {"affine": {"constant": 0, "terms": [["I7e", 1]]}}
        circ = {"affine": {"constant": 0,
                            "terms": [[trace_mod.CIRC_SYMBOL_PREFIX + "7_1", 1]]}}
        for addr in (plain, circ):
            cls, detail = W.classify_store_address(
                addr, 4, self.TARGET, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
            self.assertEqual(cls, "EXCLUDED-STACK")
            self.assertEqual(detail["assumption"], W.ENTRY_SEED_ASSUMPTION)

    def test_mixed_stack_and_circ_terms_sum_their_own_bounds(self):
        # I6 = I7 after I7's own circular MODIFY (the CJUMP-adjacent shape):
        # one plain stack term and one circ_ term in the same expression.
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1c1676"
        addr = {"affine": {"constant": 0, "terms": [["I6e", 1], [name, 1]]}}
        cls, detail = W.classify_store_address(
            addr, 4, self.TARGET, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "EXCLUDED-STACK")
        expected_lo = W.DEFAULT_STACK_LO + (W.DEFAULT_STACK_LO - W.CIRC_WRAP_SLACK)
        expected_hi = (W.DEFAULT_STACK_HI - 1) + (W.DEFAULT_STACK_HI + W.CIRC_WRAP_SLACK - 1)
        self.assertEqual(detail["range"], [expected_lo, expected_hi])

    def test_circ_symbol_out_of_the_plain_range_but_in_the_widened_range_is_excluded(self):
        # A target just past DEFAULT_STACK_HI, inside the widened range,
        # must NOT be excluded for a plain stack symbol -- it would be
        # UNRESOLVED -- but must be excluded for a circ_ symbol only if the
        # target is truly outside the widened range. This checks the
        # reverse: a target safely outside the widened range is excluded.
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1c1676"
        addr = {"affine": {"constant": 0, "terms": [[name, 1]]}}
        far_target = W.DEFAULT_STACK_HI + W.CIRC_WRAP_SLACK + 0x10000
        cls, _detail = W.classify_store_address(
            addr, 4, far_target, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "EXCLUDED-STACK")

    def test_circ_symbol_target_inside_widened_slack_only_is_unresolved_not_excluded(self):
        # A target that falls in the widened slack margin (beyond plain S,
        # but still inside S +- CIRC_WRAP_SLACK) must not be silently
        # excluded -- it must come back UNRESOLVED, the same "overlap"
        # handling as a plain stack term.
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1c1676"
        addr = {"affine": {"constant": 0, "terms": [[name, 1]]}}
        target_in_slack = W.DEFAULT_STACK_HI + W.CIRC_WRAP_SLACK - 4
        cls, detail = W.classify_store_address(
            addr, 4, target_in_slack, W.DEFAULT_STACK_LO, W.DEFAULT_STACK_HI)
        self.assertEqual(cls, "UNRESOLVED")
        self.assertIn("overlaps", detail["reason"])


class CombinedAffineRangeTest(unittest.TestCase):
    def test_plain_stack_terms_only_matches_affine_range(self):
        terms = [("I7e", 1)]
        self.assertEqual(
            W.combined_affine_range(0, terms, 0x100, 0x200, 0x0, 0x300),
            W.affine_range(0, terms, 0x100, 0x200),
        )

    def test_circ_term_uses_circ_bounds(self):
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1"
        lo, hi = W.combined_affine_range(0, [(name, 1)], 0x100, 0x200, 0x50, 0x250)
        self.assertEqual((lo, hi), (0x50, 0x24F))

    def test_mixed_terms_sum_independently(self):
        name = trace_mod.CIRC_SYMBOL_PREFIX + "7_1"
        lo, hi = W.combined_affine_range(
            0, [("I7e", 1), (name, 1)], 0x100, 0x200, 0x50, 0x250)
        self.assertEqual((lo, hi), (0x100 + 0x50, 0x1FF + 0x24F))


if __name__ == "__main__":
    unittest.main()
