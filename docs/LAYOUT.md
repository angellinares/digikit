# Repository layout

This file says who owns which part of the repository and which moves are planned and their status. Use the owner table to find code. File lists are patterns, not complete.

## Owners

| Owner | What | Where |
| --- | --- | --- |
| emulator | Native runtime, ColdFire and SHARC execution, coupled runner, launcher | `packages/emulator/{coldfire,machine,periph,card,live,boot,coupled,bootstrap,device-profile,sharc-rt,sharc-gen,sharc,sharc-fast-cl,sharc-fast-wasm}`; `tools/emulator/native_*.py`, `coupled*.py`, `digi_env.py`, `ready.py`, `card_*.py`, `live_audio.py`; trace and snapshot readers (`snapread`, `dspi_log`, `dsp_frames`, `dspdiff`, `wireevents`, `wiretrace`); scenario generators and checkers (`gen_*fx.py`, `fxcheck.py`, `scenario_dsl.py`); audio readers (`audiocheck.py`, `audiotone.py`, `palette.py`); `tests/scenarios/`; `devices/*.toml` |
| sharc-dev | SHARC+ semantics, tracing, analysis, generation | `tools/sharc_dev/sharc_core/` (the single source of instruction semantics), `tools/sharc_dev/sharcspec/` (encodings from the public manuals), `tools/sharc_dev/sharc*.py`, `tools/sharc_dev/aot_*.py`, `selasm.py`, `dspmap.py`, `framelink.py`; `tools/sharc_dev/sharc_capture.py`, `tools/sharc_dev/sharc_peer.py` |
| dkupload | Host side of the module upload protocol (`docs/design/digikit-upload.md`): codec, upload state machine, mock device; its wasm build is the mock device of the web tests; the Transfer page, on the site and in the desktop app, uses the TypeScript client of the same protocol (`packages/dk-session`, `apps/web/src/modules/dktransfer.ts`) | `packages/dkupload`; `apps/web/src/modules/`, `apps/web/src/transfer/` |
| dk-session | Shared TypeScript SysEx client, WASM wrappers, generated protocol constants and injected emulator USB RPC transport | `packages/dk-session` |
| patchkit | Patch planning, building, validation, module builds | `packages/patchkit`; `tools/patchkit/patchkit.py`, `tools/patchkit/pk_*.py`, `port_resolve.py`, `machine*.py`, `patched_*.py`, `pluck_ref.py`, `dsp_patch.py`, `tools/patchkit/dsp_patches/`; `modules/`; `devices/ports/`; `sdk/` (the module contract: `sdk/sdk.toml`, with `packages/patchkit/src/sdkgen.rs`; generated: `modules/core/runtime/sdk/src/gen_sdk.rs`, `tools/device/dklog_kinds.py`) |
| control client | Control-socket protocol | `packages/emulator/control` (crate `digi-control`: the `digi-emud` server and the `digi_control` library); `tools/control/digi_client.py` |
| device | Tools for a real device over USB MIDI | `tools/device/` (`devrpc.py` runtime RPC, `midisniff.py` SysEx capture, `mmon.py` MIDI Monitor files) |
| devtools | Verification, tests, locks, lanes, performance tooling | `tools/devtools/py_select.py`, `fx_tier.py`, `test_outcomes.py`, `rust_ws.py`, `layercheck.py` (module layering), `benchlock.py`, `lane.sh`, `worktree-setup.sh`, `perf*.py`, `fastcov.py`, `coupled_ab.py`, `spikes.py`, `app_soak.py`, `sharc_regionbench.py`, `sharc_prof_*.py`, `browser_bench.mjs`, `wasm_*.mjs` |
| apps | Desktop and browser shells | `apps/desktop` (Tauri, crate `digiemu-desktop`), `apps/web` |
| ColdFire analysis | ColdFire and Ghidra reverse-engineering tools | `tools/coldfire/cf*.py`, `ghidra*.py`, `tools/coldfire/ghidra/`, `tools/coldfire/cfisa/`, `rttiscan.py`, `refscan.py`, `vtcheck.py`, `fw.py`, `atlas.py`, `entryhist.py`, `codeseeds.py`, `hwlookup.py` |
| firmware | `.syx` container, extraction, repacking | `dt2/`, `tools/firmware/extract.py` and the other `emu/` modules, `packages/emulator/loader` (crate `dt2-firmware-loader`), `packages/emulator/plusdrive` (crate `plusdrive-format`), `tools/firmware/roundtrip.py`, `aplib_check.py` |
| tests | Python tests by owner | `tests/<owner directories>` (see `docs/TESTING.md`) |
| docs | Results and history | `docs/findings` (results), `docs/design`, `docs/plans`, `docs/handovers` (history) |

## Module folders

`modules/` holds the modules by kind; `modules/core` is split by layer (the rules are in [ARCHITECTURE.md](ARCHITECTURE.md), checked by `mise run check:layers`). Module ids do not follow the folders (`core/ui` is in `modules/core/adapters/ui`).

| Folder | Role | What |
| --- | --- | --- |
| `modules/core/runtime` | `runtime` | Loads `.dkm` modules, links their imports, owns store, selection and policy |
| `modules/core/expansion` | `installer` | Reset-time installer of all patched-in code |
| `modules/core/fw` | none | Generated firmware bindings (a library, not a module) |
| `modules/core/adapters/*` | `adapter` | One adapter per firmware plug-in point: ui, input, fx, fxpages, dsp-voice, dsp-link, dyndsp, dynfx, dynreg, machines, params, health |
| `modules/core/apps/*` | `app` | Menus and screens on the adapters' API (digikit-menu) |
| `modules/machines`, `modules/fx`, `modules/tools` | none | Content modules |

