# 46. digikit card safety and debug (log, PEEK, crash capture, dklog)

Lane r4, 2026-10-18. Plan: `docs/plans/digikit-runtime.md` Phase 9 items "Card safety" and "Debug". Spec: `docs/design/digikit-upload.md` (LOG, PEEK, INFO,
"Card safety"). Marks: **[V]** verified here, **[D]** measured or read once, **[O]** open, **[C]** corrects an earlier claim. Everything here ran in the
emulator; nothing ran on a device. What no emulator can tell is listed under "Open".

## What was built

| item | where | proof |
|---|---|---|
| One card-write function with a hard range check; read-only on a card that is not ours and not blank | `modules/core/runtime/src/rs/src/card.rs`, `block_write` in `lib.rs`; range test in `dkrt_sdk` (`store::write_allowed`, unit test) | scenario `card-safety` (dt2) |
| The card check: ours (valid header A or B, or valid selection copy) / blank (whole range all 0x00 or all 0xFF) / foreign / unreadable; claim of a blank range before an upload stages data | `card.rs` | `card-safety` |
| The runtime's event log: every call site calls `dlog::ev`; a small ring of 32 boot and error events (the crash record's tail), one sink pointer (`dkrt_log_sink_set`) that receives every event; health snapshot, DSP diagnostics and heartbeat watch (`watch.rs`) | `dlog.rs`, `watch.rs`, `crash.rs`, `lib.rs` | `debug-log` (quick and full) |
| The module `digikit/debug`: the 512-event ring (16 KiB in the module's own memory), the level filter, LOG, PEEK and LOGCFG, the track watch. The debug log is on exactly while it is loaded | `modules/tools/debug` | `modules/tools/debug/tests`, `debug-log` |
| SysEx LOG (0x20), PEEK (0x21) and LOGCFG (0x22): the service range `debug` of `sdk/sdk.toml`, registered by `digikit/debug`. INFO reports the capabilities LOG, PEEK and LOGCFG only while a sink is set, and flag bit 3 (debug module loaded); its log range is 0 | `modules/tools/debug`, `upload.rs`; spec in `digikit-upload.md` | `debug-log` |
| CPU exception capture across a soft reset, next-boot report, bad-boot counting | `crash.rs`, `src/cf/tramp.s` (`dkrt_exc_hook`), anchor `dkrt.fw.exc_hook_lit` | `crash-capture` (dt2, full tier) |
| Mac tool `tools/device/dklog.py` (`follow`, `crash`, `peek`, `info`) | `tools/device/dklog.py`, `Uploader.log_page` / `peek` in `dkupload.py` | `tests/digikit/test_dklog.py`; used by `debug-log` and `crash-capture` against the emulator |
| Device facts as data: `[digikit] keep_base`, `keep_bytes`, `peek` | `devices/*.toml`, `packages/patchkit/src/devgen.rs` (`KEEP_*`, `PEEK_*`), `device.s` | `packages/patchkit/tests/devdata.rs` |
| Emulator: RAM kept across a soft reset (model), CPU exceptions delivered to the guest (RPC `cpu.exceptions`) | `packages/emulator/{boot,control,coupled,device-profile}` | `crash-capture` |

A host asks INFO for the capabilities: without `digikit/debug` the commands LOG, PEEK and LOGCFG answer `E_PROTO`. The protocol version stays 1.

## Card safety

**[D]** Rules (the same text is in the spec): every runtime card write goes through `block_write`. It refuses a write whose sectors are not all inside
`[STORE_FIRST, STORE_FIRST + STORE_SECTORS)` (`E_RANGE`, counted in `DKRT_CARD.refused`, one error event each), and every write when the area is not ours and
not blank (`E_CARD_RO`). The check (`card::check`) runs in the boot hook, before anything is written (and at the first write if no boot hook ran):

1. **ours**: store header A or B parses (magic, version, CRC for version 2), or a selection copy decodes. Four sector reads; the range is not scanned.
2. else **blank**: every sector of the 12 MiB range reads all 0x00 or all 0xFF (per sector; the eMMC erase value is one or the other). Read in 64 KiB
   pieces through the staging buffer, stopping at the first sector that is not. This costs the boot a scan only on a card where none of our structures is
   present (a fresh card, or a card with foreign bytes: then the scan stops at the first one).
3. else **foreign** (`first_nonblank` sector and the crc32 of that sector are kept and logged), or **unreadable** when a read failed.

Foreign and unreadable make the card read-only for the runtime. Then: no boot counter (so no safe mode by counter; ABOUT says `NO BOOT COUNTER`), a menu
selection save answers `E_CARD_RO` (the popup says READ ONLY), BEGIN, DELETE and CLEAR answer `E_INUSE` (-62); INFO and LIST still work (and LOG and PEEK, with `digikit/debug`);
ABOUT shows `CARD READ-ONLY`, `NOT OURS AT <sector>` (or `CARD UNREADABLE`) and `NO BOOT COUNTER` (checked on the OLED PNG: the longest row is 15
characters and the screen holds about 22).

**The claim.** A write that touches a store header sector or a selection sector is itself the mark of ours; a blank card written by the menu (selection)
costs exactly the two record sectors, as before. An upload stages data first, which would leave unexplained bytes in the range if it were cut before a header
exists, and the next boot would call the card foreign. So the first write of data or a table into a blank range first writes an empty store header pair
(`seq` 1; the first upload on a blank card therefore commits as `seq` 2). **[D]** `card-safety` part `claim` cuts an upload after its first chunk and boots the
card it leaves: ours, empty store, the next upload works.

**Kept scenario `card-safety`** (dt2, quick tier, 23 s; cf-only daemons, the write-watch and card-write checks as for the upload scenarios): blank card
(state, 8 refused probes of different shapes, counters, nothing written, selection written as before and the area is ours then, log events), foreign card
(state foreign, sector 100 and its crc32, boot normal, selection / in-range probe refused with `E_CARD_RO`, out-of-range `E_RANGE`, no record
written, INFO flag, BEGIN/DELETE/CLEAR `E_INUSE`, the log event, ABOUT rows read from `DK_ABOUT` plus a PNG, no store-range write at all), our store (state,
no scan, selection and upload work), claim (above). The probe is the mailbox command `CMD_CARD_PROBE` (7), which calls `block_write` with the staging buffer;
it is a test hook and the scenarios use it only on sectors outside the range or on a card that is not ours.

**Residual risk [O]:** a power cut in the middle of the very first write to a blank card, inside the single sector write of the first header or selection copy,
can leave a torn sector that is neither valid nor blank: the next boot calls the card foreign and stays read-only. The way out is to wipe the range from a
host (there is no CLEAR for a foreign card by design). The eMMC guarantees sector writes only so far; the window is one sector write once in the card's life.

## The log

Event: `u32 seq` (from 1), `u32 time` (the firmware's block counter, 1500 per second; with no counter anchor 0), `u16 kind`, `u16 flags` (bit 0 error),
five argument words. The runtime's small ring holds 32: every event until the first UI frame (after all modules' `init`), then error events (not the long detail of a replayed crash record, which `crash::replay_detail` hands to a sink when one is set) and the boot's own kinds. A sink gets every event first, with interrupts masked, and the module's ring holds 512; appends mask interrupts for a few instructions. Without a sink the other events are dropped after the boot. A new sink gets the small ring's events at once, so the module starts with the boot; the module keeps their sequence numbers, so events the ring lost show as a gap (LOG's `oldest` starts after it). Nothing is ever sent
unless a host asks (LOG), and LOG, PEEK and INFO are not logged themselves. Names and arguments (`dkrt_sdk::logk`, `tools/device/dklog.py KINDS`):

