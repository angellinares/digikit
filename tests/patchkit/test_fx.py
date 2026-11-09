"""core/fx (modules/core/adapters/fx): the effects adapter, on the concrete SHARC+ core (tests/support/fx_rig.py) and through patchkit.

The first group needs only the assembler; the last one patches the stock images (skipped when they are not in the repository)."""

import json
import os
import random
import subprocess
from pathlib import Path

import pytest
from support import dsp_voice_rig as dv
from support.fx_rig import DIS, MIX, NFAULT, ONE, P0, WAS, FxRig, Row, f2w, nw, w2f
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit as front_end
from tools.patchkit import pk_sharc, pk_toolchains

ROOT = REPO_ROOT

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

SIGNALS = {
    "silence": [0.0] * 64,
    "dc": [0.5] * 64,
    "ramp": [i / 64.0 - 0.5 for i in range(64)],
    "full": [1.0 if i % 2 else -1.0 for i in range(64)],
}


def test_an_empty_chain_changes_nothing_in_any_context():
    """No row: the hook returns to the stock function with every register, the context and the master block as they came."""
    rng = random.Random(7)
    rig = FxRig([])
    for n in range(24):
        master = [w2f(f2w(rng.uniform(-1.5, 1.5))) for _ in range(64)]  # float32 values
        b = rig.block(master)
        assert b.returned, b.halt
        assert b.master == tuple(master)
        assert b.ctx_bad == [] and b.regs_bad == [], (n, b.ctx_bad, b.regs_bad)


def test_the_empty_chain_stores_only_in_the_adapters_scratch_words():
    """The only stores are the three save words of the adapter."""
    rig = FxRig([])
    rig.sess.record_stores()
    rig.block(SIGNALS["ramp"], ctx="friendly")
    syms = rig.built.data
    own = {syms[n] for n in ("_fx_s_r0", "_fx_s_i4", "_fx_s_l4")}
    # the core keeps a store to a not yet mapped address at the loader alias (pk_sharc.SW_ALIAS_BASE + address)
    stores = {
        e.address - pk_sharc.SW_ALIAS_BASE
        if e.address >= pk_sharc.SW_ALIAS_BASE
        else e.address
        for e in rig.sess.stores
    }
    assert stores == own, [hex(a) for a in stores ^ own]


def test_the_empty_chain_costs_a_few_instructions():
    rig = FxRig([])
    b = rig.block(SIGNALS["dc"], ctx="friendly")
    assert 10 <= b.instructions <= 24, b.instructions
    print(f"empty chain: {b.instructions} instructions per block on the concrete core")


def test_free_rows_cost_five_instructions_each_and_change_nothing():
    """The rows core/dynfx reserves (pc 0xffffffff) in front of the zero row: the quick scan walks them, the audio is the stock audio."""
    master = SIGNALS["ramp"]
    base = FxRig([]).block(master, ctx="friendly").instructions
    for n in (1, 3, 4):
        rig = FxRig([Row("", "")] * n)
        b = rig.block(master, ctx="friendly")
        assert b.returned and b.master == tuple(master)
        assert b.ctx_bad == [] and b.regs_bad == []
        assert b.instructions <= base + 6 * n, (n, b.instructions, base)
        print(
            f"{n} free rows: {b.instructions} instructions per block (empty chain {base})"
        )


def test_a_free_row_in_front_of_an_effect_is_skipped_by_the_chain():
    """Run-time effects fill the free rows in any order: the chain runs the effect behind a free row and skips the free row after it."""
    rig = FxRig([Row("", ""), Row("_te_half", "_te_state_a"), Row("", "")])
    for _ in range(3):
        b = rig.block(RAMP, ctx="friendly")
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
    assert b.master == tuple(HALF)
    assert rig.row_word(0, WAS) == 0 and rig.row_word(1, WAS) == 1


# ---- the chain ------------------------------------------------------------------------------------------------------------
def f32(x: float) -> float:
    return w2f(f2w(x))


