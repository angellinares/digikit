# Device test reload cost, 2026-11-10

Question: why does a reload between device tests cost 25 to 40 s under gate load, when a boot-snapshot hit is documented at 0.2 to 1.5 s?

**[D]** Answer: the slow reloads are snapshot misses. A miss is a cold boot of the card in emulated time, run inside the daemon's `firmware.load` call. The harness around it (status, guard, run mode, ready checks) costs about 0.02 s. A hit costs 0.2 s. The cache is not shared: a snapshot lives in the run dir of the daemon that booted it, and the snapshot key holds the card, so each distinct card of each file boots cold once per run dir.

## Data

Input: the 170 task outputs of the full gate of 2026-11-09 (PR 275 lane, coupled DSP default then, 6 emulator slots, machine loaded). The per-file table
below was computed once from those outputs (a throwaway script over the `runtime-phase` records, finding 69; no tool is kept). Durations are inclusive.

A reload now also writes `reload.firmware-load` (the `firmware.load` call alone; fields `startUsed`, `cacheHit`, `cacheDir`, `card`) and
`reload.other` as child records of `session.baseline-reload` (`tests/regression/support.ts`, `Session.lastReload`). The 2026-11-09 outputs predate
them: there a reload below 5 s counts as a hit (the durations are bimodal).

## Result of the gate outputs [D]

44 files had phase records. 277 reloads: 88 below 5 s (hits), 189 slow, 6,268 s in total (33 s mean). Files that reuse the daemon record no
`session.startup`; the first load of the daemon is a reload too. Top 10 by slow reload time (hits excluded):

| File | Reloads | Hits (< 5 s) | Slow reload s | Median reload s |
| --- | ---: | ---: | ---: | ---: |
| bootscreen | 32 | 8 | 1125 | 26.0 |
| introblank | 24 | 6 | 766 | 25.8 |
| fx-routes-panel | 10 | 0 | 440 | 44.2 |
| card-safety | 29 | 10 | 360 | 12.7 |
| capacity | 16 | 4 | 336 | 15.2 |
| fx-track-route | 22 | 12 | 319 | 1.7 |
| freqshift | 8 | 2 | 296 | 29.2 |
| fx-routes-persist | 6 | 0 | 282 | 47.0 |
| fx-pages | 6 | 0 | 280 | 47.4 |
| fx-chain | 6 | 0 | 273 | 44.9 |

Next: safe-mode 257 (4 reloads, median 65 s), settings 238, fits 200, fx-reuse 185.

The bootscreen order shows the pattern (DT2, seconds): 25.9, 21.5, 2.7, 21.4, 2.4, 26.1, 2.8, 27.8, 2.7, 26.8. Each test loads the first card (a hit after the first
test) and then its own card (always a miss). DN2 gives 70 to 75 s per miss and 0.7 to 1.1 s per hit; DT2 gives 22 to 27 s and 2.4 to 2.8 s. The number of slow
reloads of a file equals its number of distinct cards per device, not its number of tests.

## Explanation

**Snapshot key.** `card_snapshot_key` and `card_identity` (`packages/emulator/control/src/controller.rs`) hash the card path, size, mtime and the sha256
of `overlay.bin`; the coupled key `patched_pair_key` adds the MAIN digest, DSP image, cold state and button state. The snapshot is the file
`<run dir>/patched/<key>` (`controller.rs`, `join("patched")`). A card that the guest wrote to, or one in a new directory, has another key: that is the reason for
the content-named cards in `tests/regression/runtime/card-cache.ts`. `cardRig`, `fx-session` and `fx-live` use them, so the key is stable between runs.
No key is computed wrongly: a miss on a card never seen in that run dir is correct.

**Where a miss happens.**
1. Each test of a file that boots its own card (bootscreen files, safe-mode, settings records, fx route presets) needs a cold boot of that card. Two tests with
   the same card hit; this is the 88 fast reloads.
2. The snapshot lives in one run dir (`dist/session-cache/<daemon key>/<device>-<n>`, `claimDir`, `run-claim.ts:162`). A file gets the first free `n`. With 6
   slots a file lands on any of 6 dirs per device, and `claimDir` keeps only the newest `PATCHED_KEEP = 8` entries of a dir (`run-claim.ts:12`) at each claim.
   Over a gate of 44 files with many distinct cards per device, a snapshot of an earlier file or an earlier gate is not found again.
