"""Module-owned device acceptance survives retiring Python scenario dispatch."""

import os
import sys
from contextlib import contextmanager
from types import SimpleNamespace

import pytest

from tools.devtools import benchlock
from tools.patchkit import patchkit, pk_harness, pk_modcheck


def test_only_the_nx_graph_test_device_target_declares_device_acceptance(
    tmp_path, monkeypatch
):
    from tools.devtools import nx_graph

    folder = tmp_path / "module"
    folder.mkdir()
    monkeypatch.setattr(pk_modcheck.modroots, "module_dir", lambda _mid: folder)

    graph = {
        "nodes": {
            "owned/effect": {"data": {"targets": {"test-device": {}}}},
            "other/effect": {"data": {"targets": {"test-host": {}}}},
        }
    }
    monkeypatch.setattr(nx_graph, "nx_graph", lambda: graph)
    assert pk_modcheck.own_device_target("owned/effect") == "owned/effect:test-device"
    assert pk_modcheck.own_device_target("other/effect") is None
    assert pk_modcheck.own_device_target("missing/effect") is None
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_machine_module", lambda _mid: False)
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_fx_module", lambda _mid: False)
    assert pk_modcheck.plan("owned/effect", "dt2") == ([], {})


def test_nested_test_only_project_owns_module_device_acceptance(monkeypatch):
    from tools.devtools import nx_graph

    nested = {
        "root": "modules/tools/example/tests",
        "tags": ["test-only"],
        "metadata": {"runtimeAcceptance": [{"files": ["acceptance.device.test.ts"]}]},
        "targets": {"test-device": {}},
    }
    nodes = {
        "owned/module": {"data": {"root": "modules/tools/example", "targets": {}}},
        "owned/acceptance": {"data": nested},
        "unrelated/acceptance": {
            "data": {**nested, "root": "modules/tools/another/tests"}
        },
    }
    monkeypatch.setattr(nx_graph, "nx_graph", lambda: {"nodes": nodes})
    assert (
        pk_modcheck.own_device_target("owned/module") == "owned/acceptance:test-device"
    )
    nested["tags"] = []
    assert pk_modcheck.own_device_target("owned/module") is None
    nested["tags"] = ["test-only"]
    nested["metadata"] = {}
    assert (
        pk_modcheck.own_device_target("owned/module") == "owned/acceptance:test-device"
    )
    nested["targets"] = {}
    assert pk_modcheck.own_device_target("owned/module") is None


@pytest.mark.parametrize("root_has_target", [False, True])
def test_multiple_device_acceptance_owners_fail_clearly(monkeypatch, root_has_target):
    from tools.devtools import nx_graph

    nested = {
        "root": "modules/tools/example/tests",
        "tags": ["test-only"],
        "metadata": {"runtimeAcceptance": [{"files": ["acceptance.device.test.ts"]}]},
        "targets": {"test-device": {}},
    }
    nodes = {
        "owned/module": {
            "data": {
                "root": "modules/tools/example",
                "targets": {"test-device": {}} if root_has_target else {},
            }
        },
        "owned/acceptance": {"data": nested},
    }
    if not root_has_target:
        nodes["owned/second-acceptance"] = {
            "data": {**nested, "root": "modules/tools/example/other-tests"}
        }
    monkeypatch.setattr(nx_graph, "nx_graph", lambda: {"nodes": nodes})
    with pytest.raises(SystemExit, match="ambiguous device acceptance owners"):
        pk_modcheck.own_device_target("owned/module")


