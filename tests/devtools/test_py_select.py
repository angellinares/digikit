"""py_select: which Python tests an edited file selects."""

from tools.devtools import py_select


def test_a_helper_in_a_modules_tests_directory_selects_that_modules_tests():
    plan = dict(
        py_select.python_test_plan(["modules/machines/tb303/tests/tb303_ref.py"], False)
    )
    assert plan["modules/machines/tb303/tests/test_tb303.py"] == "module-source"


def test_a_helper_edit_selects_only_its_own_module():
    plan = dict(
        py_select.python_test_plan(["modules/machines/tb303/tests/tb303_ref.py"], False)
    )
    assert not [p for p in plan if p.startswith("modules/machines/pluck/")]
