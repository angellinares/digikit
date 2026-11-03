"""Assembled lifecycle primitives with independent state/register/label bindings."""

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import pk_sharc

RETURN = 0xB9F000
NAMES = ("open", "closed", "count", "fault", "ready", "token")
BANKS = {
    "cedar": (0x08023000, "R7", "R8", "R9", "I1"),
    "maple": (0x08024000, "R10", "R11", "R12", "I2"),
}
STATE = {"cedar": 0x08025000, "maple": 0x08025100}


def source(name, saved, tmp, other, state):
    fields = {field: f"_{name}_{field}" for field in NAMES}
    op, closed, count, fault, ready, token = (fields[n] for n in NAMES)
    lines = []
    for action in ("fresh", "cached", "release", "close", "publish"):
        label = f"_{name}_{action}"
        lines += [
            f".GLOBAL {label};",
            f"{label}:",
            f"{state} = R4;",
            f"DK_SAMPLE_BANK_LOCK({saved});",
        ]
        if action in ("fresh", "cached"):
            lines += [
                f"DK_SAMPLE_BANK_CHECK_ADMISSION({op}, {closed}, {count}, {tmp}, {other}, {label}_denied, {label}_fault);",
                (
                    f"DK_SAMPLE_BANK_LATCH_EPOCH({ready}, {token}, {state}, 3, {other}, {tmp}, {label}_denied);"
                    if action == "fresh"
                    else f"DK_SAMPLE_BANK_CHECK_EPOCH({token}, {state}, 3, {other}, {tmp}, {label}_denied);"
                ),
                f"DK_SAMPLE_BANK_ACQUIRE({count}, {tmp});",
                f"JUMP {label}_ok;",
                f"{label}_fault:",
                f"DK_SAMPLE_BANK_FAIL_CLOSED({fault}, {closed}, {op}, {ready}, {tmp});",
                f"{label}_denied:",
                "R0 = 1;",
                f"JUMP {label}_out;",
            ]
        elif action == "release":
            lines += [
                f"DK_SAMPLE_BANK_RELEASE({count}, {tmp}, {label}_fault, {label}_ok);",
                f"{label}_fault:",
                f"DK_SAMPLE_BANK_FAIL_CLOSED({fault}, {closed}, {op}, {ready}, {tmp});",
                f"DK_SAMPLE_BANK_POISON_COUNT({count}, {tmp});",
                "R0 = 1;",
                f"JUMP {label}_out;",
            ]
        else:
            if action == "close":
                lines += [f"DK_SAMPLE_BANK_CLOSE({op}, {ready}, {closed}, {tmp});"]
            lines += [
                f"DK_SAMPLE_BANK_CHECK_DRAIN({count}, {fault}, {tmp}, {other}, {label}_pending);"
            ]
            if action == "publish":
                lines += [f"DK_SAMPLE_BANK_PUBLISH({op}, {ready}, {tmp});"]
            lines += [
                f"JUMP {label}_ok;",
                f"{label}_pending:",
                "R0 = 1;",
                f"JUMP {label}_out;",
            ]
        lines += [
            f"{label}_ok:",
            "R0 = 0;",
            f"{label}_out:",
            f"DK_SAMPLE_BANK_UNLOCK({saved});",
            "JUMP (M13, I13);",
        ]
    return "\n".join(lines)


@pytest.fixture(scope="module")
def banks():
    # Repeated inclusion exercises the guard in the same assembled object.
    text = "\n".join(
        [
            '#include "dsp/sample_bank.inc"',
            '#include "dsp/sample_bank.inc"',
            ".SECTION/PM seg_pmco;",
            *(source(name, *regs[1:]) for name, regs in BANKS.items()),
        ]
    )
    externs = {
        f"_{name}_{field}": regs[0] + i
        for name, regs in BANKS.items()
        for i, field in enumerate(NAMES)
    }
    return pk_sharc.build(
        pk_sharc.apply_conditionals(
            pk_sharc.expand_includes(text, [REPO_ROOT / "sdk"])
        ),
        0xB90000,
        externs,
    )[1]


def memory(sess, bank):
    return {n: sess.peek(BANKS[bank][0] + i) for i, n in enumerate(NAMES)}


def setup(banks):
    sess = pk_sharc.CallSession([banks], RETURN)
    for i, (name, regs) in enumerate(BANKS.items()):
        for offset, value in enumerate((1, 0, 0, 0, 1, 73 + i)):
            sess.poke(regs[0] + offset, value)
        sess.poke(STATE[name] + 3, 0)
    return sess


