"""Synthetic address-map coverage for the SHARC Ghidra importer."""

import os
import struct
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
I = import_module("sharc_import")
L = import_module("sharcldr")


class GhidraAddressTest(unittest.TestCase):
    def test_l2_execution_window_maps_to_its_short_word_addresses(self):
        self.assertEqual(I.ghidra_addr(L.L2_BYTE_BASE), 2 * L.L2_SW_BASE)
        self.assertEqual(I.ghidra_addr(L.L2_BYTE_BASE + 0xFC4A), 2 * 0x00B87E25)

    def test_l2_translation_is_bounded_and_l1_is_unchanged(self):
        self.assertEqual(
            I.ghidra_addr(L.L2_BYTE_LIMIT - 1),
            2 * (L.L2_SW_BASE + 0xFFFF) + 1,
        )
        # Just past the L2 window, and below the L1 alias window (SPACE_BASE),
        # a loader byte address is neither alias: it is its own offset.
        self.assertEqual(I.ghidra_addr(L.L2_BYTE_LIMIT), L.L2_BYTE_LIMIT)
        l1 = L.sw_to_byte(0x1C1338)
        self.assertEqual(I.ghidra_addr(l1), 2 * 0x1C1338)

    def test_l1_alias_window_is_bounded_and_external_is_unchanged(self):
        # Inside the L1 system-alias window: byte = 2*sw + SPACE_BASE maps
        # down to Ghidra offset 2*sw.
        self.assertEqual(I.ghidra_addr(I.SPACE_BASE), 0)
        self.assertEqual(I.ghidra_addr(I.L1_ALIAS_LIMIT - 1), I.L1_ALIAS_LIMIT - 1 - I.SPACE_BASE)
        # At and past the L1 alias window's upper bound, an address is no
        # longer an alias -- it keeps its own value.
        self.assertEqual(I.ghidra_addr(I.L1_ALIAS_LIMIT), I.L1_ALIAS_LIMIT)
        # External memory (e.g. 0x80000000..0x82a001c4) is not an alias of
        # anything and must land at its own address, not SPACE_BASE-shifted
        # (the old bug put it at 0x58xxxxxx).
        external = 0x82A00008
        self.assertEqual(I.ghidra_addr(external), external)
        self.assertNotEqual(I.ghidra_addr(external), external - I.SPACE_BASE)

    def test_ranges_are_split_at_both_l2_mapping_boundaries(self):
        start = L.L2_BYTE_BASE - 2
        self.assertEqual(
            list(I.mapped_segments(start, 4)),
            [
                (0, I.ghidra_addr(start), 2),
                (2, I.ghidra_addr(L.L2_BYTE_BASE), 2),
            ],
        )
        start = L.L2_BYTE_LIMIT - 2
        self.assertEqual(
            list(I.mapped_segments(start, 4)),
            [
                (0, I.ghidra_addr(start), 2),
                (2, I.ghidra_addr(L.L2_BYTE_LIMIT), 2),
            ],
        )

    def test_payload_and_fill_replay_bytes_preserve_source_offset(self):
        data = b"xxpayloadyy"
        payload = {"fill": False, "payload_offset": 2}
        self.assertEqual(I.loaded_bytes(data, payload, 1, 4), b"aylo")

        fill = {"fill": True, "argument": 0x11223344}
        self.assertEqual(I.loaded_bytes(b"", fill, 0, 8), struct.pack("<II", 0x11223344, 0x11223344))
        self.assertEqual(I.loaded_bytes(b"", fill, 3, 5), bytes.fromhex("1144332211"))


if __name__ == "__main__":
    unittest.main()
