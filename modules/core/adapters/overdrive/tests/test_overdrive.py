"""core/overdrive on the concrete SHARC+ core: _od_sat called the way the stock chain driver calls the overdrive (C frame,
record, buffers, R14 = 32 - voice), against a float reference of the curves; _od_split on a frame copy."""

import math
import struct

import numpy as np
import pytest

from tools.patchkit import pk_sharc

SRC = pk_sharc.ROOT / "modules/core/adapters/overdrive/src/dsp/overdrive.s"
BASE = 0xB90000
STOCK, TAIL, RET = (
    0xB9F000,
    0xB9F010,
    0xB9F100,
)  # stand-ins: the stock routine, the hook's tail, the caller
FRAME = 0x200C0000  # the frame copy's stand-in: byte addresses, as the DSP addresses its frame
REC, IN, OUT, FP = (
    0x200B0000,
    0x200B0100,
    0x200B0200,
    0x200B1000,
)  # byte addresses (L2), as index registers address them
DATA = 0x200A0000  # the object's own data (L2, bytes)
CALLER_I6 = 0x200B2000


def f2u(x: float) -> int:
    return struct.unpack("<I", struct.pack("<f", x))[0]


def u2f(u: int) -> float:
    return struct.unpack("<f", struct.pack("<I", u & 0xFFFFFFFF))[0]