def invoke(sess, bank, action, mode):
    # No hidden general registers or pointer clobbers; only each binding's
    # named scratch and state registers and the fixture's R0 result may change.
    regs = {
        **{f"R{i}": 0x300 + i for i in range(16)},
        **{f"I{i}": 0x08026000 + i * 16 for i in range(16)},
        "R4": STATE[bank],
        "I13": RETURN,
        "M13": 0,
        "MODE1": mode,
    }
    result = sess.call(f"_{bank}_{action}", regs, read_regs=tuple(regs))
    assert result["halt"]["reason"] == "breakpoint", result["halt"]
    clobbered = {"R0", *BANKS[bank][1:]}
    assert all(
        result["regs"][r] == value for r, value in regs.items() if r not in clobbered
    )
    assert result["regs"]["MODE1"] == mode
    return result["regs"]["R0"]


@pytest.mark.parametrize("bank", BANKS)
@pytest.mark.parametrize("mode", [0x8000, 0x9000])
def test_fresh_cached_close_drain_and_new_generation(banks, bank, mode):
    sess = setup(banks)
    other = next(n for n in BANKS if n != bank)
    untouched = memory(sess, other)
    base = BANKS[bank][0]
    token = memory(sess, bank)["token"]
    assert invoke(sess, bank, "cached", mode) == 1
    assert invoke(sess, bank, "fresh", mode) == 0
    assert sess.peek(STATE[bank] + 3) == token
    assert memory(sess, bank)["count"] == 1
    assert invoke(sess, bank, "cached", mode) == 0
    assert memory(sess, bank)["count"] == 2
    assert invoke(sess, bank, "release", mode) == 0
    assert invoke(sess, bank, "close", mode) == 1
    assert memory(sess, bank) == dict(
        open=0, closed=1, count=1, fault=0, ready=0, token=token
    )
    assert invoke(sess, bank, "cached", mode) == 1
    assert invoke(sess, bank, "release", mode) == 0
    assert invoke(sess, bank, "close", mode) == 0
    # Serialized producer initializes a new generation only after exact drain.
    sess.poke(base + NAMES.index("token"), token + 1)
    sess.poke(base + NAMES.index("closed"), 0)
    assert invoke(sess, bank, "publish", mode) == 0
    assert invoke(sess, bank, "cached", mode) == 1
    assert sess.peek(STATE[bank] + 3) == token
    assert invoke(sess, bank, "fresh", mode) == 0
    assert sess.peek(STATE[bank] + 3) == token + 1
    assert invoke(sess, bank, "release", mode) == 0
    assert memory(sess, other) == untouched
    assert sess.peek(STATE[other] + 3) == 0


@pytest.mark.parametrize("bank", BANKS)
@pytest.mark.parametrize("bad_count", [0xFFFFFFFF, 0x7FFFFFFF])
def test_bad_admission_count_fails_closed_and_cannot_publish(banks, bank, bad_count):
    sess = setup(banks)
    other = next(n for n in BANKS if n != bank)
    untouched = memory(sess, other)
    sess.poke(BANKS[bank][0] + NAMES.index("count"), bad_count)
    assert invoke(sess, bank, "fresh", 0x9000) == 1
    status = memory(sess, bank)
    assert (status["open"], status["closed"], status["ready"], status["fault"]) == (
        0,
        1,
        0,
        1,
    )
    assert status["count"] == bad_count
    assert sess.peek(STATE[bank] + 3) == 0
    assert invoke(sess, bank, "close", 0x8000) == 1
    assert invoke(sess, bank, "publish", 0x9000) == 1
    assert memory(sess, other) == untouched


@pytest.mark.parametrize("bank", BANKS)
@pytest.mark.parametrize("count", [0, 0xFFFFFFFF])
def test_corrupt_release_never_becomes_drained(banks, bank, count):
    sess = setup(banks)
    sess.poke(BANKS[bank][0] + NAMES.index("count"), count)
    assert invoke(sess, bank, "release", 0x9000) == 1
    status = memory(sess, bank)
    assert (status["open"], status["closed"], status["ready"], status["fault"]) == (
        0,
        1,
        0,
        1,
    )
    # Count poison alone is insufficient: even if count is later cleared, the
    # sticky fault still prevents a drain/publication claim.
    assert status["count"] > 0
    sess.poke(BANKS[bank][0] + NAMES.index("count"), 0)
    assert invoke(sess, bank, "close", 0x8000) == 1
    assert invoke(sess, bank, "publish", 0x9000) == 1


@pytest.mark.parametrize("ready,token", [(0, 73), (2, 73), (1, 0)])
def test_fresh_rejects_unready_or_zero_epoch_without_acquire(banks, ready, token):
    sess = setup(banks)
    base = BANKS["cedar"][0]
    sess.poke(base + NAMES.index("ready"), ready)
    sess.poke(base + NAMES.index("token"), token)
    assert invoke(sess, "cedar", "fresh", 0x9000) == 1
    assert memory(sess, "cedar")["count"] == 0
    assert sess.peek(STATE["cedar"] + 3) == 0
