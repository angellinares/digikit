"""Symbolic tracer on the real DT2 1.17 blob."""

import hashlib
import os
import struct
import unittest
from importlib import import_module

from support.paths import REPO_ROOT

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T


class RealBlobStage6NormalWordAddressingTest(unittest.TestCase):
    """DT2 1.16's wavetable stage 6 (blk93@0x1cbf07) stores a compiled
    local with Type4a and re-reads it with Type15a. Skips cleanly when the
    firmware isn't present, per CLAUDE.md ("Firmware ... is Elektron's
    copyright: never commit it") and this repo's skip-not-fail convention
    for optional fixtures (see tests/sharc/analysis/test_sharcfn.py's
    RealBlobType19AndType6aRenderingTest and tests/sharc/analysis/test_sharc_interface_probe.py).
    """

    BLOB = os.path.join(
        str(REPO_ROOT),
        "out",
        "sections",
        "dt2-1.17",
        "section_7_BLOB.bin",
    )
    # CLAUDE.md / the handover's own record of this exact loader-final blob.
    BLOB_SHA256 = "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2"

    # Synthetic addresses for the caller-side frame/struct/table, matching
    # the wavetable6 scratch fixture's layout.py: word offsets from I4 are
    # scaled by 4 for --assume-32bit-normal-words, exactly like Type4a's own
    # modifier (tools/sharc_dev/sharc_trace.py's "4a" handling).
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
        os.path.exists(BLOB),
        "out/sections/dt2-1.17/section_7_BLOB.bin is not available",
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


class TypeCacheCompareRealBlobTest(unittest.TestCase):
    """Concrete run of FUN_001c2b24's type-cache compare/store (sw
    0x1c33bc..0x1c33e9) via T._execute() directly. This form's Type3b
    short-word addressing has no pypcode/SLEIGH semantics (see
    docs/findings/05-sharc-isa-and-decoding.md), so this exercises the
    behaviour through sharc_trace instead. A synthetic frame with a
    matching cached/live type takes the EQ branch and never writes the
    cache; a mismatch executes both trailing stores in sequence, so the
    final DM(I5+0xc4) value is whichever register the SECOND store uses
    (M14), not the freshly-read per-track field the first store wrote.

    Moved out of the now-removed 87df16d^:tools/sharcemu.py's test suite (this test
    never exercised sharcemu itself, only tools/sharc_dev/sharc_trace.py).
    """

    BLOB = os.path.join(
        str(REPO_ROOT),
        "out",
        "sections",
        "dt2-1.17",
        "section_7_BLOB.bin",
    )

    def run_type_cache_compare(self, cached, live, m14):
        i10_frame = 0x290000
        i6_stack = 0x292000
        track_base = i6_stack + 62 * 4
        i5_out = 0x293000
        cache_addr = 0x255970
        overlay = {}

        def poke16(addr, value):
            overlay[addr] = value & 0xFF
            overlay[addr + 1] = (value >> 8) & 0xFF

        def poke32(addr, value):
            for i in range(4):
                overlay[addr + i] = (value >> (8 * i)) & 0xFF

        poke32(track_base, 0x291000)
        poke16(cache_addr, cached)
        poke16(i10_frame, live)
        poke16(0x291000 + 0x54, 0xBEEF)

        uregs = {
            T.UREG_CODES["R5"]: T.Const(0),
            T.UREG_CODES["I6"]: T.Const(i6_stack),
            T.UREG_CODES["I10"]: T.Const(i10_frame),
            T.UREG_CODES["I5"]: T.Const(i5_out),
            T.UREG_CODES["M0"]: T.Const(0),
            T.UREG_CODES["M4"]: T.Const(0),
            T.UREG_CODES["M5"]: T.Const(0),
            T.UREG_CODES["M14"]: T.Const(m14),
            # SISD assumption (MODE1 reset default) so cond=0 (EQ) resolves
            # from AZ alone -- see the task write-up.
            T.UREG_CODES["MODE1"]: T.Const(0),
        }
        with open(self.BLOB, "rb") as fh:
            mem = L.LoadedMemory.from_stream(fh.read())
        state = T.State(
            0x1C33BC, dict(uregs), concrete=mem, overlay=overlay, assume_nw32=True
        )
        active = {T._dedupe_key(state): state}
        done = []
        steps = 0
        while active and steps < 200:
            s = active.pop(next(iter(active)))
            if s.pc_sw == 0x1C33E9:
                done.append(s)
                continue
            insn_record = T.decode_at(mem, None, s.pc_sw)
            for child in T._execute(s, insn_record):
                if child.stopped:
                    done.append(child)
                else:
                    active[T._dedupe_key(child)] = child
            steps += 1
        self.assertEqual(len(done), 1)
        return T._dm_read(done[0], i5_out + 49 * 4, 4)

    @unittest.skipUnless(
        os.path.exists(BLOB),
        "out/sections/dt2-1.17/section_7_BLOB.bin is not available",
    )
    def test_matching_cached_and_live_type_does_not_write(self):
        self.assertIsNone(self.run_type_cache_compare(cached=6, live=6, m14=0xAAAA))

    @unittest.skipUnless(
        os.path.exists(BLOB),
        "out/sections/dt2-1.17/section_7_BLOB.bin is not available",
    )
    def test_mismatched_type_writes_the_second_store_register(self):
        result = self.run_type_cache_compare(cached=6, live=7, m14=0xAAAA)
        self.assertIsNotNone(result)
        self.assertEqual(result.value, 0xAAAA)
        result2 = self.run_type_cache_compare(cached=6, live=7, m14=0x5555)
        self.assertIsNotNone(result2)
        self.assertEqual(result2.value, 0x5555)


if __name__ == "__main__":
    unittest.main()
