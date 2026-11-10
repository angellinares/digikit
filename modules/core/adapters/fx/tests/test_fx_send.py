"""core/fx, the send route (modules/core/adapters/fx/src/dsp/fxsend.s, finding 62): the send hook and the master hook on the concrete SHARC+ core.

The first group needs only the assembler (tests/support/fx_rig.py); the last one patches the stock Digitakt II image (skipped when it is not in the repository).
The model of the send bus is the routine's own float32 arithmetic: gain (byte / 127) squared, ramped by adding the step per sample, tracks summed in order."""

import json
import os
import random
import subprocess
from pathlib import Path

import pytest
from support.fx_rig import (
    DIS,
    NFAULT,
    ONE,
    ROUTE_SEND,
    TRACKS,
    WAS,
    FxRig,
    Row,
    f2w,
    w2f,
)
from support.paths import REPO_ROOT

from tools.firmware import config
from tools.patchkit import patchkit as front_end
from tools.patchkit import pk_sharc, pk_toolchains

ROOT = REPO_ROOT

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


def f32(x: float) -> float:
    return w2f(f2w(x))


RNG = random.Random(11)
RAMP = [f32(i / 64.0 - 0.5) for i in range(64)]
SIGNALS = [[f32(RNG.uniform(-1.0, 1.0)) for _ in range(64)] for _ in range(TRACKS)]
ONE_127 = w2f(0x3C010204)  # 1 / 127
STEP = 1 / 32
POISON = 0x7FC00001


def gain(byte: int) -> float:
    """The gain of an amount byte: (byte / 127) squared in the routine's float32 operations."""
    a = f32(float(min(byte, 127)) * ONE_127)
    return f32(a * a)


def send_bus(amounts, old):
    """The bus of one block: the sum over the tracks of g_k * the track, g_k = old + (k + 1) * step added per sample; returns (bus, new gains)."""
    new = [gain(b) for b in (tuple(amounts) + (0,) * TRACKS)[:TRACKS]]
    bus = [0.0] * 64
    for t in range(TRACKS):
        if new[t] == 0.0 and old[t] == 0.0:
            continue
        step = f32(f32(new[t] - old[t]) * STEP)
        g = old[t]
        for k in range(32):
            g = f32(g + step)
            for ch in (0, 32):
                bus[ch + k] = f32(bus[ch + k] + f32(SIGNALS[t][ch + k] * g))
    return bus, new


def weights(mode: str):
    """The weights of the return add per sample (both channels): 1.0 steady, (k + 1) / 32 in, 1 - (k + 1) / 32 out, as float32 sums."""
    out = []
    for _ch in (0, 32):
        w = {"steady": 1.0, "in": 0.0, "out": 1.0}[mode]
        dw = {"steady": 0.0, "in": STEP, "out": -STEP}[mode]
        for _k in range(32):
            w = f32(w + dw)
            out.append(w)
    return out


def add_return(ret, wet, mix: float, mode: str):
    w = weights(mode)
    m = f32(mix)
    return tuple(
        f32(r + f32(x * f32(wk * m))) for r, x, wk in zip(ret, wet, w, strict=True)
    )


def load(rig: FxRig) -> None:
    for t in range(TRACKS):
        rig.set_track(t, SIGNALS[t])


AMOUNTS = (0, 0, 0, 127, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1)


def send_row(**kw):
    kw.setdefault("amounts", AMOUNTS)
    return Row("_te_half", "_te_state_a", route=ROUTE_SEND, **kw)


# ---- the master chain skips send rows -------------------------------------------------------------------------------------
def test_a_send_row_is_not_run_by_the_master_chain():
    rig = FxRig([send_row()])
    for _ in range(3):
        b = rig.block(RAMP)
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.master == tuple(RAMP)
    assert rig.state_word(0, 0) == POISON  # never touched
    assert rig.row_word(0, WAS) == 0


def test_a_send_row_costs_the_master_hook_the_quick_scan_and_the_count_only():
    """With only send rows the master hook does not save the context: its quick scan counts the block and goes on."""
    master = RAMP
    base = FxRig([Row("", "")]).block(master, ctx="friendly").instructions
    rig = FxRig([send_row()])
    b = rig.block(master, ctx="friendly")
    print(
        f"master hook, one send row: {b.instructions} instructions (a free row: {base})"
    )
    assert b.instructions <= base + 10


