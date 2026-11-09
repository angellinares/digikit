"""digikit/debug: the module's logic on the host. The C file is compiled with a fake api and fake runtime imports (sink, clock, peek ranges, track types,
`dkrt_log_ev`) and driven through the sink and the three commands. It checks the level filter (default and per module, errors always kept), the ring and its
paging, PEEK's range rules, LOGCFG, the track watch and what fini clears. No firmware, no emulator."""

import shutil
import struct
import subprocess

import pytest
from support.paths import REPO_ROOT

MOD = REPO_ROOT / "modules/tools/debug"

HARNESS = r"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static unsigned char peek_mem[64];
#define DEBUG_READ8(a) (peek_mem[(a) - 4096u])
#include "debug.c"

static void (*the_sink)(const Ev *);
static void (*the_tick)(void);
static int32_t (*the_cmd)(u32, u32, const u8 *, u32, u8 *);
static u32 clock_blocks;
static int32_t track_ty[16];
static int project = 1;
static u32 ranges[2][2];
static u32 n_ranges;

void dkrt_log_sink_set(uint8_t *s, uint8_t *t) { the_sink = (void (*)(const Ev *))s; the_tick = (void (*)(void))t; }
uint32_t dkrt_blocks(void) { return clock_blocks; }
uint32_t dkrt_peek_range(uint32_t i, uint32_t *out) { if (i < n_ranges) { out[0] = ranges[i][0]; out[1] = ranges[i][1]; } return n_ranges; }
int32_t dkrt_track_type(uint32_t t) { return !project ? -2 : t < 16 ? track_ty[t] : -1; }
void dkrt_log_ev(uint32_t kind, uint32_t flags, const uint32_t *a)
{
    Ev e = { 0, clock_blocks, (u16)kind, (u16)flags, { a[0], a[1], a[2], a[3], a[4] } };
    the_sink(&e);
}
static void f_log(const u8 *m, u32 v) { (void)m; (void)v; }
static int32_t f_reg(u32 first, u32 count, dk_service_cmd *h) { the_cmd = h; printf("reg %u %u\n", first, count); return 0; }

