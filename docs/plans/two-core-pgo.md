# Two-core PGO profile for the desktop build

**Status:** done (2026-11-01). Results in finding 68, section 'Two-core PGO profile (2026-11-01)'.

## Problem

The workbench builds both devices' generated cores into one binary (feature `second-core`,
`SHARC_GEN_DIR` and `SHARC_GEN_DIR_2`). `pgo_plan` in `tools/emulator/native_emu.py` returns no
profile plan when `SHARC_GEN_DIR_2` is set, so the desktop build runs without PGO. Single-core
profiles give 19% (DT2) and 3% (DN2) on the DSP thread; finding 68 measured a plain two-core
build against non-PGO single-core binaries only.

## Design

1. Profile identity (`tools/emulator/native_pgo.py`)
   - New profile state `out/native/pgo/both/<triple>/` (device name `both`).
   - `compute_components` adds `generated2` (tree digest of `SHARC_GEN_DIR_2`) and the
     feature `second-core`; the workload name is `both-v1`.
   - `GUARDED_CRATES` gets `sharc_native_2` and `sharc_gen_2`, so `crate_mismatches` checks the
     second engine instance. `CRATE_NAME_RE` already builds its pattern from the list.
   - The slot order is fixed and part of the key: slot 1 (`SHARC_GEN_DIR`, `sharc_native`) is
     always the DN2 core, slot 2 (`SHARC_GEN_DIR_2`, `sharc_native_2`) always the DT2 core,
     whichever firmware launches. A profile matches symbols by crate hash, and the crate hash
     depends on which generated core is in the crate, so each launch order would otherwise be a
     separate binary and profile. The fast-region lists follow the firmware's device (DN2
     unsuffixed, DT2 `_2`, `digi_env.slot_env`).
   - `local_package_dirs` reads `cargo metadata` with the `second-core` feature, so the
     second-core crates are in the key.
   - `check_state`, `pgo_state`, `plan_for` and `pgo_binary` take the second core into account;
     the target dir comes from `native_target.target_dir(gen, variant, second)`, which already
     keys on both cores.
2. Training binary: an instrumented `coupled-run` built with `--features second-core` (the
   coupled crate supports a second core), not the desktop app: the desktop app has no DT2
   workload, and a profile only applies when the engine crate hashes match. `DT2_APP_UNPROFILED`
   shows that the engine crates of `coupled-run` and the app already match; the check in step 5
   proves it for the two-core pair.
3. Workload (`DT2_WORKLOAD` style list, one process per item, firmware picks the core):
   - DN2: the `exactness-100m` scenario of `tests/scenarios/dn2.json` (the coupled-run copy of
     the `coupled_ready_exactness` gate), not the desktop app workload, and one DN2 runtime
     replay.
   - DT2: `dt2-sample-trig` and the FX scenarios of `DT2_WORKLOAD`, plus one DT2 runtime replay
     with sound (patched image, the dsp-voice and converter patches, one voice held).
   - Runtime replays need a ready dir with a patched snapshot pair and the matching patched
     `.syx`. Replay fixtures come from `mise run perf:fixture -- --device dn2|dt2`
     (`tools/devtools/runtime_fixture.ts`). It cold-boots the patched runtime through `Session`
     and writes `out/fixtures/runtime/<dev>/` with `provenance.json` (file hashes, source
     head). Training checks the hashes and records the provenance in the manifest;
     `--fixture-dn2` and `--fixture-dt2` override the paths. Replays: DN2 `trig1-hold` 5 s,
     DT2 `dt2-sample-trig` 3 s with the fixture card. Regenerate the fixtures from a clean tree
     after a patch-set change.
   - Set the fast-tier defaults of each device through `digi_env.fast_env` for the stock
     firmware, as the workbench does, so training sees the same tiers as use.
4. `native_emu.pgo_plan`: remove the early return; for a two-core launch use device `both`.
   Missing or stale profile keeps the current fallback to the plain two-core build and its
   notice. The new task `perf:pgo:both` trains the two-core profile. `perf:pgo` runs dn2, dt2,
   both, then the launcher build.
5. Verify: `native_pgo check --device both`; after the profile-use build run
   `verify_binary` (crate hashes) on `digiemu-desktop`. A mismatch on `sharc_native_2` means the
   profile did nothing for core 2. `verify --device both` ignores `elektron_native_boot`, like
   the DT2 check. `GUARDED_CRATES` includes `sharc_native_2` and `sharc_gen_2`.

## Cost

| Step | Cost |
| --- | --- |
| Code and unit tests (steps 1, 2, 4; manifests, key, notices) | 3 to 4 h |
| Workload list and replay fixture hook (step 3) | 2 to 3 h |
| Cold two-core instrumented build, plain lane target | 25 to 40 min once, 5 to 15 min later |
| Training (3 runs of both devices) | about 15 min, quiet machine |
| Profile-use desktop build and crate check | 10 to 15 min |

## Risks

- A lane's target dir is cold: the first two-core build compiles both generated crates
  (about 25 minutes, 15 cores). Build in the main tree's warm key dir only with Em's go-ahead.
- The patched-runtime replay changes with each patch change; the profile then trains on a
  stale distribution. Profile data of changed functions is skipped (partial match), so the harm
  is a lower gain, not wrong code.
- Instrumented counters make runs about 2 to 3 times slower; the training scenarios use short
  bounded `--seconds`.

## Commands for the timing step (run only when Em allows timing)

Timing of the DT2 root change (commit `45c4811f`), same frozen binaries, only the environment
differs (`docs/TESTING.md` "Resumable SHARC matrix"): in the matrix configuration set

```json
"variants": {
  "A": {"SHARC_FAST_FALLBACK_REGIONS": "0x1c2607"},
  "B": {"SHARC_FAST_REGIONS": "0x1c4ed2", "SHARC_FAST_FALLBACK_REGIONS": "0x1c2607,0x1c1fe7,0x1c204b"}
}
```

with cells `dt2-stock`, a DT2 runtime idle replay and a DT2 runtime sample replay, then:

```sh
mise run perf:matrix -- run out/native/runs/<date>-two-core-pgo/config.json --baseline out/native/runs/<date>-two-core-pgo/frozen --candidate out/native/runs/<date>-two-core-pgo/frozen --out out/native/runs/<date>-two-core-pgo/results
```

The runtime cells use the fixture inputs in `out/fixtures/runtime/<dev>/`.

Training for the existing single-device profiles, to compare against the two-core build later:

```sh
mise run perf:pgo:dn2
mise run perf:pgo:dt2
```

Two-core training (after the implementation above):

```sh
uv run python -m tools.emulator.native_pgo train --device both
uv run python -m tools.emulator.native_pgo check --device both
uv run python -m tools.emulator.native_emu --build-only
```
