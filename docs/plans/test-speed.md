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

1. [ ] **Zero DSP by default** (branch `feat/zero-dsp-default`). Device tests run ColdFire only unless they need the DSP (`--dsp zero`: about 2.2 times
   real time against about 0.2 coupled). Proof: per-file `runtime-phase` totals before and after on the same files, one paired run.
   - [ ] Weighted emulator slots: a zero-DSP emulator takes about one core, a coupled one about two. `emu-slot.mjs` counts a zero-DSP run as half a slot.
     Proof: same machine load at a higher device concurrency.
2. [ ] **Reload cost.** Find out why a reload between tests costs 25 to 40 s when a snapshot hit is 0.2 to 1.5 s: snapshot misses, or module loading
   replayed in emulated time after the restore. Use the `runtime-phase` records ([finding 69](../findings/69-runtime-test-timing.md)); no new runs if
   the gate logs answer it. If loading is the cost, snapshot at "runtime ready" per image, card and module set, and restore that per test.
   Proof: `bootscreen` and `introblank` reload time per test.
3. [ ] **Device inputs from built bytes.** Device and conformance tasks hash the patched image and `.dkm` content instead of SDK, patchkit and device
   file sources. A change that does not change the bytes (a comment, an unused constant, a test-only patch set) reruns nothing. Real core changes
   still rerun everything. Owner: the Nx lane. Proof: `nx show target inputs` and one invalidation check per input kind.
   - [ ] Carried over: skip audio scenarios whose image, module objects, daemon and test code did not change.
4. [ ] **Core-change policy.** Decision for Em: (A) every core change runs all device tests, or (B) the merge tier runs runtime-tests, smoke,
   conformance and one test per area on both devices, and the full suite runs at batch points. Decide after steps 1 to 3 have shrunk the full suite.
5. [ ] **Gate output and history.**
   - [ ] One line per finished task (project:target, pass or fail, seconds) while the gate runs.
   - [ ] Append each task's result and duration to a history file; use it to start the longest tasks first, to list timeouts and flaky tests, and to
     show tests that got slower.
   - [ ] Test timeouts in emulated time or firmware state, as AGENTS.md requires for waits. A wall-time limit fails healthy tests under load.
6. [ ] **Device test audit.** For each device test, one of: needs the emulator; can be a host test (Rust runtime unit test or TS); duplicates another
   test. Start with the reload-bound files (`card-safety`, `settings`, `bootscreen`, `introblank`, `memaudit`, `capacity`). Keep one device test per
   boundary. Split composite tests such as `fx-track-route` so they run in parallel.
   - [ ] Carried over: move ColdFire-only checks to the CF-only rig (fxroute, fxroutes, fxpage, fxpersist, settings).
7. [ ] **Vitest 5.** One PR from 3.2.4 to 5.x (needs Node 22.12 or later; we run 24): `singleFork` and `poolOptions` become `maxWorkers`/`isolate`
   settings, `workspace` is `projects`. Then close the Dependabot Vitest PRs. Use tags for the DSP mode and `--last-failed` for reruns.
8. [ ] **Python device scenarios to TS** only where they boot an emulator: module `equiv-*`, the pluck menu flows, `pages-dyn-pluck`, `upload-rust`
   ([ledgers](ledgers/)). Other Python tests stay; moving them buys no speed.
9. [ ] Carried over: smoke per-cell results, so a rerun repeats the failed cells only.
10. [ ] Carried over: runtime-resident DSP code at 0xbec4xx is still interpreted on DN2 (needs a JIT or a runtime-aware gen).

## Not planned

- `@nx/vitest` inferred targets and its per-file splitting: our plugin already makes per-file device targets with the emulator and firmware inputs; the
  split pays off mainly with distributed agents.
- Vitest sharding: per-file Nx targets already split the work on one machine.
- Vitest UI: device tests are too slow to watch interactively.
- A shared emulator pool as an Nx continuous task: the test architecture plan rejected a global pool; step 2 gets the gain with snapshots.
- More emulator slots without step 1: the machine is already full during the device phase, and longer tests then hit wall-time limits.
