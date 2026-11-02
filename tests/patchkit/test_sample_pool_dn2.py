"""core/sample-pool, Digitone II object: the allocator, the page protocol and the whole ColdFire transport on the host.

pool_alloc.h and pool_wire.h are firmware-free and compile as they are. The transport (cf/dn2/pool.c, xfer.c and send.c, unchanged) runs
against tests/fixtures/sample-pool-dn2/harness.c: stand-ins for the store, the block clock, the SampleLoader worker, the LP0 port and the
runtime's completion call, plus a small model of the DSP receiver that decodes the pages from the port halves, writes them into a DDR
array and answers through the real reply hook. The harness runs the block interrupt, the UI task and the worker job on one simulated clock.
What the emulator must still show (DSP code, firmware calls, timing) is listed in docs/design/dn2-sample-pool.md section 6."""

import shutil
import subprocess

import pytest
from support.paths import REPO_ROOT

SRC = REPO_ROOT / "modules" / "core" / "adapters" / "sample-pool" / "src"
RUNTIME_INC = REPO_ROOT / "modules" / "core" / "runtime" / "include"
HARNESS = REPO_ROOT / "tests" / "fixtures" / "sample-pool-dn2" / "harness.c"
CC = shutil.which("cc") or shutil.which("gcc")

pytestmark = pytest.mark.skipif(CC is None, reason="no host C compiler")


def build(tmp_path, sources, name, defines=()):
    exe = tmp_path / name
    cmd = [
        CC,
        "-Wall",
        "-Wextra",
        "-Werror",
        "-O1",
        *defines,
        f"-I{SRC}",
        f"-I{RUNTIME_INC}",
        *map(str, sources),
        "-o",
        str(exe),
    ]
    subprocess.run(cmd, check=True)
    return exe


def lines(exe, *args):
    """The harness output, one string per line."""
    return subprocess.run(
        [str(exe), *args], check=True, capture_output=True, text=True
    ).stdout.splitlines()