| id | kind | arguments (a0..a4) | error |
|---|---|---|---|
| 1 | BOOT_START | runtime version, device id, stable_blocks of the record, record seq, boot counter | |
| 2 | CARD_CHECK | state (1 ours, 2 blank, 3 foreign, 4 unreadable), first non-blank sector (0xffffffff: none), crc32 of it, failed reads | foreign, unreadable |
| 3 | SAFE | reason (1 counter, 2 key, 3 flag), boot counter, record seq | yes |
| 4 | BOOT_RECORD | seq, copy, boot counter (after the crash adjustment), selected, flags | |
| 5 | MOD_LOAD | selection index, result, first 12 characters of the file name | on a failure |
| 6 | MOD_DSP | index, result of the DSP part | on a failure |
| 7 | UPLOAD | command, status, card writes so far | status != 0 |
| 8 | MENU | 1 select (count), 2 restart | |
| 9 | HEALTH | health flags << 16 \| cpu x10, dsp avg x10, dsp max x10, arena free, heap free KiB (0xffffffff: not walked) | |
| 10 | DSP_DIAG | count << 8 \| chunk, then four `dl_diag` words raw (24 words since runtime 5, six events; 17 words and five events before; named fields in `dklog`) | error flag in a stop's own set |
| 11 | DSP_STOPPED | heartbeat, voice hook's beat, breadcrumb word, dispatches to registered types (a4 = 1: it moves again) | yes |
| 12 | CRASH_FOUND | pc, vector \| format << 8, sr, a7, block counter of the crash | yes |
| 13 | CARD_REFUSED | sector, bytes, reason (1 outside the range, 2 read-only, 3 claim failed), card state | yes |
| 14 | MOD_LOG | the module's `api.log` text (16 characters), value | |
| 15 | UI_FIRST | block counter, safe, modules loaded | |
| 16 | STABLE | block counter, the counter the stable period leaves (0, or 1 after a crash in a stable session) | |
| 17 | DEBUG_SET | on | the runtime no longer logs it (the debug switch is gone); the id stays for old logs |
| 18 | RAM_KEEP | the keep probe of the boot before was in RAM (1/0), its generation | |
| 20 | DSP_ABSENT | (the image has no `dl_diag`; once) | |
| 21 | MOD_LIST | index, slots mask, name (when the DSP stopped: the loaded modules) | yes |
| 22..25 | CRASH_MOD, CRASH_REG, CRASH_STACK, CRASH_TAIL | the crash record, decoded by `dklog crash` | yes |
| 26 | MACHINE_CHANGE | track (0 based; `dklog` shows 1 based), old machine type, new type (0xffffffff: none) | |
| 27 | MACHINE_FAULT | type \| voice << 16, kind (1 Inf or NaN, 2 over the level bound) \| faults << 8, knobs 0 to 5 of the call in pairs (runtime 5, finding 51) | yes |
| 28 | MACHINE_DISABLED | type, machine types disabled so far, faults so far (runtime 5, finding 51) | yes |
| 37 | POOL_LOAD | client << 8 \| pool index, result (0 usable), frames, base_l, token | |
| 38 | POOL_PUBLISH | client << 8 \| track, token, frames, base_l, base_r | |
| 39 | POOL_LOST | calls of `dkrt_pool_lost` since the start (warning level) | |
| 40 | POOL_INVALID | client << 8 \| pool index (client 0xff: periodic check), reason (1 token 0, 2 load check failed, 3 no frames, 4 periodic check failed; check result in the high 16 bits), token, drops so far (first 64, then every 64th) | |
| 41 | INPUT_TAKEN | what (1 key, 2 turn), key code or encoder, delta taken (turn: signed; key: 1 down, 0 up). Logged by `digikit/remote` at debug level (3) when core/input handed the input to the firmware decoder | |

