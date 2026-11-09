# digikit: agent instructions

Reverse engineering and emulation of the Elektron Digitakt II and Digitone II, with a ColdFire MCF5441x CPU and SHARC+ DSP, for desktop and browser (Wasm).

Use **DT2 OS 1.17** and **DN2 OS 1.12** for analysis, patches, tests and reported addresses. DN2 1.11 is a secondary patch port (the health proof and its tests run on it); analysis and reported addresses stay on 1.17/1.12. DT2 1.16 and older images are references only: resolve their addresses on the current image before use. Patchkit anchors resolve by hash through `devices/ports/`.

Start with [README.md](README.md) for setup and the newest `docs/handovers/HANDOVER-*.md` for current work. Detailed guides: [layout](docs/LAYOUT.md), [testing](docs/TESTING.md), [regression acceptance](tests/regression/README.md), [instrumentation](docs/INSTRUMENTATION.md), and [findings index](docs/FINDINGS.md).

## Directory map

```text
apps/
  desktop/          Native workbench app and UI
  web/              Browser app and module gallery
packages/
  emulator/         ColdFire/SHARC engines and control daemon
  patchkit/         Firmware patching and module packaging
  control-client/   TypeScript daemon socket client
  dk-session/       TypeScript SysEx session layer
  test-support/     Shared test fixtures and helpers
  dkupload/         Rust upload protocol
  dkbs/             Boot screen format, player and authoring CLI
  dksample/         WAV, native sample format and hashing
  dkneeds/          Module resource requirements
modules/
  core/             Layers: runtime/, expansion/, adapters/, apps/, fw/ (docs/ARCHITECTURE.md)
  machines/         Instrument modules
  fx/               Effect modules
  tools/            Device utility modules
sdk/                Module contracts and headers
devices/ports/      Firmware-version anchors and device ports
tests/              Python and native tests by owner
  regression/       TypeScript device acceptance tests
tools/              Analysis, build and development tools
docs/               Guides, findings, plans and handovers
firmware/           Ignored firmware inputs
out/                Ignored generated artifacts and run outputs
```

## Core rules

- Preserve user work and follow session instructions for branches and commits. Commit verified work at useful breaking points; changes to `main` go through a PR.
- Never commit firmware or derived material: `firmware/*.syx`, `out/`, `apps/web/.local`, card images, or users' sample audio.
- Never name, copy or quote the vendor DSP toolchain or its files. Cite public manuals listed in [SHARC sources](docs/sharc/SOURCES.md).
- Put results in `docs/findings/`, one topic per file, indexed from `docs/FINDINGS.md`; handovers describe ongoing work. Marks: **[V]** verified here, **[D]** measured or read once, **[O]** open, **[C]** corrects an earlier claim. Before marking [V], a second agent must check against image bytes. An empty caller list does not prove dead code.
- Before work likely to take more than a day, state payoff and confidence; recommend stopping when they are weak.
- Use static evidence first. Bound emulator runs with `--limit` or `--seconds`. Stop only your own PIDs or run directories; never use `pkill` or `killall` on emulators.
- For performance claims, give the ceiling first, then one paired A/B using `tools/devtools/coupled_ab.py`. Host instruction ratios (`cpu.dsp_thread_insns_per_audio_s`, `sharc_matrix` `insn_ratios_B_over_A`) repeat within 0.02% on a busy host: use them to judge path-length changes, and keep the paired timing A/B as the final check. `tools/devtools/sharc_cells.py` produces these counts, parity and exit tables for two binaries.

## Checks

Use `mise run test` for complete affected verification and the merge gate.
When selection is unclear, run `mise run test:plan` first: Nx explains the tasks without running them. Device and conformance targets always run for every owner and replay from the cache when their key is unchanged; the plan does not list them.
Use the owner's explicit Nx target or named test while iterating. `test:changed` is only cheap preflight,
not complete verification. Do not use `--skip-nx-cache` to rerun a failed test: Nx does not cache failures, and the flag reruns every dependency. Full command details and preparation live in [testing](docs/TESTING.md). Emulator tier settings live in `config/emulator.json`; `tools/emulator/emulator_config.py` and `packages/test-support/src/emulator-config.ts` resolve them.

