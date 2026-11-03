# Native crates

The Rust side of the emulator: a ColdFire core, board and peripheral models, the
SHARC+ DSP engine, the coupled run loop, and the desktop and browser hosts.

## Build and run

Use mise (`mise tasks` lists every task). Cargo needs the generated SHARC+ core;
the tools set `SHARC_GEN_DIR` and the target dir for you (see "Target dirs").

```
mise run workbench -- --build-only   # build the desktop emulator without launching it
mise run workbench         # build and run it (DN2 audio when the ready inputs exist)
mise run scenario:build    # build the headless runner coupled-run
mise run scenario:all      # run every DN2 scenario against its pinned PCM hash
mise run test:quick        # fast checks for the tooling and the coupled runtime
uv run python -m tools.devtools.rust_ws check    # cargo check --workspace --all-targets
mise run web:wasm          # browser cores
```

The ready inputs come from `mise run emu:ready -- dn2|dt2` (see the top README).

## Crate map

One cargo workspace (root `Cargo.toml`, one `Cargo.lock`, profiles defined at the
root). The directory name is not always the package name; use the package name with
`cargo -p`.

| directory | package | role |
|---|---|---|
| `packages/emulator/coldfire` | `coldfire` | ColdFire V4e core (ISA_C, EMAC); decoder generated from `tools/coldfire/cfisa/coldfire.json`. Pure, builds for wasm32. |
| `packages/emulator/periph` | `periph` | MCF5441x timer, interrupt controller, eDMA, DSPI and SSI models. |
| `packages/emulator/card` | `emmc-card` | Minimal eMMC card identity and media model (sparse write overlay). |
| `packages/emulator/machine` | `machine` | The board seam: guest RAM, MMIO, eMMC DMA, timers. |
| `packages/emulator/loader` | `dt2-firmware-loader` | Decoder for the `.syx` container (ELE3 sections, no I/O). |
| `packages/emulator/device-profile` | `device-profile` | Hash-bound device and firmware identities, embedded from `devices/*.toml`. |
| `packages/emulator/plusdrive` | `plusdrive-format` | +Drive image format encoders; the `plusdrive` CLI. |
| `packages/emulator/boot` | `elektron-native-boot` | Bounded cold boot, the ColdFire runtime, the DSP peer (`sharc_peer`), DSP patches, and the examples (`card_boot`, `dsp_cold`, `sharc_live`, `turn_probe`; the headless tools `boot_gate`, `panel_drive` and `sysex_bridge`, see their headers). Also the wasm ABI. |
| `packages/emulator/sharc-rt` | `sharc-rt` | Runtime under the generated core: state, memory, decoder, instruction table. |
| `packages/emulator/sharc-gen` | `sharc-gen` | The generated SHARC+ core (from `SHARC_GEN_DIR`), compiled apart from the engine. |
| `packages/emulator/sharc-stats` | `sharc-stats` | The counters and profiles the engine reports (shared by both engine instances). |
| `packages/emulator/sharc` | `sharc-native` | The DSP engine: interpreter, AOT block dispatch and the fast tier (`src/fast/`). |
| `packages/emulator/sharc-fast-cl` | `sharc-fast-cl` | Cranelift backend for the fast-tier kernel IR. |
| `packages/emulator/sharc-gen-2`, `sharc-2`, `sharc-fast-cl-2` | `sharc-gen-2`, `sharc-native-2`, `sharc-fast-cl-2` | The second core: the same sources as `sharc-gen`, `sharc-native` and `sharc-fast-cl`, built on `SHARC_GEN_DIR_2` (see "Two cores in one binary"). Empty without that variable. |
| `packages/emulator/sharc-fast-wasm` | `sharc-fast-wasm` | WebAssembly backend for the same IR, plus the V8 gate tools. |
| `packages/emulator/live` | `live-audio` | Live audio output: SPSC ring, frame repeater, cpal callback. |
| `packages/emulator/coupled` | `digi-coupled` | The coupled ColdFire + SHARC+ run loop shared by the app and `coupled-run`; the fixture gates. |
| `packages/emulator/control` | `digi-control` | Controller and control protocol (JSON-RPC over NDJSON); the `digi-emud` daemon. Design: `docs/design/emulator-control.md`. |
| `apps/desktop/src-tauri` | `digiemu-desktop` | The desktop app (Tauri), a thin shell over `digi-control` and `digi-coupled`. |