**Periodic work** (`watch::tick`, once per UI pass, reads the block counter): the DSP poll once a second (1500 blocks), a HEALTH snapshot every ten seconds
(the heap walk only in every fourth, because it takes the heap's mutex: the snapshot uses `dkrt_health_light`, which skips the store figures and, when asked,
the walk). **[D]** `debug-log` (full tier, coupled daemon): snapshots 15,000 blocks apart, the stable-period event once after 30 s. A daemon without DSP does
not advance the block counter, so the periodic events do not happen there (the quick tier runs the cf-only daemon and checks the rest).

**INPUT_TAKEN and the host wait.** `digikit/remote` logs `INPUT_TAKEN` (kind 41) after `input_key` or `input_turn` returned 0, with the delta the module let through (a turn is
paced to 30 detents, so the delta can be smaller than the request). Level debug (3): the default level (info) drops it, so a host raises the level of the module
record with LOGCFG. The event says "the firmware decoder took it". The firmware applies a turn at its next tick, so it does not say "the parameter changed". A refused
input (a negative result of core/input) is not logged. The host side is `Events` (`packages/test-support/src/events.ts`; `listenEvents` and `remoteTurnTaken` in
`tests/regression/runtime/events.ts`): `listen()` puts a cursor at the newest event and sets the levels of `digikit/remote` and `digikit/debug` to 3 (the default stays);
`until(kind, predicate, {emulatedMs})` consumes events up to the match. Only sequence numbers order events (no block counter on a zero-DSP daemon). A restart restarts the
log at sequence 1: the helper keeps the last consumed event, reads it again at each poll, and treats a different event, a shorter log or a BOOT_START after the cursor as a new
boot (cursor to the new first event, levels set again). A sequence gap (oldest kept event after the cursor) is an error. A miss fails with the last events.
**[O]** A `PARAM_CHANGE` event of `digikit/debug` (track, parameter, old and new raw value) was not built: the track watch reads the machine type through three anchors, but the
sound parameter array is read in tests only through fixed addresses and offsets of finding 02 (`*0x80004704 + 0x34 + 0x450 t + 0x14` on DT2; DN2 has a different, odd stride).
No anchor gives the Sound array of a track. It needs: the Sound array offset and stride per device in the device files (or an anchored accessor), a check that `active_kit` returns
that kit object, and the map from a parameter code to its mirror index (the descriptor table).

