"""Finite maskable-writer experiment, not a production interrupt-mask policy.

Every arrival boundary on the no-writer reader trace is enumerated. Arrivals
while masked have no side effects and queue until the same first enabled
boundary, so we execute each distinct delivery state once. This is exact
schedule equivalence for this one pending event, not sampled arrivals.

The current core exposes IRPTEN directly in MODE1; its alternate-register bank
pipeline remains enabled. Existing LOCK/UNLOCK NOPs are retained. No claim is
made about physical IRQ pipeline timing, NMI, DMA, cache or external writers.
"""

import copy
from collections import Counter

import numpy as np
import pytest
from support.paths import REPO_ROOT

from tools.patchkit import pk_sharc, pk_toolchains

SOURCE = REPO_ROOT / "tests/fixtures/dsp/sample-stereo-exclusion.s"
BACK = 0xB9F000
OUT = 0x08036000
LEFT_PCM, RIGHT_PCM = 0x10002000, 0x10003000
# STACK is a byte-alias frame, as the adapters give it (finding 111)
STACK, WRITER_STACK = 4 * 0x08038000, 0x08039000
OLD = (0x10000000, 0xE000F000)
NEW = (0x30002000, 0x9000A000)
PAGE = 0x01000000
SPANS = ((PAGE, 4096), (PAGE + 4096, 10))
LIMIT = 5500
KEEP = {f"R{n}": 0x7000 + n for n in (3, 5, 6, 7, 9, 10, 11, 13, 14, 15)}
KEEP.update({f"I{n}": OUT + 0x500 + 16 * n for n in (0, 1, 2, 3, 5)})
CONSTS = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M14": 1}
CONSTS.update({f"L{n}": 0 for n in range(8)})


@pytest.fixture(scope="module")
def dsp():
    if pk_toolchains.selache_dir() is None:
        pytest.fail("assembled exclusion experiment requires selache")
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(SOURCE.read_text(), [REPO_ROOT / "sdk"])
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


def regs(mode, stack=STACK):
    return {**KEEP, **CONSTS, "I6": stack, "I7": stack, "MODE1": mode}