Left out of the workspace: `.claude/` (agent worktrees live inside the tree and must
not become members). A crate is never built with its own manifest's profile: cargo
reads profiles only from the root.
built with its own manifest's profile: cargo reads profiles only from the root.

## Profiles

- `release`: timing and shipping profile (opt-level 3, cargo's default codegen units,
  unwinding panics: what the desktop app has always used; PGO is trained on it).

There is no cheaper profile for correctness-only runs. Measured on `sharc-gen`
(80 MB generated core): an opt-level 1 override costs about 2.5x the CPU of
release (5.5 min wall vs 2.9 min), and opt-level 0 compiles in about 1.4 min but
runs `coupled_ready_exactness` in 150 s instead of a few seconds (and overflows
the default thread stack). Use `release`.

## Target dirs

`sharc-gen` (the generated SHARC+ core, 80 MB of Rust) takes minutes to compile
and cargo recompiles it for each target dir and each `SHARC_GEN_DIR`. So tools
build into `out/native/target/<gen-key>` (`tools/emulator/native_target.py`):

| key        | generated core                                         |
|------------|--------------------------------------------------------|
| `prod`     | `out/native/dn2-audio/gen` (desktop app, PGO)          |
| `nogen`    | none (`SHARC_GEN_DIR` unset): CF-only builds, wasm CF  |
| `generic`  | any other transpile-only gen (no `image.rs`)           |
| `g-<hash>` | any other gen with an image: hash of its resolved path |

Everything for one gen goes into its key dir, built with plain `--release`
(the desktop app, `-p sharc-fast-cl --examples`, `packages/emulator/sharc` tests, the PGO
control build), so `sharc-gen` compiles once. Builds with different RUSTFLAGS
(PGO) get a subdir of the same key: `pgo-instrument/`, `pgo-use/`. Switching gen
never touches another gen's build.

```
export SHARC_GEN_DIR=$PWD/out/native/dn2-audio/gen
T=$(uv run python -m tools.emulator.native_target dir)
cargo build --release --locked --target-dir "$T" -p sharc-fast-cl --examples
```

In a git worktree the root is `out/native/target-wt/<worktree-name>/` instead
(`out/` is linked to the main tree's, and two checkouts in one target dir would
invalidate each other). A worktree cannot reuse the main tree's compiled
`sharc-gen` -- different package paths and fresh file mtimes make cargo rebuild
it -- so it pays one compile per gen, then reuses its own dir.

A plain `cargo build` without `--target-dir` uses `./target` (ignored by git).
Wrap heavy builds in `uv run python -m tools.devtools.benchlock run --shared -- ...`.

## Two cores in one binary

The generated core is device-specific (symbol numbers and the AOT blocks differ), so a build that opens either
device's firmware coupled holds two: `SHARC_GEN_DIR` and `SHARC_GEN_DIR_2` name them, in either order, and the cargo
feature `second-core` (digi-coupled, digi-control, digiemu-desktop) links both. The `-2` packages compile the
sources of the first ones again (`[lib] path`, `build = ...`) against the second directory, so there are two engine
types and one glue per core: `elektron_native_boot::sharc_peer::{NativeDsp, open_engine_patched}` for the first core,
`sharc_peer::second::...` for the second (one file, `boot/src/sharc_engine.rs`, included twice). The engine instance
the DSP worker runs is chosen when the session starts: `coupled/src/dsp_core.rs` takes the core whose DSP image
name (`dn2-1.11`, `dt2-1.16`) starts with the loaded firmware's device id, so there is no per-instruction cost.
The second instance has no C ABI symbols (cfg `sharc_second`) and reads its own fast-tier lists
(`SHARC_FAST_REGIONS_2`, `SHARC_BLOCKTRANS_2`). The other device's ready inputs go to `digi-emud` and the app as
`--also-coupled DSP_IMAGE DSP_STATE CF_SNAPSHOT SYX` (repeatable): a firmware load for that device then boots coupled
(audio, DSP, restart) instead of ColdFire only. Without `SHARC_GEN_DIR_2` the `-2` packages are empty and every build
is as before. Proof: `uv run python -m tools.devtools.dual_core_check` (the header names the build command). The
desktop launcher builds this by default with `mise run wb` when both devices' inputs are available
(no PGO profile for it yet).

