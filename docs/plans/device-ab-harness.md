# Device A/B harness and the `wb` CLI

Status: plan, agreed with Em on 2026-11-02. Not started. Do it after the SHOT sampler branch merges.

## Goal

One fast, reusable way to check what an engine really outputs, on the device and in the emulator, against a known reference. It serves every new engine (samplers such as granular, synths, LFO and modulation work), and it gives Em and agents the same evidence: numbers and pictures.

Every device action goes through one CLI, `wb` (`tools/devtools/wb.mjs`), which runs the workbench's own code (dk-session TS, the shared Rust crate `packages/dkmidi`). The separate Python device scripts are removed once `wb` covers them.

## What we learned on 2026-11-02 (why this exists)

- A project LFO on SRC TUNE made SHOT sound different on every trigger. Hours went into static traces before Em found it on the MOD page. A pitched control machine (PLUCK) exposed it.
- ffmpeg's avfoundation input dropped about 13% of the Digitakt II USB audio (a gap every ~70 ms). Sweeps looked 1.14x fast with clicks. PortAudio (`sounddevice`) records exactly. `tools/device/devprobe.py` now uses it and reports captured vs wall-clock seconds.
- Remote key sequences were sent blind. The Python screenshot path (`tools/device/dkscreen.py`) returned a stale frame once, and there is no parameter read-back, so a key that landed on the wrong screen was not caught. One remote encoder detent is about 1/6 of a knob unit and turns can accelerate, so the reached value was unknown.
- A device restart reloads the saved project: unsaved changes (an LFO set to none, a sample on a track) come back.

## Pieces

1. `wb` commands (all through dk-session + dkmidi):
   - `wb screen [--out PNG] [--watch]`: the workbench's screenshot code (`DkSession.screen`, `decodeScreen` in `packages/dk-session/src/dkupload-core.ts`). Measure the round trip first; the Remote page waits a fixed 400 ms (`SCREEN_MS` in `apps/desktop/ui/src/RemotePage.tsx`), so its frame rate is about 2 fps.
   - `wb key LABEL [--hold MS]`, `wb turn ENC DELTA`: `remoteKey` / `remoteTurn`, labels from the faceplate data the Remote page uses.
   - `wb param get|set TRACK WORD`: a read-back of machine parameters through the runtime's `param_get`/`param_set` (new SysEx service command; the runtime already has the calls).
   - `wb samples ls|put` (exists), `wb flash`, `wb transfer`, `wb log`.
2. Navigation with feedback: a helper that presses a key, then waits until the expected screen or parameter value appears, and fails on a timeout. No fixed-delay key sequences.
3. Known state: a clean test project per device (saved on the card), and a check of the track's LFO destinations, p-locks, arpeggiator and trig mode before a run.
4. Capture and analysis: PortAudio capture (as in devprobe), per-press windows aligned to the key events, and one analysis library on `tools/emulator/audiocheck.py`: level, length, pitch, sweep rate fit, L/R, start point, end discontinuity.
5. Fixtures: a 1 s linear sweep (100 to 2000 Hz) for rate, start and direction; a pure tone for pitch and level; a clap for transients; a two-part file (440 Hz then 880 Hz) for live direction changes. Uploaded with `wb samples put`.
6. A/B modes: device vs emulator (same steps, same fixture), engine vs stock machine on the same track, before vs after a change.
7. Report: a spectrogram with the expected curve overlaid, per-hit tables, and a pass/fail summary, written to `out/runs/<harness>/<time>/` and optionally published as a page.
8. Consolidation: move devprobe's keys and screens onto `wb`, then delete `tools/device/dkscreen.py`'s device path, the remote part of `tools/device/dkupload.py`, and the scratch analysis scripts.

## Also queued

- `patchkit flash` waits up to 4 minutes per phase without output after the device restarts (`packages/patchkit/src/flash_cli.rs`, `after_device_flash`, `wait_port`). Poll INFO from the start, print progress, and do not rely on CoreMIDI's port list.

## Steps

1. [ ] `wb screen`, `wb key`, `wb turn`; measure the screenshot round trip.
2. [ ] `param_get`/`param_set` service command and `wb param`.
3. [ ] Navigation helper with screen/value waits.
4. [ ] Fixtures and the analysis library; port devprobe onto `wb`.
5. [ ] Report output.
6. [ ] Delete the replaced Python device scripts.
7. [ ] Flash wait fix.