def fresh(dsp, unlocked=False):
    linked, syms, initial = dsp
    session = pk_sharc.CallSession([linked], BACK)
    session.runner.state.stack_model = True
    session.runner.state.bank_model = True
    for address, data in initial:
        for offset in range(0, len(data), 4):
            session.poke(
                (address + offset) // 4,
                int.from_bytes(data[offset : offset + 4], "little"),
            )
    for base, pattern in zip((LEFT_PCM, RIGHT_PCM), OLD, strict=True):
        for i in range(64):
            session.poke(base + i, pattern)
    for i, base in enumerate((LEFT_PCM, RIGHT_PCM)):
        session.poke(syms["_sx_sources"] // 4 + i, base)
    session.poke(syms["_sx_unlocked_control"] // 4, int(unlocked))
    return session


def value(session, syms, name):
    return session.peek(syms[f"_sx_{name}"] // 4)


def reg(session, name):
    operand = session.runner.state.uregs[session._st.UREG_CODES[name]]
    assert isinstance(operand, session._st.Const), (name, operand)
    return operand.value & 0xFFFFFFFF


def snapshot(session):
    # All mutable register/flag/loop/stack/pending-bank/branch state is isolated.
    # Loaded code is immutable and shared; DM overlay is copied for each prefix.
    state = session.runner.state
    return copy.deepcopy(state, {id(state.concrete): state.concrete})


def expected(patterns):
    out = []
    for pattern in patterns:
        halves = [pattern & 65535, pattern >> 16]
        a, b = [np.float32((v - 65536 if v & 32768 else v) / 32768) for v in halves]
        # Cursor starts at fraction .5 and advances by exactly one frame.
        sample = np.float32(a + np.float32(np.float32(0.5) * np.float32(b - a)))
        out.extend(np.asarray([sample] * 32, dtype=np.float32).view(np.uint32).tolist())
    return out


def output(session):
    return [session.peek(OUT + i) for i in range(64)]


def run_reader(session, mode):
    result = session.call(
        "_sx_reader_h", regs(mode), max_steps=LIMIT, read_regs=tuple(regs(mode))
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert {name: reg(session, name) for name in regs(mode)} == regs(mode)
    return result


def writer(session, syms, span, unsafe=False):
    saved = snapshot(session)
    # The paused full architectural context is restored, sharing ONLY DM writes
    # from the stand-alone writer. Writer stack is disjoint from reader frames.
    before = copy.deepcopy(saved.uregs)
    result = session.call(
        "_sx_writer",
        {
            **regs(0x8000, WRITER_STACK),
            "R4": span[0],
            "R8": span[1],
            "R12": int(unsafe),
        },
        max_steps=1000,
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert reg(session, "MODE1") == 0x8000
    written = value(session, syms, "written")
    count = value(session, syms, "write_count")
    if written and not unsafe:
        assert count == 0  # Exact count, not merely an invalidated descriptor.
    if written and unsafe:
        assert count > 0
    overlay = session.runner.state.overlay
    saved.overlay = overlay
    session.runner.state = saved
    assert session.runner.state.uregs == before  # Includes flags and MODE1.
    return result["regs"]["R0"]


def trace(dsp, mode, unlocked=False):
    session = fresh(dsp, unlocked)
    result = session.call("_sx_reader_h", regs(mode), max_steps=0)
    assert result["halt"]["reason"] == "max-steps"
    modes, pcs, enabled = [], [], {}
    span = longest = 0
    for boundary in range(LIMIT):
        modes.append(reg(session, "MODE1"))
        pcs.append(session.runner.state.pc_sw)
        if modes[-1] & 0x1000:
            enabled[boundary] = snapshot(session)
            span = 0
        elif pcs[-1] != BACK:
            span += 1
            longest = max(longest, span)
        if pcs[-1] == BACK:
            break
        result = session.runner.run(1)
        assert result.halt.reason == "max-steps", result.halt
    else:
        pytest.fail("bounded reader did not return")
    assert output(session) == expected(OLD)
    assert {name: reg(session, name) for name in regs(mode)} == regs(mode)
    return modes, pcs, enabled, longest, session


@pytest.fixture(scope="module")
def baseline(dsp):
    return trace(dsp, 0x9000)


def test_single_step_trace_matches_uninterrupted_execution(dsp, baseline):
    # Validates the stepping primitive against ordinary execution, including
    # nested call frames, all output/cursor words and final registers/flags.
    session = fresh(dsp)
    result = run_reader(session, 0x9000)
    modes, _, _, longest, stepped = baseline
    assert result["instructions"] == len(modes) - 1
    assert session.runner.state.uregs == stepped.runner.state.uregs
    assert session.runner.state.overlay == stepped.runner.state.overlay
    assert 0 < longest < LIMIT


@pytest.mark.parametrize("span", SPANS, ids=["page-4096", "guard-10"])
def test_every_pending_arrival_boundary_is_old_stereo_or_denied(
    dsp, baseline, span, record_property
):
    _, syms, _ = dsp
    modes, _, enabled, longest, _ = baseline
    # Reverse scan gives the first eligible delivery boundary for EVERY arrival,
    # including arrival 0 (already pending before admission) and after return.
    first_enabled = None
    mapping = {}
    for arrival in reversed(range(len(modes))):
        if modes[arrival] & 0x1000:
            first_enabled = arrival
        mapping[arrival] = first_enabled
    assert set(mapping) == set(range(len(modes)))
    assert all(delivery is not None for delivery in mapping.values())
    groups = Counter(mapping.values())
    assert sum(groups.values()) == len(modes)
    assert max(groups.values()) >= longest
    record_property("arrival_boundaries", len(mapping))
    record_property("distinct_delivery_states", len(groups))
    record_property("reader_instructions", len(modes) - 1)
    record_property("largest_masked_instruction_span", longest)
    outcomes = set()
    for delivery, arrivals in groups.items():
        assert arrivals >= 1
        session = fresh(dsp)
        session.runner.state = copy.deepcopy(
            enabled[delivery],
            {id(enabled[delivery].concrete): enabled[delivery].concrete},
        )
        assert reg(session, "MODE1") & 0x1000
        assert writer(session, syms, span) == 0
        assert value(session, syms, "written") == 1
        assert value(session, syms, "count") == 0
        assert value(session, syms, "closed") == 1
        result = session.runner.run(LIMIT)
        assert result.halt.reason == "breakpoint", result.halt
        assert {name: reg(session, name) for name in regs(0x9000)} == regs(0x9000)
        samples = output(session)
        assert samples in (expected(OLD), [0] * 64)
        outcomes.add(tuple(samples))
        # Cached epoch 1 never restarts after CLOSE, even though the same source
        # addresses now contain distinct successor-like data.
        session.record_reads(LEFT_PCM, RIGHT_PCM + 64 - LEFT_PCM)
        run_reader(session, 0x9000)
        assert not session.stores  # No stale backing loads after CLOSE.
        assert output(session) == [0] * 64
        assert value(session, syms, "count") == 0
    assert outcomes == {tuple(expected(OLD)), tuple([0] * 64)}


@pytest.mark.parametrize("span", SPANS, ids=["page-4096", "guard-10"])
def test_incoming_disabled_mask_keeps_event_pending(dsp, span, record_property):
    _, syms, _ = dsp
    modes, _, enabled, longest, session = trace(dsp, 0x8000)
    assert not enabled
    assert longest == len(modes) - 1
    record_property("arrival_boundaries", len(modes))
    record_property("pending_arrivals", len(modes))
    record_property("largest_masked_instruction_span", longest)
    # Every arrival remains pending: no eligible delivery, no backing overwrite.
    assert all(not mode & 0x1000 for mode in modes)
    assert value(session, syms, "written") == 0
    assert value(session, syms, "count") == 0
    assert output(session) == expected(OLD)
    # Caller later enables delivery, beyond the measured callback.
    session.runner.state.uregs[session._st.UREG_CODES["MODE1"]] = session._st.Const(
        0x9000
    )
    assert writer(session, syms, span) == 0
    session.record_reads(LEFT_PCM, RIGHT_PCM + 64 - LEFT_PCM)
    run_reader(session, 0x9000)
    assert not session.stores
    assert output(session) == [0] * 64


@pytest.mark.parametrize(
    "unsafe", [False, True], ids=["exact-drain", "UNSAFE-invalidation-only"]
)
def test_unlocked_pinned_control_at_cross_channel_boundary(dsp, unsafe):
    linked, syms, _ = dsp
    session = fresh(dsp, unlocked=True)
    session.runner.breakpoints = frozenset(
        (BACK, linked.symbols["_dk_sample_stereo_after_left"])
    )
    result = session.call("_sx_reader_h", regs(0x9000), max_steps=LIMIT)
    assert result["final_pc_sw"] == linked.symbols["_dk_sample_stereo_after_left"]
    assert reg(session, "MODE1") & 0x1000
    assert value(session, syms, "count") == 1
    session.runner.breakpoints = frozenset((BACK,))
    assert writer(session, syms, SPANS[1], unsafe) == (0 if unsafe else 2)
    assert value(session, syms, "written") == int(unsafe)
    result = session.runner.run(LIMIT)
    assert result.halt.reason == "breakpoint", result.halt
    assert output(session) == expected((OLD[0], NEW[1]) if unsafe else OLD)
    if unsafe:
        assert output(session) != expected(OLD)
        assert output(session) != expected(NEW)
    assert value(session, syms, "count") == 0
    assert reg(session, "MODE1") == 0x9000
    session.record_reads(LEFT_PCM, RIGHT_PCM + 64 - LEFT_PCM)
    run_reader(session, 0x9000)
    assert not session.stores
    assert output(session) == [0] * 64


@pytest.mark.parametrize("count,fault", [(1, 0), (0xFFFFFFFF, 0), (0, 1)])
def test_writer_requires_exact_zero_count_and_no_fault(dsp, count, fault):
    _, syms, _ = dsp
    session = fresh(dsp)
    session.poke(syms["_sx_count"] // 4, count)
    session.poke(syms["_sx_fault"] // 4, fault)
    assert writer(session, syms, SPANS[0]) == 2
    assert value(session, syms, "closed") == 1
    assert value(session, syms, "written") == 0
    assert all(
        session.peek(base + i) == pattern
        for base, pattern in zip((LEFT_PCM, RIGHT_PCM), OLD, strict=True)
        for i in range(64)
    )


@pytest.mark.parametrize("span", [(PAGE - 10, 10), (PAGE + 4102, 10)])
def test_nonoverlapping_write_does_not_retire_binding(dsp, span):
    _, syms, _ = dsp
    session = fresh(dsp)
    assert writer(session, syms, span) == 0
    assert value(session, syms, "open") == 1
    assert value(session, syms, "closed") == 0
    assert value(session, syms, "written") == 0
