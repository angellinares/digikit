"""Private per-frame exclusion experiment, not an SDK or interrupt policy.

Runtime original guard proof precedes the reader experiment. The unsupported
assembler (SW) store suffix is deliberately unused; no core semantics change.
"""

import copy
import hashlib
import json
import os
from collections import Counter
from dataclasses import replace
from pathlib import Path

import pytest
import test_dsp_sample_stereo_exclusion as whole_block
from support.paths import REPO_ROOT
from test_dsp_sample_stereo_exclusion import reg
from test_dsp_sample_stereo_exclusion import trace as whole_block_trace

from tools.patchkit import dsp_sample_ref as ref
from tools.patchkit import patchkit, pk_sharc
from tools.sharc_dev.sharc_core.addressing import (
    byte_to_normal_word,
    normal_word_to_byte,
)
from tools.sharc_dev.sharc_core.memory import _canonical_dm_address
from tools.sharc_dev.sharc_disasm import decode_loaded_at
from tools.sharc_dev.sharc_run import Watchpoint
from tools.sharc_dev.sharcldr import LoadedMemory

whole_block_dsp = whole_block.dsp

SOURCE = REPO_ROOT / "tests/fixtures/dsp/sample-stereo-frame-fetch.s"
IMAGE = REPO_ROOT / "out/sections/dt2-1.17/section_7_BLOB.bin"
# STACK is a byte-alias frame, as the adapters give it (finding 111)
BACK, STACK = 0xB9F000, 4 * 0x08038000
BASE, PLANE, FRAMES = 0x8422B9C8, 0x8422C000, 65
NW = byte_to_normal_word(PLANE)
REGS = {f"R{i}": 0x7000 + i for i in range(16)}
REGS.update({f"I{i}": 0x08036000 + 16 * i for i in range(16)})
REGS.update({f"L{i}": 0 for i in range(16)})
REGS.update(M5=0, M6=1, M7=0xFFFFFFFF, M14=1, I6=STACK, I7=STACK, MODE1=0x9000)


@pytest.fixture(scope="module", autouse=True)
def immutable_tools():
    patch = pytest.MonkeyPatch()
    binary = patchkit.binary(build=False)
    patch.setattr(patchkit, "_BUILT", binary)
    yield
    patch.undo()


@pytest.fixture(scope="module")
def dsp():
    if not IMAGE.exists():
        pytest.skip("requires original DT2 1.17 section7")
    data = IMAGE.read_bytes()
    assert (
        hashlib.sha256(data).hexdigest()
        == "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2"
    )
    memory = LoadedMemory.from_stream(data)
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(SOURCE.read_text(), [REPO_ROOT / "sdk"])
    )
    address, sections, initial = 0x20080000, {}, []
    for name, (payload, alignment, _) in obj.data.items():
        address = (address + alignment - 1) & -alignment
        sections[name] = address
        initial.append((address, payload))
        address += len(payload)
    symbols = {
        name: sections[section] + off
        for name, (section, off, _) in obj.data_symbols.items()
    }
    linked = pk_sharc.link(obj, 0xB90000, symbols | {"_ff_original_guard": 0x1C400F})
    return linked, memory, symbols, initial


