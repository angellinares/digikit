"""Actual two-track PCM16 consumers and private generation retirement.

Paused runners share only DM while retaining their register/call/loop state.
This is a scheduler interleaving model, not physical IRQ, DMA or cache evidence.
"""

import struct

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import pk_sharc, pk_toolchains

SOURCE = REPO_ROOT / "tests/fixtures/dsp/sample-bindings.s"
BACK = 0xB9F000
INPUT = 0x08034000
STATE = 0x08035000
OUT = 0x08036000
OLD_A = 0x10002000
B_PCM = 0xE0004000
NEW_A = 0x6000C000
STACK_A, STACK_B, STACK_PRODUCER = 0x2009C000, 0x2009A000, 0x20098000


@pytest.fixture(scope="module")
def dsp():
    if pk_toolchains.selache_dir() is None:
        pytest.skip("assembler absent")
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(
            SOURCE.read_text(), [REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk"]
        )
    )
    address, sections, initial = 0x20080000, {}, []
    for name, (data, align, _) in obj.data.items():
        address = (address + align - 1) & -align
        sections[name] = address
        initial.append((address, data))
        address += len(data)
    symbols = {
        name: sections[section] + offset
        for name, (section, offset, _) in obj.data_symbols.items()
    }
    linked = pk_sharc.link(obj, 0xB90000, symbols)
    return linked, symbols, initial


def fresh(dsp):
    linked, syms, initial = dsp
    sess = pk_sharc.CallSession([linked], BACK)
    for address, data in initial:
        for offset in range(0, len(data), 4):
            sess.poke(
                (address + offset) // 4,
                int.from_bytes(data[offset : offset + 4], "little"),
            )
    for track in (0, 1):
        sess.poke(INPUT + track * 16 + 3, OUT + track * 64)
        sess.poke(INPUT + track * 16 + 4, track)
        assert producer(sess, "publish", track, 0, 1) == 0
        assert (
            value(sess, syms, track, "source") == syms[f"_bind_{'ab'[track]}_pcm"] // 4
        )
    return sess, syms


def regs(mode=0x9000, stack=STACK_PRODUCER):
    return {
        "I6": stack,
        "I7": stack,
        "I12": 0xB8AB00,
        "I13": BACK,
        "M5": 0,
        "M6": 1,
        "M7": 0xFFFFFFFF,
        "M13": 0,
        "M14": 1,
        "M15": 0xFFFFFFFF,
        "MODE1": mode,
    }


def consumer_context(track, mode=0x9000):
    return {
        **regs(mode, STACK_A if track == 0 else STACK_B),
        **{f"R{i}": 0x7000 + track * 0x100 + i for i in (7, 9, 10, 11, 13, 14, 15)},
    }


def call(sess, entry, arguments, mode=0x9000, context=None):
    keep = regs(mode) if context is None else context
    result = sess.call(entry, {**keep, **arguments}, read_regs=tuple(keep))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    for name, expected in keep.items():
        assert result["regs"][name] == expected, name
    return result["regs"]["R0"]


def producer(sess, action, track, token, successor=0, mode=0x9000):
    return call(
        sess,
        f"_bind_{action}",
        {"R4": track, "R8": token, "R12": successor},
        mode,
    )


def consumer(sess, action, track, mode=0x9000):
    return call(
        sess,
        f"_bind_{action}",
        {"R4": STATE + track * 32, "R8": INPUT + track * 16, "R12": 0},
        mode,
        consumer_context(track, mode),
    )


