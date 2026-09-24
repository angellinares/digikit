"""Pytest options for this suite.

Tests marked ``slow`` (long firmware integration runs) are skipped unless
``--slow`` is given.
"""

import pytest


def pytest_addoption(parser):
    parser.addoption("--slow", action="store_true", help="also run tests marked slow")


def pytest_configure(config):
    config.addinivalue_line("markers", "slow: long firmware integration test")


def pytest_collection_modifyitems(config, items):
    if config.getoption("--slow"):
        return
    skip = pytest.mark.skip(reason="slow; run with --slow")
    for item in items:
        if "slow" in item.keywords:
            item.add_marker(skip)
