"""Private DN2 LP0 diagnostic: our assembled guards, call frames and mailbox.

Driver stand-ins inspect our ABI without asserting third-party driver behavior.
The CF runner reuses the existing executable; these tests never build a daemon.
"""

import os
import struct
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit, pk_cfcall, pk_sharc, pk_toolchains

MOD = REPO_ROOT / "modules/core/adapters/dsp-lp0-proof"
BOOT_DRIVER = """
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
.GLOBAL _lp_cache_range;
_lp_cache_range:
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
FIX = REPO_ROOT / "tests/fixtures/dsp-lp0-proof/driver.s"
BACK = 0xBB0000
PORT = 0x20070000
STACK = 0x20078000
DEST = 0x10200000
CANARY = 0xA55A3CC3


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
    objects = [
        pk_sharc.assemble(
            pk_sharc.expand_includes(
                (path.read_text() + (BOOT_DRIVER if path == FIX else "")),
                [REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk"],
            )
        )
        for path in (MOD / "src/dsp/lp0-proof.s", FIX, MOD / "src/dsp/readers.s")
    ]
    return objects


def link_dsp(objects, data_address):
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
        "_lp_boot_back": BACK,
        "_lp_back": BACK,
        "_test_return": BACK,
        "_lp_port_record": PORT,
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


@pytest.fixture(scope="module")
def dsp(dsp_objects):
    return link_dsp(dsp_objects, 0x20080000)


def fresh(dsp):
    linked, syms, inits = dsp
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
    sess.lp_syms = syms
    return sess, syms


def val(sess, syms, name):
    return sess.peek(syms[name] // 4)


def put(sess, syms, name, value):
    sess.poke(syms[name] // 4, value)


CONSTS = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M13": 0, "M14": 1, "M15": 0xFFFFFFFF}
REGS = {**CONSTS, "I6": STACK, "I7": STACK, "MODE1": 0x10000, "IMASKP": 0}


def boot(sess, regs=None):
    regs = {**REGS, **(regs or {})}
    result = sess.call("_lp_boot_hook", regs, max_steps=30000, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    expected = {**regs}
    if "R2" in expected:
        expected["R2"] = 0x414141
    assert {key: result["regs"][key] for key in regs} == expected
    return result


def init(sess, regs=None):
    if sess.peek(sess.lp_syms["_lp_boot_status"] // 4) == 0:
        boot(sess)
    regs = {**REGS, **(regs or {})}
    result = sess.call("_lp_hook", regs, max_steps=30000, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert {key: result["regs"][key] for key in regs} == regs
    return result


def complete(sess, syms, tag, *, event=4, address=None, context=None):
    rx = val(sess, syms, "_lp_rx")
    payload = [(0x12340000 + i + tag) & 0xFFFFFFFF for i in range(1024)]
    sess.poke(rx // 4, tag)
    for i, word in enumerate(payload):
        sess.poke(rx // 4 + 1 + i, word)
    regs = {
        **REGS,
        "R4": syms["_lp_status"] if context is None else context,
        "R8": event,
        "R12": rx if address is None else address,
    }
    result = sess.call("_test_callback", regs, max_steps=30000, read_regs=("I6", "I7"))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert {key: result["regs"][key] for key in ("I6", "I7")} == {
        "I6": STACK,
        "I7": STACK,
    }
    return payload


def test_setup_once_uses_real_call_frames_and_preserves_hook_context(dsp):
    sess, syms = fresh(dsp)
    hostile = {f"R{i}": 0x7100 + i for i in range(16)}
    hostile.update({f"I{i}": 0x20060000 + 16 * i for i in range(16) if i not in (6, 7)})
    hostile.update({f"M{i}": 9 + i for i in range(16)})
    hostile.update({f"L{i}": 17 + i for i in range(8)})
    hostile.update({f"B{i}": 0x20050000 + 16 * i for i in range(8)})
    init(sess, hostile)
    assert val(sess, syms, "_lp_status") == 2
    assert [
        val(sess, syms, "_test_open_arg0"),
        val(sess, syms, "_test_open_arg1"),
        val(sess, syms, "_test_open_size"),
    ] == [0, 1, 128]
    assert val(sess, syms, "_test_open_state") & 15 == 0
    assert val(sess, syms, "_test_setcb_fn") == sess.symbols["_lp_callback"]
    assert val(sess, syms, "_test_setcb_context") == syms["_lp_status"]
    assert val(sess, syms, "_test_submit_config") == 0
    assert val(sess, syms, "_test_submit_arg2") == 1
    rx = val(sess, syms, "_lp_rx")
    assert rx & 63 == 0
    assert [val(sess, syms, f"_test_desc{i}") for i in range(7)] == [
        0,
        rx,
        0x100000,
        0x401,
        4,
        0,
        0,
    ]
    init(sess)
    assert (
        val(sess, syms, "_test_open_calls")
        == val(sess, syms, "_test_submit_calls")
        == 1
    )


@pytest.mark.parametrize("phase", range(0, 64, 4))
def test_rx_owns_whole_cache_lines_at_every_data_placement(dsp_objects, phase):
    sess, syms = fresh(link_dsp(dsp_objects, 0x20080000 + phase))
    init(sess)
    rx = val(sess, syms, "_lp_rx")
    storage = syms["_lp_dma_storage"]
    base = val(sess, syms, "_lp_dma_base")
    limit = val(sess, syms, "_lp_dma_limit")
    assert base % 64 == 0
    assert limit == base + 4352
    assert storage <= base and limit <= storage + 4416
    assert val(sess, syms, "_lp_descriptor") == base
    assert val(sess, syms, "_lp_state") == base + 64
    assert rx == base + 192
    section, offset, _ = dsp_objects[0].data_symbols["_lp_dma_storage"]
    reservation_bytes = len(dsp_objects[0].data[section][0]) - offset
    assert rx % 64 == 0
    assert storage <= rx
    assert rx + 4160 <= storage + reservation_bytes
    state = val(sess, syms, "_lp_state")
    assert state % 16 == 0
    for start, size in (
        (state, 128),
        (val(sess, syms, "_lp_descriptor"), 28),
        (syms["_lp_save"], 292),
        *(
            (syms[name], 4)
            for name in (
                "_lp_status",
                "_lp_error",
                "_lp_pages",
                "_lp_bytes",
                "_lp_state",
                "_lp_rx",
                "_lp_handle",
            )
        ),
    ):
        assert start + size <= rx or rx + 4160 <= start
    for name, size in (
        ("_lp_save", 292),
        ("_lp_boot_save", 292),
        ("_lp_dma_base", 4),
        ("_lp_dma_limit", 4),
        ("_lp_boot_status", 4),
        ("_lp_boot_error", 4),
    ):
        start = syms[name]
        assert start + size <= base or limit <= start
    assert val(sess, syms, "_test_desc1") == rx
    assert val(sess, syms, "_test_desc3") * val(sess, syms, "_test_desc4") == 4100
    # The simulated receive and assembled callback leave cache-line padding alone.
    padding = range(rx + 4100, rx + 4160, 4)
    for address in padding:
        sess.poke(address // 4, CANARY)
    payload = complete(sess, syms, 0)
    assert [sess.peek(DEST + i) for i in range(1024)] == payload
    assert all(sess.peek(address // 4) == CANARY for address in padding)
    assert val(sess, syms, "_test_submit_calls") == 2
    assert val(sess, syms, "_test_desc1") == rx


@pytest.mark.parametrize("bad", ["context", "owner", "lpctl", "dmacfg"])
def test_live_preflight_refuses_before_any_driver_mutation(dsp, bad):
    sess, syms = fresh(dsp)
    regs = {}
    if bad == "context":
        regs["IMASKP"] = 1
    else:
        addr = {"owner": PORT + 20, "lpctl": 0x30FFE000, "dmacfg": 0x30FFF008}[bad]
        sess.poke(addr // 4 if bad == "owner" else addr, 1)
    init(sess, regs)
    assert val(sess, syms, "_lp_status") == 4
    assert (
        val(sess, syms, "_test_open_calls")
        == val(sess, syms, "_test_submit_calls")
        == 0
    )
    init(sess)
    assert val(sess, syms, "_test_open_calls") == 0


def test_one_page_rearms_reconstructed_descriptor_and_exact_payload(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    payload = complete(sess, syms, 0)
    assert [sess.peek(DEST + i) for i in range(1024)] == payload
    assert (val(sess, syms, "_lp_pages"), val(sess, syms, "_lp_bytes")) == (1, 4096)
    assert val(sess, syms, "_test_submit_calls") == 2
    assert (
        val(sess, syms, "_test_desc2") == 0x100000
    )  # stand-in poisoned it after first submit
    assert sess.peek(DEST - 1) == sess.peek(DEST + 240000) == CANARY


@pytest.mark.parametrize(
    "bad", ["event", "address", "context", "tag", "gap", "pages", "bytes", "state"]
)
def test_callback_failures_are_sticky_without_copy_or_rearm(dsp, bad):
    sess, syms = fresh(dsp)
    init(sess)
    kwargs, tag = {}, 0
    if bad == "event":
        kwargs["event"] = 3
    elif bad == "address":
        kwargs["address"] = val(sess, syms, "_lp_rx") + 4
    elif bad == "context":
        kwargs["context"] = 0
    elif bad in ("tag", "gap"):
        tag = 235 if bad == "tag" else 1
    elif bad == "pages":
        put(sess, syms, "_lp_pages", 0xFFFFFFFF)
        tag = 0xFFFFFFFF
    elif bad == "bytes":
        put(sess, syms, "_lp_bytes", 4)
    elif bad == "state":
        put(sess, syms, "_lp_status", 1)
    sess.poke(DEST, 0xAABBCCDD)
    complete(sess, syms, tag, **kwargs)
    assert val(sess, syms, "_lp_status") == 4
    assert sess.peek(DEST) == 0xAABBCCDD
    assert val(sess, syms, "_test_submit_calls") == 1
    complete(sess, syms, 0)
    assert sess.peek(DEST) == 0xAABBCCDD
    assert val(sess, syms, "_test_submit_calls") == 1


def test_final_page_clamps_to_1536_bytes_and_verifies_full_canaries(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_lp_pages", 234)
    put(sess, syms, "_lp_bytes", 234 * 4096)
    payload = complete(sess, syms, 234)
    assert (val(sess, syms, "_lp_pages"), val(sess, syms, "_lp_bytes")) == (235, 960000)
    assert val(sess, syms, "_lp_status") == 3
    assert [sess.peek(DEST + 234 * 1024 + i) for i in range(384)] == payload[:384]
    assert all(sess.peek(DEST + 240000 + i) == CANARY for i in range(1024))
    assert all(sess.peek(DEST - 1024 + i) == CANARY for i in range(1024))
    assert val(sess, syms, "_test_submit_calls") == 2


@pytest.mark.parametrize("phase", ["open", "first_submit", "rearm"])
def test_driver_failure_is_sticky_and_never_reopens(dsp, phase):
    sess, syms = fresh(dsp)
    if phase == "open":
        put(sess, syms, "_test_open_result", 19)
    if phase == "first_submit":
        put(sess, syms, "_test_submit_result", 4)
    init(sess)
    if phase == "rearm":
        put(sess, syms, "_test_submit_result", 4)
        complete(sess, syms, 0)
    assert val(sess, syms, "_lp_status") == 4
    calls = val(sess, syms, "_test_submit_calls")
    init(sess)
    complete(sess, syms, 0)
    assert val(sess, syms, "_test_open_calls") == 1
    assert val(sess, syms, "_test_submit_calls") == calls


def test_duplicate_page_stops_and_guard_damage_prevents_completion(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    complete(sess, syms, 0)
    sess.poke(DEST, 0xABCD1234)
    complete(sess, syms, 0)
    assert val(sess, syms, "_lp_status") == 4
    assert sess.peek(DEST) == 0xABCD1234
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_lp_pages", 234)
    put(sess, syms, "_lp_bytes", 234 * 4096)
    sess.poke(DEST + 240000 + 1023, 0)
    complete(sess, syms, 234)
    assert val(sess, syms, "_lp_status") == 4


@pytest.fixture(scope="module")
def cf(tmp_path_factory):
    if pk_toolchains.m68k_bin("gcc") is None:
        pytest.skip("ColdFire compiler absent")
    runner = Path(os.environ.get("DIGI_LP0_CF_CALL", str(pk_cfcall.EXAMPLE)))
    if not runner.is_file():
        pytest.skip("prebuilt cf_call absent; no implicit native build")
    patch = pytest.MonkeyPatch()
    patch.setattr(pk_cfcall, "EXAMPLE", runner)
    patch.setattr(pk_cfcall, "_built", True)
    stub = tmp_path_factory.mktemp("lp0-proof") / "sender.c"
    stub.write_text("""#include "lp0-proof.c"
