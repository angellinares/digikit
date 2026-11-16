# Test speed (plan, 2026-11-09)

Goal: the merge gate and a module's own checks finish sooner, with less work rather than more parallel work, and the MacBook stays usable.
Commands and tiers: [testing](../TESTING.md). Test layout and selection: [test architecture](test-architecture.md). Build and cache work: [test-build workflow](test-build-workflow.md).
Items 7 and 16 are inputs to T1d and T1b of [test architecture](test-architecture.md).
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

**[D]** 2026-11-10 core gate (device-constants change, cold snapshot store, ~3 cores busy elsewhere): 126.8 min, 180 tasks. Exclusive tasks ran one after another until minute 50 (no device test started before), a 5 s exclusive CLI drained the run for ~9 min at minute 61, python-tests ran alone at the end (4 min). A one-emulator-per-target trial made smoke and tier-quick 3-5x slower (smoke 19 min, tier-quick 20 min); they keep their job counts.

**[D]** History of core-touching merges (30 days, 128 merges): 12% change no built bytes (the scoped payoff of step 4), 33% are emulator-only changes, 38% change object bytes.

**[D]** Full tier of 2026-11-10 17:08 (lane `ts-slots` task history and cached terminal outputs): device tasks 984 task-min, of which 303 inside Vitest and about 595 waiting for an emulator slot.
Only 3 to 5 emulators ran at a time: admission needs about 6 idle cores (2 plus 4 efficiency cores), and proof runs of other branches shared the slots. The wrapper held Nx slots, so `rust-tests` started at minute 77 and the smoke and fx tiers at 84 to 96. The run started at load 11.6.
Top Vitest minutes: fxpages 48, runtime-tests 45, fx 31, inferred-needs 29, conformance 21 (11 modules), freqshift 19, shot 17, introblank 13. `fx-track-route-lock--dn2`: 2018 s task, 195 s test.
Zero DSP cannot load a module with a DSP part (`ZeroPeer` drops link bytes and answers zeros; `dsp.rs` waits for `DL_DONE`), so conformance, inferred-needs, module-load, fits, dynreg roundtrip and the ColdFire-only fx checks use full SHARC.

**[D]** Byte determinism: patched images and `.dkm` files are byte-identical across comment edits. The daemon is reproducible per worktree path only.

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
     decoder, and the host helper `events.until(kind, predicate, {emulatedMs})` (`rt.listen()` of the wait API, `tests/regression/support.ts`) waits on it with a cursor over sequence numbers (new boot resets it, a gap fails). `setSmp` of `shot-dn2-supersede` turns
     through the remote and waits on the event; a failure lists the detents taken and the last events. 4 of 4 runs passed (about 1 m 52 s each, same as before).
     DT2's `shot-dt2` keeps the panel input: its `test/stock-pool` fixture holds the remote commands 0x38 to 0x3b. `PARAM_CHANGE` (kind 42, built 2026-11-10, [finding 119](../findings/119-param-change-observer.md)): `digikit/debug` logs a changed Sound word of the selected track (two new anchors, one runtime import); a knob turn is proven on both devices, but the SMP turn of `setSmp` does not change that array, so `setSmp` does not use it (module parameters such as SMP are stock parameter rows outside the Sound array). Not built: the "ready for input" wait after an install, and the `tap` helper (a key event says the decoder took it, not that the UI redrew).
     **[D]** `main_ui_reached`
     does not mean ready for input: `shot-dn2` failed in its SMP step in 9 of 19 runs with a wait on `main_ui_reached` after the install and in 0 of 12 without it
     (the SMP word reaches 0x100 and does not move for 43 more detents). **[O]** Why the word stops moving: a guess is that the note ends before the knob reaches the target; 6 passing runs on 2026-11-10 showed the voice sounding to the end of every turn (margin about 1.7 s), no failure reproduced ([handover](../handovers/HANDOVER-2026-11-10-test-speed.md)).
   - [ ] Waits on an event a module logs right after boot use `listen({ fromBoot: true })` (2026-11-10, PARAM_CHANGE baseline race). Check whether the shot-dn2 SMP stall after installModules has the same cause.
   - [ ] Stage 2, observer module: a test-only module loaded first reports events the harness cannot read (module registered, machine assigned,
     note reached the DSP, card write done, input consumed) through the runtime log sink, with a boot-record fallback. Host side:
     `events.until(kind, predicate, {emulatedMs})`; a missing event fails with the last events seen. The remote control module stays the input path.