ALLOC = r"""
#include <stdio.h>
#include <string.h>
#include "cf/pool_alloc.h"

static spa_pool P;
static uint8_t H(int n, uint8_t *h) { memset(h, n, 32); return 0; }
static int32_t open_n(uint32_t pages, uint32_t ch, int id, uint32_t *idx)
{
    uint8_t h[32];
    H(id, h);
    return spa_open(&P, pages * 2048u / ch, ch, 1000u + id, 1, id ? h : 0, idx);
}
/* every page is exactly one of: free, or in a block that is not FREE; extents sorted, merged and in range */
static int invariant(void)
{
    static uint8_t owner[70000];
    uint32_t i, j, free_pages = 0, used = 0;
    memset(owner, 0, P.pages);
    for (i = 0; i < P.n_ext; i++) {
        if (!P.ext[i].count || P.ext[i].start + P.ext[i].count > P.pages) return 0;
        if (i && P.ext[i - 1].start + P.ext[i - 1].count >= P.ext[i].start) return 0;
        for (j = 0; j < P.ext[i].count; j++) owner[P.ext[i].start + j]++;
        free_pages += P.ext[i].count;
    }
    for (i = 0; i < SPA_BLOCKS; i++) {
        spa_block *b = &P.blk[i];
        if (b->state == SPA_FREE) continue;
        for (j = 0; j < b->channels * b->plane_pages; j++) {
            if (b->first_page + j >= P.pages) return 0;
            owner[b->first_page + j]++;
        }
        used += b->channels * b->plane_pages;
    }
    for (i = 0; i < P.pages; i++) if (owner[i] != 1) return 0;
    return free_pages + used == P.pages && P.n_ext <= SPA_EXTENTS;
}

int main(void)
{
    uint32_t a, b, c, d, i;
    dkpool_handle h;
    int32_t rc;

    /* range checks of spa_init */
    printf("init %d %d %d %d %d %d\n",
           spa_init(&P, 0x80531000u, 0x10000000u), spa_init(&P, 0x80530000u, 0x1000u), spa_init(&P, 0x80531800u, 0x1000u),
           spa_init(&P, 0x80531000u, 0), spa_init(&P, 0x90000000u, 0x10000001u), spa_init(&P, 0x9ff00000u, 0x00200000u));
    spa_init(&P, 0x80531000u, 0x10000000u);
    printf("big pages %u extents %u frames %u\n", P.pages, P.n_ext, spa_free_frames(&P));

    /* first fit, and the pages of a stereo block */
    spa_init(&P, 0x80531000u, 64u * 4096u);
    printf("plane %u %u %u %u\n", spa_plane_pages(1), spa_plane_pages(2048), spa_plane_pages(2049), spa_plane_pages(144000));
    rc = spa_open(&P, 3000, 2, 0, 0, 0, &a);
    spa_fill(&P, a, &h);
    printf("stereo %d idx %u first %u plane %u l %x r %x ch %u frames %u rate %u token %u\n", rc, a, P.blk[a].first_page, P.blk[a].plane_pages,
           h.base_l, h.base_r, h.channels, h.frames, h.rate, h.token);
    rc = spa_open(&P, 3000, 1, 0, 0, 0, &b);
    spa_fill(&P, b, &h);
    printf("mono %d first %u l==r %d token %u\n", rc, P.blk[b].first_page, h.base_l == h.base_r, h.token);
    printf("inv %d\n", invariant());
    /* abort in a mixed order: the extents merge back into one */
    spa_abort(&P, a); printf("hole n_ext %u\n", P.n_ext);
    spa_abort(&P, b); printf("merged n_ext %u frames %u\n", P.n_ext, spa_free_frames(&P));

    /* eviction: four blocks of 16 pages, released 2, 0, 3, 1; a 20-page block evicts 2, 0, 3 (the first run that fits is 32..63) */
    spa_init(&P, 0x80531000u, 64u * 4096u);
    {
        uint32_t id[4];
        for (i = 0; i < 4; i++) { open_n(16, 1, (int)i + 1, &id[i]); spa_ready(&P, id[i]); }
        printf("full %d\n", open_n(1, 1, 9, &d));
        spa_release(&P, id[2], P.blk[id[2]].token);
        spa_release(&P, id[0], P.blk[id[0]].token);
        spa_release(&P, id[3], P.blk[id[3]].token);
        spa_release(&P, id[1], P.blk[id[1]].token);
        printf("cached frames %u\n", spa_free_frames(&P));   /* all four cached and adjacent: 64 pages */
        rc = open_n(20, 1, 9, &d);
        printf("evict %d first %u entry %u cached %u free %u %u\n", rc, P.blk[d].first_page, d, P.blk[id[1]].state, P.blk[id[2]].state, P.blk[id[3]].state);
        printf("inv2 %d\n", invariant());
    }

    /* largest run: a READY block in the middle splits it */
    spa_init(&P, 0x80531000u, 64u * 4096u);
    open_n(10, 1, 1, &a); spa_ready(&P, a);
    open_n(10, 1, 2, &b); spa_ready(&P, b);
    open_n(10, 1, 3, &c); spa_ready(&P, c);
    spa_release(&P, a, P.blk[a].token);
    printf("run %u\n", spa_free_frames(&P) / 2048u);          /* 10 cached | b | c | 34 free: 34 */
    spa_release(&P, c, P.blk[c].token);
    printf("run2 %u\n", spa_free_frames(&P) / 2048u);         /* c cached joins the 34 free: 44 */
    spa_release(&P, b, P.blk[b].token);
    printf("run3 %u\n", spa_free_frames(&P) / 2048u);         /* all: 64 */

    /* dedup and tokens */
    spa_init(&P, 0x80531000u, 64u * 4096u);
    {
        uint8_t hash[32]; uint32_t x, y, t1, t2;
        H(5, hash);
        rc = spa_open(&P, 4096, 1, 777, 1, hash, &x); spa_ready(&P, x); t1 = P.blk[x].token;
        printf("new %d refs %u\n", rc, P.blk[x].refs);
        rc = spa_open(&P, 4096, 1, 777, 1, hash, &y);
        printf("share %d same %d refs %u token same %d\n", rc, x == y, P.blk[x].refs, P.blk[x].token == t1);
        rc = spa_open(&P, 4096, 1, 778, 1, hash, &y);   /* another size: another identity */
        printf("other size %d differ %d\n", rc, x != y);
        spa_ready(&P, y);
        rc = spa_open(&P, 4096, 1, 777, 2, hash, &y);   /* another hash kind */
        printf("other kind %d\n", rc);
        spa_abort(&P, y);
        spa_release(&P, x, t1); printf("one release state %u refs %u\n", P.blk[x].state, P.blk[x].refs);
        spa_release(&P, x, t1); printf("two releases state %u refs %u\n", P.blk[x].state, P.blk[x].refs);
        spa_release(&P, x, t1); printf("a third is ignored %u\n", P.blk[x].state);
        rc = spa_open(&P, 4096, 1, 777, 1, hash, &y); t2 = P.blk[y].token;
        printf("cache hit %d same %d new token %d current old %d new %d\n", rc, x == y, t2 > t1, spa_current(&P, x, t1), spa_current(&P, y, t2));
        printf("no id %d\n", spa_open(&P, 4096, 1, 777, 1, 0, &y));
        P.tokens = 0xffffffffu;
        printf("tokens %d\n", spa_open(&P, 4096, 1, 1, 1, 0, &y));
    }

    /* 64 blocks fill the table; the 65th needs an entry and finds no cached block */
    spa_init(&P, 0x80531000u, 128u * 4096u);
    for (i = 0; i < 64; i++) { rc = open_n(1, 1, 100 + (int)i, &a); if (rc != SPA_NEW) break; spa_ready(&P, a); }
    printf("table %u next %d inv %d\n", i, open_n(1, 1, 99, &b), invariant());

    /* a long run of random opens, releases and aborts keeps the books exact */
    spa_init(&P, 0x80531000u, 256u * 4096u);
    {
        uint32_t seed = 12345, ok = 1, n;
        for (n = 0; n < 20000; n++) {
            seed = seed * 1103515245u + 12345u;
            uint32_t r = (seed >> 8) % 100, k = (seed >> 16) % SPA_BLOCKS;
            if (r < 45) {
                rc = open_n(1 + (seed >> 4) % 9, 1 + (seed >> 12) % 2, 1 + (int)((seed >> 3) % 40), &a);
                if (rc == SPA_NEW) { if ((seed >> 20) & 1) spa_ready(&P, a); else spa_abort(&P, a); }
            } else if (r < 90) {
                spa_release(&P, k, P.blk[k].token);
            } else {
                spa_evict_oldest(&P);
            }
            if (!invariant()) { ok = 0; break; }
        }
        printf("fuzz %u %u\n", ok, n);
    }
    return 0;
}
"""


