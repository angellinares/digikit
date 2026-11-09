"""Staleness check run when the gate selects python-tests (tools/devtools/py_select.py PY_GATE): the port files match what
port_resolve finds in the stock images. The committed core objects are checked by the Nx target patchkit:objects-check."""

import pytest
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import port_resolve

ROOT = REPO_ROOT
DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
DN2_112 = config.stock("Digitone_II_OS1.12.syx")


@pytest.mark.skipif(
    not all(p.exists() for p in (DT2_117, DN2_112)),
    reason="stock firmware images not present",
)
def test_ports_are_current():
    for name, fw in (
        ("dt2-1.17", DT2_117),
        ("dn2-1.12", DN2_112),
    ):
        got_name, text = port_resolve.resolve(fw)
        assert got_name == name
        assert (ROOT / "devices" / "ports" / f"{name}.toml").read_text() == text
