"""Pytest options for this suite.

Tests marked ``slow`` (long firmware integration runs) are skipped unless
``--slow`` is given.
"""

import pytest

from tools.sharc_dev.sharc_transpile_run import NativeLibraryUnusable


def pytest_addoption(parser):
    parser.addoption("--slow", action="store_true", help="also run tests marked slow")


def pytest_configure(config):
    config.addinivalue_line("markers", "slow: long firmware integration test")


@pytest.hookimpl(wrapper=True)
def pytest_runtest_call(item):
    """A test that needs the native SHARC library skips, with the reason, when
    the library is missing symbols or stale (tools/sharc_dev/sharc_transpile_run.py)."""
    try:
        return (yield)
    except NativeLibraryUnusable as exc:
        pytest.skip(str(exc))


def pytest_collection_modifyitems(config, items):
    if config.getoption("--slow"):
        return
    skip = pytest.mark.skip(reason="slow; run with --slow")
    for item in items:
        if "slow" in item.keywords:
            item.add_marker(skip)
