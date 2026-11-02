from __future__ import annotations

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


def test_miss_builds_once_and_stores(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = Fake(tmp_path, monkeypatch)
    root = tmp_path / "store"
    binary = fake.ensure(root)
    assert fake.builds == 1
    assert binary == root / "dn2" / fake.key / "digi-emud"
    assert binary.read_bytes() == b"binary 1"
    assert binary.stat().st_mode & 0o777 == 0o755
    assert (binary.parent / "key.txt").read_text() == fake.key + "\n"
    assert (binary.parent / "depfile.d").is_file()


def test_hit_does_not_build(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    fake = Fake(tmp_path, monkeypatch)
    root = tmp_path / "store"
    first = fake.ensure(root)
    second = fake.ensure(root)
    assert fake.builds == 1
    assert second == first


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


def test_eviction_keeps_the_four_most_recently_used(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = Fake(tmp_path, monkeypatch)
    root = tmp_path / "store"
    keys = [f"{i}" * 64 for i in range(6)]
    for age, key in enumerate(keys):
        fake.key = fake.key_after = key
        fake.ensure(root)
        stamp = 1_000_000 + age
        os.utime(root / "dn2" / key / "key.txt", (stamp, stamp))
    kept = sorted(
        p.name for p in (root / "dn2").iterdir() if not p.name.startswith(".")
    )
    assert kept == sorted(keys[2:])


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