def test_allocator(tmp_path):
    src = tmp_path / "alloc.c"
    src.write_text(ALLOC)
    out = lines(build(tmp_path, [src], "alloc"))

    def nw(phys):
        return 0x10000000 + ((phys - 0x80000000) >> 2)

    base = 0x80531000
    assert nw(base) == 0x1014C400
    assert out == [
        # ranges: the full 256 MiB is fine; below the stock writes, unaligned, empty, unaligned size and past the alias are refused
        "init 0 -1 -1 -1 -1 -1",
        f"big pages 65536 extents 1 frames {65536 * 2048}",
        "plane 1 1 2 71",
        # 3000 frames stereo = 2 pages a plane; the right plane starts 2 pages (2,048 words) after the left one; mono has l == r
        f"stereo 0 idx 0 first 0 plane 2 l {nw(base):x} r {nw(base) + 2048:x} ch 2 frames 3000 rate 48000 token 1",
        "mono 0 first 4 l==r 1 token 2",
        "inv 1",
        "hole n_ext 2",
        f"merged n_ext 1 frames {64 * 2048}",
        # eviction: the table is full of ready blocks; released ones are evicted oldest first, and only as many as the request needs
        # (2, 0 and 3: the first run that fits is 32..63; block 1 stays cached; the new block reuses table entry 0)
        "full -2",
        f"cached frames {64 * 2048}",
        "evict 0 first 32 entry 0 cached 3 free 0 0",
        "inv2 1",
        # the largest run: free and cached pages count, a ready block splits them
        "run 34",
        "run2 44",
        "run3 64",
        # identity is size, hash kind and hash; a ready share keeps the token; a cache hit starts a new life
        "new 0 refs 1",
        "share 1 same 1 refs 2 token same 1",
        "other size 0 differ 1",
        "other kind 0",
        "one release state 2 refs 1",
        "two releases state 3 refs 0",
        "a third is ignored 3",
        "cache hit 1 same 1 new token 1 current old 0 new 1",
        "no id 0",
        "tokens -3",
        # the table holds 64 blocks; nothing more fits and the books stay exact through 20,000 random operations
        "table 64 next -2 inv 1",
        "fuzz 1 20000",
    ]