def test_the_master_chain_runs_the_master_row_beside_a_send_row():
    rig = FxRig([send_row(), Row("_te_add1", "_te_state_b")])
    rig.block(RAMP)
    b = rig.block(RAMP)
    assert b.master == tuple(f32(x + 1.0) for x in RAMP)
    assert rig.state_word(0, 0) == POISON
    assert rig.state_word(1, 0) == 2


# ---- the block counter ------------------------------------------------------------------------------------------------------
def test_the_block_number_advances_once_a_block_with_or_without_a_send_row():
    """dk_fx_in.block of a master effect is the same sequence with a send row beside it; the send pass runs first in the block and sees the number of the block before."""
    n = 6
    alone = FxRig([Row("_te_param", "_te_state_a", params=(7, 0, 0, 0))])
    seen_alone = []
    for _ in range(n):
        alone.block(RAMP)
        seen_alone.append(alone.state_word(0, 1))
    both = FxRig(
        [
            Row("_te_param", "_te_state_a", params=(7, 0, 0, 0)),
            Row("_te_param", "_te_state_b", route=ROUTE_SEND, params=(7, 0, 0, 0)),
        ]
    )
    seen_master, seen_send = [], []
    for _ in range(n):
        both.send_block()
        seen_send.append(both.state_word(1, 1))
        both.block(RAMP)
        seen_master.append(both.state_word(0, 1))
    assert seen_alone == list(range(1, n + 1))
    assert seen_master == seen_alone
    assert seen_send == list(range(0, n))


def test_a_chain_of_send_rows_alone_still_counts_the_blocks():
    """No master row: the master hook's quick scan counts the block (it runs after the send hook), the send pass sees the previous block's number."""
    rig = FxRig(
        [Row("_te_param", "_te_state_a", route=ROUTE_SEND, params=(7, 0, 0, 0))]
    )
    seen = []
    for _ in range(5):
        rig.send_block()
        seen.append(rig.state_word(0, 1))
        b = rig.block(RAMP)
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.master == tuple(RAMP)
    assert seen == [0, 1, 2, 3, 4]
    assert rig.symbol_word("_fx_blk") == 5


def real_block_of(rig: FxRig) -> None:
    """One real block: the send hook, then the master hook."""
    rig.send_block()
    rig.block(RAMP)


def test_a_free_row_and_a_send_row_before_a_master_row_count_once():
    rig = FxRig(
        [
            Row("", ""),
            Row("_te_param", "_te_state_a", route=ROUTE_SEND, params=(7, 0, 0, 0)),
            Row("_te_param", "_te_state_b", params=(7, 0, 0, 0)),
        ]
    )
    for n in range(1, 5):
        real_block_of(rig)
        assert rig.state_word(2, 1) == n, n
        assert rig.symbol_word("_fx_blk") == n


def fault_rig() -> FxRig:
    rig = FxRig(
        [
            Row("_te_nan", "_te_state_a", route=ROUTE_SEND, amounts=AMOUNTS),
            Row("_te_half", "_te_state_b"),
        ]
    )
    load(rig)
    return rig


def test_the_fault_window_is_1500_real_blocks_with_a_send_row():
    """Seven faults, 1000 real blocks off, the eighth fault: inside the window, so the effect is switched off (with the counter advancing twice a block the
    window would have been 750 blocks and the count would have started again)."""
    rig = fault_rig()
    for _ in range(7):
        real_block_of(rig)
    assert rig.row_word(0, NFAULT) == 7 and rig.row_word(0, DIS) == 0
    rig.set_enabled(0, False)
    for _ in range(1000):
        real_block_of(rig)
    rig.set_enabled(0, True)
    real_block_of(rig)
    assert rig.row_word(0, NFAULT) == 8
    assert rig.row_word(0, DIS) == 1


def test_faults_more_than_1500_real_blocks_apart_do_not_switch_a_send_effect_off():
    rig = fault_rig()
    for _ in range(7):
        real_block_of(rig)
    rig.set_enabled(0, False)
    for _ in range(1600):
        real_block_of(rig)
    rig.set_enabled(0, True)
    real_block_of(rig)
    assert rig.row_word(0, NFAULT) == 8
    assert rig.row_word(0, DIS) == 0


# ---- the send hook ---------------------------------------------------------------------------------------------------------
def test_no_send_row_leaves_the_return_and_the_master_alone():
    rng = random.Random(5)
    rig = FxRig([Row("_te_half", "_te_state_a"), Row("", "")])
    load(rig)
    for n in range(8):
        ret = [f32(rng.uniform(-1.0, 1.0)) for _ in range(64)]
        master = [f32(rng.uniform(-1.0, 1.0)) for _ in range(64)]
        rig.set_return(ret)
        b = rig.send_block(master)
        assert b.returned and b.ctx_bad == [] and b.regs_bad == [], (n, b.halt)
        assert b.ret == tuple(ret) and b.master == tuple(master)
    assert rig.state_word(0, 0) == POISON


