"""Firmware-free checks of first-divergence and log identity reporting."""

import hashlib
import json
import struct

import pytest

import tools.wireevents as wireevents


def _trace(path, frames):
    blob = b"DTFR" + struct.pack("<II", 1, len(frames))
    for frame in frames:
        blob += struct.pack("<I", len(frame)) + frame
    path.write_bytes(blob)
    return hashlib.sha256(blob).hexdigest()


def test_reports_first_offset_and_release_displacement(tmp_path):
    source = tmp_path / "source.dtfr"
    native = tmp_path / "native.dtfr"
    src = [bytes(48), bytes(0x25) + b"\x01" + bytes(12), bytes(48)]
    dst = [bytes(48), bytes(50), bytes(0x25) + b"\x01" + bytes(12)]
    digest = _trace(source, src)
    _trace(native, dst)
    events = tmp_path / "events.json"
    events.write_text(
        json.dumps(
            {
                "format_version": 1,
                "dtfr_sha256": digest,
                "frames": 3,
                "actual_credited_instructions": 20,
                "stepping": "fast-observed",
                "events": [
                    {"kind": "force", "clock": 12, "forced": 2, "pre_pc": 42},
                    {
                        "kind": "feed",
                        "request_clock": 11,
                        "clock": 13,
                        "index": 0,
                        "pre_pc": 45,
                    },
                    {"kind": "force", "clock": 14, "forced": 3, "pre_pc": 42},
                ],
            }
        )
    )
    log = tmp_path / "probe.log"
    log.write_text(
        "NATIVE_FORCE 2 at 10 PC 0x0\n"
        "panel RX 0 at 13: pre-PC 0x2d, SR 0x2700\n"
        "NATIVE_FORCE 3 at 12 PC 0x0\n"
    )
    report, matched = wireevents.compare(source, native, events, native_log=log)
    assert not matched
    assert "frame 1 byte 0x25" in report
    assert "force 2: source=12, native=10, delta=-2" in report
    assert "release[0x24:0x2b]" in report
    assert "source delivered=13, native delivered=13" in report
    events_data = json.loads(events.read_text())
    events_data["dtfr_sha256"] = "0" * 64
    events.write_text(json.dumps(events_data))
    with pytest.raises(ValueError, match="do not match"):
        wireevents.compare(source, native, events, native_log=log)


def test_replay_log_must_link_exact_source_dtfr(tmp_path):
    source = tmp_path / "source.dtfr"
    digest = _trace(source, [bytes(48)])
    expected = tmp_path / "source.json"
    expected.write_text(
        json.dumps(
            {
                "format_version": 1,
                "dtfr_sha256": digest,
                "frames": 1,
                "actual_credited_instructions": 1,
                "events": [],
            }
        )
    )
    actual = tmp_path / "replay.json"
    actual.write_text(
        json.dumps({"format_version": 1, "source_dtfr_sha256": "0" * 64, "events": []})
    )
    with pytest.raises(ValueError, match="not replayed from this source"):
        wireevents.compare(source, source, expected, native_events=actual)
