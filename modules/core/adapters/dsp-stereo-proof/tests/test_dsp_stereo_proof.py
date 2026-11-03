"""Private callsite observer against original DN2 1.12 helpers, not hardware.

Public PRM Rev1.5 Table4-22 (printed4-54, PDF159) and Table4-32
(printed4-61, PDF166) AND branch conditions across PEs. MODE1 is not a
complementary register (Type18a, printed17-2..3); BIT SET/CLR change their
specified register, while TST/XOR set BTF. SIMD Mode (PDF102) specifies one
cycle PEYEN latency; the rejection path waits two NOPs after each change.
"""

import hashlib
import json
import math
import os
import struct
from collections import Counter
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

from tools.patchkit import patchkit, pk_sharc
from tools.sharc_dev.sharc_core.addressing import normal_word_to_byte
from tools.sharc_dev.sharc_core.memory import _canonical_dm_address
from tools.sharc_dev.sharc_disasm import decode_loaded_at
from tools.sharc_dev.sharcldr import LoadedMemory

MOD = Path(__file__).resolve().parents[1]
IMAGE = REPO_ROOT / "out/sections/dn2-1.12/section_7_BLOB.bin"
BACK, FRAME, CALLER = 0xB9F000, 0x2010F000, 0x2010F400
STATE, INPUT, OUTPUT, PRIVATE = 0x24F320, 0x20100000, 0x20100200, 0x200D0000
MEASURED = {"max_identity_fixture_instructions": 0, "stack_bytes_below_frame": 0}


@pytest.fixture(scope="module", autouse=True)
def save_measurements():
    yield
    if destination := os.environ.get("DSP_STEREO_PROOF_EVIDENCE"):
        Path(destination).write_text(json.dumps(MEASURED, indent=2) + "\n")


def bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


@pytest.fixture(scope="module", autouse=True)
def prebuilt_tools():
    patch = pytest.MonkeyPatch()
    patch.setattr(patchkit, "_BUILT", patchkit.binary(build=False))
    yield
    patch.undo()


@pytest.fixture(scope="module")
def rig():
    if not IMAGE.exists():
        pytest.skip("original DN2 1.12 section 7 required")
    data = IMAGE.read_bytes()
    assert (
        hashlib.sha256(data).hexdigest()
        == "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
    )
    memory = LoadedMemory.from_stream(data)
    for site, target in ((0x1C94FD, 0xB80C39), (0x1C96CB, 0xB80B93)):
        insn = decode_loaded_at(memory, site)
        assert insn.type_name == "25a_direct" and insn.length_bytes == 6
        assert (insn.fields["addr[23:16]"] << 16) | insn.fields["addr[15:0]"] == target
        # Original frame-save and explicit continuation delay instructions.
        assert decode_loaded_at(memory, site + 3).type_name == "3c"
        assert decode_loaded_at(memory, site + 4).type_name == "16a"
    source = (MOD / "src/dsp/stereo-proof.s").read_text()
    # Export a local boundary to this model fixture without changing instructions
    # or the production object's public symbol table.
    source = source.replace(
        "_sp_render_finish:", ".GLOBAL _sp_render_finish;\n_sp_render_finish:"
    )
    source = source.replace(
        "_sp_right_start:", ".GLOBAL _sp_right_start;\n_sp_right_start:"
    )
    obj = pk_sharc.assemble(pk_sharc.expand_includes(source, [REPO_ROOT / "sdk"]))
    address, syms, inits = PRIVATE, {}, []
    for section, (payload, alignment, _) in obj.data.items():
        address = (address + alignment - 1) & -alignment
        for name, (sec, offset, _) in obj.data_symbols.items():
            if sec == section:
                syms[name] = address + offset
        inits.append((address, payload))
        address += len(payload)
    MEASURED["private_l2_data_bytes"] = address - PRIVATE
    MEASURED["object_code_bytes"] = len(obj.section)
    MEASURED["object_instructions"] = len(obj.rows)
    assert address - PRIVATE <= 4096
    linked = pk_sharc.link(
        obj,
        0xB90000,
        {
            **syms,
            "_sp_stock_render": 0xB80C39,
            "_sp_stock_reset": 0xB80B93,
            "_sp_engine_base": 0x241298,
        },
    )
    return linked, memory, syms, inits, address


