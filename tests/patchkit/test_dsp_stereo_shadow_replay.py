"""Assembled synthetic DN2 subset; no claim about the stock default route.

RECIPS and RSQRTS use the measured seed tables of the core (finding 102), so
the divisions of 1c06ba are exact. The separate filter leaves use byte-space
pointers with assume_nw32=True for their stock 32-bit DM access context. No
instruction semantics are modified.

Original firmware is supplied at runtime, never copied into a fixture. Reference
channels execute independently on separate concrete cores with private state.
"""

import hashlib
import struct
from collections import Counter

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import pk_sharc
from tools.sharc_dev.sharc_core.addressing import normal_word_to_byte
from tools.sharc_dev.sharc_core.memory import _canonical_dm_address
from tools.sharc_dev.sharcldr import LoadedMemory

SOURCE = REPO_ROOT / "tests/fixtures/dsp/stereo-shadow-replay.s"
IMAGE = REPO_ROOT / "out/sections/dn2-1.12/section_7_BLOB.bin"
BACK, STACK = 0xB9F000, 0x08044000
CONTROL, FIRST, UY = 0x08040000, 0x08040200, 0x08040400
LEFT, RIGHT = 0x08041000, 0x08041100
REGS = {
    "I6": STACK,
    "I7": STACK,
    "M5": 0,
    "M6": 1,
    "M7": 0xFFFFFFFF,
    "M14": 1,
    "MODE1": 0,
}
REGS.update({f"L{i}": 0 for i in range(16)})


def bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


@pytest.fixture(scope="module")
def rig():
    if not IMAGE.exists():
        pytest.skip("requires original DN2 1.12 section 7")
    data = IMAGE.read_bytes()
    assert (
        hashlib.sha256(data).hexdigest()
        == "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
    )
    original = LoadedMemory.from_stream(data)
    source = pk_sharc.expand_includes(SOURCE.read_text(), [REPO_ROOT / "sdk"])
    linked = pk_sharc.link(
        pk_sharc.assemble(source),
        0xB90000,
        {
            "_control": 0x1CDB56,
            "_first": 0xB80C39,
            "_uy": 0xB81368,
        },
    )
    return linked, original


def fresh(rig):
    linked, memory = rig
    session = pk_sharc.CallSession([linked], BACK, base_memory=memory)
    session.record_stores()
    return session


def call(session, entry):
    result = session.call(entry, REGS, max_steps=12000)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    # Canonical byte-address watchpoints bound every executed store, including
    # nested stock frames. Preparation may copy controls/coefficients only.
    if entry == "_prepare":
        allowed = {CONTROL + 0x100 + i for i in range(14)}
        allowed |= {FIRST + 0x100 + i for i in range(21, 24)}
        allowed |= {UY + 0x100 + i for i in range(2, 6)}
    else:
        offset = 0x100 if entry in ("_right", "_right_recurrences") else 0
        allowed = {LEFT + offset + i for i in range(32)}
        allowed |= {CONTROL + offset + i for i in (4, 5, 6, 11, 12, 13)}
        allowed |= {FIRST + offset + i for i in (24, 25)}
        allowed |= {UY + offset + i for i in (6, 7)}
        allowed |= set(range(STACK - 64, STACK + 1))
    byte_addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in allowed
    }
    assert all(
        event.width == 4 and event.address in byte_addresses for event in session.stores
    ), [(hex(e.address), e.width) for e in session.stores]
    if entry in ("_left", "_right"):
        counts = Counter(byte_addresses[event.address] for event in session.stores)
        assert [counts[LEFT + offset + i] for i in range(32)] == [4] * 32
        assert [counts[CONTROL + offset + i] for i in (4, 5, 6, 11, 12, 13)] == [2] * 6
    return result


def initialize(session):
    for base, count in ((CONTROL, 14), (FIRST, 28), (UY, 8)):
        for i in range(count):
            session.poke(base + i, 0)
    for i, value in {0: 0.8, 1: 0.3, 2: 0.6, 5: 0.2, 12: 0.1}.items():
        session.poke(CONTROL + i, bits(value))
    for i, value in {21: 0.7, 22: 0.2, 23: 0.1}.items():
        session.poke(FIRST + i, bits(value))
    for i, value in {2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4}.items():
        session.poke(UY + i, bits(value))


def words(session, base, count):
    return [session.peek(base + i) for i in range(count)]


def test_multiblock_shadow_matches_isolated_mono(rig):
    stereo, left, right = (fresh(rig) for _ in range(3))
    for session in (stereo, left, right):
        initialize(session)
    # Initialize private right histories explicitly; preparation must retain them.
    for base, indices in ((FIRST + 0x100, (24, 25)), (UY + 0x100, (6, 7))):
        for i in indices:
            stereo.poke(base + i, 0)
    first_snapshot = None
    for iteration, block in enumerate((0, 1, 2, 0)):
        if iteration == 3:
            # Explicit reset of BOTH audio histories and shared controls. The
            # same block must reproduce the first execution exactly.
            for session in (stereo, left, right):
                initialize(session)
            for base, indices in ((FIRST + 0x100, (24, 25)), (UY + 0x100, (6, 7))):
                for i in indices:
                    stereo.poke(base + i, 0)
        for session in (stereo, left, right):
            session.poke(CONTROL, bits(0.8 - 0.1 * block))
            session.poke(FIRST + 21, bits(0.7 - 0.05 * block))
            session.poke(UY + 2, bits(0.6 + 0.02 * block))
        for i in range(32):
            left_bits, right_bits = (
                bits((i - 11) / 64 + block / 32),
                bits((7 - i) / 83 - block / 64),
            )
            stereo.poke(LEFT + i, left_bits)
            stereo.poke(RIGHT + i, right_bits)
            left.poke(LEFT + i, left_bits)
            right.poke(LEFT + i, right_bits)
        call(stereo, "_prepare")
        call(stereo, "_left")
        authoritative = words(stereo, CONTROL, 14)
        call(stereo, "_right")
        call(left, "_left")
        call(right, "_left")
        assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
        assert words(stereo, RIGHT, 32) == words(right, LEFT, 32)
        assert words(stereo, CONTROL, 14) == authoritative == words(left, CONTROL, 14)
        assert words(stereo, CONTROL + 0x100, 14) == words(right, CONTROL, 14)
        assert words(stereo, LEFT, 32) != words(stereo, RIGHT, 32)
        for base in (FIRST, UY):
            # Ignore unaudited padding: only coefficients/histories are used.
            indices = range(21, 26) if base == FIRST else range(2, 8)
            assert [stereo.peek(base + i) for i in indices] == [
                left.peek(base + i) for i in indices
            ]
            assert [stereo.peek(base + 0x100 + i) for i in indices] == [
                right.peek(base + i) for i in indices
            ]

        snapshot = (
            words(stereo, LEFT, 32),
            words(stereo, RIGHT, 32),
            words(stereo, CONTROL, 14),
            words(stereo, CONTROL + 0x100, 14),
            [
                stereo.peek(base + offset + i)
                for base, indices in ((FIRST, (24, 25)), (UY, (6, 7)))
                for offset in (0, 0x100)
                for i in indices
            ],
        )
        if iteration == 0:
            first_snapshot = snapshot
        elif iteration == 3:
            assert snapshot == first_snapshot


def test_recurrences_default_core_matches_independent_channels(rig):
    stereo, left, right = (fresh(rig) for _ in range(3))
    for session in (stereo, left, right):
        initialize(session)
    for base, indices in ((FIRST + 0x100, (24, 25)), (UY + 0x100, (6, 7))):
        for i in indices:
            stereo.poke(base + i, 0)
    for block in range(3):
        for session in (stereo, left, right):
            session.poke(FIRST + 21, bits(0.7 - block / 32))
            session.poke(UY + 2, bits(0.6 + block / 64))
        for i in range(32):
            left_bits, right_bits = (
                bits((i - 13) / 64 + block / 32),
                bits((11 - i) / 89),
            )
            stereo.poke(LEFT + i, left_bits)
            stereo.poke(RIGHT + i, right_bits)
            left.poke(LEFT + i, left_bits)
            right.poke(LEFT + i, right_bits)
        call(stereo, "_prepare")
        call(stereo, "_recurrences")
        call(stereo, "_right_recurrences")
        call(left, "_recurrences")
        call(right, "_recurrences")
        for reference, output, offset in ((left, LEFT, 0), (right, RIGHT, 0x100)):
            assert words(stereo, output, 32) == words(reference, LEFT, 32)
            for base, indices in ((FIRST, (24, 25)), (UY, (6, 7))):
                assert [stereo.peek(base + offset + i) for i in indices] == [
                    reference.peek(base + i) for i in indices
                ]


