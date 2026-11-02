"""How verify_route selects acceptance files for a changed tools/ Python path (no Nx, no device)."""

from __future__ import annotations

import tomllib

import pytest

from tools.devtools import verify_route
from tools.devtools.verify_route import RuntimeAcceptance
from tools.patchkit import modroots

A, B, C = "runtime/a.test.ts", "runtime/b.test.ts", "runtime/c.test.ts"
SHARED = "tools/patchkit/patchkit.py"
# Existing files: a path that is not on disk counts as deleted.
ONLY_A = "tools/devtools/verify_budget.py"
DECLARED = "tools/devtools/stablebin.py"
UNKNOWN = "tools/devtools/verify.py"
REFERENCED = "tools/patchkit/deleted_x.py"


def record(file: str, *sources: str) -> RuntimeAcceptance:
    return RuntimeAcceptance("tests/regression", (file,), (), {}, sources, False)


@pytest.fixture(autouse=True)
def fake_routing(monkeypatch: pytest.MonkeyPatch) -> None:
    a = record(A, DECLARED)
    records = {A: a, B: record(B), C: record(C)}
    closures = {a: {DECLARED, ONLY_A}}
    monkeypatch.setattr(
        verify_route, "deleted_path_referenced", lambda path: path == REFERENCED
    )
    monkeypatch.setattr(verify_route, "acceptance_records", lambda: records)
    monkeypatch.setattr(verify_route, "acceptance_shared", lambda step: {SHARED})
    monkeypatch.setattr(
        verify_route, "acceptance_record_closure", lambda r: closures.get(r, set())
    )


def select(step: str, path: str) -> set[str]:
    return set(verify_route.acceptance_files(step, [path], resolve_helpers=False))


def runtime_files() -> set[str]:
    return set(verify_route.acceptance_records())


def test_shared_closure_path_selects_all_runtime_files() -> None:
    assert runtime_files() <= select("runtime-fixture", SHARED)


def test_path_reached_from_one_record_selects_its_files() -> None:
    assert select("runtime-fixture", ONLY_A) == {A}


def test_declared_source_selects_its_records() -> None:
    assert select("runtime-fixture", DECLARED) == {A}


def test_deleted_tools_path_still_referenced_selects_all_runtime_files() -> None:
    assert runtime_files() <= select("runtime-fixture", REFERENCED)


def test_deleted_tools_path_nobody_refers_to_selects_nothing() -> None:
    assert select("runtime-fixture", "tools/patchkit/deleted_y.py") == set()


def test_module_referenced_by_dotted_name_or_import_of_the_stem() -> None:
    path = "tools/patchkit/pk_gone.py"
    assert verify_route.module_referenced(path, "run -m tools.patchkit.pk_gone x")
    assert verify_route.module_referenced(path, "from tools.patchkit import pk_gone\n")
    assert verify_route.module_referenced(path, "from . import a, pk_gone\n")
    assert verify_route.module_referenced(path, "from x import (\n  a,\n  pk_gone,\n)")
    assert verify_route.module_referenced(path, "import pk_gone\n")
    assert not verify_route.module_referenced(
        path, "x = 'pk_gone_other'\npk_gone = 1\n"
    )