Use:

- `mise run workbench` (alias `mise run wb`; `-- --build-only` builds without launching) builds with `second-core`
  and passes the other device's ready inputs and generated core. It needs both devices' ready inputs (DN2 and DT2) and both
  generated cores; without them the launcher says `second core: ... one core only` and builds one core. The two-core build has its
  own target directory (`out/native/target/<first key>+<second key>`, `tools/emulator/native_target.py`) and no PGO profile
  (`perf:pgo` trains single-core binaries). Choose File can then open either device with DSP audio. Set
  `DIGI_EMU_SECOND_CORE=0` for a single-core desktop build. Daemon launches (`mise run emu:serve`) remain single-core
  by default; set `DIGI_EMU_SECOND_CORE=1` to include the other core there.
- The patched images (runtime, core/dynfx, core/fxpages, core/fx) differ from the image the generated blocks came from. Each
  engine instance checks its own loaded code against its own generated core (`Engine::verify_code`, `code_diff.rs`) and bars only
  the blocks with a changed instruction, so the AOT path works on the second core as on the first (`SHARC_CODE_LOG=1` prints the
  check; the audio report's `dsp_tiers` has `code_mismatch`).
- Process-wide state is shared by both cores through `sharc-rt`: the instruction table (`insn_table`) is parsed once per generated
  blob (one slot each). It was one `OnceLock` for the process, so the core that asked second read the first core's table and the
  code check compared a patched DT2 image against DN2's instructions (493 changed PCs instead of 10; 0.23x against 0.38x real time
  with the runtime patched in). Any new process-wide cache in `sharc-rt` needs the same key.
- The fast tier lists exist for DN2 only (`tools/emulator/fast_regions.json`): `SHARC_FAST_REGIONS` and `SHARC_BLOCKTRANS` go to
  the first core, `SHARC_FAST_REGIONS_2` and `SHARC_BLOCKTRANS_2` (from the `--also-coupled` firmware) to the second. A DT2 core
  has no lists, so it runs AOT blocks and the interpreter only, as a single-core DT2 launch does.
- `dual_core_check` modes (a daemon each, bounded; `DIGI_EMUD` names the two-core `digi-emud`): the default (open the other
  device and back), `--module digikit/pluck --play` (`dk run` and a played note), `--menu digikit/pluck --menu digikit/freqshift`
  (the on-device path SETTINGS > DIGIKIT > MODULES > tick > APPLY & RESTART, a note, the effect's FX page; screenshots to
  `out/runs/lanes/dualcore-shots/` or `--shots DIR`) and `--bench dt2 --binary LABEL=PATH ... --pairs 3` (DSP CPU per audio second, AOT share and real-time factor of a device
  with the runtime patched in, alternating the `digi-emud` builds given; a build with DT2 as `SHARC_GEN_DIR` against one with it
  as `SHARC_GEN_DIR_2` measures what the second-core role costs).

## Headless coupled runner (`coupled-run`) and the matrix

`packages/emulator/coupled` (crate `digi-coupled`) holds the coupled ColdFire + SHARC+ run
loop that the desktop app used to own: the DSP worker thread behind the DSPI2
link, the 250k-instruction step (`STEP_CHUNK`), PCM handling and link timing.
The app (`apps/desktop/src-tauri`) is a thin shell over it, and
`coupled-run` drives the same `CoupledRuntime::step_chunk` loop with no Tauri, no
webview and no audio device, so the numbers it reports are the app's.

The fixture gates (`coupled_ready_exactness` and the Trig 1 tests) live in
`digi_coupled::gates`; the desktop crate wraps them as ignored tests under their
old names (`desktop_runtime::coupled::tests::...`), with the same asserted hashes.