4. [ ] **Device inputs from built bytes.** Device and conformance tasks hash the patched image and `.dkm` content instead of SDK, patchkit and device
   file sources. A change that does not change the bytes (a comment, an unused constant, a test-only patch set) reruns nothing. Real core changes
   still rerun everything. Owner: the Nx lane. Proof: `nx show target inputs` and one invalidation check per input kind.
   - [x] Main part (#397): `patchkit:device-bytes` writes `dist/bytes/core-objects.sha256` (core `.dko.json` files with `source_sha256` stripped). Full-input device tasks drop image-module sources, SDK headers, dko globs, the blanket output glob and `emu-slot.mjs`; the `compiles`, `patchModules` and `pythonTools` records keep SDK and toolchain inputs. **[D]** A runtime comment, an unused SDK define and a module comment keep the hash; a used constant changes it.
   - [ ] Not achieved: profile-edit selectivity (`devices/**/*.toml` stays an input because tests read it); the narrow policy-B set is unchanged; the daemon stays on its source key (its bytes embed the worktree path).
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
6. [x] **Gate output and history.**
   - [x] One line per finished task (project:target, pass or fail, cache status) while the gate runs: `--output-style=static-failures-only`. Nx prints no seconds there; the durations come with the history report at the end of the run.
   - [x] History: Nx's own task history database already records project, target, hash, status and start and end of each task. `tools/nx/gate-history.mjs` (`mise run test:history`) reads it: slowest tasks, recent failures, slowdowns. It is per worktree and written at the end of a command. Nx 24 orders ready tasks by their historical duration, longest first, as its third sort key (`node_modules/nx/dist/src/tasks-runner/tasks-schedule.js`); a fresh lane has no history, so its first run starts in graph order. Flaky tests and timeouts: not built; Nx prints its own flaky-task message.
   - [x] No gate task runs alone (2026-11-10): builds run beside other tasks with capped cargo jobs; single-emulator gate targets take an emu-slot; multi-emulator targets keep their job counts.
   - [x] Gate run trace (#395): `NX_PARALLEL` is twice the emulator ceiling (`DIGI_NX_PARALLEL` overrides). Gate tier runs write `out/runs/gate/<stamp>/` (`nx-profile.json`, `slots-trace.json` from `emu-slot.mjs`, merged `trace.json` for ui.perfetto.dev) and print wall, task-min, slot wait against run time and the slowest tasks by run time. `gate-history.mjs --trace <dir>` reads a run; `test:history` reads the newest `-v<N>.db`.
   - [x] Gate emulator targets are cached (#396): smoke, tier-sweep, dt2-tier-sweep, pins, dt2-tone, card and fast-roots (`fixtures` stays uncached). `SHARC_GEN_DIR` and `_2` are inputs of every cached gate target, `DIGI_PK_PGO` of smoke. Replay proven (rerun 3.3 s).
   - [x] Test timeouts in emulated time or firmware state, as AGENTS.md requires for waits (#398). `status.boot` reports a cold boot; the cold DSP init wall limit (120 s) became an 800M-step cap; the watchdog runs on a second connection from `Session.start` and counts instructions only during a job or boot. Fixed wall limits became `pollEmulated` waits and about 35 per-test timeouts were deleted. Guards that remain: clock stuck 60 s (`DIGI_CLOCK_STUCK_MS`), `waitReset` 80M instructions, 300 s before the socket and for the env, plusdrive exec 30 s.
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
     - [x] Each device test file runs as one task per device (`DIGI_DEVICE`), 2026-11-10; a file's floor is its DN2 half.
     - [x] nx-tools host tests keep the plugin-logic cases; cases that re-proved declared inputs through ~30 real-workspace nx spawns are gone (2026-11-10).
     - [x] Long files split by independent group (2026-11-10): bootscreen, introblank, settings. fx-routes-panel stays one file: its four legs reload one session in turn.
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
13a. [x] **Retry a device target once on a load symptom** (2026-11-14). `tools/devtools/emu-slot.mjs` wraps every device and conformance command and
    already holds the slot, so the rerun lives there: one rerun of the same command inside the same Nx task and slot, which gives one cache result.
    `tools/nx/retry.mjs` classifies the failed run's output: `no new boot within N s`, `no new boot: not seen within N ms of emulated time`,
    `runtime lifecycle deadline exceeded`, `emulator closed the connection` and `remote release timed out`. A run with an assertion failure is never
    retried, and neither is a second failure. A retry prints `emu-slot: RETRY <target>: load symptom ...` in the task output and appends to
    `retries.jsonl` in the gate trace dir; the gate summary prints `retried (n)` with target, symptom and outcome. Proof: `tools/nx/tests/retry.test.ts`.
14. [x] **Longest device tasks first.** Dropped: Nx 24 already sorts ready tasks by historical duration, longest first, as its third sort key (`node_modules/nx/dist/src/tasks-runner/tasks-schedule.js:166-177`). A fresh lane has no history, which is a small effect at most.
15. [ ] **Machine-readable gate result.** Next to `trace.json` in `out/runs/gate/<stamp>/`, write `result.json`: the failed targets, each with its
    symptom class (from `tools/nx/retry.mjs`, or `other`), its log path, and whether a retry ran and passed (read from `retries.jsonl`). Tools and
    agents then read the result instead of the console. Proof: a host test on a fake trace dir and Nx history rows.

16. [x] **Key device tests on the bytes they load.** Device test targets are keyed on the source of the tools that build their inputs (patchkit's Rust code among them), so a patchkit refactor that changes no packed byte reselects nearly the whole device suite: S3b patched-in deletion changed 0 bytes of 102 `.dkm`, 47 core objects and 2 images (`mise run sdk:hashes`), yet the affected gate selected about 360 device tasks. Key each device target on the packed `.dkm` files, the patched image and the emulator binary it loads (an Nx input from dependency outputs or a runtime input; `tools/nx/device-bytes.mjs` does part of this), so an unchanged byte set replays from the cache. Biggest single saving per gate for tool refactors.
    Landed (branch test-speed/16-byte-keys):
    - `patchkit:device-images` writes `dist/bytes/images.sha256`: image, plan and address map of each acceptance firmware (`CELLS` of `device-inputs.mjs`) with each patch set and with each static `--modules` root list of `tests/regression/runtime/image-roots.json` (38 images, about 16 s, files kept under `dist/bytes/images/`). `card-rig.ts` and `fx-session.ts` copy them (no patchkit); a list that is not in the file fails.
    - `dist/bytes/daemon-test.key` (written by `verify_build daemons`) keys the daemon on the repository files the build read (cargo dependency file, `files.txt` of the store entry) and the dependency closure from `cargo tree`, without the patchkit sources: an unrelated workspace member leaves it unchanged, an emulator source edit changes it. The daemon store key is not touched.
    - `patchkit:pack-verdicts` writes `dist/bytes/pack-verdicts.json` (the `needs` verdict of the packs in `needs-packs.json`), read by `fx-live.ts` through `pack-needs.ts`; a missing entry fails.
    - Merge keys hold these three files and not the patchkit binary or `dist/daemons.json`. A task whose import closure holds `runtime/patchkit.ts` is live: its key keeps the binary and `DIGI_PATCHKIT_LIVE=1` is set; `runtime/patchkit-exec.ts` refuses to run patchkit in any other Nx task, and `pnpm lint` (`tools/nx/check-patchkit-spawn.mjs`) rejects patchkit spawns outside that file.
    - `test-device-all` is a real run on the full key for every test (merge tests included); `test-device-full` and conformance keep the full key.
    - Counts for a patchkit edit that changes no packed byte (non-all targets, both devices): targets with the binary 40 to 50, with `dist/daemons.json` 40 to 14, live tests 20 of 79. No mergeTests target carries the binary. Python tool sources 34 to 34, compile inputs 6 to 4.

17. [x] **Progress-based hang guard for device-test steps** (this PR). `core/dynreg:test-device--roundtrip--dt2` failed twice under gate load: `advance` and `bounded` in `tests/regression/runtime/fx-live.ts` gave each step a fixed 30 s of wall time, and `loadProject` makes one 6000 ms emulated advance that took longer on a loaded host. `stepWithProgress` and `runUntilFramesWithProgress` (`packages/test-support/src/session.ts`) now run a wait in slices of 0.5 emulated seconds, each bounded by `PROGRESS_HANG_S` (30 s, `DIGI_PROGRESS_HANG_S` overrides), so the total bound grows with the emulated time and a slice that makes no progress still fails as `RuntimeDeadlineError`. `advance`, `captureNote` and the equiv note use them.
18. [ ] **Emulator slot count above the machine-usability cap** (open). `tools/nx/resources.mjs` sets the ceiling to 12 on a 12P+4E machine (`EMULATOR_CORES` 1) and Nx runs 24 tasks; the gate boots slowed from 21 s to 36 s. The agreed cap is 4 concurrent emulator runs with load under 12. Proposal: `EMULATOR_CORES = 3` gives a ceiling of 4 on 12 performance cores. Needs Em's decision.

19. [ ] **Share compiled emulator crates between lanes** (open). Every lane compiles `sharc_gen` (the generated SHARC core, one huge crate that rustc's front end builds mostly on one thread) and the other emulator crates into its own target dir, so a new lane pays about 25 minutes and the first workbench build in a lane is slow. Proposal: build once per source version and reuse the result in all lanes, with a compiler cache (sccache) or a content-keyed artifact store like the daemon store (`tools/devtools/verify_build.py`, `emud_key.py`). Proof: the time of `mise run workbench --build-only` in a fresh lane before and after, and a check that the cache keys cover the toolchain, the features, `RUSTFLAGS` and the PGO variant. Alternatives: splitting the generated core into several crates needs the costly core regeneration, so do it only after this and only if still needed; nightly `-Z threads` is not used (the stable toolchain is pinned); a lower opt-level for `sharc_gen` is rejected (a slower emulator).

Open items from the T1a baseline ([test-architecture.md](test-architecture.md), 2026-11-14). Each names the input that over-selects. Items 20, 23 and 24 are fixed (2026-11-15), and class 13 (a dk-session edit) too; items 21 and 25 were checked and need more than an input change (see each); item 22 waits for T1e.

20. [x] **Lock script reselects the device suite.** Done (2026-11-15). `tools.devtools.benchlock` is skipped in the Python import closure (`NO_RESULT_MODULES`, `tools/nx/py-imports.mjs`) and removed from the hand-listed inputs of the seven gate roots. `tools/devtools/benchlock.py` selected 67 gate tasks (49 device leaves and 10 roots among them), now 2 (python-checks and python-tests). A file the tests really run (`tools/patchkit/pk_harness.py`) still selects 48.
21. [ ] **`modules/**/*` selects Python and Rust tool checks.** Checked 2026-11-15, no change: both targets read what the globs name. `layercheck` (in python-checks and again in python-tests) scans every tracked non-doc file for mentions of user module names, reads `.c .h .s .inc .rs`, `module.toml`, `Cargo.toml` and `*.dko.json` of the modules, and `env_registry` reads every tracked `*.ts` for `SHARC_*` and `DIGI_*` names; so a module C, asm or device test edit does select them. `gate-rust-modules:rust-tests` loads and builds the whole module library (`Library::read_root`), and its globs already exclude `tests/`. A narrower scope needs `layercheck` to scan fewer files (its cross-reference scan) and `env_registry` its own target with the inputs it reads; open.
22. [ ] **One service edit selects all runtime device tests.** A change to `services/fx/fxsend.rs` selects the same 44 device leaves, smoke and fast-roots as `collect.rs`: the input is `modules/core/runtime/**/*` of every runtime-acceptance project. Needs T1e (change to service map, tagged tests).
23. [x] **TS targets hash Rust crates.** Done (2026-11-15). `@digi/desktop` `build:ui`, `check:ui` and `typecheck` and `regression:typecheck` list the TS projects they import (`@digi/workbench-core`, `@digi/dk-session`, `@digi/dkbs`, `control-client`, `@digi/test-support`) instead of `^production`, which reached the patchkit, control and coupled crates through `digiemu-desktop`. The wasm files reach the app at run time through the Host, so no TS target reads a Rust output. `packages/patchkit/src/apply.rs`: 49 selected gate tasks, now 45 (4 TS tasks fewer); `packages/emulator/control/src/bin/digi_emud.rs`: 13, now 9. A TS edit (`packages/workbench-core/src/index.ts`, `packages/dkbs/src/index.ts`) still selects the same tasks, and `regression:typecheck` now follows `@digi/dkbs`, which its tsconfig paths import.
24. [x] **Every pack and object build follows any SDK header.** Done (2026-11-15). `packSourceInputs` lists the `sdk/include` headers that the build sources reach by a textual include scan (`tools/nx/pack-inputs.mjs`); `sdk/include/**/*` is no longer a pack or object build input. `sdk/include/dk/core/mem.h`: 93 selected gate tasks (50 pack, 3 pack-aliases, 23 build-objects), now 19 (2 pack: `tools/bootscreen`, `digikit/usb-fast`; the 10 device tasks and smoke remain through the compile inputs and the gate root). `module.h` (included by 55 modules) still selects 48 packs, `status.h` 48 packs and 2 object builds. Full-input device tasks and the gate roots keep `sdk/include/**/*`.
25. [ ] **Card, smoke and fast-roots rebuild with the daemon.** Checked 2026-11-15, no change. A patchkit edit reaches them by sources (card and smoke list `packages/patchkit/**/*`; fast-roots through `fixtures`), and the daemon runs patchkit code itself (`runtime_patch.rs`, `controller.rs`: patch records, runtime patching of its inputs), with cells that are not in `dist/bytes/images.sha256`. A daemon test key plus image hashes would miss those cells. Needs the images these three targets load hashed like the device images, then the patchkit sources can leave their inputs. Also open: `@digi/web:test-wasm` (3.6 min) is selected by any `packages/workbench-core/**/*` edit.

## Later (deferred, 2026-11-10)

Measured payoff is given where known.

- CPU-measured emulator admission: count our emulators' CPU against foreign load instead of requiring about 6 idle cores. Calibrate capacity first with a 4/6/8/10/12 concurrency throughput run. Payoff: slot waits were 595 task-min in the 17:08 full tier and 767 in the green full tier on ea8132e6 (they no longer block other tasks since #395).
- Coupled emulator binaries in the shared content-keyed store, like the daemons (`verify_build daemons`). A lane without a coupled build pays a cold `sharc_gen` build (about 13 min, 6 GB) before any gate suite runs.
- A stand-in DSP peer that acknowledges loads. It would move conformance (21 Vitest-min), inferred-needs (29) and the ColdFire-only fx checks off SHARC. Validate by comparing ColdFire-visible outputs with real SHARC.
- fx helpers choose DSP mode and PCM evidence per test (`withPreparedFxSession` and `withGainFxSession` hard-code SHARC; `evidence()` always taps PCM).
- Fix the comment at `card-rig.ts:81`: it says SHARC is the default, it is not.
- The `proofs` Vitest project (12 files in `tests/regression/runtime/*.test.ts`) runs in no Nx target: move the files into modules, or delete them after review.
- `py_select` is a second selector next to Nx. Retire it by giving the Python owners Nx targets.
- SDK host compile tests for every template and header (`chooser.h`, `data.h`, `job.h`, `params.h` and `sample_picker.h` have no test reference).
- Host-test moves: fits row format, capacity error boundaries, upload protocol errors, store v3 layout, settings bounds, card-safety classification.
- Conformance uses the cached `runtimeTests` daemon instead of a fresh daemon per module.
- The root `.gitignore` selects the whole gate: `nx affected` names it as an input of every project. Find the glob that does it and list `.gitignore` only for `fmt-check` and lint. Until then a new ignored output goes in a nested `.gitignore` (`apps/web/public/.gitignore` for `dkprof.wasm`).
- [x] The daemon test key leaves out the workspace crates the daemon reaches only through patchkit (`emud_key.patchkit_only_dirs`, from `cargo metadata`; dkaddrmap, dkneeds, dksharc, dkupload and the runtime sdk and fit crates today). Their effect is in `images.sha256` and the pack bytes. Done in the dkprof PR (P2).
- Python ports still open: pluck menu flows, `pages-dyn-pluck`, `upload-rust`, the last dkrt proof gap (step 10).

## Not planned

- `@nx/vitest` inferred targets and its per-file splitting: our plugin already makes per-file device targets with the emulator and firmware inputs; the
  split pays off mainly with distributed agents.
- Vitest sharding: per-file Nx targets already split the work on one machine.
- Vitest UI: device tests are too slow to watch interactively.
- A shared emulator pool as an Nx continuous task: the test architecture plan rejected a global pool; step 2 gets the gain with snapshots.
- More emulator slots without step 1: the machine is already full during the device phase, and longer tests then hit wall-time limits.