**Machine-change watch.** `digikit/debug` (its tick, once per UI pass) reads each of the 16 tracks' machine type through the runtime import `dkrt_track_type`, which calls three anchored accessors
(`dkrt.fw.project`, `dkrt.fw.kit_of`, `dkrt.fw.track_type`: `Project *f()`, `Kit *f(Project *)`, `int f(Kit *, int track)`; the functions of the archived hot-reload
runtime's in-use check) every 30 blocks (20 ms), starting 3000 blocks after the first UI pass, and logs `MACHINE_CHANGE` when a type differs from the last read (the first
read is the baseline; the watch exists only while the module is loaded). The event's source is `digikit/debug`. A project load logs one event per track that changed. **[D]** `debug-hang`: selecting the hang machine on track 1 logs
track 0, old 0, new 19. **[O]** The first note started on a track after a change is not logged: no firmware word seen so far says which track a note started on, and the DSP
diagnostics count dispatches per machine type, not per track. The CPU cost (16 calls per 20 ms) is not measured on the device.

**DSP heartbeat watch.** `dl_diag(out: *mut u32, n: u32) -> i32` is in `core/dsp-link` (finding 45; since finding 48 17 words); it is an optional
extern (`[object] optional`, 0 when absent: the first poll then logs `DSP_ABSENT` once). The runtime asks for 17 words and takes word 1 (the link hook's beat) as the
heartbeat (`dlog::DIAG_HEARTBEAT`, one constant). The watch: a larger value than the last (the two reply records alternate, so a different value is not enough),
then no larger value for 3 s of the firmware's block counter (or none for 20 s after the first) gives one error event `DSP_STOPPED` (heartbeat, voice beat, breadcrumb,
dispatches), the full 17 words as error `DSP_DIAG` events, one `MOD_LIST` event per loaded module, the ABOUT rows `DSP STOPPED` and `LAST: ...` (the breadcrumb),
and `dkrt_diag.dsp_stopped`/`dsp_crumb`; a larger beat later logs `DSP_STOPPED` with a4 = 1. A reply with no diagnostic block counts as no beat. **[D]** `debug-log`
(full tier): `DSP_DIAG` at the first poll and then every ten seconds while only counters move (16 events in 32 s), never `DSP_STOPPED` on a running DSP; `debug-hang`
(finding 48): a routine that never returns is reported after 4 s with its step, voice and type. On the device: rung B ran 172 s with both beats rising 1500 a second
(finding 47); a stop on silicon has not been seen.

**PEEK.** Body `u32 addr, u16 len`; up to 512 bytes of RAM wholly inside one range of `[digikit] peek` (device file; generated into `device.s` as `PEEK_*`):
the firmware's DDR data and the runtime's own region with the keep area, no peripheral registers. **[D]** refused: a peripheral register, just below the first
range, past the end of the last, the gap between the ranges, an address that wraps, length 0 and 513.

## Crash capture