def fade(dry, wet, mode: str):
    """The adapter's fade in float32: master = dry + (wet - dry) * w, w = (k + 1) / 32 ("in") or 1 - (k + 1) / 32 ("out"), per channel."""
    out = []
    for ch in (0, 32):
        w = 0.0 if mode == "in" else 1.0
        dw = 1 / 32 if mode == "in" else -1 / 32
        for k in range(32):
            w = f32(w + dw)
            d, t = dry[ch + k], wet[ch + k]
            out.append(f32(d + f32(f32(t - d) * w)))
    return tuple(out)


RAMP = [f32(i / 64.0 - 0.5) for i in range(64)]
HALF = [f32(x * 0.5) for x in RAMP]


def test_an_effect_fades_in_over_the_first_block_and_then_replaces_the_master():
    rig = FxRig([Row("_te_half", "_te_state_a")])
    b0 = rig.block(RAMP)
    assert b0.returned and b0.ctx_bad == [] and b0.regs_bad == []
    assert b0.master == fade(RAMP, HALF, "in")
    for _ in range(3):
        b = rig.block(RAMP)
        assert b.master == tuple(HALF)
        assert b.ctx_bad == [] and b.regs_bad == []
    assert rig.row_word(0, WAS) == 1  # was


def test_a_new_effect_gets_a_zeroed_state_when_it_switches_on():
    rig = FxRig([Row("_te_half", "_te_state_a")])
    rig.block(RAMP)
    assert [rig.state_word(0, k) for k in range(4)] == [
        1,
        0,
        0,
        0,
    ]  # the poison is gone, one call
    rig.block(RAMP)
    rig.block(RAMP)
    assert rig.state_word(0, 0) == 3
    rig.set_enabled(0, False)
    rig.block(RAMP)  # the fade out runs it once more
    assert rig.state_word(0, 0) == 4
    rig.set_enabled(0, True)
    rig.block(RAMP)
    assert rig.state_word(0, 0) == 1  # a fresh state


def test_an_effect_fades_out_for_one_block_and_is_not_called_while_off():
    rig = FxRig([Row("_te_half", "_te_state_a")])
    for _ in range(2):
        rig.block(RAMP)
    rig.set_enabled(0, False)
    b = rig.block(RAMP)
    assert b.master == fade(RAMP, HALF, "out")
    assert (
        b.master[31] == RAMP[31] and b.master[63] == RAMP[63]
    )  # the last sample is dry
    calls = rig.state_word(0, 0)
    for _ in range(5):
        b = rig.block(RAMP)
        assert b.master == tuple(RAMP)
    assert rig.state_word(0, 0) == calls
    assert rig.row_word(0, WAS) == 0


def test_a_row_that_starts_off_is_skipped():
    rig = FxRig([Row("_te_half", "_te_state_a", enabled=0)])
    for _ in range(3):
        b = rig.block(RAMP)
        assert b.master == tuple(RAMP)
    assert rig.state_word(0, 0) == 0x7FC00001  # never touched


def test_the_chain_runs_in_row_order_and_each_effect_sees_the_previous_output():
    for first, second, want in (
        ("_te_half", "_te_add1", lambda x: f32(f32(x * 0.5) + 1.0)),
        ("_te_add1", "_te_half", lambda x: f32(f32(x + 1.0) * 0.5)),
    ):
        rig = FxRig([Row(first, "_te_state_a"), Row(second, "_te_state_b")])
        rig.block(RAMP)
        b = rig.block(RAMP)
        assert b.master == tuple(want(x) for x in RAMP), (first, second)