@pytest.mark.parametrize(
    "fault",
    [
        "copy-advanced-controls",
        "reset-right-history",
        "reset-controls-between-occurrences",
    ],
)
def test_shadow_corruption_is_observable(rig, fault):
    candidate, reference = fresh(rig), fresh(rig)
    for session in (candidate, reference):
        initialize(session)
        for base, indices in ((FIRST + 0x100, (24, 25)), (UY + 0x100, (6, 7))):
            for i in indices:
                session.poke(base + i, 0)
    differences = []
    for block in range(2):
        for session in (candidate, reference):
            session.poke(CONTROL, bits(0.9 - block / 8))
            for i in range(32):
                session.poke(LEFT + i, bits((i - 10) / 64))
                session.poke(RIGHT + i, bits((5 - i) / 73))
            call(session, "_prepare")
            if session is candidate:
                pre_controls = words(session, CONTROL + 0x100, 14)
            call(session, "_left")
        if fault == "copy-advanced-controls":
            # The shadow must start from the pre-left snapshot, not its end.
            call(candidate, "_prepare")
        elif fault == "reset-right-history":
            for base, indices in ((FIRST + 0x100, (24, 25)), (UY + 0x100, (6, 7))):
                for i in indices:
                    candidate.poke(base + i, 0)
        call(reference, "_right")
        if fault == "reset-controls-between-occurrences":
            # Pause after U/Y, before the repeated control occurrence. Reusing
            # the preblock snapshot here loses the first occurrence's writes.
            boundary = rig[0].symbols["_ru"]
            candidate.runner.breakpoints = frozenset((BACK, boundary))
            result = candidate.call("_right", REGS, max_steps=12000)
            assert result["final_pc_sw"] == boundary
            for i, word in enumerate(pre_controls):
                candidate.poke(CONTROL + 0x100 + i, word)
            candidate.runner.breakpoints = frozenset((BACK,))
            result = candidate.runner.run(12000)
            assert result.halt.reason == "breakpoint"
            assert result.final_pc_sw == BACK
        else:
            call(candidate, "_right")
        differences.append(
            (words(candidate, RIGHT, 32), words(candidate, CONTROL, 14))
            != (words(reference, RIGHT, 32), words(reference, CONTROL, 14))
        )
    assert any(differences)


def test_base_image_overlay_and_decode_cache_isolation():
    caller = pk_sharc.build(
        ".SECTION/PM seg_pmco;\n.GLOBAL _entry;\n_entry:\n CALL _callee;\n RTS;",
        0xB90000,
        {"_callee": 0xB91000},
    )[1]
    callees = [
        pk_sharc.build(
            f".SECTION/PM seg_pmco;\n.GLOBAL _callee;\n_callee:\n R0 = {value};\n RTS;",
            0xB91000,
        )[1]
        for value in (7, 19)
    ]
    sessions = []
    for callee in callees:
        data = callee.code_boot + struct.pack("<I", 0x12345678)
        original = LoadedMemory(
            data,
            [
                {
                    "index": 0,
                    "target_address": pk_sharc.SW_ALIAS_BASE + 2 * callee.base_sw,
                    "byte_count": len(callee.code_boot),
                    "fill": False,
                    "payload_offset": 0,
                    "payload_len": len(callee.code_boot),
                },
                {
                    "index": 1,
                    "target_address": LEFT * 4,
                    "byte_count": 4,
                    "fill": False,
                    "payload_offset": len(callee.code_boot),
                    "payload_len": 4,
                },
            ],
        )
        session = pk_sharc.CallSession([caller], BACK, base_memory=original)
        assert session.peek(LEFT) == 0x12345678
        result = session.call("_entry", {}, max_steps=10)
        assert result["halt"]["reason"] == "breakpoint", result
        sessions.append(session)
        # Appended linked code must override the original bytes at the same PC.
        overlaid = pk_sharc.CallSession(
            [caller, callees[1]], BACK, base_memory=original
        )
        assert overlaid.call("_entry", {}, max_steps=10)["regs"]["R0"] == 19
        assert (
            original.read(
                pk_sharc.SW_ALIAS_BASE + 2 * callee.base_sw, len(callee.code_boot)
            )
            == callee.code_boot
        )
    assert [
        session.call("_entry", {}, max_steps=10)["regs"]["R0"] for session in sessions
    ] == [7, 19]
    assert sessions[0].runner._cache is not sessions[1].runner._cache


# These are isolated finite leaves, not dispatch routing or the case-5 blend.
# snapshots = b82342, nine = b81a16, six = b81526. Small coefficients
# and nonzero per-sample increments exercise their inspected float recurrences
# without exceptional values; they are synthetic captured coefficients.
FILTER_FAMILIES = {
    "snapshots": (
        51,
        (3, 4, 48, 49, 50),
        (5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42),
        (0.2, 0.3, 0.1, 0.4, 0.0002, 0.2, 0.0001, 0.1, 0.0001, 0.1, 0.0001),
    ),
    "nine": (
        75,
        (10, 11),
        tuple(12 + 7 * k + delta for k in range(9) for delta in (0, 2)),
        tuple(
            v
            for v0 in (0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1)
            for v in (v0, 0.0001)
        ),
    ),
    "six": (
        53,
        (9, 10),
        tuple(11 + 7 * k + delta for k in range(6) for delta in (0, 2)),
        tuple(v for v0 in (0.6, 0.1, 0.15, 0.2, 0.1, 0.08) for v in (v0, 0.0001)),
    ),
}


@pytest.fixture(scope="module")
def filter_rig(rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-filter-shadow-replay.s"
    prepared = REPO_ROOT / "tests/fixtures/dsp/stereo-prepared-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(
                source.read_text() + "\n" + prepared.read_text(), [REPO_ROOT / "sdk"]
            )
        ),
        0xB92000,
        {
            "_snapshots": 0xB82342,
            "_nine": 0xB81A16,
            "_six": 0xB81526,
            "_held": 0xB8265B,
            "_prepared": 0xB80F2E,
        },
    )
    return linked, rig[1]


def filter_call(session, family, side, histories, coefficients, shared_writes=()):
    entry = f"_{side}_{family}"
    regs = {**REGS, "I6": STACK * 4, "I7": STACK * 4}
    result = session.call(entry, regs, max_steps=12000)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    if side == "prepare":
        allowed = {CONTROL + 0x100 + i for i in coefficients}
    else:
        offset = 0x100 if side == "right" else 0
        allowed = {LEFT + offset + i for i in range(32)}
        allowed |= {CONTROL + offset + i for i in (*histories, *shared_writes)}
        allowed |= set(range(STACK - 64, STACK + 1))
    stores = [e for e in session.stores if e.access == "write"]
    byte_addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in allowed
    }
    assert all(
        event.width == 4 and event.address in byte_addresses for event in stores
    ), [(hex(e.address), e.width) for e in stores if e.address not in byte_addresses]
    counts = Counter(byte_addresses[e.address] for e in stores)
    if side == "prepare":
        assert {a for a, count in counts.items() if count} == allowed
        assert set(counts.values()) == {1}
    else:
        assert [counts[LEFT + offset + i] for i in range(32)] == [1] * 32, (
            result,
            counts,
        )
        assert all(counts[CONTROL + offset + i] for i in histories)
        assert all(counts[CONTROL + offset + i] == 1 for i in shared_writes)
    if side != "prepare" and family in ("held", "prepared"):
        table = 0x26E9C8 if family == "held" else 0x26B3A8
        pcs = (0xB826E0,) if family == "held" else (0xB81053, 0xB81059)
        reads = [e for e in session.stores if e.access == "read" and e.pc_sw in pcs]
        assert len(reads) == (4 if family == "held" else 256)
        table_base = _canonical_dm_address(session.runner.state, table, 4)
        count = 128 if family == "held" else 1024
        assert all(
            e.width == 1
            and e.new_value is not None
            and table_base <= e.address < table_base + 4 * count
            for e in reads
        )
        if family == "held":
            period = struct.unpack("<f", bytes(e.new_value for e in reads))[0]
            assert 1 < period < 32
    return result


