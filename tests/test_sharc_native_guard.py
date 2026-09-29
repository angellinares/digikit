"""The native SHARC+ library's staleness guard: a library carries the hash
of the tools/sharc_core sources and the generator version its code was
generated from (sharc_native_info), and tools/sharc_transpile_run.py
refuses one that does not match this tree (native/live checks the core
hash against its live pack: cargo test in native/live).

The checks on a real library need one built with this tree's native/sharc
(``SHARC_NATIVE_GUARD_LIB``, else the default library) and are skipped
without it.
"""

import os
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import sharc_transpile as tp  # noqa: E402
import sharc_transpile_run as run  # noqa: E402

LIB = os.environ.get("SHARC_NATIVE_GUARD_LIB", run.DEFAULT_LIB)


def current() -> dict:
    return {
        "core_sha256": tp.core_hash(),
        "generator_version": tp.GENERATOR_VERSION,
        "image_sha256": "none",
        "blocks": 0,
    }


def test_a_library_from_the_current_sources_is_accepted():
    run.check_build_info(current(), "lib")


def test_a_library_from_another_core_is_refused_naming_the_command(monkeypatch):
    monkeypatch.delenv(run.ALLOW_STALE_ENV, raising=False)
    info = dict(current(), core_sha256="0" * 64)
    with pytest.raises(run.StaleNativeLibrary) as e:
        run.check_build_info(info, "some/lib.dylib")
    msg = str(e.value)
    assert "some/lib.dylib" in msg and "0" * 64 in msg and tp.core_hash() in msg
    assert "tools/sharc_rsgen.py" in msg and "cargo build --release" in msg


def test_a_library_from_another_generator_version_is_refused(monkeypatch):
    monkeypatch.delenv(run.ALLOW_STALE_ENV, raising=False)
    with pytest.raises(run.StaleNativeLibrary, match="generator version 0"):
        run.check_build_info(dict(current(), generator_version=0), "lib")
    old = {k: v for k, v in current().items() if k != "generator_version"}
    with pytest.raises(run.StaleNativeLibrary, match="predates the version stamp"):
        run.check_build_info(old, "lib")


def test_the_override_turns_the_refusal_into_a_warning(monkeypatch, capsys):
    monkeypatch.setenv(run.ALLOW_STALE_ENV, "1")
    run.check_build_info(dict(current(), core_sha256="x"), "lib")
    assert "stale native SHARC library" in capsys.readouterr().err


@pytest.mark.skipif(not os.path.exists(LIB), reason="no native library built")
def test_native_core_refuses_a_library_the_sources_moved_past(monkeypatch):
    monkeypatch.delenv(run.ALLOW_STALE_ENV, raising=False)
    monkeypatch.setattr(tp, "core_hash", lambda: "f" * 64)
    with pytest.raises(run.StaleNativeLibrary, match="f" * 64):
        run.NativeCore(run.pack_image(None), LIB)


@pytest.mark.skipif(not os.path.exists(LIB), reason="no native library built")
def test_a_library_reports_its_generator_version(monkeypatch):
    monkeypatch.setenv(run.ALLOW_STALE_ENV, "1")
    info = run.NativeCore(run.pack_image(None), LIB).info()
    assert "generator_version" in info, "%s predates the staleness guard: %s" % (
        LIB,
        run.REGENERATE_HINT,
    )
    if info["generator_version"]:
        assert info["core_sha256"] == tp.core_hash()
