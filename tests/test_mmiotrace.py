"""emu/mmiotrace.py: record a tiny synthetic machine and read it back.

No firmware: a few ColdFire instructions touch a PIT register, a stand-in
model answers a read and writes RAM, and the host raises an interrupt.
"""

import struct

import pytest
from unicorn import UC_HOOK_MEM_READ
from unicorn.m68k_const import (
    UC_M68K_REG_A7,
    UC_M68K_REG_D0,
    UC_M68K_REG_PC,
    UC_M68K_REG_SR,
)

from emu import mmiotrace
from emu.harness import VBR, Machine

CODE = 0x40100000
RAM = 0x40200000
PIT0 = 0xFC080000
HANDLER = 0x40100100


def program():
    return (
        b"\x23\xfc"
        + struct.pack(">II", 0x12345678, PIT0)  # move.l #..,PIT0
        + b"\x20\x39"
        + struct.pack(">I", PIT0 + 4)  # move.l PIT0+4,d0
        + b"\x4e\x71" * 4
    )


def machine(recorder=None):
    m = Machine()
    if recorder is not None:
        recorder.attach(m)
    m.ensure(VBR)
    m.ensure(CODE)
    m.ensure(RAM)
    m.ensure(PIT0)
    m.uc.mem_write(CODE, program())
    m.uc.mem_write(HANDLER, b"\x4e\x71" * 4)
    m.uc.mem_write(VBR + 64 * 4, struct.pack(">I", HANDLER))
    m.uc.reg_write(UC_M68K_REG_SR, 0x2700)  # supervisor, as the firmware runs
    m.uc.reg_write(UC_M68K_REG_A7, RAM + 0x1000)

    def model(uc, access, addr, size, value, data):
        # A peripheral answering a read and doing DMA into RAM.
        uc.mem_write(PIT0 + 4, b"\xca\xfe\xba\xbe")
        uc.mem_write(RAM + 0x100, b"dma!")

    m.uc.hook_add(UC_HOOK_MEM_READ, model, begin=PIT0 + 4, end=PIT0 + 7)
    return m


def run(m, clock):
    m.uc.emu_start(CODE, 0, count=2)
    clock["now"] = 2
    m.raise_vector(64, level=3)
    clock["now"] = 3


def test_round_trip(tmp_path):
    path = str(tmp_path / "t.mmio")
    rec = mmiotrace.Recorder(path, state_every=0)
    m = machine(rec)
    clock = {"now": 0}
    rec.start({"label": "unit"}, clock=lambda: clock["now"], rate=lambda: 1000)
    run(m, clock)
    rec.mark({"event": "done"})
    summary = rec.stop()
    assert summary["errors"] == 0, summary["first_error"]
    # The wrappers are gone: the instance falls back to the class methods.
    assert "mem_write" not in vars(m.uc) and "raise_vector" not in vars(m)

    reader = mmiotrace.Reader(path)
    assert reader.header["label"] == "unit"
    assert reader.header["clock_resolution"] == "step"
    recs = list(reader)
    names = [r.name for r in recs]
    assert names[0] == "TIME" and names[-1] == "END"
    assert "RATE" in names and "STEP" in names

    wr = [r for r in recs if r.tag == mmiotrace.WR]
    assert [(r.fields[0], r.fields[1], r.fields[2], r.fields[3]) for r in wr] == [
        (PIT0, 0x12345678, CODE, 4)
    ]
    rd = [r for r in recs if r.tag == mmiotrace.RD]
    # The value the guest received, after the model's read hook.
    assert [(r.fields[0], r.fields[1], r.fields[2]) for r in rd] == [
        (PIT0 + 4, 0xCAFEBABE, CODE + 10)
    ]
    assert m.uc.reg_read(UC_M68K_REG_D0) == 0xCAFEBABE

    hwr = [r for r in recs if r.tag == mmiotrace.HWR]
    got = [(reader.sources[r.fields[0]], r.fields[1], r.data) for r in hwr]
    assert ("%s.machine.<locals>.model" % __name__, RAM + 0x100, b"dma!") in got
    # The model's answer precedes the read it answers.
    order = [r.tag for r in recs if r.tag in (mmiotrace.HWR, mmiotrace.RD)]
    assert order.index(mmiotrace.HWR) < order.index(mmiotrace.RD)

    irq = [r for r in recs if r.tag == mmiotrace.IRQ]
    assert len(irq) == 1
    vec, level, flags, _sid, pc0, pc1, sr = irq[0].fields
    assert (vec, level, flags, pc1) == (64, 3, mmiotrace.IRQ_TAKEN, HANDLER)
    assert pc0 == CODE + 16
    assert sr & 0xFF00 == 0x2700  # the interrupted SR, from the pushed frame
    assert irq[0].clock == 2
    marks = [json for _c, json in reader.blobs(mmiotrace.MARK)]
    assert marks == [{"event": "done"}]


