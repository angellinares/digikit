"""Split-cursor sampler: independent fixed-point oracle and assembled-core checks.

Only assembled checks skip without the assembler. Sparse source maps also guard
large indices without allocating a large buffer.
"""

from dataclasses import replace

import numpy as np
import pytest
from support.paths import REPO_ROOT

from tools.patchkit import dsp_sample_ref as ref
from tools.patchkit import pk_sharc, pk_toolchains

ROOT = REPO_ROOT
F32 = np.float32
BASE = 0xB90000
SRC = 0x10001000
OUT = 0x08020000
STATE = OUT + 0x100
STACK = OUT + 0x300
SAVED = {f"R{n}": 0x7000 + n for n in (3, 5, 6, 7, 9, 10, 11, 13, 14, 15)}
SAVED.update({f"I{n}": 0x08021000 + 16 * n for n in (0, 1, 2, 3, 5)})
SAVED.update({"M1": 11, "M2": 12, "M3": 13, "B3": 0x08021050})
CONSTS = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M14": 1}
REGS = {**SAVED, **CONSTS, "I6": STACK, "I7": STACK}
# Conservative instruction ceiling: <=40 per output + 100 entry/exit/caller.
# This is an instruction count, not a hardware-cycle or realtime claim.
CEILING = 32 * 40 + 100


def words(state):
    return [v & 0xFFFFFFFF for v in vars(state).values()]


def bits(values):
    return np.asarray(values, dtype=F32).view(np.uint32).tolist()


def oracle(source, state, blocks=2):
    """Combined arbitrary-precision fixed-point position, independent of split update."""
    st = replace(state)
    position = st.index * 65536 + st.fraction
    outputs = []
    for _ in range(blocks):
        out = np.zeros(32, dtype=F32)
        for n in range(32):
            index, fraction = divmod(position, 65536)
            if not st.start <= index < st.end:
                st.done = 1
            if st.done:
                break
            a = F32(source[index])
            b = F32(source[min(index + 1, st.end - 1)])
            # Explicit operation order and one float32 rounding per operation.
            difference = F32(b - a)
            product = F32(F32(fraction / 65536) * difference)
            out[n] = F32(a + product)
            position += st.rate
            if not st.start * 65536 <= position < st.end * 65536:
                st.done = 1
        outputs.append(out)
    st.index, st.fraction = divmod(position, 65536)
    return outputs, st


CASES = [
    ref.Cursor(0, 0, 0, 0, 65536),
    ref.Cursor(3, 3, 2, 0, -65536),
    ref.Cursor(0, 1, 0, 0, 65536),
    ref.Cursor(0, 1, 0, 65535, -1),
    ref.Cursor(0, 2, 0, 32768, 32768),
    ref.Cursor(0, 2, 1, 0, -32768),
    ref.Cursor(3, 103, 3, 65535, 100003),
    ref.Cursor(3, 103, 102, 1, -100003),
    ref.Cursor(0, 32, 0, 0, 65536),  # exact block-end exhaustion
    ref.Cursor(0, 32, 31, 0, -65536),
    ref.Cursor(0, 2, 0, 12345, 0),
    ref.Cursor(0, 2, 0, 0, 0x7FFFFFFF),
    ref.Cursor(0, 2, 1, 65535, -0x80000000),
    ref.Cursor(0, 2, 0, 0, 65536, 1),  # authoritative done
    ref.Cursor(3, 5, 2, 65535, 1),  # already outside
    ref.Cursor(2**24 + 3, 2**24 + 103, 2**24 + 3, 1, 32769),
    ref.Cursor(2**26 - 102, 2**26 - 1, 2**26 - 2, 65535, -65537),
]


def source_for(st):
    return {i: F32(((i % 17) - 8) / 11) for i in range(st.start, st.end)}


@pytest.mark.parametrize(
    "st", CASES + [ref.Cursor(2**30 - 102, 2**30 - 1, 2**30 - 2, 65535, -65537)]
)
def test_reference_blocks_against_combined_position(st):
    source = source_for(st)
    expected, final = oracle(source, st)
    current = st
    for want in expected:
        got, current = ref.linear_block(source, current)
        assert bits(got) == bits(want)
    assert current == final


