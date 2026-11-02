# Scenario port map: Python call to TypeScript helper

Map for porting the Python device scenarios (`tools/patchkit/pk_dkrt_boot.py`, `pk_digikit_scen.py`, `pk_digikit_upload.py`) to Vitest. Paths are under `tests/regression/` unless noted.

- EXISTS: use the named TypeScript function.
- NEW: added in `runtime/card-kit.ts`.
- MISSING: not provided yet; the last column says what it needs.

In TypeScript a helper that reads guest memory takes `(session, plan, ...)`, where `plan` is the `RuntimePlan` of the patched image (`runtime-plan.ts`). A Python `rig` is a `PatchedSession` (`support.ts`: `session` and `dk`).

| Python call | TypeScript helper (file) | Status / note |
| --- | --- | --- |
| `rig.put` (upload a file) | `dk.put(path, data)` (`@digi/dk-session` DkSession) | EXISTS |
| `put_status` | `statusOf(() => dk.put(path, data))` (`runtime/card-kit.ts`) | NEW. Returns the number from the generated STATUS and LOAD_STATUS tables; other errors are rethrown |
| `Rig`, `Env`, `common_checks`, `common_checks_upload`, `wait_reset` | `cardRig` / `cardRigs` (`runtime/card-rig.ts`): `boot`, `load`, `pausedBoot`, `ready`, `after`, `records`, `storeWrites`, `waitReset`; options `uploads`, `storeExtra`, `relaxFs`, `dsp` | EXISTS. One fixture for settings, round trip, equiv, debug, card, crash, safe mode and upload |
| `Rig.note`, `pcm_check`, `pcm_pin` | `playNote`, `pcmMatches`, `pcmWindow`, `readFloatWav` (`runtime/fx-live.ts`) | EXISTS |
| `rig.ls` | `dk.list()` | EXISTS |
| `rig.info` | `dk.info()` | EXISTS |
| `rig.modules` | `dk.modules()` | EXISTS |
| `rig.capacity` | `dk.capacity()` | EXISTS |
| `card_info` | `cardInfo(session, plan)` (`card-kit.ts`) | NEW. camelCase fields (`firstNonblank`, `refusedRange`, `stateName`) |
| `boot_info` | `bootInfo(session, plan)` (`card-kit.ts`) | NEW. `uiReached`, `allResults`, `dspWait`, `reasonName` |
| `crash_info` | `crashInfo(session, plan)` (`card-kit.ts`); three-field view `readCrashSeen` (`runtime/runtime-menu.ts`) | NEW |
| `probe_write` | `probeWrite(session, plan, sector, nbytes)` (`card-kit.ts`); `E_RANGE`, `E_CARD_RO` | NEW |
| `post_raw` | `postRaw(session, plan, cmd, data)` (`card-kit.ts`); `CMD` | NEW. Same mailbox loop (200 x 20 ms) |
| `rig.post` (text argument) | `postRaw(session, plan, cmd, Buffer.from(text))` | NEW |
| `set_selection` (mailbox) | `setSelection(session, plan, paths)` (`card-kit.ts`) | NEW |
| `rig.select` over USB | `dk.select(paths)`; with upload and restart: `installModules(dk, modules)` (`runtime/live.ts`) | EXISTS |
| `sel_records` | `selRecords(session, cardImage, device, overlayFile)` (`card-kit.ts`) | NEW. `recordsIn` is the pure part |
| `make_record`, `parse_record`, `current`, `records_in`, `write_record_sectors` | `encodeRecord`, `decodeRecord`, `currentRecord`, `recordsIn`, `writeSectors` (`card-kit.ts`) | NEW. Ported to TypeScript: the plusdrive CLI writes only seq 1, counter 0, no flags |
| `rig.restart` | `dk.restart()` then `dk.waitForBoot()`; `session.restart()` (`@digi/test-support`) for the emulator reset; mailbox `CMD.RESTART` via `postRaw` | EXISTS |
| `rig.wait_reset` (guest crash soft reset) | none | MISSING. Needs `cpu.reset` and `firmware.loaded` event polling on the daemon (`session.client`) |
| `rig.note` (play TRIG 1, return PCM) | `capture`, `compare` (`runtime/fx-live.ts`), `tap` (`runtime/fx-session.ts`) | MISSING for the machine ladder: a TRIG helper that returns PCM for a WAV input |
| `rig.select_type` / `rig.select(hint)` (panel machine list) | `pickSine`, `selectSine` (`runtime/fx-live.ts`, `fx-session.ts`) | EXISTS for FX; MISSING for machine lists (needs the `M.select` key script ported) |
| `rig.status` | `session.status()` | EXISTS |
| `rig.frames` | `frames(session)` (`runtime/fx-live.ts`) | EXISTS |
| `rig.run_ms` / `s.run_ms` | `advance(session, ms)` (`runtime/fx-live.ts`) | EXISTS |
| `rig.w(addr)` | `word(session, addr)` (`runtime/fx-live.ts`) | EXISTS |
| `rig.dyn_type` (`DKRT_ASSIGN`) | none. `fx-live.ts:317` reads `DKRT_ASSIGN` for FX | MISSING: port `assign_table` (3 header words, 8-word map, 3 x 64 machine words) and `M.machine_hash` |
| `rig.voice_rows`, `machine_listed` | `rows(session, plan, slots)` (`runtime/fx-live.ts`) for FX rows | MISSING for the DSP voice registry |
| `c.card(...)` | `buildCard(device, opts)` (`card-kit.ts`): `modules`, `selection`, `seq`, `counter`, `flags`, `stableBlocks`, `map`, `recs` (`'torn'` allowed), `base`, `foreign`, `syx` | NEW. No content cache (the Python cache key); a test names its card with `name` |
| `Env.pack` / `synth` | `patchkit(['pack', ...])` (`runtime/patchkit.ts`); `moduleArtifact(device, id)` (`runtime/live.ts`) | EXISTS (`synth-dkm` through `patchkit`) |
| `build_card` / `plusdrive build` | `buildCard` (CLI `plusdrive build`) | NEW |
| `plusdrive store ls` | `storeList(cardImage, device)` (`card-kit.ts`) | NEW. Header line and `{path, firstSector, size, sha256}` |
| `plusdrive store ls --cat`, `export` | `plusdriveBinary()` (`card-kit.ts`) with `execFile` | MISSING as wrappers; the binary path is exported |
| `card_after(c, base, overlay)` | `applyOverlay(base, overlay, out)` (`runtime/write-safety.ts`) | EXISTS. Get the overlay with `rpc(session, 'card.overlay', {path})` (`fx-live.ts`) |
| `M.sym` | `planSymbol(plan, name)` (`runtime/runtime-plan.ts`) | EXISTS |
| `_poke_u32` / `emu.poke32` | `pokeWord`, `pokeKnob(session, plan, name, value)` (`card-kit.ts`) | NEW. Wraps `mem.cf.poke` |
| `common_checks` | `runtimeSafety`, `overlayWrites`, `runtimeWatchEnv` (`runtime/write-safety.ts`), `expectNoGuardHits` (`support.ts`) | EXISTS for the default and FX policies; MISSING: `firmware_areas` / `store_extra` as scenario arguments (only the named `SafetyPolicy` values) |
| `common_checks_upload` | `runtimeSafety` plus `overlayWrites` | MISSING: the upload rules (project writes allowed, store sectors written by the transfer) are not a policy yet |
| `M.pcm_check` / `pcm_pin` | `capture`, `compare` (`runtime/fx-live.ts`) | MISSING for pins: `tests/scenarios/*.json` pin reading needs Em's approval to change; no TS reader yet |
| `decode_tables` / `diff_tables` | none | MISSING: needs the `pk_digikit_scen.py:490-610` table layout ported (reads of firmware tables through `word`) |
| `pk_digikit_debug.events` | `dk.logPage(from)` (DkSession); `runtimeEvent`, `eventLine` (`@digi/dk-session` runtime-log) | EXISTS |
| `pk_digikit_debug.small_ring` | `readSmallRing(session, plan)` (`runtime/runtime-menu.ts`) | EXISTS |
| `pk_digikit_debug.about_rows` | `readAboutRows(session, plan)` (`runtime/runtime-menu.ts`) | EXISTS |
| dklog event decode (`tools.device.dklog_kinds`) | `runtimeEvent`, `fieldsOf`, `moduleNames`, `eventText` (`@digi/dk-session` runtime-log) | EXISTS |
| `rig.deliver_exceptions` | `RuntimeBaseline.deliverExceptions` (`@digi/test-support` session options); `rpc(session, 'cpu.exceptions', ...)` if needed at run time | EXISTS (option); check the method name before use |
