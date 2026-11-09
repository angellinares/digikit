# 77. The fault trace: what the device keeps when the DSP stops or the ColdFire crashes

Trigger: on a real Digitakt II, enabling the FX module `fx/freqshift` (route send by default) and restarting gave the loading spinner and, a few seconds later, a
frozen device. It does not happen in the emulator. A silent halt of the SHARC in the effect path is the likely cause **[O]**. Before this change the DSP fault
reporting covered only the voice hook, the log was RAM-only, and `DSP_STOPPED` only logged. Marks as in AGENTS.md: **[D]** measured here (emulator), **[O]** open.
Nothing is **[V]**; the device has not run any of this.

## 1. What is kept where

| Part | Where | Survives |
| --- | --- | --- |
| The fault table (32 DSP words) | the DSP's reply record, `core/dsp-link` (words 24 to 31 are new; `modules/core/adapters/dsp-link/README.md`, `src/dsp/dsp-link.inc`) | until the next block |
| The hang record (kind 2) and the crash record (kind 1) | the keep area `0x47fe0000`, format version 2 (`crash.rs`) | a soft reset (verified on the DT2, finding 47) |
| `dk/fault` (40 bytes) | the settings store on the card, written at once when the next boot finds a record | power cycles; stays until a newer fault replaces it |
| The ABOUT rows | built from `dk/fault` by `dkrt_diag` (`DkrtDiag.fault_text`) and shown by the DIGIKIT menu | |

## 2. The fault table

