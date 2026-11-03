"""Finite U/Y observer against original helpers; placements share one entry/state.

The repeated-placement fixture models early then late occurrences without forcing
stock G-table gates or claiming that a natural native scenario reaches both.
"""

import hashlib
import json
import math
import os
from collections import Counter
from pathlib import Path

import pytest
import test_dsp_stereo_proof as first

from tools.patchkit import patchkit, pk_sharc
from tools.sharc_dev.sharc_core.addressing import normal_word_to_byte
from tools.sharc_dev.sharc_core.memory import _canonical_dm_address
from tools.sharc_dev.sharc_disasm import decode_loaded_at
from tools.sharc_dev.sharcldr import LoadedMemory

STATE, STRIDE = 0x2533E0, 32
MEASURED = {"max_fixture_instructions": 0, "stack_bytes_below_frame": 0}


@pytest.fixture(scope="module", autouse=True)
def tools_and_measurements():
    patch = pytest.MonkeyPatch()
    patch.setattr(patchkit, "_BUILT", patchkit.binary(build=False))
    yield
    if destination := os.environ.get("DSP_STEREO_UY_EVIDENCE"):
        Path(destination).write_text(json.dumps(MEASURED, indent=2) + "\n")
    patch.undo()


@pytest.fixture(scope="module")
def rig():
    if not first.IMAGE.exists():
        pytest.skip("original DN2 1.12 section 7 required")
    data = first.IMAGE.read_bytes()
    assert (
        hashlib.sha256(data).hexdigest()
        == "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
    )
    memory = LoadedMemory.from_stream(data)
    for site, target, delay, second in [
        (0x1C959F, 0xB81368, "3c", 4),
        (0x1C9B7A, 0xB81368, "3c", 4),
        (0x1C96AA, 0xB8135A, "3a", 6),
    ]:
        insn = decode_loaded_at(memory, site)
        assert insn.type_name == "25a_direct" and insn.length_bytes == 6
        assert (insn.fields["addr[23:16]"] << 16) | insn.fields["addr[15:0]"] == target
        assert decode_loaded_at(memory, site + 3).type_name == delay
        assert decode_loaded_at(memory, site + second).type_name == "16a"
    base = (first.MOD / "src/dsp/stereo-proof.s").read_text()
    uy = (first.MOD / "src/dsp/uy-proof.s").read_text()
    for label in ("_uy_right_start", "_uy_render_finish"):
        uy = uy.replace(label + ":", f".GLOBAL {label};\n{label}:")
    obj = pk_sharc.assemble(
        pk_sharc.expand_includes(base + "\n" + uy, [first.REPO_ROOT / "sdk"])
    )
    address, syms, inits = first.PRIVATE, {}, []
    for section, (payload, alignment, _) in obj.data.items():
        address = (address + alignment - 1) & -alignment
        for name, (sec, offset, _) in obj.data_symbols.items():
            if sec == section:
                syms[name] = address + offset
        inits.append((address, payload))
        address += len(payload)
    MEASURED.update(
        private_l2_data_bytes=address - first.PRIVATE,
        object_code_bytes=len(obj.section),
        object_instructions=len(obj.rows),
    )
    assert address - first.PRIVATE < 8192
    bindings = syms | {
        "_sp_stock_render": 0xB80C39,
        "_sp_stock_reset": 0xB80B93,
        "_sp_engine_base": 0x241298,
        "_uy_stock_render": 0xB81368,
        "_uy_stock_reset": 0xB8135A,
        "_uy_engine_base": 0x241298,
    }
    linked = pk_sharc.link(obj, 0xB90000, bindings)
    # Appending U/Y must retain every original first-stage instruction/relocation.
    original = pk_sharc.assemble(
        pk_sharc.expand_includes(base, [first.REPO_ROOT / "sdk"])
    )
    alone = pk_sharc.link(original, 0xB90000, bindings)
    assert linked.code_boot[: len(alone.code_boot)] == alone.code_boot
    return linked, memory, syms, inits, address


def fresh(rig, mode=0):
    session = first.fresh(rig)
    session.symbols.update(uy_stock_render=0xB81368, uy_stock_reset=0xB8135A)
    session.poke(rig[2]["_sp_mode"], mode)
    return session


def seed(session, slot=0):
    state = STATE + STRIDE * slot
    for i in range(8):
        session.poke(state + 4 * i, 0)
    for i, value in {2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4, 6: 0.1, 7: -0.05}.items():
        session.poke(state + 4 * i, first.bits(value))
    return state


