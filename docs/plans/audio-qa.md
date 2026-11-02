# Module audio QA (design draft, 2026-11-02)

Status: draft for Em's review; nothing built. Separate from the test consolidation work.

## Goal

When a machine or FX module changes, Nx runs that module's audio suite: render it in the emulator
over a parameter sweep, analyse the audio, and fail on faults or on drift from the module's
baseline. Developers see the same analysis in the workbench, for emulator renders and later for
audio captured over USB from a real device.

## Decisions

- One analysis core in Rust: a small crate with no emulator dependencies (it rebuilds in seconds).
  The same crate serves three places: a CLI for the gate (JSON, optional PNG heatmaps for people and
  agents), a Tauri command for the workbench, and WASM for the web build. This follows
  `packages/dksample` (Rust, native and `apps/web/public/dksample.wasm`) and may reuse its WAV reader.
- Python stays for exploration only: design and calibrate metrics on renders (numpy, matplotlib,
  the existing `fxcheck` code). A metric moves into the crate once it proves useful. The gate and
  the workbench use only the Rust version.
- The workbench never runs Python. The UI draws spectrograms from data that Rust computes.

## Metrics

1. Faults (fail): NaN or Inf; digital clipping (runs of full-scale samples); true peak above
   0 dBTP; DC offset; silence where sound is expected and sound where silence is expected; clicks
   and discontinuities; zipper noise during parameter changes; aliasing on a sine sweep; tails that
   do not decay (FX feedback runaway); negative stereo correlation.
2. Per-module limits (thresholds in the module): THD and THD+N on a test tone, noise floor and SNR,
   crest factor, loudness consistency over each parameter's range (no large jumps or dead zones).
3. Palette and drift (baseline per module): spectral centroid, rolloff, flatness, bandwidth, MFCCs,
   loudness, envelope attack and decay over a parameter grid. Compare with the stored baseline and
   flag drift; the same data shows how much timbre the parameters cover.

Perceptual quality is not measured directly; listening stays with the developer.

## Selection and cost

- The Nx module plugin (`tools/nx/modules.mjs`) adds a `test-audio` target to modules tagged
  `type:machine` or `type:fx`. A module change runs its suite; a core module change runs its
  dependents through the graph.
- Emulator time dominates, not analysis. Render many parameter points in one boot (one session,
  scheduled parameter changes), not one boot per point.
- Later: cache renders by module and emulator inputs (SHARC output is deterministic) and analysis by
  WAV hash. Not before the graph and the metrics are stable.

## Open questions

- First metric set for the gate (proposal: the fault list only, then limits, then palette).
- Baseline storage and update rule (PCM pins need Em's approval; descriptor baselines may differ).
- Does the device appear as a class-compliant USB audio input (needed for capture with `cpal`)?
- Which existing Python audio oracles (`hook_ab`, `pk_digikit_fxsend` added-factor,
  `pk_digikit_fxshift`, `fxcheck`) move into the crate first.
