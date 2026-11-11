---
name: digikit-module
description: Create, build, check, pack, run, export or import a digikit content module (a machine, an effect with its own FX page and master or send routes, or a ColdFire service loaded from the card as a .dkm) for the Digitakt II or Digitone II. Use for module:new, module:check, module:pack, module:export, module:import, dk:run, the module.toml manifest, the machine contract (render, note_on, desc), the machine declaration (DK_MACHINE, DK_PARAM, DK_PAGE in src/module.c), the effect contract (DK_FX, DK_FX_PARAM, DK_PAGE, route, routes) and knob and parameter rules. Patched-in core modules: references/patched-in.md. Not for firmware analysis.
---

# digikit modules

The guide is `docs/MODULE-AUTHORING.md`. Read the section you need there; this file only routes you. Run every command from the repository root.

## Module kinds

`[module] type` sets the kind (guide section 1).

| Kind | What it is | Template |
| --- | --- | --- |
| `machine` | A SHARC+ voice: `render`, `note_on`, `desc` (guide 5.2, 7.1) | `sdk/templates/machine`; a real one: `modules/machines/pluck` |
| `fx` | A SHARC+ master or send effect with its own page: `process`, `desc` (guide 5.4, 7.2) | `sdk/templates/fx`; a real one: `modules/fx/freqshift` |
| `tool` | ColdFire C: `dkm_init`, `dkm_fini`, `dkm_slot_N` (guide 5.5, 7.3) | `sdk/templates/tool` |
| `raw` | No contract, only the generic load checks | `tests/fixtures/modules/c-demo` |

`framework` is the internal type of the `core/*` modules. Changes to `core/*` are firmware work and need Em.

## Workflow

1. Create: `mise run module:new -- KIND AUTHOR/NAME [--dir DIR] [--title T] [--short S] [--no-build]` (guide 3). `KIND` is `machine`, `fx` or `tool`.
   With `--dir`, set `PATCHKIT_MODULES=DIR`.
2. Edit `module.toml` and the source (guide 4 to 7). Reuse `references/dsp-recipes.md`, avoid `references/selas-pitfalls.md`, size with `references/devices.md`.
   A machine and an effect are declared in C in `src/module.c`. Machine: `DK_MACHINE`, `DK_PARAM`, `DK_PAGE` (`<dk/machine/machine.h>`). Effect: `DK_FX`,
   `DK_FX_PARAM`, `DK_PAGE` (`<dk/fx/fx.h>`). Word numbers go in a shared `src/NAME.h`. `patchkit desc AUTHOR/NAME` prints the records as JSON.
   A manifest with `[[voice]]`, `[[machine]]`, `[[param]]`, `[[page]]` or `[[fx]]` is refused.
3. After a change to `module.toml`, `src/module.c`, `devices/*.toml` or `sdk/sdk.toml`: `mise run gen:modules` (`-- --check` lists stale files). Never edit `build/gen/`.
4. Build: `mise run module:build -- PATH [--check]` (`patchkit build`; the objects go to `out/cache/dko/<id>/`, `patchkit dko show FILE` shows one). Pack and `dk:run` build what is stale, so this step is optional.
5. Check, no emulator: `mise run module:check -- AUTHOR/NAME [--device dt2|dn2]` (guide 8). The module's own tests live in its `tests/` (`test_<name>_*.py` with a unique basename, `*.test.ts`, `scenarios/*.toml`); no list names them. Code outside the module folder never names the module (`check:layers`, rule `self-contained`; `host = true` exempts a host-protocol module); shared tests use `tests/fixtures/modules`.
6. Pack: `mise run module:pack -- AUTHOR/NAME --device dt2|dn2 [--trace] [-o FILE.dkm]`. `--trace` is described under Tracing below.
7. Run: start `mise run workbench` in another shell, then `mise run dk:run -- AUTHOR/NAME --emu --seconds 10`. On a device with the runtime:
   `mise run dk:run -- AUTHOR/NAME --port Digitakt`. Hardware is Em's decision every time; never flash anything.
8. Log: `mise run dk:log -- --emu follow --module NAME` (guide 11).
9. Share: `mise run module:export -- AUTHOR/NAME` and `mise run module:import -- FILE [--dir DIR] [--replace]` (guide 10).

Each task takes `-- --help`.

## Rules

- **No firmware bytes in the repository.** Modules hold our code and hashes only. Patched images and anything derived from firmware stay under `out/`.
  Never name, copy or quote the vendor DSP toolchain's files.