def test_cursor_split_against_exact_integer_position():
    rng = np.random.default_rng(901)
    for _ in range(2000):
        index = int(rng.integers(-32768, 2**30))
        fraction = int(rng.integers(0, 65536))
        rate = int(rng.integers(-(2**31), 2**31))
        assert ref.cursor_step(index, fraction, rate) == divmod(
            index * 65536 + fraction + rate, 65536
        )


def build_reader(kernel="_dk_sample_linear", source_base=SRC):
    if pk_toolchains.selache_dir() is None:
        pytest.skip("selache not built")
    text = "\n".join(
        [
            '#include "dsp/call.inc"',
            '#include "dsp/sample.inc"',
            '#include "dsp/sample_pcm16.inc"' if "pcm16" in kernel else "",
            ".SECTION/PM seg_pmco;",
            ".GLOBAL _sample_h;",
            "_sample_h:",
            f"R4 = {source_base};"
            if source_base is not None
            else f"R4 = DM({STATE + 6});",
            f"R8 = {OUT};",
            f"R12 = {STATE};",
            f"DK_CALL({kernel}, _sample_after0);",
            f"R4 = {source_base};"
            if source_base is not None
            else f"R4 = DM({STATE + 6});",
            f"R8 = {OUT + 32};",
            f"R12 = {STATE};",
            f"DK_CALL({kernel}, _sample_after1);",
            "RTS;",
            "",
        ]
    )
    _, result = pk_sharc.build(pk_sharc.expand_includes(text, [ROOT / "sdk"]), BASE)
    return result


@pytest.fixture(scope="module")
def linked():
    return build_reader()


def poke(source, st):
    dm = {SRC + i: bits([v])[0] for i, v in source.items()}
    dm.update({STATE + i: w for i, w in enumerate(words(st))})
    dm.update({OUT + i: 0x7FC00001 for i in range(64)})
    return dm


@pytest.mark.parametrize("st", CASES)
def test_assembled_two_blocks(linked, st):
    source = source_for(st)
    expected, final = oracle(source, st)
    r = pk_sharc.run_call(
        linked,
        "_sample_h",
        regs=REGS,
        dm_words=poke(source, st),
        reads={"out": (OUT, 64), "state": (STATE, 6)},
        read_regs=tuple(REGS),
    )
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert r["state"] == words(final)
    assert r["out"] == bits(np.concatenate(expected))
    assert {name: r["regs"][name] for name in REGS} == REGS
    assert r["instructions"] <= 2 * CEILING + 1


@pytest.mark.parametrize("st", [CASES[i] for i in (0, 2, 4, 5, 6, 7, 13, 15, 16)])
def test_assembled_source_read_guards(linked, st):
    source = source_for(st)
    for lo, count, control in (
        (SRC + st.start - 1, 1, False),
        (SRC + st.end, 1, False),
        (SRC + st.start, max(1, st.end - st.start), not st.done and st.start < st.end),
    ):
        sess = pk_sharc.CallSession([linked], 0xB9F000)
        for addr, word in poke(source, st).items():
            sess.poke(addr, word)
        sess.record_reads(lo, count)
        r = sess.call("_sample_h", REGS)
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert bool(sess.stores) == control


# Packed input is deliberately encoded through transport bytes, independently of
# the kernel's shift/sign-extension sequence. Unselected halves contain poison.
PCM16_VALUES = (-32768, 32767, -1, 0, 1, -16384, 16384, -30001, 29999)
PCM16_CASES = CASES + [
    ref.Cursor(1, 2, 1, 65535, 0),  # high-half singleton, clamped neighbor
    ref.Cursor(1, 5, 1, 32768, 32768),  # odd start and odd exclusive end
    ref.Cursor(2, 7, 6, 65535, -32769),
    ref.Cursor(2**30 - 103, 2**30 - 1, 2**30 - 2, 65535, 0x7FFFFFFF),
    ref.Cursor(2**30 - 103, 2**30 - 1, 2**30 - 103, 0, -0x80000000),
    ref.Cursor(0, 1, -32768, 0, 0x7FFFFFFF),
    ref.Cursor(0, 1, 2**30 + 32767, 0, -0x80000000),
]
PCM16_CEILING = 32 * 80 + 102


