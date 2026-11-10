"""The diagnostic's actual consumer, through the machine JUMP/CJUMP bridge.

Sparse packed input covers both ends of the 480000-frame view. These checks use
its real note-on/render code and retain the adapter frame/keep-register guards.
"""

import struct

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import pk_sharc

HERE = REPO_ROOT / "modules/core/adapters/dsp-ddr-proof/src/dsp"
STATE, INPUT, PARAMS, OUT, READY, OBSERVE = [0x08020000 + n * 0x100 for n in range(6)]
CONTROL = {
    name: 0x08022000 + i
    for i, name in enumerate(
        ("admission_open", "admitted", "retire_closed", "admit_fault", "token")
    )
}
SOURCE = 0x10200000
# Snapshot words: MODE1, raw ASTATX, I6, I7, I13, active-before-entry.
FIELDS = ("", "_astatx", "_i6", "_i7", "_i13", "_active")
TRACE_NAMES = ["active", "tail_active"]
for _kind in ("note", "render", "tail"):
    TRACE_NAMES += [f"{_kind}_entries", f"{_kind}_exits"]
    TRACE_NAMES += [
        f"{_kind}_{which}{field}" for which in ("first", "last") for field in FIELDS
    ]
TRACE = {name: 0x08021000 + i for i, name in enumerate(TRACE_NAMES)}
RETURN = 0xB9F000
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
    "I7": 0x2009C000,
    "I13": RETURN,
    "MODE1": 0x8000,
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


def sample(i):
    return ((i * 137) % 65536) - 32768


def bits(i):
    return struct.unpack("<I", struct.pack("<f", sample(i) / 32768))[0]


@pytest.fixture(scope="module")
def consumer():
    text = "\n".join(
        [
            '#include "dk_machine.inc"',
            '#include "dsp/call.inc"',
            '#include "dsp/sample.inc"',
            '#include "dsp/sample_pcm16.inc"',
            '#include "playback.inc"',
        ]
    )
    externs = {"_dd_ready": READY}
    externs.update(
        {
            f"_dd_play_{name}": OBSERVE + i
            for i, name in enumerate(
                ("state", "notes", "blocks", "done", "output", "reads")
            )
        }
    )
    externs.update({f"_dd_obs_{name}": addr for name, addr in TRACE.items()})
    externs.update({f"_dd_{name}": addr for name, addr in CONTROL.items()})
    return pk_sharc.build(
        pk_sharc.expand_includes(
            text, [REPO_ROOT / "sdk/asm", REPO_ROOT / "sdk", HERE]
        ),
        0xB90000,
        externs,
    )[1]


def voice(consumer, ready=1, reverse=False):
    sess = pk_sharc.CallSession([consumer], RETURN)
    for i in [*range(0, 128, 2), *range(479872, 480000, 2)]:
        sess.poke(
            SOURCE + i // 2, (sample(i) & 65535) | ((sample(i + 1) & 65535) << 16)
        )
    sess.poke(READY, ready)
    sess.poke(PARAMS, 256 if reverse else 0)
    sess.poke(INPUT + 3, OUT)
    for i in range(6):
        sess.poke(OBSERVE + i, 0)
    for addr in TRACE.values():
        sess.poke(addr, 0)
    for addr in CONTROL.values():
        sess.poke(addr, 0)
    sess.poke(CONTROL["admission_open"], int(ready == 1))
    sess.poke(CONTROL["token"], 0x81234567)
    sess.proof_seeded = False
    return sess