@pytest.mark.parametrize("family", FILTER_FAMILIES)
@pytest.mark.parametrize(
    "corrupt_history", (False, True), ids=("independent", "bad-history")
)
def test_filter_leaf_shadow_matches_isolated_mono(filter_rig, family, corrupt_history):
    size, histories, coefficients, values = FILTER_FAMILIES[family]
    stereo, left, right = (fresh(filter_rig) for _ in range(3))
    for session in (stereo, left, right):
        # Stock raw-byte MODIFY interiors require byte-space frame/state pointers.
        # Unqualified stock DM accesses are 32-bit normal words in this context.
        session.runner.state.assume_nw32 = True
    for session in (stereo, left, right):
        for offset in (0, 0x100):
            for i in range(size):
                session.poke(CONTROL + offset + i, 0)
    # Unequal nonzero initial histories also make block zero sensitive to aliasing.
    for n, i in enumerate(histories):
        lv, rv = bits((n + 1) / 64), bits(-(n + 1) / 89)
        stereo.poke(CONTROL + i, lv)
        left.poke(CONTROL + i, lv)
        stereo.poke(CONTROL + 0x100 + i, rv)
        right.poke(CONTROL + i, rv)
    differences = []
    audio_differences = []
    for block in range(3):
        for session in (stereo, left, right):
            for i, value in zip(coefficients, values, strict=True):
                session.poke(CONTROL + i, bits(value * (1 + block / 16)))
        inputs = []
        for i in range(32):
            lv, rv = bits((i - 11) / 64 + block / 32), bits((7 - i) / 83 - block / 64)
            inputs.append((lv, rv))
            stereo.poke(LEFT + i, lv)
            stereo.poke(RIGHT + i, rv)
            left.poke(LEFT + i, lv)
            right.poke(LEFT + i, rv)
        before_right = [stereo.peek(CONTROL + 0x100 + i) for i in histories]
        filter_call(stereo, family, "prepare", histories, coefficients)
        assert [stereo.peek(CONTROL + 0x100 + i) for i in histories] == before_right
        filter_call(stereo, family, "left", histories, coefficients)
        authoritative = words(stereo, CONTROL, size)
        if corrupt_history:
            # Deliberately alias all audited histories/snapshots to the left end state.
            for i in histories:
                stereo.poke(CONTROL + 0x100 + i, stereo.peek(CONTROL + i))
        filter_call(stereo, family, "right", histories, coefficients)
        filter_call(left, family, "left", histories, coefficients)
        filter_call(right, family, "left", histories, coefficients)
        assert (
            words(stereo, CONTROL, size) == authoritative == words(left, CONTROL, size)
        )
        assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
        assert words(left, LEFT, 32) != [v[0] for v in inputs]
        assert words(right, LEFT, 32) != [v[1] for v in inputs]
        assert words(stereo, LEFT, 32) != words(stereo, RIGHT, 32)
        actual = (words(stereo, RIGHT, 32), words(stereo, CONTROL + 0x100, size))
        expected = (words(right, LEFT, 32), words(right, CONTROL, size))
        differences.append(actual != expected)
        audio_differences.append(actual[0] != expected[0])
        assert any(stereo.peek(CONTROL + i) != 0 for i in histories)
        if not corrupt_history:
            assert actual == expected
        for session in (stereo, left, right):
            for output in (LEFT, RIGHT) if session is stereo else (LEFT,):
                assert all(
                    (v & 0x7F800000) != 0x7F800000 for v in words(session, output, 32)
                )
    if corrupt_history:
        assert any(differences)
        assert any(audio_differences)


PREPARED_WRITES = tuple(17 + 7 * k + delta for k in range(6) for delta in (0, 1, 2))
PREPARED_SHARED = (0, 1, 2, 3, 4, 14, *PREPARED_WRITES)


def initialized_prepared_sessions(filter_rig, family):
    sessions = tuple(fresh(filter_rig) for _ in range(3))
    histories = (1, 2) if family == "held" else (5, 9, 10, 11, 12)
    size = 5 if family == "held" else 59
    for session in sessions:
        session.runner.state.assume_nw32 = True
        # Log reads as well as stores so dynamic table accesses are checked at
        # the actual load PCs, including any access outside the declared table.
        session.runner.attach_watchpoints(
            [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
        )
        session.runner.set_watch_active(False)
        for offset in (0, 0x100):
            for i in range(size):
                session.poke(CONTROL + offset + i, 0)
        if family == "held":
            # b82440 resets counters to M13. A finite in-period entry of 1
            # exposes private held values before the next refresh.
            for i in (3, 4):
                session.poke(CONTROL + i, 1)
        else:
            # These are captured finite controls, not a live setter invocation.
            # b811f1 seeds the six current coefficients from these expressions.
            controls = {0: 0.1, 1: 0.3, 2: 0.08, 3: 0.1, 4: 0.6, 14: 0.2}
            current = (
                controls[1],
                controls[0],
                controls[2] / 4,
                controls[3],
                controls[4],
                -controls[14] / 8,
            )
            for i, value in controls.items():
                session.poke(CONTROL + i, bits(value))
            for k, value in enumerate(current):
                for delta in (0, 1):
                    session.poke(CONTROL + 17 + 7 * k + delta, bits(value))
    stereo, left, right = sessions
    for n, i in enumerate(histories):
        lv, rv = bits((n + 1) / 256), bits(-(n + 1) / 311)
        stereo.poke(CONTROL + i, lv)
        left.poke(CONTROL + i, lv)
        stereo.poke(CONTROL + 0x100 + i, rv)
        right.poke(CONTROL + i, rv)
    return sessions, histories, size


@pytest.mark.parametrize("family", ("held", "prepared"))
@pytest.mark.parametrize("fault", (None, "advanced-controls", "shared-history"))
def test_prepared_shadow_matches_isolated_mono(filter_rig, family, fault):
    sessions, histories, size = initialized_prepared_sessions(filter_rig, family)
    stereo, left, right = sessions
    shared = (0, 3, 4) if family == "held" else PREPARED_SHARED
    writes = (3, 4) if family == "held" else PREPARED_WRITES
    audio_differences = []
    previous_histories = [stereo.peek(CONTROL + i) for i in histories]
    for block in range(3):
        for session in sessions:
            if family == "held":
                # Active clamp/index path: trunc(127*c) is 32,48,64. The
                # original table periods are finite and between 6 and 26;
                # each block therefore both holds and refreshes samples.
                session.poke(CONTROL, bits((32 + 16 * block) / 127))
            else:
                # b81024 clips before table indexing. The lookup coordinate is
                # 1023*(0.5 + ramp(-control14/8)*clipped), with |clipped|<=1.
                # Initial/current control14 stays below 0.25, so both adjacent
                # indices remain strictly inside the 1024-word original table.
                for i, value in {
                    0: 0.12,
                    1: 0.32,
                    2: 0.1,
                    3: 0.12,
                    4: 0.65,
                    14: 0.22,
                }.items():
                    session.poke(CONTROL + i, bits(value * (1 + block / 16)))
        inputs = []
        for i in range(32):
            lv, rv = (
                bits((i - 11) / 128 + block / 64),
                bits((7 - i) / 163 - block / 128),
            )
            inputs.append((lv, rv))
            stereo.poke(LEFT + i, lv)
            stereo.poke(RIGHT + i, rv)
            left.poke(LEFT + i, lv)
            right.poke(LEFT + i, rv)
        right_history = [stereo.peek(CONTROL + 0x100 + i) for i in histories]
        filter_call(stereo, family, "prepare", histories, shared)
        assert [stereo.peek(CONTROL + 0x100 + i) for i in histories] == right_history
        filter_call(stereo, family, "left", histories, shared, writes)
        authoritative = words(stereo, CONTROL, size)
        if fault == "advanced-controls":
            filter_call(stereo, family, "prepare", histories, shared)
        elif fault == "shared-history":
            for i in histories:
                stereo.poke(CONTROL + 0x100 + i, stereo.peek(CONTROL + i))
        filter_call(stereo, family, "right", histories, shared, writes)
        filter_call(left, family, "left", histories, shared, writes)
        filter_call(right, family, "left", histories, shared, writes)
        assert (
            words(stereo, CONTROL, size) == authoritative == words(left, CONTROL, size)
        )
        assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
        actual = words(stereo, RIGHT, 32)
        expected = words(right, LEFT, 32)
        audio_differences.append(actual != expected)
        if fault is None:
            assert actual == expected
            assert words(stereo, CONTROL + 0x100, size) == words(right, CONTROL, size)
            assert [stereo.peek(CONTROL + i) for i in shared] == [
                stereo.peek(CONTROL + 0x100 + i) for i in shared
            ]
        assert words(stereo, LEFT, 32) != [v[0] for v in inputs]
        assert expected != [v[1] for v in inputs]
        if family == "held":
            # Every block includes held output and at least one refresh, not
            # merely the disabled copy path or a block with stale held values.
            for output in (words(stereo, LEFT, 32), expected):
                assert any(a == b for a, b in zip(output[:-1], output[1:], strict=True))
                assert len(set(output)) > 1
        assert words(stereo, LEFT, 32) != actual
        current_histories = [stereo.peek(CONTROL + i) for i in histories]
        assert current_histories != previous_histories
        previous_histories = current_histories
        for session in sessions:
            for output in (LEFT, RIGHT) if session is stereo else (LEFT,):
                assert all(
                    (v & 0x7F800000) != 0x7F800000 for v in words(session, output, 32)
                )
    if fault is not None:
        assert any(audio_differences)


@pytest.fixture(scope="module")
def gain_rig(rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-gain-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(source.read_text(), [REPO_ROOT / "sdk"])
        ),
        0xB94000,
        {"_gain": 0xB80345},
    )
    return linked, rig[1]