int main(void)
{
    dk_api api;
    char line[4096];
    memset(&api, 0, sizeof api);
    api.log = f_log; api.register_cmds = f_reg;
    printf("init %d %d\n", dkm_init(&api), the_sink != NULL);
    while (fgets(line, sizeof line, stdin)) {
        unsigned a, b, c, d;
        char hex[2048];
        if (sscanf(line, "ev %u %x %u", &a, &b, &c) == 3) {
            /* kind, flags, first argument */
            Ev e = { 0, clock_blocks, (u16)a, (u16)b, { c, 0, 0, 0, 0 } };
            the_sink(&e);
        } else if (sscanf(line, "clock %u", &a) == 1) {
            clock_blocks = a;
        } else if (sscanf(line, "range %u %u", &a, &b) == 2) {
            ranges[n_ranges][0] = a; ranges[n_ranges][1] = b; n_ranges++;
        } else if (sscanf(line, "peekmem %u", &a) == 1) {
            ranges[n_ranges][0] = 4096; ranges[n_ranges][1] = 4096 + sizeof peek_mem; n_ranges++;
            for (b = 0; b < sizeof peek_mem; b++) peek_mem[b] = (u8)(a + b);
        } else if (sscanf(line, "track %u %d", &a, &c) == 2) {
            track_ty[a] = (int32_t)c;
        } else if (!strncmp(line, "noproject", 9)) {
            project = 0;
        } else if (!strncmp(line, "project", 7)) {
            project = 1;
        } else if (!strncmp(line, "slot", 4)) {
            the_tick();
        } else if (sscanf(line, "cmd %u %2047s", &a, hex) == 2 || sscanf(line, "cmd %u", &a) == 1) {
            u8 body[200], out[1024];
            u32 len = 0, i;
            if (sscanf(line, "cmd %u %2047s", &a, hex) != 2) hex[0] = 0;
            for (i = 0; hex[2 * i] && hex[2 * i + 1]; i++) { unsigned v; sscanf(hex + 2 * i, "%2x", &v); body[len++] = (u8)v; }
            int32_t r = the_cmd(a, 0, body, len, out);
            printf("reply %d ", r);
            for (i = 0; r > 0 && i < (u32)r; i++) printf("%02x", out[i]);
            printf("\n");
        } else if (!strncmp(line, "fini", 4)) {
            dkm_fini(&api);
            printf("fini %d\n", the_sink == NULL && the_tick == NULL);
        }
        (void)d;
    }
    return 0;
}
"""

ERR = 0x0001
INFO = 2 << 1
DEBUG = 3 << 1
TRACE = 4 << 1
WARN = 1 << 1


@pytest.fixture(scope="module")
def harness(tmp_path_factory):
    cc = shutil.which("cc")
    if cc is None:
        pytest.skip("no host C compiler")
    d = tmp_path_factory.mktemp("debug")
    src = d / "h.c"
    src.write_text(HARNESS)
    exe = d / "h"
    subprocess.run(
        [
            cc,
            "-std=c11",
            "-O1",
            "-I",
            str(MOD / "src/c"),
            "-I",
            str(REPO_ROOT / "sdk/include"),
            str(src),
            "-o",
            str(exe),
        ],
        check=True,
        capture_output=True,
    )
    return exe


def run(exe, lines):
    done = subprocess.run(
        [str(exe)],
        input="\n".join(lines) + "\n",
        capture_output=True,
        text=True,
        timeout=60,
        check=True,
    )
    return done.stdout.splitlines()


def log(reply: str):
    raw = bytes.fromhex(reply.split()[2])
    oldest, newest, n = struct.unpack(">IIB", raw[:9])
    evs = [
        struct.unpack(">IIHH5I", raw[9 + 32 * i : 9 + 32 * (i + 1)]) for i in range(n)
    ]
    return oldest, newest, evs


def ev(kind, flags, a=0):
    return f"ev {kind} {flags:x} {a}"


def test_init_registers_the_range_and_the_sink_and_fini_clears_it(harness):
    out = run(harness, ["fini"])
    assert out == ["reg 32 3", "init 0 1", "fini 1"]


def test_events_are_numbered_from_one_and_read_back_in_order(harness):
    out = run(
        harness, ["clock 77", ev(5, INFO, 10), ev(6, INFO, 11), "cmd 32 00000000"]
    )
    oldest, newest, evs = log(out[2])
    assert (oldest, newest) == (1, 2)
    assert [(e[0], e[1], e[2], e[3] & 1, e[4]) for e in evs] == [
        (1, 77, 5, 0, 10),
        (2, 77, 6, 0, 11),
    ]
    # from a later sequence number
    _, _, evs = log(run(harness, [ev(5, INFO), ev(6, INFO), "cmd 32 00000002"])[2])
    assert [e[0] for e in evs] == [2]
    # nothing logged yet
    assert log(run(harness, ["cmd 32 00000000"])[2])[:2] == (0, 0)


def test_level_filter_default_and_per_module_and_errors_always_pass(harness):
    src2 = 2 << 8
    lines = [
        ev(1, DEBUG, 1),  # above the default level (info): dropped
        ev(2, INFO, 2),
        ev(3, ERR, 3),
        ev(4, DEBUG | src2, 4),  # module record 1: no level of its own, default
        "cmd 34 0100ff03ffffffffffff",  # default 0 (errors only), record 1: debug
        ev(5, INFO, 5),  # dropped now
        ev(6, DEBUG | src2, 6),  # kept: its own level is debug
        ev(7, TRACE | src2, 7),  # above it
        ev(8, ERR, 8),  # an error passes at level 0
        "cmd 32 00000000",
        "cmd 34 00",
    ]
    out = run(harness, lines)
    _, _, evs = log(out[-2])
    assert [e[4] for e in evs] == [2, 3, 6, 8]
    r = bytes.fromhex(out[-1].split()[2])
    assert r == bytes(
        [0, 0xFF, 3, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 1]
    )  # the last byte: the module is loaded


def test_logcfg_refuses_bad_input_and_has_no_debug_switch(harness):
    out = run(
        harness,
        [
            "cmd 34",  # no op
            "cmd 34 02",  # the old debug switch is gone
            "cmd 34 01ff" + "ff" * 8,  # level above 4
            "cmd 34 0102" + "05" + "ff" * 7,  # a module level above 4
            "cmd 34 01" + "02",  # too short
            "cmd 34 00",
        ],
    )
    assert [o.split()[1] for o in out[2:7]] == ["-50", "-60", "-60", "-60", "-50"]
    assert out[7].startswith("reply 10 02ffffffffffffffff01")


def test_the_ring_keeps_512_and_pages_27_events_a_reply(harness):
    lines = [ev(9, INFO, i) for i in range(1, 601)] + [
        "cmd 32 00000000",
        "cmd 32 00000100",
        "cmd 32 000001e0",
    ]
    out = run(harness, lines)
    oldest, newest, evs = log(out[-3])
    assert (oldest, newest) == (89, 600) and len(evs) == 27 and evs[0][0] == 89
    assert [e[0] for e in log(out[-2])[2]][:2] == [
        256,
        257,
    ]  # one reply of 27 from 0x100
    assert [e[0] for e in log(out[-1])[2]][-1] == 506


def test_peek_reads_inside_the_ranges_only(harness):
    out = run(
        harness,
        [
            "peekmem 10",
            "cmd 33 000010000004",
            "cmd 33 0000103c0004",
            "cmd 33 0000103d0004",
            "cmd 33 00000fff0001",
        ],
    )
    assert out[2].split()[1:] == ["10", "000010000004" + "0a0b0c0d"]
    assert out[3].split()[2][12:] == "46474849"  # bytes 60 to 63: the end of the range
    assert out[4].startswith("reply -63")  # one byte past the end
    assert out[5].startswith("reply -63")  # one byte below


def test_peek_refusals(harness):
    out = run(
        harness,
        [
            "range 4096 8192",
            "cmd 33 00000fff0001",
            "cmd 33 00001ffe0004",
            "cmd 33 000010000000",
            "cmd 33 000010000201",
            "cmd 33 ffffffff0002",
            "cmd 33 00",
        ],
    )
    assert [o.split()[1] for o in out[2:]] == ["-63", "-63", "-63", "-63", "-63", "-50"]


def test_track_watch_logs_a_change_after_the_baseline_and_waits_for_the_project(
    harness,
):
    lines = [
        "noproject",
        "clock 0",
        "slot",  # the first pass: starts the timer
        "clock 3000",
        "slot",  # no project yet: nothing
        "project",
        "track 2 5",
        "clock 3030",
        "slot",  # the baseline: no event
        "track 2 7",
        "clock 3040",
        "slot",  # not due yet
        "clock 3060",
        "slot",  # the change
        "cmd 32 00000000",
    ]
    out = run(harness, lines)
    _, _, evs = log(out[-1])
    assert [(e[2], e[4], e[5], e[6]) for e in evs] == [(26, 2, 5, 7)]
    assert (evs[0][3] >> 1) & 7 == 2  # info
