# Handover 2026-10-06: dev cycle, audio sink and scenarios

## State

The desktop noise is output starvation, not a DSP fault (findings 07,
2026-10-06). Playback now rebuffers by default. The build/run/debug loop is one
set of mise tasks. Nothing is committed.

## What changed

- Audio sink (`native/live/src/audio.rs`, `native/boot/src/pcm_play.rs`):
  - `--audio-mode rebuffer|stretch|gap`, default rebuffer:
    - rebuffer: fade out, wait until the buffer refills (`--audio-buffer`,
      clamped 0.2-0.9 s), fade in
    - stretch: resample to the arrival rate, so pitch drops instead of gaps
    - gap: the old behaviour
  - Sink counters and an underrun/resume event timeline in the audio status.
  - `DIGI_EMU_SINK_WAV=path` captures what the device played.
  - `DIGI_EMU_PCM_F32LE=path.wav` captures the emulator's own PCM as WAV, with
    no 10 s limit (`DIGI_EMU_PCM_SECONDS` caps it).
- DSP counters (`sharc_peer.rs` `live_report`): the worker publishes tiers and
  CPU every 150 frames. Status polls no longer drain the pipeline. `dsp_tiers`
  with `dsp_cpu_per_audio_s` is on every audio status. The panel shows the
  recent real-time factor, underruns, refilling, and DSP CPU per audio s.
- Input record/replay:
  - `CoupledRuntime::button/turn` record inputs at ColdFire instruction counts.
  - Export diagnostics carries `recorded_scenario`.
  - `mise run replay FILE` (coupled-run `--replay`, `exact_events`) re-renders
    the session headless, exactly.
- Scenarios:
  - `native/coupled/src/scenario.rs` is shared by coupled-run and the app.
  - New `trig1-taps` and `trig1-tap-release`, with hashes.
  - The desktop app has `--scenario NAME|FILE --report PATH`: it runs a
    scenario in the real app and writes a report, an app-like benchmark.
- Tier bisection: `SHARC_AOT=0`, `SHARC_AOT_DENY=pc,...`. `mise run bisect NAME`
  compares against a cached interpreter-only reference and toggles fast regions,
  cluster roots and AOT.
- Tooling:
  - `tools/digi_env.py` holds the toolchain (from mise.toml), paths, firmware
    and the fast tables. The fast tables live in `config/emulator.json`.
  - The launcher builds once and runs the binary.
  - `--build-only` (`mise run emu:build`).
  - The panel build is skipped when unchanged.
  - RUSTC is pinned on every path, wasm included.
  - No auto-retrain: a stale profile is used with a warning, and `mise run pgo`
    retrains.
  - Fast-region env is out of the PGO key.
  - `tools/coupled.py` provides build/scenario/replay/bisect/test-quick.
  - `tools/wasm_parity.mjs` and `mise run wasm:parity`.
  - The region_profile dead-code warning is gone.
- PGO retrained on this batch (profile 12f75c87...). `verify` is ok.

## How to use

- `mise run emu`: build once and run the desktop emulator.
- `mise run emu:build`: the same build, no launch.
- `mise run web`: build the browser core and serve the web panel.
- `mise run wasm`: build only the browser core.
- `mise run wasm:parity NAME`: check the browser core's PCM against native, for
  example `trig1-tap-release`.
- `mise run pgo`: retrain the PGO profile (about 6 min), at milestones.
- `mise run scenario NAME`: run a headless coupled scenario, for example
  `trig1-taps`, optionally with `--wav`.
- `mise run replay FILE`: replay a recorded session headless.
- `mise run bisect NAME`: find the DSP tier that changes a scenario's PCM.
- `mise run test:quick`: fast tooling, sink and coupled-runtime checks.

To pass app flags, put them after `--`: `mise run emu -- --audio-mode stretch`.
mise appends the extra arguments to the task's command.

## Open

- Not yet exercised in the GUI: rebuffer/stretch by ear, the panel status line,
  recording then replaying a real session, `--scenario` in the app.
- The 45 `tests/test_sharc_*` failures predate this work.
  `out/native/opt/target-final/release/libsharc_native.dylib` (Oct 1) lacks
  `sharc_native_profile` (added 551bc45). The rebuild is the `REGENERATE_HINT`
  command in `tools/sharc_transpile_run.py` (regenerates gen-final, about 4 min).
- The browser runs its own stepping loop (`native/boot/src/abi.rs` plus
  `packages/web/src/runtime/runtime-worker.ts`). The desktop and headless use
  `digi-coupled`. Sharing it needs a feature split of `digi-coupled`: no threads
  or cpal on wasm.
- The coupled `?audio=1` browser page has still not been run in a real browser.
- The second `mise run emu:build` after a cold build recompiled digiemu-desktop
  once (12 s); the third was a no-op. Not diagnosed.

## Next

1. Em listens with `mise run emu` (default rebuffer) and tries
   `--audio-mode stretch`, then Export diagnostics after a few taps and
   `mise run replay` on it.
2. Real-time headroom: the cheap fast-region sweep has stopped paying (about 6 %
   ceiling). Real gains need structural work: AOT that commits state once per
   block, or a JIT-first engine. Alternatively, find why fast.run is never
   reached at 0xb8a9xx/0xb8abxx.
3. A shared stepping loop for the browser, then a real-browser listening test.
4. Regenerate the AOT core from coverage that includes note events and UI
   interaction, at the next milestone; this needs a PGO retrain too.
