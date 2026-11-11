"""Assembled planar stereo PCM16 composition against a two-channel oracle."""

from dataclasses import replace

import numpy as np
import pytest
from support.paths import REPO_ROOT

from tools.patchkit import dsp_sample_ref as ref
from tools.patchkit import pk_sharc, pk_toolchains

BASE = 0xB90000
SRC = 0x10001000
OUT = 0x08020000
RIGHT = OUT + 0x100
STATE = OUT + 0x200
SOURCES = STATE + 0x10
OUTPUTS = STATE + 0x20
STACK = OUT + 0x400
POISON = 0x5A96C33C
SAVED = {f"R{n}": 0x7000 + n for n in (3, 5, 6, 7, 9, 10, 11, 13, 14, 15)}
SAVED.update({f"I{n}": OUT + 0x600 + 16 * n for n in (0, 1, 2, 3, 5)})
SAVED.update({"M1": 11, "M2": 12, "M3": 13, "B3": OUT + 0x650})
CONSTS = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M14": 1, "M0": 0}
CONSTS.update({f"L{n}": 0 for n in range(8)})
# The actual adapters enter with a byte-alias I6 and I7 (word address * 4); frame words are still addressed as normal words below.
STACK_ALIAS = 4 * STACK
REGS = {**SAVED, **CONSTS, "I6": STACK_ALIAS, "I7": STACK_ALIAS}
CEILING = 2 * (32 * 80 + 102) + 100
CASES = [
    ref.Cursor(0, 0, 0, 0, 65536),
    ref.Cursor(0, 100, 0, 12345, 32769),
    ref.Cursor(1, 100, 99, 65535, -100003),
    ref.Cursor(1, 2, 1, 65535, 0),
    ref.Cursor(0, 32, 0, 0, 65536),
    ref.Cursor(0, 32, 31, 0, -65536),
    ref.Cursor(0, 2, 0, 12345, 0),
    ref.Cursor(0, 2, 0, 0, 0x7FFFFFFF),
    ref.Cursor(0, 2, 1, 65535, -0x80000000),
    ref.Cursor(0, 100, 0, 0, 65536, 1),
    ref.Cursor(3, 5, 2, 65535, 1),
    ref.Cursor(0, 1, -32768, 0, 0x7FFFFFFF),
    ref.Cursor(0, 1, 2**30 + 32767, 0, -0x80000000),
    ref.Cursor(2**24 + 3, 2**24 + 103, 2**24 + 3, 1, 32769),
    ref.Cursor(2**30 - 103, 2**30 - 1, 2**30 - 2, 65535, -65537),
]


def words(state):
    return [value & 0xFFFFFFFF for value in vars(state).values()]


def bits(values):
    return np.asarray(values, dtype=np.float32).view(np.uint32).tolist()


def oracle(channels, state):
    """One arbitrary-precision position drives both channels, no split update."""
    st = replace(state)
    position = st.index * 65536 + st.fraction
    out = np.zeros((2, 32), dtype=np.float32)
    for n in range(32):
        index, fraction = divmod(position, 65536)
        if not st.start <= index < st.end:
            st.done = 1
        if st.done:
            break
        for channel, source in enumerate(channels):
            a = np.float32(source[index] / 32768)
            b = np.float32(source[min(index + 1, st.end - 1)] / 32768)
            difference = np.float32(b - a)
            product = np.float32(np.float32(fraction / 65536) * difference)
            out[channel, n] = np.float32(a + product)
        position += st.rate
        if not st.start * 65536 <= position < st.end * 65536:
            st.done = 1
    st.index, st.fraction = divmod(position, 65536)
    return out, st


@pytest.fixture(scope="module")
def linked():
    if pk_toolchains.selache_dir() is None:
        pytest.fail("assembled stereo checks require selache")
    text = "\n".join(
        [
            '#include "dsp/contract/call.inc"',
            '#include "dsp/sample/float.inc"',
            '#include "dsp/sample/pcm16.inc"',
            '#include "dsp/sample/pcm16_stereo.inc"',
            ".SECTION/PM seg_pmco;",
            ".GLOBAL _stereo_h;",
            "_stereo_h:",
            f"R4 = {SOURCES};",
            f"R8 = {OUTPUTS};",
            f"R12 = {STATE};",
            "DK_CALL(_dk_sample_pcm16_stereo_linear, _stereo_after);",
            "RTS;",
        ]
    )
    return pk_sharc.build(pk_sharc.expand_includes(text, [REPO_ROOT / "sdk"]), BASE)[1]


