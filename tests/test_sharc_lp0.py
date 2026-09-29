"""tools/sharc_lp0.py: the FlexBus wire decode, and a feed through the DSP's
own link-port-0 receive callback on DT2 1.16."""

import json
import os
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import sharc_lp0 as lp  # noqa: E402

DT2_116_BLOB = ROOT / "out/sections/dt2-1.16/section_7_BLOB.bin"


def native_stereo(frames):
    """A native stereo sample (docs/findings/14): L = i, R = -i - 1."""
    pcm = b"".join(struct.pack(">hh", i, -i - 1) for i in range(frames))
    header = bytearray(0x40)
    header[1] = 1
    struct.pack_into(">II", header, 4, len(pcm), 48000)
    header[0x14] = 0x7F
    return bytes(header) + pcm + bytes(16)


def log_for(transfers):
    return [
        w
        for tag, block, swap in transfers
        for w in lp.encode_transfer(tag, block, swap)
    ]


class WireTest(unittest.TestCase):
    def words(self, writes):
        words, tail = lp.pack_words(lp.latch_bytes((a, v, False) for a, v in writes))
        self.assertEqual(tail, b"")
        return words

    def test_word_is_sent_low_byte_first_and_latched_on_falling_edge(self):
        writes = lp.encode_word_writes(0x11223344)
        self.assertEqual(
            writes[:2], [(lp.FLEXBUS_DATA, 0x4480), (lp.FLEXBUS_DATA, 0x4400)]
        )
        self.assertEqual(self.words(writes), [0x11223344])

    def test_swapped_word_lands_big_endian_int16_as_little_endian(self):
        (word,) = self.words(lp.encode_word_writes(0xAABBCCDD, swap16=True))
        self.assertEqual(
            struct.unpack("<2H", struct.pack("<I", word)), (0xAABB, 0xCCDD)
        )

    def test_other_addresses_and_latched_bytes(self):
        records = [
            (0x8C00000A, 0x80, False),
            (lp.FLEXBUS_DATA, 0x0180, False),
            (lp.FLEXBUS_DATA, 0x0100, False),
            (lp.FLEXBUS_DATA, 0x02, True),
            (lp.FLEXBUS_DATA, 0x0300, False),  # no rising clock before it
        ]
        self.assertEqual(lp.latch_bytes(records), b"\x01\x02")

    def test_log_round_trip_jsonl_and_bin(self):
        writes = lp.encode_word_writes(0xDEADBEEF)
        with tempfile.TemporaryDirectory() as tmp:
            for name in ("log.jsonl", "log.bin"):
                path = os.path.join(tmp, name)
                self.assertEqual(lp.write_log(path, writes), len(writes))
                self.assertEqual(
                    [(a, v) for a, v, _ in lp.read_log(path)], writes, name
                )
            path = os.path.join(tmp, "bytes.jsonl")
            with open(path, "w") as fh:
                for b in (0xEF, 0xBE, 0xAD, 0xDE):
                    fh.write(json.dumps({"addr": "0x8c000002", "byte": b}) + "\n")
            words, _ = lp.pack_words(lp.latch_bytes(lp.read_log(path)))
            self.assertEqual(words, [0xDEADBEEF])

    def test_native_stereo_transfers(self):
        native = native_stereo(3000)
        transfers = lp.native_sample_transfers(native, slot=3)
        alloc = (len(native) + 0x200F) & ~0x1FFF
        self.assertEqual(len(transfers), 2 + alloc // lp.PAGE_BYTES)
        reset, *pages, final = transfers
        self.assertEqual(reset[0], lp.HEADER_TAG)
        self.assertEqual(
            struct.unpack_from(">5I", reset[1]), (3, 0xFFFFFC00, 0xFFFFFC00, 0, 0)
        )
        self.assertEqual([p[0] for p in pages], list(range(alloc // lp.PAGE_BYTES)))
        self.assertEqual(
            struct.unpack_from(">5I", final[1]),
            (3, 0x20, alloc // 2 + 0x20, 48000, 3000 * 2),
        )
        buf = b"".join(p[1] for p in pages)
        left = struct.unpack_from(">4h", buf, 0x20)
        right = struct.unpack_from(">4h", buf, alloc // 2 + 0x20)
        self.assertEqual((left, right), ((0, 1, 2, 3), (-1, -2, -3, -4)))


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class FeedTest(unittest.TestCase):
    """The DSP's own callback fills the slot table and sample memory."""

    def test_feed_fills_slot_table_and_sample_memory(self):
        import sharc_harness as h

        frames = 3000
        native = native_stereo(frames)
        writes = log_for(lp.native_sample_transfers(native, slot=3))
        memory = h.load_image_memory("dt2-1.16")
        init = h.run_init(memory, "dt2-1.16")
        self.assertTrue(init.ran, init.error)
        runner = h.new_runner(memory, "dt2-1.16", init=init)
        runner, report = lp.feed(runner, "dt2-1.16", ((a, v, False) for a, v in writes))
        self.assertEqual(report["leftover_words"], 0)
        self.assertEqual(report["ignored_tags"], [])
        alloc = (len(native) + 0x200F) & ~0x1FFF
        self.assertEqual(
            report["slots"][3],
            {
                "start_l": 0x20,
                "start_r": alloc // 2 + 0x20,
                "len": frames * 2,
                "rate": 48000,
                "stereo": 1,
            },
        )
        base = lp.profile("dt2-1.16").sample_base
        self.assertEqual(
            report["slot_reader"][3],
            {
                "addr_l": base + 0x20,
                "addr_r": base + alloc // 2 + 0x20,
                "frames": frames,
                "rate": 48000,
                "stereo": 1,
            },
        )
        left = lp.read_bytes(runner.state, base + 0x20, 2 * frames)
        right = lp.read_bytes(runner.state, base + alloc // 2 + 0x20, 2 * frames)
        self.assertEqual(
            list(struct.unpack("<%dh" % frames, left)), list(range(frames))
        )
        self.assertEqual(
            list(struct.unpack("<%dh" % frames, right)), [-i - 1 for i in range(frames)]
        )


if __name__ == "__main__":
    unittest.main()
