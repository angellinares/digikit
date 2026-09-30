"""Firmware-free checks for the deterministic host-event recorder."""

import hashlib

import pytest

import tools.autoevents as autoevents


def test_feed_requests_are_delivered_at_existing_outer_boundary():
    feeds = [autoevents.feed_spec("0x10:2301"), autoevents.feed_spec("32:2300")]
    assert autoevents.due_feeds(feeds, 0, 15) == 0
    assert autoevents.due_feeds(feeds, 0, 17) == 1
    assert autoevents.due_feeds(feeds, 1, 400_000) == 2
    assert feeds == [(16, b"\x23\x01"), (32, b"\x23\x00")]


@pytest.mark.parametrize("spec", ["", "99", "1:", "-1:ab", "1:" + "aa" * 65])
def test_rejects_invalid_feeds(spec):
    with pytest.raises(ValueError):
        autoevents.feed_spec(spec)


def test_peer_captures_exact_wire_bytes_without_clock_claims():
    events = []
    peer = autoevents.FramePeer(events)
    peer.clock = lambda: 100
    peer.forced = lambda: 3
    tx = b"\x02" * 42
    assert peer.exchange(tx) == bytes(42)
    assert peer.frames == [tx]
    assert events == [
        {
            "kind": "tx",
            "index": 0,
            "after_force": 3,
            "service_clock_floor": 100,
            "length": 42,
            "sha256": hashlib.sha256(tx).hexdigest(),
            "release_window_hex": tx[0x24:0x2B].hex(),
        }
    ]


def test_derived_output_cannot_escape_ignored_out(tmp_path):
    with pytest.raises(ValueError, match="ignored out"):
        autoevents._output(tmp_path / "trace.dtfr")
