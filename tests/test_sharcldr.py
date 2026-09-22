"""tools/sharcldr.py main_program() on a hand-built boot stream."""

import os
import struct
import sys
import unittest
from importlib import import_module

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"),
)

sharcldr = import_module("sharcldr")


def block(code, addr, count, arg=0, payload=b""):
    """Build a core-0 header with a valid HDRCHK, then its payload."""
    hdr = bytearray(struct.pack("<IIII", code | 0xAD000000, addr, count, arg))
    hdr[2] = 0
    x = 0
    for b in hdr:
        x ^= b
    hdr[2] = x
    return bytes(hdr) + payload


class LoadedMemoryTest(unittest.TestCase):
    def memory(self, data):
        return sharcldr.LoadedMemory.from_stream(data)

    def test_last_write_wins_for_partial_overlap(self):
        data = block(0, 10, 4, payload=b"abcd") + block(0, 12, 3, payload=b"XYZ")
        memory = self.memory(data)
        self.assertEqual(memory.read(10, 5), b"abXYZ")
        self.assertEqual(memory.source_block(11), 0)
        self.assertEqual(memory.source_block(12), 1)

    def test_read_across_adjacent_blocks_and_gaps(self):
        memory = self.memory(
            block(0, 20, 2, payload=b"ab") + block(0, 22, 2, payload=b"cd")
        )
        self.assertEqual(memory.read(20, 4), b"abcd")
        self.assertIsNone(memory.read(19, 1))
        self.assertIsNone(memory.read(23, 2))

    def test_fill_is_lazy_little_endian_and_marker_covers_nothing(self):
        fill = 1 << sharcldr.FILL_BIT
        data = block(fill, 30, 0x10000000, arg=0x11223344) + block(
            0, 0x20000000, 0, payload=b""
        )
        memory = self.memory(data)
        self.assertEqual(memory.read(30, 10), bytes.fromhex("44332211" * 2 + "4433"))
        self.assertEqual(memory.read(30 + 0x0FFFFFFF, 1), b"\x11")
        self.assertIsNone(memory.source_block(0x20000000))
        self.assertEqual(memory.ranges(), ((30, 30 + 0x10000000),))

    def test_fill_coverage_is_distinct_from_having_file_bytes(self):
        fill = 1 << sharcldr.FILL_BIT
        data = block(fill, 0x1000, 0x400, arg=0) + block(0, 0x2000, 4, payload=b"abcd")
        blocks = sharcldr.parse_blocks(data)
        # A FILL block contributes no file offset, but the loader does write
        # the range, so "no offset" must not be read as "the loader leaves
        # this address alone".
        self.assertIsNone(sharcldr.offset_for_address(blocks, 0x1100, space="byte"))
        covering = sharcldr.fill_block_for_address(blocks, 0x1100, space="byte")
        self.assertIsNotNone(covering)
        self.assertEqual(covering["target_address"], 0x1000)
        self.assertEqual(covering["argument"], 0)
        # A payload address and an address past the fill are both unfilled.
        self.assertIsNone(sharcldr.fill_block_for_address(blocks, 0x2000, space="byte"))
        self.assertIsNone(sharcldr.fill_block_for_address(blocks, 0x1400, space="byte"))

    def test_read_sw_ranges_and_immutable_records(self):
        base_sw = 0x123
        base = sharcldr.sw_to_byte(base_sw)
        memory = self.memory(
            block(0, base, 2, payload=b"hi") + block(0, base + 4, 2, payload=b"yo")
        )
        self.assertEqual(memory.read_sw(base_sw, 2), b"hi")
        self.assertEqual(memory.ranges(), ((base, base + 2), (base + 4, base + 6)))
        with self.assertRaises(TypeError):
            memory.blocks[0]["byte_count"] = 1

    def test_read_sw_falls_back_to_l2_execution_window(self):
        payload = b"service"
        memory = self.memory(
            block(0, sharcldr.L2_BYTE_BASE + 6, len(payload), payload=payload)
        )
        self.assertEqual(memory.read_sw(sharcldr.L2_SW_BASE + 3, len(payload)), payload)
        self.assertIsNone(memory.read_sw(sharcldr.L2_SW_BASE + 2, len(payload)))

        # A large SW value must not fold the fallback into an unrelated L1
        # loader region even if bytes happen to be present there.
        l1 = self.memory(block(0, 0x28240000, 4, payload=b"l1!!"))
        folded = sharcldr.L2_SW_BASE + (0x28240000 - sharcldr.L2_BYTE_BASE) // 2
        self.assertIsNone(l1.read_sw(folded, 4))

    def test_invalid_metadata_and_arguments(self):
        with self.assertRaisesRegex(ValueError, "stream data must be bytes-like"):
            sharcldr.LoadedMemory.from_stream(object())
        with self.assertRaisesRegex(ValueError, "payload metadata exceeds"):
            sharcldr.LoadedMemory(
                b"a",
                (
                    {
                        "target_address": 0,
                        "byte_count": 2,
                        "fill": False,
                        "payload_offset": 0,
                        "payload_len": 2,
                    },
                ),
            )
        with self.assertRaisesRegex(ValueError, "payload_len"):
            sharcldr.LoadedMemory(
                b"ab",
                (
                    {
                        "target_address": 0,
                        "byte_count": 2,
                        "fill": False,
                        "payload_offset": 0,
                        "payload_len": 1,
                    },
                ),
            )
        memory = self.memory(block(0, 0, 1, payload=b"a"))
        for args in ((-1, 1), (0, -1)):
            with self.assertRaises(ValueError):
                memory.read(*args)
        with self.assertRaises(ValueError):
            memory.read_sw(-1)

    def test_header_signature_checksum_and_flags_are_distinct(self):
        first = 1 << sharcldr.BFLAGS["FIRST"]
        encoded = block(first, 0, 0)
        parsed = sharcldr.parse_blocks(encoded)
        self.assertEqual(parsed[0]["core"], 0)
        self.assertEqual(parsed[0]["flags"], ["FIRST"])

        invalid_signature = bytearray(encoded)
        invalid_signature[2] ^= 0xAD ^ 0xAA
        invalid_signature[3] = 0xAA
        self.assertEqual(sharcldr.parse_blocks(bytes(invalid_signature)), [])


class MainProgramTest(unittest.TestCase):
    def test_contiguous_run_with_fill(self):
        first = 1 << sharcldr.BFLAGS["FIRST"]
        fill = 1 << sharcldr.FILL_BIT
        base = sharcldr.sw_to_byte(0x100)
        data = (
            block(first, 0x100, 0)
            + block(0, base, 4, payload=b"\x01\x02\x03\x04")
            + block(fill, base + 4, 6, arg=0x11223344)
            + block(0, base + 10, 2, payload=b"\xaa\xbb")
            + block(0, base + 100, 2, payload=b"\xcc\xdd")
        )
        blocks = sharcldr.parse_blocks(data)
        self.assertEqual(len(blocks), 5)
        addr, code, used = sharcldr.main_program(data, blocks)
        self.assertEqual(addr, base)
        self.assertEqual(code, bytes.fromhex("01020304443322114433aabb"))
        self.assertEqual(used, [1, 2, 3])

    def test_no_entry(self):
        data = block(0, 0x28000000, 2, payload=b"\x00\x00")
        self.assertEqual(
            sharcldr.main_program(data, sharcldr.parse_blocks(data)), (None, b"", [])
        )


if __name__ == "__main__":
    unittest.main()
