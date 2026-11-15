"""py_select: which Python tests an edit selects."""

from pathlib import Path

import pytest

from tools.devtools import py_select


def helper_with_tests() -> str:
    """A helper (not a test file) of some module's own tests/ directory that has test_*.py files beside it."""
    for f in py_select.module_test_files():
        if py_select.is_module_test_helper(f) and list(
            (py_select.ROOT / f).parent.glob("test_*.py")
        ):
            return f
    pytest.skip("no module has both a test helper and tests")


def test_a_helper_in_a_modules_tests_directory_selects_that_modules_tests():
    helper = helper_with_tests()
    got = dict(py_select.python_test_plan([helper], False))
    for t in (py_select.ROOT / helper).parent.glob("test_*.py"):
        assert got[t.relative_to(py_select.ROOT).as_posix()] == "module-source"


def test_a_helper_edit_selects_only_its_own_module():
    helper = helper_with_tests()
    own = Path(helper).parent.as_posix()
    got = dict(py_select.python_test_plan([helper], False))
    assert [
        p for p, why in got.items() if why == "module-source" and not p.startswith(own)
    ] == []
