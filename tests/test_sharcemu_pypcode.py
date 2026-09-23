"""Tests for tools/sharcemu.py's Ghidra-free PypcodeConcreteBackend, and a
concrete tools/sharc_trace.py run of FUN_001c2b24's type-cache compare.

Everything here is offline (pypcode + a static image, or tools/sharc_trace.py
directly): no Ghidra JVM, no live project. Each test skips cleanly when the
extracted SHARC region is absent, per CLAUDE.md's "Run the emulator only
when..." and this repo's general pattern of skip-not-fail for optional fixtures.
"""
import os
import sys

import pytest  # pyright: ignore[reportMissingImports]

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(REPO, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

REGION = os.path.join(REPO, "out/sharc/dt2-1.16-main.bin")
REGION_BASE_SW = 0x1C1338
BLOB = os.path.join(REPO, "out/sections/dt2-1.16/section_7_BLOB.bin")

pypcode = pytest.importorskip("pypcode")

pytestmark = pytest.mark.skipif(
    not os.path.exists(REGION),
    reason="out/sharc/dt2-1.16-main.bin not present (extract the 1.16 firmware first)",
)


def _region_bytes():
    with open(REGION, "rb") as fh:
        return fh.read()


def _backend_at(sw):
    import sharcemu  # pyright: ignore[reportMissingImports]

    data = _region_bytes()
    off = (sw - REGION_BASE_SW) * 2
    return sharcemu.PypcodeConcreteBackend(data[off:], 2 * sw)


def test_pypcode_backend_executes_a_real_store():
    """0x1c2b4a: DM(I6 + ...) = M9, part of FUN_001c2b24's register-save
    prologue. Real Type15b semantics (already generated), exercised
    end-to-end with no Ghidra."""
    be = _backend_at(0x1C2B4A)
    be.set_ureg("I6", 0x2A0000)
    be.set_ureg("M9", 0x11111111)
    assert be.step() is True
    ev = be.trace[-1]
    assert ev["action"] == "store"
    assert ev["value"] == 0x11111111


def test_pypcode_backend_compute_bridge_multiplies():
    """0x1c33be: R2 = R5 * R2 (parallel with a DM load). SLEIGH's own
    `compute` marker has no effect (gen_sleigh.py); this checks the
    CALLOTHER bridge to tools/sharc_trace.py's _compute() actually performs
    the multiply and writes R2 back."""
    be = _backend_at(0x1C33BE)
    be.set_ureg("R5", 6)
    be.set_ureg("R2", 7)
    be.set_ureg("I6", 0x2A0000)
    be.step()
    assert be.get_ureg("R2") == 42


def test_pypcode_backend_executes_real_type7d_aconv():
    """0x1c1460: pure Type7d ``I7 = B2W(I7)`` from the DT2 image.

    This checks the generated semantics through the concrete backend rather
    than only inspecting the lifted p-code operations.
    """
    be = _backend_at(0x1C1460)
    be.set_ureg("I7", 0x26F7F0)
    assert be.step() is True
    # The bounded p-code represents the PRM's likely shift only.  Its
    # 0x09BDFC result intentionally differs from the observed mapped hardware
    # result 0x09BE7C until address-map/ILAD semantics are implemented.
    assert be.get_ureg("I7") == 0x09BDFC


def test_pypcode_backend_faults_on_known_gap():
    """0x1c33c4: 5a_move (register copy). gen_sleigh.py has no constructor
    for this form at all (see docs/findings entry this task added): the
    backend must raise, not silently no-op."""
    import sharcemu  # pyright: ignore[reportMissingImports]

    be = _backend_at(0x1C33C4)
    with pytest.raises(sharcemu.PypcodeFault):
        be.step()


def test_predicate_code_0x7_reads_sv_astatx_bit():
    """PGR Table 10-4 code 0x07 (tools/sharc_trace.py's SIMPLE_COND_BITS):
    ASTATX bit SV (shifter overflow) directly; 0x17 is its complement. SV
    is a modelled flag in this backend's ASTATX register -- written by
    _compute_bridge()'s astatx_update callback (tools/sharc_trace.py's
    _compute() lshift/ashift branches via _astatx_shift()) through the
    COMPUTE CALLOTHER -- so _predicate() can read it the same way it
    already reads AZ/AN/AV/AF for the other simple condition codes, rather
    than raising 'not modelled by this draft'."""
    import sharcemu  # pyright: ignore[reportMissingImports]

    be = sharcemu.PypcodeConcreteBackend(b"\x00\x00", 0)
    be.set_ureg("ASTATX", 1 << 11)
    assert be._predicate(0x07) is True
    assert be._predicate(0x17) is False
    be.set_ureg("ASTATX", 0)
    assert be._predicate(0x07) is False
    assert be._predicate(0x17) is True


def test_fun_001c2b24_type_cache_via_sharc_trace():
    """Concrete run of FUN_001c2b24's type-cache compare/store
    (sw 0x1c33bc..0x1c33e9) via tools/sharc_trace.py directly (this form's
    Type3b short-word addressing has no pypcode/SLEIGH semantics yet -- see
    the pypcode fault test above and the task write-up). A synthetic frame
    with a matching cached/live type takes the EQ branch and never writes
    the cache; a mismatch executes both trailing stores in sequence, so the
    final DM(I5+0xc4) value is whichever register the SECOND store uses
    (M14), not the freshly-read per-track field the first store wrote."""
    import sharc_trace as st  # pyright: ignore[reportMissingImports]

    with open(BLOB, "rb") as fh:
        mem = st.LoadedMemory.from_stream(fh.read())

    def run(cached, live, m14):
        i10_frame = 0x290000
        i6_stack = 0x292000
        track_base = i6_stack + 62 * 4
        i5_out = 0x293000
        cache_addr = 0x255970
        overlay = {}

        def poke16(addr, value):
            overlay[addr] = value & 0xFF
            overlay[addr + 1] = (value >> 8) & 0xFF

        def poke32(addr, value):
            for i in range(4):
                overlay[addr + i] = (value >> (8 * i)) & 0xFF

        poke32(track_base, 0x291000)
        poke16(cache_addr, cached)
        poke16(i10_frame, live)
        poke16(0x291000 + 0x54, 0xBEEF)

        uregs = {
            st.UREG_CODES["R5"]: st.Const(0),
            st.UREG_CODES["I6"]: st.Const(i6_stack),
            st.UREG_CODES["I10"]: st.Const(i10_frame),
            st.UREG_CODES["I5"]: st.Const(i5_out),
            st.UREG_CODES["M0"]: st.Const(0),
            st.UREG_CODES["M4"]: st.Const(0),
            st.UREG_CODES["M5"]: st.Const(0),
            st.UREG_CODES["M14"]: st.Const(m14),
            # SISD assumption (MODE1 reset default) so cond=0 (EQ) resolves
            # from AZ alone -- see the task write-up.
            st.UREG_CODES["MODE1"]: st.Const(0),
        }
        state = st.State(0x1C33BC, dict(uregs), concrete=mem, overlay=overlay, assume_nw32=True)
        active = {st._dedupe_key(state): state}
        done = []
        steps = 0
        while active and steps < 200:
            s = active.pop(next(iter(active)))
            if s.pc_sw == 0x1C33E9:
                done.append(s)
                continue
            insn = st.decode_at(mem, None, s.pc_sw)
            for child in st._execute(s, insn):
                if child.stopped:
                    done.append(child)
                else:
                    active[st._dedupe_key(child)] = child
            steps += 1
        assert len(done) == 1
        return st._dm_read(done[0], i5_out + 49 * 4, 4)

    # Match: no write.
    assert run(cached=6, live=6, m14=0xAAAA) is None
    # Mismatch: final value is M14, not the per-track field.
    result = run(cached=6, live=7, m14=0xAAAA)
    assert result is not None and result.value == 0xAAAA
    result2 = run(cached=6, live=7, m14=0x5555)
    assert result2 is not None and result2.value == 0x5555