WIRE = r"""
#include <stdio.h>
#include <string.h>
#include "cf/pool_wire.h"

static void halves(uint16_t *h, uint16_t st, uint16_t err, uint32_t count, uint16_t last)
{
    h[0] = SPW_ACK_MAGIC; h[1] = SPW_ACK_VERSION; h[2] = st; h[3] = err; h[4] = count & 0xffff; h[5] = count >> 16; h[6] = last;
    h[7] = (spw_ack_sum(h) ^ SPW_ACK_XOR) & 0xffff;
}

int main(void)
{
    uint32_t t = spw_tag(0x1ff, 0x1234567), i;
    uint16_t h[8];
    spw_ack a;
    spw_snap s;
    spw_x x;
    uint8_t hdr[64];
    uint32_t frames, ch;
    printf("tag %x %u %x %x\n", spw_tag(1, 5), spw_tag_seq(t), spw_tag_page(t), spw_tag_last(spw_tag(7, 0x1234)));

    /* the receiver's rule: new, repeat, wrong seq, bad prefix, index out of the range */
    printf("rule %u %u %u %u %u %u %u\n", spw_dsp_rule(spw_tag(0, 0), spw_tag(1, 7), 256), spw_dsp_rule(spw_tag(1, 7), spw_tag(1, 7), 256),
           spw_dsp_rule(spw_tag(1, 7), spw_tag(1, 8), 256), spw_dsp_rule(spw_tag(1, 7), spw_tag(3, 8), 256),
           spw_dsp_rule(spw_tag(255, 7), spw_tag(0, 8), 256), spw_dsp_rule(spw_tag(1, 7), 0xc0100008u, 256),
           spw_dsp_rule(spw_tag(1, 7), spw_tag(2, 256), 256));

    /* the acknowledgement: good, then every kind of wrong */
    halves(h, 1, 0x0305, 0x10002, 0x0402);
    printf("ack %d %u %u %u %x %x\n", spw_ack_decode(h, &a), a.status, a.error, a.errcount, a.count, a.last);
    halves(h, 1, 0, 0, 0); h[0] = 0x5351; h[7] = (spw_ack_sum(h) ^ SPW_ACK_XOR) & 0xffff; printf("bad magic %d\n", spw_ack_decode(h, &a));
    halves(h, 1, 0, 0, 0); h[1] = 1; h[7] = (spw_ack_sum(h) ^ SPW_ACK_XOR) & 0xffff; printf("bad version %d\n", spw_ack_decode(h, &a));
    halves(h, 4, 0, 0, 0); printf("bad status %d\n", spw_ack_decode(h, &a));
    halves(h, 1, 0, 5, 9);
    for (i = 0; i < 8; i++) { uint16_t keep = h[i]; h[i] ^= 0x0100; if (spw_ack_decode(h, &a)) printf("torn %u accepted\n", i); h[i] = keep; }
    printf("torn done\n");
    /* the snapshot: odd sequence and a change during the read fail, a stored invalid value reads as nothing */
    memset(&s, 0, sizeof s);
    spw_snap_store(&s, 0, &a);
    printf("snap empty %d\n", spw_snap_read(&s, &a));
    halves(h, 1, 0, 5, 9); spw_ack_decode(h, &a); spw_snap_store(&s, 1, &a);
    printf("snap %d %u\n", spw_snap_read(&s, &a), a.count);
    s.seq++;
    printf("snap odd %d\n", spw_snap_read(&s, &a));
    s.seq++;

    /* the page state machine */
    {
        spw_ack arm = {1, 0, 0, 10, spw_tag_last(spw_tag(4, 99))}, ok = arm, err = arm, dead = arm;
        int32_t r;
        spw_begin(&x, 100);
        printf("wait %d %d\n", spw_poll(&x, 100, 0), spw_poll(&x, 399, 0));
        printf("armed timeout %d\n", spw_poll(&x, 400, 0));
        spw_begin(&x, 100);
        r = spw_poll(&x, 101, &arm);
        printf("ready %d next %x\n", r, spw_next_tag(&x, 6));
        spw_sent(&x, spw_next_tag(&x, 6), 101);
        printf("pending %d %d\n", spw_poll(&x, 102, &arm), spw_poll(&x, 175, 0));       /* no news, then just before the deadline */
        printf("resend at 176 %d\n", spw_poll(&x, 176, 0));
        spw_resent(&x, 176);
        ok.count = 11; ok.last = spw_tag_last(spw_tag(5, 6));
        printf("acked %d base %u next %x\n", spw_poll(&x, 177, &ok), x.base_count, spw_next_tag(&x, 7));
        /* a stale error (same error count) is not ours; a new one ends the load; the code decides the kind */
        spw_begin(&x, 0); err.status = SPW_ST_ERROR; err.errcount = 3; err.error = SPW_ERR_SEQ;
        spw_poll(&x, 1, &err);
        spw_sent(&x, spw_next_tag(&x, 1), 1);
        printf("stale %d", spw_poll(&x, 2, &err));
        err.errcount = 4; printf(" new %d", spw_poll(&x, 3, &err));
        err.error = 9; printf(" other %d\n", spw_poll(&x, 3, &err));
        /* a count that did not move, or a different page, is no acknowledgement */
        spw_begin(&x, 0); spw_poll(&x, 1, &arm); spw_sent(&x, spw_next_tag(&x, 6), 1);
        ok = arm; ok.last = spw_tag_last(spw_tag(5, 6));
        printf("count same %d", spw_poll(&x, 2, &ok));
        ok.count = 11; ok.last = spw_tag_last(spw_tag(5, 7));
        printf(" other page %d", spw_poll(&x, 2, &ok));
        ok.last = spw_tag_last(spw_tag(5, 6)); ok.status = SPW_ST_UNARMED;
        printf(" unarmed %d\n", spw_poll(&x, 2, &ok));
        /* three resends, then the timeout; dead wins at any time */
        spw_begin(&x, 0); spw_poll(&x, 1, &arm); spw_sent(&x, spw_next_tag(&x, 6), 1);
        printf("deadline");
        for (i = 0; i < 4; i++) { r = spw_poll(&x, 1 + 75 * (i + 1), 0); printf(" %d", r); if (r == SPW_RESEND) spw_resent(&x, 1 + 75 * (i + 1)); }
        dead.status = SPW_ST_DEAD;
        printf(" dead %d\n", spw_poll(&x, 2000, &dead));
        /* the clock wraps */
        spw_begin(&x, 0xfffffff0u);
        printf("wrap %d %d\n", spw_poll(&x, 0xfffffff0u + 299, 0), spw_poll(&x, 0xfffffff0u + 300, 0));
    }

    /* the file header */
    memset(hdr, 0, 64); hdr[1] = 1; hdr[7] = 0x20; hdr[5] = 0x01; hdr[10] = 0xbb; hdr[11] = 0x80; hdr[0x14] = 0x7f;   /* pcm = 0x10020 */
    printf("hdr %d", spw_header(hdr, 64 + 0x10020 + 16, &frames, &ch)); printf(" %u %u", frames, ch);
    printf(" size %d", spw_header(hdr, 64 + 0x10020 + 17, &frames, &ch));
    printf(" notrailer %d", spw_header(hdr, 64 + 0x10020, &frames, &ch));
    printf(" short %d", spw_header(hdr, 64 + 0x10020 - 2, &frames, &ch));
    hdr[1] = 2; printf(" stereo flag 2 %d", spw_header(hdr, 64 + 0x10020 + 16, &frames, &ch)); hdr[1] = 1;
    hdr[11] = 0x81; printf(" rate %d", spw_header(hdr, 64 + 0x10020 + 16, &frames, &ch)); hdr[11] = 0x80;
    hdr[40] = 1; printf(" reserved %d", spw_header(hdr, 64 + 0x10020 + 16, &frames, &ch)); hdr[40] = 0;
    hdr[7] = 0x22; printf(" frame %d", spw_header(hdr, 64 + 0x10022 + 16, &frames, &ch)); hdr[7] = 0x20;
    hdr[1] = 0; hdr[7] = 0x22; printf(" mono %d", spw_header(hdr, 64 + 0x10022 + 16, &frames, &ch)); printf(" %u %u\n", frames, ch);

    /* chunks: where to read */
    {
        spw_chunk c;
        spw_chunk_of(2, 5000, 0, &c); printf("chunk %u %u %u %u", c.sector, c.sectors, c.skip, c.frames);
        spw_chunk_of(2, 5000, 2, &c); printf(" %u %u %u %u", c.sector, c.sectors, c.skip, c.frames);
        spw_chunk_of(1, 5000, 1, &c); printf(" %u %u %u %u", c.sector, c.sectors, c.skip, c.frames);
        spw_chunk_of(1, 2048, 0, &c); printf(" %u %u %u %u\n", c.sector, c.sectors, c.skip, c.frames);
    }

    /* reads: for many sizes, the reads taken one after the other cover every chunk once and in order, stay inside the file and inside 128
     * sectors, and each chunk lies whole inside its read, at the file offset it must have */
    {
        static const uint32_t sizes[] = {1, 2047, 2048, 2049, 4096, 5000, 14336, 14337, 16384, 30720, 30721, 61440, 61441, 144000, 480000};
        uint32_t ch, n, bad = 0, reads = 0, pages = 0;
        for (ch = 1; ch <= 2; ch++)
            for (n = 0; n < sizeof sizes / sizeof sizes[0]; n++) {
                uint32_t frames = sizes[n], total = (frames + 2047) / 2048, c = 0, j, file_sectors = (64 + frames * 2 * ch + 16 + 511) / 512;
                while (c < total) {
                    spw_span s;
                    spw_span_of(ch, frames, c, &s);
                    reads++;
                    if (s.chunks < 1 || s.sectors < 1 || s.sectors > 128 || s.sector + s.sectors > file_sectors) bad++;
                    for (j = 0; j < s.chunks; j++, c++) {
                        spw_chunk k;
                        uint32_t at = s.skip + j * 4096 * ch, end;
                        spw_chunk_of(ch, frames, c, &k);
                        end = at + k.frames * 2 * ch;
                        if (end > s.sectors * 512 || s.sector * 512 + at != 64 + c * 4096 * ch) bad++;
                        pages += ch;
                    }
                    if (c < total && s.chunks != (ch == 1 ? 15u : 7u)) bad++;   /* only the last read of a sample may be short */
                }
            }
        printf("spans bad %u reads %u pages %u\n", bad, reads, pages);
    }

    /* page data: stereo is deinterleaved by halfword, mono copied, the rest of every page is zero */
    {
        static uint8_t src[8192], l[4096], r[4096], m[4096];
        uint32_t k, bad = 0;
        for (k = 0; k < 8192; k++) src[k] = (uint8_t)(k * 7 + 3);
        memset(l, 0xaa, 4096); memset(r, 0xbb, 4096); memset(m, 0xcc, 4096);
        spw_page_stereo(src, 1500, l, r);
        for (k = 0; k < 1500; k++) {
            if (l[2 * k] != src[4 * k] || l[2 * k + 1] != src[4 * k + 1] || r[2 * k] != src[4 * k + 2] || r[2 * k + 1] != src[4 * k + 3]) bad++;
        }
        for (k = 3000; k < 4096; k++) if (l[k] || r[k]) bad++;
        spw_page_mono(src, 777, m);
        for (k = 0; k < 1554; k++) if (m[k] != src[k]) bad++;
        for (k = 1554; k < 4096; k++) if (m[k]) bad++;
        printf("pages bad %u\n", bad);
    }
    return 0;
}
"""


