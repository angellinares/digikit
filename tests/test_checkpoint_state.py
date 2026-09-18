"""Synthetic state protocol coverage; no firmware-derived inputs required."""

import collections
import os
import pickle
import tempfile
import unittest
from types import SimpleNamespace

from emu.dtim import BASES, DTMR, Dtims, Timers, restore_timers
from emu.edma import TxChannel, kick, needs_legacy_kick
from emu.longrun import register_esdhc_checkpoint_component
from emu.pit import Pits
from emu.snapshot import (
    DeferredComponentRestore,
    _restore_component,
    _validate_manifest,
    restore_into,
    save,
)


class _FakeUc:
    def __init__(self):
        self.regs = {}
        self.memory = {}

    def hook_add(self, *args, **kwargs):
        return 1

    def mem_read(self, addr, size):
        return bytes(self.memory.get(addr + offset, 0) for offset in range(size))

    def mem_write(self, addr, data):
        for offset, value in enumerate(data):
            self.memory[addr + offset] = value

    def reg_write(self, reg, value):
        self.regs[reg] = value

    def reg_read(self, reg):
        return self.regs.get(reg, 0)


class _FakeMachine:
    def __init__(self):
        self.uc, self.mmio, self.ctlregs = _FakeUc(), {}, {}
        self.mapped = set()
        self.ff1_count = self.movec_count = 0

    def ensure(self, base):
        pass


class _Component:
    def __init__(self, value=None):
        self.value = value

    def checkpoint_state(self):
        return {"type": "test-component", "version": 1, "value": self.value}

    def restore_checkpoint_state(self, state):
        if state.get("type") != "test-component" or state.get("version") != 1:
            raise RuntimeError("unsupported test component state")
        self.value = state["value"]