def test_an_empty_chain_and_free_rows_cost_the_send_hook_a_few_instructions():
    for rows in ([], [Row("", "")] * 3, [Row("_te_half", "_te_state_a")]):
        rig = FxRig(list(rows))
        b = rig.send_block(ctx="friendly")
        print(f"send hook, {len(rows)} rows, none routed to send: {b.instructions}")
        assert b.returned and b.instructions <= 46


def test_the_send_bus_is_the_sum_of_the_gained_tracks_and_the_return_gets_the_wet_bus():
    rig = FxRig([send_row()])
    load(rig)
    old = [0.0] * TRACKS
    ret = [f32(0.01 * k) for k in range(64)]
    rig.set_return(ret)
    # block 0: the row switches on (fade in), the gains ramp from 0
    bus, old = send_bus(AMOUNTS, old)
    b = rig.send_block()
    assert b.returned and b.ctx_bad == [] and b.regs_bad == []
    assert rig.bus() == tuple(bus)
    wet = tuple(f32(x * 0.5) for x in bus)
    want = add_return(ret, wet, 1.0, "in")
    assert b.ret == want
    assert rig.prev_gains(0) == tuple(old)
    # the tracks and the master are not touched
    assert b.master == (0.0,) * 64
    # block 1 and 2: steady, constant gains
    for _ in range(2):
        rig.set_return(ret)
        bus, old = send_bus(AMOUNTS, old)
        b = rig.send_block()
        assert rig.bus() == tuple(bus)
        wet = tuple(f32(x * 0.5) for x in bus)
        assert b.ret == add_return(ret, wet, 1.0, "steady")
    assert rig.row_word(0, WAS) == 1


def test_only_tracks_with_an_amount_contribute():
    """An amount on track 5 only: the bus is that track times its gain, the same signals on the other tracks add nothing."""
    amounts = tuple(64 if t == 5 else 0 for t in range(TRACKS))
    rig = FxRig([send_row(amounts=amounts)])
    load(rig)
    rig.send_block()  # the ramp block
    rig.send_block()
    g = gain(64)
    assert rig.bus() == tuple(f32(x * g) for x in SIGNALS[5])
    amounts = tuple(64 if t == 6 else 0 for t in range(TRACKS))
    rig.set_amounts(0, amounts)
    rig.send_block()  # track 5 ramps down, track 6 up
    rig.send_block()
    assert rig.bus() == tuple(f32(x * g) for x in SIGNALS[6])


def test_the_gain_is_the_amount_squared_over_127_squared():
    for byte in (0, 1, 32, 64, 100, 127, 128, 255):
        amounts = tuple(byte if t == 0 else 0 for t in range(TRACKS))
        rig = FxRig([send_row(amounts=amounts)])
        load(rig)
        rig.send_block()
        rig.send_block()
        want = min(byte, 127) ** 2 / 127**2
        got = rig.prev_gains(0)[0]
        assert abs(got - want) < 2e-7, (byte, got, want)


def test_a_change_of_amount_ramps_over_the_block():
    amounts = tuple(127 if t == 2 else 0 for t in range(TRACKS))
    rig = FxRig([send_row(amounts=amounts)])
    load(rig)
    rig.send_block()
    rig.send_block()
    old = [gain(b) for b in amounts]
    new_amounts = tuple(32 if t == 2 else 0 for t in range(TRACKS))
    rig.set_amounts(0, new_amounts)
    bus, old = send_bus(new_amounts, old)
    rig.send_block()
    assert rig.bus() == tuple(bus)
    # linear: the first sample is near the old gain, the last is the new gain
    x = SIGNALS[2]
    assert abs(bus[0] / x[0] - 1.0) < 0.05
    assert abs(bus[31] / x[31] - gain(32)) < 1e-6
    rig.send_block()
    assert rig.bus() == tuple(f32(v * gain(32)) for v in SIGNALS[2])


def test_all_amounts_zero_leave_the_return_bit_for_bit():
    rng = random.Random(8)
    rig = FxRig([send_row(amounts=(0,) * TRACKS)])
    load(rig)
    for _ in range(5):
        ret = [f32(rng.uniform(-1.0, 1.0)) for _ in range(64)]
        rig.set_return(ret)
        b = rig.send_block()
        assert b.ret == tuple(ret)
        assert rig.bus() == (0.0,) * 64
    assert rig.state_word(0, 0) == 5  # the effect runs (its tail would go on)