volatile u32 lp_capture[8];
volatile u16 lp_fake_reply[0xacc/2];
volatile unsigned char lp_fake_copy[0xabc+16];
void lp_stock_send(u32 tag, const volatile u32 *source, u32 format) {
    ++lp_capture[0]; lp_capture[1]=tag; lp_capture[2]=(u32)source;
    lp_capture[3]=format; lp_capture[4]=lp_mail.state;
    lp_capture[5]=source[0]; lp_capture[6]=source[1023];
}
void *lp_stock_copy(void *dst, const void *src, u32 n) {
    unsigned char *d=dst; const unsigned char *s=src;
    for(u32 i=0;i<n;i++) d[i]=s[i];
    return dst;
}
void lp_test_wire(u32 magic, u32 version, u32 status, u32 error, u32 pages, u32 bytes) {
    for(u32 i=0;i<0xabc/2;i++) lp_fake_reply[i]=(u16)(i*71+0x8001);
    for(u32 i=0;i<sizeof(lp_fake_copy);i++) lp_fake_copy[i]=0xa5;
    volatile u16 *e=lp_fake_reply+0xabc/2;
    e[0]=magic;e[1]=version;e[2]=status;e[3]=error;
    e[4]=pages;e[5]=pages>>16;e[6]=bytes;e[7]=bytes>>16;
    lp_capture[7]=(u32)lp_copy_reply((void *)lp_fake_copy,(const void *)lp_fake_reply,0xabc);
}
void lp_test_stage(u32 tag) {
    lp_mail.tag=tag; lp_staging[0]=0x12345678; lp_staging[1023]=0x89abcdef;
    lp_mail.state=1;
}
void lp_test_prime(u32 next) { lp_next=next; lp_mail.sent=next; }
void lp_test_odd(void) { lp_ack.sequence |= 1; }
void lp_test_close(u32 id,u32 track,u32 epoch) {
 lp_close_mail.id=id;lp_close_mail.track=track;lp_close_mail.epoch=epoch;lp_close_mail.state=1;
}
void lp_test_control_wire(u32 header,u32 id,u32 track,u32 epoch) {
 volatile u16 *e=lp_fake_reply+0xabc/2;
 e[0]=header;e[1]=header>>16;e[2]=id;e[3]=id>>16;
 e[4]=track;e[5]=track>>16;e[6]=epoch;e[7]=epoch>>16;
 lp_copy_reply((void *)lp_fake_copy,(const void *)lp_fake_reply,0xabc);
}
void lp_test_close_odd(void) { lp_close_ack.sequence |= 1; }
void lp_test_timeout(void) { lp_close_polls=LP_CLOSE_POLLS; }