def audio(session, phase):
    for i in range(32):
        session.poke(first.INPUT + 4 * i, first.bits(0.3 * math.sin(i * 0.31 + phase)))


def context(state, **extra):
    return first.regs(state, R12=first.INPUT, STKYX=0, STKYY=0x1234, **extra)


def assert_left(reference, proof, state, expected, observed):
    assert first.values(proof, state, 8) == first.values(reference, state, 8)
    assert first.values(proof, first.INPUT, 32) == first.values(
        reference, first.INPUT, 32
    )
    for name in (
        "R3",
        "R5",
        "R6",
        "R7",
        "R9",
        "R10",
        "R11",
        "R13",
        "R14",
        "R15",
        "I0",
        "I1",
        "I3",
        "I4",
        "I5",
        "I6",
        "I7",
        "MODE1",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
    ):
        assert observed["regs"][name] == expected["regs"][name], name


@pytest.mark.parametrize("mode", [0, 1])
@pytest.mark.parametrize("slot", [0, 7, 15])
def test_repeated_placements_share_histories_and_capture_each_occurrence(
    rig, mode, slot
):
    reference, proof = fresh(rig), fresh(rig, mode)
    state = seed(reference, slot)
    seed(proof, slot)
    syms = rig[2]
    for block in range(3):
        for placement in ("early", "late"):
            phase = block + (0.5 if placement == "late" else 0)
            for session in (reference, proof):
                session.poke(state + 2 * 4, first.bits(0.6 + phase * 0.01))
                audio(session, phase)
            expected = first.call(reference, "uy_stock_render", context(state))
            observed = first.call(proof, "_uy_render", context(state))
            assert_left(reference, proof, state, expected, observed)
            private = syms["_uy_states"] + slot * STRIDE
            right = first.values(proof, private, 8)
            desired = first.values(reference, state, 8)
            for word in (6, 7):
                desired[word] ^= mode << 31
            assert first.relation_words(right) == first.relation_words(desired)
            assert first.relation_words(
                first.values(proof, syms["_uy_audio"] + 128 * slot, 32)
            ) == first.relation_words(
                [x ^ (mode << 31) for x in first.values(reference, first.INPUT, 32)]
            )
            assert proof.peek(syms["_uy_mismatches"]) == 0
            assert proof.peek(syms["_uy_nonfinite"]) == 0
            MEASURED["max_fixture_instructions"] = max(
                MEASURED["max_fixture_instructions"], observed["instructions"]
            )

            def canonical(a):
                return _canonical_dm_address(
                    proof.runner.state, normal_word_to_byte(a) or a, 4, for_write=True
                )

            frame = canonical(first.FRAME)
            MEASURED["stack_bytes_below_frame"] = max(
                MEASURED["stack_bytes_below_frame"],
                *(
                    frame - e.address
                    for e in proof.stores
                    if frame - 512 <= e.address < frame
                ),
            )
            left_words = {
                canonical(a)
                for lo, size in [(state, 32), (first.INPUT, 128)]
                for a in range(lo, lo + size, 4)
            }
            assert Counter(
                e.address for e in proof.stores if e.address in left_words
            ) == Counter(e.address for e in reference.stores if e.address in left_words)
            allowed = {
                canonical(a)
                for lo, hi in [
                    (state, state + 32),
                    (first.INPUT, first.INPUT + 128),
                    (first.PRIVATE, rig[4]),
                    (first.FRAME - 512, first.FRAME),
                ]
                for a in range(lo, hi, 4)
            }
            assert all(e.width == 4 and e.address in allowed for e in proof.stores)
    assert proof.peek(syms["_uy_shadow_calls"]) == 6
    assert proof.peek(syms["_uy_coeff_blocks"]) == 6
    for name in ("mode_errors", "fallbacks", "nonfinite", "mismatches"):
        assert proof.peek(syms[f"_uy_{name}"]) == 0
    for name in ("opposite_audio", "opposite_history"):
        assert (proof.peek(syms[f"_uy_{name}"]) > 0) == bool(mode)
    # Actual reset, then a new nonzero input, retaining the initialized private slot.
    expected = first.call(reference, "uy_stock_reset", context(state))
    observed = first.call(proof, "_uy_reset", context(state))
    assert_left(reference, proof, state, expected, observed)
    assert first.values(proof, private + 24, 2) == [0, 0]
    assert proof.peek(syms["_uy_reset_mirrors"]) == 1
    for session in (reference, proof):
        audio(session, 4)
    expected = first.call(reference, "uy_stock_render", context(state))
    observed = first.call(proof, "_uy_render", context(state))
    assert_left(reference, proof, state, expected, observed)
    assert proof.peek(syms["_uy_mismatches"]) == 0


