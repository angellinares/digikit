"""Isolated DDR diagnostic: actual assembled receiver and ColdFire wrapper checks.

No daemon build or coupled run. The CF harness's normal firmware state is merely
CPU setup; every firmware function/data pointer used here is a stand-in.
"""

import os
import struct
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit, pk_sharc, pk_toolchains
from tools.patchkit.pk_cfcall import CfMachine

MOD = REPO_ROOT / "modules/core/adapters/dsp-ddr-proof"
MAGIC = 0xDDF4A601
TOKEN = 0x81234567
RING, RECORD, FRAME = 0x20040000, 0x20050000, 0x20060000
DEST = 0x10200000
BACK = 0xB9F000


def check(words):
    s1 = s2 = 0
    for word in words:
        s1 = (s1 + word) & 0xFFFFFFFF
        s2 = (s2 + s1) & 0xFFFFFFFF
    return s1 ^ s2


def packet(seq, op, offset=0, payload=(), token=TOKEN, count=None):
    count = len(payload) if count is None else count
    head = [4, MAGIC, token, seq, op, offset, count]
    return head + [check([*head, *payload])] + [0] * 24 + list(payload)


@pytest.fixture(scope="module")
def dsp():
    if pk_toolchains.selache_dir() is None:
        pytest.skip("assembler absent")
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(
            (MOD / "src/dsp/ddr-proof.s").read_text(), [REPO_ROOT / "sdk"]
        )
    )
    syms = {
        name: 0x20080000 + offset for name, (_, offset, _) in obj.data_symbols.items()
    }
    linked = pk_sharc.link(obj, 0xB90000, {**syms, "_dd_back": BACK})
    return linked, syms


