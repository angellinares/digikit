"""Synthetic card and safety proofs for the FX route data helpers."""

from __future__ import annotations

import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from tools.device import dksettings
from tools.patchkit import pk_device
from tools.patchkit import pk_digikit_fxroute as track_routes
from tools.patchkit import pk_digikit_fxroutes as routes
from tools.patchkit import pk_digikit_project as project


class FxRouteHelpersTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)

    def card(self, device):
        store = pk_device.store_of(device)
        card = self.folder / f"{device}.img"
        with card.open("wb") as file:
            file.truncate(store.end * 512)
        return card, store

    def overlay(self, *sectors):
        path = self.folder / "overlay.bin"
        path.write_bytes(
            b"".join(struct.pack("<Q", s) + bytes(64 + 512) for s in sectors)
        )
        return path

    def test_render_shots_writes_named_diagnostics_and_optional_copies(self):
        copy_dir = self.folder / "copies"
        with patch.dict("os.environ", {"DIGIKIT_SHOTS": str(copy_dir)}):
            for name in ("second", "first"):
                (self.folder / f"{name}.oled").write_bytes(bytes(1024))
            result = routes.render_shots(self.folder)
        self.assertEqual(result["count"], 2)
        self.assertEqual(
            [Path(name).name for name in result["files"]], ["first.png", "second.png"]
        )
        for name in result["files"]:
            generated = Path(name).read_bytes()
            self.assertTrue(generated)
            self.assertEqual((copy_dir / Path(name).name).read_bytes(), generated)

    def test_render_shots_rejects_malformed_batch_before_overwriting(self):
        existing = self.folder / "a.png"
        existing.write_bytes(b"previous diagnostic")
        (self.folder / "a.oled").write_bytes(bytes(1024))
        (self.folder / "b.oled").write_bytes(bytes(1023))
        with self.assertRaisesRegex(ValueError, "expected 1024"):
            routes.render_shots(self.folder)
        self.assertEqual(existing.read_bytes(), b"previous diagnostic")
        self.assertFalse((self.folder / "b.png").exists())

    def masked_overlay(self, name, sector, changes):
        mask = bytearray(64)
        data = bytearray(512)
        for offset, value in changes.items():
            mask[offset // 8] |= 1 << (offset % 8)
            data[offset] = value
        path = self.folder / f"{name}.bin"
        path.write_bytes(struct.pack("<Q", sector) + mask + data)
        return path

    def test_lock_check_applies_masks_over_base_card_bytes(self):
        card = self.folder / "lock.img"
        sector = project.SLOT0 + 1
        with card.open("wb") as file:
            file.truncate((sector + 1) * 512)
            file.seek(sector * 512)
            file.write(bytes([7]) * 512)
        base = self.masked_overlay("base", sector, {0: 7})
        none = self.masked_overlay("none", sector, {0: 8})
        # Different masks with the same effective image must compare equal.
        send = self.masked_overlay("send", sector, {0: 8, 1: 7})
        amount = self.masked_overlay("amount", sector, {0: 8, 2: 9})
        result = track_routes.lock_check("dt2", card, base, none, amount, send)
        self.assertEqual(result["trig_changed_bytes"], 1)
        self.assertEqual(result["send_changed_bytes"], 0)
        self.assertEqual(result["amount_diff"]["differ_bytes"], {"pattern": 1})
        self.assertFalse(result["amount_diff"]["equal"])
        self.assertEqual(list(self.folder.glob("fxroute-lock-*")), [])
        with card.open("rb") as file:
            file.seek(sector * 512)
            self.assertEqual(file.read(512), bytes([7]) * 512)

    def test_lock_check_compares_union_and_ignores_only_stamp(self):
        card = self.folder / "union.img"
        kit_sector = project.SLOT0 + (0x200 + 128 * 0x15C00) // 512 + 1
        with card.open("wb") as file:
            file.truncate((kit_sector + 1) * 512)
            file.seek(kit_sector * 512)
            file.write(bytes([3]) * 512)
        empty = self.folder / "empty.bin"
        empty.write_bytes(b"")
        send = self.masked_overlay("send", project.SLOT0, {0x18: 9})
        amount = self.masked_overlay("amount", kit_sector, {20: 4})
        result = track_routes.lock_check("dt2", card, empty, empty, amount, send)
        self.assertEqual(result["trig_changed_bytes"], 0)
        self.assertEqual(result["send_changed_bytes"], 0)
        self.assertEqual(result["amount_diff"]["differ_bytes"], {"kit": 1})
        self.assertEqual(result["amount_diff"]["only_first"], 0)
        self.assertEqual(result["amount_diff"]["only_second"], 0)

    def test_lock_check_rejects_torn_records_before_touching_the_card(self):
        overlay = self.overlay()
        overlay.write_bytes(b"torn")
        card = self.folder / "unused.img"
        card.write_bytes(b"original")
        with self.assertRaises(ValueError):
            track_routes.lock_check("dt2", card, overlay, overlay, overlay, overlay)
        self.assertEqual(card.read_bytes(), b"original")
        self.assertEqual(list(self.folder.glob("fxroute-lock-*")), [])

    def test_seed_round_trip_preserves_card_and_other_settings_copy(self):
        for device in ("dt2", "dn2"):
            with self.subTest(device=device):
                card, store = self.card(device)
                size = card.stat().st_size
                with card.open("r+b") as file:
                    file.write(b"caller-owned")
                entries = [
                    {"key": "fx/route/test/send", "value_hex": "00"},
                    {"key": "fx/send/test/send", "value_hex": bytes(range(16)).hex()},
                ]
                result = routes.seed_card(card, device, entries)
                self.assertEqual(result["bytes"] % 512, 0)
                self.assertEqual(card.stat().st_size, size)
                with card.open("rb") as file:
                    self.assertEqual(file.read(12), b"caller-owned")
                    file.seek(store.set_a * 512)
                    raw = file.read(result["bytes"])
                    record = dksettings.decode(raw)
                    self.assertIsNotNone(record)
                    self.assertEqual(record.seq, 1)
                    self.assertEqual(record.get(entries[0]["key"]), b"\x00")
                    self.assertEqual(record.get(entries[1]["key"]), bytes(range(16)))
                    self.assertEqual(raw[record.used :], bytes(len(raw) - record.used))
                    file.seek(store.set_b * 512)
                    self.assertEqual(file.read(512), bytes(512))
                with self.assertRaisesRegex(ValueError, "empty"):
                    routes.seed_card(card, device, entries)

    def test_seed_rejects_malformed_entries_before_writing(self):
        card, store = self.card("dn2")
        bad_entries = [
            {},
            [{}],
            [{"key": "a", "value_hex": "zz"}],
            [{"key": "space key", "value_hex": "00"}],
            [{"key": "a", "value_hex": "00"}] * 2,
            [{"key": "a", "value_hex": "00" * (dksettings.VALUE_MAX + 1)}],
            [{"key": "a", "value_hex": "", "extra": 1}],
            [{"key": "é", "value_hex": ""}],
            [
                {
                    "key": "a" * dksettings.KEY_MAX,
                    "value_hex": "00" * dksettings.VALUE_MAX,
                }
                for _ in range(dksettings.AREA // dksettings.VALUE_MAX + 1)
            ],
        ]
        for entries in bad_entries:
            with (
                self.subTest(entries=str(entries)[:100]),
                self.assertRaises(ValueError),
            ):
                routes.seed_card(card, "dn2", entries)
        with card.open("rb") as file:
            file.seek(store.set_a * 512)
            self.assertEqual(
                file.read(store.set_copy_sectors * 512),
                bytes(store.set_copy_sectors * 512),
            )

    def test_seed_rejects_short_card_and_occupied_copy_b(self):
        card = self.folder / "short.img"
        card.write_bytes(b"caller-owned")
        with self.assertRaisesRegex(ValueError, "too small"):
            routes.seed_card(card, "dt2", [])
        self.assertEqual(card.read_bytes(), b"caller-owned")
        card, store = self.card("dt2")
        with card.open("r+b") as file:
            file.seek(store.set_b * 512 + 511)
            file.write(b"x")
        with self.assertRaisesRegex(ValueError, "empty"):
            routes.seed_card(card, "dt2", [])


if __name__ == "__main__":
    unittest.main()