```
export SHARC_GEN_DIR=$PWD/out/native/dn2-audio/gen
T=$(uv run python -m tools.emulator.native_target dir)
cargo build --release --locked --target-dir "$T" -p digi-coupled --features coupled-audio --bin coupled-run
SHARC_FAST_REGIONS=0x1c399a,0x1c3862,0x1c364f,0x1c5765,0xb823db,0x1c1884,0x1cdcef,0x1c3030,0xb85347,0x1c1da8,0xb80509 "$T"/release/coupled-run \
  --syx firmware/Digitone_II_OS1.11.syx \
  --dsp-image out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-dn2-image.bin \
  --dsp-state out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-m5.snap.dsp \
  --cf-snapshot out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-m5.snap \
  --scenario idle --scenario trig1-hold --out out/runs/x/report.json
```

The launcher also exports `SHARC_BLOCKTRANS` for DN2 OS 1.11: 12 block-translator
cluster roots (`0xb8276c,0x1c369b,...`, `BLOCKTRANS_ROOTS` in `tools/emulator/native_emu.py`,
keyed by the .syx SHA-256 like `FAST_REGIONS`). Measured A/B on DSP CPU per audio
second without PGO: 0.958 idle, 0.963 chord16, PCM identical. A user-set
`SHARC_BLOCKTRANS` wins, `DIGI_EMU_FAST=0` turns it off with the fast regions, and
other firmware gets neither. The PGO key (`tools/emulator/native_pgo.py`) includes the roots.
Without the launcher, add `SHARC_BLOCKTRANS=0xb8276c,0x1c369b,0xb809f2,0x1c5796,0xb84a8c,0xb8212b,0xb827b7,0x1c5110,0xb82568,0xb813cf,0xb8040f,0xb826a5`
to the command above.

`--list` shows the scenarios, `--seconds S` overrides the length, `--no-pcm-tap`
skips the PCM hash and statistics and `--no-link-timing` the per-period and link
timers (to A/B their cost). Exit code 2 means a gate
failed (missing blocks, halt, step error, non-finite PCM, wrong expected hash).

### Scenarios (`tests/scenarios/dn2.json`)

A scenario is a length (`seconds` of audio, or `instructions` of ColdFire) and a
list of panel events. Times count from the restored ready point: `at_instr` in
ColdFire instructions, `at_s` in audio seconds (DSPI2 frames sent / 1500). Events
fire at chunk boundaries before the next chunk, so a run is a function of its
inputs. `qa_script` also plays the shared QA sequence of
`packages/emulator/boot/examples/common` (NO at ready and +20M, encoder A at +40M).

| scenario | what it does |
|---|---|
| `idle` | 10 s of audio with no input. |
| `trig1-hold` | QA prelude, then Audition Trig 1 (button 25) pressed at +60M and held to the end. One voice. |
| `kb-chord16` | QA prelude, keyboard mode on (button 18 tapped at +50M), then all 16 trig keys (25..40) pressed at +60M and held: a chromatic chord of several voices. |
| `exactness-100m` | Validation: the `coupled_ready_exactness` workload (QA, Trig 1 down at +60M, up at +100M, 100M instructions). Its PCM hash must be `b3f5230f...4d03f` (cold-start reference state, 2026-10-07; was `3b600f6d...7f267` on the old state and `38d2a322...3ff1f8` before the Type 17 SIMD fix). |
| `trig1-noqa-40m` | Validation: `desktop_trig1_capture_without_qa_script` (Trig 1 from ready, 40M instructions, no prelude; it sounds, rms 0.032). Hash `2861ce03...c6f21` (cold-start reference state; `a50586e8...b75a` on the pre-2026-10-06 state). |

The QA prelude is no longer needed: on the current ready state Trig 1 sounds
with no prelude (the second validation scenario). It was needed only with the
2026-10-03 snapshot, whose card opened an SLC dialog that swallowed the key. `kb-chord16` is the heavy best effort. It is
heavier in audio (distinct pitches 277, 311, 370, 415, 466 Hz ... against one
262 Hz tone; PCM RMS 1.3-1.5x `trig1-hold`) but only slightly heavier in DSP
CPU: the DSP cost is dominated by a constant background (the idle-skipped RTOS
loop, the fast-tier region and the generated blocks that run with no voice at
all), so a voice adds a few percent. Pressing the 8 trig keys without keyboard
mode retriggers one pitch and is not heavier. Recording a pattern by button
script and playing it was tried and does not sound reliably (the first notes are
accepted, later button events are not), so it is not a scenario.