def test_four_effects_chain():
    rows = [
        Row("_te_half", "_te_state_a"),
        Row("_te_half", "_te_state_b"),
        Row("_te_add1", "_te_state_a"),
        Row("_te_add1", "_te_state_b"),
    ]
    rig = FxRig(rows)
    rig.block(RAMP)
    b = rig.block(RAMP)
    assert b.master == tuple(
        f32(f32(f32(f32(x * 0.5) * 0.5) + 1.0) + 1.0) for x in RAMP
    )
    with pytest.raises(ValueError):
        FxRig([*rows, Row("_te_half", "_te_state_a")])


def test_the_process_routine_gets_the_row_parameters_and_the_block_number():
    rig = FxRig([Row("_te_param", "_te_state_a", params=(7, 0, 0, 0))])
    for n in (1, 2, 3):
        b = rig.block(RAMP)
        assert (
            rig.state_word(0, 1) == n
        )  # dk_fx_in.block counts the blocks that ran a chain
    assert b.master == (7.0,) * 64
    rig.set_row_word(0, P0, 3)
    assert rig.block(RAMP).master == (3.0,) * 64


@pytest.mark.parametrize("effect", ["_te_nan", "_te_big", "_te_neg"])
def test_a_bad_block_plays_dry_and_clears_the_state(effect):
    """The faulting effect sits behind a working one: the master block after it is the working effect's output, unchanged by the fault."""
    rig = FxRig([Row("_te_half", "_te_state_a"), Row(effect, "_te_state_b")])
    rig.block(RAMP)
    for _ in range(3):
        b = rig.block(RAMP)
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.master == tuple(HALF) or b.master == fade(RAMP, HALF, "in")
    assert [rig.state_word(1, k) for k in range(4)] == [
        0,
        0,
        0,
        0,
    ]  # the poison is cleared by the fault
    assert rig.row_word(1, WAS) == 0  # was
    assert (
        rig.row_word(1, NFAULT) == 4
    )  # faults since the start (the first block faults too)
    assert rig.row_word(0, NFAULT) == 0


def test_the_eighth_fault_within_1500_blocks_switches_the_effect_off():
    rig = FxRig([Row("_te_nan", "_te_state_a")])
    for n in range(7):
        rig.block(RAMP)
        assert rig.row_word(0, DIS) == 0, n
    b = rig.block(RAMP)
    assert b.master == tuple(RAMP)
    assert rig.row_word(0, DIS) == 1  # dis
    assert rig.row_word(0, NFAULT) == 8
    for _ in range(5):
        assert rig.block(RAMP).master == tuple(RAMP)
    assert rig.row_word(0, NFAULT) == 8  # not called any more
    assert rig.symbol_word("_fx_diag", 1) == 8


def test_faults_spread_over_more_than_1500_blocks_do_not_switch_it_off():
    rig = FxRig([Row("_te_nan", "_te_state_a")])
    for _ in range(7):
        rig.block(RAMP)
    rig.set_enabled(0, False)
    for _ in range(1600):
        rig.block(RAMP)
    rig.set_enabled(0, True)
    for _ in range(7):  # the window restarts: 7 faults again
        rig.block(RAMP)
    assert rig.row_word(0, DIS) == 0
    assert rig.row_word(0, NFAULT) == 14
    rig.block(RAMP)
    assert rig.row_word(0, DIS) == 1


def test_an_effect_that_clobbers_every_allowed_register_changes_nothing_else():
    """The module may clobber R0-R15, I0-I5, M1-M4 and B3; the stock function gets all of them back, in 24 hostile contexts."""
    rig = FxRig([Row("_te_trash", "_te_state_a")])
    rng = random.Random(3)
    for _ in range(24):
        master = [f32(rng.uniform(-1.5, 1.5)) for _ in range(64)]
        b = rig.block(master)
        assert b.returned, b.halt
        assert b.ctx_bad == [] and b.regs_bad == [], (b.ctx_bad, b.regs_bad)
    assert rig.block(master).master == tuple(master)  # out = in