def pcm16_source(st):
    return {
        i: PCM16_VALUES[(i - st.start) % len(PCM16_VALUES)]
        for i in range(st.start, st.end)
    }


def pcm16_base(st):
    # Rebase sparse large cursors so every accessed containing word is in the
    # architectural normal-word range, even near the signed cursor bounds.
    return (SRC - st.start // 2) & 0xFFFFFFFF


def pcm16_poke(source, st, source_base=None):
    if source_base is None:
        source_base = pcm16_base(st)
    # Every containing word is fully mapped, including an unused high half at an
    # odd end. No decision about DDR ownership or physical mapping is implied.
    packed = {}
    for i, value in source.items():
        word = packed.setdefault(i // 2, bytearray(b"\xa5\x5a\xa5\x5a"))
        offset = 2 * (i % 2)
        word[offset : offset + 2] = value.to_bytes(2, "little", signed=True)
    dm = {
        (source_base + i) & 0xFFFFFFFF: int.from_bytes(value, "little")
        for i, value in packed.items()
    }
    dm.update({STATE + i: w for i, w in enumerate(words(st))})
    dm[STATE + 6] = source_base  # Harness argument, outside the six-word ABI.
    # Guard the two output edges, state edges, and the call's stack extent.
    for addr in (OUT - 1, OUT + 64, STATE - 1, STATE + 7, STACK + 1, STACK - 15):
        dm[addr] = 0x5A96C33C
    dm.update({OUT + i: 0x7FC00001 for i in range(64)})
    return dm


@pytest.fixture(scope="module")
def linked_pcm16():
    return build_reader("_dk_sample_pcm16_linear", None)


def pcm16_expected(source, st):
    normalized = {i: F32(value / 32768) for i, value in source.items()}
    return oracle(normalized, st)


@pytest.mark.parametrize("st", PCM16_CASES)
def test_pcm16_assembled_two_blocks(linked_pcm16, st):
    source = pcm16_source(st)
    expected, final = pcm16_expected(source, st)
    dm = pcm16_poke(source, st)
    immutable = {
        addr: value
        for addr, value in dm.items()
        if addr not in range(OUT, OUT + 64)
        and addr not in (STATE + 2, STATE + 3, STATE + 5)
    }
    r = pk_sharc.run_call(
        linked_pcm16,
        "_sample_h",
        regs=REGS,
        dm_words=dm,
        reads={
            "out": (OUT, 64),
            "state": (STATE, 6),
            **{f"immutable_{addr}": (addr, 1) for addr in immutable},
        },
        read_regs=tuple(REGS),
    )
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert r["state"] == words(final)
    assert r["out"] == bits(np.concatenate(expected))
    assert {name: r["regs"][name] for name in REGS} == REGS
    assert all(r[f"immutable_{addr}"] == [value] for addr, value in immutable.items())
    assert r["instructions"] <= 2 * PCM16_CEILING + 1


@pytest.mark.parametrize("st", PCM16_CASES)
def test_pcm16_assembled_read_guards(linked_pcm16, st):
    source = pcm16_source(st)
    base = pcm16_base(st)
    lo = (base + st.start // 2) & 0xFFFFFFFF
    hi = (base + (st.end + 1) // 2) & 0xFFFFFFFF
    # Watch both adjacent words and the entire source view. This also checks that
    # done/empty/outside cursors perform no speculative source loads.
    valid = not st.done and st.start <= st.index < st.end
    for addr, count, should_read in (
        (lo - 1, 1, False),
        (hi, 1, False),
        (lo, max(1, hi - lo), valid),
    ):
        sess = pk_sharc.CallSession([linked_pcm16], 0xB9F000)
        for address, word in pcm16_poke(source, st).items():
            sess.poke(address, word)
        sess.record_reads(addr, count)
        r = sess.call("_sample_h", REGS)
        assert r["halt"]["reason"] == "breakpoint", r["halt"]
        assert bool(sess.stores) == should_read


def test_pcm16_normal_word_source_base():
    # Same packed bytes at a distinct normal-word base. Numerics establish raw
    # frame addressing; this does not assert any firmware allocator's mapping.
    source_base = 0x10200000
    linked = build_reader("_dk_sample_pcm16_linear", source_base)
    st = ref.Cursor(1, 10, 1, 16384, 32768)
    source = pcm16_source(st)
    expected, final = pcm16_expected(source, st)
    r = pk_sharc.run_call(
        linked,
        "_sample_h",
        regs=REGS,
        dm_words=pcm16_poke(source, st, source_base),
        reads={"out": (OUT, 64), "state": (STATE, 6)},
    )
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert r["out"] == bits(np.concatenate(expected))
    assert r["state"] == words(final)


@pytest.mark.parametrize("st", [CASES[8], CASES[9]])
def test_pcm16_completion_on_output31(linked_pcm16, st):
    sess = pk_sharc.CallSession(
        [linked_pcm16],
        0xB9F000,
        breaks=(linked_pcm16.symbols["_sample_after0"],),
    )
    source = pcm16_source(st)
    for addr, word in pcm16_poke(source, st).items():
        sess.poke(addr, word)
    r = sess.call("_sample_h", REGS)
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert r["final_pc_sw"] == linked_pcm16.symbols["_sample_after0"]
    assert sess.peek(STATE + 5) == 1
    expected, _ = pcm16_expected(source, st)
    assert [sess.peek(OUT + i) for i in range(32)] == bits(expected[0])


# The loop option: bit 1 of the done word is an input for one call and is written back as 0.
LOOP_BIT = 2
LOOP_CASES = [
    ref.Cursor(10, 30, 25, 0, 65536, LOOP_BIT),  # forward, wraps more than once
    ref.Cursor(10, 30, 12, 32768, -65536, LOOP_BIT),  # reverse
    ref.Cursor(10, 30, 28, 40000, 100003, LOOP_BIT),  # fractional rate
    ref.Cursor(10, 30, 11, 5, -70001, LOOP_BIT),
    ref.Cursor(10, 12, 10, 0, 2 * 65536, LOOP_BIT),  # two frame region, wrap by 2
    ref.Cursor(10, 12, 10, 0, 5 * 65536, LOOP_BIT),  # rate above the region: ends
    ref.Cursor(10, 11, 10, 0, 65536, LOOP_BIT),  # one frame region: no loop
    ref.Cursor(10, 30, 25, 0, 65536, LOOP_BIT | 1),  # already done
]


def loop_oracle(source, st):
    """One block with the loop option: wrap by the region length once, the forward neighbor at the end is the start."""
    out = np.zeros(32, dtype=F32)
    done = st.done & 1
    position = st.index * 65536 + st.fraction
    length = st.end - st.start
    for n in range(32):
        index, fraction = divmod(position, 65536)
        if not st.start <= index < st.end:
            done = 1
        if done:
            break
        nxt = (
            st.start
            if st.rate >= 0 and index + 1 >= st.end
            else min(index + 1, st.end - 1)
        )
        a, b = F32(source[index]), F32(source[nxt])
        out[n] = F32(a + F32(F32(fraction / 65536) * F32(b - a)))
        position += st.rate
        if not st.start * 65536 <= position < st.end * 65536:
            if length >= 2:
                position += (
                    -length * 65536 if position >= st.end * 65536 else length * 65536
                )
            if not st.start * 65536 <= position < st.end * 65536:
                done = 1
    index, fraction = divmod(position, 65536)
    return out, replace(st, index=index, fraction=fraction, done=done)


@pytest.mark.parametrize("st", LOOP_CASES)
def test_pcm16_loop_option_wraps_by_the_region_length(linked_pcm16, st):
    sess = pk_sharc.CallSession(
        [linked_pcm16],
        0xB9F000,
        breaks=(linked_pcm16.symbols["_sample_after0"],),
    )
    source = pcm16_source(st)
    for addr, word in pcm16_poke(source, st).items():
        sess.poke(addr, word)
    r = sess.call("_sample_h", REGS)
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    normalized = {i: F32(value / 32768) for i, value in source.items()}
    out, final = loop_oracle(normalized, st)
    assert [sess.peek(OUT + i) for i in range(32)] == bits(out)
    assert [sess.peek(STATE + i) for i in range(6)] == words(final)
