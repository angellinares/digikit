# test/card-info

Read-only diagnostic of the eMMC card and its controller path. It writes no register and no memory.

It answers two commands of the `file_write_test_b` range of `sdk/sdk.toml` (0x28 to 0x2b). The manifest conflicts with
`test/file-write-b`, which uses the same range: do not load both.

| Command | Reply |
| --- | --- |
| 0x28 EXT_CSD | The firmware's own copy of the EXT_CSD (512 bytes, optional offset and length). Address: DT2 1.17 `0x4fe69100`, DN2 1.12 `0x4e6f9100`. |
| 0x29 REGS | Pairs of address and value for fixed eSDHC, PLL, USB OTG, crossbar and eDMA channel 59 TCD registers. |

Byte order: the eSDHC runs in little-endian mode, so the copy in RAM is byte-reversed inside each 32-bit word. The decoder
(`card_order()` in `tools/device/dkcardinfo.py`) restores the card's order. Do not decode the raw bytes without it.

Run on a device, without a flash:

1. Pack the module and upload it with `dkupload put`.
2. On the device, tick it in DIGIKIT > MODULES, then APPLY & RESTART.
3. `uv run --with python-rtmidi python -m tools.device.dkcardinfo --port <name>` (`--raw` prints JSON).