Python tools live in `tools/<owner>/`, one package per owner above. The repository root is the only import root: run a tool with `uv run python -m tools.<owner>.<module>` and import it with `from tools.<owner> import <module>`. `tools/coldfire/ghidra/` and `tools/sharc_dev/sharcspec/ghidra/` hold Ghidra scripts and language files, not Python packages.

`packages/emulator/sharc-gen` is generated by `tools/sharc_dev/sharc_transpile/` (sharc-dev) and is owned by the emulator.

## What each app builds

| App | Builds |
| --- | --- |
| Desktop | `digiemu-desktop` depends on `elektron-native-boot`, `emmc-card`, `digi-coupled` and `digi-control`. Through them it pulls in most emulator crates, the loader, plusdrive and patchkit. |
| Web | `tools/emulator/native_wasm.py` builds `packages/emulator/boot` (wasm32), `packages/patchkit` and `packages/dkupload` (C ABI). Output goes to `apps/web/.local` (not committed). |

## Planned moves

| Today | Planned | Status and reason |
| --- | --- | --- |
| `native/` crates | `packages/emulator/` | Done (R3, 2026-10-15). The move needed cold Rust rebuilds and a PGO retrain. Old→new paths: `23a03a8:tools/devtools/move-r3.json` (applied with `23a03a8:tools/devtools/move_dirs.py`). |
| `packages/desktop`, `packages/web` | `apps/desktop`, `apps/web` | Done (R3, 2026-10-15). Same map file and tool. |
| `tools/` | `tools/<owner>/` | Done (R1, 2026-10-14): one package per owner under `tools/`. Old→new paths: `23a03a8:tools/devtools/relocate-r1.json` (applied with `23a03a8:tools/devtools/relocate.py`). |
| `tests/` | Domain directories | Done. See `docs/TESTING.md`. |
| `emu/`, `dt2/` | `tools/firmware/`, `tools/coldfire/`, `tools/sharc_dev/` | Done (pre-publication, 2026-10-16). |
| `packages/emulator/patchkit` | `packages/patchkit` | Done (2026-10-16): a module toolchain, not an emulator crate. |
| `scenarios/` | `tests/scenarios/` | Done (2026-10-16): the scenario files are test oracles. |
| Root firmware files, `sections/`, `snapshots/`, `scratch/` | `firmware/`, `out/sections/<image>/`, `out/snapshots/`, `out/scratch/` | Done (2026-10-16): local data in one place; tools resolve firmware through `tools/firmware/config.py`. |

Use the owner table to find code.

## Generated and ignored outputs

Never committed (firmware and anything derived from it):

- `out/` (builds, runs, caches, `out/sections/<image>`, `out/snapshots`, `out/scratch`)
- `firmware/` (stock images you supply; only `firmware/README.md` is tracked)
- `apps/web/.local`
- `*.syx` anywhere

Committed generated files:

| File | Generator | Regenerate |
| --- | --- | --- |
| `packages/emulator/sharc/src/fast/gen_helpers.rs` | `tools/sharc_dev/sharc_lowergen_helpers.py` from `tools/sharc_dev/sharc_core` | `mise run gen:regen` |
| `packages/emulator/sharc/src/fast/lower/gen_forms.rs` | `tools/sharc_dev/sharc_lowergen.py` from `tools/sharc_dev/sharc_core` | `mise run gen:regen` |
| `modules/core/runtime/sdk/src/gen_sdk.rs` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` | `patchkit gen` (`--check` reports stale files) |
| `tools/device/dklog_kinds.py` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` | `patchkit gen` (`--check` reports stale files) |
| `tools/device/dkproto.py` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` `[protocol]` and `[load.status]` | `patchkit gen` (`--check` reports stale files) |
| `packages/dk-session/src/gen_proto.ts` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` `[protocol]` | `patchkit gen` (`--check` reports stale files) |
| `packages/dkupload/src/gen_proto.rs` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` `[protocol]` | `patchkit gen` (`--check` reports stale files) |
| `sdk/asm/dk_machine.inc` | `patchkit gen` (`packages/patchkit/src/sdkgen.rs`) from `sdk/sdk.toml` `[machine]` (the C view `sdk/include/dk/gen/machine.h` is generated by `patchkit gen` from `sdk/sdk.toml` too) | `patchkit gen` (`--check` reports stale files) |
| `modules/core/adapters/dsp-voice/build/gen/adapter-{dev}.inc` | `patchkit gen` (`packages/patchkit/src/devgen.rs`) from `[adapter.machine]` of `devices/<dev>.toml` | `patchkit gen` (`--check` reports stale files) |
| `modules/core/**/build/*.dko.json` | Module builds of `core/*` modules (built module objects, committed so that the workbench and the browser patch without a toolchain; no firmware bytes) | `mise run module:build [-- modules/core/NAME] [--check]` |
| `out/cache/dko/<module id>/<object path>` | Objects of content modules and fixtures: build output, not committed. `patchkit build`, `patchkit pack` and the library scan (Rust tests, `module:pack-all`, the web gallery) build what is stale. Keyed by `source_sha256` | `mise run module:build` (also done by `module:pack`, `dk:run`, `dk:dev`) |
