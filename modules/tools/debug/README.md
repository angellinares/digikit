# digikit/debug

A ColdFire service module that holds the runtime's debug log and gives it to a computer. The debug log is on exactly while the module is loaded:
it is a default module of both device files, so a flash from the Firmware pane installs it, and the emulator loads it with the patched firmware.

The runtime calls one sink pointer (`dkrt_log_sink_set`) with every event, before any filter. The module keeps a ring of 512 events (16 KiB, static memory), applies
the level filter (a default level and a level for each of the module records 0 to 7; errors always pass) and logs `MACHINE_CHANGE` from its tick (once per UI pass, set with the sink; the module holds no slot: a slot has one owner) (the machine type of each
track, read every 20 ms through `dkrt_track_type`). When the sink is set, the runtime first hands over its own small ring (the boot and the errors so far), so the log
starts with the boot. Without the module the runtime keeps only that small ring, for the crash record's tail.

| Command | Number | Body | Reply |
| --- | --- | --- | --- |
| LOG | 0x20 | u32 first sequence number | u32 oldest, u32 newest, u8 n, then n events of 32 bytes (`seq`, `time`, `kind` u16, `flags` u16, five words; big endian), at most 27 |
| PEEK | 0x21 | u32 address, u16 length (1 to 512) | u32 address, u16 length, the bytes; only RAM inside the ranges of the device file (`[digikit] peek`, from the import `dkrt_peek_range`), else `E_PEEK` |
| LOGCFG | 0x22 | u8 op (0 get, 1 set), a set adds u8 default level and 8 module levels (0 to 4, 0xff: the default) | u8 default level, 8 module levels, u8 1 |

The commands lie in the `debug` range of `sdk/sdk.toml`. INFO reports the capabilities LOG, PEEK and LOGCFG only while the module is loaded. The levels are in RAM and start again
at each boot. Host side: `tools/device/dklog.py`, `packages/dk-session` (`logPage`, `logConfig`, `setLogConfig`), the Log drawer of the desktop workbench.
Tests: `tests/test_debug_module.py` compiles `src/c/debug.c` with a fake runtime and drives the sink and the commands. Facts and sizes: `docs/findings/46-digikit-debug.md`.