def test_recording_changes_nothing(tmp_path):
    def final(m):
        regs = [
            m.uc.reg_read(r) for r in (UC_M68K_REG_D0, UC_M68K_REG_A7, UC_M68K_REG_PC)
        ]
        mem = bytes(m.uc.mem_read(RAM, 0x2000)) + bytes(m.uc.mem_read(PIT0, 0x10))
        return regs, mem

    clock = {"now": 0}
    plain = machine()
    run(plain, clock)
    rec = mmiotrace.Recorder(str(tmp_path / "t.mmio"), state_every=0)
    recorded = machine(rec)
    rec.start({}, clock=lambda: clock["now"])
    run(recorded, clock)
    rec.stop()
    assert final(plain) == final(recorded)


def test_icount_clock(tmp_path):
    path = str(tmp_path / "t.mmio")
    rec = mmiotrace.Recorder(path, icount=True, state_every=0)
    m = machine(rec)
    rec.start({}, clock=lambda: 100)
    m.uc.emu_start(CODE, 0, count=2)
    rec.stop()
    recs = list(mmiotrace.Reader(path))
    wr = next(r for r in recs if r.tag == mmiotrace.WR)
    rd = next(r for r in recs if r.tag == mmiotrace.RD)
    assert (wr.clock, rd.clock) == (100, 101)


def copy_halfwords(m, base, data):
    # A stand-in model copying a buffer one halfword at a time.
    for i in range(0, len(data), 2):
        m.uc.mem_write(base + i, data[i : i + 2])


def test_contiguous_host_writes_merge(tmp_path):
    path = str(tmp_path / "t.mmio")
    rec = mmiotrace.Recorder(path, state_every=0)
    m = machine(rec)
    rec.start({}, clock=lambda: 0)
    copy_halfwords(m, RAM + 0x200, bytes(range(16)))
    copy_halfwords(m, RAM + 0x400, b"ab")  # not contiguous: a new record
    rec.stop()
    hwr = [r for r in mmiotrace.Reader(path) if r.tag == mmiotrace.HWR]
    assert [(r.fields[1], r.data) for r in hwr] == [
        (RAM + 0x200, bytes(range(16))),
        (RAM + 0x400, b"ab"),
    ]


def test_peripheral_map():
    assert mmiotrace.peripheral_of(0xFC08C004) == ("PIT3", "timers-intc")
    assert mmiotrace.peripheral_of(0xFC045000 + 35 * 0x20)[0].startswith("eDMA TCD35")
    assert mmiotrace.modelled_by(0xFC0CC00C) is not None
    assert mmiotrace.modelled_by(0xFC0C0000) is None


def test_rejects_other_files(tmp_path):
    p = tmp_path / "x"
    p.write_bytes(b"not a trace at all")
    with pytest.raises((ValueError, struct.error)):
        mmiotrace.Reader(str(p))