def gain_call(session, side, control=0, byte_control=0):
    regs = {
        **REGS,
        "I6": STACK * 4,
        "I7": STACK * 4,
        "M13": 0,
        "M15": 0xFFFFFFFF,
        "R0": control,
        "R1": byte_control,
    }
    result = session.call(f"_{side}_gain", regs, max_steps=12000)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    if side == "prepare":
        allowed = {CONTROL + 0x100 + i for i in range(11)}
    else:
        offset = 0x100 if side == "right" else 0
        allowed = {CONTROL + offset + i for i in (7, 9, 10)}
        allowed |= {LEFT + offset + i for i in range(32)}
        allowed |= set(range(STACK - 64, STACK + 1))
    addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in allowed
    }
    stores = [e for e in session.stores if e.access == "write"]
    assert all(e.width == 4 and e.address in addresses for e in stores), stores
    counts = Counter(addresses[e.address] for e in stores)
    if side == "prepare":
        assert set(counts) == allowed
        assert set(counts.values()) == {1}
    else:
        # Unity/hold paths may leave in-place audio untouched. Each stored
        # sample must still occur once and remain inside the 32-word plane.
        assert all(counts[LEFT + offset + i] <= 1 for i in range(32))
        tables = {
            0xB8039C: 0x26A570,
            0xB803D7: 0x26A570,
            0xB80452: 0x26A970,
            0xB8049D: 0x26A770,
            0xB804FC: 0x26A770,
        }
        for pc, table in tables.items():
            reads = [e for e in session.stores if e.access == "read" and e.pc_sw == pc]
            base = _canonical_dm_address(session.runner.state, table, 4)
            assert len(reads) % 4 == 0
            assert all(
                e.width == 1
                and e.new_value is not None
                and base <= e.address < base + 128 * 4
                for e in reads
            )
    return result


@pytest.mark.parametrize(
    "advanced_controls", (False, True), ids=("snapshot", "advanced")
)
@pytest.mark.parametrize(
    "hold_release", (False, True), ids=("attack-decay", "hold-release")
)
def test_gain_shadow_matches_isolated_mono(gain_rig, advanced_controls, hold_release):
    stereo, left, right = (fresh(gain_rig) for _ in range(3))
    sessions = (stereo, left, right)
    # b8030c/b80338 initialize words8,7,10, but do not initialize9.
    # Captured b8028c setter arguments yield words0..6 below; word9 is
    # explicitly zeroed here. Reinitialization below is the b80345 entry
    # control boundary only, not a claim about a named trigger/reset event.
    # Setter capture: R8=0, R12=1/16, stack(1/64,1/16,1/2,1/16,1).
    # The optional finite variant enables the inspected 14-sample hold then
    # phase4 release to zero; it does not invoke an unaudited live setter.
    initial = (
        7,
        1,
        7,
        bits(0 if hold_release else 0.5),
        7,
        1,
        int(hold_release),
        5,
        0,
        0,
        0,
    )
    for session in sessions:
        session.runner.state.assume_nw32 = True
        session.runner.attach_watchpoints(
            [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
        )
        session.runner.set_watch_active(False)
        for offset in (0, 0x100):
            for i, value in enumerate(initial):
                session.poke(CONTROL + offset + i, value)
    phases = []
    markers = []
    table_pcs = set()
    audio_differences = []
    for block, control in enumerate((1, 0, 0, 0, 0, 1, 0, 0, 0)):
        inputs = []
        for i in range(32):
            lv, rv = bits((i - 11) / 64 + block / 64), bits((7 - i) / 83 - block / 128)
            inputs.append((lv, rv))
            stereo.poke(LEFT + i, lv)
            stereo.poke(RIGHT + i, rv)
            left.poke(LEFT + i, lv)
            right.poke(LEFT + i, rv)
        gain_call(stereo, "prepare")
        snapshot = words(stereo, CONTROL, 11)
        assert words(stereo, CONTROL + 0x100, 11) == snapshot
        gain_call(stereo, "left", control)
        table_pcs.update(e.pc_sw for e in stereo.stores if e.access == "read")
        authoritative = words(stereo, CONTROL, 11)
        if advanced_controls:
            gain_call(stereo, "prepare")
        gain_call(stereo, "right", control)
        gain_call(left, "left", control)
        gain_call(right, "left", control)
        assert words(stereo, CONTROL, 11) == authoritative == words(left, CONTROL, 11)
        assert words(left, CONTROL, 11) == words(right, CONTROL, 11)
        assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
        actual, expected = words(stereo, RIGHT, 32), words(right, LEFT, 32)
        audio_differences.append(actual != expected)
        if not advanced_controls:
            assert actual == expected
            assert words(stereo, CONTROL + 0x100, 11) == authoritative
        assert words(stereo, LEFT, 32) != [v[0] for v in inputs]
        assert expected != [v[1] for v in inputs]
        if hold_release and block in (2, 7):
            # The bounded phase3 segment is unity with in-place source/dest.
            # Retain its input even if no multiply/store is needed there.
            original_right = [v[1] for v in inputs]
            assert any(
                expected[i : i + 14] == original_right[i : i + 14] for i in range(19)
            )
        assert words(stereo, LEFT, 32) != actual
        phases.append(stereo.peek(CONTROL + 7))
        markers.append(stereo.peek(CONTROL + 10))
        for session in sessions:
            for output in (LEFT, RIGHT) if session is stereo else (LEFT,):
                assert all(
                    (v & 0x7F800000) != 0x7F800000 for v in words(session, output, 32)
                )
    # The first attack crosses the -1 clamp in block2, then reaches phase1
    # or the finite hold/phase4 path. Entry control1 later restarts the attack.
    expected_phases = [2, 2, 4 if hold_release else 1]
    assert phases[:3] == expected_phases
    assert phases[5:8] == expected_phases
    assert {0xB8039C, 0xB803D7} <= table_pcs
    assert ({0xB80452, 0xB804FC} if hold_release else {0xB8049D}) <= table_pcs
    assert markers[0] != markers[1] != markers[2]
    assert markers[5:8] == markers[:3]
    if advanced_controls:
        assert any(audio_differences)


RING_LEFT, RING_RIGHT = 0x10040000, 0x10042000
RING_WORDS = 8192
RING_PRIVATE = (0, 13, 14, 16, 17)
RING_SHARED = tuple(i for i in range(18) if i not in RING_PRIVATE)


@pytest.fixture(scope="module")
def ring_rig(rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-ring-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(source.read_text(), [REPO_ROOT / "sdk"])
        ),
        0xB96000,
        {"_ring": 0xB806F5, "_ring_init": 0xB80575},
    )
    return linked, rig[1]


