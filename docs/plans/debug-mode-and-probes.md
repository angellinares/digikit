# Debug mode and hardware probes (plan, 2026-10-31)

Goal: evidence from the real device to fix emulator gaps, with a small runtime. Debug logging becomes a loadable module, `digikit/debug`; the
runtime keeps fault capture. Agreed with Em on 2026-10-31. Sources: the sampler gap map (the 2026-10-31 hardware-observation handover on the sampler branch), [debug finding](../findings/46-digikit-debug.md), [dsp-link](../../modules/core/adapters/dsp-link/README.md),
[capabilities](capabilities.md), [workbench log](workbench-device-log.md).

## Rules

- The runtime keeps only what field faults need: the fault record, `dk/fault`, safe mode, the DSP hang watch and a small error and boot ring.
- A debug feature costs at most one pointer test on a hot path when `digikit/debug` is not loaded.
- Kept probes are test modules or features of `digikit/debug`. Hooks for one experiment go into a separate probe image, never into the default
  `[patch_sets]`.
- Hardware answers fixed facts: instruction results, reset values, interrupt masks, cacheable ranges, cycles per block. Races are checked in the
  emulator, where all interleavings can be forced.
- DT2 evidence holds for DT2 only.
- No flash until the change works in the emulator. Phase 1 needs one flash.
- Branches: a fix to code that exists only on the sampler branch goes on a branch from it and merges back. A fix to code on `main` is its
  own PR to `main`.

## Phase 0: static checks and measurements (no device)

- [x] IRQSTATEN **[D]**: no code in DT2 1.17 or DN2 1.12 reads or writes `0xfc0cc034` (byte scan of all sections; no base-register form). The
      firmware still depends on its enable bits: the polled driver waits for TC with IRQSIGEN cleared, and the ISR tests the CC, TC and error
      bits. Because nothing writes it, it holds the power-on value `0x117F_013F` whether RSTA resets it or keeps it. The emulator uses that value
      after RSTA; no device read is needed. The fix goes on the sampler branch, with an update to its finding 107, after a second byte check.
- [x] Runtime size (`runtime.dko.json`, 2026-10-31): text 110,290, data 3,547, bss 671,140 bytes, mostly the module arena (512 KiB), the stage
      (64 KiB) and the upload buffer (36 KiB). Log code is 4.2 KB, the log ring 16 KiB, the LOG, PEEK and LOGCFG handlers 0.9 KB. The split
      saves about 15 KB; its main value is that debug and probe code lives outside the runtime. Parts of `dlog.rs` are not logging and stay:
      module-source tracking (the owner of command ranges), the DSP heartbeat watch, step addresses and guard faults.
- [x] Probe path: a module cannot import `dl_send` or `dl_diag` (they are not in `sdk/sdk.toml`). Service modules run 30-32 times a second,
      so a handshake in slot 0 moves only 15-30 words a second. The per-block hook `dl_tick` (interrupt context, 1500 Hz) sees the previous
      reply each block. Phase 1 therefore needs a small runtime change and one flash.
- [x] DN2 patched runtime speed (commits `37e3dbed`, `55de4b82`; DN2 interpreted 6.07 M to 2.57 M instructions per audio s; the two-core PGO
      profile moved to [two-core PGO](two-core-pgo.md)). Frozen 3 s replay (2026-10-27, DN2 1.11 port): stock 1.128, runtime 1.451 DSP s per audio s; 17.9 M
      Measured before the fix: instructions interpreted 17.9 M against 1.9 M on stock. 68.7 % of them are `core/dsp-voice`: `_dv_scan` walks the type table for all 16 slots
      every block, also when no module is registered. 11.7 % are the barred hook helper `FUN_b8251b`, 2.6 % dsp-link. Steps: (1) dsp-voice
      skips the scan when no module type is registered and otherwise looks only at slots with a module type (one compare when unused, which
      also follows the capability rule on the device); (2) a fast-tier root at `_dv_entry`, with its data on its own page; (3) a PGO profile
      for the two-core build (done 2026-11-01: about 18% less DSP-thread CPU on DT2 and DN2; the DT2 root change saves 9-12% on the patched
      runtime; see finding 68). Fallback roots for barred stock code give about 5 %. Own PR to
      `main`.

## Phase 1: probe kit (one small runtime change, one flash)

- [x] Runtime: one import, `dl_set_tick_cb`, that sets a callback pointer in `dl_tick`. The callback gets the previous reply record each block.
      Off: one pointer test per block. Proven in the emulator before the flash.
- [x] `test/sharc-probe`, an effect module (it runs every block): it runs a fixed sequence of RECIPS and RSQRTS over all 128 table indices plus
      boundaries, signs and special cases under each MODE1 setting, and returns one framed result word per block (magic, sequence, data)
      through word 25. It stays as a test module and runs on DN2 too.
