"""Symbolic tracer: loader blobs, pokes and the command line."""

import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from importlib import import_module
from unittest.mock import patch

from tools import python_env

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")
from support.sharc_encode import encode  # noqa: E402
from support.sharc_trace_build import insn, loader_block, loader_memory  # noqa: E402

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T

from support.sharc_trace_build import TraceRunMixin  # noqa: E402


class TraceBlobCliTest(TraceRunMixin, unittest.TestCase):
    """Loader blobs and the command line."""

    def test_cli_symbolic_seed(self):
        with tempfile.NamedTemporaryFile("wb", delete=False) as f:
            path = f.name
        try:
            valid = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tools.sharc_dev.sharc_trace",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "R1=@receive_buffer",
                    "--json",
                ],
                env=python_env(),
                capture_output=True,
            )
            invalid = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tools.sharc_dev.sharc_trace",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "R1=@not-valid",
                ],
                env=python_env(),
                capture_output=True,
            )
            invalid_register = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tools.sharc_dev.sharc_trace",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--set",
                    "BOGUS=1",
                ],
                env=python_env(),
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
            command = [sys.executable, "-m", "tools.sharc_dev.sharc_trace"]
            missing_base = subprocess.run(
                command + [stream_path, "--start", hex(pc)],
                env=python_env(),
                capture_output=True,
            )
            ambiguous = subprocess.run(
                command + [stream_path, "--blob", "--base-sw", "0", "--start", hex(pc)],
                env=python_env(),
                capture_output=True,
            )
            no_ranges = subprocess.run(
                command + [empty_path, "--blob", "--start", hex(pc)],
                env=python_env(),
                capture_output=True,
            )
            valid = subprocess.run(
                command + [stream_path, "--blob", "--start", hex(pc), "--json"],
                env=python_env(),
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
                env=python_env(),
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
                env=python_env(),
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
                env=python_env(),
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
        with patch("tools.sharc_dev.sharc_trace.decode_at", return_value=branch):
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
                        "-m",
                        "tools.sharc_dev.sharc_trace",
                        path,
                        "--base-sw",
                        "0",
                        "--start",
                        "0",
                        option,
                        value,
                    ],
                    env=python_env(),
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(invalid.returncode, 2)
                self.assertIn(message, invalid.stderr)
            out = subprocess.check_output(
                [
                    sys.executable,
                    "-m",
                    "tools.sharc_dev.sharc_trace",
                    path,
                    "--base-sw",
                    "0",
                    "--start",
                    "0",
                    "--json",
                ],
                env=python_env(),
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
                "-m",
                "tools.sharc_dev.sharc_trace",
                program_path,
                "--blob",
                "--start",
                "0x10",
                "--max-steps",
                "0",
                "--json",
                *extra_args,
            ],
            env=python_env(),
            capture_output=True,
            text=True,
        )

    def test_cli_poke_dm_and_poke_dm_file_round_trip(self):
        with tempfile.NamedTemporaryFile("wb", delete=False) as stream:
            stream.write(loader_block(1, L.sw_to_byte(0x10), 2, payload=b"\x01\x00"))
            stream.write(loader_block(1 << L.BFLAGS["FINAL"], 0, 0))
            program_path = stream.name
        poke_file = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)  # noqa: SIM115
        try:
            json.dump({"0x300000": 291, "0x310ca300": [1, 2, 3]}, poke_file)
            poke_file.close()
            missing_concrete = self._run_cli(program_path, "--poke-dm", "0x300000=1")
            self.assertEqual(missing_concrete.returncode, 2)
            self.assertIn("requires --concrete-memory", missing_concrete.stderr)
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


if __name__ == "__main__":
    unittest.main()