def fresh(rig):
    linked, memory, syms, inits, _ = rig
    session = pk_sharc.CallSession([linked], BACK, base_memory=memory)
    session.symbols.update(stock_render=0xB80C39, stock_reset=0xB80B93)
    for address, data in inits:
        for off in range(0, len(data), 4):
            session.poke(address + off, int.from_bytes(data[off : off + 4], "little"))
    for address in range(FRAME - 512, FRAME + 20, 4):
        session.poke(address, 0)
    session.poke(FRAME, CALLER)
    session.poke(FRAME - 4, BACK - 1)
    session.poke(FRAME + 4, 32)
    session.runner.state.assume_nw32 = True
    return session


def seed(session, slot=0):
    state = STATE + 112 * slot
    for i in range(28):
        session.poke(state + i * 4, 0)
    for i, value in {
        21: 0.7,
        22: 0.2,
        23: 0.1,
        24: 0.1,
        25: -0.05,
        26: 0.3,
        27: -0.2,
    }.items():
        session.poke(state + i * 4, bits(value))
    return state


def relation_words(words):
    return [0 if word & 0x7FFFFFFF == 0 else word for word in words]


def values(session, start, count):
    return [session.peek(start + 4 * i) for i in range(count)]


def regs(state, **extra):
    result = {f"R{i}": 0x1100 + i for i in range(16)}
    result.update({f"I{i}": 0x200C0000 + 256 * i for i in range(16)})
    result.update({f"L{i}": 0 for i in range(16)})
    result.update({f"B{i}": 0x200C0000 + 256 * i for i in range(16)})
    result.update({f"M{i}": 0 for i in range(16)})
    result.update(
        M5=0,
        M6=1,
        M7=0xFFFFFFFF,
        M14=1,
        I6=FRAME,
        I7=FRAME - 8,
        R2=CALLER,
        R4=state,
        R8=INPUT,
        R12=OUTPUT,
        MODE1=0x10000,
        ASTATX=0x40000,
        ASTATY=0x12345,
        LCNTR=11,
    )
    result.update({f"S{i}": 0x3F010000 + 17 * i for i in range(16)})
    return result | extra


def call(session, entry, registers, limit=12000):
    session.record_stores()
    result = session.call(entry, registers, max_steps=limit, read_regs=tuple(registers))
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == BACK
    assert session.peek(FRAME) == CALLER
    assert session.peek(FRAME - 4) == BACK - 1
    return result