- [x] A ColdFire probe service module (it becomes `digikit/debug` in phase 3): collects the words in the callback with a sequence check and
      returns records through its own SysEx range (reserved in `sdk/sdk.toml`). About 1500 words a second. It stays `digikit/probe` (phase 3 decision).
- [x] On the device: identity (firmware, runtime hash, loaded modules, a screenshot, one remote-control roundtrip), then the seed tables and
      DSP cycles per block from the health adapter (real headroom). Done on DT2 1.17 (2026-10-31): two device bugs found and fixed (service slots;
      the module step word, [finding 101](../findings/101-module-step-word-on-hardware.md)); the RECIPS and RSQRTS table complete.
- [x] One command for a capture (2026-10-31): `mise run dk:probe -- --port NAME --modules test/dsp-facts --capture OUT.json [--compare REF.json] [--keep]` (`tools/device/dkprobe_run.py`)
      packs the probe modules with `digikit/remote` and `digikit/probe`, uploads what changed, selects, restarts, checks MOD_LOAD, MOD_DSP and the log for faults, captures,
      compares and selects the previous modules again. `--spawn dt2|dn2` does the same on a private emulator daemon.
- [x] `test/dsp-facts` (`tests/fixtures/modules/dsp-facts`, items in `tools/device/dkprobe_facts.py`): cycle counts with EMUCLK (NOP, ALU, MAC, loops, branches,
      RECIPS and RSQRTS with Newton steps, L2, L1 and DDR loads, an L2 cache-conflict pair), FIX, float and fixed-point edge cases with ALUSAT clear and set (AI, AV, denormals),
      core and cache registers (SHL1C configuration and the eight range registers, revision, interrupt masks) and the cycle counter at consecutive calls. A run is 143 items,
      0.35 s, so one capture holds about 47 runs. Proven in the emulator on DT2 and DN2; the device run is open.
- [x] Device run of `test/dsp-facts` (the command is in `tests/fixtures/modules/dsp-facts/README.md`), then: fix the emulator where the values differ, record the results in `docs/findings/`. Done: 59 of 59 value items match after the fixes
      ([finding 103](../findings/103-dsp-facts-on-a-digitakt-ii.md)).

## Phase 2: emulator fixes

- [x] Seed tables in `sharc_core`; remove `approx_recips`; regression tests from the captured tables; one batched core regeneration ([finding 102](../findings/102-recips-rsqrts-seed-tables.md)).
- [x] Float ALU, FIX, ALUSAT and sticky rules from the dsp-facts capture (finding 103), one more regeneration.
- [x] Reply record check `SHARC_REPLY_INFLIGHT` (finding 78): flags core writes into a reply record while the SPI DMA sends it.
- [ ] An optional cache checker that flags reads of DMA-written lines without cache maintenance. Test builds only, never the real-time path.

## Phase 3: `digikit/debug` (branch `debug/digikit-debug-module`, agreed with Em 2026-11-01)

Saves about 15 KB of runtime memory (measured: 784,279 to 768,438 bytes of text, data and bss); debug code lives outside the runtime. One flash after the emulator proof.