class Rig:
    def __init__(self) -> None:
        obj = pk_sharc.assemble(pk_sharc.expand_includes(SRC.read_text(), [SRC.parent]))
        data, addr = {}, DATA
        self.inits = []
        for sec, (raw, _al, _bss) in obj.data.items():
            data[sec] = addr
            self.inits.append((addr, raw))
            addr += (len(raw) + 3) & ~3
        self.sym = {
            n: data[sec] + off for n, (sec, off, _g) in obj.data_symbols.items()
        }
        linked = pk_sharc.link(
            obj,
            BASE,
            {
                "_fw_frame": FRAME,
                "_fw_sat": STOCK,
                "_od_tail": TAIL,
                "_od_tail2": TAIL,
                **self.sym,
            },
        )
        self.sess = pk_sharc.CallSession([linked], RET, breaks=(STOCK, TAIL))
        for a, raw in self.inits:
            for k in range(len(raw) // 4):
                self.sess.poke(
                    a // 4 + k, int.from_bytes(raw[4 * k : 4 * k + 4], "little")
                )
        self.ctx = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M14": 1}
        for r in ("L3", "L4", "L5", "L6", "L7"):
            self.ctx[r] = 0

    def flavour(self, track: int, f: int) -> None:
        self.sess.poke(self.sym["_od_flav"] // 4 + track, f)

    def sat(
        self,
        voice: int,
        x: np.ndarray,
        rec: dict[int, float],
        n: int = 32,
        stock: bool = False,
    ) -> tuple[dict, np.ndarray, list[float]]:
        w = lambda a, k: a // 4 + k  # noqa: E731
        for k in range(24):
            self.sess.poke(w(REC, k), f2u(rec.get(k, 0.0)))
        for k, v in enumerate(x):
            self.sess.poke(w(IN, k), f2u(float(v)))
        self.sess.poke(w(FP, 1), n)
        self.sess.poke(w(FP, 0), CALLER_I6)
        self.sess.poke(w(FP, -1), RET - 1)
        regs = {
            **self.ctx,
            "R4": REC,
            "R8": IN,
            "R12": OUT,
            "R14": 32 - voice,
            "I6": FP,
            "I7": FP - 8,
        }
        for r in ("R3", "R5", "R7", "R9", "R10", "R11", "R13", "R15", "I3", "I5"):
            regs[r] = 0x1000 + len(regs)
        keep = {
            r: v
            for r, v in regs.items()
            if r[0] in "RI" and r not in ("R0", "R1", "R2", "R4", "R8", "R12", "I4")
        }
        if not stock:
            keep["I6"], keep["I7"] = (
                CALLER_I6,
                FP,
            )  # the return's RFRAME: the caller's frame
        res = self.sess.call("_od_sat", regs, read_regs=tuple(keep))
        out = np.array(
            [u2f(self.sess.peek(w(OUT, k)) or 0) for k in range(32)], dtype=np.float32
        )
        recw = [u2f(self.sess.peek(w(REC, k)) or 0) for k in range(14)]
        res["changed"] = [r for r, v in keep.items() if res["regs"].get(r) != v]
        return res, out, recw


def shape(f: int, z: float) -> float:
    if f == 0:
        y = min(max(z / 8, -1.0), 1.0)
        for _ in range(3):
            y *= 2 - abs(y)
        return y
    if f == 1:
        return math.tanh(4 * z)
    if f == 2:
        return min(max(z, -1.0), 1.0)
    if f == 3:
        return z / (1 + abs(z))
    if f == 4:
        return math.tanh(z + 0.4) - math.tanh(0.4)
    if f == 5:
        t = (z + 1) * 0.25 + 64
        return 1 - 4 * abs(t - math.floor(t) - 0.5)
    if f == 6:
        return min(max(4 * z + 0.3, -0.6), 1.0) - 0.3
    return min(max(64 * z, -1.0), 1.0)


def reference(
    f: int, x: np.ndarray, rec: dict[int, float]
) -> tuple[np.ndarray, dict[int, float]]:
    dt = min(max(rec.get(0, 0.0), rec.get(1, 0.0)), 1.0)
    dt *= 1 + dt * dt / 2
    mt = min(max(rec.get(2, 0.0), 0.0), 1.0)
    d, m = rec.get(5, 0.0), rec.get(12, 0.0)
    ds, ms = (dt - d) / 32, (mt - m) / 32
    out = []
    for v in x:
        y = shape(f, 8 * d * v)
        out.append(v + m * (y - v))
        d += ds
        m += ms
    return np.array(out), {
        4: rec.get(5, 0.0),
        5: dt,
        6: ds,
        11: rec.get(12, 0.0),
        12: mt,
        13: ms,
    }


X = np.sin(np.arange(32) * 2 * math.pi / 32 * 3) * 0.6


@pytest.mark.parametrize(
    "f", range(8), ids=["stk", "tnh4", "hard", "soft", "tube", "fold", "fuzz", "sqr"]
)
def test_each_flavour_is_its_curve_with_the_stock_ramps(f):
    rig = Rig()
    rig.flavour(5, f)
    rec = {0: 0.3, 1: 0.1, 2: 0.8, 5: 0.15, 12: 0.4}
    res, out, recw = rig.sat(11, X, rec)  # voice 11: track 5
    assert res["halt"]["pc_sw"] == RET
    assert not res["changed"], res["changed"]
    want, wrec = reference(f, X, rec)
    np.testing.assert_allclose(out, want, atol=2e-4)
    for k, v in wrec.items():
        assert abs(recw[k] - v) < 1e-6, (k, recw[k], v)


def test_odd_blocks_continue_in_the_stock_routine():
    rig = Rig()
    rig.flavour(5, 3)
    res, _, _ = rig.sat(
        10, X, {0: 0.5, 2: 1.0}, n=16, stock=True
    )  # track 5, but not a 32-sample block
    assert res["halt"]["pc_sw"] == STOCK and not res["changed"]


def test_full_drive_is_one_and_a_half_times_the_stock_drive():
    rig = Rig()
    _, _, recw = rig.sat(0, X, {0: 1.0, 2: 1.0, 5: 1.5, 12: 1.0})
    assert recw[5] == 1.5
    _, _, recw = rig.sat(0, X, {0: 0.5, 2: 1.0})
    assert abs(recw[5] - 0.5625) < 1e-7


def test_the_tanh_approximant_is_close():
    for a in np.linspace(-6, 6, 241):
        aa = min(max(a, -4.97), 4.97)
        a2 = aa * aa
        num = aa * (135135 + a2 * (17325 + a2 * (378 + a2)))
        den = 135135 + a2 * (62370 + a2 * (3150 + 28 * a2))
        assert abs(num / den - math.tanh(a)) < 2e-4


def test_the_split_keeps_the_routing_bit_and_the_flavours():
    rig = Rig()
    vals = [(t * 0x100) & 0xF00 for t in range(16)]
    word = lambda t: (FRAME + 0x138 + 0x60 * t) // 4  # noqa: E731
    for t, v in enumerate(vals):
        rig.sess.poke(word(t), 0x4000_0000 | v)
    res = rig.sess.call(
        "_od_split2", {**rig.ctx, "R0": 0x1234, "R1": 0x5678}, read_regs=()
    )
    assert res["halt"]["pc_sw"] == TAIL
    for t, v in enumerate(vals):
        w = rig.sess.peek(word(t))
        assert w == 0x4000_0000 | (v & 0x100), (t, hex(w))
        assert rig.sess.peek(rig.sym["_od_flav"] // 4 + t) == v >> 9
    assert res["regs"]["R0"] == 0x1234 and res["regs"]["R1"] == 0x5678
