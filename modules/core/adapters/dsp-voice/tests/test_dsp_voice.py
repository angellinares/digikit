"""core/dsp-voice: a static check on the assembler. The behavioural tests of the wrapper run on the SHARC rig
(modules/core/adapters/dsp-voice/tests/dsp-voice.rig.test.ts)."""

import re

import pytest
from support.dsp_voice_rig import CORE

from tools.patchkit import pk_sharc, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


C3_COMMON = ("DV_C3_FINE",)
C3_VARIANTS = {
    "a-delayed": frozenset({*C3_COMMON, "DV_C3_DB"}),
    "b-noirq": frozenset({*C3_COMMON, "DV_C3_NOIRQ"}),
    "c-nosys": frozenset({*C3_COMMON, "DV_C3_NOSYS"}),
    # rung C3d (finding 49 section 10): the deep breadcrumbs 11 to 22 (the DAG1 register stores through R2 are the default build now)
    "d1-deep": frozenset({*C3_COMMON, "DV_C3_DEEP"}),
}


INCLUDES = pk_sharc.sharc_include_dirs(
    CORE.parents[2]
)  # the adapter's includes and the SDK's


def test_no_build_of_the_module_stores_a_same_dag_register_through_an_index_register(
    tmp_path,
):
    """Finding 49 section 10: a store through DM(Ia) cannot take a DAG1 register (UREGXDAG1, PRM 15-7); the stock image has none. The
    frame save goes through R2 in every build, and a module build that has such a store fails (pk_sharc.sharc_object)."""

    def stores(defines):
        text = pk_sharc.apply_conditionals(CORE.read_text(), defines)
        return [
            ln.strip()
            for ln in text.splitlines()
            if re.fullmatch(r"\s*DM\(-?\d+, I6\) = [IMLB]([0-7])\b;.*", ln)
            and not ln.strip().startswith("//")
        ]

    for defines in (frozenset(), *C3_VARIANTS.values()):
        assert stores(defines) == []
        pk_sharc.sharc_object(
            [CORE], INCLUDES, defines
        )  # raises BuildError on an excluded store
    # the DN2 copy too
    dn2 = CORE.with_name("dsp-voice-dn2.s")
    pk_sharc.sharc_object([dn2], INCLUDES, frozenset())
    # and a source with one fails, with the parcel address in the message
    bad = tmp_path / "bad.s"
    bad.write_text(".SECTION/PM seg_pmco;\n_x:\n    DM(-16, I6) = I0;\n    RTS;\n")
    # the fork's assembler refuses it first; our check on the decoded word is the second line
    with pytest.raises(
        pk_sharc.PkSharcError,
        match=r"parcel 0x0: DM store of I0 \(DAG1\)|I0 cannot be stored through its own DAG",
    ):
        pk_sharc.sharc_object([bad], [], frozenset())
