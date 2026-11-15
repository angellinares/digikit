# digikit/debug

A ColdFire service module that holds the runtime's debug log and gives it to a computer. The debug log is on exactly while the module is loaded:
it is a default module of both device files, so a flash from the Firmware pane installs it, and the emulator loads it with the patched firmware.

The runtime calls one sink pointer (`dk_debug_log_sink_set`) with every event, before any filter. The module keeps a ring of 512 events (16 KiB, static memory), applies
the level filter (a default level and a level for each of the module records 0 to 7; errors always pass) and logs `MACHINE_CHANGE` from the tick of the sink (once per UI pass, set with the sink; the module holds no slot: a slot has one owner) (the machine type of each
track, read every 20 ms through `dk_debug_track_type`). When the sink is set, the runtime first hands over its own small ring (the boot and the errors so far), so the log
starts with the boot. Without the module the runtime keeps only that small ring, for the crash record's tail.

| Command | Number | Body | Reply |
| --- | --- | --- | --- |
| LOG | 0x20 | u32 first sequence number | u32 oldest, u32 newest, u8 n, then n events of 32 bytes (`seq`, `time`, `kind` u16, `flags` u16, five words; big endian), at most 27 |
| PEEK | 0x21 | u32 address, u16 length (1 to 512) | u32 address, u16 length, the bytes; only RAM inside the ranges of the device file (`[digikit] peek`, from the import `dk_debug_peek`), else `E_PEEK` |
| LOGCFG | 0x22 | u8 op (0 get, 1 set), a set adds u8 default level and 8 module levels (0 to 4, 0xff: the default) | u8 default level, 8 module levels, u8 1 |

## Profiling

The module keeps a table of span times for the runtime's profiling spans. It has one row per module record (`DK_MAX_MODULES`), 3 rows for the service phases (owner `0x100 | p`) and 4 rows for the polls (owner `0x200 | i`), 15 rows of 16 bytes at most. A row holds `u32 calls`, `u64 sum` (ticks, 64 bits so a long run does not wrap) and `u32 max`. Other owner ids are ignored.
Start registers the span sink and the clock (`dk_span_sink_set`); stop and fini clear them, so nothing is measured while profiling is off. The clock is the counter that `dk_debug_span_clock` returns; the module only reads it and never writes a timer register. Start answers `E_PROTO` when the device has no clock and the runtime's `E_BUSY` when another module owns the span channel.

| Command | Number | Body | Reply |
| --- | --- | --- | --- |
| PROFILE_STATE | 0x1e | u8 op (0 get, 1 start, 2 stop, 3 reset; a reset works in both states) | u8 state (0 stopped, 1 running), u32 clock Hz, u16 rows, u64 elapsed ticks (the span clock while running, since the last reset; the 32-bit counter is read each UI pass and at each PROFILE_STATE, so at least one read per wrap period, about 32.5 s at 132 MHz, keeps it exact), then the 4 poll slots, each u8 owner (0 empty, 1 + the module record that registered the function, 0xff the firmware image: a patched-in core module) and u32 function address (from `dk_debug_poll`) |
| PROFILE_READ | 0x1f | u16 first row | u16 first, u8 n, then n rows of u32 calls, u64 sum, u32 max (big endian), at most 54; a first row past the end gives `E_PEEK` |

## Card log

The module can write its ring to the store, so a log survives a crash or a power cycle while the host cannot reach the device. It is off by default. The setting `cardlog`
(8 bytes: version 2, on, last file, 0, u32 boot number; another version means the defaults) is read in `init` (`dk_settings_get`), so it is kept across restarts. Turn it on with the menu (the module's `menu` function: DIGIKIT > MODULES > SETTINGS... of the debug module: row
CARD LOG, and a STATUS row: OFF, ON n WR, FULL, ERR code) or with LOGCFG op 3. The module needs the files client and an I/O job (`dk_files_open`, `dk_io_post`); without them the card log is
unavailable (op 2 answers with an error status, the STATUS row shows N/A).

A boot writes one of two slots, `a` or `b`, and never the slot of the boot before (the setting keeps the last slot). A slot has two files: the rolling file `data/digikit/debug/cardlog-a.bin` (or `-b`) and the incident file `cardlog-a-inc.bin` (or `-b-inc`). A boot counts up the boot number. Before the first write of a boot, the tick saves the setting with this boot's file and
boot number; if that fails the state is error and nothing is written. The first write comes at least 3 s after the save (the settings reach the card 2 s after
the last change), so a file is written only when the card says that this boot owns it. The module tick (`dk_module.tick`, UI task, once per pass; the sink only sets flags) decides when to write, at most once in 2 s. Both files
use one staging buffer, one write at a time, and an I/O job replaces the file in one write.

Rolling file: written when there are events newer than the last rolling write, and there was no rolling write yet in this boot or the last one is 60 s old. Incident
writes do not reset that timer. Incident file: an incident is an event with the error flag or level warn or lower. With no capture open, an incident opens one: the incident file
is written as soon as allowed, and once more 10 s after the first incident event (the aftermath); then the capture is done. Incidents while a capture is open, or
less than 60 s after the last incident event, are a burst and open no capture; an incident 60 s or more after the last one opens a new capture, which replaces the file.
So periodic events cannot push a failure out of the incident file after the aftermath write.

Only a busy store (E_BUSY) or a file in use is retried later. A full store stops both files (state full, one log line); any other error, E_INUSE included, stops
them with the status (state error). Turning the log on again clears both. After a write, events that the write itself logged do not trigger another write.

File (big endian), always 64 + 512 * 32 = 16448 bytes: header of 64 bytes (u32 magic `DKCL` 0x444b434c, u16 version 1, u16 header bytes 64, u16 event bytes 32,
u16 events, u32 write number (from 1 in this boot), u8 file (0 a, 1 b), u8 kind (0 rolling, 1 incident; offset 17), 2 zero bytes, u32 oldest seq, u32 newest seq, u32 block clock at the write, u32 ring size 512,
u32 boot number (offset 36), zeros), then 512 records in the layout of a LOG event, oldest first; unused records are zero.

LOGCFG op 2 (get, no more body) and op 3 (set, body u8 on) reply with 16 bytes: u8 on, u8 file (0 a, 1 b), u8 state (0 off, 1 on, 2 full, 3 error), u8 0, u32 writes
(this boot, both kinds), i32 last status, u32 ms since the last write (0xffffffff: none yet). An op 3 applies at once; the tick saves the setting. A debug module without the card
log answers op 2 with an error status.

The commands lie in the `debug` range of `sdk/sdk.toml`. INFO reports the capabilities LOG, PEEK and LOGCFG only while the module is loaded. The levels are in RAM and start again
at each boot. Host side: `tools/device/dklog.py`, `packages/dk-session` (`logPage`, `logConfig`, `setLogConfig`), the Log drawer of the desktop workbench.
Tests: `tests/test_debug_module.py` compiles `src/c/debug.c` with a fake runtime and drives the sink and the commands. Facts and sizes: `docs/findings/46-digikit-debug.md`.
