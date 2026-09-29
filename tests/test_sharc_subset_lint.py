"""The SHARC core stays inside its translatable subset.

tools/sharc_subset_lint.py counts violations of tools/sharc_core/SUBSET.md
per module; tools/sharc_core/subset_allowlist.json holds what is still
allowed. A count above its allowance is a regression. A count below it
means the allowance must come down: run

    uv run python tools/sharc_subset_lint.py --update
"""

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

import sharc_subset_lint as lint  # noqa: E402


def rules(source, module="demo.py"):
    return sorted(v.rule for v in lint.check_source(module, source))


def test_core_within_allowlist():
    actual = lint.counts(lint.check_core())
    regressions, stale = lint.compare(actual, lint.load_allowlist())
    assert not regressions, "new subset violations:\n" + "\n".join(regressions)
    assert not stale, "lower the allowlist (--update):\n" + "\n".join(stale)


def test_allowlist_names_known_modules_and_rules():
    modules = set(lint.core_modules())
    for module, allowed in lint.load_allowlist().items():
        assert module in modules
        assert set(allowed) <= set(lint.RULES)


def test_lambda_and_closure():
    source = (
        "def f(x):\n    g = lambda y: y\n    def h():\n        return x\n    return h\n"
    )
    assert rules(source) == ["closure", "lambda"]


def test_isinstance_const_is_allowed_symbolic_and_type_are_not():
    source = (
        "def f(v, r):\n"
        "    a = isinstance(v, Const)\n"
        "    b = isinstance(v, Unknown)\n"
        "    c = isinstance(r, str)\n"
        "    return a, b, c\n"
    )
    assert rules(source) == ["isinstance-symbolic", "isinstance-type"]


def test_boundary_functions_are_not_checked():
    source = "def _ureg(values, code):\n    return isinstance(values, PartialConst)\n"
    assert rules(source, "state.py") == []
    assert rules(source, "memory.py") == ["isinstance-symbolic"]


def test_destination_match_is_allowed_only_where_declared():
    source = "def _apply_compute(rn):\n    return isinstance(rn, (str, tuple))\n"
    assert rules(source, "compute.py") == []
    assert rules(source, "forms.py") == ["isinstance-type"]


def test_struct_and_math_only_in_float_primitives():
    source = (
        "import math\n"
        "_isnan = math.isnan\n"
        "def _ldexp(x, n):\n    return math.ldexp(x, n)\n"
        "def other(x):\n    return math.isnan(x)\n"
    )
    assert rules(source, "floats.py") == ["struct-math"]


def test_trap_catch_try_is_allowed():
    trap = (
        "def f(state):\n"
        "    try:\n"
        "        x = g()\n"
        "    except ValueError as error:\n"
        "        return [_stop(state, None, str(error))]\n"
        "    return x\n"
    )
    other = (
        "def f():\n    try:\n        return g()\n    except KeyError:\n        pass\n"
    )
    assert rules(trap) == []
    assert rules(other) == ["try"]


def test_effect_inside_observability_call():
    source = (
        "def f(state, insn, a, v):\n"
        "    _event(state, insn, 'store', address=_render(a),"
        " concrete_write=_dm_write(state, a, 4, v))\n"
    )
    assert rules(source) == ["observability-effect"]


def test_tables_may_build_dicts_functions_may_not():
    source = (
        "TABLE = {1: 2}\n"
        "def _build_table():\n    return {k: k for k in range(3)}\n"
        "def f(x):\n    return {1: x}\n"
    )
    assert rules(source) == ["dict-build"]


def test_comprehension_dynamic_and_host_builtins():
    source = (
        "def f(xs, **kw):\n"
        "    ys = [x for x in xs]\n"
        "    return sorted(ys), getattr(xs, 'a'), 2**len(ys)\n"
    )
    assert rules(source) == [
        "comprehension",
        "dynamic",
        "dynamic",
        "host-builtin",
        "host-builtin",
    ]