@pytest.mark.parametrize("mode", [0, 1])
@pytest.mark.parametrize("slot,in_place", [(0, False), (7, True), (15, False)])
def test_identity_private_histories_coefficients_and_left_context(
    rig, slot, in_place, mode
):
    left, proof = fresh(rig), fresh(rig)
    state = seed(left, slot)
    seed(proof, slot)
    syms = rig[2]
    proof.poke(syms["_sp_mode"], mode)
    output = INPUT if in_place else OUTPUT
    for block in range(3):
        for session in (left, proof):
            session.poke(state + 21 * 4, bits(0.7 - block * 0.1))
            for i in range(32):
                session.poke(INPUT + 4 * i, bits(0.3 * math.sin(i * 0.31 + block)))
        reference = call(left, "stock_render", regs(state, R12=output))
        observed = call(proof, "_sp_render", regs(state, R12=output))
        assert values(proof, state, 28) == values(left, state, 28)
        assert values(proof, output, 32) == values(left, output, 32)
        assert relation_words(
            values(proof, syms["_sp_audio"] + 128 * slot, 32)
        ) == relation_words(
            [value ^ (mode << 31) for value in values(left, output, 32)]
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
            "ASTATX",
            "ASTATY",
            "MODE1",
        ):
            assert observed["regs"][name] == reference["regs"][name], name
        assert proof.peek(syms["_sp_mismatches"]) == 0
        assert observed["instructions"] < 6000
        MEASURED["max_identity_fixture_instructions"] = max(
            MEASURED["max_identity_fixture_instructions"], observed["instructions"]
        )

        def canonical(a):
            return _canonical_dm_address(
                proof.runner.state, normal_word_to_byte(a) or a, 4, for_write=True
            )

        frame = canonical(FRAME)
        depths = [
            frame - e.address for e in proof.stores if frame - 512 <= e.address < frame
        ]
        MEASURED["stack_bytes_below_frame"] = max(
            MEASURED["stack_bytes_below_frame"], *depths
        )
        original_words = {
            canonical(a)
            for lo, count in [(state, 28), (output, 32)]
            for a in range(lo, lo + count * 4, 4)
        }
        assert Counter(
            e.address for e in proof.stores if e.address in original_words
        ) == Counter(e.address for e in left.stores if e.address in original_words)
        expected_state = values(left, state, 28)
        for i in (24, 25):
            expected_state[i] ^= mode << 31
        assert relation_words(
            values(proof, syms["_sp_states"] + slot * 112, 28)
        ) == relation_words(expected_state)
        allowed = [
            (state, state + 112),
            (output, output + 128),
            (PRIVATE, rig[4]),
            (FRAME - 512, FRAME),
        ]
        allowed_words = {
            _canonical_dm_address(
                proof.runner.state, normal_word_to_byte(a) or a, 4, for_write=True
            )
            for lo, hi in allowed
            for a in range(lo, hi, 4)
        }
        assert all(e.width == 4 and e.address in allowed_words for e in proof.stores)
    assert proof.peek(syms["_sp_active_mode"]) == mode
    assert proof.peek(syms["_sp_mode_errors"]) == 0
    assert proof.peek(syms["_sp_nonfinite"]) == 0
    for counter in ("_sp_opposite_audio", "_sp_opposite_history"):
        assert (proof.peek(syms[counter]) > 0) == bool(mode)
    assert proof.peek(syms["_sp_shadow_calls"]) == 3
    assert proof.peek(syms["_sp_nonzero_input"]) > 0
    assert proof.peek(syms["_sp_nonidentity_coeff"]) > 0
    assert proof.peek(syms["_sp_low_entry_frame"]) == FRAME
    assert proof.peek(syms["_sp_low_entry_sp"]) == FRAME - 8


@pytest.mark.parametrize("mode", [0, 1])
def test_reset_deferred_clone_and_four_word_mirror(rig, mode):
    session = fresh(rig)
    state = seed(session, 7)
    syms = rig[2]
    session.poke(syms["_sp_mode"], mode)
    result = call(session, "_sp_reset", regs(state))
    assert result["regs"]["R2"] == CALLER
    assert values(session, state + 24 * 4, 4) == [0] * 4
    assert session.peek(syms["_sp_initialized"] + 7 * 4) == 0
    for i in range(32):
        session.poke(INPUT + i * 4, bits(0.2))
    call(session, "_sp_render", regs(state))
    for i in range(24, 28):
        session.poke(syms["_sp_states"] + 112 * 7 + 4 * i, bits(0.5))
    call(session, "_sp_reset", regs(state))
    assert values(session, syms["_sp_states"] + 112 * 7 + 24 * 4, 4) == [0] * 4
    assert session.peek(syms["_sp_reset_mirrors"]) == 1


def test_private_history_corruption_is_observed_without_left_mutation(rig):
    session = fresh(rig)
    state = seed(session)
    for i in range(32):
        session.poke(INPUT + 4 * i, bits(0.2))
    call(session, "_sp_render", regs(state))
    syms = rig[2]
    session.poke(syms["_sp_states"] + 25 * 4, bits(9.0))
    call(session, "_sp_render", regs(state))
    assert session.peek(syms["_sp_mismatches"]) > 0
    assert session.peek(syms["_sp_first_state"]) == state
    assert session.peek(syms["_sp_first_input"]) == INPUT
    assert session.peek(syms["_sp_first_output"]) == OUTPUT


