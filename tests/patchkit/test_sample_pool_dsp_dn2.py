"""core/sample-pool, Digitone II DSP object: our assembled receiver against stand-ins of the stock LP0 driver.

The stand-ins (tests/fixtures/sample-pool-dn2/driver.s) only record what we pass to the driver; they assert nothing about the driver itself.
What is tested is our code: the set-up order and refusals, the page rules against the C reference (spw_dsp_rule in pool_wire.h), where a page
lands in DDR, the rearm after every callback, and the acknowledgement words, which are decoded by the C decoder of the ColdFire side.
The real driver, the DMA, the cache and the timing are for the emulator test."""

import re
import shutil
import subprocess

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import patchkit, pk_sharc, pk_toolchains

MOD = REPO_ROOT / "modules/core/adapters/sample-pool"
FIX = REPO_ROOT / "tests/fixtures/sample-pool-dn2"
CC = shutil.which("cc") or shutil.which("gcc")
BACK = 0xBB0000
PORT = 0x20070000
STACK = 0x20078000
_GEN = (MOD / "build/gen/sample-pool-dsp-dn2.inc").read_text()
DDR_BASE_NW32 = int(re.search(r"#define SP_DDR_BASE_NW32 (\d+)", _GEN).group(1))
DDR_PAGES = int(re.search(r"#define SP_DDR_PAGES (\d+)", _GEN).group(1))
assert DDR_BASE_NW32 == 0x1014C400  # 0x80531000
CONSTS = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M13": 0, "M14": 1, "M15": 0xFFFFFFFF}
REGS = {**CONSTS, "I6": STACK, "I7": STACK, "MODE1": 0x10000, "IMASKP": 0}
REPLY = 0x20050000

BOOT_DRIVER = """
.SECTION/PM seg_pmco;
.GLOBAL _sp_cache_range;
_sp_cache_range:
    DK_ENTER(1); DK_SAVE(2, R3);
    DM(_test_cache_start) = R4; DM(_test_cache_end) = R8;
    DM(_test_cache_selector) = R12;
    R0 = DM(1, I6); DM(_test_cache_mode) = R0;
    R0 = DM(_test_cache_calls); R0 = R0 + 1; DM(_test_cache_calls) = R0;
    DM(0x3e018) = R4; R8 = R8 + 1; DM(0x3e019) = R8;
    R0 = 0x300; DM(0x3e002) = R0;
    R0 = DM(_test_cache_result);
    DK_RETADDR; NOP; NOP; NOP; DK_LEAVE(R3, 2);
.SECTION/DM seg_dmda;
.VAR _test_cache_start = 0; .VAR _test_cache_end = 0;
.VAR _test_cache_selector = 0; .VAR _test_cache_mode = 0;
.VAR _test_cache_calls = 0; .VAR _test_cache_result = 0;
"""

SOURCES = [
    MOD / "src/dsp/dn2/boot-window.s",
    MOD / "src/dsp/dn2/sample-pool.s",
    FIX / "driver.s",
]

pytestmark = pytest.mark.skipif(CC is None, reason="no host C compiler")


@pytest.fixture(scope="module", autouse=True)
def prebuilt_tools():
    patch = pytest.MonkeyPatch()
    patch.setattr(patchkit, "_BUILT", patchkit.binary(build=False))
    yield
    patch.undo()


