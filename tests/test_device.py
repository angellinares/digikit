# pyright: reportMissingImports=false
"""Device files: identity by hash, and the wire mapping.

`emu/config.py` settles a tie between firmware files by hardcoded FILENAME,
which is how one product silently runs under another's name. A device file
keys on the firmware's SHA-256 instead. These tests cover that, the
(channel, bit) <-> control-code mapping including each product's non-linear
channel 6, and that the real files in devices/ parse and round-trip.

No firmware and no emulator: the shipped device files are plain data, and
everything else is built in a temp directory.
"""
import os
import tempfile
import unittest

from emu import device


REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEVICES = os.path.join(REPO, 'devices')

SYNTHETIC = '''
[device]
name = "Test Device"
short = "td"

[[firmware]]
version = "9.9Z"
sha256 = "AABBCC"
filename = "Test_OS9.9Z.syx"

[panel]
linear_channels = 2
encoders = 3

[panel.exceptions]
20 = [6, 1]

[[panel.group]]
name = "keys"
kind = "button"
codes = [1, 2]
layout = "row"

[[panel.group]]
name = "odd"
kind = "button"
codes = [20]
layout = "single"

[[panel.group]]
name = "encoders"
kind = "encoder"
codes = [1, 2, 3]
layout = "row"
'''


def write_device(dirpath, text=SYNTHETIC, name='test.toml'):
    path = os.path.join(dirpath, name)
    with open(path, 'w') as fh:
        fh.write(text)
    return path


class ParseTest(unittest.TestCase):
    def test_parses_identity_and_panel(self):
        with tempfile.TemporaryDirectory() as d:
            dev = device.load(write_device(d))
        self.assertEqual(dev.name, 'Test Device')
        self.assertEqual(dev.short, 'td')
        self.assertEqual(dev.linear_channels, 2)
        self.assertEqual(dev.encoders, 3)
        self.assertEqual(len(dev.groups), 3)

    def test_sha256_is_lowercased(self):
        # Hashes get pasted in both cases; comparison must not care.
        with tempfile.TemporaryDirectory() as d:
            dev = device.load(write_device(d))
        self.assertEqual(dev.firmwares[0].sha256, 'aabbcc')
        self.assertIsNotNone(dev.firmware_for_sha256('AaBbCc'))

    def test_exception_codes_are_integers(self):
        # TOML bare keys are strings even when they look like integers, so
        # '20' must arrive as 20 or every lookup silently misses.
        with tempfile.TemporaryDirectory() as d:
            dev = device.load(write_device(d))
        self.assertEqual(dev.exceptions[20], (6, 1))

    def test_missing_section_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            path = write_device(d, '[device]\nname = "X"\n')
            with self.assertRaises(device.DeviceError):
                device.load(path)


class WireMappingTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dev = device.load(write_device(self.tmp.name))

    def tearDown(self):
        self.tmp.cleanup()

    def test_linear_region(self):
        self.assertEqual(self.dev.wire_for(1), (0, 0))
        self.assertEqual(self.dev.wire_for(9), (1, 0))
        self.assertEqual(self.dev.wire_for(16), (1, 7))

    def test_exception_wins_over_the_formula(self):
        self.assertEqual(self.dev.wire_for(20), (6, 1))
        self.assertEqual(self.dev.code_at(6, 1), 20)

    def test_code_beyond_this_product_has_no_wire_position(self):
        # Not an error: one product simply carries fewer controls.
        self.assertIsNone(self.dev.wire_for(40))

    def test_encoder_codes_are_a_separate_space(self):
        self.assertEqual(self.dev.encoder_channel(1), 0)
        self.assertEqual(self.dev.encoder_channel(3), 2)
        self.assertIsNone(self.dev.encoder_channel(4))


class IdentifyTest(unittest.TestCase):
    def test_unknown_hash_is_refused_not_guessed(self):
        with tempfile.TemporaryDirectory() as d:
            write_device(d)
            target = os.path.join(d, 'firmware.syx')
            with open(target, 'wb') as fh:
                fh.write(b'not a known firmware')
            with self.assertRaises(device.DeviceError):
                device.identify(target, d)

    def test_matches_by_content_not_filename(self):
        with tempfile.TemporaryDirectory() as d:
            payload = b'pretend firmware'
            import hashlib
            sha = hashlib.sha256(payload).hexdigest()
            write_device(d, SYNTHETIC.replace('"AABBCC"', '"%s"' % sha))
            # A name that matches no filename field in the device file.
            target = os.path.join(d, 'renamed-by-the-user.syx')
            with open(target, 'wb') as fh:
                fh.write(payload)
            dev, fw = device.identify(target, d)
            self.assertEqual(dev.name, 'Test Device')
            self.assertEqual(fw.version, '9.9Z')

    def test_missing_directory_is_refused(self):
        with self.assertRaises(device.DeviceError):
            device.load_all('/nonexistent-devices-dir')


class ShippedDeviceFilesTest(unittest.TestCase):
    """The real files in devices/ -- data only, no firmware needed."""

    def setUp(self):
        self.devices = device.load_all(DEVICES)

    def test_both_products_are_present(self):
        names = sorted(d.name for d in self.devices)
        self.assertEqual(names, ['Digitakt II', 'Digitone II'])

    def test_every_button_code_round_trips(self):
        for dev in self.devices:
            for code in dev.button_codes():
                pos = dev.wire_for(code)
                self.assertIsNotNone(pos, '%s code %d' % (dev.name, code))
                self.assertEqual(dev.code_at(*pos), code,
                                 '%s code %d' % (dev.name, code))

    def test_products_differ_where_the_hardware_does(self):
        by_name = {d.name: d for d in self.devices}
        # Digitakt's name table ends at 50; Digitone carries five more.
        self.assertEqual(max(by_name['Digitakt II'].button_codes()), 50)
        self.assertEqual(max(by_name['Digitone II'].button_codes()), 54)

    def test_no_duplicate_codes_within_a_kind(self):
        for dev in self.devices:
            codes = dev.button_codes()
            self.assertEqual(len(codes), len(set(codes)), dev.name)

    def test_each_lists_known_firmware_hashes(self):
        for dev in self.devices:
            self.assertGreaterEqual(len(dev.firmwares), 1, dev.name)
            hashes = [fw.sha256 for fw in dev.firmwares]
            for sha in hashes:
                self.assertRegex(sha, r'^[0-9a-f]{64}$', dev.name)
            self.assertEqual(len(hashes), len(set(hashes)), dev.name)


if __name__ == '__main__':
    unittest.main()