def test_original_guard_exact_halfwords_alias_and_call_context(dsp):
    linked, memory, _, _ = dsp
    assert NW is not None and normal_word_to_byte(NW) == PLANE
    assert NW * 4 != PLANE
    session = pk_sharc.CallSession([linked], BACK, base_memory=memory)
    session.runner.state.stack_model = True
    session.runner.state.bank_model = True
    session.runner.state.assume_nw32 = True
    for i in range(36):
        session.poke(NW + i, (0x3141 + i) | ((0x9265 - i) << 16))
    original = b"".join(session.peek(NW + i).to_bytes(4, "little") for i in range(36))
    session.record_stores()
    session.runner.attach_watchpoints([Watchpoint(PLANE, PLANE + 144, stop=False)])
    result = session.call(
        "_ff_guard_h",
        REGS | {"R4": PLANE - BASE, "R8": FRAMES * 2},
        max_steps=100,
        read_regs=tuple(REGS),
    )
    assert result["halt"]["reason"] == "breakpoint", result
    reads = [(e.address, e.width) for e in session.stores if e.access == "read"]
    writes = [(e.address, e.width) for e in session.stores if e.access == "write"]
    if report := os.environ.get("STEREO_FRAME_FETCH_EVIDENCE"):
        events = Path(report).with_name("guard-events.json")
        events.parent.mkdir(parents=True, exist_ok=True)
        events.write_text(
            json.dumps([e.to_json() for e in session.stores], indent=2) + "\n"
        )
    # The memory watch layer exposes short reads as constituent byte reads.
    assert reads == [(PLANE + i, 1) for i in range(10)]
    assert (
        sorted(Counter(e.pc_sw for e in session.stores if e.access == "read").values())
        == [2] * 5
    )
    assert writes == [(PLANE + FRAMES * 2 + i, 2) for i in range(0, 10, 2)]
    after = b"".join(session.peek(NW + i).to_bytes(4, "little") for i in range(36))
    expected = bytearray(original)
    expected[FRAMES * 2 : FRAMES * 2 + 10] = original[:10]
    assert after == expected
    # First guard halfword is the unused high half of the last packed word.
    assert after[128:130] == original[128:130]
    assert after[130:132] == original[:2] != original[130:132]
    for name, value in REGS.items():
        if name not in ("R2", "R4", "R8", "I4", "I12"):
            assert result["regs"][name] == value, name


OUT, WRITER_STACK = 0x08036000, 0x08039000
RIGHT_PLANE = PLANE + 0x2000
LIMIT = 10000
KEEP = {
    name: value
    for name, value in REGS.items()
    if name not in ("R0", "R1", "R2", "R4", "R8", "R12", "I4", "I12")
}
METRICS = {}
CASES = [
    ref.Cursor(0, 65, 0, 32768, 65536),
    ref.Cursor(0, 65, 64, 12345, -65536),
    ref.Cursor(0, 65, 7, 65535, 0),
    ref.Cursor(1, 65, 2, 12345, 32769),
    ref.Cursor(1, 65, 64, 65535, -100003),
    ref.Cursor(0, 1, 0, 65535, 32768),
    ref.Cursor(1, 2, 1, 65535, 0),
    ref.Cursor(0, 2, 0, 0, 0x7FFFFFFF),
    ref.Cursor(0, 2, 1, 65535, -0x80000000),
    ref.Cursor(0, 0, 0, 0, 65536),
    ref.Cursor(3, 5, 2, 65535, 1),
    ref.Cursor(0, 1, -32768, 0, 0x7FFFFFFF),
    ref.Cursor(0, 1, 2**30 + 32767, 0, -0x80000000),
    ref.Cursor(0, 65, 0, 0, 65536, 1),
]


@pytest.fixture(scope="module", autouse=True)
def save_metrics():
    yield
    if path := os.environ.get("STEREO_FRAME_FETCH_EVIDENCE"):
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path(path).write_text(json.dumps(METRICS, indent=2) + "\n")