def test_the_cost_of_a_chain_in_instructions():
    rig = FxRig([Row("_te_half", "_te_state_a")])
    rig.block(RAMP, ctx="friendly")
    steady = rig.block(RAMP, ctx="friendly").instructions
    rig.set_enabled(0, False)
    fade_out = rig.block(RAMP, ctx="friendly").instructions
    rig.block(RAMP, ctx="friendly")
    off = rig.block(RAMP, ctx="friendly").instructions
    print(
        f"one effect: steady {steady}, fade {fade_out}, off {off} instructions per block (the effect itself is 64 samples of 3 instructions)"
    )
    assert steady < 1200 and fade_out < 1800 and off < 200


def blend(dry, wet, mix: float, w=None):
    """The adapter's blend in float32: master = dry + (wet - dry) * (w * mix), w = 1.0 when steady."""
    m = f32(mix)
    out = []
    for ch in (0, 32):
        for k in range(32):
            wk = 1.0 if w is None else w[ch + k]
            d, t = dry[ch + k], wet[ch + k]
            out.append(f32(d + f32(f32(t - d) * f32(wk * m))))
    return tuple(out)


def fade_weights(mode: str):
    """The fade weights of `fade` (the adapter's float32 steps), per sample."""
    out = []
    for _ch in (0, 32):
        w = 0.0 if mode == "in" else 1.0
        dw = 1 / 32 if mode == "in" else -1 / 32
        for _k in range(32):
            w = f32(w + dw)
            out.append(w)
    return out


def test_mix_zero_leaves_the_master_dry_bit_for_bit_and_the_effect_still_runs():
    rig = FxRig([Row("_te_half", "_te_state_a", mix=0)])
    for n in range(1, 5):
        b = rig.block(RAMP)
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.master == tuple(RAMP), n
    assert rig.state_word(0, 0) == 4  # called every block
    assert rig.row_word(0, WAS) == 1


def test_mix_one_is_the_wet_signal_bit_for_bit():
    rig = FxRig([Row("_te_half", "_te_state_a", mix=ONE)])
    rig.block(RAMP)
    assert rig.block(RAMP).master == tuple(HALF)


def test_an_intermediate_mix_is_the_linear_blend_in_the_steady_mode_and_in_the_fades():
    for mix in (0.25, 0.5, 0.75):
        rig = FxRig([Row("_te_half", "_te_state_a", mix=f2w(mix))])
        b0 = rig.block(RAMP)
        assert b0.master == blend(RAMP, HALF, mix, fade_weights("in")), mix
        for _ in range(2):
            b = rig.block(RAMP)
            assert b.master == blend(RAMP, HALF, mix), mix
            assert b.ctx_bad == [] and b.regs_bad == []
        rig.set_enabled(0, False)
        assert rig.block(RAMP).master == blend(RAMP, HALF, mix, fade_weights("out")), (
            mix
        )
        assert rig.block(RAMP).master == tuple(RAMP)


def test_the_mix_can_change_between_blocks():
    rig = FxRig([Row("_te_half", "_te_state_a")])
    rig.block(RAMP)
    assert rig.block(RAMP).master == tuple(HALF)
    rig.set_row_word(0, MIX, f2w(0.5))
    assert rig.block(RAMP).master == blend(RAMP, HALF, 0.5)
    rig.set_row_word(0, MIX, 0)
    assert rig.block(RAMP).master == tuple(RAMP)
    rig.set_row_word(0, MIX, ONE)
    assert rig.block(RAMP).master == tuple(HALF)


def test_mix_does_not_hide_a_fault():
    """The output check comes before the blend: a faulting effect counts its fault at any mix."""
    rig = FxRig([Row("_te_nan", "_te_state_a", mix=0)])
    rig.block(RAMP)
    assert rig.row_word(0, NFAULT) == 1


