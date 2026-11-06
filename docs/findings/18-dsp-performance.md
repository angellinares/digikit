# 18. DSP performance: measurement, cost model, state at freeze

Branch `work/sharc-emulator` at 911b1fc (performance work frozen for cleanup and
merge). Sources: the lane notebooks for the fast tier, perf tool, call path,
helpers, chaining, generated lowering, AOT profile and class (e); they are folded
into this file and removed. Marks as in CLAUDE.md: **[V]** measured here at 911b1fc,
**[D]** measured earlier on a lane base commit (cited), **[O]** open or predicted.
Scenario for every number: DN2 1.11, `trig1-taps` (10.5 audio s; 502M executed
SHARC insns per audio s, another 498M idle-skipped, free).

## How to measure

- `mise run perf -- --config NAME [--config NAME ...] [--seconds S] [--timed] [--no-diag]`
  (`tools/devtools/perf.py` on `tools/devtools/fastcov.py`). Per config: a clean run (DSP CPU per audio
  s, PCM sha, tier split, region calls) and a diagnostic run (SHARC_FAST_HIST and
  SHARC_FAST_COV: interpreted insns, calls and kernel insns per function) that ranks
  functions by estimated cost. Report: `out/runs/perf/<stamp>/report.md` and `perf.json` (see docs/INSTRUMENTATION.md).
- Configs: `default` (AOT blocks + verified regions: the shipped desktop build),
  `static-blocks|static-insns[+follow][+gen][+aot]` (fast regions armed at every block
  start or instruction, AOT off unless `+aot`), `fast-auto:256,64`, `env:K=V;...`,
  `<config>+env:KEY=VALUE`, and `hybrid-top:N` (default config plus fast regions
  armed only at the static block starts of the top N functions of the cost ranking,
  taken from a cached fast-only diagnostic run, `--rank-seconds`).
- `--gen prod` is required for any AOT config (the shared core has no firmware blocks).
- `--timed` takes the exclusive benchmark lock; milestone numbers only. Unlocked runs
  are good for coverage (deterministic) and for A/B inside one session, not for
  absolute CPU. Run 1.5 s for coverage, the full 10.5 s for CPU: at 1.5 s compile time
  dominates the fast-only configs.