def value(sess, syms, track, field):
    return sess.peek(syms[f"_bind_{'a' if track == 0 else 'b'}_{field}"] // 4)


def output(sess, track):
    return [sess.peek(OUT + track * 64 + i) for i in range(32)]


def expected(word):
    halves = [(word & 0xFFFF), word >> 16]
    values = [x - 65536 if x & 0x8000 else x for x in halves]
    return [
        struct.unpack("<I", struct.pack("<f", values[i & 1] / 32768))[0]
        for i in range(32)
    ]


@pytest.mark.parametrize("mode", [0x8000, 0x9000])
def test_both_tracks_read_exact_nonzero_pcm_with_per_callback_pins(dsp, mode):
    sess, syms = fresh(dsp)
    for track, pattern in [(0, OLD_A), (1, B_PCM)]:
        assert consumer(sess, "note_on", track, mode) == 0
        assert value(sess, syms, track, "count") == 0  # no lifetime/note-off pin
        assert sess.peek(STATE + track * 32 + 6) == 1
        assert sess.peek(STATE + track * 32 + 8) == track
        sess.record_reads(syms[f"_bind_{'ab'[track]}_pcm"] // 4, 64)
        assert consumer(sess, "render", track, mode) == 0
        assert sess.stores  # Actual kernel touched its owned PCM backing.
        assert output(sess, track) == expected(pattern)
        assert all(output(sess, track))
        assert value(sess, syms, track, "count") == 0
        assert value(sess, syms, 1 - track, "count") == 0


@pytest.mark.parametrize(
    "pause",
    ["_dk_sample_pcm16_loop", "_dk_sample_pcm16_neighbor", "_bind_a_after_reader"],
)
def test_paused_a_reader_blocks_reuse_while_b_keeps_rendering_exact_pcm(dsp, pause):
    linked, syms, _ = dsp
    sess, _ = fresh(dsp)
    consumer(sess, "note_on", 0)
    consumer(sess, "note_on", 1)
    sess.runner.breakpoints = frozenset((BACK, linked.symbols[pause]))
    r = sess.call(
        "_bind_render",
        {**consumer_context(0), "R4": STATE, "R8": INPUT, "R12": 0},
    )
    assert r["final_pc_sw"] == linked.symbols[pause]
    saved = sess.runner.state
    assert value(sess, syms, 0, "count") == 1
    frame_start = STACK_A // 4 - 40
    frame_before = [sess.peek(frame_start + i) for i in range(40)]
    assert sum(word is not None for word in frame_before) >= 14
    sess.runner.breakpoints = frozenset((BACK,))
    assert producer(sess, "close", 0, 1) == 2
    sess.poke(syms["_bind_a_payload"] // 4, NEW_A)
    sess.record_stores()
    assert producer(sess, "publish", 0, 1, 2) == 2
    assert [sess.peek(syms["_bind_a_pcm"] // 4 + i) for i in range(64)] == [OLD_A] * 64
    assert value(sess, syms, 0, "token") == 1
    assert value(sess, syms, 0, "ready") == 0
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)
    assert [sess.peek(frame_start + i) for i in range(40)] == frame_before
    assert value(sess, syms, 1, "ready") == 1
    assert value(sess, syms, 1, "token") == 1
    # Another A voice arriving during close/load cannot pin or read the old bank.
    sess.record_reads(syms["_bind_a_pcm"] // 4, 64)
    delayed = STATE + 128
    assert call(sess, "_bind_note_on", {"R4": delayed, "R8": INPUT, "R12": 0}) == 1
    assert not sess.stores
    sess.runner.state = saved
    sess.runner.set_watch_active(False)
    assert sess.runner.run(4000).halt.reason == "breakpoint"
    for name, expected_reg in consumer_context(0).items():
        assert sess.runner.state.uregs[sess._st._seed_code(name)].value == expected_reg
    assert output(sess, 0) == expected(OLD_A)
    assert value(sess, syms, 0, "count") == 0
    assert producer(sess, "close", 0, 1) == 0
    assert producer(sess, "publish", 0, 1, 2) == 0
    assert value(sess, syms, 0, "source") == syms["_bind_a_pcm"] // 4
    assert value(sess, syms, 0, "frames") == 128
    assert value(sess, syms, 0, "ready") == 1
    sess.record_reads(syms["_bind_a_pcm"] // 4, 64)
    assert consumer(sess, "render", 0) == 1  # cached token1 never adopts2
    assert not sess.stores
    assert output(sess, 0) == [0] * 32
    assert consumer(sess, "note_on", 0) == 0
    assert consumer(sess, "render", 0) == 0
    assert output(sess, 0) == expected(NEW_A)
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)


@pytest.mark.parametrize("track", [2, 16, 0xFFFFFFFF])
def test_invalid_and_mismatched_cached_tracks_cannot_use_another_binding(dsp, track):
    sess, syms = fresh(dsp)
    consumer(sess, "note_on", 0)
    consumer(sess, "note_on", 1)
    before = [
        value(sess, syms, 1, field) for field in ("open", "ready", "token", "count")
    ]
    sess.poke(INPUT + 4, track)
    sess.record_reads(syms["_bind_a_pcm"] // 4, 128)
    assert consumer(sess, "render", 0) == 1
    assert not sess.stores
    assert output(sess, 0) == [0] * 32
    assert consumer(sess, "note_on", 0) == 1
    assert not sess.stores
    assert producer(sess, "close", track, 1) == 0xFFFFFFFF
    assert producer(sess, "publish", track, 1, 2) == 0xFFFFFFFF
    assert [
        value(sess, syms, 1, field) for field in ("open", "ready", "token", "count")
    ] == before
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)
    # Equal independent tokens are not enough: cached track0/current track1 fails.
    sess.poke(INPUT + 4, 0)
    consumer(sess, "note_on", 0)
    sess.poke(INPUT + 4, 1)
    lo = min(syms.values()) // 4
    hi = max(address + len(data) for address, data in dsp[2]) // 4
    sess.record_reads(lo, hi - lo)  # Both PCM banks AND descriptor controls.
    assert consumer(sess, "render", 0) == 1
    assert not sess.stores
    assert value(sess, syms, 1, "count") == 0


@pytest.mark.parametrize("successor", [0, 1, 0x80000000, 0xFFFFFFFF])
def test_stale_close_and_invalid_successors_cannot_reopen_or_modify_backing(
    dsp, successor
):
    sess, syms = fresh(dsp)
    assert producer(sess, "close", 0, 0) == 0xFFFFFFFF
    assert value(sess, syms, 0, "ready") == 1
    assert producer(sess, "publish", 0, 1, 2) == 0xFFFFFFFF  # still open
    assert producer(sess, "close", 0, 1) == 0
    sess.poke(syms["_bind_a_payload"] // 4, NEW_A)
    assert producer(sess, "publish", 0, 1, successor) == 0xFFFFFFFF
    assert value(sess, syms, 0, "ready") == 0
    assert [sess.peek(syms["_bind_a_pcm"] // 4 + i) for i in range(64)] == [OLD_A] * 64
    assert producer(sess, "publish", 0, 0, 2) == 0xFFFFFFFF
    assert producer(sess, "publish", 0, 1, 2) == 0
    assert producer(sess, "close", 0, 1) == 0xFFFFFFFF
    assert value(sess, syms, 0, "ready") == 1
    assert value(sess, syms, 0, "token") == 2
    consumer(sess, "note_on", 1)
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)


def test_real_reader_release_fault_prevents_drain_and_publication_but_not_b_audio(dsp):
    linked, syms, _ = dsp
    sess, _ = fresh(dsp)
    consumer(sess, "note_on", 0)
    consumer(sess, "note_on", 1)
    sess.runner.breakpoints = frozenset((BACK, linked.symbols["_bind_a_after_reader"]))
    sess.call(
        "_bind_render", {**consumer_context(0), "R4": STATE, "R8": INPUT, "R12": 0}
    )
    saved = sess.runner.state
    sess.poke(syms["_bind_a_count"] // 4, 0)
    sess.runner.breakpoints = frozenset((BACK,))
    sess.runner.state = saved
    assert sess.runner.run(4000).halt.reason == "breakpoint"
    assert value(sess, syms, 0, "fault") == 1
    assert value(sess, syms, 0, "count") == 0x7FFFFFFF
    assert producer(sess, "close", 0, 1) == 2
    assert producer(sess, "publish", 0, 1, 2) == 2
    assert value(sess, syms, 0, "ready") == 0
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)


def test_note_descriptor_read_is_pinned_until_callback_release(dsp):
    linked, syms, _ = dsp
    sess, _ = fresh(dsp)
    sess.runner.breakpoints = frozenset((BACK, linked.symbols["_bind_a_note_acquired"]))
    result = sess.call(
        "_bind_note_on", {**consumer_context(0), "R4": STATE, "R8": INPUT, "R12": 0}
    )
    assert result["final_pc_sw"] == linked.symbols["_bind_a_note_acquired"]
    saved = sess.runner.state
    sess.runner.breakpoints = frozenset((BACK,))
    assert value(sess, syms, 0, "count") == 1
    assert producer(sess, "close", 0, 1) == 2
    assert producer(sess, "publish", 0, 1, 2) == 2
    consumer(sess, "note_on", 1)
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)
    sess.runner.state = saved
    assert sess.runner.run(1000).halt.reason == "breakpoint"
    assert value(sess, syms, 0, "count") == 0
    assert sess.peek(STATE + 7) == syms["_bind_a_pcm"] // 4
    assert sess.peek(STATE + 1) == 128
    # An inactive voice needs no note_off to allow the producer to drain.
    assert producer(sess, "close", 0, 1) == 0


def test_completed_payload_and_descriptor_precede_ready_and_tokens_never_wrap(dsp):
    linked, syms, _ = dsp
    sess, _ = fresh(dsp)
    assert producer(sess, "close", 0, 1) == 0
    sess.poke(syms["_bind_a_payload"] // 4, NEW_A)
    sess.runner.breakpoints = frozenset((BACK, linked.symbols["_bind_a_before_ready"]))
    result = sess.call("_bind_publish", {**regs(), "R4": 0, "R8": 1, "R12": 0x7FFFFFFF})
    assert result["final_pc_sw"] == linked.symbols["_bind_a_before_ready"]
    saved = sess.runner.state
    assert value(sess, syms, 0, "ready") == 0
    assert value(sess, syms, 0, "open") == 0
    assert value(sess, syms, 0, "token") == 0x7FFFFFFF
    assert value(sess, syms, 0, "source") == syms["_bind_a_pcm"] // 4
    assert value(sess, syms, 0, "frames") == 128
    assert [sess.peek(syms["_bind_a_pcm"] // 4 + i) for i in range(64)] == [NEW_A] * 64
    sess.runner.breakpoints = frozenset((BACK,))
    assert consumer(sess, "note_on", 0) == 1
    sess.runner.state = saved
    assert sess.runner.run(1000).halt.reason == "breakpoint"
    assert value(sess, syms, 0, "ready") == 1
    assert consumer(sess, "note_on", 0) == 0
    assert consumer(sess, "render", 0) == 0
    assert output(sess, 0) == expected(NEW_A)
    assert producer(sess, "close", 0, 0x7FFFFFFF) == 0
    for successor in [0, 1, 0x80000000, 0xFFFFFFFF]:
        assert producer(sess, "publish", 0, 0x7FFFFFFF, successor) == 0xFFFFFFFF
    assert value(sess, syms, 0, "token") == 0x7FFFFFFF
    assert value(sess, syms, 0, "ready") == 0
    consumer(sess, "note_on", 1)
    assert consumer(sess, "render", 1) == 0
    assert output(sess, 1) == expected(B_PCM)