def test_the_cost_of_the_mix_per_active_row():
    def steady(mix: int) -> int:
        rig = FxRig([Row("_te_half", "_te_state_a", mix=mix)])
        rig.block(RAMP, ctx="friendly")
        return rig.block(RAMP, ctx="friendly").instructions

    one, zero, half = steady(ONE), steady(0), steady(f2w(0.5))
    print(
        f"one effect steady: mix 1.0 {one}, mix 0 {zero}, mix 0.5 {half} instructions per block"
    )
    assert one < 830 and zero < one and half < one + 420


GAIN = ROOT / "tests/fixtures/modules/fx-gain/src/dsp/fx_gain.s"


def test_the_fixture_test_fx_gain_halves_the_master_through_the_adapter():
    rig = FxRig([Row("_fg_process", "_fg_state", desc="_fg_desc")], sources=(GAIN,))
    rig.block(RAMP)
    for _ in range(3):
        b = rig.block(RAMP)
        assert b.master == tuple(HALF)
        assert b.ctx_bad == [] and b.regs_bad == []
    assert rig.symbol_word("_fg_gain") == f2w(0.5)
    assert rig.block(RAMP).instructions < 860


# ---- patchkit: the hook on both devices ------------------------------------------------------------------------------------
DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
DN2_112 = config.stock("Digitone_II_OS1.12.syx")
needs_images = pytest.mark.skipif(
    not all(p.exists() for p in (DT2_117, DN2_112)),
    reason="stock firmware images not present",
)


@pytest.fixture(scope="module")
def patchkit():
    exe = front_end.binary()

    def run(*args):
        r = subprocess.run(
            [str(exe), "--root", str(ROOT), *map(str, args)],
            capture_output=True,
            text=True,
            env={**os.environ, "PATCHKIT_MODULES": str(ROOT / "out" / "modules")},
        )
        assert r.returncode == 0, f"patchkit {args}: {r.stderr}{r.stdout}"
        return r

    return run