""")
    linked = patchkit.cf_link([stub], 0x47E80000, {}, include_dirs=[MOD / "src/cf"])
    try:
        yield linked
    finally:
        patch.undo()


def cf_run(cf, commands):
    image = config.stock("Digitone_II_OS1.12.syx")
    if not image.exists():
        pytest.skip("firmware for CF CPU setup absent")
    reads = [
        (cf.symbols["lp_mail"], 16),
        (cf.symbols["lp_capture"], 32),
        (cf.symbols["lp_ack"], 24),
        (cf.symbols["lp_fake_copy"], 0xABC + 16),
        (cf.symbols["lp_close_mail"], 24),
        (cf.symbols["lp_close_ack"], 24),
    ]
    calls = [
        {"pc": cf.symbols[name], "args": args, "reads": reads}
        for name, args in commands
    ]
    results = pk_cfcall.CfMachine(image).run(
        {cf.base: cf.image, cf.base + len(cf.image): bytes(cf.bss_size)}, calls
    )
    assert all(r.ok and not r.clobbered for r in results)
    return results


def ack(pages=0, status=2, error=0, count=None, magic=0x4C50, version=1):
    count = (960000 if pages == 235 else pages * 4096) if count is None else count
    return ("lp_test_wire", [magic, version, status, error, pages, count])


def cf_words(result, index):
    data = result.reads[index]
    return struct.unpack(">" + "I" * (len(data) // 4), data)


def test_cf_mailbox_waits_for_wire_ack_without_resending(cf):
    r = cf_run(
        cf,
        [
            ("lp_test_stage", [0]),
            ("lp_tick", []),
            ack(magic=0x1234, status=4, error=99),
            ("lp_tick", []),
            ack(),
            ("lp_tick", []),
            ("lp_tick", []),
            ack(1),
            ("lp_tick", []),
            ("lp_test_stage", [0]),
            ("lp_tick", []),
            ("lp_test_stage", [1]),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[1], 0) == (1, 0, 0, 0)  # missing stays START
    assert cf_words(r[3], 0) == (1, 0, 0, 0)  # wrong magic is not a published reply
    assert cf_words(r[5], 0) == (2, 0, 0, 1)  # sender returned, but no consumption ACK
    assert cf_words(r[5], 1)[:7] == (
        1,
        0,
        cf.symbols["lp_staging"],
        1,
        2,
        0x12345678,
        0x89ABCDEF,
    )
    assert cf_words(r[6], 0) == (2, 0, 0, 1)
    assert cf_words(r[6], 1)[0] == 1
    assert cf_words(r[8], 0) == (3, 0, 0, 1)
    assert cf_words(r[10], 0) == (4, 1, 0, 1)
    assert cf_words(r[12], 0)[0] == 4
    assert cf_words(r[12], 1)[0] == 1


@pytest.mark.parametrize(
    "wire", [ack(version=2), ack(1, count=0), ack(status=4, error=7), ack(2)]
)
def test_cf_bad_ack_is_sticky_and_never_sends(cf, wire):
    r = cf_run(
        cf,
        [wire, ack(), ("lp_test_stage", [0]), ("lp_tick", []), ack(), ("lp_tick", [])],
    )
    # Future but internally valid progress is rejected at request admission.
    # For that case preserve it until the UI observes it.
    if wire == ack(2):
        r = cf_run(
            cf, [wire, ("lp_test_stage", [0]), ("lp_tick", []), ack(), ("lp_tick", [])]
        )
    assert cf_words(r[-1], 0)[0] == 4
    assert cf_words(r[-1], 1)[0] == 0


def test_cf_terminal_requires_status_three_and_odd_snapshot_stays_pending(cf):
    r = cf_run(
        cf,
        [
            ("lp_test_prime", [234]),
            ack(234),
            ("lp_test_stage", [234]),
            ("lp_tick", []),
            ("lp_tick", []),
            ack(235, status=3),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[3], 0) == (2, 0, 234, 235)
    assert cf_words(r[4], 1)[0] == 1
    assert cf_words(r[6], 0) == (3, 0, 234, 235)
    r = cf_run(
        cf, [ack(), ("lp_test_odd", []), ("lp_test_stage", [0]), ("lp_tick", [])]
    )
    assert cf_words(r[-1], 0) == (1, 0, 0, 0)
    assert cf_words(r[-1], 1)[0] == 0


def test_cf_copy_keeps_stock_extent_return_and_explicit_halfword_order(cf):
    r = cf_run(cf, [ack(234)])[-1]
    expected = b"".join(
        struct.pack(">H", (i * 71 + 0x8001) & 65535) for i in range(0xABC // 2)
    )
    assert r.reads[3][:0xABC] == expected
    assert r.reads[3][0xABC:] == bytes([0xA5]) * 16
    assert cf_words(r, 1)[7] == cf.symbols["lp_fake_copy"]
    assert cf_words(r, 2)[1:] == (1, 2, 0, 234, 958464)


def test_restore_bases_precedes_indices_for_real_hardware():
    # PRM circular-buffer side effect is absent from the interpreter (finding49).
    # Verify the ordering that prevents a B restore overwriting a restored I.
    source = (MOD / "src/dsp/lp0-proof.s").read_text()
    restore = source.split("_lp_restore:", 1)[1].split("JUMP _lp_back;", 1)[0]
    for i in range(8):
        assert restore.index(f"B{i} = R0;") < restore.rindex(f"I{i} = R0;")


def test_dsp_reply_extension_keeps_stock_bytes_and_updates_every_tail(dsp):
    sess, syms = fresh(dsp)
    reply = 0x20050000
    for i in range((0xABC + 16) // 4):
        sess.poke(reply // 4 + i, 0xCAFE1234)
    init(sess, {"I4": reply})
    assert [sess.peek(reply // 4 + i) for i in range(0xABC // 4)] == [0xCAFE1234] * (
        0xABC // 4
    )
    assert [sess.peek((reply + 0xABC) // 4 + i) for i in range(4)] == [
        0x00014C50,
        2,
        0,
        0,
    ]
    complete(sess, syms, 0)
    init(sess, {"I4": reply, "M6": 9, "L4": 17, "MODE1": 0x210000})
    assert [sess.peek((reply + 0xABC) // 4 + i) for i in range(4)] == [
        0x00014C50,
        2,
        1,
        4096,
    ]


@pytest.mark.parametrize("wire", [ack(1, count=4095), ack(235, status=2)])
def test_cf_invalid_postack_never_releases_or_resends(cf, wire):
    start = 234 if wire[1][4] == 235 else 0
    r = cf_run(
        cf,
        [
            ("lp_test_prime", [start]),
            ack(start),
            ("lp_test_stage", [start]),
            ("lp_tick", []),
            wire,
            ("lp_tick", []),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[-1], 0)[0] == 4
    assert cf_words(r[-1], 1)[0] == 1


def test_cf_old_presend_ack_waits_then_exact_ack_permits_single_send(cf):
    r = cf_run(
        cf,
        [
            ("lp_test_prime", [1]),
            ack(0),
            ("lp_test_stage", [1]),
            ("lp_tick", []),
            ack(1),
            ("lp_tick", []),
            ("lp_tick", []),
            ack(2),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[3], 0) == (1, 0, 1, 1)
    assert cf_words(r[3], 1)[0] == 0
    assert cf_words(r[5], 0) == (2, 0, 1, 2)
    assert cf_words(r[6], 1)[0] == 1
    assert cf_words(r[-1], 0) == (3, 0, 1, 2)


def control_wire(status=2, request_id=1, track=0, epoch=1, version=1):
    return (
        "lp_test_control_wire",
        [0x4241 | version << 16 | status << 24, request_id, track, epoch],
    )


def close_command(sess, syms, request_id=1, track=0, epoch=1):
    rx = val(sess, syms, "_lp_rx")
    for i, word in enumerate([0x01014243, request_id, track, epoch]):
        sess.poke(rx // 4 + i, word)
    keep = {
        **REGS,
        "MODE1": 0x19000,
        "R3": 0x7303,
        "R5": 0x7305,
        "R6": 0x7306,
        "R7": 0x7307,
        "I0": 0x20040000,
        "I1": 0x20041000,
        "I2": 0x20042000,
        "I3": 0x20043000,
        "I5": 0x20044000,
    }
    result = sess.call(
        "_test_callback",
        {**keep, "R4": syms["_lp_status"], "R8": 4, "R12": rx},
        max_steps=30000,
        read_regs=tuple(keep),
    )
    assert result["halt"]["reason"] == "breakpoint"
    assert all(result["regs"][key] == value for key, value in keep.items())


def tail_reply(sess):
    reply = 0x20050000
    init(sess, {"I4": reply, "M6": 9, "L0": 17, "L4": 17, "MODE1": 0x219000})
    return [sess.peek((reply + 0xABC) // 4 + i) for i in range(4)]


def test_close_actual_reply_halves_cross_compiled_cf_without_payload_progress(dsp, cf):
    sess, syms = fresh(dsp)
    init(sess)
    sess.poke(DEST, 0x12349876)
    put(sess, syms, "_lp_close_a_count", 1)
    close_command(sess, syms)
    assert tail_reply(sess) == [0x01014241, 1, 0, 1]
    assert val(sess, syms, "_lp_close_a_closed") == 1
    assert val(sess, syms, "_lp_close_b_open") == 1
    assert val(sess, syms, "_lp_pages") == val(sess, syms, "_lp_bytes") == 0
    assert sess.peek(DEST) == 0x12349876
    put(sess, syms, "_lp_close_a_count", 0)
    wire = tail_reply(sess)
    assert wire == [0x02014241, 1, 0, 1]
    # Four DSP numeric words become eight low-half-first numeric wire halves.
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            ("lp_test_control_wire", wire),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[2], 4) == (2, 0, 1, 0, 1, 1)
    assert cf_words(r[-1], 4) == (3, 0, 1, 0, 1, 1)
    assert cf_words(r[-1], 5)[1:] == (1, 2, 1, 0, 1)
    assert cf_words(r[-1], 1)[:7] == (
        1,
        0x01014243,
        cf.symbols["lp_close_staging"],
        1,
        0,
        1,
        0,
    )


def test_close_pending_history_rejects_higher_and_retry_rechecks_epoch(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    close_command(sess, syms)
    assert tail_reply(sess)[0] == 0x02014241
    put(sess, syms, "_lp_close_b_count", 1)
    close_command(sess, syms, 2, 1)
    assert tail_reply(sess) == [0x01014241, 2, 1, 1]
    before = [
        val(sess, syms, name)
        for name in ("_lp_close_last_id", "_lp_close_last_drained", "_lp_close_a_epoch")
    ]
    close_command(sess, syms, 3, 0)
    assert tail_reply(sess) == [0x03014241, 3, 0, 1]
    assert [
        val(sess, syms, name)
        for name in ("_lp_close_last_id", "_lp_close_last_drained", "_lp_close_a_epoch")
    ] == before
    close_command(sess, syms, 2, 0)
    assert tail_reply(sess)[0] == 0x03014241
    put(sess, syms, "_lp_close_b_count", 0)
    assert tail_reply(sess) == [0x02014241, 2, 1, 1]
    put(sess, syms, "_lp_close_b_epoch", 2)
    put(sess, syms, "_lp_close_b_open", 1)
    close_command(sess, syms, 2, 1)
    assert tail_reply(sess) == [0x03014241, 2, 1, 1]
    assert val(sess, syms, "_lp_close_b_open") == 1


@pytest.mark.parametrize(
    "fields", [(0, 0, 1), (0x80000000, 0, 1), (1, 2, 1), (1, 0, 0), (1, 0, 0x80000000)]
)
def test_close_invalid_identity_preserves_controls_and_history(dsp, fields):
    sess, syms = fresh(dsp)
    init(sess)
    close_command(sess, syms, *fields)
    assert tail_reply(sess) == [0x03014241, *fields]
    assert val(sess, syms, "_lp_close_last_id") == 0
    assert (
        val(sess, syms, "_lp_close_a_open") == 0
        and val(sess, syms, "_lp_close_b_open") == 1
    )


def test_close_rearm_error_overrides_drained_and_tail_cannot_publish_early(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_test_submit_result", 4)
    close_command(sess, syms)
    assert tail_reply(sess) == [0x00014C50, 0x00030004, 0, 0]
    assert val(sess, syms, "_lp_close_rearming") == 1
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_lp_close_a_count", 1)
    close_command(sess, syms)
    put(sess, syms, "_lp_close_a_count", 0)
    put(sess, syms, "_lp_close_rearming", 1)
    assert tail_reply(sess)[0] == 0x00014C50
    assert val(sess, syms, "_lp_close_last_drained") == 0


def test_close_after_terminal_payload_remains_armed_and_fault_count_never_drains(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_lp_pages", 234)
    put(sess, syms, "_lp_bytes", 234 * 4096)
    complete(sess, syms, 234)
    put(sess, syms, "_lp_close_a_fault", 1)
    close_command(sess, syms)
    assert tail_reply(sess) == [0x01014241, 1, 0, 1]
    assert val(sess, syms, "_lp_status") == 3
    assert val(sess, syms, "_lp_pages") == 235
    assert val(sess, syms, "_lp_bytes") == 960000
    assert val(sess, syms, "_test_submit_calls") == 3


def test_cf_alternating_kinds_retain_separate_snapshots_and_send_once(cf):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            control_wire(1),
            ("lp_tick", []),
            ack(),
            ("lp_tick", []),
            control_wire(),
            ("lp_tick", []),
            control_wire(1),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[4], 4)[0] == cf_words(r[6], 4)[0] == 2
    assert cf_words(r[6], 5)[2] == 1
    assert cf_words(r[-1], 4)[0] == 3
    assert cf_words(r[-1], 2)[1:] == (1, 2, 0, 0, 0)
    assert cf_words(r[-1], 1)[0] == 1


@pytest.mark.parametrize("fields", [(2, 1, 1), (1, 0, 2), (2, 0, 1)])
def test_cf_close_unrelated_tuple_cannot_complete(cf, fields):
    request_id, track, epoch = fields
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            control_wire(2, request_id, track, epoch),
            ("lp_tick", []),
            control_wire(),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[4], 4)[0] == 2
    assert cf_words(r[-1], 4)[0] == 3


@pytest.mark.parametrize(
    "wire",
    [
        control_wire(3),
        control_wire(4),
        control_wire(version=2),
        ("lp_test_control_wire", [0x02014240, 1, 0, 1]),
    ],
)
def test_cf_close_matching_bad_ack_quarantines_both_and_late_ack_cannot_recover(
    cf, wire
):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            wire,
            ("lp_tick", []),
            control_wire(),
            ("lp_test_close", [2, 1, 1]),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[4], 4)[0] == cf_words(r[-1], 4)[0] == 4
    assert cf_words(r[-1], 0)[0] == 4
    assert cf_words(r[-1], 1)[0] == 1


def test_cf_close_timeout_and_odd_snapshot_are_bounded_and_fail_closed(cf):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            control_wire(),
            ("lp_test_close_odd", []),
            ("lp_tick", []),
            ("lp_test_timeout", []),
            ("lp_tick", []),
            control_wire(),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[5], 4)[0] == 2
    assert cf_words(r[-1], 4)[:2] == (4, 6)
    assert cf_words(r[-1], 1)[0] == 1


def test_cf_serializes_both_queued_and_existing_active_data(cf):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_stage", [0]),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            ("lp_tick", []),
            control_wire(),
            ("lp_tick", []),
            ("lp_tick", []),
            ack(1),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[3], 0)[0] == 1
    assert cf_words(r[3], 1)[1] == 0x01014243
    assert cf_words(r[7], 1)[0] == 2
    assert cf_words(r[-1], 0)[0] == 3
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_stage", [0]),
            ("lp_tick", []),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            ack(1),
            ("lp_tick", []),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[4], 1)[0] == 1
    assert cf_words(r[7], 1)[0] == 2
    assert cf_words(r[7], 4)[0] == 2


def test_global_driver_error_wins_even_after_control_reply(cf):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            control_wire(),
            ack(status=4, error=3),
            control_wire(),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[-1], 0)[0] == cf_words(r[-1], 4)[0] == 4


def test_close_maximum_id_epoch_pack_exactly_and_cannot_wrap(dsp, cf):
    sess, syms = fresh(dsp)
    init(sess)
    put(sess, syms, "_lp_close_a_epoch", 0x7FFFFFFF)
    close_command(sess, syms, 0x7FFFFFFF, 0, 0x7FFFFFFF)
    wire = tail_reply(sess)
    assert wire == [0x02014241, 0x7FFFFFFF, 0, 0x7FFFFFFF]
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [0x7FFFFFFF, 0, 0x7FFFFFFF]),
            ("lp_tick", []),
            ("lp_test_control_wire", wire),
            ("lp_tick", []),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[4], 4)[0] == 3
    assert cf_words(r[-1], 4)[0] == 4
    assert cf_words(r[-1], 1)[0] == 1


@pytest.mark.parametrize(
    "bad",
    [
        control_wire(3),
        control_wire(4),
        control_wire(version=2),
        ("lp_test_control_wire", [0x02014240, 1, 0, 1]),
    ],
)
def test_cf_matching_bad_frame_is_sticky_before_ui_consumes_later_drained(cf, bad):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            bad,
            control_wire(),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[-1], 4)[:2] == (4, 7)
    assert cf_words(r[-1], 0)[0] == 4
    assert cf_words(r[-1], 1)[0] == 1


def test_cf_unrelated_malformed_frame_and_postclaim_rejection_do_not_poison_success(cf):
    r = cf_run(
        cf,
        [
            ack(),
            ("lp_test_close", [1, 0, 1]),
            ("lp_tick", []),
            control_wire(4, request_id=2),
            control_wire(),
            ("lp_tick", []),
            control_wire(3),
            ("lp_tick", []),
        ],
    )
    assert cf_words(r[5], 4)[0] == cf_words(r[-1], 4)[0] == 3
    assert cf_words(r[-1], 1)[0] == 1


# These consumers are the module's real callbacks, linked alongside the actual
# LP0 callback and SDK kernel. No separately maintained reader model is used.
VOICE_INPUT = 0x08034000
VOICE_STATE = 0x08035000
VOICE_OUT = 0x08036000
VOICE_PARAMS = 0x08037000


def voice(sess, action, track, *, state=None, mode=0x10000, reverse=False):
    state = VOICE_STATE + track * 32 if state is None else state
    inp = VOICE_INPUT + track * 16
    sess.poke(VOICE_PARAMS, 256 if reverse else 0)

    sess.poke(inp + 3, VOICE_OUT + track * 64)
    sess.poke(inp + 4, track)
    keep = {**REGS, "MODE1": mode, "I12": 0xB8AB00, "I13": BACK}
    result = sess.call(
        f"_lp_play_{action}",
        {**keep, "R4": state, "R8": inp, "R12": VOICE_PARAMS},
        max_steps=3000,
        read_regs=tuple(keep),
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert all(result["regs"][name] == value for name, value in keep.items())
    return result["regs"]["R0"]


def ready_a(sess, syms):
    # Use all 235 actual callbacks; the legacy transport helper preserves its
    # unique payload words while this test validates publication sequencing.
    for tag in range(235):
        assert val(sess, syms, "_lp_close_a_ready") == 0
        complete(sess, syms, tag)
    assert val(sess, syms, "_lp_close_a_ready") == 1
    assert val(sess, syms, "_lp_reader_a_source") == DEST
    assert val(sess, syms, "_lp_reader_a_frames") == 480000


@pytest.mark.parametrize("mode", [0x10000, 0x11000, 0x8000, 0x9000])
def test_actual_readers_unready_a_and_exact_independent_b_pcm(dsp, mode):
    sess, syms = fresh(dsp)
    init(sess)
    assert voice(sess, "note_on", 0, mode=mode) == 1
    assert voice(sess, "render", 0, mode=mode) == 1
    assert [sess.peek(VOICE_OUT + i) for i in range(32)] == [0] * 32
    assert voice(sess, "note_on", 1, mode=mode) == 0
    source = val(sess, syms, "_lp_reader_b_source")
    sess.record_reads(source, 4096)
    assert voice(sess, "render", 1, mode=mode) == 0
    assert sess.stores
    expected = [
        struct.unpack("<I", struct.pack("<f", -0.25 + i / 512))[0] for i in range(32)
    ]
    assert [sess.peek(VOICE_OUT + 64 + i) for i in range(32)] == expected
    assert val(sess, syms, "_lp_close_b_count") == 0
    assert val(sess, syms, "_lp_reader_b_reads") == 1
    assert val(sess, syms, "_lp_reader_a_reads") == 0
    lower, upper = [val(sess, syms, name) for name in ("_lp_b_lower", "_lp_b_upper")]
    assert lower + 16 == source
    assert source + 4096 == upper
    assert lower == syms["_lp_b_storage"] // 4
    assert [sess.peek(lower + i) for i in range(16)] == [CANARY] * 16
    assert [sess.peek(upper + i) for i in range(16)] == [CANARY] * 16


def test_actual_a_full_transfer_publishes_once_and_reads_exact_pcm(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    ready_a(sess, syms)
    # First packed transport words are 0x12340000, 0x12340001, ...
    assert voice(sess, "note_on", 0) == 0
    assert voice(sess, "render", 0) == 0
    samples = [i // 2 if i % 2 == 0 else 0x1234 for i in range(32)]
    assert [sess.peek(VOICE_OUT + i) for i in range(32)] == [
        struct.unpack("<I", struct.pack("<f", n / 32768))[0] for n in samples
    ]
    assert val(sess, syms, "_lp_close_a_count") == 0
    close_command(sess, syms)
    assert voice(sess, "render", 0) == 1
    assert [sess.peek(VOICE_OUT + i) for i in range(32)] == [0] * 32
    assert val(sess, syms, "_lp_reader_a_reads") == 1


@pytest.mark.parametrize("phase", [0, 42, 234])
def test_actual_early_close_permanently_prevents_publication(dsp, phase):
    sess, syms = fresh(dsp)
    init(sess)
    for tag in range(phase):
        complete(sess, syms, tag)
    close_command(sess, syms)
    for tag in range(phase, 235):
        complete(sess, syms, tag)
    assert val(sess, syms, "_lp_close_a_closed") == 1
    assert (
        val(sess, syms, "_lp_close_a_ready") == val(sess, syms, "_lp_close_a_open") == 0
    )
    assert val(sess, syms, "_lp_reader_a_source") == 0
    assert voice(sess, "note_on", 0) == 1


def test_actual_terminal_rearm_failure_never_publishes(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    for tag in range(234):
        complete(sess, syms, tag)
    put(sess, syms, "_test_submit_result", 4)
    complete(sess, syms, 234)
    assert val(sess, syms, "_lp_status") == 4
    assert (
        val(sess, syms, "_lp_close_a_ready")
        == val(sess, syms, "_lp_reader_a_source")
        == 0
    )


@pytest.mark.parametrize("bad", ["epoch", "owner", "ready", "count"])
def test_actual_denied_reader_never_reads_pcm(dsp, bad):
    sess, syms = fresh(dsp)
    init(sess)
    voice(sess, "note_on", 1)
    if bad == "epoch":
        sess.poke(VOICE_STATE + 32 + 6, 2)
    elif bad == "owner":
        sess.poke(VOICE_STATE + 32 + 8, 0)
    elif bad == "ready":
        put(sess, syms, "_lp_close_b_ready", 0)
    else:
        put(sess, syms, "_lp_close_b_count", 0xFFFFFFFF)
    sess.record_reads(val(sess, syms, "_lp_reader_b_source"), 4096)
    assert voice(sess, "render", 1) == 1
    assert not sess.stores
    assert [sess.peek(VOICE_OUT + 64 + i) for i in range(32)] == [0] * 32
    if bad == "count":
        assert val(sess, syms, "_lp_close_b_count") == 0x7FFFFFFF
        assert val(sess, syms, "_lp_close_b_fault") == 1


def test_actual_paused_a_reader_keeps_close_pending_while_b_reads(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    ready_a(sess, syms)
    voice(sess, "note_on", 0)
    voice(sess, "note_on", 1)
    pause = sess.symbols["_dk_sample_pcm16_neighbor"]
    sess.runner.breakpoints = frozenset((BACK, pause))
    consumer_stack = STACK - 0x2000
    consumer_regs = {
        **REGS,
        "I6": consumer_stack,
        "I7": consumer_stack,
        "I12": 0xB8AB00,
        "I13": BACK,
    }
    result = sess.call(
        "_lp_play_render",
        {**consumer_regs, "R4": VOICE_STATE, "R8": VOICE_INPUT},
    )
    assert result["final_pc_sw"] == pause
    saved = sess.runner.state
    frame_start = consumer_stack // 4 - 40
    frame = [sess.peek(frame_start + i) for i in range(40)]
    assert sum(value is not None for value in frame) >= 14
    assert val(sess, syms, "_lp_close_a_count") == 1
    sess.runner.breakpoints = frozenset((BACK,))
    close_command(sess, syms, 1, 0)
    assert val(sess, syms, "_lp_close_last_drained") == 0
    assert tail_reply(sess)[0] == 0x01014241
    assert voice(sess, "render", 1) == 0
    assert val(sess, syms, "_lp_reader_b_reads") == 1
    assert [sess.peek(frame_start + i) for i in range(40)] == frame
    sess.runner.state = saved
    sess.runner.set_watch_active(False)
    assert sess.runner.run(3000).halt.reason == "breakpoint"
    for name, expected in consumer_regs.items():
        assert sess.runner.state.uregs[sess._st._seed_code(name)].value == expected
    assert val(sess, syms, "_lp_close_a_count") == 0
    assert tail_reply(sess) == [0x02014241, 1, 0, 1]
    assert voice(sess, "render", 0) == 1


@pytest.mark.parametrize("reverse", [False, True])
def test_actual_b_reverse_and_end_clamping_read_only_owned_pcm(dsp, reverse):
    sess, syms = fresh(dsp)
    init(sess)
    assert voice(sess, "note_on", 1, reverse=reverse) == 0
    if not reverse:
        # At the last frame, interpolation clamps its neighbor to that frame.
        sess.poke(VOICE_STATE + 32 + 2, 8191)
    assert voice(sess, "render", 1) == 0
    values = [-0.125 + (i / 512 if reverse else 0) for i in range(32)]
    assert [sess.peek(VOICE_OUT + 64 + i) for i in range(32)] == [
        struct.unpack("<I", struct.pack("<f", value))[0] for value in values
    ]
    # Crossing either edge finishes and all subsequent output is zero.
    sess.poke(VOICE_STATE + 32 + 2, 0xFFFFFFFF if reverse else 8192)
    sess.record_reads(val(sess, syms, "_lp_reader_b_source"), 4096)
    assert voice(sess, "render", 1) == 1
    assert not sess.stores
    assert [sess.peek(VOICE_OUT + 64 + i) for i in range(32)] == [0] * 32


def test_actual_unbound_track_finishes_without_touching_b(dsp):
    sess, syms = fresh(dsp)
    init(sess)
    assert voice(sess, "note_on", 2) == 1
    sess.record_reads(val(sess, syms, "_lp_reader_b_source"), 4096)
    assert voice(sess, "render", 2) == 1
    assert not sess.stores
    assert [sess.peek(VOICE_OUT + 128 + i) for i in range(32)] == [0] * 32


@pytest.mark.parametrize("track", [0, 1])
def test_actual_callback_snapshot_survives_downstream_output_overwrite(dsp, track):
    sess, syms = fresh(dsp)
    init(sess)
    name = "ab"[track]
    sequence = f"_lp_reader_{name}_snapshot_seq"
    address = syms[f"_lp_reader_{name}_snapshot"] // 4
    assert val(sess, syms, sequence) == 0  # Distinct from an emitted silent block.
    if track == 0:
        # No payload has published: the denied callback itself emits silence.
        assert voice(sess, "note_on", track) == 1
        assert voice(sess, "render", track) == 1
    else:
        assert voice(sess, "note_on", track) == 0
        assert voice(sess, "render", track) == 0
    emitted = [sess.peek(VOICE_OUT + track * 64 + i) for i in range(32)]
    assert [sess.peek(address + i) for i in range(32)] == emitted
    assert val(sess, syms, sequence) == 2
    assert bool(any(emitted)) == (track == 1)
    for i in range(32):
        sess.poke(VOICE_OUT + track * 64 + i, 0x3F000000)
    assert [sess.peek(address + i) for i in range(32)] == emitted
    assert val(sess, syms, sequence) == 2
    if track == 1:
        # CLOSE turns a previously active raw snapshot into a fresh zero snapshot.
        close_command(sess, syms, 1, 1)
    assert voice(sess, "render", track) == 1
    assert val(sess, syms, sequence) == 4
    assert [sess.peek(address + i) for i in range(32)] == [0] * 32
    assert val(sess, syms, f"_lp_close_{name}_count") == 0


def test_boot_preserves_live_startup_context_and_stock_cfg_load(dsp):
    sess, syms = fresh(dsp)
    regs = {f"R{i}": 0x7100 + i for i in range(16)}
    regs.update({f"I{i}": 0x20060000 + 16 * i for i in range(16) if i not in (6, 7)})
    regs.update({f"B{i}": 0x20050000 + 16 * i for i in range(8)})
    regs.update({f"L{i}": 17 + i for i in range(4)})
    regs.update(
        {
            "MODE1": 0x10000,
            "I6": STACK + 256,
            "I7": STACK,
            "L6": 0x1000,
            "L7": 0x1000,
            "B6": STACK - 512,
            "B7": STACK - 512,
        }
    )
    boot(sess, regs)
    assert val(sess, syms, "_lp_boot_status") == 1
    assert val(sess, syms, "_lp_boot_error") == 0
    assert val(sess, syms, "_test_cache_calls") == 1
    assert val(sess, syms, "_test_cache_start") == val(sess, syms, "_lp_dma_base")
    assert val(sess, syms, "_test_cache_end") + 1 == val(sess, syms, "_lp_dma_limit")
    assert val(sess, syms, "_test_cache_selector") == 0x604
    assert val(sess, syms, "_test_cache_mode") == 1


@pytest.mark.parametrize(
    "modifier,value",
    [("M5", 1), ("M6", 2), ("M7", 1), ("M14", 0), ("L4", 1), ("L5", 1)],
)
def test_boot_context_failure_resumes_stock_and_denies_lp0(dsp, modifier, value):
    sess, syms = fresh(dsp)
    boot(sess, {modifier: value})
    assert val(sess, syms, "_lp_boot_status") == 2
    assert val(sess, syms, "_lp_boot_error") == 0x100
    assert val(sess, syms, "_test_cache_calls") == 0
    init(sess)
    assert val(sess, syms, "_lp_status") == 4
    assert val(sess, syms, "_lp_error") == 8
    assert val(sess, syms, "_test_open_calls") == 0
    assert val(sess, syms, "_test_submit_calls") == 0
    assert val(sess, syms, "_lp_close_a_ready") == 0
    assert val(sess, syms, "_lp_close_b_ready") == 0


def test_boot_helper_error_fails_closed(dsp):
    sess, syms = fresh(dsp)
    put(sess, syms, "_test_cache_result", 3)
    boot(sess)
    assert val(sess, syms, "_lp_boot_status") == 2
    assert val(sess, syms, "_lp_boot_error") == 3
    init(sess)
    assert val(sess, syms, "_test_open_calls") == 0
    assert val(sess, syms, "_test_submit_calls") == 0
    assert val(sess, syms, "_lp_close_a_ready") == 0
    assert val(sess, syms, "_lp_close_b_ready") == 0


def test_boot_refuses_simd_before_saves_and_preserves_secondary_registers(dsp):
    sess, syms = fresh(dsp)
    regs = {**REGS, "MODE1": 0x210000}
    regs.update({f"R{i}": 0x7100 + i for i in range(16)})
    regs.update({f"S{i}": 0x8100 + i for i in range(16)})
    sess.record_stores()
    result = sess.call("_lp_boot_hook", regs, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert result["regs"]["MODE1"] == regs["MODE1"]
    for name, value in regs.items():
        if name not in ("R2", "S2"):
            assert result["regs"][name] == value
    assert result["regs"]["R2"] == 0x414141
    assert not sess.stores
    assert val(sess, syms, "_lp_boot_status") == 0
    assert val(sess, syms, "_test_cache_calls") == 0
    result = sess.call("_lp_hook", REGS, max_steps=30000)
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert val(sess, syms, "_lp_error") == 8
    assert val(sess, syms, "_test_open_calls") == 0
    assert val(sess, syms, "_test_submit_calls") == 0
    assert val(sess, syms, "_lp_close_a_ready") == 0
    assert val(sess, syms, "_lp_close_b_ready") == 0
