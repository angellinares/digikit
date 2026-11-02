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
- For performance claims, give the ceiling first, then one paired A/B using `tools/devtools/coupled_ab.py`.

## Checks

Use the owner's command while iterating. Full command details, preparation and routing live in [testing](docs/TESTING.md).

| Work | Iteration check |
| --- | --- |
| Docs | `uv run python -m tools.devtools.pathcheck --paths PATH ...` for changed path references. |
| Any change | `mise run test:changed` (`--explain` shows the selection): changed files only, no builds or devices. |
| JS/TS host and desktop UI | `mise run test:host`; `mise run verify:host` also runs lint, format and types. |
| Web Node tests | `mise run web:test` |
| Python tools | Named pytest files under `tests/<owner>/` (never a directory) and `mise run lint:py`. Typed files are listed in pyproject `[tool.mypy] files`. |
| Device tests | One file: `mise run verify -- --tests runtime/settings.test.ts` (packs the modules its record names). Suites: `mise run test:stock`, `test:runtime`, `test:modules`, `test:fx`, `test:fx:dt2`. Prerequisites (daemons, ready inputs, module artifacts) are in [testing](docs/TESTING.md); no implicit daemon builds. No ad-hoc proof scripts. |
| One module | `mise run module:check ID`: conformance and own tests; may build or emulate. This does not prove all-module loads. |
| Module, UI or runtime iteration | `mise run dk:dev -- ID --device dn2 --keys 'tap:...,shot:NAME'`: builds, reloads a kept daemon (UI modules swap in about 4 s; any other change boots: DN2 5 s, DT2 10 s) and writes `out/runs/devreload/<dev>/sheet.png`. Iterate on DN2, check DT2 at the end. Not a proof: finish with the owner's check. |

Test levels: unit tests run without a device. Device tests are TypeScript (Vitest) and live with their subject: `modules/<kind>/<name>/tests/*.device.test.ts` in the module project they test, `core/*` included, registered in that project's `runtimeAcceptance` metadata, whose `sources` list every Python tool the test runs (the gate routes Python changes by them). Only stock-firmware oracles (`tests/regression/dt2`, `tests/regression/scenarios`) and the shared harness stay in `tests/regression`; the files in `tests/regression/runtime` are moving out ([Nx plan](docs/plans/nx-pilot.md), stage 2). Device tests share their steps through the harness helpers (`expectRuntimeReady`, `waitEmulated`, `tap`, `captureNote`, `drainInputs`, `runtime/fixtures.ts`) instead of local copies. Waits are in emulated time or on firmware state; wall time only guards against a hang. Python tests test Python tools. The remaining Python device scenarios (`tests/digikit`, the smoke matrix) are deleted family by family once their TypeScript ports pass both tiers ([ledgers](docs/plans/ledgers/)). Tests must test our behaviour: no tests of third-party behaviour, of constants against themselves, or of exact command argv. Gate checks are commands, not test files.

Before merging, inspect `mise run verify -- --base main --explain`, then run the routed gate once with `mise run verify -- --base main` (use the lane's base when appropriate). Iteration checks do not replace it. The coordinator owns the gate and integration. Do not repeat passed checks without a new change or unresolved concern. Run only the tests that the changed files select. When `--explain` selects more than the change can reach (for example a deletion of code that no test calls), fix the routing in `tools/devtools/verify_route.py` instead of running the wide set.

Do not run test directories, full tier sweeps, or smoke `--update` across all cells while iterating. Use quick proof modes; the gate owns smoke coverage. Reserve `mise run verify:full` for generated-core changes before merging to `main`. Preserve SHARC golden hashes for refactors; explain intentional updates. PCM pins in `tests/scenarios/*.json` require Em's approval.

The gate manages one CPU budget. Do not start another gate or Cargo build while it runs. Put long runs in the background; poll with an exit condition and at most 60 seconds per call. Never use `tail -f`. For changed references without gate pathcheck coverage, run `uv run python -m tools.devtools.pathcheck --paths PATH ...`; cite deleted paths as `REF:path`.

## Development and analysis

Iterate with `dk:dev` ([testing](docs/TESTING.md) "Fast iteration"), not with harness runs or fresh boots; run the harness once for the proof. Respect the layers in docs/ARCHITECTURE.md; `mise run check:layers`. Prefer existing tooling and shared helpers over custom runners or wrappers. Use fast unit tests for logic and integration tests for real boundaries. New host/device scenarios use TypeScript/Vitest; analysis, decompilation and devtools use Python. Avoid unrelated refactors.

Run Python tools from the repository root as `uv run python -m tools.<owner>.<module>`. Import through `tools`; never modify `sys.path`. Child tools use `[sys.executable, "-m", ...]` with `env=tools.python_env()`. Find tasks with `mise tasks` and options with `mise run TASK -- --help`.

Use existing database helpers before one-off queries: `tools/coldfire/cf.py` and `tools/sharc_dev/sharc.py`. Read dumps first. Ghidra uses one JVM per project and language `68000:BE:32:ColdfireEMAC`; check apparent missing callers with `tools/coldfire/refscan.py`.

SHARC instruction semantics belong only in `tools/sharc_dev/sharc_core/`; trace/run tools are drivers. Changes there or in `tools/sharc_dev/sharc_transpile/`, `tools/sharc_dev/sharc_transpile_infer.py`, or `tools/sharc_dev/sharc_rsgen.py` change `CORE_FINGERPRINT` and require costly regeneration. Back up each generated directory, regenerate DN2 and DT2 using `out/native/{dn2,dt2}-audio/gen/PROVENANCE.txt`, and regenerate the shared core with `mise run gen:core`.

Builds and benchmarks use `tools/devtools/benchlock.py`. `coupled_ab.py`, `native_pgo.py`, `sharc_regionbench.py`, `pk_modcheck` and the scenario runner already take the lock; do not wrap them again (it deadlocks). Launcher outputs in `out/native/target/*/pgo-use` are reserved; experiments use another `DIGI_EMU_PGO_VARIANT`.

## Delegation

Follow the user's delegation policy and configured roles. Keep one writer per worktree; read-only agents do not edit. Hand off evidence, validation and residual risks; the coordinator reviews diffs and integrates.

Use warm lanes (`tools/devtools/lane.sh NAME [BASE]`) for parallel native edits or long measurements; small edits and reading stay in the main tree. Lanes share artifacts and the virtualenv: do not change Python dependencies there. Use a separate dev-server port per lane.