@needs_images
@pytest.mark.parametrize(
    ("fw", "second"),
    [(DT2_117, 0x1C80F5), (DN2_112, 0x1C2450)],
    ids=["dt2-1.17", "dn2-1.12"],
)
def test_the_hook_takes_the_first_instruction_of_a_function_between_master_and_pack(
    patchkit, tmp_path: Path, fw, second
):
    out = tmp_path / "fx.syx"
    plan_path = tmp_path / "fx.plan.json"
    patchkit(
        "apply",
        "--firmware",
        fw,
        "--modules",
        "core/fx",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    (hook,) = [h for h in plan["hooks"] if h["anchor"] == "dsp.fx.fn.entry"]
    assert (
        hook["cpu"] == "sharc" and hook["displaced"] == "run" and hook["site_len"] == 6
    )
    assert hook["tail"] is not None
    assert (
        hook["ret"] == second
    )  # the tail returns to the second instruction (the first is 3 parcels, 48 bits)
    assert any(k.endswith(":_fx_entry") for k in plan["symbols"])
    assert plan["symbols"]["core/fx@0.1.0:fx.rows"] > 0


@needs_images
@pytest.mark.parametrize("fw", [DT2_117, DN2_112], ids=["dt2-1.17", "dn2-1.12"])
def test_the_fixture_puts_one_row_in_the_chain_on_both_devices(
    patchkit, tmp_path: Path, fw
):
    out = tmp_path / "fx-gain.syx"
    plan_path = tmp_path / "fx-gain.plan.json"
    patchkit(
        "apply",
        "--firmware",
        fw,
        "--modules",
        "core/fx,test/fx-gain",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    sym = plan["symbols"]
    assert sym["core/fx@0.1.0:fx.count"] == 1
    assert sym["core/fx@0.1.0:fx.row_size"] == 96
    assert (
        "test/fx-gain@0.1.0:_fg_process" in sym
        and "test/fx-gain@0.1.0:_fg_state" in sym
    )


@needs_images
def test_the_dsp_diff_of_the_patched_image_is_the_hook_and_the_placed_code(
    patchkit, tmp_path: Path
):
    """tools/patchkit/pk_dsp_diff.py: a headless run (coupled_ab --patch) gets the hook word and the appended L2 bytes, and nothing else."""
    from tools.patchkit import pk_dsp_diff

    out = tmp_path / "fx.syx"
    patchkit(
        "apply",
        "--firmware",
        DT2_117,
        "--modules",
        "core/fx",
        "-o",
        out,
        "--plan",
        tmp_path / "p.json",
    )
    text = pk_dsp_diff.patch_text(DT2_117, out)
    lines = [ln.split() for ln in text.splitlines() if ln.startswith("BYTES")]
    # the send hook (master function FUN_1c207b) and the master hook (FUN_1c80f2), each at the alias of its first instruction
    assert [ln[1] for ln in lines[:2]] == [
        f"{0x28000000 + 2 * 0x1C207B:x}",
        f"{0x28000000 + 2 * 0x1C80F2:x}",
    ]
    assert all(len(ln[2]) == 12 for ln in lines[:2])  # one 48-bit jump each
    assert all(
        int(ln[1], 16) >= 0x2001AB7C for ln in lines[2:]
    )  # the rest is appended L2


def test_a_module_call_leaves_its_site_in_both_reply_records():
    """The fault table (modules/core/adapters/dsp-link/src/dsp/dsp-link.inc): the core writes `last` (stage | site << 8 | row byte offset << 16) into its own
    word and, at once, into both reply records with the header; it clears the module step word `_dk_step` (`_fx_rec[2]`) before the call, and does not
    touch the records' word 25 (the block tail copies the step there)."""
    for route, site in ((0, 2), (1, 3)):
        rig = FxRig(
            [
                Row("", ""),
                Row("_te_half", "_te_state_a", route=route, amounts=(127,) * 16),
            ]
        )
        rec = rig.built.data["_fx_rec"]
        rig.sess.poke(nw(rec), dv.RING_A)
        rig.sess.poke(nw(rec) + 1, dv.RING_B)
        rig.sess.poke(nw(rec) + 2, rig.built.data["_dk_step"])
        rig.sess.poke(nw(rig.built.data["_dk_step"]), 7)  # an old module step
        for ring in (dv.RING_A, dv.RING_B):
            rig.sess.poke(
                nw(ring) + 298, 9
            )  # a step the tail copied in an earlier block
        b = (
            rig.block(SIGNALS["ramp"], ctx="friendly")
            if route == 0
            else rig.send_block(ctx="friendly")
        )
        assert b.returned, b.halt
        want = 5 | site << 8 | 96 << 16  # returned, the second row of the table
        assert rig.symbol_word("_fx_diag", 0) == want, hex(
            rig.symbol_word("_fx_diag", 0)
        )
        for ring in (dv.RING_A, dv.RING_B):
            assert rig.sess.peek(nw(ring) + 297) == want
            assert rig.sess.peek(nw(ring) + 45) == 0x20D1A6
            assert rig.sess.peek(nw(ring) + 298) == 9, (
                "the records' word 25 is the tail's"
            )
        assert rig.sess.peek(nw(rig.built.data["_dk_step"])) == 0, (
            "cleared at the call; the test effect stores no step"
        )
        assert rig.symbol_word("_fx_diag", 4 + route) == 1, (
            "one module call on this route"
        )
        assert rig.symbol_word("_fx_diag", 5 - route) == 0


def test_a_faulting_effect_names_its_row_in_the_fault_words():
    rig = FxRig([Row("", ""), Row("_te_nan", "_te_state_a")])
    for _ in range(8):
        rig.block(SIGNALS["dc"], ctx="friendly")
    assert rig.symbol_word("_fx_diag", 1) == 8  # faults of all effects
    assert (
        rig.symbol_word("_fx_diag", 2) == 96 + 1
    )  # the row of the last fault (byte offset + 1)
    assert rig.symbol_word("_fx_diag", 3) == 96 + 1  # switched off by the eighth