@pytest.mark.parametrize(
    "python_tests, owned, python_result, device_result, expected, stages",
    [
        (True, True, 0, 0, 0, ["python", "device", "size"]),
        (True, True, 3, 0, 3, ["python"]),
        (True, True, 0, 7, 7, ["python", "device"]),
        (False, True, 0, 0, 0, ["sources", "device", "size"]),
        (False, True, 0, 7, 7, ["sources", "device"]),
        (False, False, 0, 0, 0, ["sources", "size"]),
        (True, False, 0, 0, 0, ["python", "size"]),
    ],
)
def test_owned_device_stage_follows_python_and_gates_size(
    monkeypatch,
    tmp_path,
    python_tests,
    owned,
    python_result,
    device_result,
    expected,
    stages,
):
    observed = []
    monkeypatch.setattr(sys, "argv", ["pk_modcheck", "owned/module", "--device", "dt2"])
    monkeypatch.setattr(pk_modcheck, "resolve", lambda _name: "owned/module")
    monkeypatch.setattr(
        pk_modcheck,
        "plan",
        lambda _mid, _dev: (["owned_test.py"] if python_tests else [], {}),
    )
    monkeypatch.setattr(
        pk_modcheck,
        "own_device_target",
        lambda _mid: "owned-effect:test-device" if owned else None,
    )
    monkeypatch.setattr(pk_modcheck.modroots, "module_dir", lambda _mid: tmp_path)

    def python(command, **kwargs):
        assert "pytest" in command
        assert kwargs["cwd"] == pk_modcheck.ROOT
        observed.append("python")
        return SimpleNamespace(returncode=python_result)

    def sources(folder, *, check):
        assert folder == tmp_path and check
        observed.append("sources")

    def device(mid, target):
        assert mid == "owned/module" and target == "owned-effect:test-device"
        observed.append("device")
        return device_result

    def size(mid, devices, trace):
        assert mid == "owned/module" and devices == ["dt2"] and not trace
        observed.append("size")
        return []

    monkeypatch.setattr(pk_modcheck.subprocess, "run", python)
    monkeypatch.setattr(patchkit, "build_module", sources)
    monkeypatch.setattr(pk_modcheck, "run_device_target", device)
    monkeypatch.setattr(pk_modcheck, "size_lines", size)
    assert pk_modcheck.main() == expected
    assert observed == stages


@pytest.mark.parametrize(
    "overrides, expected",
    [
        (
            {"DIGI_EMUD": "/shared", "DIGI_EMUD_DT2": "/dt2"},
            {"dt2": "/dt2", "dn2": "/shared"},
        ),
        (
            {"DIGI_EMUD": "/shared", "DIGI_EMUD_DN2": "/dn2"},
            {"dt2": "/shared", "dn2": "/dn2"},
        ),
        (
            {"DIGI_EMUD_DT2": "/dt2", "DIGI_EMUD_DN2": "/dn2"},
            {"dt2": "/dt2", "dn2": "/dn2"},
        ),
    ],
)
def test_owned_target_preserves_both_current_devices_and_legacy_overrides(
    tmp_path, monkeypatch, overrides, expected
):
    for key in ("DIGI_EMUD", "DIGI_EMUD_DT2", "DIGI_EMUD_DN2"):
        monkeypatch.delenv(key, raising=False)
    for key, value in overrides.items():
        monkeypatch.setenv(key, value)
    monkeypatch.setenv("DIGI_PK_READY_DT2_117", "/ready-dt2")
    monkeypatch.setenv("DIGI_PK_READY_DN2_112", "/ready-dn2")
    held = False
    cells = []

    @contextmanager
    def lock(mode, _label):
        nonlocal held
        assert mode == "shared"
        held = True
        try:
            yield
        finally:
            held = False

    def inputs(cell):
        assert held
        cells.append((cell.device, cell.firmware))
        return tmp_path / f"{cell.device}.syx", tmp_path

    daemon_binary = pk_harness.daemon_binary

    def binary(device, syx, pgo):
        assert held and pgo is None
        return daemon_binary(device, syx, pgo)

    def child(command, *, cwd, env):
        assert not held and cwd == pk_modcheck.ROOT
        assert "owned-effect:test-device" in command
        assert env["DIGI_EMUD_DT2"] == expected["dt2"]
        assert env["DIGI_EMUD_DN2"] == expected["dn2"]
        assert env["DIGI_PK_READY_DT2_117"] == "/ready-dt2"
        assert env["DIGI_PK_READY_DN2_112"] == "/ready-dn2"
        assert env["PYTHONPATH"].split(os.pathsep)[0] == str(pk_modcheck.ROOT)
        return SimpleNamespace(returncode=11)

    monkeypatch.setattr(benchlock, "hold", lock)
    monkeypatch.setattr(pk_harness, "firmware_inputs", inputs)
    monkeypatch.setattr(pk_harness, "daemon_binary", binary)
    monkeypatch.setattr(pk_modcheck.subprocess, "run", child)
    assert (
        pk_modcheck.run_device_target("owned/module", "owned-effect:test-device") == 11
    )
    assert cells == [("dt2", "1.17"), ("dn2", "1.12")]
