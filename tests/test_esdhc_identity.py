"""Opt-in regression: the emulated eMMC's CID/EXT_CSD pass DT2 1.16's own
eMMC-identity whitelist check.

`docs/findings/14-plus-drive-format.md` traces why `FUN_4015a450` (the
+Drive mount) was never called on any card-image boot: `FUN_400cc864` gates
that call behind an eMMC-identity check (`FUN_4012dc80` -> `FUN_4012db90`
[build a struct from the CID/EXT_CSD RAM state] -> `FUN_4012dbe0` -> two
calls into `FUN_4012da2c`, a linear scan of a 7-entry manufacturer/
product-name whitelist at `0x402b4a24`). `emu.esdhc.Card`'s CID and EXT_CSD
now encode manufacturer `0x11`/product name `"004GE0"`, one of the table's
valid entries, plus that entry's own capacity/attribute fields.

This test does not build a full `emu.harness.Machine` or restore a
snapshot: `FUN_4012dc80` and everything it calls are pure computation (no
MMIO, no DMA -- they read/write ordinary RAM the caller has already
populated), so a minimal Unicorn instance with just the MAIN_OS image
mapped at its real load address, plus the handful of RAM pages this call
touches, is enough -- the same pattern `tests/test_longrun_real.py` uses
for a single guest helper. Needs the real (uncommitted) firmware image, so
it's opt-in like that file, not part of the default `pytest` run.
"""

import os
import struct
import unittest

import emu.esdhc as esdhc

_MAIN_IMG_BASE = 0x40000400
FUN_4012dc80 = 0x4012DC80


def _dat_44e3fea0(card):
    """Reproduces FUN_4012d4b2's own capacity derivation, in order: EXT_CSD's
    SEC_COUNT (a plain native big-endian 32-bit field at offset 0xD4) if
    nonzero, else the CSD-1.0 C_SIZE/C_SIZE_MULT/READ_BL_LEN fallback formula
    (C_SIZE from csd[2]'s low 2 bits + csd[1]'s top 10 bits; C_SIZE_MULT and
    READ_BL_LEN from csd[1]/csd[2] respectively -- the same bit-shift
    sequence emu/esdhc.py's CSD_RSP1/CSD_RSP2 comment cites). See that
    module's `_CAPACITY_PARAMS` comment for why only SEC_COUNT can reach the
    eMMC-identity whitelist's larger capacity constant without corrupting the
    result via a real firmware arithmetic-shift overflow."""
    sec_count = struct.unpack_from(">I", card.ext_csd, 0xD4)[0]
    if sec_count:
        return sec_count
    rsp1, rsp2 = card.csd[1], card.csd[2]
    c_size = ((rsp2 & 3) << 10) | (rsp1 >> 22)
    c_size_mult = (rsp1 >> 7) & 7
    read_bl_len = (rsp2 >> 8) & 0xF
    return ((c_size + 1) << (c_size_mult + 2) << read_bl_len) >> 9


@unittest.skipUnless(
    os.environ.get("DT2_SYX"), "set DT2_SYX=Digitakt_II_OS1.16.syx to run this"
)
class EmmcIdentityWhitelistTest(unittest.TestCase):
    def _call_fun_4012dc80(self):
        """Runs the real FUN_4012dc80 with a fresh emu.harness.Machine, RAM
        pre-populated exactly as a completed CID (ALL_SEND_CID/SEND_CID) and
        EXT_CSD (SEND_EXT_CSD) exchange with our modeled `Card` would leave
        it, and returns D0 (0 = the whitelist accepted the identity).

        Uses `emu.harness.Machine`/`call` (not a hand-built Unicorn instance)
        because its UC_HOOK_MEM_INVALID handler auto-maps a zero page on any
        unmapped access -- `FUN_4012dc80`'s own callees touch a couple of
        stack/RAM addresses beyond the handful this test explicitly seeds,
        and a bare Unicorn instance faults on those instead of reading them
        as zero.
        """
        from emu import config, harness

        with open(config.main_image(), "rb") as fh:
            image = fh.read()

        m = harness.Machine()
        page = harness.PAGE
        end = _MAIN_IMG_BASE + len(image)
        for base in range(_MAIN_IMG_BASE & ~(page - 1), end, page):
            m.ensure(base)
        m.uc.mem_write(_MAIN_IMG_BASE, image)

        # _DAT_44e3fe90.._DAT_44e3fe9c: CMDRSP0-3 of a completed
        # ALL_SEND_CID/SEND_CID, copied here by FUN_4012d4b2 (confirmed by
        # disassembly, not the decompiler's mistaken byte-vs-word read of
        # the manufacturer field -- see emu/esdhc.py's Card docstring).
        m.ensure(0x44E3FE90)
        card = esdhc.Card()
        for i, word in enumerate(card.cid):
            m.uc.mem_write(0x44E3FE90 + i * 4, struct.pack(">I", word))

        # The second EXT_CSD-shaped buffer FUN_4012d4b2 DMAs a
        # SEND_EXT_CSD response into for this same check (0x4FE69100; not
        # emu.esdhc.EXTCSD_BUF, a different destination for the same
        # underlying Card.ext_csd bytes in this model).
        extcsd2_base = 0x4FE69100
        m.ensure(extcsd2_base)
        m.uc.mem_write(extcsd2_base, card.ext_csd)

        # _DAT_44e3fe7c/_DAT_44e3fea0: normally derived from the second
        # EXT_CSD's SEC_COUNT a few instructions before FUN_4012dc80 is even
        # called (still inside FUN_4012d4b2, not FUN_4012dc80 itself, so
        # calling FUN_4012dc80 directly needs this derivation's *result*
        # pre-seeded). Reproduced here from `card.csd` via the same formula
        # `emu/esdhc.py`'s module docstring cites, rather than a bare
        # literal, so this test tracks that module if its CSD constants
        # ever change instead of silently drifting from them.
        m.uc.mem_write(0x44E3FE7C, struct.pack(">I", 1))
        m.uc.mem_write(0x44E3FEA0, struct.pack(">I", _dat_44e3fea0(card)))

        return harness.call(m, FUN_4012dc80, [], limit=2_000_000)

    def test_identity_check_accepts_the_modeled_card(self):
        d0 = self._call_fun_4012dc80()
        self.assertEqual(
            d0,
            0,
            "FUN_4012dc80 rejected the modeled CID/EXT_CSD (D0=0x%08x); "
            "FUN_400cc864 would then never call FUN_4015a450" % d0,
        )