**The firmware side [D]** (static, all four images; the anchor resolves on all four ports and each image applies). Every vector points at one default handler
until a specific one is installed (finding 37). It sets `sr` to 0x2700, saves its stack pointer in a RAM word, saves d0, d1, a0, a1 below the exception frame,
loads a function pointer from a second RAM word (DT2 1.16 and 1.17 `0x40964918`; DN2 1.11 `0x4058d160`, 1.12 `0x4058e160`), and if it is not 0 calls it with the
saved stack pointer, which points at the hardware frame (first word: format, fault status, vector, status register; second: the PC); the handler `rte`s when the
function returns. The firmware's own function there (finding 37: `FUN_4011d0f6` on DT2 1.16) prints `EXCEPTION ...` and executes `halt`. The anchor
`dkrt.fw.exc_hook_lit` is the 4-byte literal of the `movea.l (<hook word>).l,a0`, found by the words of the handler (`cf_words`, offset 18 from the second word;
address `0x40001266` on all four images, one match each); the hook word's address is never a literal in our code.

**Our hook.** `dkrt_exc_hook` (tramp.s) saves d2-d7 and a2-a6 (still the faulting code's: no compiled code has run), then calls `dkrt_exc(frame, saved)`
(`crash.rs`), which writes the record, flushes it with `cpushl` and calls the soft reset of `dkrt_restart` (`dkrt_soft_reset`), instead of halting. The runtime
points the hook word at itself in every UI pass (the firmware sets it in its own boot, so the first assignment must come after that, and a later setting would
be undone); the cost is two reads per pass. A fault inside the handler (a second entry) skips the record and resets at once, after noting the count.

**The record** (`crash.rs`, 130 words, CRC32 over everything with the magic included, written last): the block counter, vector and format, `sr`, the stacked PC,
the interrupted `a7` (the frame address plus 8 plus the alignment padding the format records), d0-d7 and a0-a7, 32 words of stack from `a7` (only when `a7` is in
DDR and they fit), whether the boot had reached its stable period, a mask of the slots with a task inside, the index and name of the module whose code holds
the PC, and the last 8 log events.

**Where it lives and why [D].** The first 4 KiB above `cf.expansion` (`[digikit] keep_base = 0x47fe0000`, `keep_bytes = 0x1000`, both devices). Finding 01
("Expansion region") shows that region of RAM is not referenced by the firmware, is above the bss end (the reset clear does not touch it), and that the
128 KiB below the reset stack are a margin; patchkit's `cf.expansion` ends at `0x47fe0000`. The runtime's own bss is not usable: the reset path zeroes it.
The record is at the start, a probe word (magic, generation, its complement) at `+0xf00`. The probe is written at every boot and read at the next: the event
`RAM_KEEP` says whether RAM from the boot before is still there, and ABOUT shows `RAM KEPT: YES/NO`. **[O] Whether a soft reset (`RCM.RCR` = 0x80) leaves RAM
alone on the device is not known.** The firmware clears DDR word 0 itself before it resets "so that a stale magic does not start the next boot in console
mode" (finding 36), which suggests the boot code reads DDR across a reset, but the bootloader and the DDR controller after reset were not examined.
**How we will know on the device:** do APPLY & RESTART once in SETTINGS > DIGIKIT > MODULES (any selection; it is the same reset the crash path takes) and
open ABOUT: `RAM KEPT: YES` means the area survived (and `dklog follow` shows `RAM_KEEP kept=1`); `NO` means crash records are lost on that board and the
fallback has to be another place (the internal SRAM at `0x80000000`, which the boot code copies `.data` into and the reset path clears, would need a
look at what the boot code does with it; a card write from the crash handler is not an option: it runs with interrupts masked and the eMMC transfer needs them).
A power cycle always shows `NO`.

**Next boot** (`crash::at_boot`, in the boot hook after the card check): the record is found, cleared first (so a record that crashes the boot again is not
found twice), logged as events 12 and 22 to 25 (`dklog crash` rebuilds it), and kept in `DKRT_CRASH` for `dkrt_diag`, INFO flag bit 4 and ABOUT (`CRASH PC
<hex>`). **Safe mode:** a crash counts the crashed boot as bad. A crash before the stable period (counter still raised) needs nothing more: two in a row
lead to safe mode by the existing counter rule. A crash after the stable period (counter cleared) adds one to the counter at the next boot and keeps that
one (`LATE_FLOOR`: the stable period of the boot that follows leaves 1, not 0), so that a second such crash makes the third boot safe. A boot that follows a
clean boot still clears to 0. **[D]** Both sequences are in `crash-capture`.

**Kept scenario `crash-capture`** (dt2, full tier, about 2 minutes, coupled daemons; test module `test/dyn-crash`, which executes an illegal instruction (early,
before the stable period) or divides by zero (late, after it) some blocks after its first slot call and logs the address first): boot 1 sees no record, the hook
is installed and RAM from before is not there; boots 2 and 3 find the record with the PC the module logged (the same value three ways: the record, the
`DKRT_CRASH` summary, the module's own `crash-pc` event in the record's tail), vector 4 or 5, the module's index and name, 16 registers, 32 stack words and the
events before the crash; `RAM_KEEP` says kept; the counters are 0, 1, 2 (early) and 0, 1, 2 with the stable flag (late); boot 3 is safe mode (counter), loads
nothing, clears the counter at its first pass and keeps the selection.

**What the emulator models and does not.** A soft reset in the emulator is a cold boot of the same patched firmware (finding 37). The emulator now carries
the device file's `keep_base .. +keep_bytes` across it, so the runtime sees what a device that keeps RAM would show; `[O]` as above for the device. An exception
is a fault of the run by default (an exception in the firmware is a bug the harness must see: the step fails with `exception vector N`); the RPC
`cpu.exceptions {deliver: true}` makes the CPU run the guest's handler, kept for every runtime installed afterwards. An unmapped read is not an exception for the
guest in the emulator (the board returns a bus fault that ends the run), so the second kind of crash in the module is a division by zero, not an access error.
Harness gotchas found on the way **[D]**: a `step` submitted while a cold boot runs on its worker waits and then runs against the new machine with a budget
counted from the old one (hundreds of millions of instructions: a second crash before the rig has looked); the controller finishes its worker only when a
command wakes it (a `status` request will do). `BootRig.wait_reset` does both right.

## The Mac tool

`dklog follow` polls LOG once a second (`--interval`), writes a readable `# ` line and a JSON line per event to `out/runs/device-logs/<date>.log` and the
readable line to the terminal, notes lost events (the ring overwrote them) and a device restart (the sequence numbers start again), `--once` stops after
reading what is there. `dklog crash` prints the last crash record decoded; `dklog peek ADDR LEN` hex-dumps (any length, in 512-byte requests); `dklog info` prints INFO. `follow`, `peek` and `level` need `digikit/debug`; `crash` prints the kept fault record (`dk/fault`) without it. With
`--emu`, `--run-dir DIR` names the emulator's run directory (the scenario uses it for a second connection to the rig's daemon, so the command line itself is tested). Transports as
`dkupload`: `--emu`, or `--port NAME` with python-rtmidi (`uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt follow`); **the device
transport has not been run** (no real MIDI device was touched). A ring of 512 events holds about three minutes at one to three events a second, so a poll
every second loses nothing in normal use.