def test_mix_is_the_return_level():
    for mix in (0.0, 0.25, 1.0):
        rig = FxRig([send_row(mix=f2w(mix))])
        load(rig)
        ret = [f32(0.01 * k) for k in range(64)]
        old = [0.0] * TRACKS
        for n in range(3):
            rig.set_return(ret)
            bus, old = send_bus(AMOUNTS, old)
            b = rig.send_block()
            wet = tuple(f32(x * 0.5) for x in bus)
            mode = "in" if n == 0 else "steady"
            assert b.ret == (
                tuple(ret) if mix == 0.0 else add_return(ret, wet, mix, mode)
            ), (mix, n)
        assert rig.state_word(0, 0) == 3  # mix 0 still runs the effect


def test_the_effect_fades_out_over_one_block_and_is_not_called_while_off():
    rig = FxRig([send_row()])
    load(rig)
    old = [0.0] * TRACKS
    ret = [f32(0.01 * k) for k in range(64)]
    for _ in range(2):
        rig.set_return(ret)
        bus, old = send_bus(AMOUNTS, old)
        rig.send_block()
    rig.set_enabled(0, False)
    rig.set_return(ret)
    bus, old = send_bus(AMOUNTS, old)
    b = rig.send_block()
    wet = tuple(f32(x * 0.5) for x in bus)
    assert b.ret == add_return(ret, wet, 1.0, "out")
    assert b.ret[31] == ret[31] and b.ret[63] == ret[63]  # the last sample adds 0
    calls = rig.state_word(0, 0)
    for _ in range(4):
        rig.set_return(ret)
        b = rig.send_block()
        assert b.ret == tuple(ret)
    assert rig.state_word(0, 0) == calls
    assert rig.row_word(0, WAS) == 0


def test_a_row_that_starts_off_is_skipped_by_the_send_hook():
    rig = FxRig([send_row(enabled=0)])
    load(rig)
    ret = [f32(0.01 * k) for k in range(64)]
    rig.set_return(ret)
    b = rig.send_block()
    assert b.ret == tuple(ret) and rig.state_word(0, 0) == POISON


def test_a_free_row_in_front_and_a_second_send_row():
    rows = [
        Row("", ""),
        send_row(),
        send_row(amounts=tuple(127 if t == 0 else 0 for t in range(TRACKS))),
    ]
    rig = FxRig(rows)
    load(rig)
    ret = [f32(0.01 * k) for k in range(64)]
    olds = [[0.0] * TRACKS, [0.0] * TRACKS]
    for n in range(3):
        rig.set_return(ret)
        want = tuple(ret)
        for i in (1, 2):
            bus, olds[i - 2] = send_bus(rows[i].amounts, olds[i - 2])
            wet = tuple(f32(x * 0.5) for x in bus)
            want = add_return(want, wet, 1.0, "in" if n == 0 else "steady")
        b = rig.send_block()
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.ret == want, n
    assert rig.prev_gains(1)[3] == gain(127) and rig.prev_gains(2)[0] == gain(127)


@pytest.mark.parametrize("effect", ["_te_nan", "_te_big", "_te_neg"])
def test_a_bad_block_adds_nothing_to_the_return(effect):
    rig = FxRig([Row(effect, "_te_state_a", route=ROUTE_SEND, amounts=AMOUNTS)])
    load(rig)
    ret = [f32(0.01 * k) for k in range(64)]
    for _ in range(3):
        rig.set_return(ret)
        b = rig.send_block()
        assert b.returned and b.ctx_bad == [] and b.regs_bad == []
        assert b.ret == tuple(ret)
    assert rig.row_word(0, NFAULT) == 3
    assert rig.row_word(0, WAS) == 0


def test_the_eighth_fault_switches_a_send_effect_off():
    rig = FxRig([Row("_te_nan", "_te_state_a", route=ROUTE_SEND, amounts=AMOUNTS)])
    load(rig)
    for _ in range(8):
        rig.send_block()
    assert rig.row_word(0, NFAULT) == 8
    for _ in range(3):
        rig.send_block()
    assert rig.row_word(0, NFAULT) == 8  # not called any more