def test_wire_protocol(tmp_path):
    src = tmp_path / "wire.c"
    src.write_text(WIRE)
    out = lines(build(tmp_path, [src], "wire"))
    assert (
        out
        == [
            "tag d0100005 255 34567 734",
            # new, repeat, next sequence missed, a gap of two, wrap 255 -> 0, bad prefix, index outside the range
            "rule 0 1 5 5 0 4 4",
            "ack 1 1 5 3 10002 402",
            "bad magic 0",
            "bad version 0",
            "bad status 0",
            "torn done",  # no single changed half passes the checksum (a changed half would print "torn N accepted")
            "snap empty 0",
            "snap 1 5",
            "snap odd 0",
            # the receiver must be armed within 200 ms (300 blocks) of the start
            "wait 0 0",
            "armed timeout -1",
            # the next tag follows the last accepted sequence number (4 -> 5), whatever an earlier load did
            "ready 1 next d0500006",
            "pending 0 0",
            "resend at 176 3",
            "acked 2 base 11 next d0600007",
            # an error with the old error count is not ours; a new one ends the load (tag errors are format errors)
            "stale 0 new -3 other -4",
            "count same 0 other page 0 unarmed 0",
            "deadline 3 3 3 -1 dead -2",
            "wrap 0 -1",
            # pcm 0x10020 bytes stereo = 16392 frames; extra bytes after the PCM, no trailer and reserved bytes pass; a PCM length past the file,
            # flag, rate and partial frame are format errors; mono 0x10022 = 32785
            "hdr 0 16392 2 size 0 notrailer 0 short -5 stereo flag 2 -5 rate -5 reserved 0 frame -5 mono 0 32785 1",
            # chunk (channels, frames, c): stereo 0 reads 17 sectors, stereo 2 starts at sector 32 with the 904 frames left, mono 1 at sector 8
            "chunk 0 17 64 2048 32 8 64 904 8 9 64 2048 0 9 64 2048",
            # 30 sizes: 110 reads (at most 15 mono or 7 stereo chunks each) and 1,293 pages; every chunk once, in order, inside its read
            "spans bad 0 reads 110 pages 1293",
            "pages bad 0",
        ]
    )


