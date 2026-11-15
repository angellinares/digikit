"""digikit/debug: the module's logic on the host. The C file is compiled with fakes of the runtime imports (sink, clock, peek ranges, track types,
`dk_log_event`) and driven through the sink and the three commands. It checks the level filter (default and per module, errors always kept), the ring and its
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
static unsigned int fake_clk;
#define DEBUG_READ32(a) (fake_clk)
#include "debug.c"

static void (*the_span)(const dk_span *);
static uint32_t (*the_span_clock)(void);
static int span_busy, span_noclock;
int32_t dk_span_sink_set(uint8_t *emit, uint8_t *clock)
{
    if (span_busy && emit) return DK_E_BUSY;
    the_span = (void (*)(const dk_span *))emit;
    the_span_clock = (uint32_t (*)(void))clock;
    return 0;
}
static uint32_t poll_owner[4], poll_fn[4];
uint32_t dk_debug_poll(uint32_t slot, uint32_t *fn) { if (slot > 3 || !poll_owner[slot]) return 0; if (fn) *fn = poll_fn[slot]; return poll_owner[slot]; }
uint32_t dk_debug_span_clock(uint32_t *hz) { *hz = 150000000u; return span_noclock ? 0u : 0x1000u; }

static void (*the_sink)(const Ev *);
static void (*the_tick)(void);
static dk_host_cmd_fn *the_cmd;
static dk_host_cmd_fn *the_profile;
static u32 clock_blocks;
static int32_t track_ty[16];
static int project = 1;
static int32_t sel_track;
static int16_t sel_words[101];
static u32 ranges[2][2];
static u32 n_ranges;

void dk_debug_log_sink_set(uint8_t *s, uint8_t *t) { the_sink = (void (*)(const Ev *))s; the_tick = (void (*)(void))t; }
uint32_t dk_time_blocks(void) { return clock_blocks; }
uint32_t dk_debug_peek(uint32_t i, uint32_t *out) { if (i < n_ranges) { out[0] = ranges[i][0]; out[1] = ranges[i][1]; } return n_ranges; }
int32_t dk_debug_track_type(uint32_t t) { return !project ? -2 : t < 16 ? track_ty[t] : -1; }
int32_t dk_debug_selected_params(int16_t *out, uint32_t n)
{
    if (!project) return -2;
    if (sel_track < 0) return -3;
    for (uint32_t i = 0; i < n; i++) out[i] = sel_words[i];
    return sel_track;
}
void dk_log_event(uint32_t kind, uint32_t flags, const uint32_t *a)
{
    Ev e = { 0, clock_blocks, (u16)kind, (u16)flags, { a[0], a[1], a[2], a[3], a[4] } };
    the_sink(&e);
}
void dk_log(uint32_t level, const char *m, uint32_t v) { (void)level; (void)m; (void)v; }
/* the services of the card log: linked, not run (the open calls fail, so the card log is off) */
int32_t dk_files_open(dk_module_ctx *c, dk_files **o) { (void)c; (void)o; return DK_E_NOT_FOUND; }
int32_t dk_settings_open(dk_module_ctx *c, dk_settings **o) { (void)c; (void)o; return DK_E_NOT_FOUND; }
int32_t dk_settings_get(dk_settings *s, const char *k, void *o, uint32_t cap) { (void)s; (void)k; (void)o; (void)cap; return DK_E_SET_OFF; }
int32_t dk_settings_put(dk_settings *s, const char *k, const void *v, uint32_t n) { (void)s; (void)k; (void)v; (void)n; return DK_E_SET_OFF; }
int32_t dk_io_post(dk_io_step_fn *s, dk_io_done_fn *d, void *a) { (void)s; (void)d; (void)a; return DK_E_BUSY; }
int32_t dk_io_cancel(int32_t id) { (void)id; return 0; }
void dk_io_wake(int32_t id) { (void)id; }
int32_t dk_files_create(dk_files *f, uint32_t r, const char *p, uint32_t n, const void *h, uint32_t fl) { (void)f; (void)r; (void)p; (void)n; (void)h; (void)fl; return DK_E_STATE; }
int32_t dk_files_write(dk_files *f, int32_t h, uint32_t o, const void *b, uint32_t n) { (void)f; (void)h; (void)o; (void)b; (void)n; return DK_E_STATE; }
int32_t dk_files_close(dk_files *f, int32_t h, void *o) { (void)f; (void)h; (void)o; return DK_E_STATE; }
int32_t dk_files_abort(dk_files *f, int32_t h) { (void)f; (void)h; return DK_E_STATE; }
void dk_ui_menu_add_toggle(dk_ui_menu *m, const char *t, int32_t *v, dk_ui_action_fn *f, void *a) { (void)m; (void)t; (void)v; (void)f; (void)a; }
void dk_ui_menu_add_text(dk_ui_menu *m, const char *t) { (void)m; (void)t; }
uint32_t dk_text_itoa(char *o, int32_t v) { (void)o; (void)v; return 0; }
uint32_t dk_text_utoa(char *o, uint32_t v) { (void)o; (void)v; return 0; }
static unsigned reg_first, reg_count;
int32_t dk_host_register(uint32_t first, uint32_t count, dk_host_cmd_fn *h)
{
    /* the first range is the log commands, the second the profiling commands; both print as one line */
    if (!the_cmd) { the_cmd = h; reg_first = first; reg_count = count; return 0; }
    the_profile = h;
    printf("reg %u %u %u %u\n", reg_first, reg_count, first, count);
    return 0;
}

int main(int argc, char **argv)
{
    char line[4096];
    for (int k = 1; k < argc; k++) {
        if (!strcmp(argv[k], "noclock")) span_noclock = 1;
        if (!strcmp(argv[k], "busy")) span_busy = 1;
    }
    printf("init %d %d\n", (int)dk_module.init(NULL), the_sink != NULL);
    while (fgets(line, sizeof line, stdin)) {
        unsigned a, b, c, d;
        char hex[2048];
        if (sscanf(line, "ev %u %x %u", &a, &b, &c) == 3) {
            /* kind, flags, first argument */
            Ev e = { 0, clock_blocks, (u16)a, (u16)b, { c, 0, 0, 0, 0 } };
            the_sink(&e);
        } else if (sscanf(line, "clock %u", &a) == 1) {
            clock_blocks = a;
        } else if (sscanf(line, "span %u %u %u", &a, &b, &c) == 3) {
            /* owner, ticks, number of spans */
            dk_span s = { a, b };
            while (c--) the_span(&s);
        } else if (sscanf(line, "setclk %u", &a) == 1) {
            fake_clk = a;
        } else if (!strncmp(line, "readclk", 7)) {
            printf("clk %u\n", the_span_clock());
        } else if (!strncmp(line, "sinkstat", 8)) {
            printf("sink %d %d\n", the_span != NULL, the_span_clock != NULL);
        } else if (sscanf(line, "range %u %u", &a, &b) == 2) {
            ranges[n_ranges][0] = a; ranges[n_ranges][1] = b; n_ranges++;
        } else if (sscanf(line, "peekmem %u", &a) == 1) {
            ranges[n_ranges][0] = 4096; ranges[n_ranges][1] = 4096 + sizeof peek_mem; n_ranges++;
            for (b = 0; b < sizeof peek_mem; b++) peek_mem[b] = (u8)(a + b);
        } else if (sscanf(line, "track %u %d", &a, &c) == 2) {
            track_ty[a] = (int32_t)c;
        } else if (sscanf(line, "sel %d", &c) == 1) {
            sel_track = (int32_t)c;
        } else if (sscanf(line, "word %u %d", &a, &c) == 2) {
            sel_words[a] = (int16_t)c;
        } else if (sscanf(line, "poll %u %u %x", &a, &b, &c) == 3) {
            poll_owner[a] = b;
            poll_fn[a] = (uint32_t)c;
        } else if (!strncmp(line, "noproject", 9)) {
            project = 0;
        } else if (!strncmp(line, "project", 7)) {
            project = 1;
        } else if (!strncmp(line, "modtick", 7)) {
            dk_module.tick(NULL);
        } else if (!strncmp(line, "slot", 4)) {
            the_tick();
        } else if (sscanf(line, "cmd %u %2047s", &a, hex) == 2 || sscanf(line, "cmd %u", &a) == 1) {
            u8 body[200], out[1024];
            u32 len = 0, i;
            if (sscanf(line, "cmd %u %2047s", &a, hex) != 2) hex[0] = 0;
            for (i = 0; hex[2 * i] && hex[2 * i + 1]; i++) { unsigned v; sscanf(hex + 2 * i, "%2x", &v); body[len++] = (u8)v; }
            int32_t r = (a >= 0x1eu && a <= 0x1fu ? the_profile : the_cmd)(a, 0, body, len, out);
            printf("reply %d ", r);
            for (i = 0; r > 0 && i < (u32)r; i++) printf("%02x", out[i]);
            printf("\n");
        } else if (!strncmp(line, "fini", 4)) {
            dk_module.fini(NULL);
            printf("fini %d %d\n", the_sink == NULL && the_tick == NULL, the_span == NULL && the_span_clock == NULL);
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


def run(exe, lines, args=()):
    done = subprocess.run(
        [str(exe), *args],
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
    assert out == ["reg 32 3 30 2", "init 0 1", "fini 1 1"]


ROWS = 8 + 3 + 4  # module records (limits.max modules), service phases, polls
PHASE = 0x100
POLL = 0x200


def profile_state(reply: str):
    raw = bytes.fromhex(reply.split()[2])
    assert len(raw) == 35
    return struct.unpack(">BIH", raw[:7])


def profile_elapsed(reply: str):
    return struct.unpack(">Q", bytes.fromhex(reply.split()[2])[7:15])[0]


def profile_rows(reply: str):
    raw = bytes.fromhex(reply.split()[2])
    first, n = struct.unpack(">HB", raw[:3])
    assert len(raw) == 3 + 16 * n
    rows = [struct.unpack(">IQI", raw[3 + 16 * i : 19 + 16 * i]) for i in range(n)]
    return first, rows


def test_profile_start_sets_the_span_sink_and_stop_clears_it(harness):
    out = run(
        harness,
        ["sinkstat", "cmd 30 00", "cmd 30 01", "sinkstat", "cmd 30 02", "sinkstat"],
    )
    assert out[2] == "sink 0 0"
    assert profile_state(out[3]) == (0, 150000000, ROWS)
    assert profile_state(out[4]) == (1, 150000000, ROWS)
    assert out[5] == "sink 1 1"
    assert profile_state(out[6]) == (0, 150000000, ROWS)
    assert out[7] == "sink 0 0"


def test_profile_state_names_the_poll_owners(harness):
    out = run(harness, ["poll 0 3 12345678", "poll 2 255 4000", "cmd 30 00"])
    raw = bytes.fromhex(out[2].split()[2])
    assert len(raw) == 35
    assert struct.unpack(">BI", raw[15:20]) == (3, 0x12345678)
    assert struct.unpack(">BI", raw[20:25]) == (0, 0), "an empty slot"
    assert struct.unpack(">BI", raw[25:30]) == (255, 0x4000)
    assert raw[30:35] == bytes(5)


def test_profile_span_clock_reads_the_counter_and_fini_clears_the_sink(harness):
    out = run(harness, ["cmd 30 01", "setclk 12345", "readclk", "fini"])
    assert out[3] == "clk 12345"
    assert out[4] == "fini 1 1"


def test_profile_start_without_a_clock_is_refused(harness):
    out = run(harness, ["cmd 30 01", "cmd 30 00", "sinkstat"], args=["noclock"])
    assert out[2].split()[1] == "-60"
    assert profile_state(out[3])[0] == 0
    assert out[4] == "sink 0 0"


def test_profile_start_gets_the_runtime_busy_code(harness):
    out = run(harness, ["cmd 30 01", "cmd 30 00"], args=["busy"])
    assert out[2].split()[1] == "-12"
    assert profile_state(out[3])[0] == 0


def test_profile_bad_op_and_body(harness):
    out = run(harness, ["cmd 30 04", "cmd 30", "cmd 31 00", "cmd 31 00000000"])
    assert [o.split()[1] for o in out[2:]] == ["-60", "-50", "-50", "-50"]


def test_profile_spans_add_up_per_owner_and_unknown_owners_are_ignored(harness):
    out = run(
        harness,
        [
            "cmd 30 01",
            "span 3 100 1",
            "span 3 40 1",
            "span 3 70 2",
            f"span {PHASE + 1} 9 3",
            f"span {POLL + 2} 5 1",
            f"span {POLL + 4} 99 1",  # no such poll
            f"span {PHASE + 3} 99 1",  # no such phase
            "span 8 99 1",  # between the modules and the phases
            "span 4096 99 1",
            "cmd 31 0000",
        ],
    )
    first, rows = profile_rows(out[3])
    assert first == 0 and len(rows) == ROWS
    assert rows[3] == (4, 280, 100)
    assert rows[8 + 1] == (3, 27, 9)
    assert rows[8 + 3 + 2] == (1, 5, 5)
    others = [r for i, r in enumerate(rows) if i not in (3, 9, 13)]
    assert all(r == (0, 0, 0) for r in others)


def test_profile_sum_is_64_bits(harness):
    big = 0xF0000000
    out = run(harness, ["cmd 30 01", f"span 0 {big} 3", "cmd 31 0000"])
    _, rows = profile_rows(out[3])
    assert rows[0] == (3, 3 * big, big)
    assert rows[0][1] > 2**32


def test_profile_reset_zeroes_the_table_in_both_states(harness):
    out = run(
        harness,
        [
            "cmd 30 01",
            "span 1 7 2",
            "cmd 30 03",  # running
            "cmd 31 0000",
            "cmd 30 02",
            "cmd 30 03",  # stopped
            "cmd 31 0000",
        ],
    )
    assert all(r == (0, 0, 0) for r in profile_rows(out[4])[1])
    assert profile_state(out[3])[0] == 1
    assert all(r == (0, 0, 0) for r in profile_rows(out[-1])[1])


def test_profile_read_pages_and_refuses_a_first_row_past_the_end(harness):
    out = run(
        harness,
        [
            "cmd 30 01",
            "span 5 11 1",
            "span 9999 1 1",
            "cmd 31 0004",
            "cmd 31 000e",
            f"cmd 31 {ROWS:04x}",
            f"cmd 31 {ROWS + 1:04x}",
        ],
    )
    first, rows = profile_rows(out[3])
    assert first == 4 and len(rows) == ROWS - 4 and rows[1] == (1, 11, 11)
    first, rows = profile_rows(out[4])
    assert first == ROWS - 1 and len(rows) == 1
    assert out[5].split()[1] == "-63" and out[6].split()[1] == "-63"


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
    # the param watch baseline (track 0 is selected, info level) comes at the first pass after the track watch started; the machine change follows
    assert [(e[2], e[4], e[5], e[6]) for e in evs] == [
        (42, 0, 0xFFFFFFFF, 0),
        (26, 2, 5, 7),
    ]
    assert [(e[3] >> 1) & 7 for e in evs] == [2, 2]  # info


def test_param_watch_logs_a_changed_word_and_takes_the_snapshot_again_on_a_new_track(
    harness,
):
    lines = [
        "cmd 34 0103ffffffffffffffff",  # default level debug
        "project",
        "sel 3",
        "word 5 -100",
        "clock 0",
        "slot",  # the first pass: starts the timer
        "clock 3030",
        "slot",  # the track watch starts
        "slot",  # the baseline of the words: one baseline event
        "word 5 32512",
        "word 70 1",
        "slot",  # two changes
        "slot",  # no change
        "sel 4",
        "word 5 7",
        "slot",  # another track: the snapshot is taken again (a baseline event)
        "sel -1",
        "word 5 8",
        "slot",  # no record (a kit is loading): nothing
        "sel 4",
        "slot",  # the record is back: the snapshot again (a baseline event)
        "word 5 9",
        "slot",
        "cmd 32 00000000",
    ]
    out = run(harness, lines)
    _, _, evs = log(out[-1])
    changes = [(e[4], e[5], e[6], e[7], e[8]) for e in evs if e[2] == 42]
    B = 0xFFFFFFFF  # a baseline: the snapshot was taken (first pass, new track, record back)
    assert changes == [
        (3, B, 0, 0, 0),
        (3, 5, 0xFFFFFF9C, 32512, 0),
        (3, 70, 0, 1, 0),
        (4, B, 0, 0, 0),
        (4, B, 0, 0, 0),
        (4, 5, 8, 9, 0),
    ]


def test_profile_elapsed_counts_clock_ticks_across_wraps_while_running(harness):
    out = run(
        harness,
        [
            "setclk 4000000000",
            "cmd 30 01",
            "cmd 30 00",  # 0
            "setclk 4000000100",
            "cmd 30 00",  # 100
            "setclk 3000000000",  # wrapped, then 3000000000 - 4000000100 mod 2^32
            "modtick",
            "setclk 3000000010",
            "cmd 30 02",  # stop reads the clock once more
            "setclk 5",
            "cmd 30 00",  # stopped: frozen
            "cmd 30 03",  # reset zeroes it
        ],
    )
    wrapped = (3000000000 - 4000000100) % 2**32
    assert [profile_elapsed(out[i]) for i in (3, 4)] == [0, 100]
    assert profile_elapsed(out[5]) == 100 + wrapped + 10
    assert profile_elapsed(out[6]) == 100 + wrapped + 10
    assert profile_elapsed(out[7]) == 0