### Report

`report.json` has one object per scenario run: `audio_seconds`, `wall_seconds`,
`real_time_factor`; `cpu` (exact thread CPU time of the DSP worker and of the
ColdFire step thread from `CLOCK_THREAD_CPUTIME_ID`, total and per audio second);
`link_timing` (the `DN2_PROFILE_LINK` counters, as deltas over the run);
`dsp_instructions` (idle-skip / interpreter / AOT block / fast tier counts and
percentages, plus fast-tier builds and compile time); `cf_instructions`
(interpreted / idle fast-forward / ram-clear); `dsp_period_wall` (per-period DSP
wall time against the 0.667 ms deadline: count over, mean, p50/p90/p99/max);
`pcm` (SHA-256, RMS, peak, 100 ms window RMS series); `gates`.

Measurement overhead is a handful of timer reads per 0.667 ms period (the link
timing the app already has behind `DN2_PROFILE_LINK`, plus one per-period wall
time) and one hash/sum pass over the PCM on the ColdFire thread. Nothing runs
per instruction. The engine's tier counters and the worker's CPU time are read
once at the start and once at the end of a run. Measured A/B on `prod` (6 s
`trig1-hold`, three pairs, exclusive): the PCM tap changes ColdFire CPU per audio
second by under 0.01 (0.64 against 0.64); the link timers cost 0-3% of DSP CPU
(1.98 against 1.94 on average), inside the 1.84-2.07 run-to-run spread.

### Matrix (`tools/emulator/coupled_matrix.py`)

```
uv run python -m tools.emulator.coupled_matrix                       # full default matrix
uv run python -m tools.emulator.coupled_matrix --configs prod --scenarios idle --repeats 1
uv run python -m tools.emulator.coupled_matrix --pgo out/pgo/x.profdata
```

It builds `coupled-run` once per configuration (the cargo target dir from
`tools/emulator/native_target.py`, under a shared benchlock), runs every scenario under the
exclusive benchlock, one process per run, repeats outermost so drift spreads over
all configurations, and writes `out/native/runs/<timestamp>/{report.json,
summary.md,raw/}`. Configurations: `prod` (`out/native/dn2-audio/gen`,
`SHARC_FAST_REGIONS` from the launcher's table, 10 s, 3 repeats), `generic`
(`gen-generic`, interpreter only, 1.5 s, 1 repeat) and `generic-auto` (generic plus
`SHARC_FAST_AUTO=256,64`, 1.5 s, 1 repeat). The interpreter-only configurations run at
about 0.02-0.04 of real time, so their defaults are short; the full default
matrix takes about 10.5 minutes (621 s measured). The validation scenarios run once per
configuration; all configurations must produce the same hash. `--pgo PROFDATA`
builds every configuration with that profile (`pgo-use` subdir, `--target`).
Each run records the 1-minute load average; the summary flags runs that started
under load, because benchlock only excludes work that also takes it.

### Browser core tiers

The coupled browser core (wasm32) has a fast tier too (feature `fast-wasm`): its
kernels are wasm modules the page compiles (`packages/emulator/sharc-fast-wasm`, `apps/web/src/core-host.ts`).
`SHARC_FAST_REGIONS` and `SHARC_BLOCKTRANS` are environment variables, which
wasm32-unknown-unknown does not have, so the page passes the regions in
(`digi_sharc_fast_regions`). With no `?fast=` query it uses the `desktop` regions
of `tools/emulator/fast_regions.json` for a listed firmware (AOT blocks present), or
run-time selection `4,2,1` on the generic core. `node tools/devtools/wasm_parity.mjs --scenario NAME` (or
`mise run web:parity NAME`) checks that the browser PCM is byte-identical to
the native `expect_sha256`.

The coupled core (`emulator-core-sharc.wasm`) contains firmware-derived DSP code.
`tools/emulator/native_wasm.py` writes it to `apps/web/.local/`, which is gitignored
and served only by the dev server. It is never in `dist/`: a build guard fails
the build if it appears in the output. The CF-only core stays in
`apps/web/public/`.