@pytest.fixture(scope="module")
def dsp_objects():
    if pk_toolchains.selache_dir() is None:
        pytest.skip("assembler absent")
    paths = [REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk", MOD / "build/gen"]
    return [
        pk_sharc.assemble(
            pk_sharc.expand_includes(
                path.read_text() + (BOOT_DRIVER if path.name == "driver.s" else ""),
                paths,
            )
        )
        for path in SOURCES
    ]


def link_dsp(objects, data_address=0x20080000):
    address, syms, inits = data_address, {}, []
    for obj in objects:
        sections = {}
        for sec, (data, align, _) in obj.data.items():
            address = (address + align - 1) & -align
            sections[sec] = address
            inits.append((address, data))
            address += len(data)
        for name, (sec, offset, _) in obj.data_symbols.items():
            syms[name] = sections[sec] + offset
    externs = {
        **syms,
        "_sp_boot_back": BACK,
        "_sp_back": BACK,
        "_test_return": BACK,
        "_sp_port_record": PORT,
    }
    bases = [0xB90000, 0xBA0000, 0xBC0000]
    for obj, base in zip(objects, bases, strict=True):
        externs.update(
            {
                name: base + off
                for name, (off, _) in obj.symbols.items()
                if name not in syms
            }
        )
    linked = [
        pk_sharc.link(obj, base, externs)
        for obj, base in zip(objects, bases, strict=True)
    ]
    return linked, syms, inits


def fresh(objects):
    linked, syms, inits = link_dsp(objects)
    sess = pk_sharc.CallSession(linked, BACK)
    for address, data in inits:
        for off in range(0, len(data), 4):
            sess.poke(
                (address + off) // 4, int.from_bytes(data[off : off + 4], "little")
            )
    sess.poke(0x3E000, 0x414141)
    sess.poke((PORT + 20) // 4, 0)
    for address in (0x30FFE000, 0x30FFF008):
        sess.poke(address, 0)
    return sess, syms


def val(sess, syms, name):
    return sess.peek(syms[name] // 4)


def boot(sess, syms):
    regs = dict(REGS)
    result = sess.call("_sp_boot_hook", regs, max_steps=30000, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert val(sess, syms, "_sp_boot_status") == 1


def tail(sess, syms, regs=None, boot_first=True):
    """One block tail (the first one sets up the port)."""
    if boot_first and val(sess, syms, "_sp_boot_status") == 0:
        boot(sess, syms)
    regs = {**REGS, **(regs or {})}
    result = sess.call("_sp_tail", regs, max_steps=30000, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert {
        key: result["regs"][key] for key in regs
    } == regs  # every register comes back as it went in
    return result


def init(sess, syms):
    tail(sess, syms, {"I4": REPLY})


def reply_words(sess):
    return [sess.peek((REPLY + 0xABC) // 4 + i) for i in range(4)]


def send_page(sess, syms, tag, *, fill=None, event=4, address=None, context=None):
    """Hand the callback one received frame: the tag, then 1,024 payload words."""
    rx = val(sess, syms, "_sp_rx")
    payload = (
        [(0x12340000 + i * 3 + (tag & 0xFFFF)) & 0xFFFFFFFF for i in range(1024)]
        if fill is None
        else fill
    )
    sess.poke(rx // 4, tag)
    for i, word in enumerate(payload):
        sess.poke(rx // 4 + 1 + i, word)
    regs = {
        **REGS,
        "R4": syms["_sp_status"] if context is None else context,
        "R8": event,
        "R12": rx if address is None else address,
    }
    result = sess.call("_test_callback", regs, max_steps=30000, read_regs=("I6", "I7"))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert (result["regs"]["I6"], result["regs"]["I7"]) == (STACK, STACK)
    return payload


def tag(seq, page):
    return 0xD0000000 | (seq << 20) | page


def dest(sess, page):
    base = DDR_BASE_NW32 + page * 1024
    return [sess.peek(base + i) for i in range(1024)]


# ---- the C reference and the C decoder of the ColdFire side ---------------------------------------------------------------------------

REF_C = r"""
#include <stdio.h>
#include <stdlib.h>
#include "cf/pool_wire.h"
int main(int argc, char **argv)
{
    if (argv[1][0] == 'r') {   /* rule: last tag page-count */
        printf("%u\n", spw_dsp_rule((uint32_t)strtoul(argv[2], 0, 16), (uint32_t)strtoul(argv[3], 0, 16), (uint32_t)atoi(argv[4])));
    } else {                   /* decode four words */
        uint16_t h[8]; spw_ack a; int i;
        for (i = 0; i < 4; i++) { uint32_t w = (uint32_t)strtoul(argv[2 + i], 0, 16); h[2 * i] = w & 0xffff; h[2 * i + 1] = w >> 16; }
        if (!spw_ack_decode(h, &a)) { puts("invalid"); return 0; }
        printf("%u %u %u %u %x\n", a.status, a.error, a.errcount, a.count, a.last);
    }
    return 0;
}
"""


@pytest.fixture(scope="module")
def ref(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("ref")
    src = tmp / "ref.c"
    src.write_text(REF_C)
    exe = tmp / "ref"
    cf = MOD / "src"
    inc = REPO_ROOT / "modules/core/runtime/include"
    subprocess.run(
        [CC, "-Wall", "-Werror", f"-I{cf}", f"-I{inc}", str(src), "-o", str(exe)],
        check=True,
    )

    def run(*args):
        return subprocess.run(
            [str(exe), *map(str, args)], check=True, capture_output=True, text=True
        ).stdout.strip()

    return run


def decoded(ref, words):
    out = ref("d", *(f"{w:x}" for w in words))
    return (
        out
        if out == "invalid"
        else tuple(int(x, 16) if i == 4 else int(x) for i, x in enumerate(out.split()))
    )


# ---- tests ---------------------------------------------------------------------------------------------------------------------------------


def test_setup_once_in_the_stock_order_and_every_register_returns(dsp_objects):
    sess, syms = fresh(dsp_objects)
    assert val(sess, syms, "_sp_status") == 0
    init(sess, syms)
    assert val(sess, syms, "_sp_status") == 1 and val(sess, syms, "_sp_started") == 1
    assert [val(sess, syms, f"_test_open_arg{i}") for i in range(2)] == [0, 1]
    assert val(sess, syms, "_test_open_size") == 128
    assert val(sess, syms, "_test_setcb_fn") == sess.symbols["_sp_callback"]
    assert val(sess, syms, "_test_setcb_context") == syms["_sp_status"]
    assert (
        val(sess, syms, "_test_submit_config"),
        val(sess, syms, "_test_submit_arg2"),
    ) == (0, 1)
    rx = val(sess, syms, "_sp_rx")
    assert rx % 64 == 0
    # descriptor: 4,100 bytes (0x401 words of 4) into the receive buffer, rebuilt for every submit
    assert [val(sess, syms, f"_test_desc{i}") for i in range(7)] == [
        0,
        rx,
        0x100000,
        0x401,
        4,
        0,
        0,
    ]
    init(sess, syms)  # later blocks only report
    assert (
        val(sess, syms, "_test_open_calls")
        == val(sess, syms, "_test_submit_calls")
        == 1
    )
    # a hostile register file is saved and restored (tail() checks every register)
    hostile = {f"R{i}": 0x7100 + i for i in range(16)}
    hostile.update(
        {f"I{i}": 0x20060000 + 16 * i for i in range(16) if i not in (4, 6, 7)}
    )
    hostile.update({f"M{i}": 9 + i for i in range(16)})
    hostile.update({f"L{i}": 17 + i for i in range(8)})
    hostile.update({f"B{i}": 0x20050000 + 16 * i for i in range(8)})
    sess2, syms2 = fresh(dsp_objects)
    boot(sess2, syms2)
    tail(sess2, syms2, {**hostile, "I4": REPLY})
    assert val(sess2, syms2, "_sp_status") == 1


@pytest.mark.parametrize("bad", ["context", "owner", "lpctl", "dmacfg", "boot"])
def test_setup_refusals_make_the_link_dead_before_any_driver_call(dsp_objects, bad):
    sess, syms = fresh(dsp_objects)
    regs = {"I4": REPLY}
    if bad == "boot":
        pass  # no boot hook ran: the window is not there
    else:
        boot(sess, syms)
    if bad == "context":
        regs["IMASKP"] = 1
    elif bad != "boot":
        addr = {"owner": PORT + 20, "lpctl": 0x30FFE000, "dmacfg": 0x30FFF008}[bad]
        sess.poke(addr // 4 if bad == "owner" else addr, 1)
    tail(sess, syms, regs, boot_first=False)
    assert val(sess, syms, "_sp_status") == 3
    assert (
        val(sess, syms, "_sp_error")
        == {"context": 1, "owner": 2, "lpctl": 2, "dmacfg": 2, "boot": 8}[bad]
    )
    assert (
        val(sess, syms, "_test_open_calls")
        == val(sess, syms, "_test_submit_calls")
        == 0
    )
    tail(sess, syms, regs, boot_first=False)  # it stays dead and tries nothing
    assert val(sess, syms, "_test_open_calls") == 0
    assert decoded_status(sess) == 3


def decoded_status(sess):
    return reply_words(sess)[1] & 0xFFFF


def test_page_rules_match_the_c_reference_and_pages_land_where_the_tag_says(
    dsp_objects, ref
):
    sess, syms = fresh(dsp_objects)
    init(sess, syms)
    last, accepted, errors, submits = tag(0, 0), 0, 0, 1
    # new pages (any order), a gap, a repeat, bad index, bad prefix, a stale sequence number, and 270 more pages: the sequence wraps
    plan = [
        tag(1, 0), tag(2, 5), tag(3, 5), tag(3, 5), tag(5, 1), tag(4, DDR_PAGES), tag(4, DDR_PAGES - 1), 0xC0500001,
        tag(5, 0xFFFFF), tag(5, 9), tag(5, 9), tag(4, 9), tag(6, 7),
    ]  # fmt: skip
    seq = 6
    for i in range(270):
        seq = (seq + 1) & 0xFF
        plan.append(tag(seq, (i * 977) % DDR_PAGES))
    for t in plan:
        before = (
            dest(sess, t & 0xFFFFF)
            if (t >> 28) == 0xD and (t & 0xFFFFF) < DDR_PAGES
            else None
        )
        verdict = int(ref("r", f"{last:x}", f"{t:x}", DDR_PAGES))
        payload = send_page(sess, syms, t)
        submits += 1  # every callback arms again
        if verdict in (0, 1):
            assert dest(sess, t & 0xFFFFF) == payload, hex(t)
        elif before is not None:
            assert dest(sess, t & 0xFFFFF) == before, hex(
                t
            )  # a rejected page writes nothing
        if verdict == 0:
            accepted += 1
            last = t
        elif verdict >= 4:
            errors += 1
        assert val(sess, syms, "_sp_pages") == accepted, hex(t)
        assert val(sess, syms, "_sp_last") == last, hex(t)
        assert val(sess, syms, "_sp_errcount") == errors, hex(t)
        assert val(sess, syms, "_sp_error") == (verdict if verdict >= 4 else 0), hex(t)
        assert val(sess, syms, "_sp_status") == (2 if verdict >= 4 else 1), hex(t)
        assert val(sess, syms, "_test_submit_calls") == submits
    # 6 + 270 new pages (the sequence number wraps past 255); the gap, the stale number, two bad indexes and the bad prefix are the 5 errors
    assert (accepted, errors) == (276, 5)


def test_acknowledgement_words_decode_in_the_cf_decoder_and_keep_the_stock_bytes(
    dsp_objects, ref
):
    sess, syms = fresh(dsp_objects)
    for i in range((0xABC + 16) // 4):
        sess.poke(REPLY // 4 + i, 0xCAFE1234)
    init(sess, syms)
    assert [sess.peek(REPLY // 4 + i) for i in range(0xABC // 4)] == [0xCAFE1234] * (
        0xABC // 4
    )
    assert decoded(ref, reply_words(sess)) == (1, 0, 0, 0, 0)
    send_page(sess, syms, tag(1, 70000 % DDR_PAGES))
    send_page(sess, syms, tag(2, 3))
    # the tail runs with other DAG state: the reply still lands, and only after the callback finished
    tail(sess, syms, {"I4": REPLY, "M6": 9, "L4": 17, "L0": 17, "MODE1": 0x219000})
    assert decoded(ref, reply_words(sess)) == (1, 0, 0, 2, (2 << 8) | 3)
    send_page(sess, syms, tag(7, 1))  # a gap
    tail(sess, syms, {"I4": REPLY})
    assert decoded(ref, reply_words(sess)) == (2, 5, 1, 2, (2 << 8) | 3)
    send_page(sess, syms, tag(3, 0x1FF))
    tail(sess, syms, {"I4": REPLY})
    # the error is gone, its count stays; the page index shows its low byte only
    assert decoded(ref, reply_words(sess)) == (1, 0, 1, 3, (3 << 8) | 0xFF)
    # count above 16 bits and a high error count
    sess.poke(syms["_sp_pages"] // 4, 0x12345)
    sess.poke(syms["_sp_errcount"] // 4, 0x1FF)
    tail(sess, syms, {"I4": REPLY})
    assert decoded(ref, reply_words(sess)) == (1, 0, 0xFF, 0x12345, (3 << 8) | 0xFF)
    # a dead link shows status 3 and its error code
    sess.poke(syms["_sp_status"] // 4, 3)
    sess.poke(syms["_sp_error"] // 4, 9)
    tail(sess, syms, {"I4": REPLY})
    assert decoded(ref, reply_words(sess))[:2] == (3, 9)


@pytest.mark.parametrize("bad", ["event", "address", "context"])
def test_callback_with_wrong_arguments_kills_the_link_without_copy_or_rearm(
    dsp_objects, bad
):
    sess, syms = fresh(dsp_objects)
    init(sess, syms)
    kwargs = {}
    if bad == "event":
        kwargs["event"] = 3
    elif bad == "address":
        kwargs["address"] = val(sess, syms, "_sp_rx") + 4
    else:
        kwargs["context"] = 0
    before = dest(sess, 0)
    send_page(sess, syms, tag(1, 0), **kwargs)
    assert dest(sess, 0) == before
    assert (val(sess, syms, "_sp_status"), val(sess, syms, "_sp_error")) == (3, 4)
    assert val(sess, syms, "_test_submit_calls") == 1
    send_page(sess, syms, tag(1, 0))  # dead: further callbacks do nothing
    assert dest(sess, 0) == before and val(sess, syms, "_test_submit_calls") == 1


def test_callback_before_the_link_is_armed_is_refused(dsp_objects):
    sess, syms = fresh(dsp_objects)
    boot(sess, syms)
    # status 0: the port was not set up (a callback should not exist yet)
    rx = val(sess, syms, "_sp_rx")
    regs = {**REGS, "R4": syms["_sp_status"], "R8": 4, "R12": rx}
    sess.poke(rx // 4, tag(1, 0))
    result = sess.call("_test_callback", regs, max_steps=30000, read_regs=("I6",))
    assert result["halt"]["reason"] == "breakpoint"
    assert (val(sess, syms, "_sp_status"), val(sess, syms, "_sp_error")) == (3, 4)


def test_rearm_failure_is_dead_with_error_9(dsp_objects):
    sess, syms = fresh(dsp_objects)
    init(sess, syms)
    sess.poke(syms["_test_submit_result"] // 4, 5)  # the driver refuses the next submit
    send_page(sess, syms, tag(1, 2))
    assert val(sess, syms, "_sp_pages") == 1  # the page itself was taken
    assert (val(sess, syms, "_sp_status"), val(sess, syms, "_sp_error")) == (3, 9)
    tail(sess, syms, {"I4": REPLY})
    assert decoded_status(sess) == 3
