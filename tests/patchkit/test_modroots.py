"""Module roots and ids (tools/patchkit/modroots.py)."""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.patchkit import modroots  # noqa: E402

TOML = '[module]\nid = "{id}"\n'


def test_ids_are_unique_and_every_manifest_is_found() -> None:
    found = modroots.manifests()
    on_disk = {
        p.parent for r in modroots.REPO_ROOTS for p in (ROOT / r).rglob("module.toml")
    }
    assert set(found.values()) == on_disk
    assert len(found) == len(on_disk)
    for mid, d in found.items():
        assert tomllib.loads((d / "module.toml").read_text())["module"]["id"] == mid


def test_fixture_ids_stay_test_slash_name() -> None:
    d = modroots.module_dir("test/registry-demo")
    assert d == ROOT / "tests" / "fixtures" / "modules" / "registry-demo"
    assert modroots.module_dir("test/sine") == ROOT / "tests/fixtures/modules/sine"


def test_module_id_of_a_path() -> None:
    assert (
        modroots.module_id("tests/fixtures/modules/sine/src/dsp/sine.s") == "test/sine"
    )
    assert (
        modroots.module_id(ROOT / "tests/fixtures/modules/registry-demo/module.toml")
        == "test/registry-demo"
    )
    assert modroots.module_id("tools/patchkit/modroots.py") is None


def test_a_duplicate_id_raises(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d = tmp_path / "x"
    d.mkdir()
    (d / "module.toml").write_text(TOML.format(id="test/sine"))
    monkeypatch.setenv("PATCHKIT_MODULES", str(tmp_path))
    with pytest.raises(SystemExit, match="duplicate module id test/sine"):
        modroots.manifests()


def test_an_extra_root_adds_modules(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    d = tmp_path / "anything"
    d.mkdir()
    (d / "module.toml").write_text(TOML.format(id="extra/thing"))
    monkeypatch.setenv("PATCHKIT_MODULES", str(tmp_path))
    assert modroots.module_dir("extra/thing") == d
    assert modroots.module_id(d / "src" / "a.s") == "extra/thing"