def ring_call(session, side, initialization=False):
    regs = {**REGS, "I6": STACK * 4, "I7": STACK * 4, "M13": 0, "M15": 0xFFFFFFFF}
    entry = f"_init_{side}_ring" if initialization else f"_{side}_ring"
    result = session.call(entry, regs, max_steps=12000)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    offset = 0x100 if side == "right" else 0
    if side == "prepare":
        allowed = {CONTROL + 0x100 + i for i in RING_SHARED}
    else:
        mutable = (
            tuple(i for i in range(18) if i not in (13, 15, 16))
            if initialization
            else (0, 3, 4, 5, 13, 14, 17)
        )
        allowed = {CONTROL + offset + i for i in mutable}
        allowed |= set(range(STACK - 64, STACK + 1))
        if not initialization:
            allowed |= {LEFT + offset + i for i in range(32)}
        ring = RING_RIGHT if side == "right" else RING_LEFT
        allowed |= set(range(ring, ring + RING_WORDS))
    addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in allowed
    }
    stores = [e for e in session.stores if e.access == "write"]
    fill_byte = _canonical_dm_address(
        session.runner.state,
        normal_word_to_byte(CONTROL + offset + 17),
        1,
        for_write=True,
    )
    invalid = [
        e
        for e in stores
        if e.address not in addresses or e.width != (1 if e.address == fill_byte else 4)
    ]
    assert not invalid, (
        "ring store bounds",
        [(hex(e.pc_sw), hex(e.address), e.width) for e in invalid],
    )
    counts = Counter(addresses[e.address] for e in stores)
    if side == "prepare":
        assert set(counts) == allowed
        assert set(counts.values()) == {1}
    elif initialization:
        assert [counts[ring + i] for i in range(RING_WORDS)] == [1] * RING_WORDS
    else:
        assert [counts[LEFT + offset + i] for i in range(32)] == [1] * 32
        assert sum(counts[ring + i] for i in range(RING_WORDS)) == 32
        # Check actual indexed load PCs against this channel's backing, not
        # merely a read watch confined to the expected valid range.
        pcs = (0xB8079B, 0xB8079E, 0xB807A4, 0xB807A7)
        reads = [e for e in session.stores if e.access == "read" and e.pc_sw in pcs]
        base = _canonical_dm_address(session.runner.state, normal_word_to_byte(ring), 4)
        assert len(reads) == 32 * 4 * 4
        assert all(
            e.width == 1
            and e.new_value is not None
            and base <= e.address < base + RING_WORDS * 4
            for e in reads
        )
    return result


def initialized_ring_session(ring_rig, stereo=False):
    session = fresh(ring_rig)
    session.runner.state.assume_nw32 = True
    session.runner.attach_watchpoints(
        [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
    )
    session.runner.set_watch_active(False)
    for side, offset, ring in (("left", 0, RING_LEFT), ("right", 0x100, RING_RIGHT)):
        if side == "right" and not stereo:
            break
        for i in range(18):
            session.poke(CONTROL + offset + i, 0)
        # Constructor obtains this byte pointer from word16. Its actual SIMD
        # clear and 48000-Hz coefficient initialization run on private memory.
        session.poke(CONTROL + offset + 16, normal_word_to_byte(ring))
        ring_call(session, side, initialization=True)
        assert words(session, ring, RING_WORDS) == [0] * RING_WORDS
    return session


@pytest.mark.parametrize(
    "shared_pointer", (False, True), ids=("private-rings", "shared-pointer")
)
def test_ring_shadow_matches_isolated_mono(ring_rig, shared_pointer):
    stereo = initialized_ring_session(ring_rig, stereo=True)
    left, right = (initialized_ring_session(ring_rig) for _ in range(2))
    for session, offset, channel in (
        (stereo, 0, 0),
        (stereo, 0x100, 1),
        (left, 0, 0),
        (right, 0, 1),
    ):
        ring = RING_RIGHT if offset else RING_LEFT
        for i in range(RING_WORDS):
            session.poke(ring + i, bits(((i % 97) - 48) / (512 + 97 * channel)))
        # Audited setter clamps delay to1..8191; exact integer32 avoids the
        # float wrap-to8192 endpoint. Coordinates model a filled near-wrap ring.
        for i, value in {
            0: bits((1 - 2 * channel) / 32),
            13: bits(8158),
            14: 8190,
            15: bits(32),
            17: 0,
        }.items():
            session.poke(CONTROL + offset + i, value)
    differences = []
    for block in range(3):
        for session in (stereo, left, right):
            session.poke(CONTROL + 10, bits(0.1 + block / 32))
        for i in range(32):
            lv, rv = (
                bits((i - 11) / 128 + block / 64),
                bits((7 - i) / 163 - block / 128),
            )
            stereo.poke(LEFT + i, lv)
            stereo.poke(RIGHT + i, rv)
            left.poke(LEFT + i, lv)
            right.poke(LEFT + i, rv)
        private = [stereo.peek(CONTROL + 0x100 + i) for i in RING_PRIVATE]
        ring_call(stereo, "prepare")
        assert [stereo.peek(CONTROL + 0x100 + i) for i in RING_PRIVATE] == private
        ring_call(stereo, "left")
        authoritative = words(stereo, CONTROL, 18)
        left_backing = words(stereo, RING_LEFT, RING_WORDS)
        if shared_pointer:
            stereo.poke(CONTROL + 0x100 + 16, normal_word_to_byte(RING_LEFT))
            with pytest.raises(AssertionError, match="ring store bounds"):
                ring_call(stereo, "right")
            assert words(stereo, RING_LEFT, RING_WORDS) != left_backing
            return
        ring_call(stereo, "right")
        ring_call(left, "left")
        ring_call(right, "left")
        assert words(stereo, CONTROL, 18) == authoritative == words(left, CONTROL, 18)
        assert (
            words(stereo, RING_LEFT, RING_WORDS)
            == left_backing
            == words(left, RING_LEFT, RING_WORDS)
        )
        assert words(stereo, RING_RIGHT, RING_WORDS) == words(
            right, RING_LEFT, RING_WORDS
        )
        assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
        assert words(stereo, RIGHT, 32) == words(right, LEFT, 32)
        assert words(stereo, LEFT, 32) != words(stereo, RIGHT, 32)
        # Reference mono has an independently owned backing at another pointer.
        for i in range(18):
            if i != 16:
                assert stereo.peek(CONTROL + 0x100 + i) == right.peek(CONTROL + i)
        assert stereo.peek(CONTROL + 14) == (8190 + 32 * (block + 1)) % RING_WORDS
        assert stereo.peek(CONTROL + 0x100 + 14) == stereo.peek(CONTROL + 14)
        assert stereo.peek(CONTROL + 13) == bits((8158 + 32 * (block + 1)) % RING_WORDS)
        assert stereo.peek(CONTROL + 0x100 + 13) == stereo.peek(CONTROL + 13)
        assert stereo.peek(CONTROL) != bits(1 / 32)
        for session in (stereo, left, right):
            for output in (LEFT, RIGHT) if session is stereo else (LEFT,):
                assert all(
                    (v & 0x7F800000) != 0x7F800000 for v in words(session, output, 32)
                )
        differences.append(words(stereo, LEFT, 32) != words(stereo, RIGHT, 32))
    assert all(differences)


@pytest.fixture(scope="module")
def source_rig(rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-source-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(source.read_text(), [REPO_ROOT / "sdk"])
        ),
        0xB98000,
        {"_source": 0xB8251B},
    )
    return linked, rig[1]


