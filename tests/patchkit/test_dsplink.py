"""core/dsp-link (docs/findings/31-dsp-link.md): static checks of the module against the two stock images, and the
ColdFire half (dl_send, dl_poll, dl_tick) on the native ColdFire core. The DSP half and the whole channel run in the
coupled emulator: `uv run python -m tools.patchkit.pk_dsplink proof` (tools/patchkit/pk_dsplink.py).

Skips what needs a stock firmware or the m68k toolchain when they are absent."""

import re
import struct
import tomllib

import pytest
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import pk_device, pk_toolchains

ROOT = REPO_ROOT
MOD = ROOT / "modules" / "core" / "adapters" / "dsp-link"
IMAGES = {
    "dt2-1.17": "Digitakt_II_OS1.17.syx",
    "dn2-1.12": "Digitone_II_OS1.12.syx",
}


# the DSP's frame-tail geometry per device is the device file's ([digikit] dsp_tail_off, dsp_tail_words; `patchkit gen` writes it into
# build/gen/device-<dev>.inc); the firmware's call site (finding 23, S4b) is checked against it below
def geometry(dev: str) -> tuple[int, int]:
    d = pk_device.load(dev)["digikit"]
    return d["dsp_tail_off"], d["dsp_tail_words"]


def gen_inc(dev: str) -> str:
    return (MOD / "build" / "gen" / f"device-{dev}.inc").read_text()


MAGIC, ACK_MAGIC, ACK_OFF = 0xD15C, 0xA5D1, 0x60
BASE = 0x47E8B000
DATA = 0x47EC0000
needs_gcc = pytest.mark.skipif(
    pk_toolchains.m68k_bin("gcc") is None, reason="m68k toolchain not installed"
)


def define(text: str, name: str) -> int:
    m = re.search(rf"#define\s+{name}\s+(0x[0-9A-Fa-f]+|\d+)u?", text)
    assert m, name
    return int(m.group(1), 0)


def port(name: str) -> dict:
    return tomllib.loads((ROOT / "devices" / "ports" / f"{name}.toml").read_text())


# The anchors core/dsp-link links against (devices/fw/link.toml and devices/fw/dsp/link.toml).
DSPLINK_ANCHORS = (
    "cf.link.site.block_isr.frame_call",
    "cf.link.fn.frame",
    "cf.link.site.reply_copy_lit",
    "cf.link.site.tx_queue_lit",
    "dsp.link.site.block_tail",
)


def anchors(name: str) -> dict:
    return {a["name"]: a for a in port(name)["anchor"] if a["name"] in DSPLINK_ANCHORS}


def main_os(image: str) -> bytes:
    from tools.patchkit import port_resolve

    return port_resolve.sections_of(config.stock(IMAGES[image]))[3]


def test_objects_are_current():
    front_end.build_module(MOD, check=True)


