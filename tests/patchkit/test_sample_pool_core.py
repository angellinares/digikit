"""core/sample-pool: the firmware-free logic of the DT2 backing (slot choice, PCM planes, free space).

The header modules/core/adapters/sample-pool/src/cf/pool_core.h takes plain data, so a host C compiler runs it. The firmware calls and the
stock reset are tested in the emulator, not here."""

import shutil
import subprocess

import pytest

from support.paths import REPO_ROOT

SRC = REPO_ROOT / "modules" / "core" / "adapters" / "sample-pool" / "src"
RUNTIME_INC = REPO_ROOT / "modules" / "core" / "runtime" / "include"
CC = shutil.which("cc") or shutil.which("gcc")

HARNESS = r"""
#include <stdio.h>
#include <string.h>
#include "cf/pool_core.h"
#define BASE 0x8422b9c8u
int main(void)
{
    dkpool_handle h;
    static uint32_t keys[SP_SLOTS * SP_KEY_WORDS];
    static uint8_t busy[SP_SLOTS];
    uint32_t rec[SP_REC_BYTES / 4 * 2];
    uint32_t s;
    memset(keys, 0xff, sizeof keys);

    printf("nw32 %x %x %x\n", sp_nw32(BASE), sp_nw32(0x7ffffffc), sp_nw32(BASE + 2));
    /* mono, 1000 frames: P = 2000, F = 2080, A = align_up(P + 96, 8192) = 8192 */
    printf("mono %d %x %x %u %u %u\n", sp_geometry(BASE, 0x10000, 8192, 0, 2000, 2080, &h), h.base_l, h.base_r, h.frames, h.rate, h.channels);
    /* stereo, 1000 frames: P = 4000, F = 4080, A = 8192 */
    printf("stereo %d %x %x %u %u %u\n", sp_geometry(BASE, 0x10000, 8192, 1, 4000, 4080, &h), h.base_l, h.base_r, h.frames, h.rate, h.channels);
    /* a Transfer-like file without the trailer: F = 64 + P */
    printf("notrailer %d %u\n", sp_geometry(BASE, 0x10000, 8192, 0, 2000, 2064, &h), h.frames);
    printf("bad %d %d %d %d %d %d\n",
           sp_geometry(BASE, 0x10000, 8192, 1, 4002, 4082, &h),      /* P not a whole stereo frame */
           sp_geometry(BASE, 0x10000, 8192, 0, 0, 80, &h),           /* no PCM */
           sp_geometry(BASE, 0x10000, 8192, 0, 9000, 9000 + 80, &h), /* plane larger than the extent */
           sp_geometry(BASE, 0x10000, 8192, 1, 8160, 8160 + 80, &h), /* stereo planes overlap the right plane's start */
           sp_geometry(BASE, 0x10000, 8192, 0, 2000, 2000 + 64 - 2, &h), /* P larger than the file */
           sp_geometry(BASE, 0x10000, 8192, 0, 8130, 8130 + 64, &h));    /* mono PCM does not fit the extent */

    s = sp_pick_slot(0x3f0, keys, busy);
    printf("pick0 %x\n", s);
    busy[0x3ff] = 1;
    keys[0x3fe * SP_KEY_WORDS] = 7;   /* a stock slot */
    printf("pick1 %x\n", sp_pick_slot(0x3f0, keys, busy));
    for (s = 0x3f0; s < 0x400; s++) busy[s] = 1;
    printf("pick2 %x\n", sp_pick_slot(0x3f0, keys, busy));
    memset(busy, 0, sizeof busy);
    for (s = 0x381; s < 0x400; s++) busy[s] = 1;
    printf("pick3 %x\n", sp_pick_slot(0x380, keys, busy));   /* 0x380 is never a slot: (slot & 0x7f) == 0 */
    printf("pick4 %x\n", sp_pick_slot(0, keys, busy));
    printf("lost %d %d\n", sp_slot_lost(keys, 0x3fe), sp_slot_lost(keys, 0x3fd));

    memset(rec, 0, sizeof rec);
    rec[SP_REC_BLOCK / 4] = 0x10000; rec[SP_REC_EXTENT / 4] = 0x8000;
    rec[(SP_REC_BYTES + SP_REC_BLOCK) / 4] = 0x20000; rec[(SP_REC_BYTES + SP_REC_EXTENT) / 4] = 0x4000;
    printf("free %u %u\n", sp_free_frames(rec, rec + SP_REC_BYTES / 4 * 2), sp_free_frames(rec, rec));
    {
        uint32_t rel;
        int32_t r0 = sp_finish(0, 0, &rel);
        printf("finish %d %u", r0, rel);
        r0 = sp_finish(DKPOOL_E_POOL, 0, &rel);
        printf(" %d %u", r0, rel);
        r0 = sp_finish(0, 1, &rel);                   /* cancelled, but it loaded: report lost, release the slot */
        printf(" %d %u", r0, rel);
        r0 = sp_finish(DKPOOL_E_LOST, 1, &rel);       /* cancelled before it started */
        printf(" %d %u", r0, rel);
        r0 = sp_finish(DKPOOL_E_READ, 1, &rel);       /* cancelled and it failed anyway: the real error, nothing to release */
        printf(" %d %u\n", r0, rel);
    }
    return 0;
}
"""


@pytest.mark.skipif(CC is None, reason="no host C compiler")
def test_pool_core_logic(tmp_path):
    src = tmp_path / "h.c"
    src.write_text(HARNESS)
    exe = tmp_path / "h"
    subprocess.run([CC, "-Wall", "-Werror", f"-I{SRC}", f"-I{RUNTIME_INC}", str(src), "-o", str(exe)], check=True)
    out = {}
    for line in subprocess.run([str(exe)], check=True, capture_output=True, text=True).stdout.splitlines():
        key, *rest = line.split()
        out[key] = rest

    nw = lambda phys: 0x10000000 + ((phys - 0x80000000) >> 2)
    base = 0x8422B9C8
    # the DSP's normal-word alias of a DDR byte; none outside the alias or off a word boundary
    assert out["nw32"] == [f"{nw(base):x}", "0", "0"]
    # mono: PCM at block + 64, both planes the same
    assert out["mono"] == ["0", f"{nw(base + 0x10000 + 64):x}", f"{nw(base + 0x10000 + 64):x}", "1000", "48000", "1"]
    # stereo: left at block + 32, right at block + A/2 + 32
    assert out["stereo"] == ["0", f"{nw(base + 0x10000 + 32):x}", f"{nw(base + 0x10000 + 4096 + 32):x}", "1000", "48000", "2"]
    assert out["notrailer"] == ["0", "1000"]
    assert out["bad"] == ["-5", "-5", "-5", "-5", "-5", "-5"]
    # slots from 0x3ff down; busy and stock-used slots are skipped; none left gives 0
    assert out["pick0"] == ["3ff"]
    assert out["pick1"] == ["3fd"]
    assert out["pick2"] == ["0"]
    assert out["pick3"] == ["0"]
    assert out["pick4"] == ["0"]
    assert out["lost"] == ["0", "1"]
    # free space: below the loader limit, after the highest record end (0x20000 + 0x4000), minus 8 KiB headroom, in 2-byte frames
    assert out["free"] == [str((0x19000400 - 0x24000 - 8192) // 2), str((0x19000400 - 8192) // 2)]
    # a finished load job: as it is; cancelled but loaded = lost and release; cancelled before it started = lost; cancelled and failed = the error
    assert out["finish"] == ["0", "0", "-16", "0", "-15", "1", "-15", "0", "-3", "0"]