@pytest.fixture(scope="module")
def xfer_exe(tmp_path_factory):
    if CC is None:
        pytest.skip("no host C compiler")
    return build(
        tmp_path_factory.mktemp("xfer"),
        [SRC / "cf/dn2/pool.c", SRC / "cf/dn2/xfer.c", SRC / "cf/dn2/send.c", HARNESS],
        "xfer",
        defines=["-DSP_PORT_HOST"],
    )


def scenario(exe, name, *args):
    out = lines(exe, name, *map(str, args))
    # 0xabc + 16: the boot wire length the DSP builds its descriptors with
    assert out[0] == "wire 2764"
    return out[1:]


FREE_ALL = 256 * 2048  # the harness range is 256 pages


def test_transport_stereo_and_mono(xfer_exe):
    # QUEUED, then done OK after 4 pages; first page 0x80531000 = DDR word 0x1014c400, the right plane 2 pages (2,048 words) on;
    # planes in DDR equal the file's L and R and the pad is zero; page order L0 R0 L1 R1 with sequence numbers 1 to 4
    assert scenario(xfer_exe, "stereo") == [
        "rc 1 done 1 0 sends 4",
        "h id=0 frames=3000 ch=2 rate=48000 l=1014c400 r=1014cc00",
        f"planes 3 check 0 free {FREE_ALL - 4 * 2048}",
        "tags d0100000 d0200002 d0300001 d0400003",
    ]
    assert scenario(xfer_exe, "mono") == [
        "rc 1 done 1 0 req 7 sends 3",
        "h id=0 frames=5000 ch=1 rate=48000 l=1014c400 r=1014c400",
        "planes 1",
        "tags d0100000 d0200001 d0300002",
    ]
    # exactly 2 full pages a plane (no pad), and one mono frame (a page that is all pad)
    assert scenario(xfer_exe, "exact") == ["done 2 0 0 sends 5 planes 3 1"]


