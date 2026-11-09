"""Compiled CF exact-tuple drain gating against the real assembled PCM reader.

The driver delivers complete immutable numeric messages, with no physical wire,
concurrent message publication, firmware transport, DMA or cache model.
"""

import os
import struct
from pathlib import Path

import pytest
import test_dsp_sample_bindings as readers
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit, pk_cfcall, pk_sharc, pk_toolchains

FIX = REPO_ROOT / "tests/fixtures/dsp"
COMMAND, REPLY = 0x08037000, 0x08037010
REQUEST = 0x01014243
ACK = 0x00014241
MAX_ID = 0x7FFFFFFF


@pytest.fixture(scope="module", autouse=True)
def prebuilt_tools():
    runner = Path(os.environ.get("DIGI_BIND_CLOSE_CF_CALL", str(pk_cfcall.EXAMPLE)))
    assert runner.is_file(), "prebuilt cf_call required; no implicit build"
    assert pk_toolchains.m68k_bin("gcc"), "ColdFire compiler required"
    patch = pytest.MonkeyPatch()
    patch.setattr(patchkit, "_BUILT", patchkit.binary(build=False))
    patch.setattr(pk_cfcall, "EXAMPLE", runner)
    patch.setattr(pk_cfcall, "_built", True)
    yield
    patch.undo()