3. A new daemon build changes the key dir (`sessionBase`, `devices.ts`); a core change changes the MAIN digest. Every PR with a core change starts cold.
4. A fresh lane has no `dist/session-cache`.

**Not the cause.** Daemon launch and card preparation are outside the reload (reusing files record no `session.startup`, and cards come from the content-named cache). The reload path `reloadRuntimeBaseline` (`packages/test-support/src/session.ts`) is cheap after a restore. Module loading
is not replayed after a restore: a hit restores the emulator state after the modules loaded (the card holds them, the runtime loads them during the boot).
Slot waits happen in `emu-slot.mjs` before vitest starts, so no phase record includes them.

## Live check [D]

One run of `core/runtime-settings-tests:test-device--settings-api` (zero DSP, 12 reloads, 6 distinct cards over both devices) on a warm cache: 12 of 12
`snapshot-cache` hits, `reload.firmware-load` 0.16 to 0.20 s, `reload.other` 0.02 s. In the gate, the same file had 8 of 12 reloads below 5 s and 68 s slow
reload time. The cold run before it was not recorded (Nx hides task output without `--output-style=stream`), so the new records have not yet seen a miss; the cost of a miss (22 to 75 s) comes from the gate outputs.

## Fix: one shared snapshot store [D]

Replaced the snapshot directory of each run dir (`<run dir>/patched`, 8 newest entries per dir) with one store per daemon build and device,
`dist/session-cache/<key12>/snapshots/<device>`. Entries keep their content key (`card_snapshot_key`, `patched_pair_key`), which does not hold the run dir.
- Daemon: `DIGI_SNAPSHOT_DIR` sets the snapshot root (`Actor::snapshot_root`, `controller.rs`); without it the root stays `<run_dir>/patched` (desktop app, other
  users of the daemon). The ColdFire-only load now also reports `cache_dir`. Entries were already written as `.part` then renamed. This changes the daemon build
  key: the first run after the change is cold.
- Harness: `claimDir` gives each claim the store (`RunDirClaim.snapshotDir`, passed as `Session` option `snapshotDir`) and prunes it to 6 GiB, oldest
  first (`pruneSnapshots`; a hit touches its entry). `PATCHED_KEEP` and the per-dir pruning are gone.
- Failure rule: a session remembers the entries that its cold boots wrote (`Session.writtenSnapshots`); when the file fails, `releaseClaim` removes them, as
  `claimDir` renames a failed run dir. A concurrent file could restore such an entry before the failure shows; the same exposure existed inside one run dir.
- Entries are 66 to 74 MB (zero DSP); 6 GiB holds about 80 per device.

Proof on the lane (zero DSP, one emulator run at a time; the first run is cold because the daemon key changed):

| Run | Reloads | Hits | Misses | Reload total |
| --- | ---: | ---: | ---: | ---: |
| settings-api, cold store | 12 | 8 | 4 (2 cards per device), 10 to 12.5 s each | 43.1 s |
| settings-api, again | 12 | 12 | 0 | 2.3 s (0.17 to 0.20 s each) |
| settings, with `dt2-0` and `dn2-0` locked, so it ran in `-1` run dirs | 28 | 16 | 12 (each new card once) | 125.5 s |

The blank card that `settings` shares with `settings-api` hit on its first load in the other run dir (8 of 8 loads on both devices); every new card missed
once, then hit. The 2026-11-09 gate (loaded machine) had 68 s of slow reloads for settings-api and 243 s for settings. Miss times here (10 to 12 s) are lower
than the gate's because the machine was less loaded.

Not changed: a first run of a cold gate still boots every distinct card once, and a core change that changes the MAIN digest still starts cold. What the store
removes is the repeat of a boot per run dir, per rerun and per equal card in another file. Fewer distinct cards and a warm-up task (earlier options 2 and 3)
remain open. **[O]** The 6 GiB cap and the failure rule were not exercised against a gate-size load.