def source_call(session, side, depth):
    # Companion scratch is explicitly seeded, not treated as callee-preserved.
    regs = {
        **REGS,
        "I6": STACK * 4,
        "I7": STACK * 4,
        "S0": 0x12340000,
        "S5": 0x12340005,
        "S9": 0x12340009,
        "S14": 0x1234000E,
    }
    result = session.call(
        f"_{side}_source",
        regs,
        max_steps=12000,
        read_regs=("MODE1", "S0", "S5", "S9", "S14"),
    )
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    assert result["regs"]["MODE1"] == 0
    offset = 0x100 if side == "right" else 0
    allowed = {LEFT + offset + i for i in range(32)}
    allowed |= set(range(STACK - 64, STACK + 1))
    addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in allowed
    }
    stores = [e for e in session.stores if e.access == "write"]
    assert all(e.width == 4 and e.address in addresses for e in stores), stores
    counts = Counter(addresses[e.address] for e in stores)
    assert [counts[LEFT + offset + i] for i in range(32)] == [1] * 32
    # Watch every read, then classify at the actual sample/table load PCs.
    # An out-of-range access therefore fails instead of escaping the watch.
    sample_pcs = (0xB8255F, 0xB8256E, 0xB825E3, 0xB825E6, 0xB82613)
    reads = [e for e in session.stores if e.access == "read" and e.pc_sw in sample_pcs]
    audio = _canonical_dm_address(
        session.runner.state, normal_word_to_byte(LEFT + offset), 4
    )
    assert len(reads) == 32 * 4
    assert all(
        e.width == 1 and e.new_value is not None and audio <= e.address < audio + 32 * 4
        for e in reads
    )
    table_reads = [
        e
        for e in session.stores
        if e.access == "read" and e.pc_sw in (0xB825AF, 0xB825B3)
    ]
    base = _canonical_dm_address(session.runner.state, 0x26E7C8, 4)
    assert len(table_reads) == (8 if depth else 0)
    assert all(
        e.width == 1 and e.new_value is not None and base <= e.address < base + 128 * 4
        for e in table_reads
    )
    if depth:
        assert result["regs"]["S9"] == (-depth) & 0xFFFFFFFF
        assert result["regs"]["S14"] == (-depth - 1) & 0xFFFFFFFF
    else:
        assert all(
            result["regs"][name] == regs[name] for name in ("S0", "S5", "S9", "S14")
        )
    return result


@pytest.mark.parametrize(
    "control,depth", ((0, 0), (0.25, 8), (0.5, 4), (0.75, 2), (1, 1))
)
def test_source_continuation_matches_isolated_mono(source_rig, control, depth):
    stereo, left, right = (fresh(source_rig) for _ in range(3))
    for session in (stereo, left, right):
        # Byte pointers, 32-bit normal-word accesses, linear DAGs and SISD
        # entry are explicit. The callee enables SIMD internally even for copy.
        session.runner.state.assume_nw32 = True
        session.runner.attach_watchpoints(
            [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
        )
        session.runner.set_watch_active(False)
        for offset in (0, 0x100):
            session.poke(CONTROL + offset, bits(control))
    inputs = []
    for i in range(32):
        lv, rv = bits((i - 15) / 21), bits((11 - i) / 27)
        inputs.append((lv, rv))
        stereo.poke(LEFT + i, lv)
        stereo.poke(RIGHT + i, rv)
        left.poke(LEFT + i, lv)
        right.poke(LEFT + i, rv)
    source_call(stereo, "left", depth)
    authoritative = stereo.peek(CONTROL)
    source_call(stereo, "right", depth)
    source_call(left, "left", depth)
    source_call(right, "left", depth)
    assert stereo.peek(CONTROL) == authoritative == bits(control)
    for session in (stereo, left, right):
        assert session.peek(CONTROL) == session.peek(CONTROL + 0x100) == bits(control)
    for output, reference, channel in ((LEFT, left, 0), (RIGHT, right, 1)):
        result = words(stereo, output, 32)
        assert result == words(reference, LEFT, 32)
        original = [v[channel] for v in inputs]
        if depth:
            assert result != original
        else:
            assert result == original
        assert all((v & 0x7F800000) != 0x7F800000 for v in result)
    assert words(stereo, LEFT, 32) != words(stereo, RIGHT, 32)


ORDER_ENGINE, ORDER_GAIN, ORDER_PAN = 0x08040600, 0x08040800, 0x08040A00
ORDER_INPUTS, ORDER_OUTPUTS = 0x08040C00, 0x08040E00
ORDER_SCRATCH = 0x08041200
ORDER_PLANES = (0x08041400, 0x08041500, 0x08041600, 0x08041700)
ORDER_SHARED = (
    (CONTROL, (0,)),
    (FIRST, (21, 22, 23)),
    (UY, tuple(11 + 7 * k + delta for k in range(6) for delta in (0, 2))),
    (ORDER_ENGINE, (0, 2, 7, 9)),
    (ORDER_GAIN, tuple(range(11))),
    (ORDER_PAN, (0, 2, 112, 114)),
)


@pytest.fixture(scope="module")
def ordered_rig(rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-ordered-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(source.read_text(), [REPO_ROOT / "sdk"])
        ),
        0xB9A000,
        {
            "_source": 0xB8251B,
            "_first": 0xB80C39,
            "_case5": 0xB81526,
            "_gain": 0xB80345,
        },
    )
    return linked, rig[1]


def ordered_call(session, side, phase, control=0):
    offset = 0x100 if side == "right" else 0
    audio, scratch = LEFT + offset, ORDER_SCRATCH + offset
    engine, gain, pan = ORDER_ENGINE + offset, ORDER_GAIN + offset, ORDER_PAN + offset
    stack = set(range(STACK - 64, STACK + 1))
    shared = {base + i for base, indices in ORDER_SHARED for i in indices}
    histories = {FIRST + i for i in (24, 25)} | {UY + i for i in (9, 10)}
    regs = {**REGS, "I6": STACK * 4, "I7": STACK * 4, "M13": 0, "M15": 0xFFFFFFFF}
    if phase == "prepare":
        entry, stop = "_prepare_ordered", BACK
        writes = {a + 0x100 for a in shared}
        reads = shared
    elif phase == "prefix":
        entry, stop = f"_{side}_ordered", BACK
        writes = {audio + i for i in range(32)} | {scratch + i for i in range(32)}
        writes |= {a + offset for a in histories} | stack
        reads = writes | {a + offset for a in shared if a < ORDER_ENGINE}
    elif phase == "blend":
        entry, stop = "_original_blend", 0x1C98B9
        # Original continuation reloads the scratch pointer from engine+4e02.
        regs.update(I1=(CONTROL + offset) * 4, I0=engine * 4, I5=audio * 4, R9=32)
        writes = {audio + i for i in range(32)}
        reads = writes | {scratch + i for i in range(32)}
        reads |= {CONTROL + offset + 0x4E02} | {engine + i for i in (0, 2, 7, 9)}
    elif phase == "gain":
        entry, stop = f"_{side}_ordered_gain", BACK
        regs.update(R0=control, R1=0)
        writes = {audio + i for i in range(32)} | {gain + i for i in (7, 9, 10)} | stack
        reads = writes | {gain + i for i in range(11)}
    else:
        assert phase == "pan"
        entry, stop = "_original_pan", 0x1C9B30
        regs.update(
            I3=(ORDER_INPUTS + offset) * 4,
            I5=(ORDER_OUTPUTS + offset) * 4,
            I4=pan * 4,
            R9=32,
        )
        planes = ORDER_PLANES[2:] if offset else ORDER_PLANES[:2]
        writes = {base + i for base in planes for i in range(32)}
        reads = {audio + i for i in range(32)} | {pan + i for i in (0, 2, 112, 114)}
        reads |= {
            ORDER_INPUTS + offset,
            ORDER_OUTPUTS + offset,
            ORDER_OUTPUTS + offset + 1,
        }
    session.runner.breakpoints = frozenset((BACK, stop))
    result = session.call(entry, regs, max_steps=12000, read_regs=("MODE1",))
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == stop
    assert result["regs"]["MODE1"] == 0
    write_bytes = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in writes
    }
    read_bytes = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        )
        + j
        for a in reads
        for j in range(4)
    }
    if phase == "prefix":
        tables = (0x26E7C8,)
    elif phase == "gain":
        tables = (0x26A570, 0x26A770, 0x26A970)
    else:
        tables = ()
    for table in tables:
        base = _canonical_dm_address(session.runner.state, table, 4)
        read_bytes.update(range(base, base + 128 * 4))
    stores = [e for e in session.stores if e.access == "write"]
    assert all(e.width == 4 and e.address in write_bytes for e in stores), stores
    loads = [e for e in session.stores if e.access == "read"]
    assert all(
        e.width == 1 and e.new_value is not None and e.address in read_bytes
        for e in loads
    ), loads
    counts = Counter(write_bytes[e.address] for e in stores)
    if phase == "prepare":
        assert set(counts) == writes
        assert set(counts.values()) == {1}
    elif phase == "prefix":
        assert [counts[audio + i] for i in range(32)] == [2] * 32
        assert [counts[scratch + i] for i in range(32)] == [1] * 32
    elif phase == "blend":
        assert [counts[audio + i] for i in range(32)] == [1] * 32
    elif phase == "pan":
        assert set(counts) == writes
        assert set(counts.values()) == {1}
    return result


