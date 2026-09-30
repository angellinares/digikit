"""Firmware-free checks for the native/Oracle DSPI2 wire comparison seam."""

import struct

import pytest

from tools import wiretrace


def make_trace(*frames: bytes) -> bytes:
    return (
        b"DTFR"
        + struct.pack("<II", 1, len(frames))
        + b"".join(struct.pack("<I", len(frame)) + frame for frame in frames)
    )


def test_wire_comparison_reports_first_ordered_difference(tmp_path, capsys):
    expected = tmp_path / "expected.dtfr"
    actual = tmp_path / "actual.dtfr"
    expected.write_bytes(make_trace(b"\x00\x01", b"\x04\x05", b"\x08"))
    actual.write_bytes(make_trace(b"\x00\x01", b"\x04\x09", b"\x08"))
    assert wiretrace.read(expected) == [b"\x00\x01", b"\x04\x05", b"\x08"]
    assert wiretrace.main([str(expected), str(actual), "--prefix", "1"]) == 0
    assert "matched 1 ordered" in capsys.readouterr().out
    assert wiretrace.main([str(expected), str(actual)]) == 1
    assert "frame 1 byte 0x1: expected 0x05, got 0x09" in capsys.readouterr().out
    actual.write_bytes(make_trace(b"\x00\x01", b"\x04\x05", b"\x08"))
    assert wiretrace.main([str(expected), str(actual)]) == 0
    assert "full trace" in capsys.readouterr().out


def test_wire_comparison_reports_missing_extra_and_length():
    frames = [b"aa", b"bb"]
    assert wiretrace.first_mismatch(frames, [b"aa"], 2) == (
        "frame 1: missing; expected at least 2 frames"
    )
    assert wiretrace.first_mismatch(frames, frames + [b"cc"]) == (
        "frame 2: extra; expected 2 frames, got 3"
    )
    assert wiretrace.first_mismatch(frames, [b"a", b"bb"]) == (
        "frame 0: expected 2 bytes, got 1"
    )
    with pytest.raises(ValueError, match="prefix must be"):
        wiretrace.first_mismatch(frames, frames, 3)


@pytest.mark.parametrize(
    "payload,reason",
    [
        (b"D", "not a DTFR"),
        (b"DTFR" + struct.pack("<II", 2, 1) + struct.pack("<I", 1) + b"x", "version"),
        (b"DTFR" + struct.pack("<II", 1, 0), "frame count"),
        (make_trace(b"ab")[:-1], "invalid length"),
        (make_trace(b"ab") + b"x", "trailing bytes"),
        (
            b"DTFR" + struct.pack("<III", 1, 1, wiretrace.MAX_FRAME_BYTES + 1),
            "invalid length",
        ),
    ],
)
def test_wire_reader_rejects_invalid_data(tmp_path, payload, reason):
    path = tmp_path / "broken.dtfr"
    path.write_bytes(payload)
    with pytest.raises(ValueError, match=reason):
        wiretrace.read(path)