def test_the_send_hook_gives_every_register_and_the_context_back_in_hostile_contexts():
    rng = random.Random(3)
    rig = FxRig([send_row(), Row("_te_trash", "_te_state_b", route=ROUTE_SEND)])
    load(rig)
    for n in range(24):
        master = [f32(rng.uniform(-1.5, 1.5)) for _ in range(64)]
        rig.set_return([f32(rng.uniform(-1.0, 1.0)) for _ in range(64)])
        rig.set_amounts(0, [rng.randrange(0, 256) for _ in range(TRACKS)])
        b = rig.send_block(master)
        assert b.returned, b.halt
        assert b.ctx_bad == [] and b.regs_bad == [], (n, b.ctx_bad, b.regs_bad)
        assert b.master == tuple(master)


def test_the_send_hook_stores_only_in_its_own_memory():
    """The stores of a send block: the adapter's scratch, the send bus, gains and scalars, the effect's output and state, the return bus. Nothing else."""
    rig = FxRig([send_row()], record_stores=True)
    load(rig)
    rig.send_block(ctx="friendly")
    rig.sess.record_stores()
    rig.send_block(ctx="friendly")
    d = rig.built.data
    allowed = [
        (d[n], size)
        for n, size in (
            ("_fx_save", 128),
            ("_fx_s_r0", 4),
            ("_fx_s_i4", 4),
            ("_fx_s_l4", 4),
            ("_fx_ctx", 48),
            ("_fx_blk", 4),
            ("_fx_in", 12),
            ("_fx_scratch", 256),
            ("_fx_c", 16),
            ("_fx_diag", 32),
            ("_fxs_bus", 260),
            ("_fxs_gnew", 64),
            ("_fxs_prev", 256),
            ("_fxs_c", 16),
            ("_te_state_a", 16),
        )
    ]
    from support import fx_rig

    allowed.append((fx_rig.RETURN, 256))
    allowed.append((fx_rig.TABLE, 5 * 96))  # the row's was word
    bad = []
    for e in rig.sess.stores:
        at = (
            e.address - pk_sharc.SW_ALIAS_BASE
            if e.address >= pk_sharc.SW_ALIAS_BASE
            else e.address
        )
        if at < 0x200000:
            continue
        if not any(a <= at and at + e.width <= a + n for a, n in allowed):
            bad.append((hex(at), e.width, hex(e.pc_sw)))
    assert not bad, bad[:4]


def test_the_cost_of_the_send_route_in_instructions():
    def steady(amounts, mix=ONE):
        rig = FxRig([send_row(amounts=amounts, mix=mix)])
        load(rig)
        rig.send_block(ctx="friendly")
        rig.send_block(ctx="friendly")
        return rig.send_block(ctx="friendly").instructions

    none = steady((0,) * TRACKS)
    one = steady(tuple(100 if t == 3 else 0 for t in range(TRACKS)))
    all16 = steady((100,) * TRACKS)
    half = steady((100,) * TRACKS, f2w(0.5))
    print(
        f"one send effect (the half test effect, about 210): steady, all amounts 0 {none}, one track {one}, 16 tracks {all16}; 16 tracks mix 0.5 {half} instructions per block"
    )
    assert none < 1400 and one - none < 170 and all16 < 3800
    assert half == all16


# ---- patchkit: the hook on the Digitakt II -------------------------------------------------------------------------------
DT2_117 = config.stock("Digitakt_II_OS1.17.syx")
needs_image = pytest.mark.skipif(
    not DT2_117.exists(), reason="stock firmware image not present"
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


@needs_image
def test_the_send_hook_takes_the_first_instruction_of_the_master_function(
    patchkit, tmp_path: Path
):
    out = tmp_path / "fx.syx"
    plan_path = tmp_path / "fx.plan.json"
    patchkit(
        "apply",
        "--firmware",
        DT2_117,
        "--modules",
        "core/fx",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    (hook,) = [h for h in plan["hooks"] if h["anchor"] == "dsp.fx.fn.send"]
    assert (
        hook["cpu"] == "sharc" and hook["displaced"] == "run" and hook["site_len"] == 6
    )
    assert hook["tail"] is not None
    assert (
        hook["ret"] == 0x1C207E
    )  # the second instruction of FUN_1c207b (the first is 3 parcels)
    assert any(k.endswith(":_fxs_entry") for k in plan["symbols"])


@needs_image
def test_the_anchors_of_the_send_route_hold_the_track_bus_and_the_return_bus():
    port = (ROOT / "devices/ports/dt2-1.17.toml").read_text()
    for name, value in (
        ("dsp.fx.value.send.track_bus", 0x252DF8),
        ("dsp.fx.value.send.return_bus", 0x254778),
    ):
        i = port.index(f'name = "{name}"')
        block = port[i : i + 400]
        assert f"addr = {value:#x}" in block, name
