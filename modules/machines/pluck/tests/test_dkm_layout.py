"""The packed .dkm of this machine: its DSP section layout (finding 32). Other machines are covered by
tests/patchkit/test_patchkit_dkrt_machine.py with a fixture; the numbers here are this machine's own."""

import pytest

from tools.patchkit import modroots, pk_dkrt, pk_dkrt_machine, pk_toolchains
from tools.patchkit import patchkit as front_end

MODULE_ID = modroots.module_id(__file__)
needs_selache = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None, reason="SHARC+ assembler not installed"
)


@needs_selache
def test_committed_objects_are_current():
    front_end.build_module(modroots.module_dir(MODULE_ID), check=True)


def test_dsp_section_layout(tmp_path):
    tools = pk_dkrt.Tools()
    out = tmp_path / "m.dkm"
    tools.patchkit_run("pack", MODULE_ID, "--device", "dt2", "-o", out)
    info = tools.patchkit_run("dkm-info", out)
    name = MODULE_ID.split("/")[1]
    h = pk_dkrt_machine.machine_hash(MODULE_ID, name)
    assert (
        f"voice type h{h:06x} entry +0x3b6 note_on +0x1ee state 16x16512 clear 16512"
        in info
        and "imports _dv_exp2" in info
    )
    # the guard's ranges are not in the file: the parameter ranges are builder calls in init, so the file carries none
    assert "ranges [0, 0, 0, 0, 0, 0, 0, 0]" in info
