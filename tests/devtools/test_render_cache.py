"""Tests for tools/devtools/render_cache.py (firmware-free)."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]

from tools.devtools import render_cache as rc  # noqa: E402


@pytest.fixture
def world(tmp_path, monkeypatch):
    dev = tmp_path / "devices"
    dev.mkdir()
    (dev / "dt2.toml").write_text("a = 1\n")
    monkeypatch.setenv("DIGI_DEVICES_DIR", str(dev))
    binary = tmp_path / "bin"
    binary.write_bytes(b"binary-1")
    inp = tmp_path / "in.bin"
    inp.write_bytes(b"input-1")
    return tmp_path, dev, binary, inp


def _key(world, argv=None, env=None, device="dt2", peeks=("a",), binary=None):
    tmp, _dev, b, inp = world
    argv = argv if argv is not None else [str(inp), "--x", "1"]
    e = {"DIGI_DEVICES_DIR": os.environ["DIGI_DEVICES_DIR"], **(env or {})}
    return rc.context_key(binary or b, argv, e, device, peeks)


def _bump(path: Path, data: bytes) -> None:
    path.write_bytes(data)
    st = path.stat()
    os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns + 5_000_000_000))


def test_semantic_env(tmp_path):
    f = tmp_path / "f.bin"
    f.write_bytes(b"one")
    env = {
        "SHARC_AOT": "1",
        "DSP_IDLE_SKIP": "1",
        "DIGI_DSP_PERIOD": "9",
        "DN2_PROFILE_LINK": "x",
        "DIGI_JOBS": "4",
        "PATH": "/bin",
        "HOME": "/h",
        "SHARC_FAST_TL_X": "1",
        "DIGI_EMU_SYX_DT2": "p",
        "DIGI_X": f"@{f}",
    }
    out = rc.semantic_env(env)
    assert set(out) == {
        "SHARC_AOT",
        "DSP_IDLE_SKIP",
        "DIGI_DSP_PERIOD",
        "DN2_PROFILE_LINK",
        "DIGI_X",
    }
    assert out["DIGI_X"].startswith("@sha256:")
    _bump(f, b"two")
    assert rc.semantic_env(env)["DIGI_X"] != out["DIGI_X"]


def test_context_key_changes(world):
    tmp, dev, binary, inp = world
    base = _key(world)
    assert _key(world) == base
    assert _key(world, env={"SHARC_AOT": "1"}) != base
    assert _key(world, env={"PATCHKIT_MODULES": "/modules/a"}) != base
    assert _key(world, env={"PATCHKIT_MODULES": "/modules/a"}) != _key(
        world, env={"PATCHKIT_MODULES": "/modules/b"}
    )
    assert _key(world, peeks=("b",)) != base
    assert _key(world, device="dn2") != base
    _bump(inp, b"input-2")
    assert _key(world) != base
    base = _key(world)
    _bump(dev / "dt2.toml", b"a = 2\n")
    assert _key(world) != base
    base = _key(world)
    _bump(binary, b"binary-2")
    assert _key(world) != base


def test_context_key_ignores(world):
    inp = world[3]
    base = _key(world, argv=[str(inp)])
    full = [str(inp), "--out", "a", "--wav", "b", "--dump-dsp", "c"]
    assert _key(world, argv=full) == base
    other = [str(inp), "--out", "z", "--wav=y", "--scenario", "s1"]
    assert _key(world, argv=other) == base
    assert _key(world, argv=[str(inp), "--scenarios", "q"]) == base
    assert _key(world, argv=[str(inp)], env={"DIGI_JOBS": "8"}) == base
    chk = {"DIGI_RENDER_CACHE_CHECK": "0.5"}
    assert _key(world, argv=[str(inp)], env=chk) == base


def test_scenario_key():
    s = {"events": [1], "seconds": 2, "expect_sha256": "ab"}
    k = rc.scenario_key("ctx", s)
    assert rc.scenario_key("ctx", dict(s)) == k
    assert rc.scenario_key("ctx2", s) != k
    for field, val in [("events", [2]), ("seconds", 3), ("expect_sha256", "cd")]:
        assert rc.scenario_key("ctx", {**s, field: val}) != k


def test_cache_roundtrip(tmp_path):
    cache = rc.RenderCache(tmp_path / "c", enabled=True)
    wav = tmp_path / "a.wav"
    wav.write_bytes(b"RIFFdata")
    key = "ab" + "0" * 62
    assert cache.load(key) is None
    cache.store(key, {"k": 1, "scenarios": [1]}, {"name": "n"}, wav)
    top, entry, path = cache.load(key)
    assert top == {"k": 1}
    assert entry == {"name": "n"}
    assert path.read_bytes() == b"RIFFdata"
    assert cache.stats()["entries"] == 1
    assert cache.stats()["bytes"] > 0
    cache.store(key, {"k": 2}, {"name": "m"}, wav)
    assert cache.load(key)[1] == {"name": "n"}
    assert cache.clear() == 1
    assert cache.stats() == {"entries": 0, "bytes": 0}


def test_prune_and_load_touch(tmp_path):
    cache = rc.RenderCache(tmp_path / "c", enabled=True)
    wav = tmp_path / "a.wav"
    wav.write_bytes(b"x" * 100)
    keys = [f"{i:02x}" + "0" * 62 for i in range(3)]
    for key in keys:
        cache.store(key, {}, {}, wav)
    for age, key in enumerate(keys):  # keys[0] oldest
        d = cache._dir(key)
        os.utime(d, (1_000_000 + age, 1_000_000 + age))
    assert cache.load(keys[0]) is not None  # a hit refreshes the mtime
    assert cache._dir(keys[0]).stat().st_mtime > 1_000_002
    per_entry = cache.stats()["bytes"] // 3
    assert cache.prune(2 * per_entry) == 1
    assert cache.load(keys[1]) is None  # now the oldest
    assert cache.load(keys[0]) is not None
    assert cache.load(keys[2]) is not None


def test_cache_disabled(tmp_path, monkeypatch):
    wav = tmp_path / "a.wav"
    wav.write_bytes(b"x")
    cache = rc.RenderCache(tmp_path / "c", enabled=False)
    cache.store("cd" + "0" * 62, {}, {}, wav)
    assert cache.load("cd" + "0" * 62) is None
    assert cache.stats()["entries"] == 0
    monkeypatch.setenv("DIGI_RENDER_CACHE", "0")
    assert not rc.default_enabled()
    assert not rc.RenderCache(tmp_path / "c").enabled
    monkeypatch.delenv("DIGI_RENDER_CACHE")
    assert rc.RenderCache(tmp_path / "c").enabled