def test_outside_smoke_harness(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(verify_route, "smoke_harness_closure", lambda: {ONLY_A})
    assert not verify_route.outside_smoke_harness(ONLY_A)
    assert verify_route.outside_smoke_harness(UNKNOWN)
    assert not verify_route.outside_smoke_harness("docs/TESTING.md")
    assert not verify_route.outside_smoke_harness(REFERENCED)
    assert verify_route.outside_smoke_harness("tools/patchkit/deleted_y.py")


def test_smoke_reasons(monkeypatch: pytest.MonkeyPatch) -> None:
    from tools.devtools import verify

    harness = "tools/patchkit/patchkit.py"  # not gate tooling, so only the closure can exempt it
    monkeypatch.setattr(verify_route, "smoke_harness_closure", lambda: {harness})
    monkeypatch.setattr(verify, "LINT_CONFIG_ONLY", {"pyproject.toml"})
    assert "outside the smoke harness closure" in (verify.smoke_reason([UNKNOWN]) or "")
    assert "lint configuration" in (verify.smoke_reason(["pyproject.toml"]) or "")
    assert verify.smoke_reason([harness]) is None


def test_lint_config_only() -> None:
    base = '[project]\nname = "x"\n[tool.ruff]\nline-length = 80\n[tool.mypy]\nfiles = ["a"]\n'
    ruff = base.replace("80", "100")
    mypy = base.replace('["a"]', '["a", "b"]')
    dep = base.replace('"x"', '"y"')
    assert verify_route.lint_config_only(base, ruff)
    assert verify_route.lint_config_only(base, mypy)
    assert not verify_route.lint_config_only(base, dep)
    assert not verify_route.lint_config_only(base, "[broken")
    assert not verify_route.lint_config_only(None, base)


def test_python_code_unchanged() -> None:
    base = '"""Doc."""\nX = 1\n\n\ndef f():\n    """Doc."""\n    a = 1\n    return a\n'
    edited = base.replace("Doc.", "Other doc.").replace("X = 1", "# note\nX = 1")
    assert verify_route.python_code_unchanged(base, edited)
    assert not verify_route.python_code_unchanged(base, base.replace("X = 1", "X = 2"))
    moved = base.replace("    a = 1\n    return a\n", "    return a\n    a = 1\n")
    assert not verify_route.python_code_unchanged(base, moved)
    assert not verify_route.python_code_unchanged(base, "def f(:\n")
    assert not verify_route.python_code_unchanged(None, base)


def test_registered_module_device_test_routes_to_runtime_fixture_only(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = "modules/core/runtime/tests/x.device.test.ts"
    key = "../../" + path
    monkeypatch.setattr(verify_route, "acceptance_records", lambda: {key: record(key)})
    found = verify_route.fine(path)
    assert found is not None and found.steps == ("runtime-fixture",)
    assert select("runtime-fixture", path) == {key}
    assert select("module-load", path) == set()
    other = "modules/core/runtime/tests/y.device.test.ts"
    assert verify_route.registered_module_test(other) is None


def test_module_project_json_selects_declared_records(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = "modules/machines/x/project.json"
    monkeypatch.setattr(verify_route, "PROJECT_REGISTRATIONS", {path: ("../../a",)})
    found = verify_route.fine(path)
    assert found is not None and found.steps == ("runtime-fixture",)
    assert select("runtime-fixture", path) == {"../../a"}
    assert select("stock", path) == set()
    text = '{"metadata": {"runtimeAcceptance": [{"files": ["gone.device.test.ts"]}]}}'
    assert verify_route.project_record_files(path, text) == set()  # not on disk
    assert verify_route.project_record_files(path, None) == set()
    assert verify_route.project_record_files(path, "[") == set()


def test_path_in_no_closure_selects_nothing() -> None:
    assert select("runtime-fixture", UNKNOWN) == set()


def test_runtime_uploads_are_all_shipped_dynamic_service_or_host_modules() -> None:
    expected = set()
    for mid, moddir in modroots.manifests().items():
        if not moddir.is_relative_to(verify_route.ROOT / "modules"):
            continue
        meta = tomllib.loads((moddir / "module.toml").read_text(encoding="utf-8"))[
            "module"
        ]
        if meta.get("dynamic") or meta.get("host") or meta.get("type") == "service":
            expected.add(mid)
    assert expected
    assert set(verify_route.RUNTIME_UPLOADS) == expected
    assert set(verify_route.RUNTIME_UPLOADS.values()) == {
        (
            "tests/conformance/load-dt2.device.test.ts",
            "tests/conformance/load-dn2.device.test.ts",
        )
    }


def test_module_load_uses_only_the_shared_rule() -> None:
    assert select("module-load", ONLY_A) == set()
    assert select("module-load", REFERENCED) == set()
    assert select("module-load", SHARED)
