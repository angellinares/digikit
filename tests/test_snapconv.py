"""Synthetic coverage for the trusted-pickle to MSTATE conversion seam."""

import json
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / "tools"
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from tools import snapconv  # noqa: E402


def blob():
    page = bytearray(snapconv.PAGE_SIZE)
    page[9] = 0xA5
    return {
        "regs": {name: index for index, name in enumerate(snapconv.REG_NAMES)},
        "pages": {snapconv.PAGE_SIZE: zlib.compress(page)},
        "all_mapped": [0, snapconv.PAGE_SIZE],
        "mmio": {0x10: 0x20},
        "ctlregs": {0x30: 0x40},
        "ff1_count": 2,
        "movec_count": 3,
        "components": {"queue": {"type": "deque", "values": [1]}},
        "manifest": {"build": "synthetic"},
    }


def test_synthetic_roundtrip_has_nonzero_and_mapped_zero_page():
    assert len(snapconv.MAGIC) == 8
    assert snapconv.MAGIC == b"MSTATE\x00\x01"
    encoded = snapconv.convert_blob(blob())
    assert encoded[:8] == snapconv.MAGIC
    header_len = struct.unpack("<I", encoded[8:12])[0]
    header = json.loads(encoded[12 : 12 + header_len])
    assert header["format_version"] == 1
    assert header["clock"] == 0
    assert header["clock_basis"] == "checkpoint_relative_zero"
    assert header["mapped_bases"] == [0, snapconv.PAGE_SIZE]
    assert header["page_count"] == 1
    assert header["mmio_forced"] == {"16": 32}
    base, raw_len, compressed_len = struct.unpack(
        "<III", encoded[12 + header_len : 24 + header_len]
    )
    assert (base, raw_len) == (snapconv.PAGE_SIZE, snapconv.PAGE_SIZE)
    assert (
        zlib.decompress(encoded[24 + header_len : 24 + header_len + compressed_len])[9]
        == 0xA5
    )


def test_rejects_opaque_integer_outside_portable_range():
    for name in ("components", "manifest", "extra"):
        source = blob()
        source[name] = {"nested": [1 << 64]}
        try:
            snapconv.convert_blob(source)
        except ValueError as exc:
            assert "outside portable JSON bounds" in str(exc)
        else:
            raise AssertionError("%s accepted an out-of-range integer" % name)


def test_direct_script_cli_imports_from_repository_root():
    result = subprocess.run(
        [sys.executable, str(TOOLS / "snapconv.py"), "--help"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    assert "trusted local" in result.stdout


def test_malformed_legacy_input_does_not_create_portable_file(tmp_path):
    legacy = tmp_path / "untrusted.snap"
    output = tmp_path / "state.mstate"
    legacy.write_bytes(b"not a pickle")
    result = subprocess.run(
        [sys.executable, str(TOOLS / "snapconv.py"), str(legacy), str(output)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert not output.exists()
