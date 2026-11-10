"""The SHARC include that `patchkit gen` writes (modules/*/build/gen/<device>.inc) is usable from a DSP source.

The adapter of core/dsp-voice includes `adapter-<device>.inc` (its committed objects are checked by `patchkit build --check`); this file
tests the form of the generated defines. (The include expansion is tested in packages/patchkit/tests/build.rs.)"""

import re

from support.paths import REPO_ROOT

MOD = REPO_ROOT / "modules" / "core" / "adapters" / "dsp-voice"


def test_generated_include_has_no_hex_literals_after_a_plus():
    for inc in MOD.glob("build/gen/*.inc"):
        assert not re.search(r"^#define \S+ 0x", inc.read_text(), re.M), inc
