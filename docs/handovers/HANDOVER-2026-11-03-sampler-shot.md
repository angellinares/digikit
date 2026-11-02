# Handover 2026-11-03: SHOT sampler (branch sdk/dsp-sampler-discovery)

## State

- Branch `sdk/dsp-sampler-discovery`, local only (not pushed since 56e272fb). origin/main merged in at c4efdf3c (0 commits behind).
- `nx.json`: a stray local write from the Nx session (separate worktree) is in `git stash` ("stray nx.json write from Nx session (kept for Em)"). Ignore it; stage by explicit path only.
- SHOT works on a real Digitakt II (OS 1.17), checked with the device probe:
  - rate 1.00 on a 1 s sweep, 10 of 10 hits identical, like the stock one-shot;
  - level equal to stock on the same track (`DK_SAMPLE_STOCK_GAIN`), end ramp like stock;
  - PLAY replaces REV: REV, REV.L, FWD.L, FWD (stock PLAY pictogram on DT2; DN2 shows an encoder 0..3); loops repeat [START, END) seamlessly (SDK pcm16 kernel loop option);
  - REV/PLAY, START and END are live every block; SMP is latched at the trigger (finding 114).
- Elektron Transfer files load (header check matches the stock loader; PCM length from the header).
- The browser lists faster (several listing steps per UI pass).
- Knob glyphs: `DK_PARAM_GLYPH(OWNER, WORD, GLYPH)`: encoder (default), fader, pan, on/off, tune, play.

## Tools added this session

- `tools/device/devprobe.py` (`mise run device:probe`): remote keys and turns, USB audio capture through PortAudio (ffmpeg's avfoundation input dropped about 13%), per-hit analysis. See docs/INSTRUMENTATION.md "Device probe".
- `tools/devtools/wb.mjs` (`mise run wb`): workbench actions through the workbench's own code (dk-session + the shared crate packages/dkmidi): ports, info, samples ls/put (convert, upload, verify).
- Runtime log kinds 37-40: POOL_LOAD, POOL_PUBLISH, POOL_LOST, POOL_INVALID (`tools/device/dklog.py follow --once`).
- Scripted device flow: `uv run python -m tools.patchkit.patchkit flash firmware/Digitakt_II_OS1.17.syx --port Digitakt --yes` (works, but its post-restart wait can hang silently: plan device-ab-harness), `uv run --with python-rtmidi python -m tools.device.dktransfer --port Digitakt install digikit/shot` then `... restart`.

## Lessons (also in memory)

- Check the track's LFOs, p-locks and arp before debugging pitch on hardware; a device restart reloads the saved project, so save after changing them.
- Do not send remote key sequences blind; use screen and value read-back (plan device-ab-harness).

## Next (in order)

1. Run the full routed gate once on a quiet machine (`mise run verify -- --base main`; `--explain` selects all 18 steps because the branch touches emulator, runtime, SDK and patchkit). Known open item: `modules/machines/shot/tests/shot-dn2.device.test.ts` fails with "no reply from the device" during the upload stage under load (also before the merge; passed on a rerun earlier). Investigate if it fails on a quiet machine: DN2 daemon log in the kept run directory, the dk-session upload timeout.
2. Em's OK, then push and open the PR to main.
3. After the merge, from the plans:
   - docs/plans/device-ab-harness.md: `wb screen/key/turn`, `wb param`, navigation with feedback, fixtures, report; consolidate the Python device scripts into `wb`.
   - docs/design/param-views.md: live/latched parameter views in the voice adapter (needs Em's OK).
   - docs/plans/digikit-project.md: per-project persistence (`.dkp`), module persistence API.
   - SMP: name on the knob through the value-text hook (the stock sample glyph only shows a bank letter and number and needs raw storage that DN2 cannot show).
   - The 13 sampler/DSP proof tests in tests/regression/runtime/*.test.ts run only by name (Vitest project `proofs`); move them into their modules' tests folders and register them.
   - Loop budget: the loop option raised SHOT's per-block ceiling to 7000/7200 (worst case a 1-frame loop wrapping on every output); trim the wrap path.
   - Track 1/2 level difference (9.2 dB in the emulator): find the cause.