@pytest.mark.parametrize("entry", ["render", "reset"])
@pytest.mark.parametrize("tf_y", [False, True])
def test_simd_rejection_preserves_stock_behavior_and_original_frame(rig, entry, tf_y):
    proof = fresh(rig)
    state = seed(proof)
    context = regs(
        state,
        MODE1=0x210000,
        ASTATX=0x01234567,
        ASTATY=0x76503210 | (0x40000 if tf_y else 0),
    )
    context.update(STKYX=0x00112233, STKYY=0x00445566, PX1=0x12345678, PX2=0xFEDCBA98)
    from tools.sharc_dev.sharc_core.state import MR

    special = {
        "MSF": MR((1 << 80) - 1, 0x123456789ABCDEF01234),
        "MSB": MR((1 << 80) - 1, 0xABCD12345678EF907654),
    }
    proof.runner.state.special.update(special)
    stock_entry = proof.symbols[f"stock_{entry}"]
    proof.runner.breakpoints = frozenset((BACK, stock_entry))
    frame_before = values(proof, FRAME - 512, 133)
    proof.record_stores()
    observed = proof.call(
        f"_sp_{entry}", context, max_steps=50, read_regs=tuple(context)
    )
    assert observed["halt"]["reason"] == "breakpoint", observed
    assert observed["final_pc_sw"] == stock_entry
    for name, value in context.items():
        if name != "I12":  # Audited dead incoming scratch, overwritten by stock.
            assert observed["regs"][name] == value, name
    assert values(proof, FRAME - 512, 133) == frame_before
    assert not proof.stores
    assert all(
        proof.runner.state.special[name] == value for name, value in special.items()
    )
    assert proof.peek(rig[2]["_sp_render_calls"]) == 0
    assert proof.peek(rig[2]["_sp_reset_calls"]) == 0


def test_stock_zero_count_return_still_overwrites_i12(rig):
    session = fresh(rig)
    state = seed(session)
    session.poke(FRAME + 4, 0)
    result = call(session, "stock_render", regs(state))
    assert result["regs"]["I12"] == BACK - 1


@pytest.mark.parametrize("state", [STATE - 4, STATE + 4, STATE + 16 * 112])
def test_nonmatching_record_uses_stock_only(rig, state):
    left, proof = fresh(rig), fresh(rig)
    for session in (left, proof):
        for i in range(28):
            session.poke(state + i * 4, 0)
        session.poke(state + 21 * 4, bits(1.0))
        for i in range(32):
            session.poke(INPUT + 4 * i, bits(0.2))
    reference = call(left, "stock_render", regs(state))
    observed = call(proof, "_sp_render", regs(state))
    assert values(proof, OUTPUT, 32) == values(left, OUTPUT, 32)
    assert observed["regs"]["I6"] == reference["regs"]["I6"]
    assert proof.peek(rig[2]["_sp_shadow_calls"]) == 0
    assert proof.peek(rig[2]["_sp_fallbacks"]) == 1


@pytest.mark.parametrize("count", [0, 31, 33])
def test_trailing_count_passthrough(rig, count):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    for session in (reference, proof):
        session.poke(FRAME + 4, count)
        for i in range(34):
            session.poke(INPUT + i * 4, bits(0.2))
            session.poke(OUTPUT + i * 4, 0xA55A3CC3)
    call(reference, "stock_render", regs(state))
    call(proof, "_sp_render", regs(state))
    assert values(proof, OUTPUT, 34) == values(reference, OUTPUT, 34)
    assert values(proof, state, 28) == values(reference, state, 28)
    assert proof.peek(rig[2]["_sp_shadow_calls"]) == 0
    assert proof.peek(rig[2]["_sp_fallbacks"]) == 1