def initialized_ordered_session(ordered_rig, channel=0):
    session = fresh(ordered_rig)
    session.runner.state.assume_nw32 = True
    session.runner.attach_watchpoints(
        [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
    )
    session.runner.set_watch_active(False)
    session.symbols.update(_original_blend=0x1C9C12, _original_pan=0x1C9AEA)
    for offset in (0, 0x100):
        for base, count in (
            (CONTROL, 1),
            (FIRST, 28),
            (UY, 53),
            (ORDER_ENGINE, 10),
            (ORDER_GAIN, 11),
            (ORDER_PAN, 115),
        ):
            for i in range(count):
                session.poke(base + offset + i, 0)
        for i, value in enumerate((7, 1, 7, bits(0.5), 7, 1, 0, 5, 0, 0, 0)):
            session.poke(ORDER_GAIN + offset + i, value)
        side_channel = 1 if offset else channel
        for base, indices, values in (
            (FIRST, (24, 25), ((0.03, -0.01), (-0.02, 0.04))),
            (UY, (9, 10), ((0.02, -0.03), (-0.04, 0.01))),
        ):
            for i, value in zip(indices, values[side_channel], strict=True):
                session.poke(base + offset + i, bits(value))
        session.poke(CONTROL + offset + 0x4E02, (ORDER_SCRATCH + offset) * 4)
        session.poke(ORDER_INPUTS + offset, (LEFT + offset) * 4)
        planes = ORDER_PLANES[2:] if offset else ORDER_PLANES[:2]
        for i, plane in enumerate(planes):
            session.poke(ORDER_OUTPUTS + offset + i, plane * 4)
    return session


def run_ordered_channel(session, side, control, overwrite_scratch=False):
    offset = 0x100 if side == "right" else 0
    snapshots = {}
    ordered_call(session, side, "prefix")
    snapshots["prefix"] = words(session, LEFT + offset, 32)
    snapshots["scratch"] = words(session, ORDER_SCRATCH + offset, 32)
    if overwrite_scratch:
        # Deliberate lifetime fault after the case5 result but before its
        # original continuation consumes it. Bounds remain valid.
        for i in range(32):
            session.poke(ORDER_SCRATCH + offset + i, 0)
    ordered_call(session, side, "blend")
    snapshots["blend"] = words(session, LEFT + offset, 32)
    ordered_call(session, side, "gain", control)
    snapshots["gain"] = words(session, LEFT + offset, 32)
    ordered_call(session, side, "pan")
    planes = ORDER_PLANES[2:] if offset else ORDER_PLANES[:2]
    snapshots["planes"] = tuple(words(session, plane, 32) for plane in planes)
    return snapshots


@pytest.mark.parametrize(
    "fault",
    (
        None,
        "shared-history",
        "advanced-gain",
        "scratch-overwrite",
        "swapped-terminal",
        "duplicated-terminal",
    ),
)
def test_ordered_route_matches_isolated_mono(ordered_rig, fault):
    """Selected synthetic route: scalar/later ramps unity, other conditionals
    and fade off. Shared cleanup is outside replay and is not proven here.
    The terminal L.A/R.B pair is a candidate balance mapping, not an ABI,
    stock-default reachability, or mixed/send consumer integration proof.
    """
    stereo = initialized_ordered_session(ordered_rig)
    left = initialized_ordered_session(ordered_rig)
    right = initialized_ordered_session(ordered_rig, channel=1)
    differences = []
    phases = []
    for block in range(4):
        for session in (stereo, left, right):
            session.poke(CONTROL, bits(0.5))
            for i, value in {21: 0.7 - block / 64, 22: 0.2, 23: 0.1}.items():
                session.poke(FIRST + i, bits(value))
            for k, value in enumerate((0.6, 0.1, 0.15, 0.2, 0.1, 0.08)):
                session.poke(UY + 11 + 7 * k, bits(value * (1 + block / 32)))
                session.poke(UY + 13 + 7 * k, bits(0.0001))
            for i, value in {
                0: 0.4 + block / 64,
                2: 0.001,
                7: 0.6 - block / 64,
                9: -0.001,
            }.items():
                session.poke(ORDER_ENGINE + i, bits(value))
            for i, value in {
                0: 0.25 + block / 32,
                2: 0.002,
                112: 0.8 - block / 32,
                114: -0.001,
            }.items():
                session.poke(ORDER_PAN + i, bits(value))
        for i in range(32):
            lv, rv = bits((i - 15) / 21 + block / 64), bits((11 - i) / 27 - block / 128)
            stereo.poke(LEFT + i, lv)
            stereo.poke(RIGHT + i, rv)
            left.poke(LEFT + i, lv)
            right.poke(LEFT + i, rv)
        ordered_call(stereo, "left", "prepare")
        actual_left = run_ordered_channel(stereo, "left", int(block == 0))
        authoritative = [
            (base + i, stereo.peek(base + i))
            for base, indices in (*ORDER_SHARED, (FIRST, (24, 25)), (UY, (9, 10)))
            for i in indices
        ]
        left_scratch = words(stereo, ORDER_SCRATCH, 32)
        if fault == "shared-history":
            for base, indices in ((FIRST, (24, 25)), (UY, (9, 10))):
                for i in indices:
                    stereo.poke(base + 0x100 + i, stereo.peek(base + i))
        elif fault == "advanced-gain":
            ordered_call(stereo, "left", "prepare")
        actual_right = run_ordered_channel(
            stereo,
            "right",
            int(block == 0),
            overwrite_scratch=fault == "scratch-overwrite",
        )
        expected_left = run_ordered_channel(left, "left", int(block == 0))
        expected_right = run_ordered_channel(right, "left", int(block == 0))
        assert actual_left == expected_left
        assert [(a, stereo.peek(a)) for a, _ in authoritative] == authoritative
        assert words(stereo, ORDER_SCRATCH, 32) == left_scratch
        if fault == "scratch-overwrite":
            assert actual_right["prefix"] == expected_right["prefix"]
            assert actual_right["scratch"] == expected_right["scratch"]
            assert actual_right["blend"] != expected_right["blend"]
        if fault not in ("shared-history", "advanced-gain", "scratch-overwrite"):
            assert actual_right == expected_right
            assert words(stereo, ORDER_GAIN + 0x100, 11) == words(
                stereo, ORDER_GAIN, 11
            )
        terminal = (actual_left["planes"][0], actual_right["planes"][1])
        if fault == "swapped-terminal":
            terminal = (actual_left["planes"][1], actual_right["planes"][0])
        elif fault == "duplicated-terminal":
            terminal = (actual_left["planes"][0], actual_left["planes"][0])
        expected = (expected_left["planes"][0], expected_right["planes"][1])
        differences.append(terminal != expected)
        if fault is None:
            assert terminal == expected
        assert actual_left["planes"][0] != actual_right["planes"][1]
        for snapshots in (actual_left, actual_right):
            assert snapshots["prefix"] != snapshots["scratch"]
            assert snapshots["blend"] != snapshots["prefix"]
            assert snapshots["gain"] != snapshots["blend"]
            for plane in (
                *snapshots["planes"],
                snapshots["prefix"],
                snapshots["scratch"],
                snapshots["blend"],
                snapshots["gain"],
            ):
                assert all((v & 0x7F800000) != 0x7F800000 for v in plane)
        phases.append(left.peek(ORDER_GAIN + 7))
    assert phases == [2, 2, 1, 1]
    if fault is not None:
        assert any(differences)


RESET_WRITES = {
    "snapshots": (
        3,
        4,
        12,
        13,
        14,
        17,
        19,
        20,
        21,
        24,
        26,
        27,
        28,
        31,
        33,
        34,
        35,
        38,
        40,
        41,
        42,
        45,
    ),
    "nine": (10, 11, *(12 + 7 * k + delta for k in range(9) for delta in (0, 1, 2, 5))),
    "prepared": (*range(5, 13), *PREPARED_WRITES, *(22 + 7 * k for k in range(6))),
    "held": tuple(range(5)),
}
RESET_SIZES = {"snapshots": 51, "nine": 75, "prepared": 59, "held": 5}
RESET_HISTORIES = {
    "snapshots": (3, 4, 48, 49, 50),
    "nine": (10, 11),
    "prepared": (5, 9, 10, 11, 12),
    "held": (1, 2),
}


@pytest.fixture(scope="module")
def reset_rig(filter_rig):
    source = REPO_ROOT / "tests/fixtures/dsp/stereo-reset-shadow-replay.s"
    linked = pk_sharc.link(
        pk_sharc.assemble(
            pk_sharc.expand_includes(source.read_text(), [REPO_ROOT / "sdk"])
        ),
        0xB9C000,
        {
            "_reset_snapshots": 0xB81D6D,
            "_reset_nine": 0xB81C8D,
            "_reset_prepared": 0xB811F1,
            "_reset_held": 0xB82440,
        },
    )
    return [filter_rig[0], linked], filter_rig[1]


def reset_call(session, family, side):
    regs = {**REGS, "I6": STACK * 4, "I7": STACK * 4, "M13": 0, "M15": 0xFFFFFFFF}
    result = session.call(f"_reset_{side}_{family}", regs, max_steps=12000)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    offset = 0x100 if side == "right" else 0
    base = CONTROL + offset
    mutable = {base + i for i in RESET_WRITES[family]}
    writes = mutable | set(range(STACK - 64, STACK + 1))
    addresses = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        ): a
        for a in writes
    }
    stores = [e for e in session.stores if e.access == "write"]
    assert all(e.width == 4 and e.address in addresses for e in stores), stores
    assert {addresses[e.address] for e in stores} & mutable == mutable
    readable = set(range(base, base + RESET_SIZES[family]))
    readable |= set(range(STACK - 64, STACK + 1))
    read_bytes = {
        _canonical_dm_address(
            session.runner.state, normal_word_to_byte(a), 4, for_write=True
        )
        + j
        for a in readable
        for j in range(4)
    }
    if family == "snapshots":
        table = _canonical_dm_address(session.runner.state, 0x26E5C0, 4)
        read_bytes |= set(range(table, table + 128 * 4))
    loads = [e for e in session.stores if e.access == "read"]
    assert all(
        e.width == 1 and e.new_value is not None and e.address in read_bytes
        for e in loads
    ), loads
    return result