The table is filled in every image, with or without `core/dsp-voice` (before, the diagnostic words were copied only when the voice hook's `_dv_diag` existed).
The record has room: 32 slot rows, and the table used the free halves of rows 1 to 12. Words 24 to 31 use rows 13 to 16, so no word is paged or dropped and
`pk_ctxprobe` only needed the new count. **[D]** On the Digitone II these bytes are samples of the effect buses (finding 31); four more rows of the same kind as the
twelve used before. A fixed pattern in those channels at the block rate is possible there **[O]**.

New words: 24 `last` of `core/fx`, 25 the module step, 26 and 27 module calls of the master and send routes, 28 effect faults with the row of the last one,
29 and 30 the addresses of the two reply records, 31 the effect row switched off last. The voice site's `last` is the breadcrumb of finding 48 (word 14); the
Digitone II wrapper now writes its stage 4 and 5 (call and return). Stage numbers are the breadcrumb's: 4 the core is about to call the module (CALL), 5 it returned (RET).

**Why the sites write straight into the records.** The block-tail hook never runs again once the core stops (finding 48). Each site writes `last` into both reply
records, with the header word, at the call and at the return, and clears the module step word `_dk_step` (a DM word, not a record word) before a call. The tail copies what the
sites keep in DM, and `_dk_step`, for the blocks that end.

**The module step.** `core/dyndsp` exports the import `_dk_step` (a 32-bit word; lane 2's `DK_STEP` stores a step number there). The sites clear it before each call into a
module, and the block tail copies it into word 25 of the block's record. **[C]** The first design pointed the stores at word 25 of the older reply record (`patch_steps`, deleted);
the sites cleared that word in both records, and on a Digitakt II the word was lost (finding 101). A step stored in the call that stops the DSP is not in any record, so the fault record
shows the step of the last block that ended. **[O]** Whether the real DMA keeps sending both rings after the core stopped is the static argument of finding 48, section 2.

## 3. The hang record and the recovery

`dlog.rs` `stall_check` declares a stop after 3 s without a heartbeat (20 s when the heartbeat never moved twice, for example a module that stops within the first
seconds). It then logs as before, keeps reading the replies for 40 UI passes (both rings; the step and the sites with a call pending are merged), and `crash.rs`
`write_hang` writes the hang record (site, machine type or effect row, module, core step, module step, the 32 diagnostic words, the last 8 events) and soft-resets.
It is armed only after the first UI pass and a valid reply, never in a safe boot (a stop there would reset again and again). The module is found from the site:
`dsp::module_of_type` (voice) or `dsp::module_of_fx_table_row` (effect row).

At the next boot (`crash::at_boot`): a hang record gives safe mode with the new reason `SAFE_DSP` (4), ahead of the boot counter rule and after the flag and the key; one stop
is enough, even after a stable boot, and the selection stays. Both record kinds are copied into `dk/fault` and flushed at once. A hang record does not count as a crash
for the boot counter. ABOUT shows `SAFE: DSP STOPPED` and `FAULT: DSP STOP` or `FAULT: CPU CRASH`, `IN <MODULE>`, and the site and step (or `PC` for a CPU crash). ABOUT rows are cut at about 18 characters on DT2, so the fault rows are at most 18. `dklog crash` decodes the record from the
events and `dk/fault` from the card.

Effects that fault without stopping the DSP give `FX_FAULT` and `FX_DISABLED` events tagged with the module's source. The source is now set for slot 0 and for registered
service commands too (before: only `init` and `fini`). Callbacks that `core/ui` and `core/params` make into modules from C code are not tagged **[O]**.

## 4. DSP cost

Instructions per block on the concrete core (`pk_adapter_cost`, `test_fx.py`, `test_fx_send.py`; before = HEAD, after = this change; the rig has no `core/dsp-link`, so the
record writes are not in these numbers and are counted from the source):

| Part | Before | After |
| --- | --- | --- |
| DT2 voice, per dispatched voice (rig, no record) | steady 1606, idle 420 | steady 1615, idle 429 (+9; about +16 with the records) |
| DN2 voice, per dispatched slot | steady 1527, idle 284 | steady 1562, idle 319 (+35; about +63 with the records) |
| FX master or send, per module call | one effect steady 760; one send effect 1264 | 808; 1312 (+48; about +72 with the records) |
| Block tail of `core/dsp-link` (counted from the source) | 61 with `core/dsp-voice` | 98 with voice and fx (+37); 76 with voice only; about 25 with neither |
| Empty chain | 16 | 16 |

A block with one effect and one machine voice on the Digitakt II costs about 120 instructions more (0.02 % of a block's 666 667 cycles, at one cycle each).

## 5. Where the OS upgrade fits

`docs/findings/75-os-upgrade-receive.md` reads the receive path statically (a MIDI task, a background worker that erases and programs the flash, then a reboot). It
does not say whether the firmware stops or pauses the DSP blocks meanwhile, and the emulated runs there used `--dsp zero`, so they show nothing about the DSP. **[O]**
What limits the risk: the watch runs in the UI-loop hook and counts the firmware's own block counter, so it declares a stop only when that counter moves, the UI loop runs
and the DSP heartbeat does not move for 3 s (20 s if it never moved twice). A DSP that stops while the UI loop is blocked in the flash worker is not seen. The hang reset is therefore skipped while an OS upgrade is in progress **[D]**:
the anchor `cf.midi.site.os_upgrade_state_lit` (`devices/fw/midi.toml`) is the address literal of the stub `FUN_40120bcc` (DT2 1.17; DN2 1.12 `0x40122574`) that returns the
receive state object (`0x403a0ed0` / `0x4038bec0`). The byte at +0 is 0 when idle and 1 to 3 from the stream header on (the parser `FUN_40120bd4` sets 0, 1, 2 and 3; a bad
sequence or checksum sets 0 again; finding 75). `dlog.rs` writes the hang record only if that byte is 0 when the wait ends (`os_upgrade_active` in `lib.rs`); the stop stays in the log
(`DSP_STOPPED`). The state is read statically: it is not run in the emulator with a DSP that stops. If a stream is cut after its header, the byte stays non-zero until the
next boot, and a DSP stop in that time only logs. If the anchor does not resolve, the runtime acts as before.

## 5a. Runtime panics

**[D]** The panic handler of the runtime's Rust code (`lib.rs`) used to be `loop {}`: a panic froze the device with no record. It now calls `crash::panic`,
which writes a panic record (kind 3, the same keep-area record as the hang) and soft-resets. Our toolchain strips the file and line of a panic (the `Location` constants hold
neither), so `PanicInfo` is not used. The record holds `W_A7` (the stack pointer in the handler), 32 stack words from it (bounds-checked like the CPU crash record), the name
`runtime`, the last 8 events and no PC. The return addresses in the stack lead to the panicking code: in the `CMD_FAULT` test the 32 words hold the return into the panicking handler and into `dkrt_midi_rx`, the SysEx dispatcher. A call to a function that does not return (`panic_bounds_check`) is often the last instruction, so its return address is the first byte of the next function: look up the address minus 2. Naming them from the host is open (docs/plans/sdk-api.md, "Later").

Rules: only the first fault writes (the `IN_EXC` guard, shared with `dkrt_exc`); the settings store is not touched, because the panic may be inside it. The device resets only
when the boot is not already safe mode (no reset loop) and no OS upgrade is running (`os_upgrade_active`); otherwise the record is sealed and the CPU stays in `loop {}`.

At the next boot `at_boot` logs `FAULT_FOUND` (kind 3, second argument the stack pointer), the `CRASH_STACK` events and the last events, and keeps `dk/fault` (kind 3, name `runtime`).
Safe mode reason `SAFE_PANIC` (5), after `SAFE_DSP` and before the boot counter rule; a panic does not add to the boot counter. ABOUT shows `SAFE: CPU PANIC`, `FAULT: CPU PANIC`
and `IN RUNTIME` (no place row). `dklog crash` prints `CPU PANIC in runtime` and the stack.

Dev trigger: the SysEx command `CMD_FAULT` (0x23), body byte 0 = 1, runs an out-of-range index (a real `panic_bounds_check`). Other values give `E_PROTO`.

## 6. Tests

Emulator, `tests/regression/runtime/fault-trace.test.ts` (Vitest `runtime` project; the daemon runs a stopped DSP at about 50 blocks a second, so a stop takes about 5 minutes
of wall time; the tests wait in DSP blocks, not in seconds): **[D]** an effect on the send route that stops the DSP gives the hang record, safe mode `DSP STOPPED`, `dk/fault`
(kind DSP, site FX send, core step CALL, module step 2, module `dyn-fx-hang`), the `FAULT_FOUND` and `SAFE` events and the last events of the boot before (DT2 and DN2); the
ABOUT rows are read too on both devices (`SAFE: DSP STOPPED`, `FAULT: DSP STOP`, `IN DYN-FX-HANG`, `FX SEND STEP 2`). A machine that stops the DSP (DT2, `dyn-dsp-hang`) gives
site voice, core step CALL and the machine type in `dk/fault`. Effect faults give `FX_FAULT` and `FX_DISABLED` with the module's source (DT2, DN2). A normal send effect runs 9000
blocks with no stop and no reset (DT2, DN2). A CPU crash gives `dk/fault` kind CPU with the PC of the crash record (DT2). `CMD_FAULT` gives a panic record, safe mode `CPU PANIC`, `dk/fault` kind panic and the ABOUT rows (DT2, DN2). The test sends it once (`quick`): a retry of the unanswered request reached the safe boot and panicked it again, which by the rules above does not reset. **[C]** An earlier version of this section said the DT2 ABOUT page could not be opened in the safe boot. The cause was the test: its keys were timed in DSP blocks (100 ms hold), and the DT2 UI did not take them; with 300 ms wall-clock taps (as the input-adapter test) SETTINGS > DIGIKIT > ABOUT opens in the safe boot and shows the rows. The DIGIKIT menu is reachable in safe mode on the DT2 (the first row is `SAFE: DSP STOPPED`, then ABOUT, HEALTH, MEMORY, MODULES).

Unit tests: `dkrt-sdk` `fault.rs` (record and row text), `tests/digikit/test_dklog.py` (decoding), `tests/patchkit/test_dsplink.py` (the word slots),
`tests/patchkit/test_fx.py` (the marks in both records, the fault words), `tests/patchkit/test_dsp_voice.py` (the step word cleared before a call),
`packages/dk-session/tests/fault.test.ts` (the record decoder and rows, in `packages/dk-session/src/fault.ts`). Fixtures: `tests/fixtures/modules/dyn-fx-hang`, `dyn-fx-nan`. The fixture `dyn-fx-hang` uses the `DK_STEP` macro of `sdk/asm/dk_trace.inc` and defines `DK_TRACE` itself, because the test needs the steps in a build without `--trace`.
