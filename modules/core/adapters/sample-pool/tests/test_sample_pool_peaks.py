"""Worker-built overviews preserve channel extrema and bounded output."""

import shutil
import subprocess

import pytest
from support.paths import REPO_ROOT


@pytest.mark.skipif(shutil.which("cc") is None, reason="no host C compiler")
def test_peak_bins_stereo_chunk_boundaries_and_capacity(tmp_path):
    source = tmp_path / "peaks.c"
    source.write_text(
        r"""
#include <assert.h>
#include <string.h>
#include "cf/pool_peaks.h"
int main(void) {
    spp_peak p[SPP_BINS], out[SPP_BINS];
    /* 1025 frames => width 3. Split a stereo stream inside a bin. */
    const uint8_t a[] = {0x80,0, 0x7f,0xff, 0,1, 0xff,0xff};
    const uint8_t b[] = {0x40,0, 0xc0,0, 0x20,0, 0xe0,0};
    spp_clear(p);
    spp_feed(p, 1025, 0, a, 2, 2);
    spp_feed(p, 1025, 2, b, 2, 2);
    assert(p[0].min == -32768 && p[0].max == 32767);
    assert(p[1].min == -8192 && p[1].max == 8192);
    assert(p[2].min == 0 && p[2].max == 0);
    memset(out, 0x5a, sizeof(out));
    assert(spp_copy(p, 1025, (uint8_t *)out, 1) == -48);
    assert(((uint8_t *)out)[0] == 0x5a);
    assert(spp_copy(p, 1025, (uint8_t *)out, sizeof(out)) == 342);
    assert(out[0].min == -32768 && out[1].max == 8192);
    /* Mono loader chunks meet at frame 2048 without losing an extremum. */
    uint8_t mono[4098] = {0};
    mono[4094] = 0x80;
    mono[4096] = 0x7f; mono[4097] = 0xff;
    spp_clear(p);
    spp_feed(p, 2049, 0, mono, 2048, 1);
    spp_feed(p, 2049, 2048, mono + 4096, 1, 1);
    assert(p[409].min == -32768 && p[409].max == 32767);
    /* The last frame of a long sample stays in bounds. */
    spp_clear(p);
    spp_feed(p, 134217728, 134217727, a, 1, 1);
    assert(p[511].min == -32768);
    assert(spp_copy(p, 134217728, (uint8_t *)out, sizeof(out)) == 512);
    return 0;
}
"""
    )
    binary = tmp_path / "peaks"
    subprocess.run(
        [
            "cc",
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(REPO_ROOT / "modules/core/adapters/sample-pool/src"),
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    subprocess.run([str(binary)], check=True)