| Work | Iteration check |
| --- | --- |
| Any change | `mise run test:changed`: lint, format, types, host tests and Python checks of the affected projects; no builds or devices. |
| JS/TS host helpers | `pnpm nx:pilot run <project>:test-host` and `<project>:typecheck`. |
| Desktop UI | `pnpm nx:pilot run @digi/desktop:test-host` and `@digi/desktop:check:ui`; `@digi/desktop:typecheck` checks its test sources. |
| Runtime Rust | `pnpm nx:pilot run core/runtime:rust-tests`: standalone host units, no generated core or daemon prerequisites. |
| Web Node tests | `pnpm --filter @digi/web test` |
| Python tools | Named pytest files under `tests/<owner>/` (never a directory) and `mise run lint:py`. Typed files are listed in pyproject `[tool.mypy] files`. |
| Device tests | One module: `pnpm nx:pilot run <module-id>:test-device` (packs, CLIs and daemons are `dependsOn` targets built on demand into `dist/`); a project with several test files also has `<module-id>:test-device--<file>` for one file; add `-- -t 'DN2 1.12'` to such a target to run only DN2 while iterating. Suites: `mise run test:stock`, `test:fx`, `test:fx:dt2`; `pnpm nx:pilot run-many -t test-conformance` runs the conformance tests. Prerequisites (daemons, ready inputs) are in [testing](docs/TESTING.md); these three suites run through Nx, which builds the prerequisites and reuses cached results. No ad-hoc proof scripts. |
| One module | `mise run module:check ID`: conformance and own tests; may build or emulate. This does not prove all-module loads. |
| Module, UI or runtime iteration | `mise run dk:dev -- ID --device dn2 --keys 'tap:...,shot:NAME'`: builds, reloads a kept daemon (UI modules swap in about 4 s; any other change boots: DN2 5 s, DT2 10 s) and writes `out/runs/devreload/<dev>/sheet.png`. Iterate on DN2, check DT2 at the end. Not a proof: finish with the owner's check. |

Test levels: unit tests run without a device. A device test runs without the SHARC DSP unless it passes `dsp: 'sharc'` (`resolveDsp` in `packages/test-support/src/session.ts` is the only place that decides): a test needs it for audio, PCM, DSP memory or a machine or effect module's DSP path, and the session throws on `pcm.*`, `dsp.*` and `audio.*` calls without it. Device tests are TypeScript (Vitest) and live with their subject: `modules/<kind>/<name>/tests/*.device.test.ts` in the owning module or nested test-only project (`core/runtime-tests` for runtime acceptance), registered in that project's `runtimeAcceptance` metadata, which `modules.mjs` reads (modules, aliases, plusdrive). Only stock-firmware oracles (`tests/regression/dt2`, `tests/regression/scenarios`) and the shared harness stay in `tests/regression`; `tests/regression/runtime` holds only shared harness helpers. Device tests share their steps through the harness helpers (`expectRuntimeReady`, `waitEmulated`, `tap`, `captureNote`, `drainInputs`, `runtime/fixtures.ts`) instead of local copies. Waits are in emulated time or on firmware state; wall time only guards against a hang. Python tests test Python tools. Python device scenarios remain for the two pluck menu flows and `pages-dyn-pluck` (same folder), and `tests/digikit/scenarios/upload-rust.toml`; the patchkit smoke matrix stays Python (`gate-smoke`, `tools/patchkit/pk_harness.py`). A Python scenario is deleted once its TypeScript port passes on both devices ([ledgers](docs/plans/ledgers/)). Tests must test our behaviour: no tests of third-party behaviour, of constants against themselves, or of exact command argv. Gate checks are commands, not test files.

Before merging, run `mise run test` once (`DIGI_GATE_BASE` sets the base for Nx and the Python selector); selection and targets are in [testing](docs/TESTING.md) "Nx gate". Iteration checks and cheap preflight do not replace it. Do not repeat passed checks without a new change or unresolved concern. Run only the tests that the changed files select. When the selection is wider than the change can reach, fix the project inputs and edges (`tools/nx/*.mjs`, `project.json`) instead of running the wide set.