def initialized_reset_session(reset_rig, family, channel=0):
    linked, memory = reset_rig
    session = pk_sharc.CallSession(linked, BACK, base_memory=memory)
    session.record_stores()
    session.runner.state.assume_nw32 = True
    session.runner.attach_watchpoints(
        [session._run.Watchpoint(0, 1 << 40, stop=False, label="access")]
    )
    session.runner.set_watch_active(False)
    size = RESET_SIZES[family]
    for offset in (0, 0x100):
        for i in range(size):
            session.poke(CONTROL + offset + i, bits(0.01 + i / 512))
        if family == "snapshots":
            _, _, coefficients, values = FILTER_FAMILIES[family]
            controls = dict(zip(coefficients, values, strict=True))
            controls.update(
                {
                    9: 0.2,
                    10: 0.25,
                    11: 0.3,
                    16: 0.4,
                    23: 0.1,
                    30: 0.15,
                    37: 0.2,
                    44: 0.25,
                }
            )
        elif family == "nine":
            controls = {
                i: value
                for i, value in enumerate(
                    (0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1), start=1
                )
            }
        elif family == "prepared":
            controls = {0: 0.1, 1: 0.3, 2: 0.08, 3: 0.1, 4: 0.6, 14: 0.2}
        else:
            controls = {0: 0.25}
            session.poke(CONTROL + offset + 3, 1)
            session.poke(CONTROL + offset + 4, 1)
        for i, value in controls.items():
            session.poke(CONTROL + offset + i, bits(value))
        side_channel = 1 if offset else channel
        for n, i in enumerate(RESET_HISTORIES[family]):
            session.poke(
                CONTROL + offset + i, bits((n + 1) / (64 if side_channel == 0 else -89))
            )
    return session


@pytest.mark.parametrize(
    "family,fault",
    [(family, fault) for family in RESET_SIZES for fault in (None, "missed-reset")]
    + [("snapshots", "blanket-snapshots")],
)
def test_helper_reset_mirroring_matches_isolated_mono(reset_rig, family, fault):
    """Helper boundaries only: no event-name, global writer or reset-hook claim.

    b82440 is the held-sample record reset here, not an assumption that every
    use of that helper has the same record layout as the source continuation.
    """
    stereo = initialized_reset_session(reset_rig, family)
    left = initialized_reset_session(reset_rig, family)
    right = initialized_reset_session(reset_rig, family, channel=1)
    size = RESET_SIZES[family]
    before_right = words(stereo, CONTROL + 0x100, size)
    reset_call(stereo, family, "left")
    authoritative = words(stereo, CONTROL, size)
    reset_call(left, family, "left")
    reset_call(right, family, "left")
    if fault != "missed-reset":
        reset_call(stereo, family, "right")
    if fault == "blanket-snapshots":
        # b81d6d resets3/4, but retains48..50. Their external consumers are
        # unaudited, so check loss exactly at the reset boundary; the leaf
        # renderer later overwrites these snapshots and can hide the error.
        for i in (48, 49, 50):
            stereo.poke(CONTROL + 0x100 + i, 0)
    assert words(stereo, CONTROL, size) == authoritative == words(left, CONTROL, size)
    actual, expected = words(stereo, CONTROL + 0x100, size), words(right, CONTROL, size)
    if fault is None:
        assert actual == expected
    else:
        assert actual != expected
    if family == "snapshots":
        assert [right.peek(CONTROL + i) for i in (48, 49, 50)] == [
            before_right[i] for i in (48, 49, 50)
        ]
        assert [right.peek(CONTROL + i) for i in (3, 4)] == [0, 0]
        assert all(before_right[i] != 0 for i in (48, 49, 50))
    # Run each actual renderer after resetting.
    if family == "held":
        # Capture an active post-reset control, retaining actual reset counters
        # and held values. A missed reset must not be hidden by a copy bypass.
        for session, offset in ((stereo, 0), (stereo, 0x100), (left, 0), (right, 0)):
            session.poke(CONTROL + offset, bits(0.25))
    for i in range(32):
        lv, rv = bits((i - 11) / 128), bits((7 - i) / 163)
        stereo.poke(LEFT + i, lv)
        stereo.poke(RIGHT + i, rv)
        left.poke(LEFT + i, lv)
        right.poke(LEFT + i, rv)
    if family in FILTER_FAMILIES:
        _, histories, shared, _ = FILTER_FAMILIES[family]
        shared_writes = ()
    else:
        histories = RESET_HISTORIES[family]
        shared = (0, 3, 4) if family == "held" else PREPARED_SHARED
        shared_writes = (3, 4) if family == "held" else PREPARED_WRITES
    filter_call(stereo, family, "left", histories, shared, shared_writes)
    authoritative = words(stereo, CONTROL, size)
    filter_call(stereo, family, "right", histories, shared, shared_writes)
    filter_call(left, family, "left", histories, shared, shared_writes)
    filter_call(right, family, "left", histories, shared, shared_writes)
    assert words(stereo, CONTROL, size) == authoritative == words(left, CONTROL, size)
    assert words(stereo, LEFT, 32) == words(left, LEFT, 32)
    actual_audio, expected_audio = words(stereo, RIGHT, 32), words(right, LEFT, 32)
    if fault == "missed-reset":
        assert actual_audio != expected_audio
    else:
        assert actual_audio == expected_audio
    assert words(stereo, LEFT, 32) != actual_audio
    assert any(v & 0x7FFFFFFF for v in expected_audio)
    for session in (stereo, left, right):
        for output in (LEFT, RIGHT) if session is stereo else (LEFT,):
            assert all(
                (v & 0x7F800000) != 0x7F800000 for v in words(session, output, 32)
            )