class CheckpointStateTest(unittest.TestCase):
    def test_pits_round_trip_preserves_cadence(self):
        original = Pits.__new__(Pits)
        original.channels, original.ips = (3, 2, 0), 4680000
        original.next, original.now, original.held = [None, 22.5, None, 9], 21, True
        original.fired = collections.Counter({3: 4})
        original.missed = collections.Counter({2: 5})
        restored = Pits.__new__(Pits)
        restored.channels, restored.ips = (3, 2, 0), 4680000
        restored.restore_checkpoint_state(original.checkpoint_state())
        self.assertEqual(restored.checkpoint_state(), original.checkpoint_state())

    def test_timers_restores_source_order_and_rejects_mismatch(self):
        pit = Pits.__new__(Pits)
        pit.channels, pit.ips, pit.next, pit.now, pit.held = (
            (3,),
            1,
            [None] * 4,
            7,
            False,
        )
        pit.fired, pit.missed = collections.Counter(), collections.Counter()
        dtim = Dtims.__new__(Dtims)
        dtim.channels, dtim.ips, dtim.next, dtim.now, dtim.held = (
            (1,),
            1,
            [None] * 4,
            7,
            False,
        )
        dtim.fired, dtim.missed, dtim.arm, dtim.stale = (
            collections.Counter(),
            collections.Counter(),
            {1},
            [1],
        )
        state = Timers(pit, dtim).checkpoint_state()
        other_pit = Pits.__new__(Pits)
        other_pit.channels, other_pit.ips = (3,), 1
        other_dtim = Dtims.__new__(Dtims)
        other_dtim.channels, other_dtim.ips = (1,), 1
        restored = Timers(other_pit, other_dtim)
        restored.restore_checkpoint_state(state)
        self.assertEqual(restored.now, 7)
        with self.assertRaisesRegex(RuntimeError, "source order"):
            Timers(other_dtim, other_pit).restore_checkpoint_state(state)

    def test_deque_and_edma_round_trip_and_no_legacy_kick_signal(self):
        queue = collections.deque([1, 2])
        state = {"type": "deque", "version": 1, "values": [3, 4]}
        _restore_component(queue, state, "uart_in")
        self.assertEqual(list(queue), [3, 4])
        tx = TxChannel.__new__(TxChannel)
        tx.chan, tx.vector, tx.pending, tx.bytes, tx.transfers = 35, 155, 2, 10, 3
        clone = TxChannel.__new__(TxChannel)
        clone.chan, clone.vector = 35, 155
        clone.restore_checkpoint_state(tx.checkpoint_state())
        self.assertEqual((clone.pending, clone.bytes, clone.transfers), (2, 10, 3))
        self.assertTrue(clone._checkpoint_restored)
        fresh = TxChannel.__new__(TxChannel)
        fresh._checkpoint_restored = False
        self.assertFalse(needs_legacy_kick(clone))
        self.assertTrue(needs_legacy_kick(fresh))

    def test_legacy_edma_kick_uses_resolved_tx_state(self):
        machine = _FakeMachine()
        old_state, resolved_state = 0x4094CD74, 0x40964D74
        machine.uc.mem_write(old_state, b"\x00\x00\x00\x00")
        machine.uc.mem_write(resolved_state, b"\x00\x00\x00\x01")

        class Channel:
            def __init__(self):
                self.runs = 0

            def run(self):
                self.runs += 1
                return 5

        channel = Channel()
        self.assertEqual(kick(machine, channel, tx_state=resolved_state), 5)
        self.assertEqual(channel.runs, 1)

    def test_restore_timers_preserves_armed_dtim_registers(self):
        machine = _FakeMachine()
        machine.uc.mem_write(BASES[3] + DTMR, b"\x00\x1d")
        saved = Timers(
            Dtims(machine, channels=(3,), clear_stale=False)
        ).checkpoint_state()
        deferred = DeferredComponentRestore(("timers",))
        deferred.defer("timers", saved)
        restored = restore_timers(machine, deferred)
        assert restored is not None
        self.assertIsInstance(restored.sources[0], Dtims)
        self.assertEqual(machine.uc.mem_read(BASES[3] + DTMR, 2), b"\x00\x1d")
        deferred.require_claimed()

    def test_save_restore_named_and_deferred_components(self):
        machine = _FakeMachine()
        handle, path = tempfile.mkstemp()
        os.close(handle)
        try:
            save(
                machine,
                path,
                components={
                    "uart_in": collections.deque([1, 2]),
                    "timers": _Component("saved cadence"),
                },
                manifest={"hooks": 1},
            )
            queue = collections.deque()
            deferred = DeferredComponentRestore(("timers",))
            self.assertEqual(
                restore_into(
                    _FakeMachine(),
                    path,
                    components={"uart_in": queue},
                    manifest={"hooks": 1},
                    deferred=deferred,
                ),
                0,
            )
            self.assertEqual(list(queue), [1, 2])
            with self.assertRaisesRegex(RuntimeError, "unclaimed deferred"):
                deferred.require_claimed()
            timers = _Component()
            self.assertTrue(deferred.claim("timers", timers))
            self.assertEqual(timers.value, "saved cadence")
            deferred.require_claimed()
            with self.assertRaisesRegex(RuntimeError, "not configured for deferral"):
                deferred.claim("other", _Component())
        finally:
            os.unlink(path)

    def test_esdhc_component_is_registered_serialized_and_restored(self):
        profile = SimpleNamespace(
            sd_status=0x50000000,
            sd_cmd_sem=0x50000010,
            sd_data_sem=0x50000020,
            sd_dma_sem=0x50000030,
        )
        machine, events = _FakeMachine(), {}
        components = {"uart_in": collections.deque()}
        model = register_esdhc_checkpoint_component(
            machine, events, profile, components
        )
        model.card.overlay[7 * 512] = 0xA5

        handle, path = tempfile.mkstemp()
        os.close(handle)
        try:
            save(machine, path, components=components)
            restored_machine, restored_events = _FakeMachine(), {}
            restored_components = {"uart_in": collections.deque()}
            restored = register_esdhc_checkpoint_component(
                restored_machine,
                restored_events,
                profile,
                restored_components,
            )
            restore_into(
                restored_machine,
                path,
                components=restored_components,
            )
            self.assertIs(restored_events["esdhc"], restored)
            self.assertIs(restored_components["esdhc"], restored)
            self.assertEqual(restored.card.data_for(18, 7, 1), b"\xA5")
        finally:
            os.unlink(path)

    def test_malformed_component_fails_before_guest_mutation(self):
        machine = _FakeMachine()
        machine.mmio[1] = 2
        blob = {
            "checkpoint_version": 2,
            "regs": {
                name: 0
                for name, _ in __import__("emu.snapshot", fromlist=["REGS"]).REGS
            },
            "pages": {},
            "all_mapped": [],
            "mmio": {3: 4},
            "ctlregs": {},
            "ff1_count": 0,
            "movec_count": 0,
            "extra": {},
            "components": {"uart_in": {"type": "deque", "version": 1, "values": [999]}},
        }
        handle, path = tempfile.mkstemp()
        os.close(handle)
        try:
            with open(path, "wb") as out:
                pickle.dump(blob, out)
            with self.assertRaisesRegex(RuntimeError, "invalid deque"):
                restore_into(machine, path, components={"uart_in": collections.deque()})
            self.assertEqual(machine.mmio, {1: 2})
        finally:
            os.unlink(path)

    def test_unsupported_top_level_checkpoint_version_is_rejected(self):
        blob = {
            "checkpoint_version": 99,
            "regs": {},
            "pages": {},
            "all_mapped": [],
            "mmio": {},
            "ctlregs": {},
            "ff1_count": 0,
            "movec_count": 0,
            "extra": {},
        }
        handle, path = tempfile.mkstemp()
        os.close(handle)
        try:
            with open(path, "wb") as out:
                pickle.dump(blob, out)
            with self.assertRaisesRegex(RuntimeError, "unsupported checkpoint version"):
                restore_into(_FakeMachine(), path)
        finally:
            os.unlink(path)

    def test_legacy_snapshot_without_components_still_restores(self):
        blob = {
            "regs": {
                name: 0
                for name, _ in __import__("emu.snapshot", fromlist=["REGS"]).REGS
            },
            "pages": {},
            "all_mapped": [],
            "mmio": {},
            "ctlregs": {},
            "ff1_count": 0,
            "movec_count": 0,
            "extra": {},
        }
        blob["regs"]["pc"] = 0x1234
        handle, path = tempfile.mkstemp()
        os.close(handle)
        try:
            with open(path, "wb") as out:
                pickle.dump(blob, out)
            self.assertEqual(restore_into(_FakeMachine(), path), 0x1234)
        finally:
            os.unlink(path)

    def test_configuration_manifest_rejection(self):
        _validate_manifest({"edma": True}, {"edma": True})
        with self.assertRaisesRegex(RuntimeError, "manifest mismatch"):
            _validate_manifest({"edma": True}, {"edma": False})


if __name__ == "__main__":
    unittest.main()