## Cost when nobody polls

**[D]** One paired measurement, `uv run python -m tools.patchkit.pk_digikit_cost run` (dt2-1.17, coupled daemon, `core/health` in both images): A is the runtime
of the commit before this work (`git archive` laid over a copy of `modules/`, patched with `patchkit --root`), B is the working tree with the log ON (the
default) and nobody polling; the same card (a selection that loads the pluck machine), the same script: boot, one note on the run-time machine, 30 emulated
seconds of idle UI loop. The emulator is deterministic, so these are counts:

| | A (before) | B (with the log) |
|---|---|---|
| PCM of the note (95,872 samples, peak 0.114) | | **equal** (`numpy.array_equal`) |
| ColdFire instructions interpreted per emulated second | 539,041,455 | 539,061,958 (+20,503 a second, **+0.004 %**) |
| HEALTH CPU load (tenths of a percent, `dkrt_health`) | 205 | 205 |
| HEALTH idle-loop iterations in the last one-second window | 30,453,738 | 30,452,519 (-0.004 %) |
| HEALTH DSP block time, average and maximum (tenths of a percent) | 311, 311 | 311, 311 |

The per-pass cost is two reads for the exception hook word, one block-counter read and a compare for the periodic work; the once-a-second DSP poll (absent
here: no `dl_diag`) and the ten-second health snapshot add a few hundred instructions each. The ceiling for the claim is the cost of the work itself:
the added code is a few hundred instructions a second against 539 million, and the measurement agrees. The log's own appends happen at events
(boot, uploads, menu actions), not per pass. What this does not show: a real `dl_diag` poll (lane b) costs what that function costs, and the heap walk
of every fourth health snapshot (bounded, under the firmware heap's mutex, as the HEALTH page's refresh does) is in the figures only if one fell in the window.

