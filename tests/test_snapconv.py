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

from emu.snapshot import _validate_timer_source  # noqa: E402
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


def test_timer_component_normalizes_tuple_channels_and_counter_keys():
    source = blob()
    source["components"] = {
        "timers": {
            "type": "Timers",
            "version": 1,
            "sources": [
                {
                    "type": "Pits",
                    "version": 1,
                    "channels": (3, 2, 0),
                    "ips": 4_680_000,
                    "next": [None, None, 125.5, 101.0],
                    "now": 100,
                    "held": False,
                    "fired": {3: 4},
                    "missed": {2: 5},
                    "cleared": {0: 6},
                    "pending": [1, 1],
                },
                {
                    "type": "Dtims",
                    "version": 1,
                    "channels": (3,),
                    "ips": 4_680_000,
                    "next": [None, None, None, 130.0],
                    "now": 100,
                    "held": False,
                    "fired": {3: 7},
                    "missed": {3: 8},
                    "cleared": {3: 9},
                    "pending": [0, 0],
                    "arm": [],
                    "stale": [0, 0, 2],
                },
            ],
        }
    }
    for timer_source in source["components"]["timers"]["sources"]:
        _validate_timer_source(timer_source)
    encoded = snapconv.convert_blob(source)
    header_len = struct.unpack("<I", encoded[8:12])[0]
    timers = json.loads(encoded[12 : 12 + header_len])["components"]["timers"]
    assert timers["sources"][0]["channels"] == [3, 2, 0]
    assert timers["sources"][0]["fired"] == {"3": 4}
    assert timers["sources"][0]["pending"] == [1, 1]
    assert timers["sources"][1]["pending"] == [0, 0]
    assert timers["sources"][1]["stale"] == [0, 0, 2]


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
