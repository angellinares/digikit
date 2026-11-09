"""Numerical checks of the fixture's actual note-on/render consumer, no device IO."""

import struct
from pathlib import Path

import pytest

from tools.patchkit import pk_sharc

ROOT = Path(__file__).resolve().parents[5]
HERE = Path(__file__).resolve().parents[1]
VIEW, SOURCE, STATE, INPUT, PARAMS, OUT, STACK = [
    0x08020000 + n * 0x100 for n in range(7)
]
RETURN = 0xB9F000
# Actual adapters enter through JUMP with byte-alias I6 at the caller's
# frame, I7 below their own frame, M7=-1 and a separate I13 return target.
# Keep-register sentinels expose the kernel's I12 scratch and stack leaks.
REGS = {
    **{f"I{i}": 0x20098000 + i * 64 for i in range(8, 16)},
    **{f"M{i}": i * 17 for i in range(16)},
    **{f"L{i}": 0 for i in range(8)},
    "M0": 0,
    "M5": 0,
    "M6": 1,
    "M7": 0xFFFFFFFF,
    "M13": 0,
    "M14": 1,
    "M15": 0xFFFFFFFF,
    "I6": 0x2009C000,
    "I7": 0x2009C000 - 40 * 4,
    "I13": RETURN,
    "MODE1": 0x00008000,
}
KEEP = (
    "I6",
    "I7",
    *[f"I{i}" for i in range(8, 16)],
    "M0",
    *[f"M{i}" for i in range(5, 16)],
    *[f"L{i}" for i in range(8)],
    "MODE1",
)


def bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


@pytest.fixture(scope="module", params=[40, 0], ids=["dt2-frame", "dn2-frame"])
def linked(request):
    text = "\n".join(
        [
            '#include "dk_machine.inc"',
            '#include "dsp/call.inc"',
            '#include "dsp/sample.inc"',
            '#include "playback.inc"',
        ]
    )
    expanded = pk_sharc.expand_includes(
        text, [ROOT / "sdk/asm", ROOT / "sdk", HERE / "src/dsp"]
    )
    return pk_sharc.build(expanded, 0xB90000, {"_sp_view": VIEW})[1], request.param


def voice(linked, *, ready=1, start=0, end=127, reverse=0):
    code, frame_words = linked
    session = pk_sharc.CallSession([code], RETURN)
    session.proof_regs = {**REGS, "I7": REGS["I6"] - frame_words * 4}
    session.proof_frame_words = frame_words
    session.proof_seeded = False
    source = [((n * 71) % 101 - 50) / 128 for n in range(64)]
    for i, value in enumerate(source):
        session.poke(SOURCE + i, bits(value))
    for i, value in enumerate([ready, SOURCE, len(source), 48000]):
        session.poke(VIEW + i, value)
    for i, value in enumerate([start * 256, end * 256, reverse * 256]):
        session.poke(PARAMS + i, value)
    session.poke(INPUT + 3, OUT)
    return session, source


def call(session, entry):
    expected = session.proof_regs
    frame = expected["I7"] // 4
    count = session.proof_frame_words
    for i in range(count):
        session.poke(frame + i, 0xABCD0000 + i)
    # Seed the adapter context once. Subsequent calls update only arguments,
    # so no reinitialization can conceal a leaked stack or keep register.
    regs = expected if not session.proof_seeded else {}
    result = session.call(
        entry, {**regs, "R4": STATE, "R8": INPUT, "R12": PARAMS}, read_regs=KEEP
    )
    session.proof_seeded = True
    assert result["halt"]["reason"] == "breakpoint"
    assert {name: result["regs"][name] for name in KEEP} == {
        name: expected[name] for name in KEEP
    }
    assert [session.peek(frame + i) for i in range(count)] == [
        0xABCD0000 + i for i in range(count)
    ]
    return result


def note(session):
    call(session, "_sp_note_on")


def render(session):
    result = call(session, "_sp_render")
    return [session.peek(OUT + i) for i in range(32)], result["regs"]["R0"]


@pytest.mark.parametrize(
    "start,end,reverse",
    [(0, 127, 0), (7, 39, 0), (7, 39, 127), (63, 127, 127), (31, 31, 0)],
)
def test_note_bounds_direction_completion_and_retrigger(linked, start, end, reverse):
    session, source = voice(linked, start=start, end=end, reverse=reverse)
    wanted = source[min(start, 64) : min(end, 64)]
    if reverse >= 64:
        wanted = wanted[::-1]
    for _ in range(2):
        note(session)
        # Parameters and READY/source changes after note-on cannot mutate the
        # latched immutable voice. Retrigger latches again after restoration.
        session.poke(PARAMS, 127 * 256)
        session.poke(VIEW, 0)
        got, done = [], 0
        for _ in range(3):
            block, done = render(session)
            got.extend(block)
        assert got == [bits(v) for v in wanted] + [0] * (96 - len(wanted))
        assert done == 1
        session.poke(PARAMS, start * 256)
        session.poke(VIEW, 1)


def test_before_ready_is_silent_and_done_even_with_bad_source_pointer(linked):
    session, _ = voice(linked, ready=0)
    session.poke(VIEW + 1, 0xFFFFFFFF)
    note(session)
    assert render(session) == ([0] * 32, 1)


@pytest.mark.parametrize("device", ["dt2", "dn2"])
def test_actual_adapter_entry_stack_matches_ownership_boundary(device):
    from support import dsp_voice_rig as dv
    from support.machine_rig import MachineRig

    rig = MachineRig("test/sample-proof", device, record_stores=True)
    entry = rig.built.symbols["_sp_render"]
    rig.sess.runner.breakpoints |= frozenset([entry])
    rig.trigger()
    for _ in range(3):
        block = rig.block("friendly")
        if block.out["final_pc_sw"] == entry:
            break
    else:
        pytest.fail("actual adapter did not enter the fixture render")
    observed = block.out["regs"]["I7"]
    assert observed == dv.STACK - (160 if device == "dt2" else 0)
    _, output = rig._regs(dv.FRIENDLY)
    stack = next(
        region for region in rig._allowed(output) if region[2] == "module stack"
    )
    assert stack[:2] == (observed - rig.built.desc[9], rig.built.desc[9])

    # Run the same real adapter and consumer without the observation stop.
    # Keep the ownership checks active: kernel stack stores must be permitted,
    # while adapter data/frame, input, parameters and other voices stay guarded.
    rig.reset()
    rig.trigger()
    saw_module_store = False
    for _ in range(3):
        block = rig.block("friendly")
        assert block.returned, block.halt
        assert block.context_bad == []
        assert block.foreign == []
        saw_module_store |= any(
            lo <= event.pc_sw < hi
            for event in rig.sess.stores
            for lo, hi in rig.built.module_code
        )
    assert saw_module_store