@pytest.mark.parametrize("register", ["L3", "L4", "L5"])
def test_non_linear_stock_dag_falls_back_without_private_access(rig, register):
    proof = fresh(rig)
    state = seed(proof)
    context = regs(state, **{register: 128})
    proof.runner.breakpoints = frozenset((BACK, proof.symbols["stock_render"]))
    observed = proof.call(
        "_sp_render", context, max_steps=200, read_regs=tuple(context)
    )
    assert observed["halt"]["reason"] == "breakpoint", observed
    assert observed["final_pc_sw"] == proof.symbols["stock_render"]
    for name, value in context.items():
        if name != "I12":
            assert observed["regs"][name] == value, name
    assert values(proof, rig[2]["_sp_initialized"], 16) == [0] * 16
    assert proof.peek(rig[2]["_sp_shadow_calls"]) == 0


def test_packaging_retargets_only_the_five_engine_calls(rig, tmp_path):
    from tools.patchkit.port_resolve import sections_of

    stock = REPO_ROOT / "firmware/Digitone_II_OS1.12.syx"
    output, plan_path = tmp_path / "proof.syx", tmp_path / "plan.json"
    result = patchkit.run(
        [
            "apply",
            "--firmware",
            str(stock),
            "--modules",
            "core/dsp-stereo-proof",
            "-o",
            str(output),
            "--plan",
            str(plan_path),
        ],
        capture=True,
    )
    assert result.returncode == 0, result.stderr
    plan = json.loads(plan_path.read_text())
    assert [name.split("@")[0] for name in plan["modules"]] == ["core/dsp-stereo-proof"]
    original = rig[1]
    patched = LoadedMemory.from_stream(sections_of(output, (7,))[7])
    expected = bytearray(original.read_sw(0x1C8EF1, (0x1C9B84 - 0x1C8EF1) * 2))
    for site, name in [
        (0x1C94FD, "_sp_render"),
        (0x1C96CB, "_sp_reset"),
        (0x1C959F, "_uy_render"),
        (0x1C9B7A, "_uy_render"),
        (0x1C96AA, "_uy_reset"),
    ]:
        symbols = {
            value for key, value in plan["symbols"].items() if key.endswith(":" + name)
        }
        assert len(symbols) == 1
        insn = decode_loaded_at(patched, site)
        assert insn.type_name == "25a_direct"
        assert (
            (insn.fields["addr[23:16]"] << 16) | insn.fields["addr[15:0]"]
        ) == symbols.pop()
        start = (site - 0x1C8EF1) * 2
        expected[start : start + 6] = patched.read_sw(site, 6)
        assert patched.read_sw(
            site + 3, 12 if site == 0x1C96AA else 8
        ) == original.read_sw(site + 3, 12 if site == 0x1C96AA else 8)
    assert patched.read_sw(0x1C8EF1, len(expected)) == bytes(expected)
    for site in [
        0xB84D92,
        0xB84DA9,
        0xB834C8,
        0xB834CF,
        0x1C6EE9,
        0x1C7926,
        0xB836F6,
        0xB84876,
        0xB8488E,
    ]:
        assert patched.read_sw(site, 6) == original.read_sw(site, 6)
    for lo, hi in [
        (0xB80C39, 0xB80C6E),
        (0xB80B93, 0xB80BA5),
        (0xB81368, 0xB813E8),
        (0xB8135A, 0xB81368),
    ]:
        assert patched.read_sw(lo, (hi - lo) * 2) == original.read_sw(lo, (hi - lo) * 2)


def test_scratch_dags_are_private_and_caller_bounds_restored(rig):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    for session in (reference, proof):
        for i in range(32):
            session.poke(INPUT + i * 4, bits(0.2))
    context = regs(state, L0=64, L1=128, B0=0x200C0000, B1=0x200C0100)
    expected = call(reference, "stock_render", context)
    observed = call(proof, "_sp_render", context)
    assert values(proof, OUTPUT, 32) == values(reference, OUTPUT, 32)
    for name in ("I0", "I1", "L0", "L1", "B0", "B1", "MODE1", "ASTATX", "ASTATY"):
        assert observed["regs"][name] == expected["regs"][name], name
    assert proof.peek(rig[2]["_sp_mismatches"]) == 0


