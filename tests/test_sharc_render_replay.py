"""Firmware-free bounds and SHA checks for rendered-input replay."""

import hashlib
import json

import pytest

from tools import sharc_render_replay


def test_exact_post_swap_input_requires_ordered_ordinals_and_digest(tmp_path):
    source = tmp_path / "rendered.ndjson"
    raw = bytes([3, 0, 205, 171])
    record = {
        "ordinal": 0,
        "source": "taken",
        "byte_len": 4,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "bytes_hex": raw.hex(),
    }
    source.write_text(json.dumps(record) + "\n")
    assert sharc_render_replay.logged_inputs(source, 1) == [(raw, "taken")]
    record["sha256"] = "0" * 64
    source.write_text(json.dumps(record) + "\n")
    with pytest.raises(ValueError, match="ordinal 0"):
        sharc_render_replay.logged_inputs(source, 1)
    record["sha256"] = hashlib.sha256(raw).hexdigest()
    record["ordinal"] = 1
    source.write_text(json.dumps(record) + "\n")
    with pytest.raises(ValueError, match="ordinal 0"):
        sharc_render_replay.logged_inputs(source, 1)


def test_rejects_missing_or_unbounded_input(tmp_path):
    source = tmp_path / "empty.ndjson"
    source.write_text("")
    with pytest.raises(ValueError, match="missing"):
        sharc_render_replay.logged_inputs(source, 1)
    with pytest.raises(ValueError, match="unbounded"):
        sharc_render_replay.logged_inputs(source, 257)


def test_python_native_frame_gate_fails_closed_on_count_and_stop():
    sha = "a" * 64
    observed = [
        {
            "ordinal": 0,
            "source": "repeat",
            "sha256": sha,
            "terminal": "frame-returned",
            "dma_instructions": 6,
            "handler_instructions": 192,
        }
    ]
    native = [
        {
            "ordinal": 0,
            "source": "repeat",
            "sha256": sha,
            "frame_end": "clean",
            "instructions": 198,
        }
    ]
    assert sharc_render_replay.native_agreement(observed, native, None) == "full"
    native[0]["instructions"] = 199
    assert sharc_render_replay.native_agreement(observed, native, None).startswith(
        "frame 0:"
    )
    native[0]["instructions"] = 198
    assert sharc_render_replay.native_agreement(observed, native, "mmr") == "incomplete"