def test_transport_sharing_and_cache(xfer_exe):
    # a second load of the same file is DONE at once: no page, same planes, the first handle stays valid until both are released
    assert scenario(xfer_exe, "dedup") == [
        "second rc 0 sends 0 same 1 token 1 0",
        "after one release check 0 0",
        f"after two release check -15 free {FREE_ALL}",
    ]
    # a released block stays cached: reselect is DONE without a transfer, a new life (old handle stale), content intact
    assert scenario(xfer_exe, "cache") == [
        "rc 0 sends 0 same 1 tokens differ 1 old -15 new 0 planes 3",
        "still ready 0",
    ]
    # 138 pages do not fit twice; a cached block counts as room; the load evicts it, and a reload of the evicted file transfers again
    assert scenario(xfer_exe, "evict") == [
        f"a 0 free {(256 - 138) * 2048}",
        "b while a is ready: -16",
        f"free with a cached {FREE_ALL}",
        "b rc 1 done 0 planes 3 a stale -15",
        "a again rc 1",
        "a sends 138 planes 3",
    ]
    # 400 pages in five loads: the sequence number wraps and every load still lands right
    assert scenario(xfer_exe, "many") == ["sends 400 bad 0 last seq 144"]


def test_transport_faults(xfer_exe):
    # a lost page is resent after 50 ms with the same tag
    assert scenario(xfer_exe, "resend") == [
        "done 1 0 sends 5 planes 3 tag0 d0100000 tag1 d0100000"
    ]
    # an accepted page whose acknowledgement is late (replies stop for 80 ms with page 2 in flight): the resend is a repeat and is not counted twice
    assert scenario(xfer_exe, "lostack") == ["done 1 0 sends 5 count 4 planes 3"]
    # the receiver rejects a tag: E_FORMAT, the block is freed, the next load works
    assert scenario(xfer_exe, "dsperr") == [f"a -5 free {FREE_ALL}", "b 0 planes 3"]
    # a dead receiver: E_READ, and so is every later load
    assert scenario(xfer_exe, "dead") == ["a -3 sends 0", "next -3"]
    # nothing is acknowledged: the page, three resends, then E_READ at 4 x 75 blocks (the job lasts 301: one block until the first reply);
    # the block is freed
    assert scenario(xfer_exe, "timeout") == [f"a -3 sends 4 job blocks 301 free {FREE_ALL}"]
    # a receiver that never arms: E_READ after 200 ms (300 blocks), nothing sent
    assert scenario(xfer_exe, "unarmed") == ["a -3 sends 0 job blocks 299"]
    # an acknowledgement with a bad checksum is no acknowledgement
    assert scenario(xfer_exe, "garble") == ["a -3 sends 0"]
    # a card read fails in the middle of a load (the second read, after 14 pages): E_READ, nothing in flight, the block is freed
    assert scenario(xfer_exe, "readfail") == [f"a -3 sends 14 free {FREE_ALL}"]
    assert scenario(xfer_exe, "badfile") == ["missing -1 bad -5", f"free {FREE_ALL}"]
    assert scenario(xfer_exe, "tokens") == ["rc -48"]


