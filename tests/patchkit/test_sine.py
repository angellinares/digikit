"""test/sine on the concrete SHARC+ core through core/dsp-voice: the one assertion that no rig check replaces yet, the DN2 wrapper's fall-through.
A slot with an unregistered type goes back to the stock code without the table walk (under 70 instructions, the hostile-context registers restored);
the DT2-only adapter test does not cover the DN2 wrapper dsp-voice-dn2.s. Kept until T1g phase 5 ports it
(docs/plans/ledgers/t1g-retirement.md "Kept until phase 5"). Skips when Selache is not built."""

import pytest
import support.dsp_voice_rig as dv
from support.machine_rig import MachineRig

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = "test/sine"
DEVICES = ["dt2", "dn2"]


# kept until T1g phase 5 ports it
@pytest.mark.parametrize("device", DEVICES)
def test_a_slot_with_an_unregistered_type_goes_back_to_the_stock_code_without_the_table_walk(
    device,
):
    rig = MachineRig(MODULE, device)
    rig.select(1)
    if device == "dt2":
        rig._set_type_dt2(1, 0)
    else:
        rig.hw(74 + 1, 0)
    b = rig.block()  # a drawn context: the stock code gets its own registers back
    assert b.halt["reason"] == "breakpoint" and b.out["final_pc_sw"] == dv.STOCK_BP, (
        b.halt
    )
    ctx = rig.last_ctx
    assert [
        n
        for n in dv.CONTEXT_REGS
        if (b.out["regs"][n] or 0) & 0xFFFFFFFF != ctx[n] & 0xFFFFFFFF
    ] == []
    assert b.instructions < 70, b.instructions
