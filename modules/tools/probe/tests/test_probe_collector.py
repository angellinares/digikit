"""digikit/probe: the collector's logic on the host. The module's own C file is compiled with a fake api, a fake `dl_set_tick_cb` and fake reply records,
and driven through the callback and the three commands. It checks which records add a word, the ring and its overruns, and the frame check of STATUS
(frames, sequence errors for a dropped frame, checksum errors for a dropped or repeated word, the start inside a frame). No firmware, no emulator."""

import shutil
import struct
import subprocess

import pytest
from support.paths import REPO_ROOT

MOD = REPO_ROOT / "modules/tools/probe"

HARNESS = r"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "probe.c"

static void (*the_cb)(const volatile u16 *);
static int32_t (*the_cmd)(u32, u32, const u8 *, u32, u8 *);
void dl_set_tick_cb(uint8_t *cb) { the_cb = (void (*)(const volatile u16 *))cb; }
static u8 *f_alloc(u32 n) { return (u8 *)calloc(1, n); }
static void f_free(u8 *p) { free(p); }
static void f_log(const u8 *m, u32 v) { (void)m; (void)v; }
static int32_t f_reg(u32 first, u32 count, dk_service_cmd *h) { the_cmd = h; printf("reg %u %u\n", first, count); return 0; }

static void setw(u16 *rec, u32 j, u32 v) { u32 h = DIAG_OFF(j) / 2u; rec[h] = (u16)v; rec[h + 1] = (u16)(v >> 16); }

