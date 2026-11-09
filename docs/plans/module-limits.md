# Module limits (plan, 2026-10-31)

Goal: modules are limited only by what the firmware allows; each limit has one source; every usage number we show is measured correctly.
Facts: [finding 100](../findings/100-module-limits.md). Branch: limits/single-source. It lands before the sampler branch; that branch then merges
`main`. Step 5 starts after the sampler branch is merged.

## Status at end of session (2026-10-31)

Steps 1 to 3 are done on the branch (not merged, not pushed). Step 4 is next. Before merging: `mise run verify -- --base main --explain`, then the
routed gate once. Not yet run: the emulator health proof (`tools/coldfire/digikit_health_proof.py`), the capacity and FX scenarios
(`pk_digikit_capacity.py`, `pk_digikit_fx.py`), and a look at the desktop Memory panel and Console (their preview needs the web build).

## 1. Findings

- [x] Finding 100, stale passages replaced with pointers (f998fdce).

## 2. One source for every limit

Sources: `sdk/sdk.toml` (contract, format and protocol limits) and `devices/*.toml` `[limits]` (per device). Everything else is generated, imported or
read from CAPACITY.

- [x] `[limits.max]` gains `machine_rows` and `stage_bytes`; the runtime registry, patchkit's register check and `.dkm` size read them (5c69d720).
- [x] `[protocol.limits]` generated into every language; the copies in dkupload, dk-session, dkupload.py and upload.rs are gone (5c69d720).
- [x] `pk_capacity.py`, `pk_conform.py` read the device files; stock DSP busy counts in `devices/cycle-factors.json`; dyndsp voice rows tested
      against `machine_rows`; DN2 keeps the L2 cache range out (30e9c6d9).
- [x] The fit rule exists once, in `dkrt-fit` (web: `pk_fit`; tools: `patchkit fit`); `dkfit.ts`, `dkfit.py` and their tests are gone (46b7bb98).
- [x] `[limits.format]`: the loader's format limits generated into dkneeds, Python, TypeScript and C (`sdk/include/dk/gen/limits.h`); tests that
      tested themselves removed (73013d5d, c1e49779).
- [x] `patchkit gen --check` runs in the gate and in `test:changed` (c1e49779).

## 3. Usage numbers that are measured correctly

- [x] Heap LARGEST exact (the walk starts at the highest order) (035302d0).
- [x] HEALTH page: CPU marked relative, DSP marked as an assumed 1 GHz block; largest free block shown when it is smaller than the total (035302d0).
- [x] Desktop Memory panel: largest free block and an over-limit state (035302d0).
- [x] Emulator load meter: window peak; the Console labels the estimate and shows the model figure (035302d0).
- [ ] Re-measure stock DSP busy on 1.17 / 1.12 and record the free cycles per block in finding 100.

## 4. Raise the limits that are only our numbers

Each with one proof run on DN2, then DT2.

- [ ] DN2 DSP area to 448 KiB (below `0x200fa000`).
- [x] Parameters: DT2 `param_slots` `[1,2,4,5,6]`; DN2 `[1,2,4,5,6,0,3,7]` (words 0 to 4 on the same letters on both devices). [ ] Check that kit and project save and load keep the new knobs.
- [ ] ColdFire arena and staging: size from the measured runtime footprint inside the expansion region (DT2 1.33 MiB in all, DN2 much more).
- [ ] `param_rows`, `page_rows`, `machine_rows`: raise with the dynreg and dyndsp entries; size from RAM cost.

## 5. Contract changes (after the sampler branch is merged)

- [ ] DN2: several pages per machine and more than 8 parameters (per-machine slot maps; `DK_MAX_PARAMS`).
- [ ] The capabilities from PR #209 (value text, track sample, stereo voice, all 8 knobs, chords): [capabilities plan](capabilities.md).
- [ ] L1 placement for hot code.
- [ ] DT2 DSP area above 512 KiB (wider offset field or a second window).
- [ ] DDR for modules: through the sampler branch's work.

## 6. Gaps to revisit

- [ ] Arena holes after boot: freed DSP code buffers leave holes for the session (finding 100, section 5); they affect only run-time `alloc` in
      modules and the largest free block shown. Fix if needed: allocate those temporary buffers from the top of the arena.
- [ ] The steps list of a module with DSP code is not in its needs (tens of bytes): add it to `dkneeds`.
- [ ] Build cache: `patchkit build` keeps a cached object as "unchanged" when only the Selache revision changed (`toolchain/selache_rev`
      is not part of the check in `object_unchanged`, packages/patchkit/src/build/mod.rs), while `--check` rebuilds and finds it stale. After
      a Selache update the gate then fails on old objects in `out/cache` (seen 2026-10-31: freqshift, pluck, sine and 20 others). Fix: include
      the toolchain revision in the unchanged check. Workaround: `patchkit build --force` on the module.
- [ ] Rust formatting: `rust_ws fmt` reports about 190 diffs on `main` in files this branch did not touch; the check is red regardless.
- [ ] Gate: a failed python-tests file is not recorded in `failed_units` of `result.json` (the 2026-10-31 run listed three failed files in its
      summary and an empty `failed_units["python-tests"]`), so `--rerun-failed` reruns no file. Fix in tools/devtools/verify.py.
- [ ] Gate: `--rerun-failed` after a change of only docs and one web test helper added `smoke` with the full matrix ("full matrix for
      explicit/full/rerun acceptance"). Check the routing of the changed files and whether a rerun needs the full matrix.