def word(session, syms, name):
    return session.peek(syms[f"_ff_{name}"] // 4)


def poke(session, syms, name, value):
    session.poke(syms[f"_ff_{name}"] // 4, value)


def fresh(dsp, cursor=CASES[0], same=False, split=False):
    linked, memory, syms, initial = dsp
    session = pk_sharc.CallSession([linked], BACK, base_memory=memory)
    session.runner.state.stack_model = True
    session.runner.state.bank_model = True
    session.runner.state.assume_nw32 = True
    for address, data in initial:
        for off in range(0, len(data), 4):
            session.poke(
                (address + off) // 4, int.from_bytes(data[off : off + 4], "little")
            )
    planes = (PLANE, PLANE if same else RIGHT_PLANE)
    for channel, plane in enumerate(planes):
        base = byte_to_normal_word(plane)
        assert base is not None and normal_word_to_byte(base) == plane
        # Include all 65 frames plus six sentinel/guard bytes, all initialized.
        for i in range(36):
            pattern = (
                ((0x1200 + 17 * i) | ((0x2400 + 13 * i) << 16))
                if plane == PLANE
                else ((0xE100 + 11 * i) | ((0xC300 + 19 * i) << 16))
            )
            session.poke(base + i, pattern)
        session.poke(syms["_ff_sources"] // 4 + channel, base)
        # Full containing words, including odd terminal's unused high half.
        lo, hi = plane, plane + 4 * ((FRAMES + 1) // 2)
        assert 0x80000000 <= lo < hi <= 0xA0000000
        session.poke(syms["_ff_spans"] // 4 + channel * 2, lo)
        session.poke(syms["_ff_spans"] // 4 + channel * 2 + 1, hi)
    for i, value in enumerate(vars(cursor).values()):
        session.poke(syms["_ff_cursor"] // 4 + i, value & 0xFFFFFFFF)
    poke(session, syms, "split_control", int(split))
    for i in range(64):
        session.poke(OUT + i, 0x7FC00001)
    return session


def output(session):
    return [session.peek(OUT + i) for i in range(64)]


def cursor_words(session, syms):
    return [session.peek(syms["_ff_cursor"] // 4 + i) for i in range(7)]


def run_reader(session, mode=0x9000, reference=False):
    context = REGS | {"MODE1": mode}
    result = session.call(
        "_ff_reference_h" if reference else "_ff_reader_h",
        context,
        max_steps=LIMIT,
        read_regs=tuple(KEEP),
    )
    assert result["halt"]["reason"] == "breakpoint", result
    for name in KEEP:
        assert reg(session, name) == context[name], name
    assert reg(session, "MODE1") == mode
    return result


@pytest.mark.parametrize("cursor", CASES)
@pytest.mark.parametrize("same", [False, True])
def test_no_writer_exact_existing_kernel_output_cursor_and_context(dsp, cursor, same):
    reference, experiment = fresh(dsp, cursor, same), fresh(dsp, cursor, same)
    run_reader(reference, reference=True)
    result = run_reader(experiment)
    assert output(experiment) == output(reference)
    assert cursor_words(experiment, dsp[2]) == cursor_words(reference, dsp[2])
    assert word(experiment, dsp[2], "count") == 0
    METRICS["largest_no_writer_total"] = max(
        METRICS.get("largest_no_writer_total", 0), result["instructions"]
    )


def snapshot(session):
    # Reuse the existing architectural snapshot method, excluding the observational
    # dict-subclass watch tracker from overlay copying. Runtime LoadedMemory now
    # contains immutable mappingproxy metadata, which the tracker references.
    state = session.runner.state
    plain = replace(state, overlay=dict(state.overlay))
    return copy.deepcopy(plain, {id(state.concrete): state.concrete})


def stock_page(plane):
    return BASE + ((plane - BASE) // 4096) * 4096


def watched_reads(session, syms):
    session.record_stores()  # Enable the CallSession watch-log lifecycle.
    session.runner.attach_watchpoints(
        [
            Watchpoint(PLANE, PLANE + 144, on_write=False, stop=False, label="left"),
            Watchpoint(
                RIGHT_PLANE,
                RIGHT_PLANE + 144,
                on_write=False,
                stop=False,
                label="right",
            ),
            Watchpoint(
                syms["_ff_sources"],
                syms["_ff_sources"] + 8,
                on_write=False,
                stop=False,
                label="descriptor",
            ),
        ]
    )


def writer(session, dsp, span, source_plane=None):
    syms = dsp[2]
    saved = snapshot(session)
    before = copy.deepcopy(saved.uregs)
    cursor_before = cursor_words(session, syms)
    outputs_before = output(session)
    session.record_stores()
    result = session.call(
        "_ff_writer_h",
        REGS
        | {
            "I6": WRITER_STACK,
            "I7": WRITER_STACK,
            "MODE1": 0x8000,
            "R4": span[0],
            "R8": span[1],
            "R12": (source_plane or PLANE) - BASE,
        },
        max_steps=6000,
        read_regs=("R0",),
    )
    assert result["halt"]["reason"] == "breakpoint", result
    assert reg(session, "MODE1") == 0x8000
    writes = [e for e in session.stores if 0x80000000 <= e.address < 0xA0000000]
    if writes:
        assert (
            word(session, syms, "write_count")
            == word(session, syms, "write_fault")
            == 0
        )
        width = 2 if span[1] == 10 else 4
        assert [(e.address, e.width) for e in writes] == [
            (span[0] + i, width) for i in range(0, span[1], width)
        ]
        assert word(session, syms, "written") == 1
    # Cursor/output/scratch are callback-owned, independent of backing count0.
    assert cursor_words(session, syms) == cursor_before
    assert output(session) == outputs_before
    allowed_control = {
        _canonical_dm_address(
            session.runner.state, syms[f"_ff_{name}"], 4, for_write=True
        )
        for name in ("open", "closed", "ready", "write_count", "write_fault", "written")
    }
    writer_stack_byte = _canonical_dm_address(
        session.runner.state, normal_word_to_byte(WRITER_STACK), 4, for_write=True
    )
    assert writer_stack_byte is not None
    bad = [
        e.to_json()
        for e in session.stores
        if not (
            e.address in allowed_control
            or span[0] <= e.address < span[0] + span[1]
            or writer_stack_byte - 128 <= e.address <= writer_stack_byte
        )
    ]
    assert not bad, bad
    saved.overlay = session.runner.state.overlay
    session.runner.state = saved
    assert session.runner.state.uregs == before
    return result["regs"]["R0"], writes


def trace(dsp, cursor, same=False, mode=0x9000):
    session = fresh(dsp, cursor, same)
    syms = dsp[2]
    watched_reads(session, syms)
    session.call("_ff_reader_h", REGS | {"MODE1": mode}, max_steps=0)
    session.runner.set_watch_active(True)
    modes, fetched_at, representatives = [], [], {}
    span = longest = prior_reads = 0
    for _boundary in range(LIMIT):
        current_mode = reg(session, "MODE1")
        fetched = word(session, syms, "fetched")
        modes.append(current_mode)
        fetched_at.append(fetched)
        if current_mode & 0x1000:
            assert word(session, syms, "count") == 0
            if fetched not in representatives:
                representatives[fetched] = snapshot(session)
            span = 0
        elif session.runner.state.pc_sw != BACK:
            span += 1
            longest = max(longest, span)
        if session.runner.state.pc_sw == BACK:
            break
        count = word(session, syms, "count")
        result = session.runner.run(1)
        assert result.halt.reason == "max-steps", result.halt
        new_reads = session.runner.watch_log[prior_reads:]
        if new_reads:
            assert not current_mode & 0x1000
            assert count == 1
        prior_reads = len(session.runner.watch_log)
    else:
        pytest.fail("bounded per-frame reader did not return")
    session.runner.set_watch_active(False)
    assert word(session, syms, "count") == 0
    for name in KEEP:
        assert reg(session, name) == (REGS | {"MODE1": mode})[name], name
    return modes, fetched_at, representatives, longest, session


@pytest.fixture(scope="module")
def traces(dsp):
    return {
        (kind, same): trace(
            dsp, CASES[0] if kind == "page" else ref.Cursor(0, 65, 64, 65535, 0), same
        )
        for kind in ("page", "guard")
        for same in (False, True)
    }


@pytest.mark.parametrize(
    "kind,same,plane",
    [
        (kind, same, plane)
        for kind in ("page", "guard")
        for same in (False, True)
        for plane in ([PLANE] if same else [PLANE, RIGHT_PLANE])
    ],
)
def test_every_arrival_old_stereo_prefix_then_zero_without_torn_frame(
    dsp, traces, kind, same, plane, record_property
):
    cursor = CASES[0] if kind == "page" else ref.Cursor(0, 65, 64, 65535, 0)
    modes, fetched_at, reps, longest, baseline = traces[kind, same]
    # Every instruction arrival maps to its first eligible delivery, including
    # pending before entry and delivery after return. No masked-arrival effects.
    next_enabled = None
    arrival_to_delivery = {}
    for arrival in reversed(range(len(modes))):
        if modes[arrival] & 0x1000:
            next_enabled = arrival
        arrival_to_delivery[arrival] = next_enabled
    assert set(arrival_to_delivery) == set(range(len(modes)))
    assert None not in arrival_to_delivery.values()
    classes = Counter(fetched_at[delivery] for delivery in arrival_to_delivery.values())
    assert set(classes) == set(range(33)) == set(reps)
    assert sum(classes.values()) == len(modes)
    # Exact equivalence within each class, not sampled execution: all descriptor
    # and backing reads in this complete trace were checked masked/count1 above.
    # Between fetches, only callback-owned copies drive compute/store/cursor step.
    # The single writer mutates backing+close controls only and restores the FULL
    # architectural context. Moving delivery within that interval therefore
    # commutes with its private work; next admission always denies. Equal fetched
    # count means the same old frame prefix, including a currently prefetched but
    # not yet stored frame. No writer changes descriptor, cursor or scratch.
    old = output(baseline)
    assert all(x != 0 for x in old)
    span = (stock_page(plane), 4096) if kind == "page" else (plane + FRAMES * 2, 10)
    # Guard starts in the unused half of a packed word the terminal reader loads.
    if kind == "guard":
        assert span[0] == plane + 128 + 2
    for fetched, arrivals in classes.items():
        assert arrivals > 0
        session = fresh(dsp, cursor, same)
        saved = reps[fetched]
        session.runner.state = copy.deepcopy(
            saved, {id(saved.concrete): saved.concrete}
        )
        code, writes = writer(session, dsp, span, plane)
        assert code == 0 and writes
        assert word(session, dsp[2], "closed") == 1
        watched_reads(session, dsp[2])
        session.runner.clear_watch_log()
        session.runner.set_watch_active(True)
        result = session.runner.run(LIMIT)
        session.runner.set_watch_active(False)
        assert result.halt.reason == "breakpoint", result.halt
        assert not session.runner.watch_log  # Even discarded forbidden loads fail.
        assert output(session) == old[:fetched] + [0] * (32 - fetched) + old[
            32 : 32 + fetched
        ] + [0] * (32 - fetched)
        position = cursor.index * 65536 + cursor.fraction + fetched * cursor.rate
        index, fraction = divmod(position, 65536)
        assert cursor_words(session, dsp[2]) == [
            cursor.start,
            cursor.end,
            index,
            fraction,
            cursor.rate,
            cursor.done,
            1,
        ]
        for name in KEEP:
            assert reg(session, name) == REGS[name], name
        watched_reads(session, dsp[2])
        run_reader(session)
        assert not session.stores
        assert output(session) == [0] * 64
    key = f"{kind}-{'same' if same else 'stereo'}"
    METRICS[key] = {
        "arrival_boundaries": len(modes),
        "delivery_classes": len(classes),
        "total_reader_instructions": len(modes) - 1,
        "max_masked_instructions": longest,
    }
    for name, value in METRICS[key].items():
        record_property(name, value)


def test_split_channel_negative_control_tears_actual_right_page(dsp):
    session = fresh(dsp, split=True)
    boundary = dsp[0].symbols["_ff_split_boundary"]
    session.runner.breakpoints = frozenset((BACK, boundary))
    result = session.call("_ff_reader_h", REGS, max_steps=LIMIT)
    assert result["final_pc_sw"] == boundary
    assert reg(session, "MODE1") & 0x1000
    assert word(session, dsp[2], "count") == 0
    expected = fresh(dsp)
    run_reader(expected)
    old = output(expected)
    code, writes = writer(session, dsp, (stock_page(RIGHT_PLANE), 4096), RIGHT_PLANE)
    assert code == 0 and writes
    session.runner.breakpoints = frozenset((BACK,))
    result = session.runner.run(LIMIT)
    assert result.halt.reason == "breakpoint", result.halt
    actual = output(session)
    assert actual[0] == old[0]
    assert actual[32] != old[32] and actual[32] != 0
    assert actual[1:32] == actual[33:64] == [0] * 31
    assert word(session, dsp[2], "count") == 0


@pytest.mark.parametrize("count,fault", [(1, 0), (0xFFFFFFFF, 0), (0, 1)])
def test_writer_refuses_stores_without_exact_zero_fault_free_drain(dsp, count, fault):
    session = fresh(dsp)
    poke(session, dsp[2], "count", count)
    poke(session, dsp[2], "fault", fault)
    code, writes = writer(session, dsp, (stock_page(PLANE), 4096))
    assert code == 2 and not writes
    assert word(session, dsp[2], "written") == 0


@pytest.mark.parametrize(
    "span,source_offset",
    [
        ((PLANE + 130, 10), 0xFBDD4636),  # source crosses signed boundary
        ((PLANE + 130, 10), 0xFFFFFFFF),  # unaligned source
        ((PLANE + 130, 10), PLANE + 126 - BASE),  # source/destination overlap
        ((PLANE + 131, 10), PLANE - BASE),
        ((0xFFFFFFF8, 10), PLANE - BASE),
        ((0x7FFFFFF8, 4096), PLANE - BASE),
        ((PLANE, 12), PLANE - BASE),
        ((BASE + 0x19200 * 4096, 4096), PLANE - BASE),
        ((PLANE, 4096), PLANE - BASE),
    ],
)
def test_invalid_writer_extent_has_no_backing_access_or_close(dsp, span, source_offset):
    session = fresh(dsp)
    # Raw call deliberately bypasses the safe test writer wrapper's source base.
    session.record_stores()
    ranges = [(0x80000000, 0xA0000000)]
    for start, length in [((BASE + source_offset) & 0xFFFFFFFF, 10), span]:
        end = start + length
        ranges.append((start, min(end, 1 << 32)))
        if end > 1 << 32:
            ranges.append((0, end - (1 << 32)))
    session.runner.attach_watchpoints(
        [
            Watchpoint(lo, hi, on_read=True, on_write=True, stop=False)
            for lo, hi in ranges
        ]
    )
    session.runner.breakpoints = frozenset((BACK, 0x1C400F))
    result = session.call(
        "_ff_writer_h",
        REGS
        | {
            "I6": WRITER_STACK,
            "I7": WRITER_STACK,
            "R4": span[0],
            "R8": span[1],
            "R12": source_offset,
        },
        max_steps=LIMIT,
        read_regs=("R0",),
    )
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK  # Original helper entry was never reached.
    assert result["regs"]["R0"] == 3
    assert not session.stores
    assert word(session, dsp[2], "closed") == 0
    assert word(session, dsp[2], "written") == 0


@pytest.mark.parametrize(
    "epoch,count,closed,fault",
    [(0, 0, 0, 0), (2, 0, 0, 0), (1, 0xFFFFFFFF, 0, 0), (1, 0, 1, 1)],
)
def test_stale_or_faulted_callback_reads_no_descriptor_or_backing(
    dsp, epoch, count, closed, fault
):
    session = fresh(dsp)
    session.poke(dsp[2]["_ff_cursor"] // 4 + 6, epoch)
    poke(session, dsp[2], "count", count)
    poke(session, dsp[2], "closed", closed)
    poke(session, dsp[2], "fault", fault)
    watched_reads(session, dsp[2])
    run_reader(session)
    assert not session.stores
    assert output(session) == [0] * 64
    assert cursor_words(session, dsp[2])[6] == epoch


def test_disabled_incoming_mask_retains_pending_event_until_return(dsp):
    modes, _, reps, longest, session = trace(dsp, CASES[0], mode=0x8000)
    assert not reps and all(not mode & 0x1000 for mode in modes)
    assert longest == len(modes) - 1
    reference = fresh(dsp)
    run_reader(reference, reference=True)
    assert output(session) == output(reference)
    session.runner.state.uregs[session._st.UREG_CODES["MODE1"]] = session._st.Const(
        0x9000
    )
    assert writer(session, dsp, (stock_page(PLANE), 4096))[0] == 0
    watched_reads(session, dsp[2])
    run_reader(session)
    assert not session.stores
    METRICS["incoming-disabled"] = {
        "arrival_boundaries": len(modes),
        "total_reader_instructions": len(modes) - 1,
        "max_masked_instructions": longest,
    }


@pytest.mark.parametrize(
    "span", [(BASE + 4096, 4096), (PLANE - 10, 10), (PLANE + 132, 10)]
)
def test_nonoverlap_respects_exact_packed_word_boundaries(dsp, span):
    session = fresh(dsp)
    code, writes = writer(session, dsp, span)
    assert code == 0 and not writes
    assert word(session, dsp[2], "open") == 1
    assert word(session, dsp[2], "closed") == 0


def test_writer_return_obeys_repository_index_load_spacing_contract(dsp):
    session = fresh(dsp)
    pc = dsp[0].symbols["_ff_writer_exit"]
    leave = dsp[0].symbols["_ff_writer_leave"]
    rows = []
    while pc < leave:
        insn = decode_loaded_at(session.runner.state.concrete, pc)
        rows.append(insn)
        pc += insn.length_bytes // 2
    assert pc == leave
    # Exit is exact MODE1 restore +2 NOPs, I12 load, R7 restore +3 NOPs.
    assert len(rows) == 8
    assert rows[3].type_name == "15b"
    assert len(rows[4:]) >= 4
    # This checks call.inc's scheduling contract, not an absent hardware stall.


def test_instruction_count_comparison_only(whole_block_dsp, traces):
    modes, _, _, longest, _ = whole_block_trace(whole_block_dsp, 0x9000)
    METRICS["existing-whole-block-reference"] = {
        "total_reader_instructions": len(modes) - 1,
        "max_masked_instructions": longest,
        "scope": "Existing represented-span fixture only; instruction counts, not cycles/latency or production performance.",
    }
    assert longest > traces["page", False][3]


def test_raw_fetch_shared_index_at_last_ddr_word():
    # New macro domain edge: the two indices may be the SAME register. Frame2
    # is an odd terminal's low half in the final valid full NW32 DDR word.
    source = """#include "dsp/sample_pcm16_fetch.inc"
#include "dsp/sample_pcm16_fetch.inc"
.SECTION/PM seg_pmco;
.GLOBAL _raw_fetch_h;
_raw_fetch_h:
    DK_SAMPLE_PCM16_FETCH_WORDS(R4, R7, R7, R8, R0, R1, I4);
    RTS;
"""
    obj = pk_sharc.assemble(pk_sharc.expand_includes(source, [REPO_ROOT / "sdk"]))
    linked = pk_sharc.link(obj, 0xB90000, {})
    session = pk_sharc.CallSession([linked], BACK)
    raw = 0x80017FFF
    session.poke(0x17FFFFFF, raw)
    session.record_stores()
    session.runner.attach_watchpoints(
        [
            Watchpoint(0, 1 << 40, on_read=False, stop=False),
            Watchpoint(0x9FFFFFFC, 0xA0000000, on_write=False, stop=False),
        ]
    )
    context = REGS | {"R4": 0x17FFFFFE, "R7": 2}
    result = session.call(
        "_raw_fetch_h", context, max_steps=30, read_regs=("R4", "R7", "R8", "R0")
    )
    assert result["halt"]["reason"] == "breakpoint", result
    assert {name: result["regs"][name] for name in ("R4", "R7", "R8", "R0")} == {
        "R4": context["R4"],
        "R7": 2,
        "R8": raw,
        "R0": raw,
    }
    assert all(e.access == "read" for e in session.stores)
    assert sum(e.width for e in session.stores) == 8
    assert all(
        0x9FFFFFFC <= e.address < e.address + e.width <= 0xA0000000
        for e in session.stores
    )