@pytest.fixture(scope="module")
def dsp():
    assert pk_toolchains.selache_dir(), "assembler required"
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(
            (FIX / "sample-binding-close.s").read_text(),
            [FIX, REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk"],
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
    return pk_sharc.link(obj, 0xB90000, symbols), symbols, initial


@pytest.fixture(scope="module")
def cf():
    return patchkit.cf_link([FIX / "sample-binding-close.c"], 0x47E80000, {})


class CfConsumer:
    def __init__(self, linked):
        self.linked = linked
        self.memory = linked.image + bytes(linked.bss_size)
        self.command = (0, 0, 0, 0)
        self.outcome = 0
        self.last_id = 0

    def call(self, entry, *args):
        linked = self.linked
        result = pk_cfcall.CfMachine(config.stock("Digitone_II_OS1.12.syx")).run(
            {linked.base: self.memory},
            [
                {
                    "pc": linked.symbols[entry],
                    "args": args,
                    "max": 20000,
                    "reads": [
                        (linked.base, len(self.memory)),
                        (linked.symbols["close_command"], 16),
                        (linked.symbols["close_outcome"], 4),
                        (linked.symbols["close_last_id"], 4),
                    ],
                }
            ],
        )[0]
        assert result.ok and not result.clobbered, result
        self.memory = result.reads[0]
        self.command = struct.unpack(">4I", result.reads[1])
        self.outcome = struct.unpack(">I", result.reads[2])[0]
        self.last_id = struct.unpack(">I", result.reads[3])[0]
        return result.d0

    def begin(self, request_id=1, track=0, epoch=1):
        return self.call("close_begin", request_id, track, epoch)

    def observe(self, words):
        return self.call("close_test_observe", *words)


def deliver(sess, words):
    assert len(words) == 4
    for i, word in enumerate(words):
        sess.poke(COMMAND + i, word)
    context = {
        **readers.regs(),
        "I4": 0x08039000,
        **{f"R{i}": 0x7500 + i for i in (3, 5, 6, 7, 9)},
    }
    status = readers.call(
        sess, "_bind_control", {"R4": COMMAND, "R8": REPLY}, context=context
    )
    reply = tuple(sess.peek(REPLY + i) for i in range(4))
    assert reply == (ACK | status << 24, *words[1:])
    return reply


def ack(status, request_id=1, track=0, epoch=1):
    return (ACK | status << 24, request_id, track, epoch)


def test_compiled_cf_waits_for_paused_reader_then_reuses_only_a(dsp, cf):
    linked, syms, _ = dsp
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    for track in (0, 1):
        assert readers.consumer(sess, "note_on", track) == 0
    pause = linked.symbols["_dk_sample_pcm16_loop"]
    sess.runner.breakpoints = frozenset((readers.BACK, pause))
    result = sess.call(
        "_bind_render",
        {
            **readers.consumer_context(0),
            "R4": readers.STATE,
            "R8": readers.INPUT,
            "R12": 0,
        },
    )
    assert result["final_pc_sw"] == pause
    saved = sess.runner.state
    frame = readers.STACK_A // 4 - 40
    stack_before = [sess.peek(frame + i) for i in range(40)]
    assert sum(word is not None for word in stack_before) >= 14
    assert readers.value(sess, syms, 0, "count") == 1
    sess.runner.breakpoints = frozenset((readers.BACK,))
    assert host.begin() == 1
    request = host.command
    assert host.observe(deliver(sess, request)) == 1
    assert host.begin(2, 1, 1) == 0
    assert host.command == request
    sess.poke(syms["_bind_a_payload"] // 4, readers.NEW_A)
    assert readers.producer(sess, "publish", 0, 1, 2) == 2
    assert readers.consumer(sess, "render", 1) == 0
    assert readers.output(sess, 1) == readers.expected(readers.B_PCM)
    assert [sess.peek(frame + i) for i in range(40)] == stack_before
    sess.runner.state = saved
    sess.runner.set_watch_active(False)
    assert sess.runner.run(4000).halt.reason == "breakpoint"
    for name, value in readers.consumer_context(0).items():
        assert sess.runner.state.uregs[sess._st._seed_code(name)].value == value
    assert readers.output(sess, 0) == readers.expected(readers.OLD_A)
    assert host.observe(deliver(sess, request)) == 2
    assert host.outcome == 2
    assert readers.producer(sess, "publish", 0, 1, 2) == 0
    sess.record_reads(syms["_bind_a_pcm"] // 4, 64)
    assert readers.consumer(sess, "render", 0) == 1
    assert not sess.stores
    assert readers.consumer(sess, "note_on", 0) == 0
    assert readers.consumer(sess, "render", 0) == 0
    assert readers.output(sess, 0) == readers.expected(readers.NEW_A)
    assert readers.consumer(sess, "render", 1) == 0
    assert readers.output(sess, 1) == readers.expected(readers.B_PCM)
    # Old exact CLOSE cannot drain the newly published bank or alter CF terminal state.
    assert host.observe(deliver(sess, request)) == 2
    assert readers.value(sess, syms, 0, "ready") == 1
    assert host.begin(2, 0, 2) == 1
    assert host.observe(deliver(sess, host.command)) == 2


@pytest.mark.parametrize(
    "wrong", [ack(2, track=1), ack(2, request_id=2), ack(2, epoch=2)]
)
def test_unrelated_ack_cannot_authorize_reuse(dsp, cf, wrong):
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    assert host.begin() == 1
    # Obtain an actual DSP ACK, then model an unrelated complete snapshot.
    exact = deliver(sess, host.command)
    assert host.observe(wrong) == 1
    assert host.observe(exact) == 2


@pytest.mark.parametrize("header", [ACK ^ 1, ACK ^ (1 << 16), ACK | 4 << 24, ACK])
def test_matching_malformed_or_unknown_ack_quarantines(dsp, cf, header):
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    assert host.begin() == 1
    exact = deliver(sess, host.command)
    assert host.observe((header, 1, 0, 1)) == 3
    assert host.observe(exact) == 3
    assert host.begin(2) == 0


def test_timeout_quarantines_even_late_actual_drained_ack(dsp, cf):
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    host.begin()
    exact = deliver(sess, host.command)
    assert host.call("close_timeout") == 3
    assert host.observe(exact) == 3
    assert host.begin(2) == 0


def test_dsp_pending_identity_cannot_be_replaced_or_advanced_by_invalid_tuple(dsp, cf):
    sess, syms = readers.fresh(dsp)
    host = CfConsumer(cf)
    host.begin(10)
    sess.poke(syms["_bind_a_count"] // 4, 1)
    assert host.observe(deliver(sess, host.command)) == 1
    for words in [
        (REQUEST, 9, 0, 1),
        (REQUEST, 10, 1, 1),
        (REQUEST, 10, 0, 2),
        (REQUEST, 11, 1, 1),
    ]:
        assert deliver(sess, words) == ack(3, *words[1:])
    assert readers.value(sess, syms, 1, "ready") == 1
    sess.poke(syms["_bind_a_count"] // 4, 0)
    assert host.observe(deliver(sess, host.command)) == 2
    # Invalid higher identity must not consume its ID or close B.
    assert deliver(sess, (REQUEST, 11, 1, 2)) == ack(3, 11, 1, 2)
    assert deliver(sess, (REQUEST, 11, 1, 1)) == ack(2, 11, 1, 1)
    assert deliver(sess, (REQUEST, 10, 0, 1)) == ack(3, 10, 0, 1)
    # A new pending operation must clear the previous operation's drained history.
    assert readers.producer(sess, "publish", 0, 1, 2) == 0
    sess.poke(syms["_bind_a_count"] // 4, 1)
    assert deliver(sess, (REQUEST, 12, 0, 2)) == ack(1, 12, 0, 2)
    assert deliver(sess, (REQUEST, 13, 1, 1)) == ack(3, 13, 1, 1)


@pytest.mark.parametrize(
    "words",
    [
        (REQUEST ^ 1, 1, 0, 1),
        (REQUEST ^ (1 << 16), 1, 0, 1),
        (REQUEST ^ (3 << 24), 1, 0, 1),
        (REQUEST, 0, 0, 1),
        (REQUEST, 0x80000000, 0, 1),
        (REQUEST, 1, 2, 1),
        (REQUEST, 1, 0, 0),
        (REQUEST, 1, 0, 0x80000000),
    ],
)
def test_invalid_dsp_message_never_closes_either_bank(dsp, words):
    sess, syms = readers.fresh(dsp)
    assert deliver(sess, words) == ack(3, *words[1:])
    for track in (0, 1):
        assert readers.value(sess, syms, track, "ready") == 1
    assert deliver(sess, (REQUEST, 1, 0, 1)) == ack(2)


def test_corrupt_reader_fault_never_drains(dsp, cf):
    sess, syms = readers.fresh(dsp)
    host = CfConsumer(cf)
    host.begin()
    sess.poke(syms["_bind_a_fault"] // 4, 1)
    assert host.observe(deliver(sess, host.command)) == 1
    assert host.outcome != 2
    assert readers.producer(sess, "publish", 0, 1, 2) == 2


def test_cf_and_dsp_ids_exhaust_without_wrap_or_terminal_ack_regression(dsp, cf):
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    for request_id, track, epoch in [
        (0, 0, 1),
        (1, 2, 1),
        (1, 0, 0),
        (0x80000000, 0, 1),
    ]:
        assert host.begin(request_id, track, epoch) == 0
    assert host.begin(MAX_ID) == 1
    assert host.observe(deliver(sess, host.command)) == 2
    assert host.observe(ack(1, MAX_ID)) == 2
    for request_id in (0, 1, MAX_ID, 0x80000000, 0xFFFFFFFF):
        assert host.begin(request_id) == 0
    assert deliver(sess, (REQUEST, 1, 1, 1)) == ack(3, 1, 1, 1)


def test_new_pending_b_request_excludes_third_request_after_a_drained(dsp, cf):
    linked, syms, _ = dsp
    sess, _ = readers.fresh(dsp)
    host = CfConsumer(cf)
    assert host.begin() == 1
    assert host.observe(deliver(sess, host.command)) == 2
    assert readers.consumer(sess, "note_on", 1) == 0
    pause = linked.symbols["_dk_sample_pcm16_neighbor"]
    sess.runner.breakpoints = frozenset((readers.BACK, pause))
    result = sess.call(
        "_bind_render",
        {
            **readers.consumer_context(1),
            "R4": readers.STATE + 32,
            "R8": readers.INPUT + 16,
            "R12": 0,
        },
    )
    assert result["final_pc_sw"] == pause
    saved = sess.runner.state
    sess.runner.breakpoints = frozenset((readers.BACK,))
    assert host.begin(2, 1, 1) == 1
    assert host.observe(deliver(sess, host.command)) == 1
    fields = [
        f"_bind_{track}_{field}"
        for track in "ab"
        for field in (
            "open",
            "closed",
            "count",
            "fault",
            "ready",
            "token",
            "source",
            "frames",
        )
    ] + [
        "_control_last_id",
        "_control_last_track",
        "_control_last_epoch",
        "_control_last_drained",
    ]
    before = [sess.peek(syms[name] // 4) for name in fields]
    assert deliver(sess, (REQUEST, 3, 0, 1)) == ack(3, 3, 0, 1)
    assert [sess.peek(syms[name] // 4) for name in fields] == before
    assert host.begin(3, 0, 1) == 0
    sess.runner.state = saved
    assert sess.runner.run(4000).halt.reason == "breakpoint"
    assert readers.output(sess, 1) == readers.expected(readers.B_PCM)
    assert host.observe(deliver(sess, host.command)) == 2


def test_actual_dsp_rejection_quarantines_cf_without_mutating_banks(dsp, cf):
    sess, syms = readers.fresh(dsp)
    host = CfConsumer(cf)
    assert host.begin(epoch=2) == 1
    assert host.observe(deliver(sess, host.command)) == 3
    assert host.begin(2) == 0
    assert all(readers.value(sess, syms, track, "ready") == 1 for track in (0, 1))
