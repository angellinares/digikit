"""Synthetic provenance failures for the local checkpoint preparation gate."""

import hashlib
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from tools import checkpointprep  # noqa: E402


def test_verify_rejects_wrong_source_hash_before_snapshot(tmp_path, monkeypatch):
    monkeypatch.setattr(
        checkpointprep,
        "PRODUCTS",
        {
            "test": checkpointprep.Product(
                "0" * 64, "1" * 64, "sections", "checkpoint.snap", "out/test.mstate"
            )
        },
    )
    monkeypatch.setattr(checkpointprep, "ROOT", tmp_path)
    syx = tmp_path / "source.syx"
    syx.write_bytes(b"wrong")
    with pytest.raises(ValueError, match="source SHA-256 mismatch"):
        checkpointprep.verify("test", syx)


def test_verify_rejects_changed_main_os_before_snapshot(tmp_path, monkeypatch):
    source = tmp_path / "source.syx"
    source.write_bytes(b"source")
    expected_source = hashlib.sha256(source.read_bytes()).hexdigest()
    expected_image = hashlib.sha256(b"expected").hexdigest()
    section = tmp_path / "sections"
    section.mkdir()
    (section / ".source-sha256").write_text(expected_source)
    (section / "section_3_MAIN_OS.bin").write_bytes(b"changed")
    monkeypatch.setattr(
        checkpointprep,
        "PRODUCTS",
        {
            "test": checkpointprep.Product(
                expected_source,
                expected_image,
                "sections",
                "checkpoint.snap",
                "out/test.mstate",
            )
        },
    )
    monkeypatch.setattr(checkpointprep, "ROOT", tmp_path)
    with pytest.raises(ValueError, match="MAIN OS SHA-256 mismatch"):
        checkpointprep.verify("test", source)


def test_verify_rejects_bad_section_source_hash(tmp_path, monkeypatch):
    source = tmp_path / "source.syx"
    source.write_bytes(b"source")
    expected_source = hashlib.sha256(source.read_bytes()).hexdigest()
    section = tmp_path / "sections"
    section.mkdir()
    (section / ".source-sha256").write_text("wrong")
    monkeypatch.setattr(
        checkpointprep,
        "PRODUCTS",
        {
            "test": checkpointprep.Product(
                expected_source,
                "0" * 64,
                "sections",
                "checkpoint.snap",
                "out/test.mstate",
            )
        },
    )
    monkeypatch.setattr(checkpointprep, "ROOT", tmp_path)
    with pytest.raises(ValueError, match="section source hash mismatch"):
        checkpointprep.verify("test", source)


def test_verify_rejects_snapshot_loaded_image_mismatch(tmp_path, monkeypatch):
    source = tmp_path / "source.syx"
    source.write_bytes(b"source")
    image = b"image"
    expected_source = hashlib.sha256(source.read_bytes()).hexdigest()
    section = tmp_path / "sections"
    section.mkdir()
    (section / ".source-sha256").write_text(expected_source)
    (section / "section_3_MAIN_OS.bin").write_bytes(image)

    class WrongImageSnapshot:
        components = []

        def __init__(self, _path):
            pass

        def read(self, _address, _size):
            return b"other"

    monkeypatch.setattr(
        checkpointprep,
        "PRODUCTS",
        {
            "test": checkpointprep.Product(
                expected_source,
                hashlib.sha256(image).hexdigest(),
                "sections",
                "checkpoint.snap",
                "out/test.mstate",
            )
        },
    )
    monkeypatch.setattr(checkpointprep, "ROOT", tmp_path)
    monkeypatch.setattr(checkpointprep, "Snapshot", WrongImageSnapshot)
    with pytest.raises(ValueError, match="snapshot loaded MAIN OS mismatch"):
        checkpointprep.verify("test", source)


@pytest.mark.parametrize("limit", [0, 1001])
def test_oracle_trace_rejects_out_of_range_limit(limit, tmp_path):
    with pytest.raises(ValueError, match="limit must be between 1 and 1000"):
        checkpointprep.oracle_trace("dt2", tmp_path / "source.syx", limit)


def test_trace_states_keeps_repeated_program_counter(monkeypatch):
    monkeypatch.setattr(
        checkpointprep,
        "_regs",
        lambda _uc, _constants: {"d": [0] * 8, "a": [0] * 8, "pc": 0x4000, "sr": 0},
    )
    calls = []

    def step(_uc, _constants, _trace, pc):
        calls.append(pc)
        return SimpleNamespace(exception=False, unmapped=False)

    states = checkpointprep._trace_states("test", object(), object(), object(), 2, step)
    assert calls == [0x4000, 0x4000]
    assert [state["clock"] for state in states] == [0, 1, 2]


def test_gate_does_not_start_cargo_when_a_product_fails(monkeypatch, tmp_path):
    prepared = []

    def fake_prepare(product, _syx):
        prepared.append(product)
        if product == "dn2":
            raise ValueError("dn2 failed verification")
        return tmp_path / "dt2.mstate"

    def no_cargo(*_args, **_kwargs):
        raise AssertionError("cargo must not start after failed provenance")

    monkeypatch.setattr(checkpointprep, "prepare", fake_prepare)
    monkeypatch.setattr(checkpointprep.subprocess, "run", no_cargo)
    with pytest.raises(ValueError, match="dn2 failed verification"):
        checkpointprep.gate(tmp_path / "dt2.syx", tmp_path / "dn2.syx")
    assert prepared == ["dt2", "dn2"]


def test_diff_passes_requested_limit_to_rust(monkeypatch, tmp_path):
    monkeypatch.setattr(
        checkpointprep, "prepare", lambda product, _syx: tmp_path / f"{product}.mstate"
    )
    monkeypatch.setattr(
        checkpointprep,
        "oracle_trace",
        lambda product, _syx, _limit: tmp_path / f"{product}.json",
    )
    invoked = {}
    monkeypatch.setattr(
        checkpointprep.subprocess,
        "run",
        lambda _args, **kwargs: invoked.update(kwargs),
    )
    checkpointprep.diff(tmp_path / "dt2.syx", tmp_path / "dn2.syx", 7)
    assert invoked["env"]["NATIVE_CHECKPOINT_DIFF_LIMIT"] == "7"


@pytest.mark.parametrize("limit", [0, 1001])
def test_diff_invalid_limit_does_not_start_cargo(monkeypatch, tmp_path, limit):
    monkeypatch.setattr(
        checkpointprep,
        "prepare",
        lambda *_args: pytest.fail("invalid limit must fail before preparation"),
    )
    monkeypatch.setattr(
        checkpointprep.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("invalid limit must fail before cargo"),
    )
    with pytest.raises(ValueError, match="limit must be between 1 and 1000"):
        checkpointprep.diff(tmp_path / "dt2.syx", tmp_path / "dn2.syx", limit)


def test_diff_does_not_start_cargo_when_a_product_fails(monkeypatch, tmp_path):
    prepared = []

    def fake_prepare(product, _syx):
        prepared.append(product)
        if product == "dn2":
            raise ValueError("dn2 failed verification")
        return tmp_path / "dt2.mstate"

    monkeypatch.setattr(checkpointprep, "prepare", fake_prepare)
    monkeypatch.setattr(
        checkpointprep.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail(
            "cargo must not start after failed provenance"
        ),
    )
    with pytest.raises(ValueError, match="dn2 failed verification"):
        checkpointprep.diff(tmp_path / "dt2.syx", tmp_path / "dn2.syx")
    assert prepared == ["dt2", "dn2"]
