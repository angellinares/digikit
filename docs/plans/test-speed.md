# Test speed (plan, 2026-11-09)

Goal: the merge gate and a module's own checks finish sooner, with less work rather than more parallel work, and the MacBook stays usable.
Commands and tiers: [testing](../TESTING.md). Test layout: [test architecture](test-architecture.md). Build and cache work: [test-build workflow](test-build-workflow.md).
This plan replaces the gate-speed plan of 2026-10-30, whose router (`verify.py`) was deleted. Its open items are carried over below.

## Measurements

**[D]** One merge gate, PR 275 merged with main (lane `pr-275`, 2026-11-09, M3 Max, 6 emulator slots, another session's device run at the same time).
Durations come from the Nx terminal outputs (vitest `Duration`, nextest summary, `runtime-phase` records). They vary 2 to 4 times between runs with load.

- Device tests are about 17,600 of 19,000 task-seconds (52 `test-device` tasks, 10 `test-conformance`). Rust tests, smoke, packs and host tests are the rest.
- Almost every test boots or reloads its own emulator: about 380 in the gate. Under load a reload takes 25 to 40 s. A snapshot hit is documented at 0.2 to 1.5 s.
- Reload-bound files: `bootscreen` 1,139 of 1,171 s (32 reloads), `introblank` 777 of 844 s (24), `card-safety` 364 of 435 s (29), `settings` 243 of 412 s (28).
- Body-bound files: `shot-dn2` (632 s for one boot), `fx-track-route` (1,303 s; ten former tests in one, which timed out at its 600 s wall limit on DN2).
- The 6 slots were full for most of the device phase, with each emulator at 100 to 180 % CPU. Average concurrency was 3.6: the last 15 minutes ran three long files alone.
- What a single-file change invalidates (declared inputs, not observed reads):

| Change | Device tasks invalidated (of 63) | Estimate |
| --- | ---: | ---: |
| `modules/machines/pluck/src/dsp/pluck.s` | 1 | 3 task-min |
| `modules/machines/pluck/src/module.c` | 2 | 4 task-min |
| `sdk/sdk.toml`, `sdk/include/dk/gen/machine.h` | 63 | 293 task-min |
| `modules/core/runtime/src/rs/src/loader.rs` | 63 | 293 task-min |
| `dsp-voice.s`, `params.s` (core adapters) | 62 | 291 task-min |
| `devices/digitakt-ii.toml` | 63 | 293 task-min |
| `tests/regression/runtime/fixtures.ts` | 41 | 204 task-min |
| `packages/patchkit/src/apply.rs` | 63 | 293 task-min |

Conclusions: module work is already scoped well. A core change reruns every device test. Device time is mostly emulator reloads, and the machine is
already full during the device phase, so speed must come from less work per test, not more slots.

## Steps

Highest payoff first. Each step states its proof.

1. [x] **Zero DSP by default** (merged in #361, 2026-11-09; the paired before/after proof below is still open). Device tests run ColdFire only unless they need the DSP (`--dsp zero`: about 2.2 times
   real time; coupled is about real time on DT2 and 0.62 to 0.66 times on DN2 at load, finding 18). Proof: per-file `runtime-phase` totals before and after on the same files, one paired run.
   - [x] Emulator ceiling: one core per emulator (`tools/nx/resources.mjs`), so 12 emulators and 12 Nx tasks on a 12P+4E machine. The idle-core
     admission still stops new emulators while coupled runs load the machine. Half-slot weights were built and dropped: they needed polling of the
     process list and rebalancing for the same result.
2. [x] **Reload cost.** A slow reload is a snapshot miss, a cold boot of the card inside `firmware.load` ([finding 118](../findings/118-device-test-reload-cost.md)).
   Snapshots lived per run dir and were pruned to 8, so a file rarely found an earlier one. One shared store per daemon build and device replaces them.
   - [ ] A cold gate still boots each distinct card once. Reduce distinct cards where the subject is not the boot (write state through the protocol).
3. [ ] **Wait on events, not fixed times.** The idea is the deferred "event-driven test observer" of the [test-build workflow](test-build-workflow.md);
   its constraints apply (one log sink, safe-mode fallback, cursor, epoch and loss semantics, per-file isolation, fixed waits only where the subject is a duration).
   - [x] Stage 1: `settleUi` is deleted. **[D]** It waited a fixed 45 s of emulated time on DN2 and 12 s on DT2 (`shot-dn2`: 262 of 697 s in a loaded gate),
     after readiness was already established. Without it the `shot-dn2` body takes about 215 s instead of about 390 s.
   - [ ] Stage 2, first job (partly built, no new module): `digikit/remote` logs `INPUT_TAKEN` (kind 41, debug level) when core/input handed a key or turn to the firmware
     decoder, and the host helper `events.until(kind, predicate, {emulatedMs})` (`packages/test-support/src/events.ts`, `listenEvents` and `remoteTurnTaken` in
     `tests/regression/runtime/events.ts`) waits on it with a cursor over sequence numbers (new boot resets it, a gap fails). `setSmp` of `shot-dn2-supersede` turns
     through the remote and waits on the event; a failure lists the detents taken and the last events. 4 of 4 runs passed (about 1 m 52 s each, same as before).
     DT2's `shot-dt2` keeps the panel input: its `test/stock-pool` fixture holds the remote commands 0x38 to 0x3b. `PARAM_CHANGE` (kind 42, built 2026-11-10, [finding 119](../findings/119-param-change-observer.md)): `digikit/debug` logs a changed Sound word of the selected track (two new anchors, one runtime import); a knob turn is proven on both devices, but the SMP turn of `setSmp` does not change that array, so `setSmp` does not use it (module parameters such as SMP are stock parameter rows outside the Sound array). Not built: the "ready for input" wait after an install, and the `tap` helper (a key event says the decoder took it, not that the UI redrew).
     **[D]** `main_ui_reached`
     does not mean ready for input: `shot-dn2` failed in its SMP step in 9 of 19 runs with a wait on `main_ui_reached` after the install and in 0 of 12 without it
     (the SMP word reaches 0x100 and does not move for 43 more detents). **[O]** Why the word stops moving: a guess is that the note ends before the knob reaches the target; 6 passing runs on 2026-11-10 showed the voice sounding to the end of every turn (margin about 1.7 s), no failure reproduced ([handover](../handovers/HANDOVER-2026-11-10-test-speed.md)).
   - [ ] Stage 2, observer module: a test-only module loaded first reports events the harness cannot read (module registered, machine assigned,
     note reached the DSP, card write done, input consumed) through the runtime log sink, with a boot-record fallback. Host side:
     `events.until(kind, predicate, {emulatedMs})`; a missing event fails with the last events seen. The remote control module stays the input path.
4. [ ] **Device inputs from built bytes.** Device and conformance tasks hash the patched image and `.dkm` content instead of SDK, patchkit and device
   file sources. A change that does not change the bytes (a comment, an unused constant, a test-only patch set) reruns nothing. Real core changes
   still rerun everything. Owner: the Nx lane. Proof: `nx show target inputs` and one invalidation check per input kind.
   - [x] Generated device includes hold only the constants each module lists in `uses` (runtime-and-services.md section 5.8).
     Test selection is unchanged: the daemon, the patchkit CLI and the device-test inputs hash the whole profile.
   - [ ] A profile edit selects only the device tests whose built bytes change (needs the items below).
   - [ ] Carried over: skip audio scenarios whose image, module objects, daemon and test code did not change.
   - [x] (merge tier selects them with `affected`; `full` runs all) **[D]** The merge tier ran `test-device` and `test-conformance` for every owner and relied on cache replay. After main moves, nothing is
     cached for the new main, so the next PR gate reruns the whole device suite, whatever the PR changed. Seen 2026-11-09: PR 362 (a Python test
     helper and the formatting of one Nx file) started the full device suite. Fix: select device tasks whose inputs differ between base and head, or
     warm the cache on main once after each merge.
   - [x] **[D]** Tool files that are declared inputs select heavy runs on any edit: a prettier-only change to `tools/nx/coupled-inputs.mjs` selected
     `fx`, `fx-no-aot`, `fx-dt2`, `tier-quick` and `dt2-tier-quick`. Inputs of tool files should be what they compute, not their source text.
5. [x] **Core-change policy.** (B), decided 2026-11-10. A change runs all device tests of the projects it edits. Device tests it reaches only through shared inputs
   run only the subset marked in `metadata.mergeTests` of the owning project.json: other files get a narrow merge cache key (own folder, test code, loaded packs), so Nx replays their last result. `test-device-all` (run by
   `mise run test:full`) runs every file with full inputs. Batch point: run `test:full` before each push or PR to main.
6. [ ] **Gate output and history.**
   - [x] One line per finished task (project:target, pass or fail, cache status) while the gate runs: `--output-style=static-failures-only`. Nx prints no seconds there; the durations come with the history report at the end of the run.
   - [x] History: Nx's own task history database already records project, target, hash, status and start and end of each task. `tools/nx/gate-history.mjs` (`mise run test:history`) reads it: slowest tasks, recent failures, slowdowns. It is per worktree and written at the end of a command. Nx already uses the database's average durations to order tasks, so longest-first needs no code. Flaky tests and timeouts: not built; Nx prints its own flaky-task message.
   - [ ] Test timeouts in emulated time or firmware state, as AGENTS.md requires for waits. A wall-time limit fails healthy tests under load.
7. [ ] **Device test audit.** For each device test, one of: needs the emulator; can be a host test (Rust runtime unit test or TS); duplicates another
   test. Start with the reload-bound files (`card-safety`, `settings`, `bootscreen`, `introblank`, `memaudit`, `capacity`). Keep one device test per
   boundary. Split composite tests such as `fx-track-route` so they run in parallel.
   - [ ] Carried over: move ColdFire-only checks to the CF-only rig (fxroute, fxroutes, fxpage, fxpersist, settings).
   - [ ] **Rule: a shared SDK component is tested once; a module tests only its glue.** Component logic (ordering, levels, cursor, keys) in host
     tests; the component on a device once, in the `ui-demo` fixture on zero DSP (keys reach it, it draws, it calls the file and pool API); each
     module keeps one thin device test of its own glue, on zero DSP where the glue is checked through API state (`param_get`, pool state) and not
     through DSP memory or framebuffer hashes. Audio stays in the module's playback test.
     - [x] First case: `shot-picker.device.test.ts` (coupled DSP, 25 min timeout, 312 s in the 2026-11-09 gate). It re-checks the shared picker
       (`sdk/src/picker_logic.c`, `sample_picker.c`, `chooser.c`; host tests in `modules/core/adapters/ui/host/tests/picker.rs`): folder-first
       order (it reimplements `pickerOrder`), levels closing to the page, cursor on slot 0, NO and YES. Move those to the host tests and ui-demo;
       keep "a click on SMP opens shot's slot list and a chosen file sets that pool slot" on zero DSP; "the sample plays" is in `shot-dt2`.
       **[D]** Done: the picker logic (order, levels, cursor) is in `picker.rs`; shot keeps one glue test that runs the real hooks: click, slot list, slot 0 and 1,
       replace of a filled slot (new token), NO one level at a time, held TRIG (E_HELD: SMP stays, the file loads), press with a turn and an unused C turn (no
       list). State is read from the pool API and the SMP word; one screen hash tells page, list and browser apart. It stays on coupled DSP: shot's pack
       carries its DSP part and fails with E_DSP_XFER (-35) on zero DSP. Not done: the picker in `ui-demo` (the fixture has no picker or pool client).
       Time: 312 s in the gate before, 2m15s for the Nx target now.
       Waits after `settleUi`: `sample-playback` (UI after a restart), `stock-pool-borrow` (stock slots loaded) and `stock-sampler-lifetime`
       (`cf.main_ui_reached`) wait on firmware state, each in its own file. `installModules` has no wait (see step 3, stage 2).
     - [x] Then search for other cases: device tests that assert behaviour of `sdk/src` components (pages, chooser, picker, lists, knob handling)
       or of core adapters that a host test or another device test already covers.
       **[D]** Done as a static audit: [ledgers/device-test-audit.md](ledgers/device-test-audit.md). The candidates are listed there for decision.
8. [ ] **Per-test emulator coverage.** After steps 1 and 2 (cheaper runs). Record the ColdFire and SHARC PCs each device test executes and map them
   to runtime and module functions through the patch plan and symbols. `digi-emud` already writes SHARC PC counts ([instrumentation](../INSTRUMENTATION.md),
   engine coverage files); the ColdFire side is not checked yet. Uses:
   - overlap for step 7: tests whose functions are a subset of another test's are candidates to merge or delete (a list for review: the same code can
     be checked by different assertions);
   - selection for step 5: a changed function selects the tests that execute it, instead of every test that declares the image as an input;
   - host gaps: `cargo llvm-cov` on the runtime unit tests shows logic that only a device test reaches, the first candidates for host tests.
   Vitest's own coverage is not used: it covers only the TS harness. Proof: per-function test lists for one core adapter, checked by hand against its tests.