int main(void)
{
    dk_api api;
    char line[4096];
    static u16 rec[0x800];
    memset(&api, 0, sizeof api);
    api.alloc = f_alloc; api.free = f_free; api.log = f_log; api.register_cmds = f_reg;
    printf("init %d\n", dkm_init(&api));
    while (fgets(line, sizeof line, stdin)) {
        unsigned a, b, c, d, e;
        char hex[2048];
        if (sscanf(line, "rec %u %u %u %u %u", &a, &b, &c, &d, &e) == 5) {
            /* beat, master calls, send calls, step, record address */
            setw(rec, 1, a); setw(rec, 26, b); setw(rec, 27, c); setw(rec, 25, d); setw(rec, 29, e);
            the_cb(rec);
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
            printf("fini %d\n", the_cb == NULL);
        }
    }
    return 0;
}
"""


@pytest.fixture(scope="module")
def harness(tmp_path_factory):
    cc = shutil.which("cc")
    if cc is None:
        pytest.skip("no host C compiler")
    d = tmp_path_factory.mktemp("probe")
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


def status(reply: str) -> dict:
    raw = bytes.fromhex(reply.split()[2])
    f = struct.unpack(">16I", raw)
    keys = [
        "format",
        "blocks",
        "records",
        "drops",
        "repeats",
        "idle",
        "words",
        "overruns",
        "cap",
        "parsed",
        "frames_ok",
        "seq_errors",
        "cks_errors",
        "last_seq",
        "last_beat",
        "synced",
    ]
    return dict(zip(keys, f, strict=True))


def fletcher(ws):
    s1 = s2 = 0
    for w in ws:
        s1 = (s1 + w) & 0xFFFFFFFF
        s2 = (s2 + s1) & 0xFFFFFFFF
    return s1 ^ s2


def frame(seq, payload):
    hdr = (0xB0 << 24) | (len(payload) << 16) | (seq & 0xFFFF)
    return [hdr, *payload, fletcher([hdr, *payload])]


def feed(words, *, beat0=100):
    """What the callback sees when a module stores one word on every block: every record carries it, the heartbeat steps by 1, and a module call
    counts in every block."""
    return [f"rec {beat0 + i} {i + 1} 0 {w} 7" for i, w in enumerate(words)]


def test_records_that_carry_a_word_and_the_counters(harness):
    lines = [
        "rec 11 1 0 111 9",  # the first record only sets the baseline
        "rec 12 2 0 222 9",  # one beat later, a new call: word 222
        "rec 13 3 0 333 9",
        "rec 13 3 0 333 9",  # the same record again: a repeat
        "rec 14 3 0 555 9",  # new but no new call: idle
        "rec 18 4 0 666 9",  # a jump of 4: three records missed
        "rec 19 4 1 777 9",  # the send route's counter also counts
        "cmd 50",
        "cmd 51 0000000000" + "05",
    ]
    out = run(harness, lines)
    assert out[0] == "reg 50 3" and out[1] == "init 0"
    s = status(out[2])
    assert (
        s["blocks"],
        s["records"],
        s["drops"],
        s["repeats"],
        s["idle"],
        s["words"],
    ) == (7, 5, 3, 1, 1, 4)
    raw = bytes.fromhex(out[3].split()[2])
    assert struct.unpack(">2I", raw[:8]) == (4, 0)
    assert struct.unpack(">4I", raw[8:]) == (222, 333, 666, 777)


def test_the_ring_stops_when_full_and_counts_overruns_and_reset_empties_it(harness):
    cap = 24576
    lines = [
        *feed(range(1, cap + 6), beat0=1),  # the first word only sets the baseline
        "cmd 50",
        "cmd 52",
        "cmd 50",
    ]
    out = run(harness, lines)
    s = status(out[2])
    assert s["words"] == cap and s["overruns"] == 4 and s["cap"] == cap
    assert out[3].startswith("reply 0")
    s = status(out[4])
    assert s["words"] == 0 and s["overruns"] == 0 and s["blocks"] == 0


def test_frames_are_checked_and_errors_are_counted(harness):
    f = [frame(n, [n * 7 + k for k in range(12)]) for n in range(10)]
    stream = [
        0x1234,
        0xB00C0000,
        0x55,
    ]  # a partial frame first: no error is counted for it
    for fr in f[:4]:
        stream += fr
    stream += f[5]  # frame 4 missing: a sequence error
    bad = list(f[6])
    del bad[5]  # a dropped word: the frame fails its checksum
    stream += bad
    stream += f[7]
    rep = list(f[8])
    rep.insert(3, rep[3])  # a repeated word
    stream += rep
    stream += f[9]
    stream += f[9][:6]  # an unfinished frame at the end waits
    lines = [*feed(stream, beat0=1), "cmd 50"]
    s = status(run(harness, lines)[2])
    assert s["frames_ok"] == 4 + 1 + 1 + 1  # 0-3, 5, 7, 9
    assert s["seq_errors"] >= 1
    assert s["cks_errors"] == 2
    assert (
        s["parsed"] == len(stream) - 1 - 6
    )  # the first word is the baseline; the unfinished frame is left for later
    # a clean stream: no error of either kind
    clean = [0x77] + [
        w for n in range(20) for w in frame(n, [n] * 12)
    ]  # 0x77 is the baseline
    s = status(run(harness, [*feed(clean, beat0=1), "cmd 50"])[2])
    assert (s["frames_ok"], s["seq_errors"], s["cks_errors"]) == (20, 0, 0)


def test_read_limits_and_fini_clears_the_callback(harness):
    lines = [
        *feed([1, 2, 3, 4], beat0=1),
        "cmd 51 00000001" + "0002",
        "cmd 51 00000009" + "0002",
        "cmd 51 00",
        "cmd 51 00000000" + "ffff",
        "fini",
    ]
    out = run(harness, lines)
    assert struct.unpack(">4I", bytes.fromhex(out[2].split()[2])) == (3, 1, 3, 4)
    assert struct.unpack(">2I", bytes.fromhex(out[3].split()[2])) == (
        3,
        9,
    )  # past the end: no words
    assert out[4] == "reply -50 "  # a body of the wrong length
    assert (
        len(bytes.fromhex(out[5].split()[2])) == 8 + 4 * 3
    )  # a count above what the ring holds is cut
    assert out[6] == "fini 1"