def test_constants_agree_between_the_two_sides():
    c = (MOD / "src/cf/dsp-link.c").read_text()
    s = (MOD / "src/dsp/dsp-link.inc").read_text()
    for name, want in (
        ("DL_MAGIC", MAGIC),
        ("DL_ACK_MAGIC", ACK_MAGIC),
        ("DL_ACK_OFF", ACK_OFF),
    ):
        assert define(c, name) == want == define(s, name)
    # the area of each device: the DL_AREA_BYTES define, the .PKBSS literal and the word count agree, and the 17-bit offset reaches it
    for dev in pk_device.digikit_devices():
        d = gen_inc(dev)
        area = pk_device.load(dev)["digikit"]["dsp_area_bytes"]
        assert define(d, "DL_AREA_BYTES") == area
        assert define(d, "DL_AREA_WORDS") * 4 == define(d, "DL_AREA_BYTES")
        # the ColdFire side checks against the contract's maximum (generated from sdk.toml), which a device's area does not pass
        h = (MOD / "build/gen/dl-limits.h").read_text()
        assert (
            define(h, "DL_AREA_MAX_BYTES")
            == pk_device.contract_max()["dsp_area_bytes"]
            >= area
        )
        assert define(h, "DL_AREA_MAX_WORDS") * 4 == define(h, "DL_AREA_MAX_BYTES")
        # the DSP source sizes its bss by the generated define
        assert re.search(
            r"\.PKBSS _dl_area, DL_AREA_BYTES;",
            (MOD / "src" / "dsp" / f"{dev}.s").read_text(),
        )
        assert define(d, "DL_AREA_BYTES") <= define(h, "DL_AREA_MAX_BYTES") <= 4 << 17
        # the C file takes its width from the header, not from a literal
        assert re.search(r"#define DL_AREA_BYTES DL_AREA_MAX_BYTES", c)


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_anchors_are_in_the_port(image):
    a = anchors(image)
    assert set(a) == {
        "cf.link.site.block_isr.frame_call",
        "cf.link.fn.frame",
        "cf.link.site.reply_copy_lit",
        "cf.link.site.tx_queue_lit",
        "dsp.link.site.block_tail",
    }


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_call_site_geometry_matches_the_dsp_constants(image):
    if not config.stock(IMAGES[image]).exists():
        pytest.skip("firmware not present")
    dev = image[:3]
    a = anchors(image)
    main = main_os(image)
    base = 0x40000400
    call = a["cf.link.site.block_isr.frame_call"]["addr"] - base
    # `jsr abs.l` to the stock driver, preceded by the four pushes: record copy, frame bytes, data, data bytes
    assert main[call : call + 2] == b"\x4e\xb9"
    assert (
        struct.unpack(">I", main[call + 2 : call + 6])[0]
        == a["cf.link.fn.frame"]["addr"]
    )
    pre = main[call - 20 : call]
    m = re.fullmatch(
        rb"\x48\x79(....)\x48\x78(..)\x48\x79(....)\x48\x78(..)", pre, re.S
    )
    assert m, pre.hex()
    frame_bytes = struct.unpack(">H", m.group(2))[0]
    data_bytes = struct.unpack(">H", m.group(4))[0]
    assert frame_bytes == 0xABC
    e0 = (data_bytes // 2 + 1) & ~1
    tail_off, words = geometry(dev)
    assert 2 * e0 == tail_off
    assert (frame_bytes // 2 - e0) // 2 == words
    for name, want in (("DL_TAIL_OFF", tail_off), ("DL_TAIL_WORDS", words)):
        assert define(gen_inc(dev), name) == want
    # the literal of `lea <tx queue>,a0` in the driver
    # the literal of the first `pea` of the handler: the record copy (dl_diag reads it)
    rl = a["cf.link.site.reply_copy_lit"]["addr"] - base
    assert main[rl - 2 : rl] == b"\x48\x79"
    assert (
        struct.unpack(">I", main[rl : rl + 4])[0] == struct.unpack(">I", m.group(1))[0]
    )
    lit = a["cf.link.site.tx_queue_lit"]["addr"] - base
    assert main[lit - 2 : lit] == b"\x41\xf9"
    assert 0x80000000 <= struct.unpack(">I", main[lit : lit + 4])[0] < 0x80010000


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_dsp_site_is_the_ring_toggle_store(image):
    from tools.sharc_dev import sharc

    if not config.stock(IMAGES[image]).exists():
        pytest.skip("firmware not present")
    pm = anchors(image)["dsp.link.site.block_tail"]["pm"]
    img = sharc.load("dt2-1.17" if image[:3] == "dt2" else "dn2-1.12")
    rows = dict(img.listing(pm - 1, 4))
    assert re.fullmatch(r"DM\(0x[0-9a-f]+\) = R1", rows[hex(pm)]), rows


# ---- the ColdFire half on the core -------------------------------------------------------------------------------

AREA = (
    0x2001AB00  # a stand-in for the DSP's reserved area (a DSP address; only compared)
)
TXLIT = DATA + 0x100  # holds the queue's address
RECLIT = DATA + 0x110  # holds the record copy's address (0 until a test sets it)
TXQ = DATA + 0x1000  # the fake queue: 4 bytes per entry
REC = DATA + 0x8000


@pytest.fixture(scope="module")
def built():
    if pk_toolchains.m68k_bin("gcc") is None:
        pytest.skip("m68k toolchain not installed")
    syms = {
        "_dl_area": AREA,
        "dl_tx_lit": TXLIT,
        "dl_rec_lit": RECLIT,
        "dl_stock_send": BASE + 0x100,
    }
    return front_end.cf_link(
        [MOD / "src/cf/dsp-link.c"], BASE, syms, [MOD / "build/gen"]
    )


@pytest.fixture(scope="module")
def machine():
    from tools.patchkit.pk_cfcall import CfMachine

    syx = config.stock(IMAGES["dt2-1.17"])
    if not syx.exists():
        pytest.skip("firmware not present")
    return CfMachine(syx)


def pokes(linked, extra):
    return {
        linked.base: linked.image,
        linked.base + len(linked.image): b"\0" * linked.bss_size,
        TXLIT: struct.pack(">I", TXQ),
        **extra,
    }


def run(machine, linked, extra, calls):
    return machine.run(pokes(linked, extra), calls)


def call(linked, fn, args, reads=()):
    return {"pc": linked.symbols[fn], "args": list(args), "reads": list(reads)}


LINK_FIELDS = ["state", "err", "buf", "dst", "nwords", "xid", "cap", "nchunks", "acked", "next", "stall", "since", "sent", "resent", "nerr", "ending", "dirty", "win"]  # fmt: skip


def link_of(linked, r):
    b = r.reads[0]
    return dict(zip(LINK_FIELDS, struct.unpack(">18I", b), strict=True))


def tail_words(blob: bytes, e0: int, n: int) -> list[int]:
    """Words of the queue tail from entry e0: entry e has a command halfword and the data halfword at +2."""
    out = []
    for j in range(n):
        lo = struct.unpack(">H", blob[4 * (e0 + 2 * j) + 2 : 4 * (e0 + 2 * j) + 4])[0]
        hi = struct.unpack(
            ">H", blob[4 * (e0 + 2 * j + 1) + 2 : 4 * (e0 + 2 * j + 1) + 4]
        )[0]
        out.append(lo | (hi << 16))
    return out


def check_word(words: list[int]) -> int:
    s1 = s2 = 0
    for w in words:
        s1 = (s1 + w) & 0xFFFFFFFF
        s2 = (s2 + s1) & 0xFFFFFFFF
    return s1 ^ s2


@needs_gcc
def test_dl_send_checks_its_arguments(machine, built):
    ln = built.symbols["dl_link"]
    src = DATA + 0x4000
    ok = call(built, "dl_send", [AREA + 0x40, src, 64], [(ln, 72)])
    cases = [
        ok,
        call(
            built, "dl_send", [AREA + 0x80, src, 64]
        ),  # busy: the first request has not been taken
    ]
    r = run(machine, built, {src: bytes(range(64))}, cases)
    assert [x.d0 & 0xFFFFFFFF for x in r] == [0, 0xFFFFFFFF]
    lk = link_of(built, r[0])
    assert (
        lk["state"] == 4
        and lk["dst"] == 0x10
        and lk["nwords"] == 16
        and lk["buf"] == src
    )
    bad = [
        [AREA + 1, src, 64],  # unaligned destination
        [AREA, src, 63],  # length not a multiple of 4
        [AREA, src, 0],
        [AREA - 4, src, 64],  # before the area
        [AREA + 0x80000, src, 4],  # after it
        [AREA + 0x7FFFC, src, 8],  # runs over its end
        [AREA, 0, 64],
    ]
    res = run(machine, built, {}, [call(built, "dl_send", a) for a in bad])
    assert [x.d0 & 0xFFFFFFFF for x in res] == [0xFFFFFFFE] * len(bad)
    last = run(
        machine,
        built,
        {src: bytes(4)},
        [call(built, "dl_send", [AREA + 0x7FFFC, src, 4])],
    )
    assert last[0].d0 == 0


@needs_gcc
def test_first_tick_writes_a_chunk_the_dsp_can_check(machine, built):
    ln = built.symbols["dl_link"]
    src = DATA + 0x4000
    data = bytes((i * 7 + 3) & 0xFF for i in range(1024))
    nwords = len(data) // 4
    words = [int.from_bytes(data[i : i + 4], "little") for i in range(0, len(data), 4)]
    # DT2 geometry: 0x802 data bytes, a 0xabc-byte frame: tail from entry 0x402, 174 words, 171 for the payload
    tick = call(built, "dl_tick", [0x802, 0, 0xABC, 0], [(ln, 72), (TXQ, 0x55E * 4)])
    r = run(
        machine,
        built,
        {src: data},
        [call(built, "dl_send", [AREA + 0x20, src, len(data)]), tick, tick],
    )
    first = r[1]
    lk = link_of(built, first)
    assert (lk["cap"], lk["nchunks"], lk["xid"], lk["state"], lk["sent"]) == (
        171,
        2,
        1,
        1,
        1,
    )
    w = tail_words(first.reads[1], 0x402, 174)
    assert w[0] == MAGIC | (0 << 16)
    off, n, last, end, xid = (
        w[1] & 0x1FFFF,
        (w[1] >> 17) & 0xFF,
        (w[1] >> 25) & 1,
        (w[1] >> 26) & 1,
        w[1] >> 27,
    )
    assert (off, n, last, end, xid) == (8, 171, 0, 0, 1)
    assert w[0] >> 30 == 0  # window 0
    assert w[2 : 2 + n] == words[:171]
    assert w[2 + n] == check_word(w[: 2 + n])
    # no acknowledgement came, so the second tick sends chunk 1
    w1 = tail_words(r[2].reads[1], 0x402, 174)
    assert w1[0] == MAGIC | (1 << 16)
    off1, n1, last1 = w1[1] & 0x1FFFF, (w1[1] >> 17) & 0xFF, (w1[1] >> 25) & 1
    assert (off1, n1, last1) == (8 + 171, nwords - 171, 1)
    assert w1[2 : 2 + n1] == words[171:]
    assert w1[2 + n1] == check_word(w1[: 2 + n1])
    # halfwords of the queue outside the tail are untouched (the stock driver writes the command halfwords there)
    blob = first.reads[1]
    assert blob[: 0x402 * 4] == bytes(0x402 * 4)


@needs_gcc
def test_acknowledgements_drive_the_state(machine, built):
    ln = built.symbols["dl_link"]
    src = DATA + 0x4000
    data = (
        bytes(range(256)) * 2
    )  # 128 words: one chunk on DT2 geometry... use DN2 geometry: 12 words per chunk
    n_chunks = 11  # 128 / 12 rounded up

    def ack(cum, xid, status, nerr=0):
        rec = bytearray(0xABC)
        struct.pack_into(
            ">HHHH", rec, ACK_OFF, ACK_MAGIC, cum, xid | (status << 8), nerr
        )
        return bytes(rec)

    def tick(rec_addr):
        return call(built, "dl_tick", [0xA80, 0, 0xABC, rec_addr], [(ln, 72)])

    recs = {
        REC + 0x1000 * i: ack(*a)
        for i, a in enumerate([(0, 1, 0), (5, 1, 0), (5, 2, 0), (11, 1, 1), (11, 1, 1)])
    }
    r0, r1, r2, r3, r4 = recs
    calls = [
        call(built, "dl_send", [AREA, src, len(data)]),
        tick(
            r0
        ),  # starts: DN2 geometry (0xa80 data bytes, 15 tail words), 12 payload words
        tick(r1),  # five chunks taken
        tick(r2),  # an acknowledgement for another transfer id is ignored
        tick(r3),  # all taken, status done
        tick(r4),  # END chunks follow
    ]
    r = run(machine, built, {src: data, **recs}, calls)
    lk = [link_of(built, x) for x in r[1:]]
    assert (lk[0]["cap"], lk[0]["nchunks"], lk[0]["state"]) == (12, n_chunks, 1)
    assert lk[1]["acked"] == 5 and lk[1]["state"] == 1
    assert lk[2]["acked"] == 5 and lk[2]["state"] == 1
    assert (
        lk[3]["acked"] == 11 and lk[3]["state"] == 2 and lk[3]["ending"] == 5
    )  # the first END chunk goes out in the same tick
    assert lk[4]["ending"] == 4
    # polling
    p = run(
        machine,
        built,
        {src: data, **recs},
        [
            call(built, "dl_send", [AREA, src, len(data)]),
            call(built, "dl_poll", [REC + 0x100], [(REC + 0x100, 28)]),
        ],
    )
    assert p[1].d0 == 1  # busy
    assert struct.unpack(">7I", p[1].reads[0])[0] == 1


@needs_gcc
def test_dsp_error_status_and_timeout_stop_the_transfer(machine, built):
    ln = built.symbols["dl_link"]
    src = DATA + 0x4000
    rec = bytearray(0xABC)
    struct.pack_into(
        ">HHHH", rec, ACK_OFF, ACK_MAGIC, 0, 1 | (3 << 8), 0
    )  # status 3: destination refused
    tick = call(built, "dl_tick", [0xA80, 0, 0xABC, REC], [(ln, 72)])
    r = run(
        machine,
        built,
        {src: bytes(64), REC: bytes(rec)},
        [call(built, "dl_send", [AREA, src, 64]), tick],
    )
    lk = link_of(built, r[1])
    assert lk["state"] == 3 and lk["err"] == 3 and lk["ending"] == 5
    # no acknowledgement at all: the timeout is 900 ticks
    ticks = [
        call(built, "dl_tick", [0xA80, 0, 0xABC, 0], [(ln, 72)]) for _ in range(902)
    ]
    r = run(
        machine,
        built,
        {src: bytes(64)},
        [call(built, "dl_send", [AREA, src, 64]), *ticks],
    )
    assert link_of(built, r[-1])["state"] == 3 and link_of(built, r[-1])["err"] == 100
    # a frame with no room in the tail (a data length that fills it) is a geometry error, never a write
    r = run(
        machine,
        built,
        {src: bytes(64)},
        [
            call(built, "dl_send", [AREA, src, 64]),
            call(built, "dl_tick", [0xAB0, 0, 0xABC, 0], [(ln, 72), (TXQ, 64)]),
        ],
    )
    assert link_of(built, r[1])["err"] == 101 and r[1].reads[1] == bytes(64)


@needs_gcc
def test_window_transfers_carry_the_window_and_refuse_bad_arguments(machine, built):
    ln = built.symbols["dl_link"]
    src = DATA + 0x4000
    tick = call(built, "dl_tick", [0x802, 0, 0xABC, 0], [(ln, 72), (TXQ, 0x55E * 4)])
    data = struct.pack("<2I", 0x11223344, 0x55667788)
    r = run(
        machine,
        built,
        {src: data},
        [call(built, "dl_send_win", [1, 3, src, 8]), tick],
    )
    assert r[0].d0 == 0
    w = tail_words(r[1].reads[1], 0x402, 174)
    assert w[0] == MAGIC | (0 << 16) | (1 << 30)
    assert (w[1] & 0x1FFFF, (w[1] >> 17) & 0xFF, (w[1] >> 25) & 1) == (3, 2, 1)
    assert w[2:4] == [0x11223344, 0x55667788]
    assert w[4] == check_word(w[:4])
    # window 2 (the effect rows of the FX chain) is the same transfer with the window number 2 in the header
    r = run(
        machine,
        built,
        {src: data},
        [call(built, "dl_send_win", [2, 5, src, 8]), tick],
    )
    assert r[0].d0 == 0
    w = tail_words(r[1].reads[1], 0x402, 174)
    assert w[0] == MAGIC | (0 << 16) | (2 << 30)
    assert (w[1] & 0x1FFFF, (w[1] >> 17) & 0xFF, (w[1] >> 25) & 1) == (5, 2, 1)
    bad = [
        [0, 0, src, 8],  # window 0 is dl_send's
        [3, 0, src, 8],  # no window 3
        [1, 0, src, 0],
        [1, 0, src, 6],
        [1, 0x20000, src, 4],
        [1, 0, 0, 4],
    ]
    res = run(machine, built, {}, [call(built, "dl_send_win", a) for a in bad])
    assert [x.d0 & 0xFFFFFFFF for x in res] == [0xFFFFFFFE] * len(bad)


DG_MAGIC, DG_WORDS = 0xD1A6, 32


def diag_off(j: int) -> int:
    """Record byte offset of diagnostic word j: slot row 1 + j // 2 (0x1c + 0x54 row), +0x44 or +0x48."""
    return 0x1C + 0x54 * (1 + j // 2) + (0x48 if j & 1 else 0x44)


def diag_record(words: list[int], magic: int = DG_MAGIC) -> bytes:
    rec = bytearray(0xABC)
    for j, w in enumerate([magic | (len(words) + 1) << 16, *words]):
        struct.pack_into(">HH", rec, diag_off(j), w & 0xFFFF, w >> 16)
    return bytes(rec)


def test_diagnostic_words_have_the_same_slots_on_both_sides():
    """The DSP writes word j at word index 7 + 21 (1 + j / 2) + 17 + j % 2 of the record (dsp-link.inc); dl_diag reads the same bytes.
    None of them collides with the acknowledgement (row 0, +0x60 and +0x64) or with the entries the DSP writes (6 to 9 of each row)."""
    inc = (MOD / "src/dsp/dsp-link.inc").read_text()
    assert (
        define(inc, "DL_DG_MAGIC")
        == DG_MAGIC
        == define((MOD / "src/cf/dsp-link.c").read_text(), "DL_DG_MAGIC")
    )
    assert define((MOD / "src/cf/dsp-link.c").read_text(), "DL_DG_WORDS") == DG_WORDS
    # the stores of the DSP routine, as written in the source
    stores = {
        int(m.group(1)): int(m.group(2))
        for m in re.finditer(r"DM\((\d+), I2\) = R0;\s+// word (\d+)", inc)
    }
    # the step is stored by the carry of the static word, which the loop above does not need to see twice
    assert sorted(stores.values()) == list(range(DG_WORDS))
    for widx, j in stores.items():
        assert 4 * widx == diag_off(j), (j, widx)
        row = (diag_off(j) - 0x1C) // 0x54
        entry_off = (diag_off(j) - 0x1C) % 0x54
        assert 1 <= row <= 16
        assert (
            entry_off >= 6 * 11 and entry_off + 4 <= 6 * 13 - 2
        )  # entries 11 and 12 only
    assert define(inc, "DL_ACK_OFF") not in {diag_off(j) for j in range(DG_WORDS)}


@needs_gcc
def test_dl_diag_reads_the_words_from_the_record_copy(machine, built):
    out = DATA + 0x6000
    regs = [
        0,
        1,
        0xFFFFFFFF,
        0,
        1,
        0xFFFFFFFF,
        0x2CB2C8,
        0x2CB2C0,
        0x2CA5A0,
        994,
        0x39011CF8,
    ]
    words = [
        7,
        100,
        *regs,
        0x00120405,
        77,
        76,
        0x01010009,
        0x00020012,
        0x4000,
        0x2000,
        0x7F00,
        0x0100,
        0x12,
    ]  # tail beat, hook beat, M5 .. MODE1, last breadcrumb, dispatches, returns, then the guard's words 17 to 23
    rec = diag_record(words)
    ptr = struct.pack(">I", REC)

    def diag(n):
        return call(built, "dl_diag", [out, n], [(out, 24 * 4)])

    # the literal holds no address (0): -1; then the record
    r = run(machine, built, {out: bytes(0x80)}, [diag(24)])
    assert r[0].d0 & 0xFFFFFFFF == 0xFFFFFFFF
    r = run(
        machine,
        built,
        {RECLIT: ptr, REC: rec, out: bytes(0x80)},
        [diag(24), diag(5)],
    )
    assert r[0].d0 == 24
    assert list(struct.unpack(">24I", r[0].reads[0])) == [DG_MAGIC | 24 << 16, *words]
    assert r[1].d0 == 5  # a shorter buffer
    assert list(struct.unpack(">5I", r[1].reads[0][:20])) == [
        DG_MAGIC | 24 << 16,
        *words[:4],
    ]
    # a record with no diagnostic block (core/dsp-voice absent): -2
    r = run(
        machine, built, {RECLIT: ptr, REC: bytes(0xABC), out: bytes(0x80)}, [diag(24)]
    )
    assert r[0].d0 & 0xFFFFFFFF == 0xFFFFFFFE


@needs_gcc
def test_tick_callback_gets_the_reply_record_and_can_be_cleared(machine, built):
    stub, seen = DATA + 0x7000, DATA + 0x7100
    # move.l 4(sp),d0 ; move.l d0,seen ; rts: the callback stores its argument (the record copy)
    code = struct.pack(">HHHIH", 0x202F, 4, 0x23C0, seen, 0x4E75)
    rec = DATA + 0x8000
    tick = call(built, "dl_tick", [0x802, 0, 0xABC, rec], [(seen, 4)])
    r = run(
        machine,
        built,
        {stub: code, seen: bytes(4)},
        [
            tick,  # no callback: nothing stored
            call(built, "dl_set_tick_cb", [stub]),
            tick,  # the callback sees the record
            call(
                built, "dl_tick_cb_drop", [stub + 0x100, stub + 0x200]
            ),  # not in range: kept
            call(
                built, "dl_tick_cb_drop", [stub, stub + 4]
            ),  # the callback's first byte is in range: dropped
            call(built, "dl_set_tick_cb", [stub]),
            call(
                built, "dl_tick_cb_drop", [stub - 0x100, stub]
            ),  # [lo, hi): the end is excluded
            tick,
        ],
    )
    assert r[0].reads[0] == bytes(4)
    assert struct.unpack(">I", r[2].reads[0])[0] == rec
    # a dropped callback is not called any more: fresh memory, set, drop, tick
    r = run(
        machine,
        built,
        {stub: code, seen: bytes(4)},
        [
            call(built, "dl_set_tick_cb", [stub]),
            call(built, "dl_tick_cb_drop", [stub, stub + 4]),
            tick,
        ],
    )
    assert r[2].reads[0] == bytes(4)