def test_transport_cancel(xfer_exe):
    # request 2 never started: E_LOST once; request 1 loads normally; request 3 is cancelled with its second page in flight: that page is
    # settled, no third page goes out, the result is E_LOST and the pages are free again (the 4 of request 1 stay)
    assert scenario(xfer_exe, "cancel") == [
        "done 2: 2 -15, 1 0",
        "a planes 3 b blocks 0",
        "c done 3: 3 -15 sends 2",
        f"free {FREE_ALL - 4 * 2048}",
    ]
    # cancelled during the job's first card read: nothing is sent; the next load works
    assert scenario(xfer_exe, "cancelread") == [f"done 1 -15 sends 0 reads 2 free {FREE_ALL}", "b 0 planes 3"]
    # cancelled with page 1 in flight and its reply late: the page is acknowledged first (the receiver counted it), then the load ends
    assert scenario(xfer_exe, "settle") == ["done 1 -15 sends 1 count 1"]


def test_transport_job_and_worker(xfer_exe):
    # one job per load, queued on the worker with the std::function layout, run one at a time; every completion comes from the UI tick
    # (not from the job and not from dkpool_load); the port sees only whole, well-formed words
    assert scenario(xfer_exe, "worker") == [
        "queued 2 runs 2 name 1 layout 1 done 2 outside 0 planes 3 1",
        "bad port 0 stuck 0",
    ]
    # no worker (the getter returns 0): nothing is posted, the load ends with E_READ at the next tick, the block is freed
    assert scenario(xfer_exe, "noworker") == [f"a -3 queued 0 free {FREE_ALL}"]


def test_raw_sender(xfer_exe):
    # a tag d0 12 34 56 is sent as the bytes 56 34 12 d0 (least significant first), each as `byte << 8 | 0x80` and `byte << 8`; a page
    # whose first word is bytes 01 06 0b 10 goes out as 10 0b 06 01; 1,025 words; the model checks every word's command and pairs
    out = scenario(xfer_exe, "wire")
    assert out[0] == "send 0"
    assert out[1].split() == "5680 5600 3480 3400 1280 1200 d080 d000 1080 1000 0b80 0b00 0680 0600 0180 0100".split()
    assert out[2] == "sent tag d0123456 page first 01 06 0b 10 last fc bad 0 words 1025"
    # a port that stops answering ends the send after 8,192 reads of one word (the bound), with an error
    assert out[3] == "stuck send -1 max polls 8192 reads"
    # in a load: the stuck page is lost, counted, sent again after 50 ms, and the load completes
    assert scenario(xfer_exe, "stuck") == ["done 1 0 stuck 1 max polls 8192 sends 4 planes 3"]


def test_card_reads(xfer_exe):
    # stereo 144,000 frames = 71 chunks: 11 reads of 7 chunks (113 sectors; the next read starts at sector 112), the last one 640 frames
    # (6 sectors at 1120); mono 5,000 frames: one read of 20 sectors; mono 60,000 frames = 30 chunks: 15 (121 sectors), then 15 from sector 120
    assert scenario(xfer_exe, "ranges") == [
        "a reads 11 first 0/113 112/113 last 1120/6 planes 3",
        "b reads 1 0/20 planes 1",
        "c reads 2 0/121 120/115 planes 1",
    ]


def rate(exe, ui_period, latency, send_us):
    out = scenario(exe, "slowui", ui_period, latency, send_us)
    assert out[0] == "done 0 pages 142 reads 12 planes 3"
    return 142 / (int(out[1].split()[2]) / 1e6)


def test_page_rate(xfer_exe):
    # The pump runs on the worker and does not wait for the UI pass: the job takes the same time with a UI pass every 5 or 400 blocks
    # (3 ms, 267 ms) as every 50. A page costs its send time plus the wait for the next reply: with the DSP answering one frame (667 us)
    # after a page, about one page per frame; two or three frames of latency give half or a third. Even a send of 890 us (the slow end
    # of the 0.29 to 0.89 ms estimate) with three frames of latency stays above 300 pages/s.
    base = rate(xfer_exe, 50, 1, 300)
    assert 1100 < base < 1300
    assert abs(rate(xfer_exe, 5, 1, 300) - base) < 0.01 * base
    assert abs(rate(xfer_exe, 400, 1, 300) - base) < 0.01 * base
    assert 600 < rate(xfer_exe, 50, 2, 300) < 700
    assert 420 < rate(xfer_exe, 50, 3, 300) < 500
    assert 600 < rate(xfer_exe, 50, 1, 890) < 700   # the send is longer than a frame: the next reply is two frames away
    assert 300 < rate(xfer_exe, 50, 3, 890) < 400