The local `.nx/` cache is per worktree. Module `test-device` and `test-conformance` (and other cached targets) also use the clone's shared remote cache (`tools/nx/remote-cache.mjs`; plain runs and every gate tier read and write when the server answers). Before enabling or changing `cache: true`, inspect `pnpm nx:pilot show target inputs PROJECT:TARGET --json` / `--check PATH`, review source/dependency/output/toolchain/environment coverage, and prove relevant invalidation and output restoration with scoped checks. Native inspection describes declared inputs, not observed read closure. For Cargo targets retain `node tools/nx/cargo-depfiles.mjs`; see [testing](docs/TESTING.md) "Nx cache". The `digi-control:daemons` Nx prerequisite uses `verify_build daemons`, backed by a content-keyed store shared by all worktrees.

Do not run test directories, full tier sweeps, or smoke `--update` across all cells while iterating. Use quick proof modes; the default test owns smoke coverage. Reserve `mise run test:full` for generated-core changes before merging to `main`. Preserve SHARC golden hashes for refactors; explain intentional updates. PCM pins in `tests/scenarios/*.json` require Em's approval.

Wait without polling. Run a long command with a long tool timeout, up to 2 hours for a Claude Code session (`BASH_MAX_TIMEOUT_MS`). Claude Code moves it to the background after about 3 minutes and notifies you when it finishes: wait for that notification. Do not wait with `sleep`, `tail` or repeated log reads, and never use `tail -f`. If you stop early, stop what you started.

## Development and analysis

Iterate with `dk:dev` ([testing](docs/TESTING.md) "Fast iteration"), not with harness runs or fresh boots; run the harness once for the proof. Respect the layers in docs/ARCHITECTURE.md; `mise run check:layers`. Prefer existing tooling and shared helpers over custom runners or wrappers. Use fast unit tests for logic and integration tests for real boundaries. New host/device scenarios use TypeScript/Vitest; analysis, decompilation and devtools use Python. Avoid unrelated refactors.

Run Python tools from the repository root as `uv run python -m tools.<owner>.<module>`. Import through `tools`; never modify `sys.path`. Child tools use `[sys.executable, "-m", ...]` with `env=tools.python_env()`. Find tasks with `mise tasks` and options with `mise run TASK -- --help`.

Use existing database helpers before one-off queries: `tools/coldfire/cf.py` and `tools/sharc_dev/sharc.py`. Read dumps first. Ghidra uses one JVM per project and language `68000:BE:32:ColdfireEMAC`; check apparent missing callers with `tools/coldfire/refscan.py`.

SHARC instruction semantics belong only in `tools/sharc_dev/sharc_core/`; trace/run tools are drivers. Changes there or in `tools/sharc_dev/sharc_transpile/`, `tools/sharc_dev/sharc_transpile_infer.py`, or `tools/sharc_dev/sharc_rsgen.py` change `CORE_FINGERPRINT` and require costly regeneration. Back up each generated directory, regenerate DN2 and DT2 using `out/native/{dn2,dt2}-audio/gen/PROVENANCE.txt`, and regenerate the shared core with `mise run gen:core`.

Builds and benchmarks use `tools/devtools/benchlock.py`. `coupled_ab.py`, `native_pgo.py`, `sharc_regionbench.py`, `pk_modcheck` and the scenario runner already take the lock, and the Nx launcher (`pnpm nx:pilot`) takes it for heavy task runs; do not wrap them again (it deadlocks). Launcher outputs in `out/native/target/*/pgo-use` are reserved; experiments use another `DIGI_EMU_PGO_VARIANT`.

## Worktrees

Use warm lanes (`tools/devtools/lane.sh NAME [BASE]`) for parallel native edits or long measurements; small edits and reading stay in the main tree. Lanes share artifacts and the virtualenv: do not change Python dependencies there. Use a separate dev-server port per lane.
Each lane builds into its own `dist/native-target/` (deleted with the lane; a lane that predates this pays one cold build). Reuse one cargo target per lane: a new target variant name builds cold (10 to 30 minutes), a rebuild takes seconds. Run `cargo check` before a long build. Start a new branch in a warm lane rather than a new lane; a new worktree pays about 25 minutes of cold builds.