@pytest.mark.parametrize("fault,sticky_bit", [(0x7F7FFFFF, 7), (0x7FC00001, 9)])
def test_divergent_right_does_not_leak_sticky_status(rig, fault, sticky_bit):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    context = regs(state, STKYX=0, STKYY=0x1234)
    for session in (reference, proof):
        for i in range(32):
            session.poke(INPUT + 4 * i, bits(0.2))
        call(session, "stock_render" if session is reference else "_sp_render", context)
        session.poke(state + 22 * 4, bits(2.0))
    proof.poke(rig[2]["_sp_states"] + 24 * 4, fault)
    expected = call(reference, "stock_render", context)
    finish = proof.symbols["_sp_render_finish"]
    proof.runner.breakpoints = frozenset((BACK, finish))
    before_restore = proof.call(
        "_sp_render", context, max_steps=12000, read_regs=tuple(context)
    )
    assert before_restore["halt"]["reason"] == "breakpoint", before_restore
    assert before_restore["final_pc_sw"] == finish
    from tools.sharc_dev.sharc_core.values import _astatx_known_bit

    raw = proof.runner.state.uregs[proof._st.UREG_CODES["STKYX"]]
    assert _astatx_known_bit(raw, sticky_bit) is True
    proof.runner.breakpoints = frozenset((BACK,))
    done = proof.runner.run(100)
    assert done.halt.reason == "breakpoint"
    assert done.final_pc_sw == BACK
    observed = {
        "regs": {
            name: getattr(
                proof.runner.state.uregs.get(proof._st.UREG_CODES[name]), "value", None
            )
            for name in context
        }
    }
    assert values(proof, OUTPUT, 32) == values(reference, OUTPUT, 32)
    assert values(proof, state, 28) == values(reference, state, 28)
    assert observed["regs"]["STKYX"] == expected["regs"]["STKYX"]
    assert observed["regs"]["STKYY"] == expected["regs"]["STKYY"]
    assert proof.peek(rig[2]["_sp_mismatches"]) > 0


@pytest.mark.parametrize("fault", ["copied_input", "copied_history"])
def test_polarity_detects_left_alias_controls_without_left_mutation(rig, fault):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    syms = rig[2]
    proof.poke(syms["_sp_mode"], 1)
    context = regs(state, STKYX=0, STKYY=0x1234)
    for block in range(2 if fault == "copied_history" else 1):
        for session in (reference, proof):
            for i in range(32):
                session.poke(INPUT + 4 * i, bits(0.3 * math.sin(i * 0.31 + block)))
        expected = call(reference, "stock_render", context)
        if fault == "copied_history" and block == 0:
            call(proof, "_sp_render", context)
            assert proof.peek(syms["_sp_mismatches"]) == 0
            continue
        boundary = proof.symbols["_sp_right_start"]
        proof.runner.breakpoints = frozenset((BACK, boundary))
        paused = proof.call("_sp_render", context, max_steps=12000)
        assert paused["final_pc_sw"] == boundary
        if fault == "copied_input":
            for i, word in enumerate(values(proof, INPUT, 32)):
                proof.poke(syms["_sp_audio"] + 4 * i, word)
        else:
            # Deliberately replace independent histories AFTER a nonzero trajectory.
            assert proof.peek(syms["_sp_opposite_history"]) > 0
            for i in (24, 25):
                proof.poke(syms["_sp_states"] + 4 * i, proof.peek(state + 4 * i))
        proof.runner.breakpoints = frozenset((BACK,))
        done = proof.runner.run(12000)
        assert done.halt.reason == "breakpoint" and done.final_pc_sw == BACK
        assert proof.peek(syms["_sp_mismatches"]) > 0
        assert proof.peek(syms["_sp_nonfinite"]) == 0
        assert values(proof, state, 28) == values(reference, state, 28)
        assert values(proof, OUTPUT, 32) == values(reference, OUTPUT, 32)
        for name in (
            "ASTATX",
            "ASTATY",
            "STKYX",
            "STKYY",
            "MODE1",
            "I6",
            "I7",
            "R3",
            "R11",
            "R15",
        ):
            assert (
                proof.runner.state.uregs[proof._st.UREG_CODES[name]].value
                == expected["regs"][name]
            ), name


