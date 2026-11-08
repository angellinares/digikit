# 116. Nx test and gate speed measurements

Scope: measurements of 2026-11-07/08 on an M3 Max (12 performance and 4 efficiency cores, 128 GB). Marks: **[D]** measured or read once, **[O]** open. PR numbers are references.

## Nx cache

- **[D]** `test-device` task hashes are identical across worktrees at the same commit. All 22 tasks of `digikit/remote:test-conformance` matched between a lane and main.
- **[D]** A rerun in the same checkout replays in about 2-3 s.
- **[D]** Before PR #332, plain `pnpm nx:pilot` runs never wrote the remote cache, so results did not cross worktrees. From 2026-11-03 to 2026-11-07, 1 of 175 device or conformance rows replayed. All 132 remote lookups for these targets missed.
- **[D]** `affected` selects a target by its inputs. A no-op parent target with no inputs dropped out of the selection. The per-file parent therefore keeps the inputs of the whole project.

## Per-file device targets (PR #331)

- **[D]** An edit to one core/dynreg test file reran only that file (2m11s).
- **[D]** An unchanged rerun replayed in 2.7-3.2 s.

## fast-roots (PR #331)

- **[D]** A fixture miss took 17 min inside the check task. 11 min of that was a hidden compile of the plain daemon.
- **[D]** With the fixtures as an Nx task that uses the daemon from `digi-control:daemons`, a miss takes 3m45s. With stored fixtures it takes 21 s.
- **[D]** Interpreted instructions per audio second were identical with both daemons: dn2 first-boot 2,407,312; dn2 reset 3,100,841 (6,392,691 block entries); dt2 first-boot 1,834,555.

## Suites in parallel

- **[D]** `fx`, `fx-no-aot` and `fx-dt2` together took 556 s. Run alone, they take about 767 s.
- **[D]** `fx-dt2` still waits for `digi-coupled:build-dt2`, which runs alone.

## Reload costs

- **[D]** A cold boot to ready takes about 38 s on DN2 and 14 s on DT2 in coupled SHARC mode. In zero-DSP mode it takes about 8 s.
- **[D]** A snapshot restore takes 0.5 s (DN2) and 1.4 s (DT2) coupled, and about 0.2 s in zero-DSP mode.
- **[D]** In `store-format`, 5 of 7 baseline reloads per device were cold. They were about 68% of the time of the file. The card contents really differ, so these boots were needed.
- **[D]** The snapshot key of the daemon used the card path and mtime. Each file ran in a new temp dir, so no snapshot survived a run.
- **[D]** `store-format` on zero-DSP with `test/c-demo` (PR #332): 381 s to 122 s. DT2 went from 103 to 60 s, DN2 from 278 to 58 s.
- **[D]** A module with a DSP part never reaches loaded in zero-DSP mode. The DSP transfer cannot complete (`E_DSP_XFER`).
- **[D]** Snapshot cache across runs (PR #340): on a rerun, `store-format` dropped from 5 of 7 cold reloads per device to 1. `fx-reuse` restored every reload.

## Patchkit object cache

- **[D]** `out/cache/dko` was keyed by module path. A worktree at another commit overwrote the shared objects. A bisect did this.
- **[D]** PR #340 keys the entries by source content.

## Disk

- **[D]** The test harness left about 2,600 `digi-*` temp dirs (about 111 GB allocated).
- **[D]** Time Machine local snapshots kept the deleted blocks. Cleanup freed no space until the snapshots were deleted.
- **[D]** The harness refuses to start below 50 GB free.

## Gate time

- **[D]** Full merge gates took about 140 min on 2026-11-05, before these changes. They took 61-92 min on 2026-11-08, on different loads.
- **[O]** No paired A/B was run, so the gain is not proven.

## Open: freqshift on DT2

- **[O]** `freqshift` on DT2 has failed on main since PR #288 (first bad commit `34583f6b`): `turning ENC A: value 25344, wanted 26624`.
- **[O]** The cause is not found. The native-page turn hook and the `source_draw` and `source_code` hooks are ruled out.