- [x] Runtime keeps the fault record, `dk/fault`, safe mode, the DSP heartbeat watch, module-source tracking and the effect fault guards, plus a
      ring of about 32 boot and error events (about 1 KB) that feeds the crash record's last events. Every log call site stays; the call goes
      to one sink pointer. Without `digikit/debug`, error events go to the small ring and the rest is dropped (one pointer test).
      Done (2026-11-01, "Runtime: log sink and a 32-event ring instead of the 512-event log"): `dlog.rs` has the ring (32 events: error events and the boot's own kinds),
      the sink (`dkrt_log_sink_set`, which replays the ring to a new sink; cleared after the owner's `fini`), `dkrt_log_ev`, `dkrt_peek_range` and
      `dkrt_track_type` (module imports); the DSP watch, guards and snapshot moved to `watch.rs`. LOG, PEEK, LOGCFG, the debug-off flag, `dkrt_debug_set`,
      `CMD_DEBUG` and the DIGIKIT menu's DEBUG row are gone; INFO reports CAP_LOG, CAP_PEEK and CAP_LOGCFG only while a sink is set. Runtime
      text 109,624 to 108,406 bytes, data 3,547 to 2,736, bss 671,108 to 657,296.
- [x] `digikit/debug` (a service module) holds the 512-event ring (16 KiB in its own memory), the level filter (default and per module), the
      LOG, PEEK and LOGCFG commands with their numbers kept (0x20-0x22, now a service range it owns, so `dklog`, the Log drawer and the tests
      work unchanged) and the track watches; at start it copies the boot events from the small ring.
      Done (2026-11-01, "digikit/debug: the log, its levels, LOG, PEEK and LOGCFG as a service module"): `modules/tools/debug`, 1,724 bytes of file, 16,472 bytes of bss
      (the 512-event ring), host tests in `modules/tools/debug/tests/test_debug_module.py`.
- [x] Debug on or off means the module is loaded or not: delete the saved debug-off flag, the DIGIKIT menu toggle and LOGCFG op 2. The Log
      drawer's Debug switch becomes "debug module: loaded / not loaded".
      Done (2026-11-01, "Workbench and tools: the debug module state replaces the Debug switch; digikit/debug is a default module"): the drawer shows the read-only
      state and, without the module, the line "The debug module is not loaded: only crash reports are available." with the fault record; `dklog crash` falls back to
      `dk/fault`; `setDebug`, `logcfg_debug` and LOGCFG op 2 are removed.
- [x] Workbench: `digikit/debug` joins `digikit/remote` in `default_modules` of both device files; the Firmware pane and the emulator install it,
      the drop-down lists it, ticked by default.
- [x] Proof in the emulator on DT2 and DN2 (boot events, logging, levels, crash record with and without the module, `dklog`, the Log drawer),
      the routed gate, then one DT2 flash and a check on the device.
      Emulator (2026-11-01): `runtime/debug-module.test.ts` (with and without the module) and `runtime/fault-trace.test.ts` (the crash tail comes
      from the small ring) pass on DT2 and DN2. Gate: `verify --only build,desktop,host,web,runtime-fixture,module-load,python-tests,rust-tests,card,smoke`
      (the SHARC-only steps left out: no SHARC core or DSP adapter change), then the failed steps rerun after fixes ("Gate fixes: menu frame
      without the DEBUG row, store order in transfer-live, unused import, winmidi source wording"). Device (DT2 1.17, image `4a0465b00d65c475`):
      the flash installed `digikit/debug` next to the kept selection; boot events, a level change and PEEK work; without the module the device
      boots, INFO drops the LOG, PEEK and LOGCFG capabilities (caps 4095 to 4088), LOG answers E_PROTO and remote still works.
- [x] Info events of modules that load before `digikit/debug` reach its log (on the device the remote's start message was dropped because remote
      loaded first): the runtime's small ring keeps every event until the first UI frame; the debug module keeps the runtime's sequence numbers, so a
      ring overflow shows as a gap. The runtime size went from 784,279 to 768,438 bytes (15.8 KB saved) in this phase. Commit "Runtime log: the small ring keeps every event until the first UI frame".
- Decided not now: dsp-link page mode (nothing needs more than 1500 words a second yet; it changes the DSP side of dsp-link); run-time
  ColdFire hooks ([capabilities](capabilities.md) step 2); the probe collector stays its own module `digikit/probe`.

## Workbench (independent of the phases)

- [x] Log controls in the Log drawer (PR #228; Debug switch in PR #231): Debug on or off, the default level, and a level for each module. Needs a LOGCFG setter in
      `packages/dk-session`. Works with the current runtime; after the split the controls show only when the device reports LOGCFG.
- [x] Default modules (PR #228; `digikit/debug` joins in phase 3): flashing from the Firmware pane uploads and enables `digikit/remote` and `digikit/debug` in one flow. The emulator does the
      same when it loads patched firmware. A small drop-down at the firmware file chooser lists the default modules, ticked by default. Desktop
      app only, not the web app. The exact UI needs Em's OK first.

## Later

- Interrupt order traces for storage (a probe image).
- Sampler cache and reader-lifetime experiments; page mode if they need more than 1500 words a second.
- A cycle-cost model for the emulator's DSP meter from the measured cycle table (finding 103).
- [x] Digitone II bring-up on hardware (2026-11-01, OS 1.12): the patched image boots with `digikit/remote` and `digikit/debug`, both DSP
  heartbeats run, the RECIPS and RSQRTS table and the dsp-facts capture equal the Digitakt II ([finding 102](../findings/102-recips-rsqrts-seed-tables.md),
  [finding 103](../findings/103-dsp-facts-on-a-digitakt-ii.md) section 10).
- The DN2 DSP_DIAG breadcrumb reads `NONE` where the DT2 shows `HOOK_ENTERED voice=0` with no module voice type registered; the heartbeat shows
  voice 0 runs. Check where the DN2 voice code writes its breadcrumb.

## Docs to correct

- `modules/core/adapters/health/README.md` says it has not run on a device; the health adapter reported DSP load on DT2 1.17 on 2026-10-31
  (the dsp-link README was corrected in `c0cb94a3`).
