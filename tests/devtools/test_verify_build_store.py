from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from tools.devtools import verify_build
from tools.patchkit import pk_harness


class Fake:
    """Stands in for the key, the build and the audit of one device."""

    def __init__(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        self.tmp_path = tmp_path
        self.key = "k" * 64
        self.key_after = self.key
        self.missing: list[str] = []
        self.builds = 0
        self.on_build = lambda: None
        monkeypatch.setattr(verify_build.emud_key, "key", lambda _dev: self.key)
        monkeypatch.setattr(verify_build.emud_key, "inputs", lambda _dev: "inputs")
        monkeypatch.setattr(verify_build.emud_key, "key_of", lambda _in: self.key_after)
        monkeypatch.setattr(
            verify_build.emud_key, "audit", lambda _in, _text, _dir: self.missing
        )
        monkeypatch.setattr(verify_build, "build_one", self.build_one)

    def build_one(self, _device: str, _syx: Path) -> tuple[Path, Path]:
        self.builds += 1
        target = self.tmp_path / "target"
        binary = target / "release" / "digi-emud"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(b"binary %d" % self.builds)
        binary.with_suffix(".d").write_text("digi-emud: a.rs\n")
        self.on_build()
        return binary, target

    def ensure(self, root: Path) -> Path:
        return verify_build.ensure_daemon("dn2", Path("stock.syx"), root)


def test_key_changed_during_build_is_an_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = Fake(tmp_path, monkeypatch)
    fake.key_after = "x" * 64
    root = tmp_path / "store"
    with pytest.raises(pk_harness.HarnessError, match="changed during the build"):
        fake.ensure(root)
    assert not (root / "dn2" / fake.key).exists()


def test_audit_findings_are_an_error_and_store_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = Fake(tmp_path, monkeypatch)
    fake.missing = ["/repo/uncovered.rs"]
    root = tmp_path / "store"
    with pytest.raises(pk_harness.HarnessError, match="uncovered.rs"):
        fake.ensure(root)
    assert not (root / "dn2" / fake.key).exists()


def test_concurrent_insert_uses_the_existing_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = Fake(tmp_path, monkeypatch)
    root = tmp_path / "store"
    final = root / "dn2" / fake.key

    def other_process_inserts() -> None:
        final.mkdir(parents=True)
        (final / "digi-emud").write_bytes(b"theirs")
        (final / "key.txt").write_text(fake.key + "\n")

    fake.on_build = other_process_inserts
    binary = fake.ensure(root)
    assert binary == final / "digi-emud"
    assert binary.read_bytes() == b"theirs"
    assert not list((root / "dn2").glob(".tmp-*"))


def make_entry(device_dir: Path, key: str, used: int) -> Path:
    entry = device_dir / key
    entry.mkdir(parents=True)
    (entry / "digi-emud").write_bytes(b"x")
    stamp = entry / "key.txt"
    stamp.write_text(key + "\n")
    os.utime(stamp, (used, used))
    return entry


def test_evict_keeps_the_most_recently_used(tmp_path: Path) -> None:
    device_dir = tmp_path / "dn2"
    for n in range(6):
        make_entry(device_dir, f"k{n}", 1000 + n)
    verify_build.evict(device_dir, "k5")
    assert sorted(p.name for p in device_dir.iterdir()) == ["k2", "k3", "k4", "k5"]


def test_evict_uses_refreshed_use_time(tmp_path: Path) -> None:
    device_dir = tmp_path / "dn2"
    for n in range(6):
        make_entry(device_dir, f"k{n}", 1000 + n)
    os.utime(device_dir / "k0" / "key.txt", (2000, 2000))  # restored lately
    verify_build.evict(device_dir, "k5")
    assert sorted(p.name for p in device_dir.iterdir()) == ["k0", "k3", "k4", "k5"]


def test_evict_never_removes_referenced_or_keep_key(tmp_path: Path) -> None:
    device_dir = tmp_path / "dn2"
    for n in range(7):
        make_entry(device_dir, f"k{n}", 1000 + n)
    verify_build.evict(device_dir, "k0", {"k1"})
    names = sorted(p.name for p in device_dir.iterdir())
    assert names == ["k0", "k1", "k3", "k4", "k5", "k6"]


def test_referenced_keys_reads_daemons_json_of_each_worktree(tmp_path: Path) -> None:
    device_dir = tmp_path / "store" / "dn2"
    with_file = tmp_path / "a"
    (with_file / "dist").mkdir(parents=True)
    pinned = {
        "DIGI_EMUD_DN2": str(device_dir / "kA" / "digi-emud"),
        "DIGI_EMUD_DT2": str(tmp_path / "store" / "dt2" / "kB" / "digi-emud"),
    }
    (with_file / "dist" / "daemons.json").write_text(json.dumps(pinned))
    broken = tmp_path / "b"
    (broken / "dist").mkdir(parents=True)
    (broken / "dist" / "daemons.json").write_text("{not json")
    without = tmp_path / "c"
    without.mkdir()
    keys = verify_build.referenced_keys(device_dir, [with_file, broken, without])
    assert keys == {"kA"}