9. [x] **Vitest 5.** All packages are on 5.1.0. `singleFork` became `fileParallelism: false`, the default `pool: 'forks'` and `extends: true` were removed, and the root
   package is `"type": "module"` (Vitest 5 loads the config natively). Tags for the DSP mode and `--last-failed` for reruns stay for later.
10. [ ] **Python device scenarios to TS** only where they boot an emulator: module `equiv-*`, the pluck menu flows, `pages-dyn-pluck`, `upload-rust`
   ([ledgers](ledgers/)). Other Python tests stay; moving them buys no speed. `py_select` is a second, coarser selector next to Nx (file patterns and
   the import graph); each ported or deleted scenario takes work away from it.
   - [x] **[D]** Narrow first (#363): `tests/digikit/scen_cases.py:9` imports `pk_dkrt` only for `DEVICE_SYX` (two firmware file names), so any edit to
     `tools/patchkit/pk_dkrt.py` selects every `tests/digikit` scenario (equiv, menu, upload). PR 362 (one line in `pk_dkrt.py`) ran
     `test_equiv_dt2` 493 s and `test_menu_dt2` 528 s for it. Move `DEVICE_SYX` to `tools.firmware.config`; check which edge brings `pk_dkrt` into
     `test_machine_conformance.py` and `test_fx_conformance.py` and cut it.
   - [x] Port next: the `equiv-dyn-pluck`, `-analog`, `-bd`, `-pd` scenarios as rows of `equiv.device.test.ts` (now one file per module, shared runner `tests/regression/runtime/equiv.ts`; only `equiv-dn2-set` is ported;
     the PCM pins are already approved, a port reuses the same values). Then `test_equiv_dt2.py` and `test_equiv_dn2.py` go.
     Done 2026-11-10: analog, bd, pd, pluck (both devices) and equiv-mixed (DT2) are rows; `test_equiv_dt2.py` and `test_equiv_dn2.py` are deleted.
     The DN2 pluck reference has no runtime: `cardRig` option `plain`. `equiv-dyn-pluck.toml` stays as the input of `tools/devtools/sharc_matrix.py`, so `run_equiv` in `pk_digikit_scen.py` stays with it. Then the menu flows
     (`zero/menu.device.test.ts` covers only the empty-store menu) and the `pk_dkrt` boot proof as a safe-mode-style test.
   - [x] Module-subject Python tests live with their module (29 files moved out of `tests/patchkit`, 2026-11-10); `py_select` selects them by folder;
     a patchkit edit no longer runs them.
   - [ ] Zero DSP where the subject allows: the boot, restart and safe-mode proofs of `test_patchkit_dkrt*.py` and the C-demo test. Equiv, menu and
     the conformance rigs keep the DSP (audio or SHARC subject).
   - Keep in Python (host or RE tooling): `test_machine_conformance.py`, `test_fx_conformance.py`, `test_pk_current.py` and the static parts of the
     patchkit tests.
11. [ ] Carried over: smoke per-cell results, so a rerun repeats the failed cells only.
12. [ ] Carried over: runtime-resident DSP code at 0xbec4xx is still interpreted on DN2 (needs a JIT or a runtime-aware gen).
13. [ ] **Test quality: mutation testing**, later and occasionally, on host unit tests of logic-heavy code only (runtime loader, store format and
    settings record; patchkit), with `cargo-mutants`. It checks that tests assert our behaviour. Never on device tests: it reruns the tests once per
    mutant. Not a speed step.

## Not planned

- `@nx/vitest` inferred targets and its per-file splitting: our plugin already makes per-file device targets with the emulator and firmware inputs; the
  split pays off mainly with distributed agents.
- Vitest sharding: per-file Nx targets already split the work on one machine.
- Vitest UI: device tests are too slow to watch interactively.
- A shared emulator pool as an Nx continuous task: the test architecture plan rejected a global pool; step 2 gets the gain with snapshots.
- More emulator slots without step 1: the machine is already full during the device phase, and longer tests then hit wall-time limits.