def call(sess, entry, context=None):
    # The DN2 adapter enters with I7==I6, an exclusive boundary. Keep a guard
    # there; the bridge must reserve space before the kernel's postmodify pushes.
    sess.poke(REGS["I7"] // 4, 0xABCD1234)
    regs = REGS if not sess.proof_seeded else {}
    r = sess.call(
        entry,
        {**regs, **(context or {}), "R4": STATE, "R8": INPUT, "R12": PARAMS},
        read_regs=KEEP,
    )
    sess.proof_seeded = True
    assert r["halt"]["reason"] == "breakpoint", r["halt"]
    assert all(r["regs"][n] == REGS[n] for n in KEEP)
    assert sess.peek(REGS["I7"] // 4) == 0xABCD1234
    assert r["instructions"] <= (2600 if "note_on" in entry else 2400)
    return r


@pytest.mark.parametrize("ready", [0, 2, 0xFFFFFFFF])
def test_unready_note_is_silent_done_and_does_not_adopt_later_ready(consumer, ready):
    sess = voice(consumer, ready)
    call(sess, "_dd_play_note_on")
    sess.poke(READY, 1)
    sess.record_reads(SOURCE, 240000)
    r = call(sess, "_dd_play_render")
    assert not sess.stores
    assert [sess.peek(OUT + i) for i in range(32)] == [0] * 32
    assert r["regs"]["R0"] == 1
    assert sess.peek(STATE + 1) == 0


@pytest.mark.parametrize("reverse", [False, True])
def test_full_view_direction_final_block_done_and_retrigger(consumer, reverse):
    sess = voice(consumer, reverse=reverse)
    for _ in range(2):
        call(sess, "_dd_play_note_on")
        assert sess.peek(STATE + 1) == 480000
        assert sess.peek(STATE + 2) == (479999 if reverse else 0)
        assert sess.peek(STATE + 4) == ((-65536 if reverse else 65536) & 0xFFFFFFFF)
        sess.poke(READY, 0)  # Admission is latched; the owned source is immutable.
        sess.poke(PARAMS, 0 if reverse else 256)
        r = call(sess, "_dd_play_render")
        indices = range(479999, 479967, -1) if reverse else range(32)
        assert [sess.peek(OUT + i) for i in range(32)] == [bits(i) for i in indices]
        assert r["regs"]["R0"] == 0
        sess.poke(STATE + 2, 31 if reverse else 479968)
        r = call(sess, "_dd_play_render")
        indices = range(31, -1, -1) if reverse else range(479968, 480000)
        assert [sess.peek(OUT + i) for i in range(32)] == [bits(i) for i in indices]
        assert sess.peek(STATE + 2) == (0xFFFFFFFF if reverse else 480000)
        assert sess.peek(STATE + 5) == 1
        assert r["regs"]["R0"] == 1
        sess.record_reads(SOURCE, 240000)
        assert call(sess, "_dd_play_render")["regs"]["R0"] == 1
        assert not sess.stores
        assert [sess.peek(OUT + i) for i in range(32)] == [0] * 32
        sess.poke(READY, 1)
        sess.poke(PARAMS, 256 if reverse else 0)


def test_observer_keeps_first_and_latest_context_without_changing_playback(consumer):
    sess = voice(consumer)
    first_astat = 0x12345000
    last_astat = 0x23456000
    call(sess, "_dd_play_note_on", {"ASTATX": first_astat})
    call(sess, "_dd_play_note_on", {"ASTATX": last_astat})
    expected = [REGS["MODE1"], first_astat, REGS["I6"], REGS["I7"], RETURN, 0]
    assert [sess.peek(TRACE[f"note_first{f}"]) for f in FIELDS] == expected
    expected[1] = last_astat
    assert [sess.peek(TRACE[f"note_last{f}"]) for f in FIELDS] == expected
    assert sess.peek(TRACE["note_entries"]) == 2
    assert sess.peek(TRACE["note_exits"]) == 2
    # Seed an observed outer consumer; this tests marker bookkeeping only,
    # without claiming that a native run has actually preempted a callback.
    sess.poke(TRACE["active"], 1)
    call(sess, "_dd_play_render", {"ASTATX": first_astat})
    assert sess.peek(TRACE["render_last_active"]) == 1
    assert sess.peek(TRACE["active"]) == 1
    assert sess.peek(TRACE["render_entries"]) == 1
    assert sess.peek(TRACE["render_exits"]) == 1
    assert [sess.peek(OUT + i) for i in range(32)] == [bits(i) for i in range(32)]


@pytest.mark.parametrize("mode", [0x8000, 0x9000])
def test_terminal_closed_callbacks_have_no_source_reads_and_keep_incoming_mode(
    consumer, mode
):
    sess = voice(consumer)
    call(sess, "_dd_play_note_on")
    call(sess, "_dd_play_render")
    sess.poke(CONTROL["admission_open"], 0)
    sess.poke(CONTROL["retire_closed"], 1)
    sess.poke(READY, 0)
    sess.record_reads(SOURCE, 240000)
    # A cached active render, delayed note, held/retriggered note and armed-like
    # late render all pass the same terminal admission check.
    for entry in (
        "_dd_play_render",
        "_dd_play_note_on",
        "_dd_play_render",
        "_dd_play_note_on",
        "_dd_play_render",
    ):
        r = sess.call(
            entry,
            {**REGS, "MODE1": mode, "R4": STATE, "R8": INPUT, "R12": PARAMS},
            read_regs=KEEP,
        )
        assert r["halt"]["reason"] == "breakpoint"
        assert r["regs"]["MODE1"] == mode
        assert all(r["regs"][n] == REGS[n] for n in KEEP if n != "MODE1")
        assert not sess.stores
        assert sess.peek(STATE + 1) == 0
        assert sess.peek(STATE + 5) == 1
        assert sess.peek(STATE + 6) == 0
        assert sess.peek(CONTROL["admitted"]) == 0
    assert [sess.peek(OUT + i) for i in range(32)] == [0] * 32
    assert sess.peek(OBSERVE + 5) == 1


def test_counter_corruption_fails_closed_without_wrapping_to_drained(consumer):
    sess = voice(consumer)
    sess.poke(CONTROL["admitted"], 0xFFFFFFFF)
    sess.record_reads(SOURCE, 240000)
    call(sess, "_dd_play_render")
    assert not sess.stores
    assert sess.peek(CONTROL["admitted"]) == 0xFFFFFFFF
    assert sess.peek(CONTROL["retire_closed"]) == 1
    assert sess.peek(CONTROL["admit_fault"]) == 1
    assert sess.peek(CONTROL["admission_open"]) == 0


@pytest.mark.parametrize("mode", [0x8000, 0x9000])
def test_cached_voice_rejects_new_generation_until_fresh_note(consumer, mode):
    sess = voice(consumer)
    call(sess, "_dd_play_note_on")
    old_token = sess.peek(STATE + 6)
    assert old_token == sess.peek(CONTROL["token"])
    call(sess, "_dd_play_render")
    reads = sess.peek(OBSERVE + 5)
    sess.poke(CONTROL["token"], old_token + 1)
    sess.record_reads(SOURCE, 240000)
    for _ in range(2):
        r = sess.call(
            "_dd_play_render",
            {**REGS, "MODE1": mode, "R4": STATE, "R8": INPUT, "R12": PARAMS},
            read_regs=KEEP,
        )
        assert r["halt"]["reason"] == "breakpoint"
        assert r["regs"]["MODE1"] == mode
        assert all(r["regs"][n] == REGS[n] for n in KEEP if n != "MODE1")
        assert not sess.stores
        assert sess.peek(STATE + 6) == 0
        assert sess.peek(STATE + 5) == 1
        assert sess.peek(CONTROL["admitted"]) == 0
        assert sess.peek(OBSERVE + 5) == reads
        assert [sess.peek(OUT + i) for i in range(32)] == [0] * 32
    # Reopening is modeled only by the control words; this is not a wire test.
    sess.poke(CONTROL["token"], old_token)
    sess.proof_seeded = False  # Resume the common helper with its expected MODE1.
    call(sess, "_dd_play_render")
    assert not sess.stores  # An invalidated cached voice cannot revive.
    sess.poke(CONTROL["token"], old_token + 1)
    call(sess, "_dd_play_note_on")
    assert sess.peek(STATE + 6) == old_token + 1
    call(sess, "_dd_play_render")
    assert sess.stores
    assert [sess.peek(OUT + i) for i in range(32)] == [bits(i) for i in range(32)]


@pytest.mark.parametrize(
    "denial", ["admission_open", "retire_closed", "token", "ready"]
)
def test_denied_note_cannot_arm_after_gate_changes(consumer, denial):
    sess = voice(consumer)
    call(sess, "_dd_play_note_on")
    sess.poke(
        CONTROL[denial] if denial != "ready" else READY,
        1 if denial == "retire_closed" else 0,
    )
    call(sess, "_dd_play_note_on")
    assert sess.peek(STATE + 6) == 0
    assert sess.peek(STATE + 5) == 1
    sess.poke(READY, 1)
    sess.poke(CONTROL["admission_open"], 1)
    sess.poke(CONTROL["retire_closed"], 0)
    sess.poke(CONTROL["token"], 0x81234568)
    # An armed-like cursor still lacks an accepted note's private generation.
    sess.poke(STATE + 1, 480000)
    sess.poke(STATE + 5, 0)
    sess.record_reads(SOURCE, 240000)
    call(sess, "_dd_play_render")
    assert not sess.stores
    assert [sess.peek(STATE + i) for i in range(7)] == [0, 0, 0, 0, 0, 1, 0]
    assert sess.peek(OBSERVE + 5) == 0