def receiver(dsp):
    linked, syms = dsp
    sess = pk_sharc.CallSession([linked], BACK)
    for address in syms.values():
        sess.poke(address // 4, 0)
    sess.poke(FRAME // 4 - 3, RING)
    return sess, syms


def deliver(sess, words):
    for i, word in enumerate(words):
        sess.poke(RING // 4 + i, word)
    regs = {
        "I4": RECORD,
        "I6": FRAME,
        "I7": FRAME - 0x100,
        "R1": 0x1234,
        "R2": 0x5678,
        "M5": 7,
        "M6": 9,
        "L2": 17,
        "L3": 19,
        **{f"R{i}": 0x7000 + i for i in (0, 3, 4, 5, 6)},
    }
    r = sess.call("_dd_hook", regs, max_steps=30000, read_regs=tuple(regs))
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert all(r["regs"][name] == value for name, value in regs.items())
    return r


def value(sess, syms, name):
    return sess.peek(syms[f"_dd_{name}"] // 4)


def test_init_data_duplicate_and_stale_token(dsp):
    sess, syms = receiver(dsp)
    deliver(sess, packet(0, 1))
    assert value(sess, syms, "token") == TOKEN
    assert value(sess, syms, "ready") == 0
    for lo in (DEST - 1024, DEST + 240000):
        assert [sess.peek(lo + i) for i in range(1024)] == [0xA55A3CC3] * 1024
    data = [(i * 0x12571 + 0x8000FFFF) & 0xFFFFFFFF for i in range(640)]
    deliver(sess, packet(1, 2, payload=data))
    assert [sess.peek(DEST + i) for i in range(640)] == data
    # Poison the destination after acceptance. A duplicate must ACK, never rewrite.
    sess.poke(DEST, 0xEEDDCCBB)
    deliver(sess, packet(1, 2, payload=data))
    assert sess.peek(DEST) == 0xEEDDCCBB
    assert value(sess, syms, "words") == 640
    deliver(sess, packet(2, 2, 640, [99], token=TOKEN + 1))
    assert value(sess, syms, "token") == TOKEN
    assert value(sess, syms, "seq") == 1
    assert value(sess, syms, "words") == 640
    assert sess.peek(RECORD // 4 + 0x64 // 4) == TOKEN
    # INIT cannot replace the existing token or reset the canaries.
    sess.poke(DEST - 1, 77)
    deliver(sess, packet(0, 1, token=TOKEN + 1))
    assert sess.peek(DEST - 1) == 77


@pytest.mark.parametrize("bad", ["checksum", "range", "gap", "partial", "commit"])
def test_failed_transfer_stays_unready_without_payload_write(dsp, bad):
    sess, syms = receiver(dsp)
    deliver(sess, packet(0, 1))
    sess.poke(DEST, 0x44556677)
    words = packet(1, 2, payload=[123])
    if bad == "checksum":
        words[7] ^= 1
    elif bad == "range":
        words = packet(1, 2, 240000, [123])
    elif bad == "gap":
        words = packet(2, 2, payload=[123])
    elif bad == "partial":
        deliver(sess, packet(1, 2, payload=[0x44556677]))
        words = packet(2, 3, 1)
    else:
        words = packet(1, 3)
    deliver(sess, words)
    assert value(sess, syms, "status") >= 2
    assert value(sess, syms, "ready") == 0
    assert sess.peek(DEST) == 0x44556677
    deliver(sess, packet(1, 2, payload=[0xABCD]))
    assert sess.peek(DEST) == 0x44556677
    assert value(sess, syms, "ready") == 0


def test_commit_seals_until_explicit_open(dsp):
    sess, syms = receiver(dsp)
    deliver(sess, packet(0, 1))
    # Boundary setup isolates the final block and COMMIT; the coupled test owns
    # the complete 960000-byte transport and readback proof.
    sess.poke(syms["_dd_seq"] // 4, 374)
    sess.poke(syms["_dd_words"] // 4, 239360)
    data = [0x87654321] * 640
    deliver(sess, packet(375, 2, 239360, data))
    assert value(sess, syms, "words") == 240000
    assert value(sess, syms, "ready") == 0
    assert sess.peek(DEST + 239999) == data[-1]
    deliver(sess, packet(376, 3, 240000))
    assert value(sess, syms, "ready") == 0
    assert value(sess, syms, "admission_open") == 0
    assert value(sess, syms, "status") == 1
    assert sess.peek(RECORD // 4 + 24) == 0xA6DD | (376 << 16) | (1 << 25)
    deliver(sess, packet(376, 3, 240000))
    assert value(sess, syms, "status") == 1
    assert value(sess, syms, "ready") == 0
    deliver(sess, packet(378, 5))
    assert value(sess, syms, "ready") == 1
    assert value(sess, syms, "status") == 7
    deliver(sess, packet(378, 5))
    assert value(sess, syms, "status") == 7


@pytest.fixture(scope="module")
def cf(tmp_path_factory):
    if pk_toolchains.m68k_bin("gcc") is None:
        pytest.skip("ColdFire compiler absent")
    stub = tmp_path_factory.mktemp("ddr-proof") / "stock.c"
    stub.write_text("""unsigned int dd_capture[4], dd_clear_mask;
void dd_stock_send(int n, const unsigned short *s, int r, const volatile unsigned short *p)
{ dd_capture[0]=n; dd_capture[1]=(unsigned int)s;
  dd_capture[2]=r; dd_capture[3]=(unsigned int)p;
  if (dd_clear_mask) for (unsigned int i=17;i<=20;i++) ((unsigned short *)s)[i]=0;
}
extern unsigned short dd_frame[];
extern unsigned int dd_mail[];
void dd_test_request(const unsigned short *p)
{ for(unsigned int i=0;i<1344;i++) dd_frame[i]=p[i]; dd_mail[0]=4; }
void dd_test_ack(unsigned short *p, unsigned int token, unsigned int seq, unsigned int status)
{ p[48]=0xa6dd; p[49]=seq|(status<<9); p[50]=token; p[51]=token>>16; }
""")
    return patchkit.cf_link([MOD / "src/cf/ddr-proof.c", stub], 0x47E80000, {})


@pytest.mark.parametrize("normal_len", [0x802, 0xA80])
def test_cf_quiet_gate_ack_and_watchdog(cf, normal_len):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    state = os.environ.get("DIGI_DDR_PROOF_CF_STATE")
    machine = CfMachine(syx, state=Path(state) if state else None)
    src, rec = 0x47EC0000, 0x47ED0000
    frame = bytearray(0xA80)
    struct.pack_into(">H", frame, 0, 3)
    staging = b"".join(struct.pack(">HH", w & 65535, w >> 16) for w in packet(0, 1))
    mail, stage, capture = (
        cf.symbols[n] for n in ("dd_mail", "dd_frame", "dd_capture")
    )
    reads = [(mail, 16), (capture, 16)]

    def call(p=0):
        return {
            "pc": cf.symbols["dd_frame_hook"],
            "args": [normal_len, src, 0xABC, p],
            "reads": reads,
        }

    pokes = {
        cf.base: cf.image,
        cf.base + len(cf.image): bytes(cf.bss_size),
        src: bytes(frame),
        stage: staging,
        mail: struct.pack(">4I", 4, 0, 0, 0),
    }
    r = machine.run(pokes, [call(), call()])
    assert all(x.ok and not x.clobbered for x in r)
    assert struct.unpack(">4I", r[0].reads[1]) == (normal_len, src, 0xABC, 0)
    assert struct.unpack(">4I", r[1].reads[1]) == (0xA80, stage, 0xABC, 0)
    ack = bytearray(0xABC)
    struct.pack_into(">4H", ack, 0x60, 0xA6DD, 0, TOKEN & 65535, TOKEN >> 16)
    r = machine.run({**pokes, rec: bytes(ack)}, [call(), call(), call(rec)])
    assert struct.unpack(">4I", r[-1].reads[0])[0] == 2
    assert struct.unpack(">4I", r[-1].reads[1])[1] == src
    stale = bytearray(ack)
    struct.pack_into(">H", stale, 0x64, (TOKEN + 1) & 65535)
    r = machine.run({**pokes, rec: bytes(stale)}, [call(), call(), call(rec)])
    assert struct.unpack(">4I", r[-1].reads[0])[0] == 1
    # Each mask independently blocks substitution, including a one-frame pulse.
    for index in (17, 18, 19, 20):
        noisy = bytearray(frame)
        struct.pack_into(">H", noisy, index * 2, 1)
        pulse = machine.run(
            {
                **pokes,
                src: bytes(noisy),
                cf.symbols["dd_clear_mask"]: struct.pack(">I", 1),
            },
            [call(), call(), call()],
        )
        assert [struct.unpack(">4I", x.reads[1])[1] for x in pulse] == [src, src, stage]
        r = machine.run({**pokes, src: bytes(noisy)}, [call()] * 902)
        assert all(x.ok and not x.clobbered for x in r)
        assert struct.unpack(">4I", r[-1].reads[0])[:2] == (3, 100)
        assert all(struct.unpack(">4I", x.reads[1])[1] == src for x in r)


@pytest.mark.parametrize("normal_len", [0, 0x800, 0x801, 0x803, 0xA7E, 0xA82, 0xABC])
@pytest.mark.parametrize("held", [False, True])
def test_cf_unsupported_frame_lengths_never_substitute(cf, normal_len, held):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    state = os.environ.get("DIGI_DDR_PROOF_CF_STATE")
    machine = CfMachine(syx, state=Path(state) if state else None)
    src, rec = 0x47EC0000, 0x47ED0000
    normal = bytearray(0xABC)
    struct.pack_into(">H", normal, 0, 3)
    struct.pack_into(">H", normal, 34, int(held))
    # RETIRE is eligible for a held-note grant, but only on a supported frame.
    staging = b"".join(
        struct.pack(">HH", word & 65535, word >> 16) for word in packet(377, 4)
    )
    mail, stage, capture = (
        cf.symbols[name] for name in ("dd_mail", "dd_frame", "dd_capture")
    )
    results = machine.run(
        {
            cf.base: cf.image,
            cf.base + len(cf.image): bytes(cf.bss_size),
            src: bytes(normal),
            rec: bytes(0xABC),
            stage: staging,
            mail: struct.pack(">4I", 4, 0, 0, 0),
        },
        [
            {
                "pc": cf.symbols["dd_frame_hook"],
                "args": [normal_len, src, 0xABC, rec],
                "reads": [(mail, 16), (capture, 16)],
            }
        ]
        * 3,
    )
    assert all(result.ok and not result.clobbered for result in results)
    for index, result in enumerate(results):
        assert struct.unpack(">4I", result.reads[0]) == (1, 0, index + 1, 0)
        assert struct.unpack(">4I", result.reads[1]) == (normal_len, src, 0xABC, rec)


@pytest.mark.parametrize("kind", ["checksum", "range", "token", "operation"])
def test_malformed_init_poison_cannot_be_reset(dsp, kind):
    sess, syms = receiver(dsp)
    sess.poke(DEST - 1, 0x11223344)
    words = packet(0, 1)
    if kind == "checksum":
        words[7] ^= 1
    elif kind == "range":
        words = packet(0, 1, 240001)
    elif kind == "token":
        words = packet(0, 1, token=0)
    else:
        words = packet(0, 2)
    deliver(sess, words)
    assert value(sess, syms, "status") >= 2
    deliver(sess, packet(0, 1))
    assert value(sess, syms, "token") == 0
    assert value(sess, syms, "ready") == 0
    assert sess.peek(DEST - 1) == 0x11223344


def test_normal_observer_masks_pitch_but_proof_gate_requires_exact_word(dsp):
    sess, syms = receiver(dsp)
    sess.poke(DEST - 1, 0x11223344)
    for i, pitch in enumerate((0x3C00, 0x7F00), 1):
        deliver(sess, [(pitch << 16) | 3])
        assert value(sess, syms, "normal_frames") == i
    deliver(sess, [0x3C000001])
    assert value(sess, syms, "normal_frames") == 2
    forged = packet(0, 1)
    forged[0] |= 0x3C000000
    deliver(sess, forged)
    assert value(sess, syms, "token") == 0
    assert value(sess, syms, "ready") == 0
    assert sess.peek(DEST - 1) == 0x11223344
    deliver(sess, packet(0, 1))
    assert value(sess, syms, "token") == TOKEN


def test_tail_observer_records_active_marker_and_balanced_entries(dsp):
    sess, syms = receiver(dsp)
    sess.poke(syms["_dd_obs_active"] // 4, 2)
    deliver(sess, [3])
    assert value(sess, syms, "obs_tail_entries") == 1
    assert value(sess, syms, "obs_tail_exits") == 1
    assert value(sess, syms, "obs_tail_active") == 1
    assert value(sess, syms, "obs_active") == 2
    first = [sess.peek(syms["_dd_obs_tail_first"] // 4 + i) for i in range(6)]
    assert first[2:] == [FRAME, FRAME - 0x100, 0, 2]
    sess.poke(syms["_dd_obs_active"] // 4, 0)
    deliver(sess, [3])
    assert value(sess, syms, "obs_tail_entries") == 2
    assert value(sess, syms, "obs_tail_exits") == 2
    assert value(sess, syms, "obs_tail_active") == 1
    assert [sess.peek(syms["_dd_obs_tail_first"] // 4 + i) for i in range(6)] == first
    assert sess.peek(syms["_dd_obs_tail_last"] // 4 + 5) == 0


def committed(sess, syms):
    for name, value_ in (
        ("token", TOKEN),
        ("seq", 378),
        ("status", 7),
        ("ready", 1),
        ("admission_open", 1),
        ("words", 240000),
    ):
        sess.poke(syms[f"_dd_{name}"] // 4, value_)


@pytest.mark.parametrize("active", [0, 1, 16])
def test_retire_closes_once_and_duplicate_rechecks_exact_bank_drain(dsp, active):
    sess, syms = receiver(dsp)
    committed(sess, syms)
    sess.poke(syms["_dd_admitted"] // 4, active)
    sess.poke(DEST, 0x12345678)
    retire = packet(377, 4)
    deliver(sess, retire)
    assert value(sess, syms, "status") == (6 if active else 5)
    assert value(sess, syms, "ready") == 0
    assert value(sess, syms, "admission_open") == 0
    assert value(sess, syms, "retire_closed") == 1
    assert sess.peek(DEST) == 0x12345678
    sess.poke(syms["_dd_admitted"] // 4, 0)
    deliver(sess, retire)
    assert value(sess, syms, "status") == 5
    assert sess.peek(RECORD // 4 + 24) == 0xA6DD | (377 << 16) | (5 << 25)
    for late in (packet(1, 2, payload=[99]), packet(0, 1), packet(376, 3, 240000)):
        deliver(sess, late)
        assert sess.peek(DEST) == 0x12345678
        assert value(sess, syms, "retire_closed") == 1
        assert value(sess, syms, "ready") == 0
        assert value(sess, syms, "admission_open") == 0


@pytest.mark.parametrize(
    "kind", ["uncommitted", "token", "checksum", "sequence", "offset", "count"]
)
def test_invalid_retire_never_claims_retired(dsp, kind):
    sess, syms = receiver(dsp)
    committed(sess, syms)
    req = packet(377, 4)
    if kind == "uncommitted":
        sess.poke(syms["_dd_seq"] // 4, 375)
    elif kind == "token":
        req = packet(377, 4, token=TOKEN + 1)
    elif kind == "checksum":
        req[7] ^= 1
    elif kind == "sequence":
        req = packet(376, 4)
    elif kind == "offset":
        req = packet(377, 4, 1)
    else:
        req = packet(377, 4, payload=[1])
    sess.poke(DEST, 0x12345678)
    deliver(sess, req)
    assert value(sess, syms, "status") not in (5, 6)
    assert value(sess, syms, "retire_closed") == 0
    assert sess.peek(DEST) == 0x12345678


@pytest.fixture(scope="module")
def full_dsp():
    here = MOD / "src/dsp"
    text = (here / "ddr-proof.s").read_text() + "\n" + (here / "playback.s").read_text()
    text = pk_sharc.expand_includes(
        text, [here, REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk"]
    )
    obj = pk_sharc.assemble(text)
    syms = {
        name: 0x20080000 + offset for name, (_, offset, _) in obj.data_symbols.items()
    }
    linked = pk_sharc.link(obj, 0xB90000, {**syms, "_dd_back": BACK})
    return linked, syms


@pytest.mark.parametrize(
    "pause",
    ["_dk_sample_pcm16_loop", "_dk_sample_pcm16_neighbor", "_dd_play_after_linear"],
)
def test_actual_reader_interleaving_cannot_retire_until_guarded_release(
    full_dsp, pause
):
    # Deliberate scheduler model: preserve the paused runner's full register,
    # call/loop state while the actual hook shares only its DM overlay. This
    # is not a simulation or proof of the processor's physical IRQ dispatch.
    linked, syms = full_dsp
    sess, _ = receiver(full_dsp)
    committed(sess, syms)
    state, inputs, out, params = 0x08030000, 0x08030100, 0x08030200, 0x08030300
    regs = {
        "R4": state,
        "R8": inputs,
        "R12": params,
        "I6": 0x2009C000,
        "I7": 0x2009C000,
        "I13": BACK,
        "M5": 0,
        "M6": 1,
        "M7": 0xFFFFFFFF,
        "M13": 0,
        "M14": 1,
        "M15": 0xFFFFFFFF,
        "MODE1": 0x9000,
    }
    sess.poke(inputs + 3, out)
    sess.poke(params, 0)
    for i in range(32):
        sess.poke(DEST + i, 0x10002000)
    assert sess.call("_dd_play_note_on", regs)["halt"]["reason"] == "breakpoint"
    sess.runner.breakpoints = frozenset((BACK, linked.symbols[pause]))
    r = sess.call("_dd_play_render", regs)
    assert r["final_pc_sw"] == linked.symbols[pause]
    saved = sess.runner.state
    assert value(sess, syms, "admitted") == 1
    deliver(sess, packet(377, 4))
    assert value(sess, syms, "status") == 6
    assert value(sess, syms, "retire_closed") == 1
    assert value(sess, syms, "ready") == 0
    deliver(sess, packet(0, 1, token=TOKEN + 1))
    assert value(sess, syms, "token") == TOKEN
    assert value(sess, syms, "status") == 6
    assert [sess.peek(DEST + i) for i in range(32)] == [0x10002000] * 32
    sess.runner.state = saved
    sess.runner.breakpoints = frozenset((BACK,))
    assert sess.runner.run(3000).halt.reason == "breakpoint"
    assert value(sess, syms, "admitted") == 0
    assert sess.peek(state + 2) == 32
    deliver(sess, packet(377, 4))
    assert value(sess, syms, "status") == 5
    # Reuse is permitted only after the exact RETIRED ACK, then both cached
    # render and a delayed note fail admission before entering the source kernel.
    sess.poke(DEST, 0x7FFF7FFF)
    reads_before = value(sess, syms, "play_reads")
    sess.record_reads(DEST, 240000)
    for entry in ("_dd_play_render", "_dd_play_note_on", "_dd_play_render"):
        assert sess.call(entry, regs)["halt"]["reason"] == "breakpoint"
        assert not sess.stores
    assert value(sess, syms, "play_reads") == reads_before
    assert [sess.peek(out + i) for i in range(32)] == [0] * 32


def test_cf_retirement_pending_exact_ack_busy_context_and_watchdog(cf):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    state = os.environ.get("DIGI_DDR_PROOF_CF_STATE")
    machine = CfMachine(syx, state=Path(state) if state else None)
    src, rec = 0x47EC0000, 0x47ED0000
    frame = bytearray(0xA80)
    struct.pack_into(">H", frame, 0, 3)
    struct.pack_into(">H", frame, 34, 1)  # Held note: intentionally not quiet.
    staging = b"".join(struct.pack(">HH", w & 65535, w >> 16) for w in packet(377, 4))
    mail, stage, capture = (
        cf.symbols[n] for n in ("dd_mail", "dd_frame", "dd_capture")
    )
    reads = [(mail, 16), (capture, 16)]

    def call(length=0xA80, source=src, reclen=0xABC):
        return {
            "pc": cf.symbols["dd_frame_hook"],
            "args": [length, source, reclen, rec],
            "reads": reads,
        }

    def ack(status, seq=377, token=TOKEN):
        reply = bytearray(0xABC)
        word = 0xA6DD | (seq << 16) | (status << 25)
        struct.pack_into(
            ">4H", reply, 0x60, word & 65535, word >> 16, token & 65535, token >> 16
        )
        return bytes(reply)

    pokes = {
        cf.base: cf.image,
        cf.base + len(cf.image): bytes(cf.bss_size),
        src: bytes(frame),
        stage: staging,
        mail: struct.pack(">4I", 4, 0, 0, 0),
    }
    pending = machine.run({**pokes, rec: ack(6)}, [call()] * 3)
    assert all(r.ok and not r.clobbered for r in pending)
    assert all(struct.unpack(">4I", r.reads[0])[0] == 1 for r in pending)
    assert all(struct.unpack(">4I", r.reads[1])[1] == stage for r in pending)
    retired = machine.run({**pokes, rec: ack(5)}, [call()] * 2)
    assert struct.unpack(">4I", retired[-1].reads[0])[:2] == (2, 5)
    for stale in (ack(5, seq=376), ack(5, token=TOKEN + 1)):
        r = machine.run({**pokes, rec: stale}, [call()] * 2)
        assert struct.unpack(">4I", r[-1].reads[0])[0] == 1
    timeout = machine.run({**pokes, rec: ack(6)}, [call()] * 902)
    assert struct.unpack(">4I", timeout[-1].reads[0])[:2] == (3, 100)
    for malformed in (call(length=0), call(source=0), call(reclen=0)):
        r = machine.run({**pokes, rec: ack(6)}, [malformed])
        assert struct.unpack(">4I", r[0].reads[1])[1] == malformed["args"][1]
    non3 = bytearray(frame)
    struct.pack_into(">H", non3, 0, 1)
    r = machine.run({**pokes, src: bytes(non3), rec: ack(6)}, [call()])
    assert struct.unpack(">4I", r[0].reads[1])[1] == src
    bad = bytearray(staging)
    bad[28] ^= 1
    r = machine.run({**pokes, stage: bytes(bad), rec: ack(6)}, [call()])
    assert struct.unpack(">4I", r[0].reads[0])[:2] == (3, 101)
    assert struct.unpack(">4I", r[0].reads[1])[1] == src


def retired(sess, syms, token=TOKEN):
    committed(sess, syms)
    sess.poke(syms["_dd_token"] // 4, token)
    deliver(sess, packet(377, 4, token=token))
    assert value(sess, syms, "status") == 5


@pytest.mark.parametrize(
    "bad",
    [
        "skip",
        "old",
        "zero",
        "exhausted",
        "pending",
        "count",
        "fault",
        "checksum",
        "op",
        "offset",
    ],
)
def test_successor_rejection_preserves_bank_and_ack_identity(dsp, bad):
    sess, syms = receiver(dsp)
    token = 0xFFFFFFFF if bad == "exhausted" else TOKEN
    retired(sess, syms, token)
    next_token = (token + 1) & 0xFFFFFFFF
    if bad == "skip":
        next_token += 1
    elif bad == "old":
        next_token = token - 1
    elif bad == "zero":
        next_token = 0
    elif bad == "pending":
        sess.poke(syms["_dd_status"] // 4, 6)
    elif bad == "count":
        sess.poke(syms["_dd_admitted"] // 4, 1)
    elif bad == "fault":
        sess.poke(syms["_dd_admit_fault"] // 4, 1)
    req = packet(
        0, 2 if bad == "op" else 1, 1 if bad == "offset" else 0, token=next_token
    )
    if bad == "checksum":
        req[7] ^= 1
    before = {
        name: value(sess, syms, name)
        for name in (
            "token",
            "seq",
            "ackseq",
            "status",
            "ready",
            "words",
            "retire_closed",
            "admission_open",
        )
    }
    for addr in (DEST - 1, DEST, DEST + 240000):
        sess.poke(addr, 0x11223344)
    deliver(sess, req)
    assert {name: value(sess, syms, name) for name in before} == before
    assert [sess.peek(addr) for addr in (DEST - 1, DEST, DEST + 240000)] == [
        0x11223344
    ] * 3


def test_successor_adoption_sealed_skip_and_stale_controls(dsp):
    sess, syms = receiver(dsp)
    retired(sess, syms)
    for token in (TOKEN + 1, TOKEN + 2):
        deliver(sess, packet(0, 1, token=token))
        assert value(sess, syms, "token") == token
        assert value(sess, syms, "status") == 0
        assert value(sess, syms, "retire_closed") == 0
        assert value(sess, syms, "words") == 0
        assert value(sess, syms, "ready") == 0
        sess.poke(syms["_dd_seq"] // 4, 375)
        sess.poke(syms["_dd_words"] // 4, 240000)
        deliver(sess, packet(376, 3, 240000, token=token))
        assert value(sess, syms, "status") == 1
        assert value(sess, syms, "ready") == 0
        if token == TOKEN + 2:
            deliver(sess, packet(378, 5, token=token))
            assert value(sess, syms, "status") == 7
            assert value(sess, syms, "ready") == 1
        else:
            deliver(sess, packet(377, 4, token=token))
            assert value(sess, syms, "status") == 5
    sess.poke(DEST, 0x12345678)
    for old in (TOKEN, TOKEN + 1):
        for req in (
            packet(0, 1, token=old),
            packet(1, 2, payload=[99], token=old),
            packet(376, 3, 240000, token=old),
            packet(378, 5, token=old),
            packet(377, 4, token=old),
        ):
            deliver(sess, req)
            assert value(sess, syms, "token") == TOKEN + 2
            assert value(sess, syms, "ackseq") == 378
            assert value(sess, syms, "status") == 7
            assert sess.peek(DEST) == 0x12345678
    deliver(sess, packet(377, 4, token=TOKEN + 2))
    deliver(sess, packet(378, 5, token=TOKEN + 2))
    assert value(sess, syms, "status") == 5
    assert value(sess, syms, "ackseq") == 377
    assert value(sess, syms, "ready") == 0


@pytest.mark.parametrize("kind", ["early", "checksum", "seq", "offset", "count"])
def test_open_requires_exact_sealed_control(dsp, kind):
    sess, syms = receiver(dsp)
    deliver(sess, packet(0, 1))
    if kind != "early":
        sess.poke(syms["_dd_seq"] // 4, 375)
        sess.poke(syms["_dd_words"] // 4, 240000)
        deliver(sess, packet(376, 3, 240000))
    req = packet(
        379 if kind == "seq" else 378,
        5,
        1 if kind == "offset" else 0,
        [1] if kind == "count" else (),
    )
    if kind == "checksum":
        req[7] ^= 1
    deliver(sess, req)
    assert value(sess, syms, "status") in (2, 3, 4)
    assert value(sess, syms, "ready") == 0
    assert value(sess, syms, "admission_open") == 0


@pytest.mark.parametrize("normal_len", [0x802, 0xA80])
def test_cf_closed_bank_grants_require_exact_ack_and_expire_at_open(cf, normal_len):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    state = os.environ.get("DIGI_DDR_PROOF_CF_STATE")
    machine = CfMachine(syx, state=Path(state) if state else None)
    src, rec = 0x47EC0000, 0x47ED0000
    mail, stage, capture = (
        cf.symbols[n] for n in ("dd_mail", "dd_frame", "dd_capture")
    )
    held = bytearray(0xA80)
    struct.pack_into(">H", held, 0, 3)
    struct.pack_into(">H", held, 34, 1)
    pokes = {
        cf.base: cf.image,
        cf.base + len(cf.image): bytes(cf.bss_size),
        src: bytes(held),
        rec: bytes(0xABC),
    }
    reads = [(mail, 16), (capture, 16)]
    calls = []
    observations = []

    def submit(req):
        addr = 0x47EE0000 + len(pokes) * 0x1000
        raw = b"".join(struct.pack(">HH", w & 65535, w >> 16) for w in req)
        pokes[addr] = raw.ljust(2688, b"\0")
        calls.append({"pc": cf.symbols["dd_test_request"], "args": [addr]})

    def ack(token, seq, status):
        calls.append(
            {"pc": cf.symbols["dd_test_ack"], "args": [rec, token, seq, status]}
        )

    def hook(expected_state, expected_source, error=0):
        observations.append((len(calls), expected_state, expected_source, error))
        calls.append(
            {
                "pc": cf.symbols["dd_frame_hook"],
                "args": [normal_len, src, 0xABC, rec],
                "reads": reads,
            }
        )

    submit(packet(1, 2, payload=[1]))
    hook(1, src)  # Held DATA has no closed-bank grant, even with a plausible ACK.
    ack(TOKEN, 1, 0)
    hook(1, src)  # sent=0: an unsolicited ACK cannot grant or complete anything.
    submit(packet(377, 4))
    hook(1, stage)
    ack(TOKEN + 1, 377, 5)
    hook(1, stage)  # Wrong-token retirement ACK cannot grant successor INIT.
    submit(packet(0, 1, token=TOKEN + 1))
    hook(1, src)
    submit(packet(377, 4))
    hook(1, stage)
    ack(TOKEN, 377, 5)
    hook(2, src, 5)
    submit(packet(0, 1, token=TOKEN + 1))
    hook(1, stage)
    ack(TOKEN + 1, 0, 0)
    hook(2, src)
    submit(packet(1, 2, payload=[2], token=TOKEN + 1))
    hook(1, stage)
    ack(TOKEN + 1, 1, 0)
    hook(2, src)
    submit(packet(376, 3, 240000, token=TOKEN + 1))
    hook(1, stage)
    ack(TOKEN + 1, 376, 1)
    hook(2, src, 1)
    submit(packet(376, 3, 240000, token=TOKEN + 1))
    hook(1, stage)  # A sealed grant also permits a fresh duplicate COMMIT.
    hook(2, src, 1)
    submit(packet(378, 5, token=TOKEN + 1))
    hook(1, stage)
    ack(TOKEN + 1, 378, 7)
    hook(2, src, 7)
    submit(packet(378, 5, token=TOKEN + 1))
    hook(1, stage)  # Only the OPEN ACK's separate grant permits duplicate OPEN.
    hook(2, src, 7)
    submit(packet(1, 2, payload=[2], token=TOKEN + 1))
    hook(1, src)  # OPEN cleared all loading/closed grants.
    submit(packet(377, 4, token=TOKEN + 1))
    hook(1, stage)
    ack(TOKEN + 1, 377, 5)
    hook(2, src, 5)
    submit(packet(378, 5, token=TOKEN + 1))
    hook(1, src)  # RETIRE cleared the duplicate-OPEN grant; no resurrection.
    results = machine.run(pokes, calls)
    assert all(r.ok and not r.clobbered for r in results)
    for index, expected_state, expected_source, error in observations:
        result = results[index]
        assert struct.unpack(">4I", result.reads[0])[:2] == (expected_state, error)
        assert struct.unpack(">4I", result.reads[1])[1] == expected_source


@pytest.mark.parametrize(
    "op,seq,offset,status",
    [
        (1, 0, 0, 1),
        (2, 1, 0, 1),
        (3, 376, 240000, 0),
        (4, 377, 0, 1),
        (5, 378, 0, 1),
    ],
)
def test_cf_success_status_is_operation_specific(cf, op, seq, offset, status):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    machine = CfMachine(syx)
    src, rec = 0x47EC0000, 0x47ED0000
    mail, stage = (cf.symbols[n] for n in ("dd_mail", "dd_frame"))
    raw = b"".join(
        struct.pack(">HH", w & 65535, w >> 16)
        for w in packet(seq, op, offset, [1] if op == 2 else ())
    )
    normal = bytearray(0xA80)
    struct.pack_into(">H", normal, 0, 3)
    ack = bytearray(0xABC)
    struct.pack_into(
        ">4H", ack, 0x60, 0xA6DD, seq | (status << 9), TOKEN & 65535, TOKEN >> 16
    )
    calls = [
        {
            "pc": cf.symbols["dd_frame_hook"],
            "args": [0xA80, src, 0xABC, rec],
            "reads": [(mail, 16)],
        }
    ] * 3
    result = machine.run(
        {
            cf.base: cf.image,
            cf.base + len(cf.image): bytes(cf.bss_size),
            src: bytes(normal),
            rec: bytes(ack),
            stage: raw,
            mail: struct.pack(">4I", 4, 0, 0, 0),
        },
        calls,
    )
    assert all(r.ok and not r.clobbered for r in result)
    assert struct.unpack(">4I", result[-1].reads[0])[:2] == (3, 102)


@pytest.mark.parametrize("kind", ["checksum", "bounds", "open", "commit"])
def test_cf_rejects_malformed_staging_before_any_send(cf, kind):
    syx = config.stock("Digitakt_II_OS1.17.syx")
    if not syx.exists():
        pytest.skip("CF test setup firmware absent")
    machine = CfMachine(syx)
    src = 0x47EC0000
    mail, stage, capture = (
        cf.symbols[n] for n in ("dd_mail", "dd_frame", "dd_capture")
    )
    req = packet(1, 2, payload=[99])
    if kind == "checksum":
        req[7] ^= 1
    elif kind == "bounds":
        req = packet(1, 2, 240000, [99])
    elif kind == "open":
        req = packet(378, 5, payload=[99])
    else:
        req = packet(375, 3, 240000)
    raw = b"".join(struct.pack(">HH", w & 65535, w >> 16) for w in req)
    normal = bytearray(0xA80)
    struct.pack_into(">H", normal, 0, 3)
    result = machine.run(
        {
            cf.base: cf.image,
            cf.base + len(cf.image): bytes(cf.bss_size),
            src: bytes(normal),
            stage: raw,
            mail: struct.pack(">4I", 4, 0, 0, 0),
        },
        [
            {
                "pc": cf.symbols["dd_frame_hook"],
                "args": [0xA80, src, 0xABC, 0],
                "reads": [(mail, 16), (capture, 16)],
            }
        ]
        * 3,
    )
    assert all(r.ok and not r.clobbered for r in result)
    assert struct.unpack(">4I", result[-1].reads[0]) == (3, 101, 0, 0)
    assert all(struct.unpack(">4I", r.reads[1])[1] == src for r in result)