- `--calibrate` fits the cost model below (5 configs, least squares, kernel ns fixed
  at the gate's 0.5), writes `out/native/perf/calibration.json`.
- `--regions` (build with `region-profile`): wall time per function; `--device dt2`.
- Pinned PCM at 10.5 s: `d60ad3390038a84b`. Other pins: 3 s `9c3b55b48ca49d41`; 1.5 s
  fast-only `3bf3ba3026953c22`; 12 s `8cbae26b73014732`.

## Cost model

| item | ns | basis |
|---|---:|---|
| interpreted insn | ~105 (profile 116-119) | **[D]** fit; step loop 29 + exec_insn 25 + generated core 66 |
| region call (engine round trip) | 130 by profile, 266 fitted | **[D]** fit also absorbs declines |
| kernel insn (Cranelift) | ~0.5 | **[D]** gate 0.49 |
| AOT insn | ~4.4 | **[D]** 46.6% of insns take 3.07 of 3.64 s region time |
| generated helper (body 6 + sync 5) | ~11 | **[D]** laneF microbench; interpreter-step body ~140 |

CPU = interp x 105 + calls x 266 + kernel x 0.5 + AOT x 4.3. It lands within 4-11% of
measured CPU on the calibration runs and 6-25% over on the runs below. **[V]**
Call path, phases per region call **[D]**: entry checks 8, requirements 13, window
resolution 22, kernel 22 (13 of it its own work), flag replay 6, exit bookkeeping 6,
other 5, plus run_inner/wants/step share 40-60: 100-130 ns. No single phase is large.

## State at freeze **[V]** (timed, exclusive lock, prod gen, 10.5 audio s)

| config | DSP CPU / audio s | PCM | region calls/s | AOT / fast / interp (% of executed insns) |
|---|---:|---|---:|---|
| default | **1.584** | d60ad3390038a84b | 0.62M | 46.7 / 52.9 / 0.37 |
| hybrid-top:10 | 3.845 | d60ad3390038a84b | 4.06M | 59.3 / 37.3 / 3.37 |
| hybrid-top:20 | 4.251 | d60ad3390038a84b | 4.92M | 36.7 / 59.2 / 4.02 |
| hybrid-top:50 | 4.614 | d60ad3390038a84b | 6.76M | 23.7 / 71.6 / 4.68 |
| fast-only (static-insns+follow) | 7.037 | d60ad3390038a84b | 20.96M | 0 / 94.4 / 5.57 |

Fast-only has the same PCM as the default here.
The headline desktop metric is 1.584 DSP CPU per audio s: **target <= 1.0 not met**
(base 2085854 measured 1.46; the two runs are on different sessions, so the 8% gap
is inside the noise of this machine; lane F's 1.41 was another session again).
Hybrid-top is worse than default at every N: the AOT code it displaces runs at 4.4
ns/insn with almost no calls, while each armed function brings 4-7M region calls/s
(215 ns residual each) and 3-5% interpreted insns (about 1.5-2.5 s). The lowering is
exact in all five rows; it is a cost question, not a correctness one.
Other measured points:
- ColdFire CPU **[D]**: 0.52 s per audio s native.
- Wasm kernel gate **[D]**: 0.50 ns/insn in V8 (TurboFan) against 0.435 Cranelift
  native (limit 1.2); Liftoff-only 2.96 ns/insn until TurboFan finishes; compile
  about 1 ms per kernel, 104 us for the Rust-side emit.
- Browser **[D]**, Node 24/V8, two instances over a SharedArrayBuffer, `idle`
  scenario, fast tier on, no AOT: DSP worker ~39 s per audio s, ColdFire worker ~1.3
  s. One instance without the fast tier: 140 s. Browser DSP at <= 0.6 needs the
  interpreted share <= 0.1% at 105 ns, or <= 1% at 10 ns/insn.

## What worked

- Per-range AOT invalidation **[D]**: a code patch bars only the regions with an
  entry in a bad range; trig1-tap-release with a patch kept 697M of 701M insns in AOT,
  CPU 4.53 vs 3.99 s unpatched (was: all AOT barred).
- Per-block AOT invalidation **[D]** (2026-10-24): on a range whose hash differs the engine decodes the loaded
  words, compares them with the generated instruction table and bars only the regions with a block that holds a
  different instruction (`sharc/src/code_diff.rs`; stock parity 0 differing PCs on DN2 1.12 and DT2 1.17). The DN2
  `core/fx` hook (range 3482 words) costs x1.013 instead of x2.38 (finding 55, route A).
- Generated lowering (laneF, `tools/sharc_dev/sharc_lowergen*.py`, 24 forms) **[D]**: fast
  89.5 -> 93.4% (92.5% on 1.5 s) and interpreted insns -29%; one semantics source
  (`tools/sharc_dev/sharc_core`), fuzzed against the interpreter; on by default.
- Region-call back-off after window declines **[D]**: calls 27.5M -> 16.6M/s, CPU
  11.13 -> 10.08 (-9%), PCM identical.
- Class (e) volatile-M fix **[D]**: an M register that fails its baked value twice
  is not baked again (`region::mark_volatile`), and a call that exits at its first
  insn four times turns it into a helper: 8.82 -> 7.19 -> 6.93 CPU/audio s, built
  regions 8317 -> 5291, req declines 4.05M -> 0.58M per 1.5 s.
- Generated helper bodies **[D]**: 6 ns mean against 62 ns for the interpreter step;
  ~11 ns per helper insn with sync. Gain where used is small (-5% in 0x1c8ef1).
- SIMD semantic fixes **[D]**: both PEs for 7a, 9a/11a; fuzz found 14a store dropped,
  PEy compare shifting PEx CACC, a circular tail-write bug (halt at 0xb88ea8).

## What did not work

- Form-family lowering **[D]** moved coverage (83 -> 87 -> 93%) but CPU stayed
  11.25 -> 11.5: more guarded windows and built regions (1686 -> 2846) cost as much
  as the interpreter time saved. Coverage is not the metric; calls and interpreted
  insns are.
- Trimming the call path **[D]**: 20 ns per call is not reachable from 100-130 ns by
  tightening run_cfg; no phase exceeds 22 ns. Needs fewer calls or chaining.
- Flat guarded access (`SHARC_FAST_FLAT`) **[D]**: exact, no coverage gain, about 25
  IR ops per access, Cranelift compile 5.8 -> 31 s. Off by default.
- Blanket helpers **[D]** (all `3a`): +0.30 s; 1 ns guarded hits became 140 ns.
- Hot-function HLE **[D]**: DN2 top 20 functions are 84% of executed insns (79.5% of
  AOT region time), ~21 functions for 80%; DT2 needs ~35 (top 20 = 70%). Not viable.
- Mask-free AOT emission **[O]** (superseded: slices 0-2' below and in the design doc; concrete registers were
  dropped, hot-code density gave -15% without PGO and -9% with PGO): predicted AOT 70 -> 22% of the DSP thread, 4.4 ->
  ~1.5 ns/insn, default 1.41 -> ~0.75; +-30%, 1-2 weeks, changes the transpiler V type.
  **[D]** Design and redone ceiling in `docs/design/aot-mask-free.md` (2026-10-09, nothing built):
  the estimate rests on one xctrace category split; warm region benchmarks already reach
  1.4-2.3 ns/insn and the single ablations (undo copies, budget check) are worth 0-9%, so the
  in-situ 4.0 ns may be code size (about 1.1 KB machine code per block insn) and region entry
  rather than masks. Central estimate default 1.05-1.2, 0.8-0.9 optimistic; slices 0 and 1 (2 days)
  decide.
## Where the time is **[D]**

Default at 1.58: AOT 234M insns x 4.4 = ~1.0 s, kernel 266M x 0.5 = 0.13 s, 0.62M
calls and 1.9M interpreted insns the rest. Fast-only exit classes per audio s (laneB
chaining table, 16.95M calls, 8.9 s): (a) exit at another region's entry 6.21M calls,
1.76 s; (a2) call/return/jump structure stops 2.43M, 0.74 s; (a3) exit to a non-entry
0.32M, 0.34 s; (b) refused instruction 3.19M, 1.22 s; (c) guard failure in a region
2.72M, 1.02 s; (c') entry-requirement declines 1.33M, 0.35 s; (d) shape declines
0.49M, 0.13 s; (e) 36.5M interpreted insns where no region call happens, 3.8 s
(40%), untouched by chaining. About 70% of interpreted PCs lie after mid-region exits.

## Next levers, ranked

Desktop (target default <= 1.0, ideally 0.7):
1. Mask-free AOT emission: 1.58 -> ~0.8-0.9. Confidence low-medium (+-30%). The only
   lever aimed at the 1.0 s that is AOT, now 65% of the default CPU. High effort/risk.
2. Fast kernels for the hottest AOT functions without calls: needs (3) first; today a
   function moved from AOT to fast costs more than it saves (hybrid rows).
3. Chaining, class (a)+(a2)+(a3), 8.96M calls/s: -2.2 s of 9.5 fast-only at 20 ns per
   transition (-1.0 s by the profile cost). Step one is a Rust-loop chain that skips
   Engine::step and run_inner (~50 ns), then kernel-to-kernel jumps (~20 ns).
   Confidence medium for step one, low for 20 ns.
4. Continue class (e) and mid-region exits: each fix so far gave -10 to -20% on
   fast-only (volatile M: 8.82 -> 6.93). Confidence medium; no effect on default.
5. Generated compute/MR helper bodies with flag sync (+10 ns): ~20% of the
   interpreted insns that end regions; fast-only -1 to -2 s predicted, confidence low
   (flag state would need dynamic known-bit masks in the tracer).
6. hybrid-top coverage: no gain at any N; drop unless chaining makes calls cheap
   (then re-measure with `--config hybrid-top:N`).

Browser (no AOT possible without firmware; fast tier only):
1. Interpreted share to <= 0.1% (or <= 1% with a ~10 ns generated interpreter): the
   DSP worker's 39 s is ~97% interpreter; the fast tier cut 140 -> 42. Needs 4 and 5
   above plus the missing forms; confidence medium.
2. CF interpreter speed: 1.3 s of 1 s budget is already a CPU core; a faster
   interpreter loop (cached decode, fewer machine hooks) is needed before DSP matters.
   Confidence medium, effort moderate.
3. Chaining (3 above) in the wasm backend: imported calls are 2.7 ns, so kernel to
   kernel jumps in one module look cheaper than native; confidence low-medium.

## Per-FX coverage in the no-AOT config **[D]**

Measured 2026-10-08 on branch `work/scenario-oracles` (DN2 1.11, shared generated core, no
firmware blocks). Not re-checked by a second agent. Tools: `tools/devtools/perf.py --scenarios FILE
--scenario NAME... --seconds 0` (0 = each scenario's own length) and `tools/devtools/perf_agg.py DIR...`
(union and FX increase over the dry hold). Scenarios: `tests/scenarios/dn2.json`,
`REF:tests/scenarios/dn2-trackfx.json`, `REF:tests/scenarios/dn2-sendfx.json`.

Config: **`static-insns+follow`** is the translator-only, no-AOT configuration (finding 18's
"fast-only"; fast regions armed at every instruction, generated lowering and helpers on, AOT
off). It needs no per-firmware core, so the shared core is enough. Check: `trig1-taps`, 1.5 s,
5.48% interpreted (finding: 5.57% at 10.5 s; the full run here gives 5.57% too), PCM
`3bf3ba3026953c22` at 1.5 s as pinned.

Counters are deterministic (untimed, shared lock, three runs in parallel). The CPU column is
the finding-18 cost model (105 ns interpreted, 266 ns per call, 0.5 ns kernel) and is 15-35%
high on this config; the two timed rows (exclusive lock) anchor it.

| scenario | audio s | interp % (no AOT) | calls/s | est. CPU s per audio s | interp % (default) | est. CPU (default) |
|---|---:|---:|---:|---:|---:|---:|
| trig1-taps | 10.50 | 5.57 | 20.95M | 8.75 (timed 6.52) | 0.37 | 1.50 (meas. 1.47) |
| tf-dry-hold | 4.10 | 5.50 | 19.67M | 8.38 | 0.25 | 1.42 |
| tf-br-127 | 4.10 | 5.47 | 19.71M | 8.40 | **1.24** | 1.94 |
| tf-over-127 | 4.10 | 5.50 | 19.67M | 8.38 | 0.25 | 1.42 |
| tf-srr-127 | 4.10 | 5.42 | 19.69M | 8.39 | **1.88** | 2.28 |
| tf-del-127 | 5.90 | 5.46 | 19.60M | 8.34 | 0.25 | 1.42 |
| tf-rev-127 | 5.90 | 5.46 | 19.60M | 8.34 | 0.25 | 1.42 |
| tf-chr-127 | 5.90 | 5.46 | 19.62M | 8.35 | 0.25 | 1.42 |
| sf-dry-hold | 4.36 | 5.50 | 19.67M | 8.38 | 0.25 | 1.42 |
| sf-del-fb-d127 | 6.16 | 5.46 | 19.63M | 8.35 | 0.25 | 1.42 |
| sf-rev-decay-u20 | 6.16 | 5.45 | 19.62M | 8.34 | 0.25 | 1.42 |
| sf-chr-depth-d127 | 4.36 | 5.52 | 19.86M | 8.44 (timed 7.26) | 0.25 | 1.42 |

Timed (exclusive lock, no-AOT, unloaded machine): `trig1-taps` 6.52 CPU s per audio s,
`sf-chr-depth-d127` 7.26 (the 4 s run carries more compile time per audio s); both gate ok,
PCM unchanged from the untimed runs. The model over-estimates this config by 34% and 16%.
Default-config runs were sequential but unlocked; the "est." column is the model, measured
CPU was 1.47 (trig1-taps), 1.39 (dry hold), 2.21 (`tf-br-127`), 1.52 (`tf-over-127`) and
2.23 (`tf-srr-127`).

What this shows:
- **No-AOT, the FX do not change the picture.** Every scenario sits at 5.4-5.6% interpreted
  and 19.6-21M region calls per audio s. The FX code is part of a chain that runs whatever the
  parameter values; the interpreted cost is the base DSP chain's, which the dry hold already
  has. The interpreted tail to cover is therefore the same list for every FX.
- **With AOT, br and srr are the exceptions.** `FUN_b8258f` (313 insns, br) and `FUN_b826c8`
  (272 insns, srr) are cold in the AOT core: interpreted 0 insns/s on the dry hold,
  5.0M and 8.3M insns/s with the effect up. That is +0.9% and +1.6% interpreted and about
  +0.5 and +0.9 s of estimated CPU per audio s (default 1.42 -> 1.94 and 2.28).
  Overdrive, delay, reverb and chorus (track and send pages) add no cold code in the AOT core
  (at most 8k insns/s in `FUN_b80432`).
- Without AOT, the same two functions are translated (70k insns/s for b8258f in `tf-br-127`,
  35k in `FUN_1c07c6` for srr), so a translator-only build is not worse than AOT on them.
- **Fix [V by pins, counters from one run]: FX-active profiles (2026-10-09).** The DN2 AOT
  core was regenerated from the old inputs (cov6/ent9/trans9) plus `coupled-run
  --profile-execution` profiles of `tf-br/srr/over/del/rev/chr-127` and `sf-del-fb-d127`,
  `sf-rev-decay-u20`, `sf-chr-depth-d127`, `sf-mst-thr0` (default config, current core;
  `tools/sharc_dev/aot_fxprof.py`, merged by sum per key, same rsgen flags). Blocks 1751 -> 2657 (20722
  -> 38378 insns; gen 90 -> 122 MB, so the `sharc-gen` compile is longer). Default config,
  `perf.py --gen prod --config default`, interpreted share: br 1.24% -> 0.33%, srr 1.88% ->
  0.24%, dry hold 0.25% -> 0.25%. `FUN_b826c8` is now 0 interpreted; `FUN_b8258f` fell from
  3.7M to 0.44M insns/s (its loop body at 0xb825f2..0xb82658 still runs in the interpreter;
  +46 ms/audio s, not resolved). Measured CPU s per audio s (unlocked, other jobs running):
  br 2.21 -> 1.61, srr 2.23 -> 1.51, dry hold 1.39 -> 1.51. The dry-hold figure went up with
  the larger core (or the load), so check with an exclusive timed run before quoting it. DT2
  was not regenerated: it has no FX scenarios to profile.
- **Send-FX coverage from a live daemon [V, 2026-10-30], "Send-FX coverage".** The coupled scenarios run no runtime, so
  a send effect module (`dyn-fx-gain-send`, `freqshift`) ran about 85% of its instructions in stock functions that no profile
  had seen: DN2 fell from about 1000 to 40-99 DSP blocks/s. Recipe for DN2 1.12 (DT2: same with its card image):
  1. Build `digi-emud` with `SHARC_PROFILE_PREFIX` support (this tree) and start it with the runtime firmware and
     `SHARC_PROFILE_PREFIX=out/native/dn2-audio/fxprof-send/NAME`.
  2. Wait 45 s for the boot. Install the module over SysEx (`dk.put`, `select`, `restart`, `waitForBoot`) and run
     100 s of audio. The restart makes a second engine, so every run writes two file sets. Do this once per module
     (`NAME` = `gainsend` for `dyn-fx-gain-send.dkm`, `freqshift` for `freqshift.dkm`). The module takes the send route by default.
  3. Merge into the old profile: `uv run python -m tools.sharc_dev.aot_fxprof --out out/native/dn2-audio/fxprof-send
     --base-cov .../fxprof-s3/prof-fx --base-entries .../prof-fx.entries --base-trans .../prof-fx.trans
     --live .../fxprof-send/gainsend --live .../fxprof-send/freqshift` (counts are summed per key).
  4. Regenerate with the usual `sharc_transpile` and `sharc_rsgen` recipe (`--coverage/--entries/--transitions` = the merged files).
  Result: 2692 -> 2789 blocks; the daemon with `dyn-fx-gain-send` went from 97-101 to 600-660 blocks/s; PCM identical on all
  DN2 pins and scenarios. Not covered: a module on the master route, module unload.
- **A/B of the larger core [V, timed, exclusive lock, 2026-10-09].** Old (`gen-pre-fxprof-20261009`,
  binary in `out/native/target/g-e220e7a2`) vs new (prod gen), same sources, `coupled-run`
  alternated old/new, 2.0 s each, DSP CPU s per audio s (median, min-max): br-127 2.04
  (1.97-2.15) -> 1.62 (1.53-1.64), 7 pairs, -21%; srr-127 1.94 (1.92-1.99) -> 1.47
  (1.45-1.51), 7 pairs, -24%; dry hold 1.439 (1.41-1.57) -> 1.444 (1.42-1.63), 20 pairs, +0.3%
  (inside the IQR 1.42-1.46 / 1.43-1.48); trig1-taps 1.424 (1.41-1.47) -> 1.430 (1.41-1.46), 20
  pairs, +0.4%. PCM sha256 is identical old vs new in every run of all four scenarios. The
  earlier "dry hold 1.39 -> 1.51" was load noise. **Recommendation: keep the larger core**
  (large FX wins, no measurable cost elsewhere; the price is a longer `sharc-gen` compile).
  Script: scratchpad `ab.py` over `perf.timed_run` (`native_pgo bench-bins` runs the PGO
  workload test binary, not `coupled-run`).
- `FUN_b84099` (saturator) and `FUN_b8478d` (FX orchestrator) are not in the top 50 of the
  cost ranking in any scenario: they are covered. `FUN_b83f72` (delay writer) is rank 4.
- `tf-dry-hold` vs the del/rev/chr scenarios differ by the voice (hold vs tap); the negative
  "extra" in `perf_agg` for those is that, not an FX saving.

Ranked uncovered (interpreted) functions, no-AOT config, union of the 12 scenarios, by mean
interpreted cost (interp insns x 105 ns). Share is of the 8.4 s estimate. "Baseline" is
trig1-taps and the dry holds; no top-10 function is FX-only. The refusal is fastcov's reason
at the function's hottest interpreted PC.

| # | function | size | interp insns/s | est. ms per audio s | share | what | why the fast tier stops |
|---|---|---:|---:|---:|---:|---|---|
| 1 | 0xb82d1a | 1582 | 4.43M | 465 | 5.5% | loop kernel, unnamed | deny at 0xb832e3: form 3a, loop with a register step that is not stable |
| 2 | 0x1c2712 | 2633 | 3.9M | 409 (+193 calls) | 4.9% | unnamed | stop at 0x1c2df3: form 14d (not lowered) |
| 3 | 0x1c4f04 | 568 | 2.26M | 237 | 2.8% | unnamed | stop at 0x1c50e5: 3b "cannot be lowered in this region" |
| 4 | 0xb83f72 | 295 | 1.86M | 196 | 2.3% | delay writer (finding 15) | stop at 0xb8403a: form 2a_short, a compute opcode with no lowering |
| 5 | 0x1c54c7 | 779 | 1.85M | 194 | 2.3% | unnamed | stop at 0x1c5619: 8a_rel with a compute in the delay slot (4a) |
| 6 | 0x1c3911 | 254 | 1.54M | 161 | 1.9% | unnamed | stop at 0x1c39e0: 3a "cannot be lowered in this region" |
| 7 | 0x1c8ef1 | 3202 | 1.49M | 143 (+1072 calls) | 1.7% | unnamed, the largest call source (3.8M calls/s) | stop at 0x1c99a6: 2a_short compute |
| 8 | 0x1c37b4 | 326 | 1.32M | 139 | 1.7% | unnamed | stop at 0x1c38bd: 3a "cannot be lowered in this region" |
| 9 | 0x1c35a7 | 303 | 1.08M | 113 | 1.3% | unnamed | no stop recorded: never selected or dropped |
| 10 | 0x1c244d | 709 | 0.79M | 78 (+235 calls) | 0.9% | unnamed | deny at 0x1c2680: 3a, address range unknown |

Together the ten are about 2.1 s of the 2.9 s estimated interpreter time (the interpreted
share is 5.5% of 502M insns per audio s). Reasons group into five: 3a/3b accesses the lowering
cannot place (ranks 3, 6, 8, 10: "cannot be lowered in this region", "address range unknown"),
a loop with an unstable register step (1), compute forms without a lowering (2a_short: 4, 7),
form 14d (2) and a compute in a branch delay slot (5). Names for the 0x1c.... functions were
not looked up in findings 06/15; `FUN_1c8ef1` is the biggest call source, so chaining matters
more there than coverage.

Recommendation: the FX are not a separate coverage problem in the no-AOT build; fix the base
chain list above, in this order: (a) the 3a/3b "cannot be lowered in this region" and
"address range unknown" refusals (ranks 3, 6, 8, 10, about 0.6 s); (b) the 2a_short compute
opcodes at 0xb8403a and 0x1c99a6 (ranks 4, 7); (c) form 14d at 0x1c2df3 and the 8a_rel
delay-slot compute at 0x1c5619 (ranks 2, 5); (d) the stable-step loop in `FUN_b82d1a`
(rank 1, the single largest, 5.5%). Together the ten are 25% of the estimated CPU. The interpreted
tail needs to fall from 5.5% to 0.1% for the browser (see "Next levers"), so this list is a
start, not the goal: the rest is a long tail (top 20 are 78% of the estimated cost in
this config, but each remaining function is under 1%). Separately, `FUN_b8258f` and
`FUN_b826c8` are the only FX code that the AOT desktop build misses; a regenerated AOT core
or hybrid arming of those two functions fixes the desktop.

Batch 1 of the refusals (2026-10-09) **[D]**, no-AOT `static-insns+follow`, `trig1-taps`,
shared core, first opt-in `SHARC_FAST_FLATAUTO=1` (config suffix `+flat` in `tools/devtools/fastcov.py`), default since the flip below. Not
re-checked by a second agent.
- Root causes. The four "3a/3b cannot be lowered" stops were not one refusal. (1) Address
  range unknown: a pointer loaded from memory or passed in a register has no static range and
  no anchor register (0x1c2680 `R1 = DM(I2,M6)`, I2 loaded from a stack slot; 0x1c399a,
  0x1c3862 the same; 0x1c5054), or a constant base with a data-dependent modifier (jump-table
  loads `I12 = DM(I4,M4)` at 0x1c50f1 and 0x1c5284, I4 constant, M4 = R8). The region ended
  there (`deny`). (2) Window decline at run time, the larger part: 0x1c3911 and 0x1c37b4 (roots
  never ran: 569 and 284 declines, then back-off) and the roots 0x1c4fbe..0x1c5016 of
  `FUN_1c4f04` declined `window: not plain/present RAM (write)`. A static store window is the
  hull of all stores of the region and `Mem::window` needs every byte of the hull
  overlay-written; the hull covers stores on paths that never ran for that object (first byte
  and last byte neither present nor dirty), so the region declined forever.
- Fix, translator only (`packages/emulator/sharc/src/fast`): (a) `flat_auto`: an instruction refused for
  "address range unknown" is re-lowered as a flat guarded access (the existing
  `Lower::flat_access`, per-access page-table and bitmap guards) at that PC only, not for every
  access as `SHARC_FAST_FLAT` does; (b) a root whose windows decline 3 times is rebuilt with
  every window access flat (`Env::flat_windows`), so a hole in a hull costs one guard exit on
  the path that touches it; (c) `Abs::as_addr`: a constant above 2^31 was sign-extended
  (`I4 = 0x8052d790` gave the range -2142053488), so its `NwPlain` requirement and window range
  failed for ever; it is now read modulo 2^32 (needed once (a) let regions reach such code:
  `FUN_1c8ef1` went 2.1M to 6.4M interpreted insns per 1.5 s without it).
- Result (1.5 s): interpreted 5.48% -> 0.99% of 502M insns/s, region calls 19.5M -> 17.2M/s,
  PCM `3bf3ba3026953c22` unchanged. Interpreted insns/s of the four: `FUN_1c4f04` 2.26M ->
  0.08M, `FUN_1c3911` 1.54M -> 0, `FUN_1c37b4` 1.32M -> 0, `FUN_1c244d` 0.79M -> 0.06M. It also
  covers most of the next ranks (`FUN_b82d1a` 4.43M -> 0.04M, `FUN_1c2712` 3.9M -> 0.15M,
  `FUN_b83f72`, `FUN_1c54c7`, `FUN_1c35a7`, `FUN_1c8ef1` 1.4M -> 0.24M).
- Timed (exclusive lock, 10.5 s, no diagnostics): 6.63 -> 5.00 DSP CPU s per audio s (-25%), PCM
  `e0e6c8af7a84c443` in both (the current `trig1-taps` pin). A 1.5 s run shows 15.7 s per audio
  s because the flat kernels are larger: Cranelift compile time is paid up front, so judge it on
  the full length. For the browser that compile cost has to be re-measured with the wasm backend.
- Exactness: oracle suite (`tests/scenarios --slow`, 61 passed) and `scenario --all` pins ok in
  this config on the shared core; `cargo test` in `packages/emulator/sharc` 64 passed (two new unit tests).
  Default (AOT) config not measured: the flag is opt-in, `as_addr` is always on.
- Default flip (2026-10-09) **[D]**: `SHARC_FAST_FLATAUTO` is now on unless set to `0`
  (`+flat` in fastcov is a no-op; `env:SHARC_FAST_FLATAUTO=0` turns it off). Default config
  (AOT core + fast tier), one binary, env on/off, exclusive lock, alternating pairs, full scenario
  length, DSP CPU s per audio s (median, IQR): `trig1-taps` on 1.393 (1.392-1.397) vs off 1.395
  (1.392-1.399), 10 pairs; `tf-br-127` on 1.433 (1.413-1.445) vs off 1.443 (1.415-1.527), 10 pairs.
  No slowdown, no gain (AOT covers these regions). No-AOT `static-insns+follow` `trig1-taps`: on
  5.66 (5.14-5.67) vs off 7.32 (6.49-7.36), 5 pairs, so the batch-1 gain holds. PCM identical in
  every run (`e0e6c8af7a84c443`, `16f3d0a81e686049`). Checks with the default on: `scenarios`
  pins ok, oracle suite 61 passed, `dt2_sample_check` clean tone, golden 6 passed, `cargo test
  --lib --tests` in `packages/emulator/sharc` 48 passed. The `region_bench` example no longer compiles (its
  `Env` literal lacks `flat_windows`), so `cargo test --workspace` fails; not touched here.
- Left in these functions (batch 2): `2a_short` compute cu=0 opcode 0xad at 0x1c4fa3, 0x1c516b,
  0x1c5186; multifunction category 0x1e/0x1f at 0x1c5060 (3a), 0x1c553a, 0x1c5547; `8a_rel` with a
  compute in the delay slot at 0x1c55c5; in `FUN_1c244d` the 17a/4a/18a loop stops at 0x1c2583,
  0x1c25ae, 0x1c25bd and 0x1c252d. Interpreted share is now 0.99%; the top remaining are
  `FUN_1c0c76` (1.1M), `FUN_1c07c6` (0.87M), `FUN_1c06ba` (0.69M), `FUN_1c0a6f`, `FUN_1c02a4`.

## Region-call overhead **[D]**

Measured 2026-10-09 on `work/scenario-oracles` (uncommitted), no-AOT `static-insns+follow`, shared
core, DN2 `trig1-taps`, 10.5 audio s, flat fallback on (`SHARC_FAST_FLATAUTO=1`), PCM
`e0e6c8af7a84c443` throughout. Not re-checked by a second agent. Tools: `tools/devtools/perf.py` (diagnostic
run), new `tools/devtools/perf_calls.py` (call sources and exit kinds from its stderr), `xctrace` Time
Profiler on `tools/sharc_dev/sharc_build_dbg.py` output (`generic/dbg` target).

Instrumentation added: every `cov run` exit now carries a kind (`ret`, `call`, `edge` = taken
branch or loop-back to an address the kernel does not hold, `stop`, `guard`, `budget`; with
SHARC_FAST_COV a guard is named by the source line that emitted it, `g@mem_addr.rs@207` is the
dynamic-window guard). `tools/devtools/fastcov.py` `Diag.parse_run` split the `exits=` field on `,` while
the engine joins it with `;`, so it kept only roots with a single exit (it saw 82M of 196M calls);
fixed.

**The 10.5 s headline is 55% compile.** Timed run: 53.6 DSP-thread s = 19.8 s Cranelift compile +
2.9 s region build (6483 regions; the JSON has `fast_compile_ms_total` and `fast_build_ms_total`)
+ 30.9 s run time. Run time is 2.94 s per audio s; the 5.1 headline adds 2.16 s of compile. All A/B
below therefore give both numbers. (The 20 s run costs 110 s, 5.5 per audio s: later audio compiles
more regions; no linear fit.)

*Call sources.* 18.66M region calls per audio s: 16.75M run a kernel, 1.9M decline (exit0 1.23M,
req 0.55M, shape 0.13M, window 5k per run, budget 0). No chain misses (every PC is a root; the
interpreted gap between a region exit and the next call is 0.03M insns/s). Exits that ran, M per
audio s (insns per call): `edge>pc` 4.29 (6.9), `stop` 2.99 (23.7), `ret` 2.88 (112.6), dynamic-window
guard 2.69 (7.6), `edge>fn` 2.04 (8.1), loop-shape guard 1.23 (21.5), other guards 0.37 (4.3), call
0.22 (31.2). Top five functions, 8.25M calls (44%):

| function | calls/s | insns/call | why the calls end |
|---|---:|---:|---|
| `FUN_1c8ef1` (1352 roots) | 3.48M | 6.6 | the DO loop 0x1c9b12..0x1c9b2d is three regions per iteration (2.16M; 480 iterations per call, 1500 calls/s): edge>pc 0.92 (1.4i), loop guard 0.77 (7.3i), dynamic-window guard 0.76 (6.4i); stop 0.61 (8.8i); edge>fn 0.29; exit0 declines 0.11 |
| `FUN_b80c39` (22) | 1.75M | 2.7 | DO loop 0xb80c54..0xb80c60 calls 0xb809f2 every iteration: region to the call (4 insns, edge>fn 0.86M), callee, region at the loop end (1 insn, edge>pc 0.84M) |
| `FUN_b82d1a` (602) | 1.15M | 6.0 | dynamic-window guard 0.73 (4.1i), edge>pc 0.39 (5.0i) |
| `FUN_b809f2` (15) | 1.00M | 14.4 | the callee of the previous loop, ends in its return (ret 0.96M, 14.9i) |
| `FUN_b82a5b` (73) | 0.87M | 6.9 | dynamic-window guard 0.58 (4.8i), edge>pc 0.29; exit0 0.29M |

Next: `FUN_1c0c76` 0.73M (stop 0.44, ret/call 0.30), `FUN_1cdfa1` 0.58M and `FUN_1cdf46` 0.29M (the
same loop-calls-small-function shape as b80c39), `FUN_1c07c6` 0.58M (stop+ret, 3-4 insns).

*Cause of 29% of the calls.* A pre-modify or post-modify access whose address is a base register
plus a modifier loaded from memory (`I0 = modify(I1, M4)` with `M4 = DM(I5,M6)`, then
`DM(I0,M5) = R8`) is lowered through a guarded dynamic window anchored on the base register I1
(`lower/mem_addr.rs` `xfer_at`/`dyn_xfer`). The span resolved at entry is around I1's value, and the
real address (0x258xxx) is nowhere near it, so the guard fails on every call: the region does 4-5
instructions, leaves, and the next region starts at the failing store, where I0 is an entry
register and the window is right. Measured: 7.2M of 7.56M calls of that loop failed (only 480 distinct
addresses, all present and dirty, so it is not a memory-state guard). The chain of regions reached
from such exits is 252 roots, 5.9M calls per audio s, in `1c8ef1`, `b82d1a`, `b82a5b`, `1c57d2`.

*Per-call cost, run time only* (xctrace, 22.6K samples of the 59 s DSP thread, the build samples
removed, 9615 left; sampling covers a minority of the run, so shares only). JIT kernel code 46.3%
(0x... leaf addresses), interpreter plus generated core 24.0% (4.9M insns/s at ~120 ns = 0.6 s,
agrees), `glue` (`run_cfg`, `resolve_window`, `apply_*`, `sites`) 15.0%, engine step loop 3.4%,
`run_inner`/`wants` 2.6%, memmove/memset 2.2%, `Mem::window` 2.0%, helpers 1.6%. Per ran call
(2.94 s / 18.66M = 158 ns all in): about 73 ns in kernel code, of which ~13 ns is the instructions
(26.6 x 0.5); about 39 ns in Rust glue, dispatch, window and copy; plus the interpreter 0.6 s total.
All-in cost of an avoided call, interpreter and kernel work taken out: 2.94 - 0.6 - 0.25 = 2.1 s over
18.66M = 112 ns; measured below: 117 ns. The kernel's non-work part is not code size (a one-insn kernel
is 53 IR ops, 27 machine instructions): **[O]** it is cold code and an indirect call per region over
~1500 hot kernels, and data misses on the first loads; not separated. Chaining that only skips the
Rust loop can therefore remove the 39 ns part (about 0.5 s of 2.94 at 13-16M calls), not the rest.

*Ceilings* (run time per audio s, 112 ns per call): `FUN_1c8ef1` alone 3.48M calls = 0.39 s if all
its calls went (10x fewer: 0.35 s); top five 8.25M = 0.92 s; the 5.9M-call guard chain 0.66 s; all
calls 2.1 s (not reachable: the regions still run the same instructions).

*Change tried, behind `SHARC_FAST_DYNFLAT` (off by default; `1` = every pc, `LO-HI` hex = a range).*
When a region's calls leave the kernel at one of its dynamic-window instructions on at least half the
calls (checked at 64, 128, 256, ... calls), the slot is dropped and rebuilt from the next call's state
with those instructions as flat guarded accesses (`cfg::mark_flat_dyn`, `Lower::flat_dyn_cur`,
`Region::dyn_pcs`, `FastEngine::run_inner`). It uses `flat_access`, which batch 1 already proved
exact (a failing guard leaves before the instruction), and changes nothing without the flag.
A/B, same binary, exclusive lock, alternating pairs, 3 pairs each, medians:

| variant | calls/s | DSP CPU s / audio s | of which run time | compile + build s per run |
|---|---:|---:|---:|---:|
| base | 18.66M | 5.099 | 2.942 | 22.6 |
| slice `1c8ef1-1c9b73` (FUN_1c8ef1 only) | 16.57M | 5.029 | 2.777 | 23.6 |
| all pcs (`SHARC_FAST_DYNFLAT=1`) | 13.21M | 4.599 | 2.306 | 24.1 |

PCM `e0e6c8af7a84c443` in all 18 runs. The slice removes 2.1M calls for 0.165 s (79 ns each), the
widened change 5.45M for 0.637 s (117 ns each, run time -22%, headline -9.8%). The flat kernels are
bigger: compile +1.5 s per run. Remaining calls: 13.21M (11.68M ran); top: `FUN_b80c39` 1.75M,
`FUN_1c8ef1` 1.40M (stop 0.62M), `FUN_b809f2` 1.00M, `FUN_1c0c76` 0.73M, `FUN_1cdfa1` 0.58M,
`FUN_1c07c6` 0.58M.

*Next structural options, ranked by ceiling over exactness risk* (112 ns per call):
1. A call from inside a DO loop to a callee outside it, and its return (b80c39+b809f2+1cdfa1+1cdf46+
   1c07c6, 4.2M calls = 0.47 s). `cfg.rs` makes any branch whose target is outside the innermost loop
   an exit, and a return (`JUMP (Mm,Ii)`, a register target) always leaves with `DYN_PC`. Inline the
   callee (inside REACH) as nodes with the loop dormant, and turn the return into a guard "target ==
   the address after the call's slots" that continues inline and leaves with `DYN_PC` otherwise. Risk:
   the PC stack and loop stack at mid-callee exits (the sites must push the open calls), the 29-entry
   stack limit, the loop-end test inside the callee; no wasm-specific risk (it is IR only).
2. `stop` exits, 3.0M calls (24 insns each): form coverage, as in batch 2 above.
3. Kernel-to-kernel chaining that skips the Rust loop (`fast_continue`, `run_inner`, `run_cfg`
   entry checks and copy-out): at most the 39 ns glue part, about 0.5 s, and needs the state commit
   moved into the kernels. A redesign; wasm needs the kernels in one module for direct calls (an
   imported call costs 2.7 ns, an indirect call between modules does not exist).
Not recommended now: JIT-first or commit-once-per-block; the data above show a quality bug, not the
engine round trip alone, was the largest single source.

Exactness: PCM identical in 18 timed runs (full 10.5 s, trig1-taps pin). Oracle suite and
`scenario --all` in this config: see the status line in `docs/plans/scenario-oracles.md`.
Open: **[O]** wasm: `flat_access` takes its 32-bit path when `usize` is 32 bits, not run here; the
compile cost of the bigger kernels in V8 is not measured.

## Compile timeline and offload premises **[D]**

Measured 2026-10-09 on `work/scenario-oracles`, no-AOT `static-insns+follow` (flat fallback on),
DN2 1.11, shared core, exclusive lock; new `SHARC_FAST_TL=1` timeline, summarised by
`tools/devtools/fast_timeline.py`. Not re-checked by a second agent. Design: `docs/design/fast-compile-offload.md`.

- **Compile is a cold-start freeze, not a trickle.** `trig1-taps` 20 audio s, PCM `e1556053127f219c`:
  99.8 DSP-thread s = compile 19.8 + build 2.9 + run 77.1. 6789 of 8741 builds and 16.5 s of compile fall
  in the first 0.1 audio s (wall 20.2 s to reach audio 0.25 s); 1429 builds (2.67 s) come after audio 1 s,
  in bursts (1038 ms in one 0.25 s window at 1.5 s) and stop at 13.5 s. After audio 1 s: 867 first entries,
  229 `rebuild-exit0`, 200 retries, 93 `rebuild-req`, 40 `rebuild-win`.
- **Stall.** Compile per region mean 3.06 ms, p50 1.01, p99 27.2, max 42.6 ms (8226 IR ops, ~1.9 us per
  op); build mean 0.42, p99 4.4, max 27.7 ms. Build plus compile in one call: p99 25.4 ms, max 64.4 ms (97
  periods of 0.667 ms); 4253 builds exceed a period and hold 22.0 of 22.8 s. AOT default: 22 builds, 0.02 s,
  worst 2.3 ms, so offload does nothing for the AOT desktop build.
- **Waste.** 95% of rebuilds (2150 of 2270, 1.5 s run) replaced a slot that had run no instruction: 4.83 s of
  17.5 s compile (28%) went into kernels discarded before their first run (window and requirement declines
  are found at the first call).
- **Run time without compile**, DSP CPU minus compile and build: audio 2.5-8.5 s **2.66** per audio s
  (DYNFLAT 1.99, AOT 1.42); audio 12-19.75 s **4.88** (DYNFLAT 4.28, AOT 1.83). The call rate steps from
  19.4M to 26.8M/s around 9.2 s with no builds; cause found: see "Late-run slowdown" below (**[D]**). The 10.5 s headline mixes both regimes.
- **Determinism [V]:** `SHARC_FAST_DEFER=N` (a new kernel is not used for N-4N calls of its entry) gives the
  same PCM (`40d829c95c1d1480`, 2 s) for N = 0, 1, 7, 40 and 3000 (interpreted share 0.98% to 4.6%) and for
  AOT N = 0, 7, 300; the AOT and no-AOT PCM are equal at 2 s and 20 s. The switch point does not change results.
- **Cacheability [V in process, O across processes]:** Cranelift code has 0 relocations in 5599 kernels, is
  byte-deterministic, loads from stored bytes in 4.6 us (`define_function_bytes`), and a run with every kernel
  loaded from a blob (`SHARC_FAST_RELOAD=1`) has the pin PCM `3bf3ba3026953c22`. Not yet relocatable: the 8.8% of
  kernels with `Op::Helper` (a leaked record pointer in the IR and `helper_exec` as an immediate). Native
  compile scales with threads: 16.1 s (1), 4.2 (4), 2.15 (8), 1.36 s (16) for 4566 kernels. Code size 32.9 MB
  for them. `SHARC_FAST_CL_OPT=none`: compile -27% (19.8 to 14.4 s), code -26%, run time +1%, same PCM.
- **Wasm in V8 (Node 24)** for the same 4566 kernels: emit 179 us, 14.3 KB mean (66.7 MB; 24.7 MB zstd);
  `new Module` 0.05 ms (lazy), first call compiles: mean 1.43 ms, p99 23.8, max 45.2 ms, 6.5 s total; async
  eager compile 2.3 s wall; a worker that compiles and calls each kernel once hands over a `Module` whose first
  call costs 2 us; V8 does not share compiled code across threads by bytes. `Op::Helper` is unimplemented in the
  wasm emitter (8.8% of kernels). The DSP worker blocks in `Atomics.wait`, so receiving a `Module` needs a
  yield or `Atomics.waitAsync`. Browser behaviour **[O]** (Node only).

## Late-run slowdown **[D]**

Measured 2026-10-09 on `work/scenario-oracles`, DN2 1.11 and DT2 1.16, shared core (no-AOT,
`static-insns+follow`, `SHARC_FAST_DYNFLAT=0`, `SHARC_FAST_CL_OPT=none` set explicitly) and the prod
core (AOT, `default`); a binary copy per run; timed pairs under the exclusive lock. Not re-checked by
a second agent. **Superseded 2026-10-09:** the core now flushes denormals as the SHARC+ does
(finding 15, "Denormal flush"); the step is gone without any flag and `SHARC_FAST_DENORM` and
`Req::StkyMus` were removed. The text below is the measurement that found the cause and the interim fix.

**Characterisation.** "Calls" are fast-tier region calls per audio s, "run" is DSP CPU minus compile
and build per audio s (`tools/devtools/fast_timeline.py` ticks, `SHARC_FAST_TL=1`).

| case | step | calls/s before -> after | run before -> after |
|---|---|---|---|
| `trig1-taps`, no-AOT, 20 s | yes: ramp from 6.5 s, jump 9.0-9.5 s, flat to 20 s | 17.2M -> 26.2M | 2.66 -> 4.88 |
| `trig1-taps`, AOT default, 20 s | yes, same time | 0.5M -> 0.9M | 1.38 -> 1.86 |
| `idle`, no-AOT, 20 s | none | 17.2M flat | 3.2 flat (loaded machine) |
| `trig1-hold`, no-AOT, 20 s | none | 17.2M flat | 3.2 flat (loaded machine) |
| `dt2-sample-trig`, no-AOT, 20 s | yes, smaller, at about 4 s (the 1 s sample ended at 1.3 s) | 17.4M -> 19.1M | 2.8 -> 3.3 (loaded) |
| run length | none: the 12, 14 and 20 s runs and the 4/8/12/16 s diagnostic runs agree at equal audio time | | |

- The taps are at 0.06-0.10, 0.21-0.25 and 0.36-0.40 audio s (events at 60M-400M instructions,
  `tests/scenarios/dn2.json`). The step comes 9 s after the last release, so it is not an event time. It
  needs a sound that has decayed (`idle` never made one, `trig1-hold` never lets it decay) and it is
  independent of config and run length.
- **The firmware executes the same instructions.** Non-idle instructions per audio s are 497M before and
  after (the 492M seen without the fix looks like accounting at guard exits **[O]**: with the fix the count
  stays 497M); AOT config 267M both with the fix, 260M after the step without it. The extra work is the engine's, not the firmware's.

**Cause (verified).** Diagnostic runs (`SHARC_FAST_COV`/`HIST`, cumulative, differenced as
4-8 s against 12-16 s windows): region calls 17.4M -> 26.3M/s, interpreted instructions 5.0M -> 9.7M/s,
kernel instructions 497M -> 492M/s; the growth is in guard exits at one site, `result_normal_or_zero`
(`packages/emulator/sharc/src/fast/lower/mod.rs`, the float multiply result guard): 0.07M -> 2.19M exits/s, plus the
edge and `exit` re-entries each exit forces (edge +1.1M, exit +1.0M). Another 0.19M/s come from the
`guard_zero_or_normal` guard (`float_alu.rs`: `fix`/`trunc`/`scalb` of a denormal operand, one site in `FUN_1c244d`, not fixed). The functions are
the per-voice and FX stages: `FUN_1cdc30`, `FUN_b826c8`, `FUN_1c6664`, `FUN_1c244d`, `FUN_1c1f1e`,
`FUN_b809f2`, `FUN_b80432`, `FUN_b851e5` (IIR/envelope/gain loops), each from 0.00 to 0.1-1.5M calls/s.
`SHARC_PCDUMP` at `0xb82772..0xb8278e` in the window 12.0-12.1 s shows the multiplier operands: the
filter and envelope state registers hold **denormal floats** (`1e-45`, `3e-45`, `-1.96e-43`, `2e-44`,
`3.5e-39`) and STKYX is `0x01000100` (MUS already set). The guard fails because the product is
nonzero and below 2^-126, a case the lowering sends to the interpreter since the interpreter sets
ASTATX.MU and the sticky MUS.

**Why they appear and stay (verified for the persistence, inferred for the onset).** As a tail decays
the recurrences pass through the denormal range (the ramp from 6.5 s) and then stick at 1-3 ulp:
`x * g` with `g > 0.5` rounds back to the same denormal, so the state never reaches zero. The core
does not flush denormals (`tools/sharc_dev/sharc_core/floats.py` header: "not modeled"; the Rust runtime computes
f32 in IEEE). `idle` has no excited state; `trig1-hold` stays normal.

**Hardware: not genuine load [D].** SHARC+ PRM (`out/refs/sharc-plus-prm`, "Floating-Point Format" PDF p.100, "Denormal
operands ... flush to zero when input to a computational unit and do not generate an underflow
exception"; the 2136x PGR add/mult entries: "post-rounded denormal returns +-zero. Denormal inputs are
flushed to +-zero"; multiplier MU/MUS flag text in chapter 22 p.22-1). On the device the same states
decay to exact zero and every instruction takes the same time whatever the data. The late cost exists
only because the emulator keeps denormal state alive and its fast tier leaves a kernel at each
denormal product. Flushing as the hardware does would change the model; its effect on PCM is
**[O]**, not tried (it needs the Python core, the transpile and the golden hashes).

**Fix (behind `SHARC_FAST_DENORM=1`).** A region built while STKYX.MUS is known set (`Env::mus_set`)
lowers a float multiply without the denormal half of the guard (infinities and NaN still leave); it
adds `Req::StkyMus` (a masked compare of STKYX bit 8 at entry, `glue::reg_checks`), so MUS needs no
update; ASTATX.MU comes from the result bits at the flag replay (`glue::apply_flag` `Fmul`, and
`cond.rs` for lazily tracked flags). PEy (SIMD) multiplies keep the old guard. A region built with MUS
clear keeps the old guard and nothing else changes without the flag. Test:
`fast_denormal_multiply_matches_the_interpreter` (`packages/emulator/sharc/src/tests.rs`: all registers
including ASTATX and STKYX equal the interpreter at 8 chunk sizes, MUS set and clear, a flag-reading
`IF MS` in the loop; dropping the MU line makes it fail on `r118`); `cargo test --lib` 65 passed.

A/B, same binary (`SHARC_FAST_DENORM` unset vs `1`), exclusive lock, 14 audio s, run time per audio s
in the windows 3-8.5 s and 10-14 s (the second pair of the no-AOT runs was redone because a build ran
beside it):

| config | pair | base pre / post | fixed pre / post | headline DSP CPU per audio s |
|---|---|---|---|---|
| no-AOT | 1 | 2.67 / 4.92 | 2.63 / 2.77 | 4.76 -> 3.88 |
| no-AOT | 2 | 2.82 / 5.08 | 2.69 / 2.84 | 4.95 -> 3.98 |
| AOT default | 1-3 | 1.42, 1.41, 1.41 / 1.84, 1.83, 1.83 | 1.41, 1.41, 1.40 / 1.42, 1.41, 1.40 | 1.575, 1.567, 1.566 -> 1.423, 1.412, 1.404 |

The step is gone in both configs: post-step run time -44% (no-AOT) and -23% (AOT) and the 14 s
headline -19% / -10%; calls 26.2M -> 17.9M (no-AOT), 0.9M -> 0.5M (AOT). PCM `18e5204f2015f2eb`
(14 s) in all 12 14-s runs, AOT and no-AOT, flag off and on. DT2 (20 s, unlocked): calls
19.1M -> 17.4M, PCM `16b4abef610918fe` equal. Scenario sweep, flag off vs on, PCM equal in 25 scenarios no-AOT (idle, trig1-hold, kb-chord16,
exactness-100m, trig1-noqa-40m, trig1-tap-release, 7 dn2-fx, 7 trackfx, 5 sendfx) and the same 25 AOT plus
`trig1-taps` (pin `e0e6c8af7a84c443` holds); AOT and no-AOT hashes are equal per scenario.

Residual **[O]**: the tail of an AOT run also pays the interpreter's `_astatx_mult_float` general path
(four `_flags_put`) for denormal results in AOT blocks; not separated, and the fix above already
brings the AOT run to its pre-step cost (1.41), so it is small.

- **[D] Defaults changed 2026-10-09:** `SHARC_FAST_DYNFLAT` is on by default (`=0` off, `LO-HI` still works) and
  the fast tier's Cranelift `opt_level` is `none` (`SHARC_FAST_CL_OPT=speed` restores it). Same binary, exclusive
  lock, alternating pairs, PCM `e0e6c8af7a84c443` in all runs. Default AOT `trig1-taps`: 1.476 vs 1.477 DSP CPU per
  audio s (8 pairs, no change). No-AOT `static-insns+follow`, 5 pairs: 4.72 vs 5.89 (-20%), compile 1.62 vs 2.09,
  run 2.79 vs 3.50 per audio s. Not re-checked by a second agent.

## Tier mismatch: MODE1 read at entry **[D]**

`sf-del-time-u20` (REF:tests/scenarios/dn2-sendfx.json) rendered `67e0ea5c3f9a7166` in the default config
(AOT core and fast tier) and `c4688d57f9ef9aa9` in no-AOT `static-insns+follow` (rms 0.0127184 vs
0.0127187); finding 15 recorded it as open.

- Ground truth: the interpreter (`SHARC_AOT=0 SHARC_FAST_REGIONS= SHARC_BLOCKTRANS=`, 6.16 audio s, about
  12 min) renders `67e0ea5c3f9a7166`. AOT is right, the fast tier was wrong. The two WAVs first differ at
  frame 115653 (t = 2.4094 s); 24-bit-scale differences of the delay line contents follow from there.
- Cause: the fast tier read MODE1 bits from `Env::mode1` (the value at region entry) where it needs MODE1 as
  the kernel has it after an earlier MODE1 write in the same region (`mode1_cur`). The `fix` lowering
  (`falu_fix`) took MODE1.TRUNCATE from the entry value. DN2 1.11 function `0xb8b4dc` has
  `MODE1 = set(MODE1, TRUNCATE)` at `0xb8b4de` (decoded from the image); a region that goes on to a `fix`: the kernel rounded to
  nearest where the core truncates. Reverting only that line in an otherwise fixed tree gives
  `c4688d57f9ef9aa9` again; the fix alone gives `67e0ea5c3f9a7166`. (The generated forms already read
  MODE1 through `m1()`, which uses `mode1_cur`; only hand-written lowering used `env.mode1`.)
- Same pattern, fixed with it: ALUSAT in the signed conditions (`cond_value`, LT/GE/LE/GT) and
  MODE1.CBUFEN in the circular post-modify (`post_value`). A write makes the region require all of MODE1 at
  entry (`mode1_mask = u32::MAX`), so only the reads were stale. Not observed in a scenario, found by review;
  covered by unit tests.
- Related entry-state check: a MODE1 write that turns IRPTEN or NESTM on can make a latched, unmasked source
  deliverable at the next instruction boundary. The region's entry check (`latent_interrupt`) saw interrupts
  off, so the kernel ran on past the write while the interpreter enters the interrupt. Such a write is now
  refused (`g_wr_mode1`: "MODE1 write enables interrupts"). Unit test: without the refusal the kernel and the
  interpreter end at different PCs. `sf-del-time-u20` does not need it (equal with and without); the sweep below ran with it.
- Checked, no change: other `env.` reads are guarded. Registers the region writes are not baked (`baked`,
  `entry_value_of` use `plan.written`); L and B of an index register are refused when written
  (`lb_stale`), with one gap closed: `require_linear` (7a/19a MODIFY, hand-written forms only; the generated
  forms already refuse) now checks `lb_stale` too. MODE2, USTAT and MMASK are plain registers in the kernel.
  Register bank selection: a MODE1 write that changes the bank bits is refused with the bank model. STKY is
  not lowered. ASTAT is tracked per instruction (`fm_static`); `cacc_known` is an entry property by design.
- Tier equivalence after the fix, DN2 1.11, PCM sha256 of every scenario of `REF:tests/scenarios/dn2-trackfx.json` (47) and
  `REF:tests/scenarios/dn2-sendfx.json` (87), default (AOT + fast tier) against no-AOT `static-insns+follow`: 134 of 134 equal,
  `sf-del-time-u20` `67e0ea5c3f9a7166` in the interpreter, the default and the no-AOT config.
- Tests: `fast_reads_mode1_as_the_region_wrote_it` (TRUNCATE on/off, ALUSAT on/off, CBUFEN on/off in the
  region; fails on `env.mode1`), `fast_mode1_write_that_enables_interrupts_matches_the_interpreter`,
  `fast_modify_follows_l_written_in_the_region` (passes with and without the `require_linear` check, since
  the generated forms are the default; it guards the default path).

## Generated lowering: design and how to add a family **[D]**

Folded from the lane F notebooks (not re-checked by a second agent).

Design. `tools/sharc_dev/sharc_lowergen.py` runs the real `tools/sharc_dev/sharc_core/forms` handlers over a
tracing state: decoded fields and lowering context (MODE1, `assume_nw32`) are static
symbols, a test on one forks the trace, register contents and memory words are dynamic
nodes, and a data-dependent branch refuses. `tools/sharc_dev/sharc_lowergen_rs.py` value-numbers the
leaf programs and folds the decision tree into `packages/emulator/sharc/src/fast/lower/gen_forms.rs`
(one method per form over the primitives of `gen_prims.rs`, hand-written once). Never edit
`gen_forms.rs` by hand. Control transfers (calls, returns, loops, delayed branches) stay in
`cfg.rs`/`flow.rs` because the tracer never runs `_advance`. Flags stay lazy and are
replayed at exit. MODE1 is static per program point. Compute bodies still come from the
hand-written `compute_full`/`compute_short`/`shiftimm`. 27 forms are generated.

Add a family:
- Trace and print a form: `uv run python -m tools.sharc_dev.sharc_lowergen_survey FORM...` (no args:
  all forms). One leaf: `tools/sharc_dev/sharc_lowergen_leaf.py FORM field=value ...`.
- A new intrinsic is an `i_*` function plus a `PATCHES` entry in `tools/sharc_dev/sharc_lowergen.py`,
  a case in `tools/sharc_dev/sharc_lowergen_rs.py`, and a `g_*` primitive in `gen_prims.rs`.
- Regenerate with `mise run gen:regen -- --add FORM,...`. Add the form to
  `tools/sharc_dev/sharc_lowergen_vectors.py` (steer the random fields into the accepted region) and,
  for sequences, to `tools/sharc_dev/sharc_lowergen_programs.py`.
- Check: `cargo test --lib gen_` in `packages/emulator/sharc` (with `SHARC_GEN_DIR` set; about 75 s),
  `tests/sharc/generation/test_sharc_lowergen.py` (fails when `gen_forms.rs` is stale), and
  `tools/sharc_dev/sharc_lowergen_bisect.sh` for PCM and coverage with a form subset.

Results **[D]** (DN2 1.11, `trig1-taps`, 1.5 s, `static-insns+follow`, PCM
`3bf3ba3026953c22` throughout): hand forms 89.5% fast; generated forms 92.5%; control forms
(calls, returns, indirect jumps, IDLE) 93.0% with interpreted instructions -7%; MODIFY with
circular wrap (7a, 7b, 19a_scaled), MODE1 from a register and L/B writes 93.0% with
52.9M interpreted instructions (7.0%). What is left interpreted: compute bodies (about
21%), 20a PUSH/POP and conditional or call forms of 9a/9b (small), window-analysis refusals
(14%), system registers with machine state (ASTATX, PCSTK, LCNTR, IRPTL; they sit in the cold
context-save function `FUN_1c02a4`), and 7d (ACONV, depends on the address map).

Semantics worth knowing **[D]**: `_type_7a` wraps only on the cond 0x17 path, so an
unconditional MODIFY is a plain sum; 7b and 19a_scaled wrap when L != 0 whatever
MODE1.CBUFEN says. A post-modify is exactly `candidate >= B+L ? -L : candidate < B ? +L`
in unsigned 32 bits and needs no `|delta| <= L`. A return's last slot (`I7 = I6; I6 =
DM(I6 + 0)`) is a tail write.

Bug found with `tools/sharc_dev/fast_ddmin.py` **[D]**: the halt at 0xb88ea8 (`fork`). `fast_ddmin`
(with the `SHARC_FAST_ONLY` entry allow-list) shrank 2153 armed entries to three regions
(0xb85a02, 0xb85a19, 0xb85a1f). The epilogue loads `DM(I6-3)`, `DM(I6-2)` and `DM(I6)` read
through an absolute window, because I6 had been given its circular buffer's range without a
`Req::Within`: the only write of I6 was the RFRAME `I6 = DM(I6)` in a RETURN delay slot, and
`commit` skipped the in-buffer check for tail writes. A tail write of a circular register is
never "inside", so the register is now marked plain. Debug variables from the same lane:
`SHARC_TRAP_REGS`, `SHARC_PCDUMP_MEM`, `SHARC_FAST_DEBUG_REQ=0xPC`, `SHARC_FAST_DEBUG_WIN`,
`SHARC_FAST_DUMP_ALL`.

AOT cost profile **[D]** (default config, 1.41 DSP CPU per audio s, sampled with
`xctrace`): AOT is 70% of the DSP thread, Cranelift kernels 24%. Inside AOT, V/Option/Result
plumbing is 27%, memory access 14%, uncategorised moves 11%; real arithmetic is under 10%.
This is the basis of the mask-free emission estimate above.

Mask-free slices 0-1 **[D]** (2026-10-10, `docs/design/aot-mask-free.md` section 6; the correction
of the profile above is: the plumbing share is not the register masks). LLVM already folds the
masks of the 123 ordinary registers (asserting them known: warm 0.98-1.02, in situ not built);
only the ASTAT/STKY known-bit pairs remain, and assuming those known too (unsound experiment,
exits and PCM identical) is 6-16% warm and 6.5% in situ (paired 0.935, trig1-taps, 8 pairs).
In situ AOT is 34 ns per region call + 4.0 ns per instruction (region profile, 27.1M calls);
the default-bench regions (1C399A, 1C3862, 1C364F, 1C5765, B823DB, B8276C) are fast-tier kernels in
the default config, not AOT. The DSP thread loses 28-32% of its cycles to the front end in situ
(17.7% with PGO) against 1.5% warm (CPU Counters bottleneck split, calibrated with synthetic
programs); AOT code is 208-283 machine instructions per SHARC instruction in the binary (832-1130
B), 47-103 executed. Gate: no concrete-register mask-free; do the sound flag-known gate and hot-code
density (cold-path outlining, shared trap tail) first. Tools: `tools/sharc_dev/aot_mcount.py`,
`tools/sharc_dev/aot_dyntrace.py`, `tools/devtools/xctrace_bottleneck.py`.

DT2 AOT and the DT2 render are in finding 15.

## Desktop baseline 2026-10-09 **[D]**

Default config (AOT core + fast tier), `coupled-run` with `--scenario`, exclusive benchlock,
5 runs per cell, DN2 1.11. Cell = median DSP thread CPU per audio s (IQR); CF thread CPU per
audio s and real_time_factor are medians. PCM identical in every run to the pins
(trig1-taps `e0e6c8af..`, tf-dry-hold `79e2dac0..`, tf-br-127 `16f3d0a8..`). Machine load
average (1 min, read before each run) 2.2-4.3 for the prod and old-PGO rows, 2.5-4.2 for the new
PGO row; no Digiemu, a short `duetexpertd` burst before the new-PGO run. Not a quiet machine:
the floor is a desktop session (WindowServer, Chromium, wezterm). trig1-taps is 10.5 audio s
(not 14); tf-* are 4.1 audio s.

| scenario | prod (no PGO) | PGO old profile (stale) | PGO new profile | CF CPU/audio s (prod / new) | rtf (prod / new) |
|---|---|---|---|---|---|
| trig1-taps | 1.588 (0.053) | 1.636 (0.085) | **1.173 (0.024)** | 0.509 / 0.502 | 0.628 / 0.851 |
| tf-dry-hold | 1.578 (0.145) | 1.556 (0.074) | **1.172 (0.023)** | 0.550 / 0.544 | 0.632 / 0.851 |
| tf-br-127 | 1.638 (0.037) | 1.618 (0.049) | **1.255 (0.064)** | 0.601 / 0.602 | 0.601 / 0.775 |

A second prod run after the pin edit below reproduced the first (1.593 / 1.568 / 1.613). So:
- The old profile (trained 2026-10-07, key stale on sources and generated core) buys nothing
  (-3% to +3%, inside the noise). The retrained profile cuts DSP CPU per audio s by 26%
  (trig1-taps), 26% (tf-dry-hold) and 23% (tf-br-127) and moves real time from 0.60-0.63 to
  0.78-0.85. CF thread cost is unchanged (0.50-0.60).
- Gap to the 1.0 target: 1.17-1.26 now (needs a further -15% to -20%); without PGO 1.58-1.64.
  The no-PGO ceiling analysis (AOT 70% of the DSP thread) still applies; PGO and mask-free AOT
  would stack only if the profile transfers to the new emission.

How measured, and caveats:
- The PGO binaries are `coupled-run` built with `-Cprofile-use=<profdata>` into
  `out/native/target/prod/pgo-cr-old` and `pgo-cr-new` (`--target aarch64-apple-darwin`,
  rustup 1.98.1). Profiles are trained on the desktop app (`digiemu-desktop --pgo-train`), so
  `native_pgo.py verify` reports one crate hash mismatch (`elektron_native_boot`) for
  `coupled-run`; the other crates match. The launcher's own `pgo-use/.../digiemu-desktop`
  (2026-10-08 18:24) is stale and was not rebuilt; `mise run emu` rebuilds it with the new
  profile (`38d7c8fe..`) on the next launch. Its `pgo-use/.../coupled-run` (2026-10-05) is
  unrelated and stale. The app's own CPU split was not measured here.
- The training gate (`coupled_ready_exactness`, `packages/emulator/coupled/src/lib.rs`) failed before
  training: its CF state digest and DSP export pins were stale since 7c489c5 (2026-10-08, SIMD
  shift fix). PCM pin `b3f5230f..` and sample count held. Observed CF digest
  `4a9d3123..` (pinned `55dc699e..`), DSP export `20905b23..` (pinned `4dc48019..`); both pins
  were set to the observed values locally (uncommitted) so training could run. Not
  independently checked: a second agent should confirm the new digests are the intended
  result of 7c489c5 before the pins are committed **[O]**. Editing `lib.rs` also made the
  instrumented build a full 8 min compile.
- Training: 4 runs of the gate, 507 s total, profile 40 MB.

## Slice 2' (hot-code density) 2026-10-10 **[D]**

`docs/design/aot-mask-free.md` section 7. Built, not committed: flag registers required fully known at region
entry (they were at `0xFFFFFFFF` at every dispatch of seven scenarios; the `0x00FFFFFF` ASTATX mask in the old
comment was stale), cold marking of every trap, budget and entry-gate path, one trap tail per region, chain exits as
two tables and one function per successor set, one budget check per body (a body that would cross the limit is left
to the interpreter), the normal-word range table out of line, a 32-bit plain-RAM address path.

| | old core | slice 2' |
|---|---:|---:|
| bytes per SHARC site (150 hottest in-situ regions) | 868 | 465 |
| front end of the DSP thread in situ (no PGO / PGO) | 27-32% / 14.6% | 21-24% / 10.4% |
| DSP CPU per audio s, trig1-taps, no PGO / PGO | 1.502 / 1.163 | **1.277 / 1.055** |
| tf-dry-hold, no PGO / PGO | 1.510 / 1.161 | **1.282 / 1.059** |
| tf-br-127, no PGO / PGO | 1.612 / 1.237 | **1.368 / 1.129** |

Exclusive alternating A/B (`tools/devtools/coupled_ab.py`), 6 to 7 pairs, paired ratio 0.85 without PGO and 0.91 with PGO
(0.90 to 0.93 over all pairs); PCM equal to every pin; DN2 pins, oracle suites (default and no-AOT), the sweep, the
134-scenario AOT against no-AOT comparison and DT2 (clean tone, AOT = no-AOT = AOT + fast WAV hash) unchanged.
Block traps are identical in count and code in 25 scenarios; block entries grow 0.27% and interpreted instructions
0.5 to 0.7% because a body that would cross the budget limit is no longer split at the instruction. The density
gate of the design (<= 400 B per site, front end < 12% without PGO) was missed (465 B, 21-24%); the warm gate was met
(0.77 to 0.91). The undo-copy ordering analysis was not built: deleting every undo copy (unsound ablation) is worth
2.4% of the bytes and 0 to 9% warm, and LLVM already drops the restores of Err paths it can prove dead. The
desktop is at **1.06 (trig1-taps), 1.06 (tf-dry-hold), 1.13 (tf-br-127) with PGO: not yet 1.0**; it needs
another 5 to 11%. Largest remaining items in `tools/sharc_dev/aot_attrib.py`: the normal-word test (12.6% of the bytes), the
memory fast paths (8.6%), the float helpers (19%), `rf_set` (3.8%), `s_set_special` (3.1%).

### `tests_gen` "the fast tier never ran" (2026-10-10) **[D]**

`gen_programs_match_the_interpreter` and `gen_control_programs_match_the_interpreter` failed ("simd_loop: the fast
tier never ran", `ctl_call_ret` in the control test) only when built against a gen directory with an image (DN2/DT2
`out/native/*-audio/gen`); they passed against `shared-core/gen`. Not a fast-tier regression and not specific to
`simd_loop`: configuration 1 of the test turns on the bank and stack models (`set_option` 19, 20), which calls
`Cfg::refresh`; that recomputes `block_base_ok`, which also requires `explicit_memory_model` to equal the image's
`GEN_EXPLICIT_MEMORY_MODEL` (false for the DN2/DT2 images, true for the test engine's default). With a mismatch,
`Engine::block_plan` returns None before it offers the fast tier (gated 0, nothing built). Configuration 0 never calls
`refresh`, so it was unaffected. The runtime sets the image's value. Fix: `tests_gen.rs` `engine_cfg` sets option 10 to
`gen_explicit_memory_model()` for configuration 1. Both tests pass against the DN2, DT2 and shared gens.
Observation (open): `block_plan` also gates the fast tier on `block_base_ok`, though the fast tier does not use
blocks; a coupled run with a mismatched memory-model setting would silently run no fast tier.

## Slice 3 (float, memory and interpreter items) 2026-10-11 **[D]**

`docs/design/aot-mask-free.md` section 8. Built, not committed. The desktop is under real time: DSP CPU per audio s with
PGO **0.896 (trig1-taps), 0.901 (tf-dry-hold), 0.922 (tf-br-127)** (slice 2': 1.054, 1.056, 1.126), without PGO 1.104,
1.106, 1.122 (1.279, 1.280, 1.358); paired ratios 0.82 to 0.86, 6 exclusive alternating pairs each, PCM equal to every
pin.

| | slice 2' | slice 3 |
|---|---:|---:|
| DSP CPU per audio s, trig1-taps, no PGO / PGO | 1.279 / 1.054 | **1.104 / 0.896** |
| tf-dry-hold, no PGO / PGO | 1.280 / 1.056 | **1.106 / 0.901** |
| tf-br-127, no PGO / PGO | 1.358 / 1.126 | **1.122 / 0.922** |
| warm time, hot AOT regions (ratio, 13 regions) | | 0.70 to 0.94 (two at 1.0 and 1.1) |
| interpreted instructions, trig1-taps | 13,151,269 | 6,760,382 |
| bytes per SHARC site, whole DN2 core | 397.5 | 394.5 |
| front end of the DSP thread, no PGO / PGO | 20.6% / 10.4% | 20.9% / 9.0% |

What moved it, in order of size: (1) the interpreter ran 7.5 of 13.2M instructions as one 238-instruction routine
because a block entry gated by a pending bank swap sends the whole block body to the interpreter (a region has entries
only at block starts). The profile now records the PC after the first instruction of such a run, 35 more block starts
(6 rounds of `tools/sharc_dev/aot_entry_iter.py`) cut the interpreted instructions by half and the DSP time by about 7% (no PGO; the build before the search
measured 0.926 against slice 2', after it 0.862). (2) The memory path was 34% of the dynamic instructions of the in-situ AOT regions (not the 8.6% and 12.6% of the
bytes): aligned-word and narrow paths, fixed-size page tables, a two-compare normal-word band test, no `fast_mem` test
and no 128-bit address mapping in the register load. (3) Float add, subtract and multiply flush the operands and
classify the result (normal, zero, rest), and the multifunction multiply, `_float32` and `_float32_result` are native
boundary functions. (4) `sec_arbitrate` visits only the SSTAT registers that exist (2.4% to 0.6% of the thread).
Dropped: float variants that test the operands first (they follow the data: silence is slower), memory accesses as
one shared out-of-line body per width (-9% code, +0 to +68% warm), running blocks in the idle probe (the probe
iteration is 24 instructions of the idle range, which has no blocks).

Exactness: the seven DN2 pins, `scenarios:check` (default and no-AOT, 61 each), `scenarios:sweep` (191), 134 of 134 FX
scenarios AOT = no-AOT PCM, golden/denormal/density/lint tests, `cargo test -p sharc-rt` (16, three new differential
tests), DT2 clean tone and WAV hash `4bab8330915f0929` in AOT, no-AOT and AOT + fast. Block traps: count identical in
all 134 FX scenarios, code and count identical in six profiled ones; block entries +1.1% and zero-progress bails +7%
in trig1-taps (the new block starts), AOT instructions +0.26%. **[O]** DT2 has no entry search yet; the loop end
(`stk_set_loops` with its undo entry, 8% of the in-situ regions) and the idle probes are the next items.

## Desktop app real time 2026-10-11 **[D]**

Spike log (`DIGI_SPIKE_LOG`, `tools/devtools/spikes.py`, docs/INSTRUMENTATION.md) in the desktop app: DN2 1.11, Trig 1
on track 1 with send delay, a little reverb and track SRR, PGO launcher build, 1 s prefill, Rebuffer. Steady state
(after the queue first reaches 900 ms), one run each; not re-checked by a second agent.

| | webview pump (`Mode::Ui`, b4f3693) | paced free running (f344278) |
|---|---:|---:|
| audio s per wall s | 0.907 | 0.995 (held by pacing) |
| underruns | 9 in 100 s | 0 in 53 s |
| queue min / median | 0 / 506 ms | 899 / 992 ms |
| DSP busy / CPU per audio s | 0.988 / 1.002 | 0.942 / 0.955 |
| CF work / DSP reply wait / gap between steps, per audio s | 0.492 / 0.356 / 0.254 | 0.478 / 0.489 / 0.039 |
| worst DSP period / CF step | 1.36 / 13.4 ms | 1.63 / 7.2 ms |

- The app was below real time on average, not spiking: the 1 s prefill hid it until it ran out. Fast-tier
  compile was 1.7 ms in the whole run; no period exceeded 2.1 ms.
- The webview's `setTimeout(0)` `emu_step` loop cost the CF thread 0.48 ms per call (25% of its time) and slowed
  the DSP thread by about 5% (1.002 -> 0.955 CPU per audio s); the cause of the DSP slowdown (contention) was not
  measured.
- The DSP runs the same 502M non-idle instructions per audio s whatever the FX settings (the FX code runs whether
  audible or not): headless `trig1-taps`, `sf-del-ref`, `sf-rev-ref`, `tf-srr-127` and `tf-del-127` all cost 0.93
  to 0.94 with the slice 3 PGO `coupled-run` (load average about 3; 0.896 earlier on a quieter machine). One
  scenario stands for every patch.
- Headroom in the paced app: DSP busy per 0.1 audio s window p50 94.0, p99 99.7, max 100.4 ms (4 of 519 windows
  over). CF work reached 1.0 to 1.4 per audio s in bursts while browsing pages; with the depth-1 link pipeline a
  CF burst stalls the DSP. A buffer well below 1 s needs DSP headroom first (target about 0.8).

## Linker order file: ceiling 2026-10-11 **[D]**

Measured before building (trig1-taps, DN2, PGO `pgo-cr-s3b` and prod `coupled-run`, exclusive benchlock, busy
machine; scripts and traces were in a session scratchpad, not kept). Not re-checked by a second agent.

- Front end of the DSP thread (`tools/devtools/xctrace_bottleneck.py`): PGO 11.8%, no PGO 24.1%; discarded (mispredict
  flush) 2.5% in both. The CPU Counters template gives no L1I/iTLB/resteer split.
- 20% (PGO) and 15% (no PGO) of DSP samples are outside the binary (Cranelift kernels, system libraries); 73% of the
  in-binary samples are in `r_*` AOT regions.
- Functions holding 90% of in-binary samples: PGO 167 functions, 6.1 MB, 467 16 KB pages, 94 address ranges over
  18.5 MB of a 31.2 MB text. Executed bytes are 15-25% of each region function (60-175 KB each), so the hot set is
  about 1.2-1.6 MB (lower bound from samples 268 KB): 6-8x the 192 KB P-core L1I, spread over 430-650 pages.
- Ceiling: an order file is function-granular. It can pack the pages (about 100 instead of 430-650, inside the L1
  iTLB reach) but not the cold blocks inside the region functions, so L1I capacity misses stay. Estimated 0-3% of
  DSP CPU with PGO, 1-4% without (1.10 -> 1.06-1.09); most of the no-PGO front-end excess is in-function layout.
  Not built. Note: `tools/devtools/xctrace_bottleneck.py` needs the `record` options before OUT, or xctrace fails with
  "Path not found '--time-limit'".

## LLVM hot-cold splitting with PGO 2026-10-11 **[D]**

`-Cllvm-args=-hot-cold-split=true` added to the slice 3 PGO `coupled-run` build (same profile `a76e820c...profdata`,
same DN2 gen; `out/native/target/prod/pgo-cr-hcs`). It outlined 1949 `.cold` functions and cut `__TEXT` from 46.4 to
42.6 MB (-8.2%), but DSP CPU per audio s rose: trig1-taps 0.935 (A) against 0.943 (B), paired ratio 1.014 (0.985 to
1.023), 6 exclusive alternating pairs (`tools/devtools/coupled_ab.py`), load average about 3, PCM equal to the pin in all 12
runs. Not adopted. Caveat: the profile was trained without the split, so its mapping onto outlined code is untested.

## Fast-over-AOT: per-region ceiling and stop reasons 2026-10-11 **[D]**

Can the hottest AOT regions run faster as fast-tier kernels? The fast tier already takes any PC listed in
`SHARC_FAST_REGIONS` before AOT. Region profile (`region-profile` build, no PGO, DN2 trig1-taps, timer overhead of
about 26 ns per dispatch subtracted): 1291 AOT entry PCs, 2.63M entries and 235M instructions per audio s (89 per
entry), 668 ms, 2.85 ns per instruction, about 59% of the DSP thread. The top 25 regions hold 75.9% of AOT
instructions and 70.1% of AOT time (top 50: 89.1% / 83.9%).

- Estimated saving per region S = AOT wall - (insns x 0.5 ns + entries x 160 ns): positive for 767 regions, 328 ms
  per audio s in total (the cost-model ceiling, no PGO; kernels run 0.5 to 1.6 ns per instruction in situ, so it is
  an upper bound).
- Probe: arming the top 5 by S (0x1cdb85, 0x1c0c9d, 0x1c926f, 0x1c25cc, 0x1c963b) on top of the shipped list changed
  nothing (1.1413 -> 1.1409, 3 runs each); interpreted instructions +0.5%, so exits land on AOT block starts. The
  kernels stop after a few instructions and AOT runs the rest, adding 219k region calls per audio s.
- Stop reasons with the top 50 armed (each region charged to its dominant exit; blocked saving = S x uncovered
  share; covered 57 of 300 ms):

| reason | blocked ms/s | regions | kind |
|---|---:|---:|---|
| call, return or region edge: continuation outside the kernel | 76.3 | 16 | structural |
| memory address range unknown: the 16a return-address push `DM(I7,M7)` in a CALL delay slot (I7 has no static range; `flat_auto` does not rescue it) | 67.3 | 11 | analysis |
| loop "register step not stable" (0x1cdb85 alone 32) | 48.1 | 7 | structural |
| shifter BTGL by register (cu=2, opcode 0xc8) | 23.3 | 1 | form |
| never built or entered | 13.7 | 3 | analysis |
| multifunction multiply with min/max (0x1e/0x1f), float MANT, a SIMD condition | 11.1 | 4 | form |

- BSET/BCLR/BTGL by register (0xc0/0xc4/0xc8) are now lowered (`shift_bitop` in `fast/lower/shift.rs`, `ShiftDyn`
  flags). `fast_diff forms` 16 runs x 5000 trials, both backends: 0 mismatches; seven DN2 pins, `scenarios:check`
  in both configs (61 each), 134 of 134 FX scenarios AOT = no-AOT, DT2 clean tone. 0x1c926f now runs 23 instructions
  per call (was 4) and exits at an edge to 0xb813e8; 0x1c963b runs 35 (was 18) and stops at form 25a with unknown
  address ranges. The probe medians moved 5% between sessions for the unchanged configuration (1.14 -> 1.20), so no
  speed change is claimed: each lifted stop exposes the next structural one.

### Run length to the first call or return (top 50 regions) 2026-10-11 **[D]**

Interpreter-only `coupled-run --profile-execution` of trig1-taps (exact per-PC counts) combined with the static CFG
in `out/sharcdb/dn2-1.11.sqlite`; branch probabilities fitted from the counts, 3000 Monte Carlo walks per region from
the entry to the first CALL, return, indirect or cross-function jump. The means match the kernel diagnostic's exact
per-call counts on all 10 regions where a kernel already reaches a call (e.g. 0x1c0c9d 34/34, 0xb83635 50/50,
0x1c25cc 8483/8378). p10/p90 are Markov estimates.

| share of the top-50 saving S | run >= 100 insns | 45-100 | < 45 |
|---|---:|---:|---:|
| kernel stops at the first call | 40.6% (23 regions) | 3.0% | 56.4% (24) |
| leaf callees of <= 64 insns followed | 50.6% (26) | 5.4% | 44.0% (22) |

- 58% of S ends at a call, 39% at a return. The long share is mostly regions that run to a return without a call:
  0x1cdb85 (446 insns), 0x1c25cc, 0x1c9ad5, 0x1c248a, 0x1c254e; their blockers are the loop step, unknown address
  ranges and one region never built.
- The short share stops after 5 to 45 insns at a call into a non-leaf callee (e.g. 0xb836ff: 5 insns, then 0xb82a5b
  at 4232 insns per call), so it needs kernels that follow calls.
- Without call following, the reachable part is about 5-8% of DSP CPU with PGO (0.93 -> about 0.86), an estimate.

## DT2 parity 2026-10-11 **[D]**

Method. DT2 1.16 (`sections/.source-sha256` = 278541e4...), prod core `out/native/dt2-audio/gen`, default config, scenario
`dt2-sample-trig` (3 s, one-sample card of `tools/emulator/dt2_sample_check.py`; the only DT2 load scenario). The DT2 DSP image has
no fast regions in `tools/emulator/fast_regions.json`, so the tiers are AOT block, idle skip and interpreter. `tools/sharc_dev/aot_entry_iter.py`
now takes `--device dt2` (`aot_fxprof.py --dt2-card` runs on the card's ready state; the base profile is
`prof-merged-c2`). Four rounds, each profiled with the core built from the previous round (rsgen about 50 s, build 1.5 min):
new block starts 27, 8, 3, 1 (the fifth round finds one PC again, a 9000-count chain of one instruction per round, not
worth a round). The hot one is 0x1c0885 (873,000 interpreter runs in 3 s: a gated entry whose body ran in the
interpreter). Timing is `tools/devtools/coupled_ab.py --dt2-card --device dt2`: 6 exclusive alternating pairs, load average
3.4 to 6.6 from other jobs (pair ratios stay within 0.870 to 0.874).

| | prod core | with entry search (`gen-lane-entry`, not promoted) |
|---|---:|---:|
| DSP CPU per audio s, median of 6 (no PGO) | 1.311 (IQR 0.005) | **1.144** (IQR 0.004) |
| paired ratio | | **0.871** (0.870 to 0.874) |
| interpreted instructions, 3 s | 13,227,236 (0.44% of all, 1.4% of non-idle) | 5,926,406 (0.20%) |
| AOT block instructions | 938.4M | 945.7M |
| block entries | 6,832,135 | 6,907,129 |
| blocks / instructions in the core | 1202 / 12,588 | 1241 / 13,398 |
| PCM hash (3 s, coupled run) | 999dd10944ce7e62 | 999dd10944ce7e62 |

Exactness: `dt2_sample_check.py --seconds 3.0` gives a clean tone and WAV hash `4bab8330915f0929` for the new core in
AOT, AOT + fast (`--fast`) and no-AOT (`--env SHARC_AOT=0`), as the prod core does. (The pin is of the 3.0 s run; the
default 1.6 s run hashes a9cbc3beee5267fc for both cores.) DT2 has no pinned scenario hashes. Block traps: 36,000 in
both.

**[O]** 5.9M instructions are still interpreted; they are not gated block bodies and were not analysed. The DT2 core
has no FX profile (only a trig of one sample), so FX code stays interpreted whenever an effect is on.

DT2 FX scenarios, plan (not built). (1) Find the DT2 FX page button and encoder map (finding 03) and measure the UI
value per encoder event by read-back of the live kit Fx array (`*0x80004708`) and Sound array, as DN2 did (`UPS` in
`REF:tools/emulator/gen_trackfx.py`). Parameter codes are known (finding 02: sends 0x44/0x45/0x43, delay time 0x71, feedback 0x74, reverb
decay 0x7c, BR/SRR/routing/overdrive 0xff..0x102). (2) Check the DT2 values of `FX_PAGE` and `TRIG1` in
`scenario_dsl.py` (TRIG1 = 25 and the NO prelude already work in `dt2.json`). (3) Generate `tests/scenarios/dt2-trackfx.json`
(BR, SRR, overdrive, delay, reverb, chorus sweeps on a card sample, so the DSP is loaded) and `dt2-sendfx.json` (send
FX and master). (4) Run them on the card's ready state (`--card-image`, as `aot_fxprof.py --dt2-card` now does;
`coupled.py scenario --device dt2` has no card option yet). (5) Profile with `aot_fxprof.py --device dt2 --dt2-card`,
merge into `prof-merged-c2`, run entry rounds, regenerate, and gate on AOT = no-AOT PCM in every scenario.

## Browser (Chrome) baseline 2026-10-11 **[D]**

Em's page showed 0.22 audio s per wall s on DN2 1.11 after Trig 1 (underruns 9). Method: `mise run web:bench`
(docs/INSTRUMENTATION.md): headless Chromium 150 (Google Chrome is not installed here; same V8), dev server,
`apps/web/.local/emulator-core-sharc.wasm` built 12:55 on 2026-10-11, DN2 1.11 ready state, `tap(25)` at
load as the Trig 1 button does, 10 audio s, no AudioContext (PCM port drained), exclusive benchlock, fresh
profile per run (no code cache). One machine, load not controlled, not re-checked by a second agent.

| V8 flags | audio s / wall s | first half / last half | worker busy per audio s |
|---|---:|---:|---:|
| default (3 runs) | 0.259, 0.256, 0.249 | 0.247 / 0.261 (mean) | 3.17-3.30 |
| `--no-liftoff`, TurboFan only (3 runs) | 0.222, 0.238, 0.219 | 0.178 / 0.280 (mean) | 3.52-3.83 |
| `--liftoff --no-wasm-tier-up`, Liftoff only | 0.248 (1 run; 0.254 and 0.243 in an interrupted batch) | 0.228 / 0.261 | 3.32 |

Each run leaves 17-18% of wall time outside the step call (busy fraction 0.82). Per audio s: DSP 1.000G
instructions, ColdFire 132M.

- **H1 (one worker, costs add): true, but not the main cause.** `digi_step` runs the DSP synchronously inside
  the ColdFire DSPI2 frame write (packages/emulator/boot/src/abi.rs `mod coupled`), in the one worker
  (apps/web/src/runtime/runtime-worker.ts). CDP CPU profile of that worker (6 s, default flags, share of
  non-idle time): DSP 77% (generated `sharc_gen` kernels plus the engine), ColdFire 21%, other 2%. Per audio s
  that is about DSP 2.6 s and ColdFire 0.7 s, against the desktop's 0.93-1.10 and 0.5. Serial on the
  desktop numbers the page would reach about 0.66; it reaches 0.25, so the wasm is also about 2.5x (DSP) and
  1.4x (ColdFire) slower than native.
- **H2 (generated code stuck in Liftoff): refuted as the main cause.** Liftoff only is about 3% slower than
  default. TurboFan only reaches 0.27-0.29 in the last half of a run, after a first half of 0.17-0.19 (lazy
  compile of the 45 MB module), against 0.26 for default. Steady-state TurboFan is at most about 10% faster
  than default, so the 2.5x gap to native is not a missing tier-up. Top self time: `image::blocks_10` kernels
  (4.9% and 3.1%), `Emulator::step_chunk` 5.7%.
- **New: about 16.5% of the worker is idle by construction.** The profile's `(idle)` is 16.2-16.5%, the
  complement of the busy fraction. The pump runs 20 ms slices and re-arms with `setTimeout(0)`; nested timers
  clamp to 4 ms, and 4 / 24 ms = 16.7%. A `MessageChannel` yield or a longer slice should give about
  0.25 -> 0.30. **[O]** not tried.
- Getting to real time in one worker needs the DSP about 2x faster in wasm; the alternative is the DSP in a
  second worker (shared memory, needs cross-origin isolation). **[O]**

## Browser: yield and split workers 2026-10-11 **[D]**

Same method as the baseline above (`mise run web:bench -- --seconds 10 --runs 3`, headless Chromium 150, DN2 1.11,
`tap(25)` at load, exclusive benchlock, one machine, load not controlled, not re-checked by a second agent). The
core was rebuilt for this lane (adds `digi_dsp_*`, `digi_load_coupled_remote`); `--split 0` keeps one worker.

| Step | audio s / wall s (3 runs) | mean | ColdFire worker busy / audio s | DSP worker busy / audio s |
|---|---|---:|---:|---:|
| baseline: one worker, `setTimeout(0)` re-arm | 0.251, 0.247, 0.252 | 0.250 | 3.28 (both chips, 82% of wall) | (same worker) |
| one worker, `MessageChannel` yield | 0.315, 0.304, 0.294 | 0.305 | 3.2-3.4 (99.8% of wall) | (same worker) |
| one worker, yield, rebuilt core (`--split 0`) | 0.312, 0.301, 0.312 | 0.309 | same | same |
| two workers over a SharedArrayBuffer | 0.394, 0.390, 0.389 | 0.391 | 0.71-0.72 (blocked on the DSP the rest) | 2.57-2.59 |

- **Yield: confirmed.** Replacing the 4 ms-clamped `setTimeout(0)` by a `MessageChannel` task in
  `runtime-worker.ts` moves the worker from 82% to 99.8% busy and 0.25 to 0.30-0.31, as the idle share predicted.
  Input still interleaves (the queue is the worker's task queue; `web:test` passes with the harness faking the channel).
- **Split: 0.39, bounded by the DSP.** The DSP instance's 2.57 s per audio s equals the whole run's wall time per
  audio s (2.56): the ColdFire's 0.71 s overlaps it entirely. The split is pipelined like the native `ThreadedPeer`:
  the DSP worker answers frame N after the SPI exchange and renders the period afterwards (`RemoteDsp::reply` /
  `render`, `digi_dsp_reply` / `digi_dsp_render`), so the ColdFire runs up to one frame ahead. The branch's
  `digi_dsp_exchange` (reply and render in one call) would have been serial; it is kept as reply then render.
- **Exactness [V under Node, D in the browser].** `node tools/devtools/wasm_parity.mjs --scenario trig1-tap-release --split 1`
  (ColdFire instance in the main thread, DSP instance in a `worker_threads` worker, frames over the same
  `sab-link.ts`) gives `883476213933333f0c5f4e2b343f65ed470fda0c50c8fa37905972457e819cdd`, the pinned native hash
  and the single-instance wasm hash (816000 values, 8.5 s). Wall 23.6 s against 30.6 s single. In Chromium the Trig
  tap lands at a host-timing dependent instruction count, so the PCM cannot be hashed there; the bench reports
  `pcm_peak` (0.0549 in both modes), `missing_blocks` 0 and no halt.
- **Setup.** The page needs cross-origin isolation: the astro dev server now sends COOP `same-origin` and COEP
  `require-corp` (`astro.config.mjs`), `splitAvailable()` in `runtime.ts` enables the split when the page is isolated
  and the core exports `digi_dsp_reply`, else (or with `?split=0`) the single worker runs. The page compiles the core
  once and hands the `WebAssembly.Module` to both workers. `coi-serviceworker.js` from the parked branch is in
  `public/` but not loaded (GitHub Pages has no coupled core anyway). The generic-core build step of the branch's
  `tools/emulator/native_wasm.py` was left out.
- **Why wasm is still about 2.5x the native DSP.** Not measured further here. Known from above: not tiering; the
  browser core has no fast tier (V8 runs the AOT blocks; native reaches 0.93-1.10 s per audio s with its fast tier
  and AOT), so the remaining gap is wasm codegen of the generated blocks (bounds checks, no SIMD in the generated
  Rust for wasm32, 32-bit pointers). The wasm fast tier of the parked branch (`digi_sharc_fast`, generic core)
  is a separate path. **[O]**

## DT2 FX scenarios and profile 2026-10-11 **[D]**

Method. DT2 1.16 (`sections/.source-sha256` = 278541e4...), the one-sample card of `tools/emulator/dt2_sample_check.py` (1 kHz sine,
1 s), core `gen-lane-entry` as the baseline, new core `out/native/dt2-audio/gen-lane-fx` (PROVENANCE.txt there; neither is
promoted). Two lanes' work on one machine: load average 3 to 130 during the runs, so every timing is a paired A/B.
Not re-checked by a second agent.

**Scenarios.** `tests/scenarios/dt2-trackfx.json` (25) and `dt2-sendfx.json` (35), generator `tools/emulator/gen_dt2fx.py`, same names and
layout as the DN2 files (`tf-*`, `sf-*`) so `tools/emulator/fxcheck.py` (`configure("dt2")`: F0 1 kHz, Sound array
`*0x8000470c+0x14:142`, Fx array `*0x80004708:142`) and `tests/scenarios/conftest.py` (`--device dt2`) read them. The
panel is the DN2's (finding 03, "DT2 1.16 FX pages"). Each run is 2 to 4 audio s: NO prelude, a 56-slot sweep (1M
instructions per slot), Trig 1. `coupled.py scenario --device dt2 --dt2-card` (new flag, `DIGI_DT2_CARD=1`) runs them on the
card's ready state; `tools/emulator/dt2fx.py render|check|tiers` renders both files with the peeks, prints the fxcheck tables and
compares PCM hashes. Oracle tests: `tests/scenarios/test_dt2_fx.py` (146 cases, 106 s, `mise run scenarios:dt2`).

- **Read-back [D].** The intended parameter lands on a calibrated UI value (BR/overdrive/SRR 22, 54, 127; sends 54, 127;
  delay time 0, 30, 100; feedback 0, 39, 109; reverb decay 0, 37, 108; and so on) and no other stored parameter differs from
  the reference, in all 52 non-reference scenarios. The DN2 event counts do not transfer (44 events give UI 90, not 127; the
  value is not smooth in the count: 48 events 127, 50 events 113), so the counts are calibrated by read-back per layout
  (`gen_dt2fx.py OUT --ladder`).
- **Audio direction [D].** BR: error against dry -52.1, -19.5, +0.7 dB and THD -56.9, -21.2, -7.3 dB for 22, 54, 127;
  overdrive: 3rd harmonic -40.3, -35.6, -25.3 dB, gain +2.7, +7.1, +15.8 dB; SRR: lowest alias image 12000, 2525, 475 Hz,
  error rising; BR 0 is bit-identical to dry; filter at UI 60 takes 35 dB off the 1 kHz note; SRR/overdrive routing 1 gives a
  different signal with the filter closed. Sends: wet RMS rises with the send (delay -48.2 to -33.4 dB, reverb -60.4 to
  -48.1, chorus -52.2 to -37.3). Delay: first echo at 0.087, 0.443, 0.568, 1.863 s for time 0, 23, 30, 100; repeats change by
  -9.4, -4.6, -3.1, +4.7 dB for feedback 0, 32, 39, 109 (above unity grows, peak 0.69); width 0 puts the echo hard left (R
  92 dB lower), 127 right (L 36 dB lower); pingpong follows the width; LPF 0 takes 29 dB off; mix 0 leaves no echo. Reverb
  RT60 1.41, 1.94, 2.12, 6.06 s for decay 0, 31, 37, 108; pre-delay 127 delays the onset by 0.38 s; mix 0 is silent. Chorus
  rate 0.86, 0.99, 3.11 Hz for speed 0, 32, 127; depth 0 removes the modulation (L/R wet correlation 0.999 against 0.04);
  mix 0 is silent. Master compressor (fully wet): RMS change -7.7 (ratio), -3.6 (threshold 0), +7.5 (mix only), +19.5 dB
  (makeup 127).
- **DT2 against DN2.** Delay repeat step at the default feedback is -4.6 dB (DN2 -6.0); echo spacing is about 18.4 ms per
  TIME step against DN2's 15.625 ms (not proportional to TIME + 1; the tempo of the ready state is the likely cause, not
  checked) **[O]**; chorus Depth and Width start at their maximum (a +4 turn changes nothing).

**Tier equality [D].** All 60 scenarios give the same PCM sha in the default config, in `SHARC_AOT=0` (interpreter only; DT2
has no fast regions, so that is the only other tier) and, on `gen-lane-fx`, in the default config again: 60 of 60 for both
cores. The no-AOT render of 197 audio s took 2189 s on six jobs (about 67 CPU s per audio s). `tools/devtools/verify.py --full --dt2`
(step `dt2-tier-sweep`) does the same comparison with `coupled.py scenario --device dt2 --dt2-card`.

**Profile and regeneration.** `aot_fxprof.py --device dt2 --dt2-card` over `dt2-sample-trig` and 18 FX scenarios, merged with
the entry-lane r4 profile, then two entry rounds (3 new block starts, then 1; the next one is a 190k-count chain of one
instruction per round, as before: stopped). The FX scenarios change the DSP instruction mix much less than expected:
block instructions differ by 0.0% to 0.4% between a dry run and any send or master scenario, because the send FX, reverb
and the compressor code run the same instructions whatever the sends and mix are. Interpreted instructions per 2 audio s:
3.95M dry, 3.95M with overdrive or SRR, **5.3 to 5.5M with bit reduction** (+1.4M: the only effect with its own interpreted
path), 7.9M per 4 s with sends. So the claim above, that FX code stays interpreted whenever an effect is on, was wrong
**[C]**: the baseline trig profile already covers it, except for the bit-reduction code. In `gen-lane-fx` the BR scenario
interprets 3.76M (was 5.5M); over the 60 scenarios the total falls from 396M to 372M (-6.3%); block traps are equal in 60
of 60.

| Paired DSP CPU per audio s, 6 exclusive pairs | gen-lane-entry | gen-lane-fx | paired ratio |
|---|---:|---:|---:|
| dt2-sample-trig (3 s) | 1.176 | 1.180 | 1.003 (1.001 to 1.010) |
| tf-br-127 (bit reduction on) | 1.303 | 1.189 | **0.913** (0.909 to 0.917) |
| tf-srr-127-flt-rt1 | 1.178 | 1.186 | 1.006 (1.002 to 1.016) |
| sf-del-ppwidth-u50 | 1.171 | 1.181 | 1.008 (1.004 to 1.012) |
| sf-mst-thr0 | 1.178 | 1.186 | 1.006 (1.001 to 1.015) |

PCM hashes are equal in every pair. The new core has 1256 blocks / 13,654 instructions against 1241 / 13,398. Verdict:
bit reduction on is 8.7% cheaper, everything else is 0.3% to 0.8% dearer (more entry checks and a larger table; not
analysed), so **do not promote `gen-lane-fx` over `gen-lane-entry`** unless bit reduction matters; the FX profile has no
other payoff on DT2. Gate on `gen-lane-fx`: `dt2_sample_check.py --seconds 3.0` is a clean tone (peak share 0.99995,
harmonics below -89 dB) with WAV hash `4bab8330915f0929` in AOT, AOT + fast and `SHARC_AOT=0`; block traps unchanged (36,000
for the 3 s trig; equal in all 60 scenarios); the oracle tests pass on both cores (146).

**[O]** What is left interpreted: about 1.9M instructions per audio s in every DT2 run. The largest single source is the entry
bail at 0x1c06fe (455,870 interpreter runs in the 3 s trig, a gated entry that is already a block start); the rest was not
analysed.

DT2 PGO, what it would need (measured by reading, nothing built): `tools/emulator/native_pgo.py` trains one profile for DN2 only
(state in `out/native/pgo/dn2/<triple>`; the workload is the `coupled_ready_exactness` gate run from the instrumented
desktop app with `--pgo-train`, and the gate restores the DN2 ready snapshot). There is no DT2 training workload. It would
need (1) a device argument for the state directory and `SHARC_GEN_DIR`, (2) a DT2 workload: the card's ready state
(`--card-image`) with `dt2-sample-trig` and three or four FX scenarios (tf-br-127, sf-del-ppwidth-u50, sf-mst-thr0, about
15 audio s) as a `coupled-run` scenario batch or as a DT2 variant of the gate, (3) the A/B on tf-br-127 and the trig with
`bench-bins`. Cost: one 6-minute train and one 6-minute bench; headless DT2 is at 1.18 DSP CPU per audio s without PGO
(DN2 0.93 with it).

## Browser: why the wasm DSP is slower 2026-10-11 **[D]**

Question: the dev page's AOT SHARC core in wasm costs about 2.6 DSP-worker s per audio s (DN2 1.11, headless Chromium
150, split workers), native 0.93-1.21. Method: `tools/devtools/browser_bench.mjs` against a private dev server
(`--url`, below), `--seconds 10 --runs 2-3`, exclusive benchlock; native by `coupled.py scenario trig1-taps
--seconds 10 --no-build` (prod core, no PGO) with and without `SHARC_FAST_REGIONS=`. One machine; other lanes run
unlocked jobs, so consecutive identical runs differ by up to +-8% (base core: 2.46-2.71 over 7 runs). Not re-checked
by a second agent.

- **Build configuration: same as native, nothing wrong.** `Cargo.toml` `[profile.release]`: opt-level 3, no LTO,
  16 codegen units, no debug, no overflow checks, panic=abort (forced on wasm32-unknown-unknown); the desktop
  build uses the same profile (plus PGO in the launcher). Target features at rustc 1.98.1: bulk-memory,
  multivalue, mutable-globals, nontrapping-fptoint, reference-types, sign-ext; not simd128, tail-call, atomics.
  `tools/emulator/native_wasm.py` runs no `wasm-opt` (none installed) and passes no RUSTFLAGS. The core has a name section
  (45 MB). Native differs only by PGO (0.93 against 1.10) and the fast tier.
- **Tier split.** The browser core runs no fast tier, so the instructions the desktop runs as Cranelift kernels
  run in AOT blocks. Native, 10 audio s, per audio s: default 235M AOT + 267M kernel insns (0.50M kernel calls),
  0.6M interpreted, DSP CPU **1.209**; with `SHARC_FAST_REGIONS=` 472M AOT + 28M kernel (blocktrans clusters) +
  1.2M interpreted, **1.498** (3.2 ns per executed insn); same PCM. So the fast tier is worth 19% native, and the
  browser at 2.6 (about 5.4 ns per executed insn) is **1.74x native AOT-only** (the wasm codegen share) times
  **1.24x** for the missing tier: 1.21 x 1.24 x 1.74 = 2.6. Browser instructions per audio s as in the baseline
  (1.31G with idle skip).
- **Profile** (CDP, DSP worker, 5 s, 15.0 s sampled, `--profile` now reports every worker and
  `--profile-all FILE` dumps all self times): AOT regions `r_*`/`b_*` **84.9%** (1209 functions; top r_1C399A 6.4%,
  r_1C3862 4.0%, r_1C364F 3.5%, r_1C9504 2.6%, r_B8276C 2.2%, r_1C5765 2.1%, r_1C1884 1.5%, r_1C0CA0 1.4%,
  r_1CDBAC 1.4%, r_B823DB 1.4%), `Engine::step` 3.6%, `exec_insn` 1.5%, generated per-insn interpreter (`core_g`)
  3.8%, sharc_rt 1.3%, SAB-link wait/serve 2.0%, other glue 0.7%. No single function stands out; the distribution
  is the native one with the 53% kernel share added to the regions. Out-of-line helpers in the hot regions
  (`normal_word_map`, `nan_order`, `__dm_read_full`, `__multi3`, `__fixdfti`) never appear in self time. A hot
  region (r_1C399A, 24 KB of wasm) has 207 i32 and 139 i64 loads, 97 calls and one `call_indirect`: the register
  file and state live in linear memory as natively.
- **Refuted (a) guesses of the earlier note:** bounds checks (`--js-flags=--no-wasm-bounds-checks`: no change, the
  trap handler is active), wasm inlining and loop peeling flags (`--no-wasm-inlining`, `--no-wasm-loop-peeling`,
  `--wasm-loop-unrolling`: all within noise), Rosetta (Chromium and Node are arm64). **[C]** "No SIMD in the generated
  Rust" is not the gap either (below).
- **Experiments** (DSP worker busy s per audio s, runs in order; parity = `wasm_parity.mjs --scenario
  trig1-tap-release` single instance, and `--split 1` where noted, pin `883476213933...819cdd`):

| change | audio s / wall s | DSP busy / audio s | parity |
|---|---|---|---|
| base (as `tools/emulator/native_wasm.py`) | 0.37-0.41 | 2.71 2.56 2.70 / 2.47 2.46 2.59 2.64 | match, split match |
| `lto=fat`, `codegen-units=1` (25 min build) | 0.36-0.37 | 2.75 2.70 2.77 | match |
| `sharc-gen` opt-level `s` | 0.36-0.41 | 2.47 2.46 2.47 / 2.74 2.79 2.82 2.60 | match |
| `-C target-feature=+simd128` | 0.37-0.44 | 2.40 2.30 2.30 / 2.53 2.64 2.60 2.76 | match, split match |
| simd128 + `sharc-gen` opt-level `s` | 0.36-0.40 | 2.63 2.79 2.75 / 2.79 2.60 2.50 2.73 | match, split match |
| simd128 + `sharc-gen` opt-level 2 | 0.37-0.41 | 2.48 2.48 2.43 / 2.65 2.60 2.55 2.71 | match, split match |

  No variant beats the base outside the noise (means 2.59 base, 2.74 LTO, 2.62 s, 2.50 simd128, 2.68, 2.56). The
  first batch of every variant ran in a quiet period (about 2.3-2.5) and the interleaved A/B (2 rounds, same
  order) did not repeat the gain, so I read it as load, not as a gain. simd128 changes the code (24 `v128`
  loads/stores and some `f32x4` in r_1C399A) but not the time. No build change is kept.
- **Ceiling (a), AOT core with build fixes: about 2.4-2.6** (confidence medium-high; the build knobs are exhausted
  short of `wasm-opt`, which is not installed and which V8's TurboFan mostly duplicates). The 1.7x is V8's
  code for LLVM's wasm output against LLVM's arm64 output for the same Rust.
- **Ceiling (b), the fast tier in the browser core: about 2.0 (1.8-2.3), confidence low.** Not measurable here:
  `digi_sharc_fast` panics on the first kernel build on `wasm32-unknown-unknown` (`std::time::Instant::now`; fixed
  in `fast/mod.rs` by a clock shim), and with the DN2 static regions (`tools/emulator/fast_regions.json`, tried via a
  temporary `threshold 0` seeding) it stops at `Op::Helper` (`unimplemented!`, `sharc-fast-wasm/src/lib.rs`).
  Auto selection (256,64,0) is exact (3 s hash equals the AOT-only 3 s hash `ae86d0b3...`) but arms only 19
  kernels worth 0.4% of the instructions. Model from the native numbers: AOT 235M x 5.4 ns = 1.27 s, plus the
  kernel part (native 0.46 s: 0.13 s kernel body at 1.15x for V8, 0.33 s engine round trips at 1.7x, plus about
  0.1 s for 0.5M `host_wasm_run` crossings) about 0.8 s: 2.1 against 2.6, the same 19% as native. Real time
  (<= 1.0) is out of reach for AOT plus today's regions in one worker either way; it needs kernels that follow
  calls and chain (fewer than 1M calls per audio s at 0.5 ns per insn), which is the engine work of step 2.
- **Bench pitfalls found.** (1) `browser_bench.mjs` without `--url` starts `astro dev --port 4399`; if another
  worktree's server owns 4399 the run silently measures that server's core. (2) Astro 7 keeps one dev server per
  `apps/web/node_modules/.astro`; a worktree that symlinks `node_modules` as a whole shares that lock and its
  start is skipped. Give the worktree a real `node_modules` directory with symlinked entries (not `.astro`), start
  `astro dev --port N` once and pass `--url`. The first baseline numbers of this lane (0.36-0.40) were taken against
  another lane's server; the figures above are from a private one and agree.

## Browser: fast tier in wasm 2026-10-11 **[D]**

Question: what does the fast tier buy in Chrome, replacing the modelled 1.24x? Setup as the previous browser lanes
(private `astro dev` on its own port, `--url`, real `node_modules` directory, exclusive benchlock; headless Chromium 150,
split workers, `--seconds 10`; other lanes ran unlocked jobs, so single runs differ by up to +-8%). Not re-checked by a
second agent.

- **`Op::Helper` in the wasm emitter.** A kernel with a helper call imports `env.helper(st, rec) -> status` (function 0,
  `k<i>` shift by one; only when a kernel needs it). The page links the import to the core's `digi_helper_exec(st, rec)`
  export (`packages/emulator/boot/src/abi.rs`, `helper_exec` with `rec` as the wasm32 pointer), a wasm-to-wasm call with no JS
  trampoline; `core-host.ts` and `tools/devtools/wasm_parity.mjs` pass it. A record above 32 bits is refused (`emit_function`
  returns Err; the region falls back to the interpreter). No helper kernel arises in the shipped DN2 config (helpers
  need `SHARC_FAST_HELPER`, which the browser core cannot set, or the `exit0` rebuild, which did not fire in
  these runs), so the path is tested apart: `cargo test -p sharc-fast-wasm` (module imports `helper`; wide record
  refused) and `node packages/emulator/sharc-fast-wasm/helper_check.mjs` on the emitted module (V8 validates it; the stub sees
  `(st, rec)` per iteration; status 0 leaves with exit 3). **[O]** a real helper kernel in the browser, which needs a
  way to turn `SHARC_FAST_HELPER` on without an environment (`cfg.rs`).
- **Build and seeding.** `tools/emulator/native_wasm.py` now builds the coupled core with `fast-wasm` (`--no-fast` leaves it out;
  the tier stays off until the page asks). New ABI `digi_sharc_fast_regions(regions, n, roots, n, threshold, min_insns,
  follow)` = the desktop configuration: the static regions and the block-translator cluster roots of
  `tools/emulator/fast_regions.json` for the firmware's SHA-256 (`FastEngine::new(..).with_clusters(..)`), optional run-time
  selection on top (`threshold != u32::MAX`). Page: `?fast=desktop|desktop+auto|auto|T,M,F`; `browser_bench.mjs --fast X`;
  `wasm_parity.mjs --fast desktop[+auto]` (also with `--split 1`). The bench reports the kernel host's counters and
  `digi_sharc_fast_report` (`dsp_kernels`, `dsp_fast`). The dev page is the only consumer: `?fast=` is off by default.
- **Exactness [V under Node].** `node tools/devtools/wasm_parity.mjs --scenario trig1-tap-release --fast desktop` and
  `--fast desktop --split 1`: `883476213933333f0c5f4e2b343f65ed470fda0c50c8fa37905972457e819cdd` = the native pin
  (816000 values). Generic core, `--fast desktop+auto --split 1`: the same hash. In Chromium `pcm_peak` 0.0549 and
  `missing_blocks` 0 in every run. Web tests (`node --test`, 10) and `astro check` (0 errors) pass.
- **Kernels.** DN2 desktop config: 23 kernels (11 regions + 12 clusters), 0.09 MB wasm, 0 refused. Chrome: module
  compile + instantiate 2.3 ms in total (Liftoff compiles lazily), the first call of every kernel 5 ms in total
  (compile plus one run), so the cold start is about 7 ms; 5.0M kernel calls per 10 audio s. With `desktop+auto`: 41
  kernels, 0.38 MB, 4.6 ms + 40 ms. Node: 2 ms for the 23 kernels.

AOT core (`SHARC_GEN_DIR` = prod gen) with fast tier off or on, 10 audio s, split workers:

| config | runs | audio s / wall s | DSP worker busy / audio s | kernel insns / non-idle insns |
|---|---|---|---|---|
| AOT only | 8 (2.34-2.77) | 0.399 | 2.54 | 0 |
| AOT + fast, desktop config | 8 (2.05-2.29) | 0.469 | **2.16** | 53% (2.67G of 5.0G per 10 s) |
| AOT + fast, desktop + auto 256,64 | 5 (1.93-2.24) | 0.46-0.52 | 2.11 | 53% |

  Interleaved ABA runs (3 each): 2.62 / 2.15 / 2.22. **The fast tier is worth 15% in Chrome (2.54 -> 2.16), against
  19% native and 19% modelled (2.6 -> 2.1)**; audio per wall second 0.40 -> 0.47. The ColdFire worker (0.71 s per
  audio s) is then blocked 1.4 s of every 2.2 on the DSP. `desktop+auto` adds 18 kernels and no instructions
  (interleaved it is not better than `desktop`; the 1.93-1.98 runs were from a quiet period). The kernel share of
  executed instructions equals native (53%).

Generic core (`out/native/shared-core/gen`, 3.2 MB, no AOT; the DSP image is decoded at run time), one 10 s run each,
the same page, DSP worker busy per audio s and share of the 4.70G non-idle instructions the interpreter runs:

| config | DSP busy / audio s | audio / wall | interpreted share | kernels (compile, first calls, wasm) |
|---|---|---|---|---|
| no fast tier | 88.9 | 0.011 | 100% | 0 |
| desktop regions | 42.3 | 0.024 | 44% | 24 (2 ms, 5 ms, 0.09 MB) |
| desktop + auto 256,64 | 24.3 | 0.041 | 25% | 617 (41 ms, 566 ms, 6.4 MB) |
| auto 256,64 + follow | 17.8 | 0.056 | 18% | 2234 (163 ms, 1.75 s, 35.7 MB) |

  Exact against the pin (Node, `desktop+auto`, split). The interpreter runs at about 190 ns per instruction in the
  browser (88.9 s for the 4.70G non-idle instructions; the other rows fit it: 2.08G x 190 ns = 400 s of 423 s
  for `desktop`), and the kernel engine's job is to take that share to zero: at the AOT's 5.4 ns per instruction
  all 4.70G would cost 2.5 s per audio s, the AOT core's 2.2-2.5. So the generic core can only reach AOT speed if
  nearly all instructions run in kernels. Today the interpreted share (18-44%) and the 58M kernel calls per 10
  audio s of `auto+follow` (43.6M ran, 13.9M declined at the shape check) dominate, not kernel compile time (first
  calls 1.75 s once, 35.7 MB of wasm in memory).
- **Open.** Single-worker mode (`?split=0`) still stubs the kernel host (`runtime-worker.ts`); a helper kernel in the
  browser; the Safari/Firefox timings; compile in a worker pool (design doc, stage 4) is not needed for the desktop
  config (7 ms) but would be for the generic core (0.6-1.8 s on the DSP worker).
## Lowering: register-count loops and the I7 push 2026-10-11 **[D]**

Two coverage features of the CFG lowering (lane K2, branch `worktree-agent-a39ce353f62582daa`), each exact against the
interpreter and switchable for an A/B (`SHARC_FAST_NOREGN=1`, `SHARC_FAST_NOPUSHDYN=1`). Not re-checked by a second agent.

- **A. DO loops with a register trip count.** `Lower::note_reg_count` (`lower/flow.rs`) reads the abstract value of the
  count at the DO. If it is exact (`lo == hi`, no `ts`/`nf` term) the loop gets a known count: a constant 1..=u32::MAX
  becomes `Count::Lit`; `Base::Reg(c) + k` becomes the region's one symbolic N (`Count::Entry` semantics: `ts` while
  widening, `nf` after the loop), but only when the region has no entry loop and no earlier loop took N.
  `Lowered::reg_n` -> `CfgMeta::reg_n`; `glue::run_cfg` sets `iters = r[c] + k` at entry (register known, result in
  1..=u32::MAX, else `Decline::Shape` with no rebuild), and `iters` feeds `nw_ok`, the window spans and the lean path as
  before. The kernel still reads the count register and guards it non-zero. A pass-2 change of the count source refuses
  the region (one refusal in 1800 fuzz runs). Regions with call targets placed before their callers keep the old
  behaviour (a count source that appears in a later pass keeps the call-entry ranges from settling).
- **B. The push in a call delay slot.** `form_16a` (`lower/callret.rs`) is now `dm_transfer(post, StoreV(imm))`, so an
  I7 with no static range gets the guarded dynamic window anchored on I7 (`xfer_at`/`dyn_xfer`: a store outside the
  span leaves the kernel before the instruction). Same restriction: not in regions with call targets, where the 64
  regions it newly built were refused with "call entry: register ranges not stable" and the fast-only interpreted
  instructions rose 12%. **[O]** K1's call-entry convergence is the next blocker there.
- **Found on the way.** `glue::sites::apply`: in the stack model a DO that ends in the region (`CTX_STK26 == 2`) set
  STKYX bit 26 known even where the entry bit was unknown; the interpreter keeps it unknown (value bit as at entry).
  Newly exposed by `fast_calls_and_returns_match_the_interpreter` ("callee loop"), now fixed.

Tests. `fast_register_count_loop_with_stepped_pointers_matches` (R5 = R7 - 2 and R7 = R7 - 2, two stepped pointers, a
pointer read after the loop, counts 1..40, 3000 and 0x10000002; asserts `reg_n == (7, -2)`),
`fast_call_slot_push_through_an_unranged_stack_pointer_matches` (fails with `SHARC_FAST_NOPUSHDYN=1`: the call becomes a
stop). `cargo test -p sharc-native -p sharc-rt` passes. `fast_diff cfg` with the new `--regloop` (DO loops counting
R12 +- 1; 61 regions with a register trip count per 300 cases) and `--pushcall` (delayed CALL with a 16a slot) over
`--stride`, `--shape cfg`, `--circ` and other seeds: 0 mismatches. `verify.py --only build,pins,check,check-no-aot`
passes. Failures that are not from this change (identical with both switches set and on other lanes' binaries):
`fast_diff cfg --simple`, `--check-windows` with `--shape cfg` or past 100 cases ("window access ... outside every
window"), `fast_diff forms --trials 1000` (2a, 2a_short, 2c, 3a, 4a, 5a, 7a). **[O]** The fuzz does not catch an
under-sized N: a mutant `iters = n - 1` survived 100 cases of `--regloop --stride --check-windows`; the argument for
the windows rests on the Abs algebra shared with `Count::Entry` and the unit test.

Census (`SHARC_FAST_COV`, DN2 trig1-taps 3 s, every AOT entry of the gen plus the shipped list armed, 2692 roots,
before -> after):

| root | kernel insns per call | note |
|---|---:|---|
| 0x1cdb85 (R7 = R15 - 2, I3 += 4) | 55.0 -> **484.0** | 72,000 calls, now runs to its return |
| 0x1c963b | 35.3 -> 38.3 | |
| 95 roots cut by a 16a push | 30.3M -> 34.4M insns in total | +3 insns per call (the push and its slot) |
| 0x1c0c9d, 0x1c926f, 0x1c254e, 0x1c248a, 0xb82a5b, 0x1c25cc | unchanged | other stop reasons |

Denials at a CJUMP (25a_direct) by "address range unknown", weighted by root calls: 4.25M -> 0.64M (what is left is in
regions with call targets). All denials 969 -> 777. The loop "register step not stable" denials stay (301 -> 310): the
other loops are not register-count loops. Top stop reasons now (denial weighted by root calls): 8a_rel address range
unknown 0.70M, 25a_direct address range unknown 0.64M (call targets), MODE1 write with the bank model 0.64M, condition
in SIMD mode 0.60M, 8a_rel address range unknown (another insn) 0.57M, loop step in a 3a body 0.40M.

Timing (trig1-taps, 8 audio s, 3 runs each, exclusive lock, load 2 to 4, PCM `f679cda8f1233791` equal in all runs):

| config | DSP CPU per audio s, before -> after | other |
|---|---:|---|
| fast-only (`static-insns+follow`, no AOT) | 5.02 -> **4.86** (-3.2%; includes about 3 s of one-time compile of 6000 kernels) | region calls 91.9M -> 87.0M (-5.3%), interpreted 1.45% -> 1.44% |
| default (shipped regions) | 1.107 -> 1.108 | no region changes |
| shipped + 0x1cdb85, 0x1c0c9d, 0x1c926f, 0x1c25cc, 0x1c963b | 1.131 -> **1.110** (-1.9%) | same calls |

## Kernels that follow calls: proof 2026-10-11 **[D]**

Question: can a fast-tier kernel follow a call into its callee and come back (CALL ... RETURN inside one kernel), and
does that deliver the modelled gain? One lane, base 9bba563, DN2 1.11 `trig1-taps` 10.5 audio s, PCM `e0e6c8af7a84c443`
in every run listed. Not re-checked by a second agent. Switch: `SHARC_FAST_RETFOLLOW=0` turns the new code off.

**What the compiler call is.** The firmware's calls are `25a_direct` (CJUMP, a delayed jump with `R2 = I6; I6 = I7`, the
return address pushed by a `16a` store in the slots) and the return is the indirect `JUMP (M14, I12) (DB)` with
RFRAME in its slots (`cfg::NodeKind::Return`). Neither touches the PC stack, call stack or `at_loaded_entry`
(stack model), so a kernel can follow them with no state to replay at an exit. The kernel already followed the CALL
(an edge inside the region when the target lies within 0x1000 and in the same loop context); only the return always
left (`Site::pc == DYN_PC`). That is the 39% "ret" share of the run-length data. Hardware `CALL`/`RTS` (`Tail::Call`,
`Tail::Rts`) are not touched.

**Design (cfg.rs, lower/flow.rs).**
- A CJUMP whose callee is in the kernel makes the instruction after its slots a continuation (`Structure::conts`);
  each `Return` of that callee (a walk that steps over nested calls) gets the continuation in `NodeKind::Return::conts`.
- The lowered return stores the run-time target as before, then for each continuation `BrIf(target == cont)` to its
  label; no match leaves at `DYN_PC` as before. Exact by construction: the kernel continues where the interpreter
  would land, and a mismatch exits with the same state as today.
- At the continuation the register ranges are the call site's minus every register the callee (with everything it
  calls) can write (`Plan::node_written`), MODE1 and known flag bits as at the call (`Lower::cont_saved`); loop heads
  also blank what followed callees write (`written_in`), and a compare/forget pair across a dispatch is refused.
- A call inside a loop to code outside the loop's range: the callee's instructions take the caller's loop context
  (`propagate_call_ctx`, only when the callee has no DO of its own and no other context uses it).
- Calls follow at any distance (`CALL_REACH` 0xff_ffff), `MAX_CFG_JUMPS` 16 -> 64 and `MAX_CFG_LOOPS` 12 -> 24 (ctx
  layout), `SHARC_FAST_MAXNODES` (default still 400).
- The I7 push (K2's item): `form_16a` now goes through `dm_transfer_w` (static, guarded dynamic or flat window like the
  other stores) instead of a static window only. Minimum change; K2 may replace it.
- The `STKYX` bit 26 bookkeeping needed two fixes the new unit test found: a DO after a loop end in one run (var value 3)
  and a loop end with STKYX not fully known (the generic core's `_bitwise` gives Unknown) in `glue::sites::apply`.
- Diagnostic: `SHARC_FAST_KTIME=1` prints cumulative wall ns per CFG kernel root (kernel.run only).

**Exactness [V by pins].** `cargo test` workspace (prod gen) passes, with the new `fast_kernel_follows_calls_and_returns`
(runs of >= 100 insns across nested calls, call in a DO loop, callee DO loop) and the existing
`fast_calls_and_returns_match_the_interpreter` (state equal to the interpreter at five step sizes; it found the two
STKYX cases). `verify.py --only build,pins,check` and `--only check-no-aot` pass; trig1-taps with the chain armed passes
10.5 s. `fast_diff` was not run (it needs a captured state and has no call/return generator; the `16a` change is covered
by the pins only).

**Chain (default config, shipped regions plus 0x1c94f7, 0xb84d99, 0xb84d12, 0xb836ff armed; exclusive lock, 2 paired
rounds, DSP s per audio s excluding the one-time 0.62 s compile and build).**

| | run time | region calls/s | kernel insns/s |
|---|---|---|---|
| base, shipped regions | 1.129 1.128 | 0.503M | 266.7M |
| base, chain armed (kernels stop at the 16a push) | 1.134 1.127 | 0.532M | 266.8M |
| new, chain armed | 1.115 1.118 | 0.461M | 289.6M |
| new, `RETFOLLOW=0`, chain armed | 1.133 1.151 | 0.532M | 266.9M |
| new, shipped regions only | 1.127 1.160 | 0.503M | 266.7M |

Runs differ by +-0.015. Root 0x1c94f7: 20 -> 15,705 insns per call (72k -> 4.5k calls per 3 audio s); 0xb84d12: 24 ->
2,447. Model: 22.8M insns/s move from AOT (4.4 ns) to kernel (0.5 ns) and 71k calls/s disappear (266 ns):
saves 108 ms/s (9.5%). Measured 15-35 ms/s (1.5-3%).
**In-situ ns per instruction** (`SHARC_FAST_KTIME`): 0x1c94f7 2.08 (Cranelift `none`) and 1.57 (`speed`), the shipped
cluster 0xb8276c 1.01 and 0.82, against the model's 0.5. With 2.08 the model gives 71 ms/s. The cause is op count: the
0x1c94f7 kernel has 19-21k ops for 384 insns (50 per insn; 0xb8276c 19) and 167 flat guarded accesses (105 are `15b`
frame loads), because I6 and I7 lose their ranges across the call (the callee writes them, `RFRAME` restores them, the
analysis cannot see that).

**Fast-only (no-AOT, `static-insns+follow`, every instruction a root), 10.5 s, PCM pin held:**

| | base | new |
|---|---|---|
| region calls/s | 11.52M | 8.70M (-24%) |
| interpreted insns/s | 7.3M | 12.3M |
| run time (CPU - compile - build) | 2.369 | 3.028 (+28%) |
| compile / build (s per run) | 16.5 / 3.5 | 35.9 / 19.2 |

Exits per 3 audio s: edge 13.2M -> 5.3M, ret 8.6M -> 5.2M, stop 7.1M -> 8.8M, total 30.4M -> 20.7M (-32%). The stops
grew because a CALL whose slot store has no range (I7 after an earlier followed call) is denied (flat slots are limited
to returns, below); the engine then declines 'pending transfer' at the armed slot addresses (1.5M -> 4.0M per 3 s).

**Verdict: NO-GO as built, medium confidence.** The call reduction on the chain is as modelled (16x fewer calls per
root) and the exactness holds, but the in-situ cost of the extended kernels is 3-4x the 0.5 ns model (1.6-2.1 ns),
so the gain is about a fifth of the model, and in the all-roots fast-only configuration the net effect is negative
(compile 2x, more stops and declines). Following calls is not the blocker; kernel quality and coverage after the call are.
Estimate for general following in fast-only: calls fall from 11.5M to about 3M/s (the stop, exit and guard share, 9.5M
per 3 s); at 1.5 ns per kernel insn and 105 ns per interpreted insn that is about 2.1 DSP s per audio s against
2.37, far from the < 1M calls/s bar.

**What generalising needs (in order).** (1) Ranges for I6/I7 (and callee-saved index registers) across a return: keep
the call site's range at the continuation and guard it at the dispatch (`(I - base) in [lo, hi]` against the entry
value); expect ops per insn to fall toward the cluster kernels' 19. An unguarded experiment is not enough because
`mark_flat_pc` is process-wide and sticky. (2) A kernel-local return stack for hardware `CALL`/`RTS` (the
`Tail::Call` push and `Tail::Rts` pop replayed at an exit; `max_call_depth` and the PC stack depth checked at entry).
(3) Callees with their own DO loops under a caller loop (loop depth and `deepest_in_region`). (4) Share callee code between
kernels (direct kernel-to-kernel calls) instead of duplicating it: compile time doubled. (5) **[O]** flat accesses in
the delay slots of a Jump (not a Return) crash the DSP thread in the all-roots no-AOT run after about 7 audio s
(root 0xb80515, SIGBUS in a flat window load); flat slot marking is therefore limited to Return nodes
(`lower_cfg` error path). Find the cause before lifting it. (6) Region selection for kernels with many exits (stop
nodes) so declines at slot addresses do not appear.

## Second check: register-count N 2026-10-11 **[D]**

Verdict: sound, no bug found. Read `note_reg_count`, `loop_kind`, the pass-2 `reg_sym` refusal and `glue::run_cfg`. N is
`r[c] + k` only when the count's Abs is exact (`lo == hi`, no `ts`/`nf`) with `Base::Reg(c)`, so it is the entry value plus
a constant on every path that reaches the DO (a join of unequal values is inexact; a count changed in an outer body is
widened away and a changed source refuses the region). `iters` is evaluated in i64 and declined outside 1..=u32::MAX, so
a 32-bit wrap or a negative sum only declines (the real count would be the residue; the sign of `b` cannot make the
window smaller). Early exits and a DO on a conditional path only make the window an upper bound. Nested: a Lit outer
loop widens the inner Entry-kind terms through the same `widen`, and a stride with a `ts` term gives no stride. Regions
with entry loops or call targets never take N. The `iters = n - 1` mutant survives the old fuzz and test because a
window is a host pointer into one 64 KiB page: a window one byte short is harmless until the last access crosses the page
end. New test `fast_register_count_loop_windows_end_exactly_at_the_page_boundary` (`packages/emulator/sharc/src/tests.rs`) steps a
load pointer or a store pointer to end just before, on and one byte past the page end (n = 3, 6, 9) and reads the last
store back; it passes on the real code and fails on the mutant (R3 differs at ptr=0 n=3 d=1). **[O]** the same mutant on
the NwPlain hull (`nw_ok`) is not covered: byte forms do not take NwPlain, so a normal-word variant at a plain-range end
is still untested.

fast_diff mismatch triage 2026-10-11 **[D]**. Setup: DN2 1.11 DSP image and `snapshots/dn2-audio-ready-2026-10-03/digi-audio-loop2-fresh.bin`, both backends (cl, interp), base 6b4daf7. The failures reported as pre-existing split into two harness artefacts, one stale-expectation group and one real lowering bug; one more crash is open.
- `forms --trials 1000`: 27,724 mismatches on every float unit (2a, 2a_short, 2c, 3a load/store, 4a, 5a, 7a with fadd, fsub, fneg, fabs, fclip, fcomp, fmax, fmin, fmul, fpass, dual and multifunction float) and 612 on `2c/mul`; identical on cl and interp. All were "exit before the instruction on a normal result". Class (a), harness: since e8353a8 (denormal flush, 2026-10-09) every computational float op leaves on a nonzero denormal operand (`rd_f_op`) and a multiply leaves on a product that underflows to zero or below the smallest normal (`result_no_underflow`); `expect_special` still predicted only NaN, infinity and denormal results, and `rand_f32` makes a denormal operand about one time in 16 per operand. `2c/mul` is `RN = RN * RX (ssi)` (sharc_core `compute_multi.short_multiply`, `MULTIPLY_SSI_RN`): the kernel leaves when the product does not fit 32 bits (MV), the unit had `Check::None`. Fixed in `fast_diff.rs` (`is_denormal`, `mul_leaves`, `Check::MulSsi` for the short multiply). `forms` 5000 trials on both backends: 0 mismatches; `forms --cond` 1000: 0.
- `cfg --simple` cases 180 and 275: class (b), real lowering bug. A MODE1 write inside a DO loop body that changes MODE1 (the generator's `BIT SET MODE1 0x200000` or a random mask sets PEYEN; `gen_prims::g_wr_mode1` lowers such writes by tracking `mode1_cur`) was lowered once under the MODE1 of the loop head, and `flow.rs::loop_back` branched back to the head without comparing it (`Joins::edge` checks only jumps and fall-through). From the second iteration the interpreter runs the body in SIMD (case 275: ASTATY `0x401` in the interpreter, `0` in the kernel; case 180: memory bytes written by a PEy half). The kernel is refused now when a loop body leaves MODE1 different from the loop head (`Joins::back_edge_mode1`, "MODE1 differs where paths join"); regression test `fast_mode1_write_in_a_loop_body_is_not_carried_to_the_next_iteration` (fails without the check, S3 differs after 11 steps; also covers a body with a no-op MODE1 write that still runs). Present since 0c88ae5 (2026-10-07: `g_wr_mode1` and the `Joins` MODE1 check arrived together without a loop-edge check), not from the register-count loop change. Reachable in firmware only by a DO loop whose body changes MODE1 (PEYEN, ALUSAT, TRUNCATE, RND32, CBUFEN); the DN2 pins and tier sweep do not contain one.
- `cfg --check-windows` ("window access at 0x128 outside every window with this base", case 19 of the default seed, regions with 0 windows): class (a), checker artefact in `InterpKernel::check_window`. The flat-access lookup reads the page table (`CTX_FLAT_EFF`) and the code-watch bytes (`CTX_FLAT_WATCH`) through their own pointers, which are neither a window nor an entry of the table. Fixed: those two bases pass. `cfg --check-windows` 300 cases, with `--shape cfg`, with `--stride --regloop --pushcall --shape cfg`: 0 mismatches. `cfg --simple` 300 cases (cl and interp), `cfg --simple --circ --stride` 200, `cfg --simple --stride --seed 11` and `--seed 5 --shape cfg`: 0 mismatches except the crash below.
- **[O]** `fast_diff cfg --simple --seed 11 --stride --case 170` (cl and interp, also with the tier armed only at pc 0) crashes with SIGSEGV in `InterpKernel::exec`: a `Load32` whose base pointer is 0 and whose offset is `0x97daf` (a variable, `Var(30)`), in a region with static windows w0 and w2 at one page. It does not depend on the MODE1 fix (same crash with the check disabled). Possibly the same family as the flat-window SIGBUS in item (5) above. Not diagnosed; case 170 is the reproducer (13 instructions, a 7d, 3a, 4a and a 7a with a shifter immediate).

## Kernels that follow calls: ranges across calls 2026-10-11 **[D]**

Question: if I6/I7 (and the other registers the callee writes) kept their ranges at the continuation after a followed
call, would the 0x1c94f7 kernel drop from 2.08 to about 0.8 ns/insn and make the follow-calls tier pay? Lane K4, base
6b4daf7 plus K1 (d84aa8a) merged. Same chain and scenario as above (DN2 1.11 `trig1-taps` 10.5 s, PCM `e0e6c8af7a84c443`
in every run listed). Not re-checked by a second agent.

**Merge.** K1 and K2 both rewrote `form_16a` and the STKYX bit 26 bookkeeping. Kept: K2's `dm_transfer` path (guarded
dynamic window anchored on the entry value) and its bit 26 test (`CTX_STK26` 1, or 2 with the stack model), plus K1's
"loop end with STKYX not fully known gives Unknown" case. K2's condition that sent `16a` through the old static-window
path in a region with call targets before their callers is gone: with it, 0x1c94f7 stops at its first push ("address
range unknown") and runs 2 insns per call instead of 15,705. Tests `fast_kernel_follows_calls_and_returns`,
`fast_calls_and_returns_match_the_interpreter`, `fast_register_count_loop_with_stepped_pointers_matches` and
`fast_call_slot_push_through_an_unranged_stack_pointer_matches` pass.

**What was tried.**
1. Call-site range kept at the continuation, with a run-time guard at the return dispatch (I6/I7 equal to the values the
   CJUMP read, flag and two saved values per call site; ranges for the next pass in `Plan`; loop heads ignore the writes
   of guarded call sequences; a back edge whose I6/I7 range is not inside the head's refuses). I6 gets
   `Reg(22)+[0,0]` at the first three call sites of 0x1c94f7, **I7 never has a range there**: the root's loop
   (0x1c94f7 .. 0x1c9505) reads through the stack pointer with post-modify (`7a`/`3a` write I7 at 0x1c94f7 and
   0x1c94fa, so I7 steps every iteration) and three other call sites have no I6 range either. The first-pass learning had to be made optimistic (pass 0
   cannot bake modifiers, so it never sees an I7 range) and `improve` (more passes, not an error) added to
   `region.rs`. With the back-edge check the root is refused at its third call (I6 not kept over the loop back edge), so
   the kernel shrinks to 17 insns per run. Dropped; the patch is not kept in the tree.
2. Anchor hint only: at the continuation, `hint[I6] = I6`, `hint[I7] = I7` (the entry values of the registers RFRAME
   wrote), so the accesses after the call become guarded dynamic windows instead of flat ones. This is sound (hints
   only choose a window, each access is checked) and is the cheap form of the idea. It changes the first two builds
   (flat accesses per kernel 89 -> 41 and 174 -> 121) and nothing in the final one: **the final kernel is the rebuild
   after `dynflat`** (`SHARC_FAST_DYNFLAT`, on by default): at least half the calls leave at a pc that has a guarded
   dynamic window, so every such pc is rebuilt flat. With `DYNFLAT=0` the hint kernel exits after 22 insns per run
   (240,000 runs against 8,192), so the guessed anchor is wrong at run time. Dropped.

**Measurements (same binary, hint on/off; exclusive lock, loaded machine so absolute times are inflated).**

| | K1 merged (no hint) | hint (anchors of I6/I7 restored) |
|---|---|---|
| 0x1c94f7 in situ, ns/insn (`SHARC_FAST_KTIME`) | 2.069, 2.070 | 2.062, 2.059 |
| 0x1c94f7 insns per call | 15,705 | 15,626 |
| final kernel: ops / flat accesses | 20,637 / 206 | 20,613 / 206 |
| chain, DSP CPU per audio s (incl. 0.6 s compile) | 1.216, 1.212 | 1.201, 1.193 |
| no-AOT `static-insns+follow`, run time (CPU - compile - build) | 3.04 | 3.02 |
| no-AOT region calls per audio s | 8.56M | 8.30M |

K1's figures were 2.08 ns/insn, 3.03 and 8.7M; the baseline (no follow) is 2.37 and 11.5M. The 1% chain difference is
within the +-0.015 spread.

**Why the idea does not reach 0.8 ns/insn.** The flat accesses are not there because I6/I7 lost a proven range: I7 had
none to lose (it steps in the loop around the call), and the dynamic windows that would replace the flat accesses fail at
run time and are flattened by `dynflat` anyway. Also `SHARC_FAST_WINFAIL` shows a second cost in this root: a static
window `Reg(25)+[0x0, 0x13919]` (I9, 80 KB) that is not resident, so the entry declines it. What would be needed is a
cheaper flat access (the per-access page-table walk is the difference between ~19 ops/insn in cluster kernels and ~50
here, not measured per op) or keeping the frame in one resolved span
across the call; neither is a range question.

**Verdict: NO-GO, medium confidence.** 0x1c94f7 stays at 2.06 ns/insn (needs <= 0.8) and the no-AOT run time at 3.02
(needs < 2.37). PCM `e0e6c8af7a84c443` held in every chain and no-AOT run above; the exactness gates hold, with the code that is committed (K1 merged, `16a` always through
`dm_transfer`). Open: the Jump-slot SIGBUS (root 0xb80515) was not chased.

## DT2 PGO 2026-10-11 **[D]**

Method. DT2 1.16 (`sections/.source-sha256` = 278541e4...), core `out/native/dt2-audio/gen-lane-entry` (the entry-search
core; `gen` is not it and is unchanged), headless `coupled-run` on the one-sample card, default config (AOT, idle skip,
interpreter; no DT2 fast regions). One machine, load average 2.2 to 3.2 (other lanes running), exclusive alternating pairs,
6 per cell (`tools/devtools/coupled_ab.py --dt2-card --device dt2`). Not re-checked by a second agent.

**Workload: instrumented `coupled-run`, not the desktop app.** The app's `--pgo-train` runs the `coupled_ready_exactness`
gate, which restores the DN2 ready snapshot and pins a DN2 PCM; DT2 would need a second gate, its own fixtures and an app
mode to select them. `coupled-run` already runs DT2 on the card's ready state, so `tools/emulator/native_pgo.py train --device dt2`
builds it with `-Cprofile-generate` (target dir variant `pgo-dt2-instrument`), runs `dt2-sample-trig` (3 s), `tf-br-127`
(2 s), `sf-del-ppwidth-u50`, `sf-rev-decay-u50` and `sf-mst-thr0` (4 s each; delay send, reverb send, master compressor)
three times (42 s per pass, instrumented), and merges. Profile (8.2 MB):
`out/native/pgo/dt2/aarch64-apple-darwin/profiles/48c76e4edd336a0c35fa908fd1516f1055e00f5416ca4f2acacb91997f163dca.profdata`
(manifest next to it; key `f77b62da`). The profile applies to `coupled-run` (the crate hashes of `sharc_native`, `sharc_gen`,
`sharc_rt` and `elektron_native_boot` match, and they are the same in a worktree and the main tree, so a main-tree build
uses it). It does not train the desktop app's DT2 build (the app's `elektron_native_boot` differs, as for DN2). New options:
`native_pgo.py train|check --device dt2`, `native_pgo.py coupled-build --device dt2 [--profile PATH|current] [--variant V]`
(plain or profile build, prints the crate-hash check), `dt2_sample_check.py --binary PATH`. The DN2 paths are unchanged
(`plan` and `check` output identical before and after).

| DSP CPU per audio s (median of 6, IQR) | no PGO | DT2 profile | paired ratio | CF CPU per audio s (no PGO / PGO) | real-time factor (no PGO / PGO) |
|---|---:|---:|---:|---:|---:|
| dt2-sample-trig (3 s) | 1.150 (0.024) | **0.938** (0.010) | **0.814** (0.804 to 0.840) | 0.482 / 0.444 | 0.866 / 1.060 |
| tf-br-127 (2 s, bit reduction) | 1.289 (0.037) | **1.051** (0.039) | **0.813** (0.808 to 0.818) | 0.620 / 0.546 | 0.724 / 0.896 |

Cross-device: the DN2 profile (`a76e820c...`, stale for the current sources) applied to the DT2 build (only
`elektron_native_boot` mismatches): dt2-sample-trig 1.148 -> 1.116 (ratio 0.973, 0.949 to 1.013), tf-br-127 1.279 -> 1.225
(0.958, 0.940 to 0.976). Worth 3% to 4%, through shared engine code only, so DT2 needs its own profile.

Exactness. PCM hash equal in every run of each scenario: `dt2-sample-trig` 999dd10944ce7e62 (plain, DT2 profile, DN2
profile) and `tf-br-127` 18ea948d77b551da; interpreted instruction counts equal (5,926,406 and 5,682,912).
`dt2_sample_check.py --seconds 3.0` with the plain and the DT2-profile binary, and the DT2-profile binary with the default
fast tier: clean tone (peak share 0.99995) and WAV hash `4bab8330915f0929`.

What dominates at 0.94 to 1.05. Interpreted code is 5.7M to 5.9M of about 950M instructions per run (0.6%), so the cost is
the AOT blocks plus the ColdFire thread, which runs in parallel (0.44 to 0.55 per audio s). The trig is under 1.0 and runs
in real time (1.06); a bit-reduction scene does not (DSP 1.05, real-time factor 0.90). A DT2 fast-region list
(`tools/emulator/fast_regions.json` has only the DN2 1.11 entry) is not cheap: it needs a region profile of the DT2 DSP, a list per
firmware hash and an AOT = fast = no-AOT gate over the 60 FX scenarios; on DN2 the list is worth 19% over AOT without PGO.
Not tried **[O]**. A promoted `gen` or a later entry round changes `sharc_gen`, so the profile needs a retrain
(`check --device dt2` reports stale).

## Launcher: per-device PGO 2026-10-11 **[D]**

The launcher now picks the profile by the firmware's device. `native_emu.device_for_args` maps `--audio-profile-syx`
(else the default firmware) to `dn2` or `dt2` through `digi_env.device_for_syx`; `native_pgo.pgo_state`, `current_profile`,
`check_state`, `launch_plan`, `training_blocker` and the notices take that device. DT2 reads
`out/native/pgo/dt2/<triple>/manifest.json` (key as in `train --device dt2`, workload name included); DN2 is unchanged.
Layout unchanged: the app binary is still `out/native/target/<gen-key>/pgo-use/<triple>/release/digiemu-desktop`, and the
gen key already differs per device (a DT2 gen gives `g-<hash>`, the DN2 gen is `prod`), so the two binaries never share a
target dir. No merge of profiles: a merged DN2 plus DT2 profile would not help, because each binary is built from one gen
and `sharc_gen` has a different crate hash per gen, so each profile only matches its own device's binary.

Coverage, measured on a DT2 desktop build (`SHARC_GEN_DIR=gen-lane-entry`, the gen the profile was trained with): the profile
matches `sharc_native`, `sharc_gen` and `sharc_rt` (same crate hashes as the `coupled-run` build) and does not match the
app's `elektron_native_boot` (as for the coupled-run profile in the section above). The DSP engine and the AOT blocks are in the
matching crates, so the app's DSP cost should follow the 0.94 (trig) and 1.05 (bit reduction) measured on `coupled-run` with
the same profile; the ColdFire thread (0.44 to 0.55 per audio s, parallel) is not profile-optimised in the app. Not measured on
the app itself: it has no headless DT2 workload **[O]**. `native_pgo.py verify --device dt2` ignores
`elektron_native_boot` (`DT2_APP_UNPROFILED`) and prints `ok` for this build; the launcher passes `--device` and the built
binary to it, and prints the DT2 retrain command (`native_pgo.py train --device dt2`) when a DT2 profile is missing, stale
or mismatched.

The registered gen of the DT2 image (`out/native/gen-index.json` -> `dt2-audio/gen`) is not the gen the profile was trained
with (`gen-lane-entry`), so a launch with the registered gen reports the profile stale (`changed: generated`) and uses it
anyway with the warning; the `sharc_gen` hash then decides how much applies. Retrain after promoting a gen.

New: `DIGI_EMU_PGO_VARIANT=NAME` builds the launcher's profile-use binary in target variant NAME instead of `pgo-use`
(scratch builds, measurements); `native_pgo.py plan|verify|check --device dt2`.

## App soak: latency and DT2 2026-10-11 **[D]**

Unattended app runs (`tools/devtools/app_soak.py`: Trig 1 every 2 s, TRIG/SRC/FLTR/AMP/FX page changes every 5 s, frame
revisions confirm the presses land), paced controller, spike log over the whole app life including boot.

| run | prefill | DSP load overall / p99 window / max | queue min | underruns |
|---|---:|---:|---:|---:|
| DN2, DN2 profile (stale: sources changed) | 0.3 s | 0.926 / 1.004 / 1.086 | 99 ms | 0 |
| DN2 (page codes partly guessed) | 0.15 s | 0.915 / 1.033 / 1.109 | 69 ms | 0 |
| DT2 1.16, DT2 profile, `gen-lane-entry` | 1 s | 0.953 / 1.048 / 1.092 | 109 ms | 0 |
| DT2 1.16, DT2 profile, `gen-lane-entry` | 0.3 s | 0.958 / 1.059 / 1.099 | 113 ms | 0 |

- Every behind-real-time window is DSP-caused; the 1 s and 0.3 s buffers absorb the over-budget windows. The
  launcher default prefill is now 0.3 s (was 1 s).
- DT2 has no fast-tier regions (`fast_present: false`), which is why its load is above DN2's; a DT2 region list is
  the next DT2 lever. One machine, one run per row, not re-checked by a second agent.

## HLE: first function and checker 2026-10-12 **[D]**

Hand-written native replacement of a hot DSP function ("HLE"), with an exactness checker. **Verdict for the first
function: no gain, killed.** The mechanism and the checker work and are reusable; the cost model that chose the
function was wrong.

**Mechanism** (`packages/emulator/sharc/src/hle.rs`, `hle/dn2_b82342.rs`; off unless `SHARC_HLE=1` or `SHARC_HLE=name,..`). A table
maps an entry PC to a Rust routine. `Engine::step` tries it where it tries the fast tier (same `block_plan` gates:
models, latent interrupt, timer limit, `stop_loop_empty`), before the fast tier and the AOT block; entry PCs are in
the chain-stop set. Guards: SHA-256 of the function's code words (re-run when a watched code page is stored to), then
the state the routine was written for (all used registers known, MODE1 without PEYEN/BDCST, M6=1, M7=-1, no circular
buffer on I2..I5, I6's buffer covers every address used, plain-RAM windows from `Mem::window`, `icount + insns <=
s.limit`), then the value domain (every float operand and result a finite normal or a zero, no zero product of two
nonzero operands; anything else declines). A decline leaves the state untouched: stores go to the window with an undo
log, the loop bookkeeping runs in one `begin`/`commit_host`/`rollback` unit. On success it writes the registers
(scratch ones too), ASTATX by the last writer of each flag group (`glue::apply_flag`), LCNTR, the DO loop's push/pop
bookkeeping (as `glue::sites` does it), PC, `icount`, `steps`. The call is atomic: no interrupt is taken inside it.

**Function.** DN2 0xb82342 (byte-exact in DT2 1.16 at 0x1cdbb2; the table is keyed to the DN2 PC): 17-insn DO loop
(N-2 trips, N the stack argument) with spill prologue and `JUMP (M14,I12)` + RFRAME return; 98 + 17 (N-2) insns
(608 at N=32), 24k calls per audio s. Gotchas found by the checker: low addresses are keyed by their short-word alias
in the interpreter's write probe; DO UNTIL LCE loads LCNTR with the count (invisible while the count never changes);
the core's state functions journal register writes, so calling them without `begin`/`commit_host` overflows the
journal every ~5 calls and the failure shows as a loop-depth leak and a guest halt in the context switch
(`_restore_packed_loop`) far from the cause; I6 is a 994-word circular buffer (CBUFEN set), so the routine checks that
its addresses stay inside it.

**Checker** (`SHARC_HLE_CHECK=N,V`, `tools/hle_check.py`, `mise run hle-check`). In a coupled run, at each of the first N
calls (and 1 in 1000 after): snapshot the machine, run the routine, snapshot, roll back, run the interpreter (generated
core, one instruction at a time, with the write probe) to the routine's return PC, compare every register,
the stacks, loop slots, flags, MMRs, PC, `icount`, `steps` and every stored byte, roll back; V further cases per
call mutate the record, streams and trip count (zeros, denormals, NaN, infinity, extremes, random, N in 0..200). The
run itself is unchanged. Results with the final routine: **252,000 captured calls (the whole trig1-taps scenario): 0
mismatches; 110,160 cases (12,240 captured + 97,920 synthetic), 84,856 run by both, 25,304 declined on the value
domain: 0 mismatches.** With `SHARC_HLE=1`: `verify.py --only build,pins,check` passes (7 PCM pins, 61 oracles), and
the DT2 gates (`dt2-tone`, `card`, `SHARC_GEN_DIR=.../dt2-audio/gen-fext`) pass. Open: the synthetic cases decline
on exceptional values, so the denormal/underflow edge is covered by "declines", not by comparing flags **[O]**.

**Measured (trig1-taps, DN2, `coupled_ab.py`, 8 pairs, PCM identical `e0e6c8af7a84c443`).** A (no HLE) 1.112, B
(HLE) 1.115 DSP CPU per audio s, B/A 1.003 (0.998 to 1.009): no saving. Model: 24k x (1.7 us AOT - 0.3 us) = 34 ms
= 3.5 %. In situ the routine costs 0.72 us per call (`SHARC_HLE_TIME`/`SHARC_HLE_PROF`, 1 ns counter ticks, ~40 ns of
it the timing itself): windows 51, compute 568, commit 48. The first version cost 0.83 us; cheaper checks (flag
accumulation, one test before commit) and unchecked window accesses took off 0.12 us. The old path costs about
0.65 us per call: its loop is the fast-tier region 0xb823db (Cranelift kernel) and the rest is AOT, ~1.1 ns/insn, not
the 2.85 ns/insn average the model used.

**Why the model failed.** (1) The baseline per call was 2.6x too high: the function was already partly native.
(2) The routine is 530 ns of float work with a dependent chain (~24 cycles per trip, ~0.2 us floor), and the
value-domain test costs as many uops as the arithmetic (6 per result, ~110 per trip). Floor with these checks ~0.5
us; without them the exact flags and sticky bits cannot be reproduced. (3) Hook and exit state are small: ~0.1 us
(guard, windows, commit). Even a free routine would save 0.65 us x 24k = 16 ms/s = 1.4 %.

**Recipe for the next function.** Pick by measured per-call cost of the current path, not insns x average:
(a) skip anything inside a fast-tier region or kernel (`fast_regions`); (b) prefer integer/memory-bound routines
(copies, clears, table walks, bit work) where native code beats 1 ns/insn by 3x or more, over float chains;
(c) estimate the ceiling first: instructions x measured ns/insn of the current tier vs the dependency-chain floor;
(d) write the routine against `hle::Win`/`Hx` (windows with undo log, `state_ok`, `astatx`, `run_do_loop`), decline
on every state or value it does not model; (e) run `mise run hle-check` until 0 mismatches over >= 10,000 compared
cases, then `SHARC_HLE=1 verify.py --only build,pins,check`, then one paired A/B.

HLE mechanism removed 2026-10-12 (closed experiment).

## DT2 hot functions and the per-instruction floor 2026-10-12 **[D]**

DT2 1.16, gen-fext core, region-profile build, one-sample card, dt2-sample-trig + tf-br-127 + sf-del-ref (shares
agree within 0.2 points; FX paths barely register). DT2 has no fast-tier regions, so ~95% of DSP instructions run
as AOT blocks (interpreter ~0.2%). Top 20 by AOT region time:

| # | DT2 fn | share | ins/call | calls/s | DN2 match |
|---|---|---:|---:|---:|---|
| 1 | 0x1c642a | 13.2% | 3540 | 13k | none |
| 2 | 0x1c0d68 | 9.0% | 76 | 297k | reloc 1c0c76 |
| 3 | 0x1c4afe | 8.4% | 457 | 51k | none |
| 4 | 0x1cb3d8 | 7.1% | 526 | 51k | reloc 1cdb56 |
| 5 | 0x1cd906 | 7.0% | 394 | 48k | reloc b82096 |
| 6 | 0x1cbc44 | 4.6% | 310 | 46k | reloc b80432 |
| 7 | 0x1c18a6 | 4.5% | 3414 | 4k | none |
| 8 | 0x1c0874 | 4.1% | 25 | 582k | reloc 1c07c6 |
| 9 | 0x1c2b24 | 3.7% | 1061 | 14k | none |
| 10 | 0xb8063e | 3.4% | 1224 | 9k | reloc b82d1a |

Top 5 / 10 / 20 hold 44.7 / 65.1 / 87.8% (under 40 functions cover ~99%). 50.6% of DT2 time is in functions with
a DN2 counterpart (8.0% byte-exact), so per-firmware work is unavoidable; DN2 b82342 (byte-exact at DT2 0x1cdbb2)
is not entered in these DT2 scenarios.

Lesson from "HLE: first function and checker": the cost model's "2.85 ns per instruction" is the AOT average; code
already in a fast-tier kernel runs at ~1.1 ns, and a hand-written, bit-exact native routine costs ~0.9 ns per
replaced instruction (SHARC float semantics: value-domain checks, flush and flags) — no better than the fast tier.
So: (1) choose targets by the measured per-call cost of their current path, never instructions × average; (2) the
lever is moving AOT-path work (2.85 ns native, ~5.4 ns in wasm) onto the ~1 ns paths (fast-tier kernels or HLE),
which matters most on DT2, where nothing runs in the fast tier; (3) the DSP executes ~500M instructions per audio s,
so ~1 ns per instruction is a floor of ~0.5 s per audio s native unless the instruction count itself drops.

### DT2 fast-region probe 2026-10-12 **[D]**

DT2 1.16, gen-fext core, regprof `coupled-run` for every arm, dt2-sample-trig on the card (3 audio s; 1.158 DSP CPU
per audio s without fast regions, no PGO), 4-5 alternating exclusive pairs, PCM `999dd10944ce7e62` in every run.
Candidates: the hottest AOT region entries in the top functions. They are call continuations, not DO-loop heads
(only 0x1cc481 is a loop header), unlike DN2's shipped 11 regions.

| fast regions | DSP CPU per audio s | paired ratio |
|---|---:|---|
| none | 1.158 | 1.000 |
| 25 PCs (6% of instructions in kernels, 870k region calls/s) | 1.479 | 1.278 |
| 8 PCs (kernels under 30 insns per call dropped) | 1.423 | 1.231 |
| 3 PCs: 0x1cb407, 0x1ccbd8, 0x1cbce1 | 1.147 | 0.986 |

- Most kernels stop within 2-40 instructions at a return, call or edge (DT2's hot code is call-heavy): 0x1c642a
  (13.2% of time) runs 5-17 kernel instructions per call against 6,383-6,969 on the AOT path; 0x1c4afe 3 vs 457.
  Refusals: unknown address ranges (3a/3b/3c), form 14d, MODE1 bank swap, MR moves, multifunction min/max.
- At 160-300 ns per region call a kernel needs well over 100 saved instructions per call; the three good kernels'
  AOT path already ran at ~2.3 ns per instruction, so they gained 1.4% against ~6.7% modelled. Not shipped. Reaching
  the ~0.6 ceiling needs kernels that span calls (cluster roots like DN2's `blocktrans`, or call following) and the
  lowering gaps closed — the same blockers as "Kernels that follow calls".


## HLE: silent voice fast-forward 2026-10-12 **[D]**

**Verdict: not built; the premise is false.** An inactive DN2 voice (0x1c54c7, 10411 instructions per call, 16 calls per
frame) cannot be skipped: it still computes and stores real signal, and its hot loops are already fast-tier kernels.

Trace of one idle call (`SHARC_PCDUMP=all` with `SHARC_PCDUMP_ICOUNT`, new `all` mode in `pc_dump.rs`; scenario i1,
icount 600030698..600041108, no fast tier/AOT, 10411 rows). Path: prologue and parameter setup (87 insns), the four-slot
loop at 0x1c5619 (4 trips, about 26 insns), then the oscillator trio: 0x1c35a7 (loop 0x1c364f, 64 x 31), 0x1c3911 (loop
0x1c399a, 64 x 25), 0x1c37b4 (loop 0x1c3862, 64 x 36), 0x1c3911 again, then the output stage (loop 0x1c5765, 64 x 19)
and the return at 0x1c57d1. The rest of the 10411 is the caller's per-voice work (0x1c962e, 0xb82728 loop 32 x 26).
The five 64-trip loops are 8.7k of the 10.4k instructions.

- The loops are not idle arithmetic. Loop 0x1c364f writes 64 samples to the scratch streams at 0x25e2e8 and 0x25e6e8 (for
  example 0xbf0e0f00, 0xbf0e2bac, ...: a smooth ramp of -0.555 to -0.578), i.e. a table-interpolated sine at 8.18 Hz,
  and the later loops read and filter these streams. The "16 changed words" seen in the voice record and the 0x24ebb0
  block are only the persistent state; the checker compares all memory, so the 64-sample stream stores must be
  reproduced bit for bit. No closed form exists (table lookup, linear interpolation, float rounding per sample).
- All four loop heads (0x1c399a, 0x1c3862, 0x1c364f, 0x1c5765) are already in `tools/emulator/fast_regions.json`
  (`fast_regions`) and run as Cranelift kernels, with 0x1c369b and 0x1c5796 as `blocktrans` roots. So the work an HLE
  could not avoid is the work that already runs at kernel speed. From "HLE: first function and checker", a
  hand-written exact routine costs about 0.9 ns per replaced instruction (value-domain checks, flags), no better than a
  kernel. What remains for an HLE to win is the AOT glue (about 2.5k instructions per call, about 7.6% of DSP CPU at
  2.85 ns), and that glue contains the loop entries, setup and register restores that must be exact too.
- Rough ceiling: loops at kernel speed cannot gain; glue gain at most about 7.6% of DSP CPU on 16 voices, well below the
  model's 19% and the stop line (70% of the model, 13% on trig1-taps). Not built, no checker or A/B run.

What DT2 would need: the same analysis per voice function (DT2 has no HLE entry; its hot code is call-heavy and runs
in AOT, see "DT2 fast-region probe"). A silent-voice skip is possible only where the inactive path stores nothing
that is read later. Check that first with the write-watch (`SHARC_WWATCH`) over one inactive call: any stream store
that a later stage reads kills the skip.

## Busy-wait loops 2026-10-12 **[D]**

Interpreter-only per-PC profile of DN2 trig1-taps (10.5 s) mapped onto sharcdb loops, blocks and memory accesses;
a cf-profile build for the ColdFire (DN2 trig1-taps and DT2 dt2-sample-trig).

- DSP: the idle skip already covers the waiting. The idle loop's own PCs still execute 1.86M instructions per run
  (0.035% of interpreted work). The 12 hottest loops are DO compute loops that store; the only no-store backward
  branches are epilogues. Ceiling: 0.
- ColdFire, per audio s: 132M instructions, 50.3M idle-skipped, 81.5M in run_fast (0.577 s), of which the 5 fused
  loops (MacA 0x400db160, interleave 0x400cf918, MacB 0x400db1f4, copy 0x401344a6, clear 0x40133ec8) are 44M at
  0.118 s and ~37M unfused instructions cost ~12 ns each; ~124k run_fast calls per s. No MMIO polling loops in
  either firmware. Ceiling: <= 0.03 CPU s per audio s. The ColdFire levers are unfused interpreter speed and the
  run_fast call overhead.

## DN2 hot functions 2026-10-12 **[D]**

Per-function cost from the interpreter per-PC profile, fast-tier instructions at their measured ~0.98 ns and the
rest at the AOT 2.85 ns (scaled to the 0.93 PGO figure). Top 20 = 74.6% of desktop time, 84% of instructions; top
10 = 51% / 62%. Largest: 0x1c8ef1 voice/track mixer driver 7.4%; 0x1c244d buffer clear + pair FMULs 6.5%; 0x1c0c76
exp/mapping primitive 5.8% (shared with DT2 0x1c0d68); the oscillator trio 0x1c3911 / 0x1c35a7 / 0x1c37b4 (64-trip
loops, FEXT phase, linear interpolation; 15.5% desktop, ~37% browser; already fast-tier kernels); 0xb809f2 small
IIR, 960k calls per s (shared with DT2). 8 of the 20 exist in DT2 1.16 by function hash (5 byte-exact). The
hand-written ceilings first estimated from this table assumed 0.2-0.5 ns per replaced instruction; "HLE: first
function and checker" measured ~0.9 ns, so they do not hold.

## Silent voices (DN2) 2026-10-12 **[D]**

| scenario | voices playing | DSP non-idle instructions per audio s |
|---|---:|---:|
| idle | 0 | 501.7M |
| trig1-hold | 1 | 502.0M |
| kb-chord16 | 13 of 16 | 502.5M |

- The voice function 0x1c54c7 runs 16 times per frame (records 0x2412a0 + k*0x240) at exactly 10411 instructions,
  active or not: 49.8% of DSP instructions. Its path does not depend on activity; an inactive voice's oscillators
  free-run (an 8.18 Hz table-interpolated sine).
- Liveness over 3 frames of idle voices (a derived access log from `SHARC_PCDUMP=all` plus sharcdb operand decode;
  no engine read watch): 6 of the 7 scratch streams at 0x25e1e8..0x25e9e4 are overwritten by the next voice before
  any read (dead); the stream 0x25e5e8..0x25e6e4 is read by voice k+1 before it writes it (voice 0 reads voice 15's
  from the previous frame); about 25 record words and the 12-word output block at 0x24ebb0 + 0x78*k are read by the
  same voice's next-frame call (carried state, not a mixer input); the tail words 0x2d1523-0x2d153c and
  0x2d1596-0x2d15b0 are rewritten by every voice and only voice 15's values are read after the loop.
- An exact skip of a silent voice would still have to reproduce its oscillator state recurrences and the carried
  64-word stream, so it keeps a large part of the cost and needs per-voice hand-written recurrences; not pursued.
  Open **[O]**: a capture with an actively playing voice, the PCM bus path (outside the logged range), what note-on
  resets.

## Performance: where it stands 2026-10-12 **[D]**

- Desktop: real time on both devices (DSP CPU per audio s with PGO: DN2 ~0.93, DT2 ~0.95); the paced app holds the
  0.3 s prefill with 0 underruns ("App soak: latency and DT2").
- Browser: 0.47x real time on the DN2 dev page (AOT core in wasm, fast tier, split workers), DSP-bound at ~2.16 s
  per audio s.
- Measured and closed (sections above): linker order file; LLVM hot-cold splitting; fast-over-AOT regions on DN2
  and DT2; kernels that follow calls (K1) and keep ranges across calls (K4); hand-written functions (HLE, 0xb82342);
  busy-wait skipping; silent-voice skipping.
- Floor: exact emulation costs ~1 ns per SHARC instruction whether by fast-tier kernel or hand-written code, and the
  DSP executes ~500M instructions per audio s, so ~0.5 s per audio s native unless the instruction count drops.
- Remaining levers, all large and uncertain: kernels that span calls with cheap memory access (the flat and dynamic
  window problem), a different AOT code shape for wasm (the 1.74x wasm penalty; only build flags were tried),
  frame-level skipping of idle voices with state recurrences.
- Method for any next attempt: a ceiling from the measured per-call cost of the current path (never instructions x
  an average), a one-function or one-region proof (`mise run hle-check`, the region bench), and a stop when the
  measured gain is under 70% of the model.

## Browser: where the wasm penalty is 2026-10-12 **[D]**

- **[D]** The page arms the fast tier `desktop` by default when tools/emulator/fast_regions.json has an entry for the firmware
  (DN2 1.11 only; a DT2 image arms nothing). `?fast=off` turns it off. DN2 in headless Chromium, 3 runs of 10 s: DSP
  worker 2.07-2.21 s per audio s and 0.471 audio s per wall s by default, against 2.45-2.48 and 0.411 with
  `?fast=off`.
- **[D]** Region micro-benchmark under wasm: `tools/devtools/sharc_regionbench.py bench --agg --engine node|bun` runs the same
  AOT region entries from the same captured states, built for wasm32-wasip1 and run by tools/devtools/wasm_regionbench.mjs.
  Instruction counts and exit PCs are equal on native, node and bun. `ratio A B` prints the per-region table.
- **[D]** Production DN2 core, 24 regions (all 51 captured states), 16000 calls and 7 reps. For the 8 regions of at
  least 1 µs native: node (V8 13.6) is 1.57x native and bun (JavaScriptCore) 1.33x. Six default regions on the mini
  core: node 1.63x, bun 1.38x. Same wasm bytes, so most of the penalty is specific to V8.
- **[D]** V8 flag sweep (node, six regions, ratio to node default): `--liftoff-only` 1.42 (so tier-up works);
  `--no-liftoff` 0.99; `--no-wasm-bounds-checks` 0.99; `--wasm-enforce-bounds-checks` 1.04 (so the trap handler is
  on); `--no-wasm-stack-checks` 0.99; `--no-wasm-loop-unrolling` 1.00; `--no-turboshaft-wasm-load-elimination` 0.98;
  a larger inlining budget 0.99; `--no-wasm-inlining` 1.06; `--no-wasm-loop-peeling` 1.06. No flag closes the gap.
- **[D]** Where Chrome's time goes. Method: a CDP profile of the DSP worker (page default) against an Instruments CPU
  Profiler run of a no-PGO `coupled-run trig1-taps` with the `desktop` fast regions. Shares are scaled to the
  quiet-machine totals of 2.15 and 1.11 s per audio s; the machine load was 21-26. Native / Chrome, in s per audio s:
  - AOT code 0.78 / 1.65 (2.1x).
  - Dispatch and interpreter (`Engine::step`, `exec_insn`) 0.10 / 0.18.
  - Fast-tier glue 0.04 / 0.08.
  - Fast kernels 0.14 / 0.13.
  - sharc_rt helpers 0.03 / 0.04.
  - JS (sab-link `serve`, `onPcm`) 0 / 0.04.
  - Other 0.02 / 0.03.
  - Kernel crossings through `host_wasm_run` are 0.07% of the profile and GC is negligible.
- **[D]** Inside the AOT code, the 56 functions above 3 ms native run at 1.67x in Chrome, as the region bench predicts.
  The other 529 small block functions run at 2.5x and add about 0.47 s per audio s.
- **[D]** The 529 "small" block functions are region functions of 5k-43k native instructions that run rarely, not
  short code.
  - V8 emits 1.76-2.57x native's instructions for them.
  - Spills and movz/movk constants are 77% of the excess: on `b24_1C98B6`, +6.5k and +2.8k instructions. The wasm
    calling convention has no callee-saved registers, so V8 spills live values around each of the hundreds of helper
    calls per function.
  - The fixed per-entry cost is about 20-45 ns per entry, or 13-30% of the excess. It includes:
    - a 7-instruction prologue;
    - the mask check;
    - the register write-back;
    - a chain link of about 25 instructions, a `call_indirect` where native uses a tail `br`.
  - Offset folding is only 2-4% of these functions' instructions.
- Caveats: the bench restores a light state before each call. The restore loop is 47-94% of the measured time and is
  subtracted, so regions under about 1 µs are noise. Node 24 runs V8 13.6; Chrome runs a newer V8. PGO for wasm was
  not tried: cargo's crate hashes include the target triple, so a native profile would likely match no wasm function.
- **[D]** V8 machine code for `r_1C3862` (node 24, TurboFan):
  - About 2x the instructions of native, and 1.6x those of JavaScriptCore's optimizing tier (OMG), for the same wasm.
  - About 95% of CPU-state accesses become `movz` + `add` + `ldr [membase, reg]`. V8 does not fold the constant offset
    into the load, and 299 of the 646 wasm memory ops have offsets above 4095. JSC computes `membase + s` once and
    uses `ldr [reg, #imm]`.
  - V8 reloads each spilled value about 4.5 times. It reloads the memory base, the instance and `s` after each of the
    ~134 calls.
  - Chromium 150 has no disassembler in its release build. It emits code 6.6% smaller for the same function, so the
    pattern very likely holds there too.
- **[D]** Micro-benchmark: 64 read-modify-writes over a 32 KB struct, with 16 cold calls.
  - Only a `static` state at a low linear-memory address (1024, linked with `--no-stack-first --global-base=1024`)
    turns each access into one `ldr [membase, #imm]` in V8.
  - That gives -36% instructions and -17% time on node, -8% time on bun, and no change natively.
  - A pointer parameter, or a static at the default address of 1 MB, gains nothing.
  - Estimate for the real code: 8-15% of AOT time on V8 (inference). V8 still reloads the memory base and the
    instance after calls.
- **[D]** Firefox 148 against Chrome 150, paired runs on a loaded machine: the DSP worker takes 4.67-4.76 against
  3.05-3.14 s per audio s, so Firefox is about 1.5x slower. Safari is not measured yet (it needs Develop > Allow
  Remote Automation).
- **[D]** Cheap build experiments on the region bench (variant `cur`, 4 node rounds and 2 bun rounds, load 21-27, ratios
  to the baseline wasm):
  - `-C target-feature=+tail-call`: 1.04 node, 1.01 bun. 60 `return_call` are emitted, but `r_1C3862` is unchanged.
  - `--no-stack-first --global-base=1024`: 1.01, 1.02. It removes only 2% of the movz/movk instructions, because the
    engine state is heap-allocated, not static data.
  - `wasm-opt -O3`: 0.91, 0.97.
  - Instruction counts and exit PCs equal native in every variant.
- **[D]** `wasm-opt -O3` (binaryen 133) on the DN2 web core:
  - Size 45.1 to 41.6 MB, 55 s extra build time.
  - PCM equals the pin (`wasm_parity --scenario trig1-tap-release --fast desktop`) and cold start is unchanged.
  - Chrome, 3 paired runs, load 21-26: DSP worker 3.114 to 2.987 s per audio s (0.959) and audio per wall 1.042x.
  - Now the default of `mise run web:wasm`.
- **[C]** Not pursued: the SHARC state at a fixed low address (wasm only).
  - The micro-benchmark gained 17%, but in the real functions offset folding and address constants are a small share.
  - This corrects the 8-15% estimate above to about 3-6%, for about a day of generator and engine work.
- **[D]** Instruction-level profile of V8's code for `r_1C3862` and `r_1C364F`: samply 0.13.1 on node 24 with
  `--perf-prof --no-liftoff`, region bench variant `cur`, with the jitdump code bytes disassembled. Shares of samples:
  - stack spill stores 29-31%, though they are 4% of instructions;
  - stack reloads 13-21%;
  - linear-memory loads and stores 20-27%;
  - constants 2-4%;
  - calls 0%.
  Skid may move samples onto the stores that follow long-latency instructions.
- **[C]** The helpers are `#[inline(always)]`, and the register file `rf` is scalarized into about 100 wasm locals, with
  a 272-byte frame. Frame-based memory ops are at most 6% of samples. It is wrong that `&mut rf` forces `rf` into
  linear memory.
- **[D]** Sinking the per-instruction rollback snapshots (`svN`) into a volatile stack array in `r_1C3862`:
  - V8 spill stores went from 614 to 627;
  - the region got 13-23% slower on node and bun, and 53-62% slower natively.
  The spill pressure comes from the roughly 100 live `rf.r` values, not from the snapshots (docs/design/aot-mask-free.md:
  `abl-nosv` gave 0-3%).
- **[O]** Fewer live values per function, by splitting regions, is the remaining code-shape lever for V8. It costs entry
  and exit work natively and is a redesign, so it is not pursued.
- Tools: `tools/devtools/browser_bench.mjs --browser chrome|firefox|safari` (Firefox through WebDriver BiDi, Safari through
  safaridriver) and `--profile-out DIR` (raw `.cpuprofile` files, Chrome only); instruction-level V8 profiles:
  `samply record -- node --perf-prof --perf-prof-path=/tmp --no-liftoff ...` (samply pinned in mise.toml); disassemble
  the jitdump code bytes, since a `--print-wasm-code` run without `--perf-prof` compiles different code.

## Firmware-independent DSP in the browser 2026-10-12 **[D]**

- Context: the browser build is meant for GitHub Pages, where users upload and patch their own firmware. It cannot ship
  a per-firmware AOT core, so it runs the generic (shared) core with run-time translation.
- **[D]** Fast tier on the generic core, DN2 1.11, Chrome 150 (DSP worker s per audio s, interpreted share, kernels):
  - auto 256,64,1: 17.10, 13.6%, 2198 kernels.
  - 16,8,1: 9.57, 7.7%.
  - 4,2,1: 8.37, 5.8%, 2728 kernels, 260 ms compile, 2.14 s first runs, 36.9 MB of kernel wasm.
  - PCM equals the pin under node (`wasm_parity`, 883476…).
  - Native on the same core: 4,2+follow gives 5.12 at 5.78% interpreted; 2,1 gives 5.45%, the floor.
  - `Op::Helper` and `flat_auto` already work in the wasm build, so the "Helper unimplemented" note in
    docs/design/fast-compile-offload.md is stale.
- **[D]** At 4,2,1 in Chrome:
  - the fallback interpreter is about 72% of DSP time (27.4M instructions per audio s at about 220 ns);
  - kernels and their calls are about 2.0 s;
  - kernel chaining would save under 0.3%.
- **[D]** Page default: with no `?fast=` and no fast_regions.json entry, the DSP worker arms 4,2,1 when the core
  reports `digi_sharc_aot_blocks() == 0` (the generic core). An unlisted AOT core keeps the fast tier off.
- **[D]** sharc-jit revived on lane/jit (a2292d7, 4637c61; about an hour of porting to today's sharc-rt):
  - Exact on 91/91 frames of the drive3-f0 DT2 pack, against fresh interpreter hashes, in wasmtime and V8.
  - 12.56 ns per instruction in V8 and 15.05 in wasmtime. With the loop-back fix (`SHARC_JIT_LOOPSTARTS`) and
    wasm-opt: 10.08 in V8.
  - 702 regions, 33.2 MB of wasm, 0.83% of instructions interpreted.
  - Costs: 0.63 helper calls per instruction (every DM access is a cross-module call); 1.9 KB of wasm per
    instruction; about 20 ms to translate and 19-64 ms to compile per region.
- **[D]** Hybrid with sharc-jit regions as the fallback tier (lane/jit fcd0d4b): stopped at the stop rule, with no
  gain.
  - sharc-jit was exact only with the opt-in models off. With models off, translated code halted on an empty
    architectural PC stack within 14k instructions.
  - With the real model flags it cannot translate code that runs inside a DO loop (`loop_depth`, `pc_stack` ops). All
    39.2M interpreted instructions of FUN_1c8ef1 run inside one, so 0 regions were entered.
  - In-core translation costs about 2.7 ms per SHARC instruction in V8 (75.5 s for 274 regions).
  - V8 rejects 58 of the 274 regions for having more than 50k locals.
  - The wiring is inert-exact: PCM equals the pin with the tier on.
- **[O]** Making the translator model-aware, about 100x faster and region-capped is estimated at 4-6 agent-days, at
  about 35% confidence. Parked.
- **[D]** Profile of the native generic interpreter (shared core, `SHARC_AOT=0`, 4,2+follow), per interpreted
  instruction, scaled to 105 ns:
  - fetch/decode 15.6 ns (a by-value copy of the 224-byte `Insn` and a HashMap lookup);
  - generic execute 63.2 ns (of which 19.7 is the field-by-name linear scan and 25.1 the core semantics);
  - model and engine bookkeeping 23.4 ns;
  - memory 0.7 ns.
- **[D]** Fast-tier refusals behind the interpreted share at 4,2+follow:
  - no stop recorded: 32% (open);
  - address range unknown / too many windows: 17%;
  - too many delayed branches: 10% (FUN_1c8ef1 hits `MAX_CFG_JUMPS = 16`, ir.rs:243);
  - dynamic branch: 6.7%;
  - MODE1 bank model: 6.6%.
- **[D]** Faster generic interpreter (lane/fastweb d116a0d, 10497b3, b74c69a; no generated-core change):
  - Native interpreter-only on the shared core went from 110.8 to 69.8 ns per instruction. Paired coupled_ab ratios:
    interned `Fields` with perfect-hash lookup 0.69; dense two-level decode cache 0.93; inline `ww_flush` watch check
    0.97.
  - Generic web core at 4,2,1, node: 74.0/75.3 to 54.8/55.8 s for 8.5 audio s (0.74), PCM MATCH.
  - Chrome 150: 7.90/8.09 to 5.82/5.83 DSP s per audio s (0.73), which is the native gain at the interpreter's ~72%
    share.
  - DN2 verify steps pass. The DT2 steps are blocked by the stale DT2 generated core.
  - The `Fields` intern pool never frees blocks; it is bounded by the number of distinct decoded instructions.

## Why patched DSP images were slow (lane b, DT2 1.17, `tools/patchkit/pk_tiers.py`) **[D]**

DT2 has no fast-tier regions (`fast_present` false), so the tiers are AOT blocks and the interpreter (about 70 ns per
instruction against about 1 ns in AOT). Window of 1 audio s after the note started, one daemon per row, non-PGO build;
DSP CPU per audio s and DSP instructions per audio s in millions:

| image                              | CPU before | interp (of it appended code) | CPU after fix |
|------------------------------------|-----------:|-----------------------------:|--------------:|
| stock                              | 1.31       | 2.0 (0)                      | 1.31          |
| digikit/master-gain                     | 1.45       | 3.9                          | 1.46          |
| core/dsp-voice, no type            | 2.1        | 12.2 (1.1)                   | 2.0           |
| + test/sine-voice, sine ringing    | 3.9        | 13.8 (3.0)                   | 2.1           |
| PLUCK set, idle                    | 4.3        | 44.2 (1.4)                   | 4.3           |
| PLUCK set, note ringing            | 42         | 45.8 (3.3)                   | 4.5           |

- **Cause of the 42 s (fixed).** Appended module code and data sit on the 64 KiB page that also holds the last stock L2
  code (stock code ends at word `0xb88fb1`, byte `0x2001_1f62`; the module state is at `0x2001afd8` and up). The code
  watch (`Mem::watch_code`) marked whole pages, so every store of a module to its own state bumped `code_gen`, and the
  next dispatch ran `verify_code`, a SHA-256 over all 230 code ranges (about 0.6 ms). A ringing PLUCK note caused 73,500
  verifications per audio s (about 49 per 32-sample block): 100 % of the extra 38 s. The sine voice caused 3,000 (2.0 s of
  its 3.9 s). Counters: `code_verifies`, `code_verify_ns` in `dsp_tiers`. Fix: `Mem` keeps the exact watched spans
  (`code_spans`) and a store bumps `code_gen` only when it overlaps one; window writes still count per page. PCM of all
  six images is bit-identical before and after (sha256 of the window), stock scenarios keep their pins; stock cost
  +0.7 % (coupled_ab B/A 1.007 and 1.008, tf-dry-hold and tf-br-127).
- **Remaining cost (not fixed).** (a) The hook word `0x1c4ecf` is in the hashed range `0x1c4e8d..0x1c5048` and the
  `selector_table` words of core/machines in `0x1c24e9..0x1c2acf`; every region with an entry in a changed range is
  barred (`verify_code`: 6 regions for the first range, including the stock sample renderer `0x1c4f81`, the arm
  routine `0x1c4eaf` and `0x1c6b07`), so the stock voice code runs interpreted: +10 M interpreted instructions per
  audio s for the hook alone (+0.7 s), +40 M with the selector patch (+3 s). Ceiling, measured with an engine run
  that bars only the regions that hold a changed word (`0x1c4ecf`, `0x1c2607`; experiment, not committed): core/dsp-voice
  2.0 to 1.52, PLUCK ringing 4.5 to 2.76 CPU per audio s. Exact barring needs per-region code extents from the
  generator (`REGION_ENTRIES` lists entries only), that is a change of `tools/sharc_dev/sharc_rsgen.py`, so a regeneration of
  the DT2, DN2 and shared cores. (b) Appended code is interpreted (not in the AOT image): 1.1 to 3.3 M instructions per
  audio s, 0.1 to 0.25 s.
- The fast-tier kernels still decline any store into a code-watched page (`mem_addr.rs` guards on the page's watch
  byte), so on DN2 module data on a code page leaves the fast tier; placing module data on its own page would avoid it.

## Block bodies continue after MODE1 writes (2026-11-05)

- **[D]** Dispatch causes, DN2 1.12, held note (trig1-hold), 1 audio second, existing `SHARC_PROFILE_PREFIX` `.trans`
  data, classified against the 1.12 program database:
  - 3.07M engine dispatches per audio second (fast-tier defaults).
  - At least 54% (1.67M) end within 4 instructions after a MODE1 write. This is a lower bound: 44% end near
    undecoded instructions. Returns to a caller are about 0%.
  - Top stops: 0x1c07d2 and 0x1c07d7 (291k each; float-to-int helper: set TRUNCATE 0x8000, `fix`, `MODE1 = R2`);
    0x1c06c0 and 0x1c06fe (float divide 0x1c06ba: clear RND32 0x10000, `frnd`, `MODE1 = R1`); 0x1c0c9d and 0x1c0d9f
    (0x1c0c76: set 0x2000, `MODE1 = R12`). Others: PEYEN 0x200000 on and off.
- **[D]** Cause: `tools/sharc_dev/sharc_rsgen.py` ended a body after any MODE1 write (TERMINAL), so the engine could
  complete a bank request (`St::bank_complete`) and check interrupts. These writes change no bank-select bit (0x4f8),
  IRPTEN or NESTM.
- **[D]** Change (lane/sharc-mode1, commit 45ebd538): a MODE1 write that changes none of `MODE1_ENGINE_BITS`
  (0x4f8|0x800|0x1000) and is followed by an instruction of the same block continues the body. Block code does the bank
  bookkeeping of `St::commit` itself: one `St::bank_complete` after the write, one after the next instruction. A restore
  with an unknown value (`MODE1 = Rn`) continues only under a run-time guard that MODE1 equals the body's entry value;
  every guard exits with the state of the old exit. Tests: `tests/sharc/generation/test_sharc_rsgen_mode1.py` (decision
  cases; emitted code run against the native runtime for the exact bank state, the guard exit and a trap in the next
  instruction).
- **[D]** Full DN2 core: OLD (rule off) is byte-identical to the promoted gen in code. MODE1 terminal sites 324 to 67;
  `blocks_*.rs` +3.9% (57.96 MB to 60.20 MB). Exact PCM, full SHRD, instruction count and duration on held note and idle.
- **[D]** Paired A/B (`tools/devtools/sharc_matrix.py`, plain builds, no PGO, default tiers, alternating pairs of 3
  audio s). Criterion declared first: median B/A <= 0.98 in both scenarios and every pair < 1.
  - Quiet host, DN2 only: held 0.9631, idle 0.9670.
  - Later batch at load about 4: DN2 held 0.983, DN2 idle 0.996 (pairs 0.954 to 1.041), DT2 sample-trig 0.982, DT2
    retrigger 0.985.
  - Exact PCM, state, instruction count and duration on all cells.
  - Block entries per 3 s: DN2 9.14M to 7.58M, DT2 7.95M to 6.51M.
  - The gain in time is about 2 to 3.5% and is limited by host noise. The predeclared criterion passed in the first
    run and failed in the second.
  - DT2: MODE1 sites 107 to 29, `blocks_*.rs` +5.8%.
  - Artifacts of the first run: `out/native/runs/20261105-mode1-full/ab/` (`results/summary.json`, `criterion.json`).
- **[O]** About 383k MODE1 stops per audio second remain: restores whose value is not the body's entry value (for example
  0x1c06fe, 0x1c0d9f). DT2 regeneration, the shared core and PGO are not measured yet.

### MODE1 effect latency

- **[D]** The public SHARC programming reference (ADSP-2136x/2137x/214xx, Table A-11 and the text before it) gives MODE1
  one cycle of effect latency: the instruction right after a MODE1 write still uses the old mode. The public SHARC+
  programming reference keeps this through stalls for compatibility. `sharc_core` delays only the bank selects
  (`sharc_core/state.py` `bank_model`); rounding, truncation, saturation and PEYEN take effect on the next instruction.
- **[D]** A static scan of DT2 1.17 (229 MODE1 writes) and DN2 1.12 (252) found no next instruction that depends on the
  changed mode, except DN2 0x1c6d7c (ALUSAT set, then `R7 = add(R10,R10)`: differs only if that add overflows).
  `recips` and `rsqrts` right after an RND32 clear at DT2 0x1c06bd and 0x1c1287 and DN2 0x1c06bd and 0x1c1231 are seed
  approximations (inferred unaffected, not checked).
- **[O]** Whether to model the latency.

### Hardware counters and host noise

- **[D]** CPU time for the same binary spread 10 to 24% on the development laptop in normal use. The DSP worker now
  reports per-thread retired instructions and cycles (report.json `cpu.dsp_thread_insns_per_audio_s`,
  `cpu.dsp_thread_cycles_per_audio_s`; docs/INSTRUMENTATION.md).
- **[D]** A/A control, DN2 held, 3 pairs, load about 4: instructions 1.0002, 1.0000, 1.0000; cycles 1.0125, 0.9906,
  0.9974; CPU time 1.0129, 0.9855, 0.9975.
- **[D]** Instruction ratios settle a path-length question in one run. They do not show stalls or layout, so a timing
  A/B on a quiet host remains the final check. IPC is about 5.5 in every tier configuration.

### Engine work per dispatch

- **[D]** A block dispatch no longer runs the full interrupt candidate when nothing allowed is latched and enabled,
  reuses that result for the latent-interrupt check, calls the fast tier's run only for a PC it wants or when it has
  queued work, and syncs chain stops only when some are pending. Exact PCM, state and every tier counter on the four
  cells.
- **[D]** Host instructions: DN2 0.9924, DT2 0.9895. Cycles in 3 pairs: DN2 held median 0.980, idle 0.978, DT2
  sample-trig 1.021, DT2 retrigger 1.012.
- **[O]** The DT2 cycle rise against fewer instructions, and an earlier timing run of the same source change at 0.955,
  suggest a code-layout effect of several percent between builds; not separated.

### Host instruction cost model

- **[D]** Steady state per audio second, (6 s - 3 s)/3, plain build:

  | cell / config | host insns | cycles | guest AOT | fast | interp | idle | block entries | fast calls |
  |---|---:|---:|---:|---:|---:|---:|---:|---:|
  | DN2 held, default | 24.52G | 4.47G | 235.5M | 265.5M | 2.0M | 497.0M | 2.525M | 516k |
  | DN2 held, fast tier off | 31.71G | 6.05G | 500.4M | - | 2.5M | - | 1.728M | - |
  | DT2 sample-trig, default | 25.29G | 4.42G | 294.1M | 18.6M | 1.8M | 685.4M | 2.168M | 313k |
  | DT2 sample-trig, fast tier off | 47.42G | 8.89G | 295.2M | - | 19.4M | - | - | - |
  | DT2 sample-trig, idle skip off | 129.3G | - | 979.8M | - | - | - | 145.0M | - |
  | DN2 held, AOT off | 344.1G | - | - | 279.7M | 223.2M | - | - | - |

- **[D]** One interpreted guest instruction costs about 900 to 1500 host instructions (three measurements), so 0.2% of
  the guest work is about 8 to 12% of the host instructions.
- **[O]** AOT about 63 and the fast tier about 12 host instructions per guest instruction (estimates from profile
  shares). A dispatch costs about 550 (from the idle-skip-off row).
- **[D]** Runtime roots 0xb8251e and 0x1c201d in the DN2 1.12 block-translator list: host instructions 24.54G to
  23.85G (-2.8%), interpreted guest instructions 1.99M to 1.19M per audio second, exact PCM and state at 3 s and 6 s.

### AOT path length

- **[O]** From a hand count of older PGO disassembly (about +-20%): about 43 host instructions per guest instruction in
  hot straight-line code. Real work 10%, float flush and classify 23%, flags 22%, spills 12%, loop end 10%, dead
  Result tests 9%, memory checks 7%, other 8%. Undo copies, PC guards and budget checks fold away.
- **[O]** Candidates: lazy flags, canonical floats, memory-test layout, loop-end index width.

## Runtime patches bar hot regions on DN2 (2026-11-06)

- **[D]** Live Workbench DN2 session at 947d2765 with valid PGO: 0.48-0.57x real time, DSP CPU 1.73-2.06 s per audio s,
  13.5M interpreted guest instructions per audio s. The fixtures in out/fixtures/runtime dated from 2026-11-01 and
  reported 8 differing PCs and 33 barred entries. The current runtime reports 44 and 106; most new PCs lie in
  FUN_1c8ef1. `packages/emulator/sharc/src/code_diff.rs` bars all entries of a region with a changed block, so unchanged
  code in those regions runs in the interpreter. DT2 is unaffected: 10 PCs, 44 barred, 1.8M interpreted per audio s, and
  extra roots give no gain.
- **[D]** 58.6% of the interpreted instructions were one region, 0x1c9aea..0x1c9b30: a DO loop of about 496 iterations
  once per audio block that applies two gain ramps and writes through an offset table.
- **[D]** Fix: DN2 `fallback_regions` in tools/emulator/fast_regions.json (20 roots). The loop-body roots (0x1c9b0d,
  0x1c9b12) give nearly all of the gain; the region entry alone does not. Fresh-fixture coupled run, PGO build, steady
  state per audio s:

  | cell | host insns | cycles | interpreted |
  |---|---:|---:|---:|
  | held, before | 39.11G | 5.15G | 13.5M |
  | held, after | 25.89G | 4.34G | 3.9M |
  | idle, before | 39.24G | 7.07G | 13.5M |
  | idle, after | 25.98G | 4.42G | 3.9M |

  PCM and full state are exact. Live Workbench, single 15 s samples at load about 5: 0.57x to 0.83x, CPU 1.73 to 1.19 s
  per audio s.
- **[O]** DN2 stays above real time on a busy host; the remaining interpreted work is about 4M instructions per audio s.
- Tooling: `tools/devtools/fast_roots.py` (`rank` from a capture; `check` against `tools/emulator/fast_budgets.json` on a
  fixture built from the current runtime and cached by content key); gate `gate-fast-roots`. **[D]** Without the roots
  the guard fails at 13.4M against a 5M budget; with them it passes at 4.03M, and DT2 passes at 1.85M. Repeat runs give
  equal counts.
- The PGO training workload no longer checks pins. The gate `coupled_ready_exactness` reads its PCM expectation from
  tests/scenarios/dn2.json and no longer checks the DSP export hash, whose literal was stale since 2026-11-01 (the pre-
  and post-#291 builds give the same export).

## Multiply-accumulate in the fast tier (2026-11-06)

[D] After a soft reset DN2 runs a sample interpolation loop in FUN_1c44ae (hardware DO, about 64 iterations of 42 instructions, 1.5M AOT entries per audio second). The fast tier stopped at its first `MRF = Rx*Ry`, because kernels held no MR state. Kernels now hold MRF as three pseudo words and lower `MRF = Rx*Ry (SSF)`, `MRF = MRF + Rx*Ry (SSF)` and `Rn = SAT MRF (SF)`, SISD only; an MV result leaves the kernel. The DN2 fast root 0x1c451c then retires about 2,400 instructions per call. Plain build, DN2 post-reset, per audio second: 42.20G to 37.81G host instructions, 13.28M to 8.67M block entries, exact PCM and state. No change on DN2 first-boot or DT2.

[D] Forms 16b and 14d and `Rn = mant Fx` also lower now. On their own they gave no gain (about +0.2 to +0.5% host instructions), and a static census of what they would unblock overestimated badly. A region that stops at a PC with no entry is now refused over a healthy AOT root: this kept the root above from raising interpreted work to 26M.

[O] The next blocker is the add overflow guard `fixed_alu.rs:24` in loop 0x1c4b72 (about 27% of iterations). DN2 post-reset stays below real time.

## ALU overflow and appended entries in the fast tier (2026-11-07)

[D] FUN_1c4b54 on DN2 post-reset runs a loop of about 13 instructions, 32 iterations for each of 24,000 calls per audio second. It adds `R2 += R1; R1 += R2` and stores a sample only past a threshold. It looks like a wrapping noise generator, so about one add in four overflows by design. With a root at 0x1c4b72, the old overflow guard left the kernel about 508k times per audio second over 768k iterations, so the root gained nothing.

[D] With MODE1.ALUSAT clear and outside SIMD, a fixed add, subtract, increment, decrement or negate now wraps in the kernel. The kernel ORs each overflow word into `PSEUDO_AOS`, and `glue::store_aos` sets STKYX.AOS when the kernel returns. Such a kernel requires ALUSAT clear (`Req::Mode1`). With ALUSAT set, in SIMD, and for abs, the guard stays. Root 0x1c4b72 then runs 270 instructions per call and only leaves by its return. Its calls fall from about 520k to 24k per audio second.

[D] The workload cannot check AOS: the state hash stayed equal with the latch left out. `fast_diff` covers it: `forms` starts STKYX.AOS clear, set or unknown and builds a quarter of the instances with ALUSAT set, and `cfg` starts it clear or set. With `store_aos` disabled, `forms --filter /add` gave 1811 mismatches and `cfg` 3 differing runs.

[D] Appended code entered by a block or kernel exit never became an entry: `note_appended` ran only after an interpreted branch landing. The interpreter now also notes a PC in appended code that the previous interpreted instruction did not lead to. Interpreted appended instructions per audio second: post-reset 2.68M to 1.14M, first boot 2.20M to 1.14M, DT2 no real change. `fast_budgets.json` for DN2 reset is now 4.5M interpreted and 8M block entries.

[D] PGO builds, steady state per audio second, A is main at 32b8af05 and B is both changes. PCM and full state are exact on every state:

| state | host insns A | host insns B | B/A | interpreted A to B | block entries A to B |
|---|---:|---:|---:|---:|---:|
| DN2 post-reset | 34.03G | 31.58G | 0.928 | 4.61M to 3.09M | 8.67M to 6.38M |
| DN2 first boot | 25.19G | 24.54G | 0.974 | 3.45M to 2.41M | 3.08M to 3.09M |
| DT2 | 22.49G | 22.57G | 1.0035 | 1.80M to 1.80M | 2.17M to 2.17M |

[D] Timing, DN2 post-reset, 3 s runs including startup, three alternating pairs at load 9 to 12 (not a quiet machine): DSP CPU ratio 0.829, 0.942, 0.946, median 0.942; real time 0.62x to 0.66x in the last two pairs. The first pair ran at the highest load and is not used.

[O] DT2 costs 0.35% more host instructions under PGO (1.0028 plain, also with runs one at a time) with equal tier counters and cycles. The cause is not known.

[O] A fully unknown STKYX shows a known-mask difference in STKYX bit 26 between the tiers after a DO: the interpreter's `_push` leaves STKYX unknown, and the fast tier knows bit 26. It predates this work and `fast_diff cfg` avoids that state. Whether real runs reach it is not checked.

[D] A DN2 core regenerated from the state B profile, on main before these changes: 15.5% more host instructions on post-reset, because the root lists no longer match its regions (interpreted 9.98M). With 36 more roots: post-reset 2.2% fewer, first boot 12% more. A core from the union of state A and B profiles, with those roots, in paired 3 s runs: post-reset 0.948 host instructions and 0.921 cycles, first boot 1.076 and 1.068. Dispatches fall from 8.67M to 2.68M but fast calls rise from 1.12M to 3.39M. Not adopted: it may overlap the gain above and needs a combined measurement.

[O] About 1.14M interpreted appended instructions per audio second remain. Six entries at 24,000 calls per audio second each stop at a form 14a access: `normal-word address outside the plain range` (`_static_plain` in `sharc_lowergen.py`).

[O] DN2 post-reset stays below real time, which is about 23G host instructions by DT2 at 22.5G and 1.02x. Remaining estimates: AOT path length about 12% (a rough hand count), appended code about 1.4G, union core up to 5%. Their sum still falls short, so reaching it also needs more AOT work moved to the fast tier; AOT is about 60% of the total.
