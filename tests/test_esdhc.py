"""Synthetic eSDHC/eDMA data-path coverage; no firmware input required."""

import struct
import unittest

from emu.edma import (
    BITER,
    CITER,
    CSR,
    DADDR,
    NBYTES,
    SADDR,
    SOFF,
    TCD_BASE,
)
from emu.esdhc import BASE, CMDARG, DPSEL, DTDSEL, Esdhc, Card


class FakeUc:
    def __init__(self):
        self.memory = {}

    def hook_add(self, *args, **kwargs):
        return 1

    def mem_read(self, addr, size):
        return bytes(self.memory.get(addr + offset, 0) for offset in range(size))

    def mem_write(self, addr, data):
        for offset, value in enumerate(data):
            self.memory[addr + offset] = value


class FakeMachine:
    def __init__(self):
        self.uc = FakeUc()

    def ensure(self, addr):
        pass


def put16(uc, addr, value):
    uc.mem_write(addr, struct.pack(">H", value))


def put32(uc, addr, value):
    uc.mem_write(addr, struct.pack(">I", value))


def get32(uc, addr):
    return struct.unpack(">I", uc.mem_read(addr, 4))[0]


class EsdhcBulkReadTest(unittest.TestCase):
    def test_cmd18_moves_backing_sector_and_posts_both_data_completions(self):
        machine = FakeMachine()
        image = bytes((i & 0xFF) for i in range(1024))
        cmd_sem, data_sem, dma_sem = 0x50001000, 0x50001010, 0x50001020
        drv_status, destination = 0x50001030, 0x50002000
        model = Esdhc(
            machine,
            card=Card(image=image),
            drv_status=drv_status,
            cmd_sem=cmd_sem,
            data_sem=data_sem,
            dma_sem=dma_sem,
        )

        channel = 59
        tcd = TCD_BASE + channel * 0x20
        put32(machine.uc, tcd + SADDR, BASE + 0x20)
        put32(machine.uc, tcd + NBYTES, 4)
        put32(machine.uc, tcd + DADDR, destination)
        put16(machine.uc, tcd + CITER, 128)
        put16(machine.uc, tcd + BITER, 128)
        put16(machine.uc, tcd + CSR, 0)
        put32(machine.uc, BASE + CMDARG, 1)

        model._on_serq(None, None, 0, 1, channel, None)
        model._on_serq(None, None, 0, 1, 35, None)
        self.assertEqual(model.armed, channel)
        model._on_xfertyp(
            None, None, 0, 4, (18 << 24) | DPSEL | DTDSEL, None
        )

        self.assertEqual(machine.uc.mem_read(destination, 512), image[512:])
        self.assertEqual(get32(machine.uc, dma_sem), 1)
        self.assertEqual(get32(machine.uc, data_sem), 1)
        self.assertEqual(get32(machine.uc, cmd_sem), 1)
        self.assertEqual(get32(machine.uc, drv_status), 0)
        self.assertEqual(get32(machine.uc, tcd + DADDR), destination + 512)
        csr = struct.unpack(">H", machine.uc.mem_read(tcd + CSR, 2))[0]
        self.assertEqual(csr & 0x80, 0x80)

    def test_cmd25_consumes_host_buffer_and_is_visible_to_cmd18(self):
        machine = FakeMachine()
        data_sem, dma_sem = 0x50001010, 0x50001020
        source = 0x50002000
        payload = bytes((255 - i) & 0xFF for i in range(512))
        model = Esdhc(machine, data_sem=data_sem, dma_sem=dma_sem)

        channel = 59
        tcd = TCD_BASE + channel * 0x20
        machine.uc.mem_write(source, payload)
        put32(machine.uc, tcd + SADDR, source)
        put16(machine.uc, tcd + SOFF, 16)
        put32(machine.uc, tcd + NBYTES, 16)
        put32(machine.uc, tcd + DADDR, BASE + 0x20)
        put16(machine.uc, tcd + CITER, 32)
        put16(machine.uc, tcd + BITER, 32)
        put16(machine.uc, tcd + CSR, 0)
        put32(machine.uc, BASE + CMDARG, 7)

        model._on_serq(None, None, 0, 1, channel, None)
        model._on_xfertyp(None, None, 0, 4, (25 << 24) | DPSEL, None)

        self.assertEqual(model.card.data_for(18, 7, 512), payload)
        self.assertEqual(get32(machine.uc, dma_sem), 1)
        self.assertEqual(get32(machine.uc, data_sem), 1)

        restored = Esdhc(FakeMachine())
        restored.restore_checkpoint_state(model.checkpoint_state())
        self.assertEqual(restored.card.data_for(18, 7, 512), payload)


if __name__ == "__main__":
    unittest.main()