## Open

- **[O]** RAM across a soft reset on the device (above). If `NO`, crash capture needs another place before it is worth anything on the desk.
- **[O]** `dl_diag` (lane b): the word layout and the heartbeat index; the watch has never seen a real heartbeat.
- **[O]** The stacked PC of an access error, an address error and a divide on a real MCF5441x (the emulator stacks the faulting instruction's address;
  the module's `crash-pc` check proves that for the illegal instruction and the divide here).
- **[O]** A card whose range holds old bytes from earlier formats would be foreign. The range was chosen as unused by the firmware
  (finding 23 S6: not a measurement); the first device boot will say (`CARD_CHECK` in the log, and ABOUT).
- **[O]** The torn first write to a blank card (above).
- The log is lost at a reset (RAM); only the crash record, with the last 8 events, survives.

## The log as a module (phase 3 of docs/plans/debug-mode-and-probes.md)

**[D]** The runtime keeps what the fault paths need and moves the log out. Runtime object (`runtime.dko.json`) before and after: text 109,624 to 108,406
bytes, data 3,547 to 2,736, bss 671,108 to 657,296 (the 16 KiB ring became 1 KiB, and 0.5 KiB holds the crash record found at boot for a sink that is set
later). `digikit/debug` is 1,724 bytes of file and 16,472 bytes of bss; so with the module loaded the memory use is about the same as before, and without it
the runtime holds about 15 KiB less.

- The runtime keeps: the fault record, `dk/fault`, safe mode, the DSP heartbeat watch, the guards (`watch.rs`), module-source tracking (`dlog::enter_module`, owner of
  the command ranges) and the small ring of 32 events for the crash record's tail. A crash record found at boot is replayed as events; the long detail (registers,
  stack, tail, DSP words) is kept in RAM and goes to a sink when one is set.
- The module: the sink, the level filter (default and per module, RAM only), LOG, PEEK (the ranges come from the runtime import `dkrt_peek_range`), LOGCFG (ops 0 and 1;
  op 2 is gone), the track watch. Imports added: `dkrt_log_sink_set` (the sink and a tick, so the module holds no slot: a slot has one owner and the test modules need slot 0), `dkrt_log_ev`, `dkrt_peek_range`, `dkrt_track_type`.
- The debug switch is gone: the selection record's bit 1 is reserved and ignored, `dkrt_debug_set` and the mailbox command 8 are removed, and so is the DEBUG row of the
  DIGIKIT menu. Debug on or off is `digikit/debug` loaded or not; it is a default module of both device files.
- Without the module a crash still leaves its record: the tail comes from the small ring, so it holds error events and the boot's events, not the info events
  (`MOD_LOG`, `HEALTH`, `MACHINE_CHANGE`) that the 512-event ring used to add.
- Safe mode loads no module, so after a crash or a stop the boot has no log: `digikit/debug` is not loaded, INFO has no LOG capability, and the long detail of the
  crash record (`CRASH_REG`, `CRASH_STACK`, `CRASH_TAIL`, the DSP words) exists only in RAM (`DKRT_CRASH.tail` counts the tail's events; the runtime's ring
  `DKRT_LOG` has `FAULT_FOUND`, `SAFE` and `CRASH_FOUND`). `dk/fault` and ABOUT carry the verdict. A crash that does not lead to safe mode (the first one) loads the
  module again, which gets the whole record from `crash::replay_detail`. **[O]** Whether the safe boot should load `digikit/debug` (the one module that cannot crash a
  boot by itself) is open.
- A host cannot tell a new boot from the old one by the log without the module. INFO keeps its wire format (bytes 17 to 24, the log range, are 0 and 0; a host takes
  the range from the module's LOG reply). `DkSession.waitForBoot` takes the device as booted when it answers again after a silent spell; `dkrun` always selects
  the module, so its boot wait uses the log.
- A module that loads before `digikit/debug` in the selection logs its first lines (`MOD_LOG` of its `init`) before the sink exists. Until the first UI frame
  the small ring keeps every event, so the debug module copies them when it starts (test `a module that loads before the debug module has its log line in the
  debug log`, with `digikit/remote` first). If more than 32 events come first, the oldest are lost and the debug log starts after a gap.