@pytest.mark.parametrize("fault", ["input", "history"])
def test_polarity_fault_controls_preserve_stock_left(rig, fault):
    reference, proof = fresh(rig), fresh(rig, 1)
    state = seed(reference)
    seed(proof)
    syms = rig[2]
    for session in (reference, proof):
        audio(session, 0)
    first.call(reference, "uy_stock_render", context(state))
    first.call(proof, "_uy_render", context(state))
    for session in (reference, proof):
        audio(session, 1)
    expected = first.call(reference, "uy_stock_render", context(state))
    boundary = proof.symbols["_uy_right_start"]
    proof.runner.breakpoints = frozenset((first.BACK, boundary))
    paused = proof.call("_uy_render", context(state), max_steps=12000)
    assert paused["final_pc_sw"] == boundary
    if fault == "input":
        # Actual U/Y is in-place: original left input was overwritten by left.
        # Reinstall its deliberately captured source values without inversion.
        for i in range(32):
            proof.poke(
                syms["_uy_audio"] + 4 * i, first.bits(0.3 * math.sin(i * 0.31 + 1))
            )
    else:
        assert proof.peek(syms["_uy_opposite_history"]) > 0
        for i in (6, 7):
            proof.poke(syms["_uy_states"] + 4 * i, proof.peek(state + 4 * i))
    proof.runner.breakpoints = frozenset((first.BACK,))
    done = proof.runner.run(12000)
    assert done.halt.reason == "breakpoint" and done.final_pc_sw == first.BACK
    observed = {
        "regs": {
            name: proof.runner.state.uregs[proof._st.UREG_CODES[name]].value
            for name in context(state)
        }
    }
    assert_left(reference, proof, state, expected, observed)
    assert proof.peek(syms["_uy_mismatches"]) > 0
    assert proof.peek(syms["_uy_nonfinite"]) == 0


@pytest.mark.parametrize("first_entry", ["_uy_render", "_sp_render"])
def test_cross_stage_mode_switch_refuses_both_until_reload(rig, first_entry):
    proof = fresh(rig, 1)
    uy_state = seed(proof)
    sp_state = first.seed(proof)
    audio(proof, 0)
    first.call(
        proof,
        first_entry,
        context(uy_state if first_entry.startswith("_uy") else sp_state),
    )
    syms = rig[2]
    proof.poke(syms["_sp_mode"], 0)
    other = "_sp_render" if first_entry.startswith("_uy") else "_uy_render"
    first.call(proof, other, context(sp_state if other.startswith("_sp") else uy_state))
    assert proof.peek(syms["_sp_mode_errors"]) > 0
    proof.poke(syms["_sp_mode"], 1)
    before = {p: proof.peek(syms[f"_{p}_shadow_calls"]) for p in ("sp", "uy")}
    first.call(proof, "_sp_render", context(sp_state))
    first.call(proof, "_uy_render", context(uy_state))
    assert {p: proof.peek(syms[f"_{p}_shadow_calls"]) for p in ("sp", "uy")} == before


@pytest.mark.parametrize("count", [0, 31, 33])
def test_count_passthrough(rig, count):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    for session in (reference, proof):
        session.poke(first.FRAME + 4, count)
        audio(session, 0)
    expected = first.call(reference, "uy_stock_render", context(state))
    observed = first.call(proof, "_uy_render", context(state))
    assert_left(reference, proof, state, expected, observed)
    assert proof.peek(rig[2]["_uy_fallbacks"]) == 1
    assert proof.peek(rig[2]["_uy_shadow_calls"]) == 0


@pytest.mark.parametrize("entry", ["render", "reset"])
@pytest.mark.parametrize("tf_y", [False, True])
def test_simd_handoff_preserves_frame_and_context(rig, entry, tf_y):
    proof = fresh(rig)
    state = seed(proof)
    registers = context(
        state, MODE1=0x210000, ASTATY=0x76503210 | (0x40000 if tf_y else 0)
    )
    target = proof.symbols[f"uy_stock_{entry}"]
    proof.runner.breakpoints = frozenset((first.BACK, target))
    before = first.values(proof, first.FRAME - 512, 133)
    proof.record_stores()
    observed = proof.call(
        f"_uy_{entry}", registers, max_steps=50, read_regs=tuple(registers)
    )
    assert observed["final_pc_sw"] == target
    assert all(observed["regs"][k] == v for k, v in registers.items() if k != "I12")
    assert first.values(proof, first.FRAME - 512, 133) == before
    assert not proof.stores


