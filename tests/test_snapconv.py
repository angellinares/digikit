"""Synthetic coverage for the trusted-pickle to MSTATE conversion seam."""

import json
import struct
import subprocess
import sys
import zlib
from pathlib import Path

import pytest

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


def test_longrun_manifest_and_card_overlay_have_narrow_json_normalization():
    source = blob()
    source["manifest"] = {"protocol": 1, "unblock_except": (0x1000, 0x2000)}
    source["components"] = {
        "esdhc": {
            "type": "Esdhc",
            "version": 1,
            "pattern": 0,
            "armed": None,
            "dma_bytes": 0,
            "card_blocks": 1024,
            "card_rca": 0,
            "card_selected": False,
            "card_overlay": {0: 0, 512: 255},
        }
    }
    encoded = snapconv.convert_blob(source)
    length = struct.unpack("<I", encoded[8:12])[0]
    header = json.loads(encoded[12 : 12 + length])
    assert header["manifest"]["unblock_except"] == [0x1000, 0x2000]
    assert header["components"]["esdhc"]["card_overlay"] == {
        "0": 0,
        "512": 255,
    }
    source["components"]["esdhc"]["card_overlay"] = {True: 1}
    with pytest.raises(ValueError, match="card_overlay"):
        snapconv.convert_blob(source)


def compact_blob(overlay):
    source = blob()
    source["pages"] = {}
    source["all_mapped"] = []
    source["components"] = {
        "timers": {"type": "Timers", "sources": []},
        "esdhc": {
            "type": "Esdhc",
            "version": 1,
            "pattern": 0,
            "armed": None,
            "dma_bytes": 0,
            "card_blocks": 200_000,
            "card_rca": 0,
            "card_selected": False,
            "card_overlay": overlay,
        },
        "edma_tx": {"type": "TxChannel", "version": 1},
        "uart_in": {"type": "deque", "values": []},
    }
    return source


def compact_parts(encoded):
    header_len = struct.unpack("<I", encoded[8:12])[0]
    header = json.loads(encoded[12 : 12 + header_len])
    return header, zlib.decompress(encoded[12 + header_len :])


def test_compact_overlay_encodes_partial_full_and_written_zero_sectors():
    encoded = snapconv.convert_blob(
        compact_blob({0: 0, 1: 0xA5, 511: 0x11, 512: 0x22}),
        compact_overlay=True,
    )
    header, raw = compact_parts(encoded)
    assert encoded[:8] == b"MSTATE\x00\x02"
    assert header["format_version"] == 2
    assert header["components"]["esdhc"]["card_overlay"] == {}
    assert header["overlay_encoding"] == "sector-bitmap-v1"
    assert header["overlay_sector_count"] == 2
    assert header["overlay_written_bytes"] == 4
    assert (
        header["overlay_compressed_len"]
        == len(encoded) - 12 - struct.unpack("<I", encoded[8:12])[0]
    )
    first, second = raw[:584], raw[584:]
    assert struct.unpack("<Q", first[:8])[0] == 0
    assert first[8] == 0b11 and first[8 + 63] == 1 << 7
    assert first[72] == 0 and first[73] == 0xA5 and first[-1] == 0x11
    assert struct.unpack("<Q", second[:8])[0] == 1
    assert second[72] == 0x22
    assert not any(second[73:])


def test_compact_overlay_is_sorted_deterministic_and_v1_stays_default():
    first = compact_blob({1024: 3, 0: 1, 512: 2})
    second = compact_blob({512: 2, 1024: 3, 0: 1})
    assert snapconv.convert_blob(first, compact_overlay=True) == snapconv.convert_blob(
        second, compact_overlay=True
    )
    _, raw = compact_parts(snapconv.convert_blob(first, compact_overlay=True))
    assert [
        struct.unpack("<Q", raw[index : index + 8])[0]
        for index in range(0, len(raw), 584)
    ] == [
        0,
        1,
        2,
    ]
    assert snapconv.convert_blob(blob()) == snapconv.convert_blob(
        blob(), compact_overlay=False
    )


@pytest.mark.parametrize(
    "overlay",
    [{True: 1}, {-1: 1}, {200_000 * 512: 1}, {0: True}, {0: 256}],
)
def test_compact_overlay_rejects_malformed_positions_and_values(overlay):
    with pytest.raises(ValueError, match="card_overlay"):
        snapconv.convert_blob(compact_blob(overlay), compact_overlay=True)


def test_compact_overlay_rejects_unsupported_component_shape():
    source = compact_blob({0: 1})
    source["components"]["ssi0_dma"] = source["components"].pop("edma_tx")
    with pytest.raises(ValueError, match="four supported host components"):
        snapconv.convert_blob(source, compact_overlay=True)


def test_compact_overlay_header_stays_below_one_megabyte_for_large_overlay():
    source = compact_blob({sector * 512: sector & 0xFF for sector in range(100_000)})
    encoded = snapconv.convert_blob(source, compact_overlay=True)
    header_len = struct.unpack("<I", encoded[8:12])[0]
    assert header_len < 1024 * 1024


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
    assert "--compact-overlay" in result.stdout


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