- **Reserved authors.** `core` and `test` are reserved. Use `digikit` or your own author. `module:new` refuses `core` and `framework`.
- **Commit `build/gen/`** (only `gallery.json` is generated there); do not edit it by hand (guide 4, 6). Do not commit built objects: a content module has no `build/*.dko.json` (they are cache in `out/cache/dko`; only `core/*` commits objects).
- **Knobs and parameters.** A machine has at most as many `DK_PARAM` words as the device's `param_slots` (Digitakt II 5, Digitone II 8; in order, knob units 0 to 127) and at most one `DK_PAGE` with `DK_KNOB` only.
  The knob look is `DK_GLYPH_ENCODER` (0 to 127) unless `DK_PARAM_GLYPH(machine, word, G)` sets `DK_GLYPH_FADER` (0 to 127), `_PAN` (0 to 127, centre 64), `_ONOFF` (0 to 1) or `_TUNE` (4 to 124, centre 64, semitones) or `_PLAY` (0 to 3: REV, REV.L, FWD.L, FWD); the range must fit. Not for effects yet.
  The adapter places word w on a fixed knob (words 0 to 4 on knobs B C E F G on both devices; the Digitone II adds A, D, H for words 5 to 7). The word is the slot, so a released parameter keeps its word.
  Effect knobs may sit at any position 0 to 7 (`DK_KNOB_AT`). No data-only machines; `clone_of` is fixed by the profile (guide 5.2, 5.3, 5.7).
- **No fixed type number** for a run-time machine. The runtime assigns it at load. Never write a number into code or a test.
- **Effects.** MIX, the route and the send amounts belong to the framework. Write the wet signal only and do not assume you are last (guide 5.4, 7.2).
  Never rename the module id or the effect `id` after release.
- **Elektron files stay stock.** Digikit data lives in the runtime's settings store, not in kit, project or pattern files.
- **Versions.** Contract versions are in `sdk/sdk.toml`. A contract major mismatch is refused (`E_CONTRACT`): rebuild (guide 9).
- **Tests.** Use the owner's checks (`mise run test:changed`). Do not run the emulator when a static answer is enough, and bound every run.
- Facts go in `docs/findings/` with the marks [V], [D], [O], not in handovers.

## Tracing

Opt-in marks that compile to nothing without `--trace` (same objects, byte-identical `.dkm`). Full text: `sdk/README.md`, "Tracing".

- C: `#include <dk/core/trace.h>`, `DK_TRACE_INIT(api);` once per source file in `dkm_init`, then `DK_TRACE_LOG("msg", value);` (calls `api->log`).
- SHARC+: `#include "dk_trace.inc"`, then `DK_STEP(3, R0);` stores 3 in `_dk_step` using the scratch register you name (free at that point).
- Build: `module:pack -- ID --device dt2 --trace`, `module:check -- ID --trace` (size lines), `dk:dev -- ID --store ID --trace`. `--trace` defines
  `DK_TRACE` for C and SHARC+ sources and sets the trace bit in the `.dkm` header (`patchkit dkm-info` shows it).
- You see it as ` TRACE` on the module's row in DIGIKIT, ABOUT, and as `STEP n` in the fault record (the core clears `_dk_step` before each call
  into a module). A traced module imports `_dk_step` from `core/dyndsp`: a runtime without it answers `DSP IMPORT`.
- Do not ship a traced `.dkm`. The fixture is `tests/fixtures/modules/trace-demo`; the zero-cost proof is `packages/patchkit/tests/trace.rs`.

## Firmware functions for a core adapter

Only `core/*` adapters need firmware addresses. Use the firmware-query skill: `fw.py atlas TERM`, then `fw.py catalog TERM -i IMAGE --grade`. Check the
candidate on both devices and confirm callers with `tools/coldfire/refscan.py`. Content modules never use anchors.

## References

- `references/dsp-recipes.md`: tested assembly from pluck and the deleted master-drive (parameter to float, block loop, noise, one-pole, all-pass, delay line, comb, silence, ramp).
- `references/selas-pitfalls.md`: assembler forms that are wrong on the silicon and what to write instead.
- `references/devices.md`: Digitakt II against Digitone II (budget, voices, parameters, memory, loader limits, effects).
- `references/patched-in.md`: patched-in modules (anchors, claims, `patch`, `module:dev`, `patch:test`).
- `docs/findings/54-module-cookbook.md`: what each reference module taught. Add to it with every module.
- `docs/design/module-sdk.md`, `modules/README.md`, `docs/PATCHING.md`.
