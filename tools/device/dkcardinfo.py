#!/usr/bin/env python3
"""`dkcardinfo`: read the eMMC card's EXT_CSD copy and the controller registers from the read-only module `test/card-info`, and print named fields.

    uv run --with python-rtmidi python -m tools.device.dkcardinfo --port Digitakt [--raw]
    uv run python -m tools.device.dkcardinfo --emu [--run-dir DIR] [--raw]

The module (tests/fixtures/modules/card-info) must be loaded: pack it, put it in the store, select it, restart. It answers two commands of the
`file_write_test_b` range (0x28 EXT_CSD, 0x29 REGS) and writes nothing. EXT_CSD is the firmware's own copy (read once at its init with CMD8); the register list is
fixed in the module. `--raw` prints everything as JSON (the EXT_CSD as hex, the registers by address).

Field names and sizes of the EXT_CSD are the eMMC 5.1 layout as the author knows it (no copy of the standard is in the repository): a name marked `?` is
from memory and unconfirmed; a field the card's revision does not have reads as zero. The register fields are from the MCF5441x reference manual.
The SD clock needs the PLL reference clock: `--fref` (MHz, default 24; the bus of 132 MHz on a DT2/DN2 unit gives 24 MHz with FBKDIV 21: docs/findings/07-emulator.md).
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path
from typing import Any

CMD_EXTCSD = 0x28
CMD_REGS = 0x29
EXTCSD_LEN = 512

# ---- the EXT_CSD --------------------------------------------------------------------------------------------------------------

REV = {
    0: "4.0",
    1: "4.1",
    2: "4.2",
    3: "4.3",
    4: "4.4 (obsolete)",
    5: "4.41",
    6: "4.5",
    7: "5.0",
    8: "5.1",
}
DEVICE_TYPE_BITS = (
    "HS 26 MHz",
    "HS 52 MHz",
    "HS DDR 52 MHz 1.8/3 V",
    "HS DDR 52 MHz 1.2 V",
    "HS200 1.8 V",
    "HS200 1.2 V",
    "HS400 1.8 V",
    "HS400 1.2 V",
)
HS_TIMING = {0: "backward compatible", 1: "high speed", 2: "HS200", 3: "HS400"}
BUS_WIDTH = {
    0: "1 bit",
    1: "4 bit",
    2: "8 bit",
    5: "4 bit DDR",
    6: "8 bit DDR",
    0x82: "8 bit strobe",
    0x86: "8 bit DDR strobe",
}
LIFE = {0: "not defined", 11: "exceeded"} | {
    i: f"{(i - 1) * 10}-{i * 10} % used" for i in range(1, 11)
}
PRE_EOL = {0: "not defined", 1: "normal", 2: "warning (80 % reserve used)", 3: "urgent"}


def u(b: bytes, lo: int, hi: int | None = None) -> int:
    """Little-endian value of EXT_CSD[hi..lo] (hi omitted: one byte)."""
    hi = lo if hi is None else hi
    return int.from_bytes(b[lo : hi + 1], "little")


def bits(v: int, names: tuple[str, ...]) -> str:
    return ", ".join(n for i, n in enumerate(names) if v >> i & 1) or "none"


def extcsd_fields(b: bytes) -> list[tuple[str, str, str]]:
    """(index text, name, value text); a name ending in `?` is not confirmed against the standard."""
    if len(b) != EXTCSD_LEN:
        raise ValueError(f"EXT_CSD is {len(b)} bytes, not {EXTCSD_LEN}")
    f: list[tuple[str, str, str]] = []

    def add(idx: str, name: str, val: str) -> None:
        f.append((idx, name, val))

    rev = b[192]
    add("[192]", "EXT_CSD_REV", f"{rev} (eMMC {REV.get(rev, 'newer than 5.1')})")
    add("[196]", "DEVICE_TYPE", f"{b[196]:#04x}: {bits(b[196], DEVICE_TYPE_BITS)}")
    add(
        "[185]",
        "HS_TIMING",
        f"{b[185]:#04x}: {HS_TIMING.get(b[185] & 15, '?')}, driver strength {b[185] >> 4}",
    )
    add("[183]", "BUS_WIDTH", f"{b[183]:#04x}: {BUS_WIDTH.get(b[183], 'unknown')}")
    add("[184]", "STROBE_SUPPORT ?", f"{b[184]}")
    add("[187]", "POWER_CLASS", f"{b[187]:#04x}")
    add("[197]", "DRIVER_STRENGTH ?", f"{b[197]:#04x}")
    sec = u(b, 212, 215)
    add("[212..215]", "SEC_COUNT", f"{sec} sectors = {sec * 512 / 2**30:.2f} GiB")
    add("[222]", "REL_WR_SEC_C", f"{b[222]} sector(s) of 512 B in one reliable write")
    wp = b[166]
    add(
        "[166]",
        "WR_REL_PARAM",
        f"{wp:#04x}: HS_CTRL_REL {wp & 1}, EN_REL_WR {wp >> 2 & 1}, EN_RPMB_REL_WR {wp >> 4 & 1} (bit names ?)",
    )
    ws = b[167]
    add(
        "[167]",
        "WR_REL_SET",
        f"{ws:#04x}: reliable write on user area {ws & 1}, GP1..GP4 {ws >> 1 & 1}{ws >> 2 & 1}{ws >> 3 & 1}{ws >> 4 & 1}",
    )
    cache = u(b, 249, 252)
    add("[249..252]", "CACHE_SIZE", f"{cache} KiB" + ("" if cache else " (no cache)"))
    add("[33]", "CACHE_CTRL", f"{b[33]:#04x}: cache {'ON' if b[33] & 1 else 'OFF'}")
    add(
        "[240]",
        "CACHE_FLUSH_POLICY ?",
        f"{b[240]:#04x}: {'FIFO' if b[240] & 1 else 'device defined'}",
    )
    add("[32]", "FLUSH_CACHE", f"{b[32]:#04x}")
    add("[31]", "BARRIER_CTRL ?", f"{b[31]:#04x}")
    add("[486]", "BARRIER_SUPPORT ?", f"{b[486]:#04x}")
    add(
        "[502]",
        "BKOPS_SUPPORT",
        f"{b[502]:#04x}: {'supported' if b[502] & 1 else 'not supported'}",
    )
    add(
        "[163]",
        "BKOPS_EN",
        f"{b[163]:#04x}: manual {b[163] & 1}, auto {b[163] >> 1 & 1}",
    )
    add(
        "[246]",
        "BKOPS_STATUS",
        f"{b[246]} (0 none, 1 non-critical, 2 performance impacted, 3 critical)",
    )
    for idx, name in (
        (266, "OPTIMAL_READ_SIZE ?"),
        (265, "OPTIMAL_WRITE_SIZE ?"),
        (264, "OPTIMAL_TRIM_UNIT_SIZE ?"),
    ):
        add(f"[{idx}]", name, f"{b[idx]} (x 4 KiB = {b[idx] * 4} KiB; 0: not reported)")
    add("[224]", "HC_ERASE_GRP_SIZE", f"{b[224]} (erase unit {b[224] * 512} KiB)")
    add("[221]", "HC_WP_GRP_SIZE", f"{b[221]}")
    add(
        "[175]",
        "ERASE_GROUP_DEF",
        f"{b[175]:#04x}: high-capacity erase groups {'ON' if b[175] & 1 else 'off'}",
    )
    add("[223]", "ERASE_TIMEOUT_MULT", f"{b[223]} (x 300 ms)")
    add("[232]", "TRIM_MULT", f"{b[232]} (x 300 ms)")
    add("[231]", "SEC_FEATURE_SUPPORT", f"{b[231]:#04x}")
    add("[225]", "ACC_SIZE", f"{b[225]} (superpage {512 << b[225] if b[225] else 0} B)")
    add(
        "[179]",
        "PARTITION_CONFIG",
        f"{b[179]:#04x}: boot enable {b[179] >> 3 & 7}, access {b[179] & 7}",
    )
    add("[226]", "BOOT_SIZE_MULT", f"{b[226]} (boot partition {b[226] * 128} KiB each)")
    add("[168]", "RPMB_SIZE_MULT", f"{b[168]} (RPMB {b[168] * 128} KiB)")
    add(
        "[160]",
        "PARTITIONING_SUPPORT",
        f"{b[160]:#04x}: partitioning {b[160] & 1}, enhanced attr {b[160] >> 1 & 1}, extended attr {b[160] >> 2 & 1}",
    )
    add("[155]", "PARTITION_SETTING_COMPLETED", f"{b[155]}")
    add(
        "[156]",
        "PARTITIONS_ATTRIBUTE",
        f"{b[156]:#04x}: enhanced attribute per area (bit 0 user, 1..4 GP1..GP4)",
    )
    unit = b[224] * b[221] * 512 * 1024
    mx = u(b, 157, 159)
    add(
        "[157..159]",
        "MAX_ENH_SIZE_MULT",
        f"{mx} (max enhanced area {mx * unit / 2**20:.0f} MiB)",
    )
    enh = u(b, 140, 142)
    add(
        "[140..142]",
        "ENH_SIZE_MULT",
        f"{enh} (enhanced user area {enh * unit / 2**20:.0f} MiB)",
    )
    add("[136..139]", "ENH_START_ADDR", f"{u(b, 136, 139):#x}")
    for i, hi in enumerate((154, 151, 148, 145)):
        gp = u(b, hi - 2, hi)
        add(
            f"[{hi - 2}..{hi}]",
            f"GP_SIZE_MULT_GP{i} ?",
            f"{gp} (GP{i + 1} {gp * unit / 2**20:.0f} MiB)",
        )
    add("[34]", "POWER_OFF_NOTIFICATION", f"{b[34]}")
    add("[247]", "POWER_OFF_LONG_TIME", f"{b[247]} (x 10 ms)")
    add("[248]", "GENERIC_CMD6_TIME", f"{b[248]} (x 10 ms)")
    add("[199]", "PARTITION_SWITCH_TIME", f"{b[199]} (x 10 ms)")
    add("[198]", "OUT_OF_INTERRUPT_TIME", f"{b[198]} (x 10 ms)")
    add("[241]", "INI_TIMEOUT_AP", f"{b[241]} (x 100 ms)")
    add("[210]", "MIN_PERF_W_8_52", f"{b[210]} (x 0.3 MB/s = {b[210] * 0.3:.1f} MB/s)")
    add("[209]", "MIN_PERF_R_8_52", f"{b[209]} (x 0.3 MB/s = {b[209] * 0.3:.1f} MB/s)")
    add(
        "[208]",
        "MIN_PERF_W_8_26_4_52",
        f"{b[208]} (x 0.3 MB/s = {b[208] * 0.3:.1f} MB/s)",
    )
    add(
        "[207]",
        "MIN_PERF_R_8_26_4_52",
        f"{b[207]} (x 0.3 MB/s = {b[207] * 0.3:.1f} MB/s)",
    )
    add("[206]", "MIN_PERF_W_4_26", f"{b[206]} (x 0.3 MB/s = {b[206] * 0.3:.1f} MB/s)")
    add("[205]", "MIN_PERF_R_4_26", f"{b[205]} (x 0.3 MB/s = {b[205] * 0.3:.1f} MB/s)")
    add("[268]", "DEVICE_LIFE_TIME_EST_TYP_A", f"{b[268]}: {LIFE.get(b[268], '?')}")
    add("[269]", "DEVICE_LIFE_TIME_EST_TYP_B", f"{b[269]}: {LIFE.get(b[269], '?')}")
    add("[267]", "PRE_EOL_INFO", f"{b[267]}: {PRE_EOL.get(b[267], '?')}")
    add(
        "[254..261]",
        "FIRMWARE_VERSION ?",
        b[254:262].hex() + f" ({b[254:262].decode('ascii', 'replace')!r})",
    )
    add("[262..263]", "DEVICE_VERSION ?", f"{u(b, 262, 263):#06x}")
    add("[308]", "CMDQ_SUPPORT ?", f"{b[308]:#04x}")
    add("[307]", "CMDQ_DEPTH ?", f"{b[307]:#04x}")
    add("[15]", "CMDQ_MODE_EN ?", f"{b[15]:#04x}")
    add("[494]", "EXT_SUPPORT ?", f"{b[494]:#04x}")
    add("[493]", "SUPPORTED_MODES ?", f"{b[493]:#04x}")
    add("[500..501]", "MAX_PACKED_WRITES / READS ?", f"{b[500]} / {b[501]}")
    add("[36]", "PACKED_COMMAND_STATUS ?", f"{b[36]:#04x}")
    add("[17]", "PRODUCT_STATE_AWARENESS_ENABLEMENT ?", f"{b[17]:#04x}")
    add("[504]", "S_CMD_SET", f"{b[504]:#04x}")
    add("[62..63]", "USE_NATIVE_SECTOR / NATIVE_SECTOR_SIZE ?", f"{b[62]} / {b[63]}")
    return f


# ---- the registers ------------------------------------------------------------------------------------------------------------

ESDHC = 0xFC0CC000
NAMES = {
    ESDHC + 0x24: "ESDHC PRSSTAT",
    ESDHC + 0x28: "ESDHC PROCTL",
    ESDHC + 0x2C: "ESDHC SYSCTL",
    ESDHC + 0x40: "ESDHC HOSTCAPBLT",
    ESDHC + 0x44: "ESDHC WML",
    ESDHC + 0xC0: "ESDHC VENDOR",
    ESDHC + 0xFC: "ESDHC HOSTVER",
    0xFC0C0000: "PLL_CR",
    0xFC0C0004: "PLL_DR",
    0xFC0C0008: "PLL_SR",
    0xFC0B0140: "USB OTG USBCMD",
    0xFC0B0144: "USB OTG USBSTS",
    0xFC0B0160: "USB OTG BURSTSIZE",
    0xFC0B0164: "USB OTG TXFILLTUNING",
    0xFC0B0184: "USB OTG PORTSC1",
    0xFC0B01A8: "USB OTG MODE",
    0xFC004000: "XBS_PRS0",
    0xFC004010: "XBS_CRS0",
    0xFC004200: "XBS_PRS2",
    0xFC004210: "XBS_CRS2",
}
TCD = 0xFC045760
TCD_FIELDS = (
    ("SADDR", 0x00, "L"),
    ("ATTR", 0x04, "H"),
    ("SOFF", 0x06, "h"),
    ("NBYTES", 0x08, "L"),
    ("SLAST", 0x0C, "l"),
    ("DADDR", 0x10, "L"),
    ("CITER", 0x14, "H"),
    ("DOFF", 0x16, "h"),
    ("DLAST_SGA", 0x18, "l"),
    ("BITER", 0x1C, "H"),
    ("CSR", 0x1E, "H"),
)
MASTERS = ("core", "eDMA", "USB OTG", "ENET", "ENET", "eSDHC", "USB host", "NFC")


def parse_regs(body: bytes) -> tuple[int, dict[int, int]]:
    if len(body) < 4 or body[0] != 1:
        raise ValueError(f"REGS reply: format {body[0] if body else None}, expected 1")
    n = body[2]
    if len(body) < 4 + 8 * n:
        raise ValueError(f"REGS reply of {len(body)} bytes for {n} registers")
    regs = {
        struct.unpack_from(">I", body, 4 + 8 * i)[0]: struct.unpack_from(
            ">I", body, 8 + 8 * i
        )[0]
        for i in range(n)
    }
    return body[1], regs


def tcd_bytes(regs: dict[int, int]) -> bytes:
    return b"".join(struct.pack(">I", regs[TCD + 4 * i]) for i in range(8))


def pll(regs: dict[int, int], fref_mhz: float) -> dict[str, Any]:
    cr, dr = regs[0xFC0C0000], regs[0xFC0C0004]
    fbk, refdiv = cr & 0x3F, (cr >> 8) & 7
    fvco = fref_mhz * (fbk + 1) / (2 ** (refdiv if refdiv < 2 else 0))
    div = {
        "core": dr & 31,
        "bus": (dr >> 5) & 31,
        "esdhc": (dr >> 10) & 31,
        "usb": (dr >> 16) & 31,
        "nfc": (dr >> 21) & 31,
    }
    # core and bus: the output is fVCO / (OUTDIV + 1), 0 disables it (the RM's formula for the bus gives fSYS/2 = fVCO / (OUTDIV2 + 1))
    out = {k: (fvco / (v + 1) if v else 0.0) for k, v in div.items()}
    return {"fvco_mhz": fvco, "divider_field": div, "mhz": out}


def esdhc_clock(regs: dict[int, int], fref_mhz: float) -> dict[str, Any]:
    base = pll(regs, fref_mhz)["mhz"]["esdhc"]
    sysctl = regs[ESDHC + 0x2C]
    fs, dvs = (sysctl >> 8) & 0xFF, (sysctl >> 4) & 15
    pre = fs * 2 if fs else 1
    return {
        "base_mhz": base,
        "SDCLKFS": fs,
        "DVS": dvs,
        "sd_clock_mhz": base / (pre * (dvs + 1)) if base else 0.0,
        "SDCLKEN": sysctl >> 3 & 1,
    }


def register_lines(regs: dict[int, int], fref_mhz: float) -> list[str]:
    L: list[str] = []

    def line(name: str, text: str) -> None:
        L.append(f"  {name:<22}{text}")

    p = pll(regs, fref_mhz)
    cr, dr, sr = regs[0xFC0C0000], regs[0xFC0C0004], regs[0xFC0C0008]
    L.append("clocks (PLL):")
    line("PLL_CR", f"{cr:#010x}  FBKDIV {cr & 63}, REFDIV {(cr >> 8) & 7}")
    line(
        "PLL_DR",
        f"{dr:#010x}  OUTDIV1 {dr & 31}, OUTDIV2 {(dr >> 5) & 31}, OUTDIV3 {(dr >> 10) & 31}, OUTDIV4 {(dr >> 16) & 31}, OUTDIV5 {(dr >> 21) & 31}",
    )
    line(
        "PLL_SR",
        f"{sr:#010x}  LOCK {sr >> 4 & 1}, LOCKS {sr >> 5 & 1}, LOLF {sr >> 6 & 1}, LOC {sr >> 8 & 1}",
    )
    m = p["mhz"]
    line("fVCO", f"{p['fvco_mhz']:.1f} MHz (assumes fREF {fref_mhz:g} MHz)")
    line(
        "core / bus",
        f"{m['core']:.1f} / {m['bus']:.1f} MHz   (fVCO/(OUTDIV1+1), fVCO/(OUTDIV2+1))",
    )
    line("eSDHC base clock", f"{m['esdhc']:.2f} MHz   (fVCO/(OUTDIV3+1))")
    line("USB clock", f"{m['usb']:.2f} MHz   (fVCO/(OUTDIV4+1))")

    sysctl, proctl = regs[ESDHC + 0x2C], regs[ESDHC + 0x28]
    prs, cap, wml = regs[ESDHC + 0x24], regs[ESDHC + 0x40], regs[ESDHC + 0x44]
    ven, ver = regs[ESDHC + 0xC0], regs[ESDHC + 0xFC]
    c = esdhc_clock(regs, fref_mhz)
    L.append("eSDHC:")
    line(
        "SYSCTL",
        f"{sysctl:#010x}  SDCLKFS {c['SDCLKFS']:#04x}, DVS {c['DVS']}, SDCLKEN {c['SDCLKEN']}, DTOCV {(sysctl >> 16) & 15}",
    )
    line(
        "SD card clock",
        f"{c['sd_clock_mhz']:.2f} MHz   (base / (SDCLKFS x 2 (1 if 0) x (DVS + 1)))",
    )
    dtw = {0: "1 bit", 1: "4 bit", 2: "8 bit"}.get((proctl >> 1) & 3, "reserved")
    dmas = {0: "simple DMA", 2: "ADMA1", 3: "ADMA2"}.get((proctl >> 8) & 3, "reserved")
    line(
        "PROCTL",
        f"{proctl:#010x}  bus width {dtw}, DMA select {dmas}, endian mode {(proctl >> 4) & 3}, D3CD {proctl >> 3 & 1}",
    )
    line("bus width", dtw)
    line(
        "WML", f"{wml:#010x}  read {wml & 0xFF} words, write {(wml >> 16) & 0xFF} words"
    )
    line(
        "HOSTCAPBLT",
        f"{cap:#010x}  max block {512 << ((cap >> 16) & 7)} B, DMA {cap >> 22 & 1}, high speed {cap >> 21 & 1}, ADMA {cap >> 20 & 1}, 1.8/3.0/3.3 V {cap >> 26 & 1}{cap >> 25 & 1}{cap >> 24 & 1}",
    )
    line("VENDOR", f"{ven:#010x}  EXT_DMA_EN {ven & 1}")
    line("HOSTVER", f"{ver:#010x}  vendor {ver >> 8 & 0xFF}, spec {ver & 0xFF}")
    line(
        "PRSSTAT",
        f"{prs:#010x}  CIHB {prs & 1}, CDIHB {prs >> 1 & 1}, DLA {prs >> 2 & 1}, SDSTB {prs >> 3 & 1}, WTA {prs >> 8 & 1}, RTA {prs >> 9 & 1}, CINS {prs >> 16 & 1}",
    )

    burst, mode = regs[0xFC0B0160], regs[0xFC0B01A8]
    cmd, sts = regs[0xFC0B0140], regs[0xFC0B0144]
    fill, port = regs[0xFC0B0164], regs[0xFC0B0184]
    L.append("USB OTG:")
    line(
        "BURSTSIZE",
        f"{burst:#010x}  TX burst {(burst >> 8) & 0xFF} words, RX burst {burst & 0xFF} words",
    )
    line(
        "MODE",
        f"{mode:#010x}  {({0: 'idle', 2: 'device', 3: 'host'}).get(mode & 3, 'reserved')}, ES {mode >> 2 & 1}, SLOM {mode >> 3 & 1}, SDIS {mode >> 4 & 1}",
    )
    line("USBCMD", f"{cmd:#010x}  RS {cmd & 1}, ITC {(cmd >> 16) & 0xFF}")
    line("USBSTS", f"{sts:#010x}")
    line(
        "TXFILLTUNING",
        f"{fill:#010x}  TXSCHOH {fill & 0xFF}, TXSCHEALTH {(fill >> 8) & 31}, TXFIFOTHRES {(fill >> 16) & 63}",
    )
    line(
        "PORTSC1",
        f"{port:#010x}  CCS {port & 1}, PE {port >> 2 & 1}, speed {({0: 'full', 1: 'low', 2: 'high'}).get((port >> 26) & 3, '?')}",
    )

    L.append("crossbar (priority level 1 = highest; masters by port):")
    for s in (0, 2):
        prs_v, crs_v = regs[0xFC004000 + 0x100 * s], regs[0xFC004010 + 0x100 * s]
        pr = ", ".join(f"{MASTERS[i]} {((prs_v >> (4 * i)) & 7) + 1}" for i in range(8))
        line(f"S{s} PRS", f"{prs_v:#010x}  {pr}")
        line(
            f"S{s} CRS",
            f"{crs_v:#010x}  PARK {crs_v & 7}, PCTL {(crs_v >> 4) & 3}, ARB {'round robin' if crs_v >> 8 & 1 else 'fixed'}, RO {crs_v >> 31}",
        )

    t = tcd_bytes(regs)
    L.append(
        "eDMA channel 59 TCD (the eSDHC read channel; as the last transfer left it):"
    )
    for name, off, fmt in TCD_FIELDS:
        v = struct.unpack_from(">" + fmt, t, off)[0]
        line(name, f"{v & 0xFFFFFFFF:#x}" if v >= 0 else f"{v}")
    return L


def card_order(mem: bytes) -> bytes:
    """The EXT_CSD in the card's byte order. The eSDHC runs in little-endian mode (PROCTL EMODE 2), so the eDMA stores each
    32-bit DATPORT word byte-reversed for the big-endian CPU: card byte 4k+j sits at memory byte 4k+3-j. On the DT2 this gives
    EXT_CSD_REV 8, S_CMD_SET 1 and the right capacity; the memory order gives nonsense. The firmware reads SEC_COUNT as one
    big-endian word at +0xd4, which is the same value."""
    return b"".join(mem[i : i + 4][::-1] for i in range(0, len(mem), 4))


def extcsd_lines(b: bytes) -> list[str]:
    return [f"  {i:<11} {n:<38} {v}" for i, n, v in extcsd_fields(card_order(b))]


# ---- the device ---------------------------------------------------------------------------------------------------------------


def read_extcsd(up: Any) -> tuple[int, int, bytes]:
    st, body = up.request(CMD_EXTCSD)
    if st != 0:
        raise SystemExit(
            f"EXT_CSD: status {st} (is test/card-info loaded? see the docstring)"
        )
    fmt, dev, addr, off, n = struct.unpack_from(">BBIHH", body, 0)
    data = body[10 : 10 + n]
    if fmt != 1 or off != 0 or len(data) != n or n != EXTCSD_LEN:
        raise SystemExit(
            f"EXT_CSD reply: format {fmt}, offset {off}, {len(data)} of {n} bytes"
        )
    return dev, addr, data


def read_regs(up: Any) -> tuple[int, dict[int, int]]:
    st, body = up.request(CMD_REGS)
    if st != 0:
        raise SystemExit(f"REGS: status {st}")
    return parse_regs(body)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument("--emu", action="store_true", help="the running emulator")
    where.add_argument(
        "--port", help="a MIDI port name substring (a real device; python-rtmidi)"
    )
    ap.add_argument(
        "--run-dir", type=Path, help="with --emu: the daemon's run directory"
    )
    ap.add_argument(
        "--raw",
        action="store_true",
        help="print everything as JSON (EXT_CSD hex, registers by address)",
    )
    ap.add_argument(
        "--fref",
        type=float,
        default=24.0,
        help="PLL reference clock in MHz (default 24)",
    )
    args = ap.parse_args(argv)

    from tools.device import dkupload as D

    emu = D._emu(args.run_dir) if args.emu else None
    transport: Any = D.EmuTransport(emu) if args.emu else D.RtMidiTransport(args.port)
    up = D.Uploader(transport, skip_foreign=not args.emu)
    dev, addr, ext = read_extcsd(up)
    dev2, regs = read_regs(up)
    if args.raw:
        print(
            json.dumps(
                {
                    "device_id": dev,
                    "extcsd_address": f"{addr:#010x}",
                    "extcsd_memory_order": ext.hex(),
                    "extcsd": card_order(ext).hex(),
                    "registers": {f"{a:#010x}": f"{v:#010x}" for a, v in regs.items()},
                },
                indent=1,
            )
        )
        return 0
    print(
        f"device id {dev} ({'DT2' if dev == 1 else 'DN2' if dev == 2 else '?'}); EXT_CSD copy at {addr:#010x}"
    )
    print("EXT_CSD:")
    print("\n".join(extcsd_lines(ext)))
    print("registers:")
    print("\n".join(register_lines(regs, args.fref)))
    return 0 if dev == dev2 else 1


if __name__ == "__main__":
    sys.exit(main())