def setup(st, alias=False):
    values = (-32768, 32767, -1, 0, 1, -16384, 16384, -30001, 29999)
    left = {i: values[(i - st.start) % len(values)] for i in range(st.start, st.end)}
    right = left if alias else {i: -value - 1 for i, value in left.items()}
    bases = [(SRC - st.start // 2) & 0xFFFFFFFF]
    bases.append(bases[0] if alias else (bases[0] + 0x10000) & 0xFFFFFFFF)
    dm = {}
    for source, base in zip((left, right), bases, strict=True):
        packed = {}
        for i, value in source.items():
            word = packed.setdefault(i // 2, bytearray(b"\xa5\x5a\xa5\x5a"))
            offset = 2 * (i % 2)
            word[offset : offset + 2] = value.to_bytes(2, "little", signed=True)
        dm.update(
            {
                (base + i) & 0xFFFFFFFF: int.from_bytes(w, "little")
                for i, w in packed.items()
            }
        )
    dm.update({STATE + i: word for i, word in enumerate(words(st))})
    dm.update(
        {SOURCES: bases[0], SOURCES + 1: bases[1], OUTPUTS: OUT, OUTPUTS + 1: RIGHT}
    )
    for output in (OUT, RIGHT):
        dm.update({output + i: 0x7FC00001 for i in range(32)})
    for address in (
        OUT - 1,
        OUT + 32,
        RIGHT - 1,
        RIGHT + 32,
        STATE - 1,
        STATE + 6,
        SOURCES - 1,
        SOURCES + 2,
        OUTPUTS - 1,
        OUTPUTS + 2,
        STACK + 1,
        STACK - 26,
    ):
        dm[address] = POISON
    return (left, right), bases, dm


@pytest.mark.parametrize("st", CASES)
@pytest.mark.parametrize("alias", [False, True], ids=["stereo", "mono-to-stereo"])
def test_assembled_stereo_cursor_once_and_call_contract(linked, st, alias):
    channels, _, dm = setup(st, alias)
    expected, final = oracle(channels, st)
    immutable = {
        a: w
        for a, w in dm.items()
        if a not in range(OUT, OUT + 32)
        and a not in range(RIGHT, RIGHT + 32)
        and a not in (STATE + 2, STATE + 3, STATE + 5)
    }
    result = pk_sharc.run_call(
        linked,
        "_stereo_h",
        regs=REGS,
        dm_words=dm,
        max_steps=CEILING + 1,
        reads={
            "left": (OUT, 32),
            "right": (RIGHT, 32),
            "state": (STATE, 6),
            "right_state": (STACK - 11, 6),
            **{f"immutable_{a}": (a, 1) for a in immutable},
        },
        read_regs=tuple(REGS),
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    assert result["left"] == bits(expected[0])
    assert result["right"] == bits(expected[1])
    assert result["state"] == result["right_state"] == words(final)
    assert {name: result["regs"][name] for name in REGS} == REGS
    assert all(result[f"immutable_{a}"] == [w] for a, w in immutable.items())
    assert result["instructions"] <= CEILING + 1
    assert (
        linked.symbols["_dk_sample_pcm16_stereo_linear"]
        == linked.symbols["dk_sample_pcm16_stereo_linear."]
    )


@pytest.mark.parametrize("st", CASES)
def test_both_source_read_bounds(linked, st):
    _, bases, dm = setup(st)
    valid = not st.done and st.start <= st.index < st.end
    for base in bases:
        lo = (base + st.start // 2) & 0xFFFFFFFF
        hi = (base + (st.end + 1) // 2) & 0xFFFFFFFF
        for address, count, should_read in (
            (lo - 1, 1, False),
            (hi, 1, False),
            (lo, max(1, hi - lo), valid),
        ):
            session = pk_sharc.CallSession([linked], 0xB9F000)
            for a, word in dm.items():
                session.poke(a, word)
            session.record_reads(address, count)
            result = session.call("_stereo_h", REGS, max_steps=CEILING + 1)
            assert result["halt"]["reason"] == "breakpoint", result["halt"]
            assert bool(session.stores) == should_read


def test_nested_frames_with_architectural_stack(linked):
    st = CASES[1]
    channels, _, dm = setup(st)
    session = pk_sharc.CallSession([linked], 0xB9F000)
    session.runner.state.stack_model = True
    session.runner.state.bank_model = True
    # Poison the full claimed writable stack extent before the real call-frame
    # model executes both nested calls and indirect returns.
    for address in range(STACK - 25, STACK + 1):
        session.poke(address, 0xDEADBEEF)
    for address, word in dm.items():
        session.poke(address, word)
    result = session.call(
        "_stereo_h", REGS, max_steps=CEILING + 1, read_regs=tuple(REGS)
    )
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    expected, final = oracle(channels, st)
    assert [session.peek(OUT + i) for i in range(32)] == bits(expected[0])
    assert [session.peek(RIGHT + i) for i in range(32)] == bits(expected[1])
    assert [session.peek(STATE + i) for i in range(6)] == words(final)
    assert {name: result["regs"][name] for name in REGS} == REGS
    assert session.peek(STACK) == STACK_ALIAS
    assert session.peek(STACK - 1) == linked.symbols["_stereo_after"] - 1
    assert session.peek(STACK - 12) == STACK_ALIAS
    assert (
        session.peek(STACK - 13) == linked.symbols["_dk_sample_stereo_after_right"] - 1
    )
    assert session.peek(STACK - 26) == POISON


def test_right_channel_is_not_silent_under_the_adapter_byte_alias_frame(linked):
    """Finding 111: the cursor copy is found at I6 - 44 bytes; I6 - 11 left R silent under the real adapters."""
    st = CASES[1]
    channels, _, dm = setup(st)
    session = pk_sharc.CallSession([linked], 0xB9F000)
    for address, word in dm.items():
        session.poke(address, word)
    result = session.call("_stereo_h", REGS, max_steps=CEILING + 1)
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    right = [session.peek(RIGHT + i) for i in range(32)]
    assert right == bits(oracle(channels, st)[0][1])
    assert any(word & 0x7FFFFFFF for word in right), "R must carry the right plane"