@pytest.mark.parametrize("mode", [2, 0xFFFFFFFF])
def test_invalid_mode_uses_stock_only(rig, mode):
    proof = fresh(rig)
    state = seed(proof)
    proof.poke(rig[2]["_sp_mode"], mode)
    call(proof, "_sp_render", regs(state))
    assert proof.peek(rig[2]["_sp_mode_errors"]) == 1
    assert proof.peek(rig[2]["_sp_shadow_calls"]) == 0


@pytest.mark.parametrize("entry", ["render", "reset"])
def test_mode_is_latched_before_first_use_and_cannot_switch(rig, entry):
    proof = fresh(rig)
    state = seed(proof)
    syms = rig[2]
    proof.poke(syms["_sp_mode"], 1)
    for i in range(32):
        proof.poke(INPUT + 4 * i, bits(0.2))
    call(proof, f"_sp_{entry}", regs(state))
    assert proof.peek(syms["_sp_active_mode"]) == 1
    shadows = proof.peek(syms["_sp_shadow_calls"])
    proof.poke(syms["_sp_mode"], 0)
    call(proof, "_sp_render", regs(state))
    assert proof.peek(syms["_sp_mode_errors"]) == 1
    assert proof.peek(syms["_sp_shadow_calls"]) == shadows
    proof.poke(syms["_sp_mode"], 1)
    call(proof, "_sp_render", regs(state))
    assert proof.peek(syms["_sp_shadow_calls"]) == shadows


@pytest.mark.parametrize(
    "source", ["input", "coefficient", "left_history", "right_history", "output"]
)
def test_polarity_nonfinite_trajectory_refuses_further_shadows(rig, source):
    reference, proof = fresh(rig), fresh(rig)
    state = seed(reference)
    seed(proof)
    syms = rig[2]
    proof.poke(syms["_sp_mode"], 1)
    for session in (reference, proof):
        for i in range(32):
            session.poke(INPUT + 4 * i, bits(0.2))
    call(reference, "stock_render", regs(state, STKYX=0))
    call(proof, "_sp_render", regs(state, STKYX=0))
    if source == "right_history":
        proof.poke(syms["_sp_states"] + 24 * 4, 0x7FC00001)
    elif source == "output":
        # Finite inputs/state overflow only the deliberately corrupted right.
        proof.poke(syms["_sp_states"] + 24 * 4, 0x7F7FFFFF)
        for session in (reference, proof):
            session.poke(state + 22 * 4, bits(2.0))
    else:
        for session in (reference, proof):
            address = {
                "input": INPUT,
                "coefficient": state + 21 * 4,
                "left_history": state + 24 * 4,
            }[source]
            session.poke(address, 0x7FC00001)
    expected = call(reference, "stock_render", regs(state, STKYX=0))
    observed = call(proof, "_sp_render", regs(state, STKYX=0))
    assert proof.peek(syms["_sp_nonfinite"]) > 0
    assert values(proof, state, 28) == values(reference, state, 28)
    assert values(proof, OUTPUT, 32) == values(reference, OUTPUT, 32)
    for name in ("MODE1", "ASTATX", "STKYX", "I6", "I7"):
        assert observed["regs"][name] == expected["regs"][name]
    shadows = proof.peek(syms["_sp_shadow_calls"])
    call(proof, "_sp_render", regs(state, STKYX=0))
    assert proof.peek(syms["_sp_shadow_calls"]) == shadows


def test_polarity_signed_zero_relation(rig):
    proof = fresh(rig)
    state = seed(proof)
    syms = rig[2]
    proof.poke(syms["_sp_mode"], 1)
    for i in (24, 25):
        proof.poke(state + 4 * i, 0)
    for i in range(32):
        proof.poke(INPUT + 4 * i, (i % 2) << 31)
    call(proof, "_sp_render", regs(state))
    assert proof.peek(syms["_sp_mismatches"]) == 0
    assert proof.peek(syms["_sp_nonfinite"]) == 0
    assert proof.peek(syms["_sp_opposite_audio"]) == 0