@pytest.mark.parametrize("bad", [STATE - 4, STATE + 4, STATE + STRIDE * 16])
def test_slot_passthrough(rig, bad):
    reference, proof = fresh(rig), fresh(rig)
    for session in (reference, proof):
        for i in range(8):
            session.poke(bad + 4 * i, 0)
        audio(session, 0)
    expected = first.call(reference, "uy_stock_render", context(bad))
    observed = first.call(proof, "_uy_render", context(bad))
    assert_left(reference, proof, bad, expected, observed)
    assert proof.peek(rig[2]["_uy_fallbacks"]) == 1


@pytest.mark.parametrize("register", ["L3", "L4", "L5"])
def test_linear_dag_required(rig, register):
    proof = fresh(rig)
    state = seed(proof)
    registers = context(state, **{register: 128})
    target = proof.symbols["uy_stock_render"]
    proof.runner.breakpoints = frozenset((first.BACK, target))
    observed = proof.call(
        "_uy_render", registers, max_steps=200, read_regs=tuple(registers)
    )
    assert observed["final_pc_sw"] == target
    assert all(observed["regs"][k] == v for k, v in registers.items() if k != "I12")
    assert proof.peek(rig[2]["_uy_shadow_calls"]) == 0


@pytest.mark.parametrize("mode", [0, 1])
@pytest.mark.parametrize(
    "source", ["input", "coefficient", "left_history", "right_history", "output"]
)
def test_nonfinite_refusal_is_sticky_and_left_remains_stock(rig, mode, source):
    reference, proof = fresh(rig), fresh(rig, mode)
    state = seed(reference)
    seed(proof)
    syms = rig[2]
    for session in (reference, proof):
        audio(session, 0)
    first.call(reference, "uy_stock_render", context(state))
    first.call(proof, "_uy_render", context(state))
    if source == "right_history":
        proof.poke(syms["_uy_states"] + 6 * 4, 0x7FC00001)
    elif source == "output":
        proof.poke(syms["_uy_states"] + 6 * 4, 0x7F7FFFFF)
        for session in (reference, proof):
            session.poke(state + 2 * 4, first.bits(2.0))
    else:
        address = {
            "input": first.INPUT,
            "coefficient": state + 2 * 4,
            "left_history": state + 6 * 4,
        }[source]
        for session in (reference, proof):
            session.poke(address, 0x7FC00001)
    expected = first.call(reference, "uy_stock_render", context(state))
    observed = first.call(proof, "_uy_render", context(state))
    assert_left(reference, proof, state, expected, observed)
    assert proof.peek(syms["_uy_nonfinite"]) > 0
    shadows = proof.peek(syms["_uy_shadow_calls"])
    first.call(proof, "_uy_render", context(state))
    assert proof.peek(syms["_uy_shadow_calls"]) == shadows


@pytest.mark.parametrize("mode", [0, 1])
def test_signed_zero_and_deferred_reset(rig, mode):
    proof = fresh(rig, mode)
    state = seed(proof, 7)
    syms = rig[2]
    first.call(proof, "_uy_reset", context(state))
    assert proof.peek(syms["_uy_initialized"] + 7 * 4) == 0
    assert proof.peek(syms["_uy_reset_mirrors"]) == 0
    assert first.values(proof, state + 6 * 4, 2) == [0, 0]
    for i in range(32):
        proof.poke(first.INPUT + 4 * i, (i % 2) << 31)
    first.call(proof, "_uy_render", context(state))
    assert proof.peek(syms["_uy_mismatches"]) == 0
    assert proof.peek(syms["_uy_nonfinite"]) == 0
    assert proof.peek(syms["_uy_opposite_audio"]) == 0


def test_scratch_dags_are_restored(rig):
    reference, proof = fresh(rig), fresh(rig, 1)
    state = seed(reference)
    seed(proof)
    for session in (reference, proof):
        audio(session, 0)
    registers = context(state, L0=64, L1=128)
    expected = first.call(reference, "uy_stock_render", registers)
    observed = first.call(proof, "_uy_render", registers)
    assert_left(reference, proof, state, expected, observed)
    for name in ("L0", "L1", "B0", "B1"):
        assert observed["regs"][name] == expected["regs"][name]
