# SHARC runtime performance diagnosis, 2026-10-26

**[D] [C]** DT2 OS 1.17, native host, base `d0a4d76d`. The initial diagnosis
changed no emulator source. The October 26 follow-up tested a small fast-tier
load guard change. October 27 default-activation experiments found stock
regressions and were initially withdrawn. The earlier V2 candidate was then
restored for DT2 default use after explicitly separating the remaining DN2
overhead investigation (see the final sections). No generator or firmware
patches changed.
This updates the October 23 external performance plan and the historical
patched-image section of [finding 18](18-dsp-performance.md). The narrower
invalidation in `60aa71ba` already exists; do not repeat that implementation.

## Decision

Retain V2's measured DT2 runtime improvement and activate `0x1c2607` by
default only when code verification bars its generated entry. Track the
measured DN2 overhead separately; it is not a reason to discard the DT2 gain.
The user's October 27 scope decision supersedes the initial blanket rejection.

Current runtime patches still cause substantial stock code to execute through
the interpreter. Stock firmware has a different execution distribution.
The recoverable **host time** of the whole converter remains unmeasured;
instruction share is not a speedup ceiling. The `14d` continuation is now
implemented and measured below. Re-profile the current runtime before choosing
the next optimization; pending-transfer dispatch remains lower priority.

Any accepted follow-up must enable its improvement in normal workbench
launches, without experimental environment variables. Opt-in measurements
alone do not satisfy that requirement.

## Inputs and build cost

Artifacts are local under `out/native/runs/20261026-sharc-diagnosis/`:
`manifest.json`, `stock-clean.json`, `stock-profile.json`, `stock-host.trace`,
`stock-host.xml`, `host-attribution.json`, `runtime-probe.json`,
`runtime-diagnostic.json`, `runtime-fast-probe.json`, their logs and diagnostic
scripts. Firmware, snapshots, card and binary hashes for the coupled stock
probe are in the manifest. Do not commit these derived artifacts.

The coupled binary is the immutable copy `coupled-run-1a3ab96cb008`, copied
from the nominal plain target used by the October 26 verification run. There
is no immutable original build-flags manifest, so non-PGO provenance is not
fully established. The separate daemon comparison uses the same immutable
`digi-emud-355279ba4e0b` for both configurations, from the PGO daemon target.
Do not combine those binaries' timings as an A/B.

`native_pgo check --device dt2` reports stale sources, workspace, generated
core and workload. Refresh applicability/training before a tuned baseline or
optimization claim. Its input preparation unexpectedly ran a cached +Drive
build check (0.20 seconds, no compilation reported). There were no SHARC Rust
builds or core regenerations in this investigation.

The stock SYX hash is `26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5`,
matching DT2 1.17 and the ready-state provenance. The generated core reports
`dt2-1.16`: both versions' section-7 blobs hash to
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`, matching
the generated image hash. The label does not mean these runs loaded OS 1.16.
An independent evidence audit checked those hashes and the stock results.

Keep the registered generated-core path and existing target directory when
iterating. `tools/emulator/native_target.py` keys Cargo reuse by generated
path and variant; a new worktree, generated path or RUSTFLAGS variant can pay
for another large generated-crate compilation. An engine-only change can
reuse that crate. Keep experiments out of the launcher's reserved `pgo-use`.

## Stock observations

Two three-audio-second scenarios used the same one-sample card and ready
state: dismiss the startup modal with no note, and the existing
`dt2-sample-trig`. These short observations include startup after restoration;
they are not a steady-state or representative workload baseline.

| Scenario | DSP thread CPU / audio second | Interpreted / non-skipped instructions | PCM |
| --- | ---: | ---: | --- |
| No note | 1.07909 | 0.62072% | Three seconds of zeros |
| Sample trigger | 1.08257 | 0.62279% | Finite, nonzero audio |

Both passed runner health gates with no missing blocks. Neither supplied an
expected PCM pin. A separate execution-profile run reproduced both full PCM
hashes and reported zero code mismatches. Its timing was perturbed and is not
a performance comparison.

A separate 12-audio-second no-note recording used CPU Profiler. In the
trace-relative `[2,10)` window, 28,403 running DSP-thread samples partitioned
by stack ancestry as follows (not exact CPU-time shares or removable costs):

| Stack ancestry, in classification precedence | Samples | Share |
| --- | ---: | ---: |
| `exec_insn` | 2,354 | 8.29% |
| Generated compiled block, without `exec_insn` | 22,850 | 80.45% |
| Other `sharc_native::Engine` | 2,928 | 10.31% |
| Other or unresolved | 271 | 0.95% |

The XML analysis resolves IDs/references and nested tagged backtraces. An
independent auditor reproduced the counts. The largest compiled leaf samples
were `r_1C6EA9` and `r_1CB407`; do not infer their full function costs from leaf
samples. The historical generic-browser 5.8%-instructions/72%-time result
does not describe this native stock configuration.

One stock entry, `0x1c06fe`, made 463,500 zero-progress compiled calls in the
three-second no-note probe. Every recorded bail had a pending delayed
transfer. The generated entry immediately returns when `s.pending` is set.
Checking earlier could avoid the attempt while preserving interpretation,
but would not remove interpreted instructions. Its host-time ceiling is
unknown; do not relax the delayed-transfer safety condition.

## Current runtime observations

The disposable `runtime-probe.ts` uses the existing `Session` helpers and
pinned daemon. Stock starts from its ready snapshot; runtime uses the current
`emulator` patch set and a genuine patched cold boot. Both run freely, settle
for 12 wall seconds, then measure status-counter deltas over at least three
audio seconds. Each owns its card overlay. No module is uploaded by this probe.

| Configuration | Audio window | DSP CPU / audio second | Interpreter instructions / audio second | Appended instructions / audio second |
| --- | ---: | ---: | ---: | ---: |
| Stock | 3.2 s | 0.94084 | 1.948M | 0 |
| Runtime | 3.0 s | 2.33669 | 23.239M | 4.715M |

The runtime has 249,750 code-mismatch rejections per audio second. Both
windows have zero additional code verifications and zero verification time:
the old repeated-hashing bug is not the observed cause. Boot and selected
firmware identities are saved in `runtime-probe.json`.

This is one exploratory configuration comparison, not an alternating paired
optimization benchmark. It has no PCM/state equivalence gate; stock and
runtime boot paths differ. It establishes a target for attribution, not an
approved speedup or real-time claim.

The diagnostic runtime pass logs 10 differing decoded PCs:
`1c207a, 1c207b, 1c2731, 1c2732, 1c2733, 1c4ecf, 1c75c0, 1c75c1, 1c75c2, 1c80f2`.
No range is barred whole; 44 entries are barred by the narrower verifier.
The final engine-lifetime histogram contains 182,999,253 interpreted
instructions. Of these, 105,457,536 (57.63%) are in
`0x1c2607..0x1c28b5`, within DT2 1.17's parameter converter `FUN_1c24e9`.
The histogram includes settling and shutdown work; it is not the measured
three-second window and has no host-time attribution.

There is also conservative over-barring from inferred block extents: the
previous entry `0x1c78cc` is treated as extending to the next entry plus six
words, which includes changed `0x1c80f2` across a large gap. Static review
found no such instruction in that body, but complete dependency coverage is
still required before narrowing it. The sampled instruction counts in this
affected group are much smaller than in the genuinely patched converter;
do not prioritize it merely because it appears easier to change.

## Bounded fast-tier probe and next experiment

One diagnostic run set only `SHARC_FAST_REGIONS=0x1c2607`, preserving the
existing models and guards. It built a kernel and executed 3.696M fast-tier
instructions per audio second, at 48,000 calls per audio second: **77 useful
instructions per call**. The remaining work still fell back. Compilation
reported a stop at `0x1c2844`, unsupported form `14d`; that static stop is not
proof of the dominant dynamic exit.

The instrumented window observed 2.35579 DSP CPU/audio second. It is not
comparable to the clean runtime number as a speedup measurement: logging,
different ordering and no paired correctness gate remain. Do not ship the
region list on this evidence. This probe required no Rust rebuild.

The initial next step was to obtain dynamic exits/useful-work attribution for the patched
converter and its continuation, plus a bounded runtime host profile. Choose
between a narrowly supported run-time compilation improvement and splitting
generated dependencies/entries around the patch. State the measured host-time
ceiling before implementation; avoid broad regeneration until that decision.
Then use current PGO policy consistently, exact PCM and state boundaries,
and one alternating `coupled_ab.py` comparison. Its current acceptance gaps
(failed exits/gates/hashes are recorded but not rejected before ratios) must
be addressed before relying on automated speed claims. Include audible
transitions and the dynamically loaded/selected PLUCK case before acceptance.

## Same-page load experiment, October 26

**[D]** Opportunity ceiling first: the restored runtime's separate eight-audio-
second host profile has 20,402 interpreter-ancestry samples out of 42,818
running DSP-thread samples (47.65%) in trace-relative `[2,14)`. This is the
broad interpreter opportunity, not an exact removable-time bound for this
load or region. Compiled ancestry accounts for 35.46%, other engine ancestry
16.39%, and unresolved samples 0.50%.

The actual dynamic exit was `0x1c26b7`, at the flat-memory bitmap guard,
before the static `14d` stop. Register captures show a four-byte SISD load
at byte address `0x2559de`: offset 30 within a 32-bit presence-bitmap word.
Subsequent addresses advance by `0x60`, alternating offsets 30 and 62 within
a 64-bit bitmap word. The old guard rejected these even when all four bytes
were present on the same page.

The public [SHARC+ Core Programming Reference](../sharc/SOURCES.md), printed
page 7–6 (PDF page 224, local extraction `out/refs/sc58x-2158x-prm/pages/p0224.txt`),
allows all alignments for normal-word SISD accesses in byte space. It separately
requires normal-word alignment for SIMD. The manual establishes that this
captured load is legal; the bitmap restriction is an emulator implementation
detail, not a SHARC alignment requirement.

The experiment checks both affected 32-bit presence words for multibyte
loads and requires the complete access to remain inside one page. Stores,
page absence, MMIO, aliases and code-watch handling retain their guards.
No instruction semantics, generated cores or default region lists changed.
Focused tests cover 1/2/4-byte accesses, both bitmap boundaries, each missing
byte, page ends/crossings, absent/MMIO pages, aliases and store guards through
both the interpreter backend and native Cranelift. Both tests and
`mise run test:changed` passed; an independent source review found no issue.

Artifacts are under `out/native/runs/20261026-sharc-flat-crossing/`:
`runtime-inputs.json`, `runtime-host-attribution.json`, `crossing-registers.csv`,
`build.json`, `before-correctness.json`, `after-correctness.json`, `paired.json`
and `paired-validated.json`. The replay restores a genuine current patched
runtime boot, including its matching ColdFire and DSP snapshots and card.
It contains no dynamically uploaded module. Each scenario runs three audio
seconds, including restoration startup rather than only steady state.

One alternating `coupled_ab.py` experiment used three pairs per scenario,
the same frozen inputs and `SHARC_FAST_REGIONS=0x1c2607` on both binaries:

| Scenario | Before median CPU/audio s | After median CPU/audio s | Median paired reduction |
| --- | ---: | ---: | ---: |
| No note | 2.304 | 1.734 | 24.76% |
| Sample trigger | 2.353 | 1.745 | 25.59% |

Paired ratios ranged 0.752–0.761 and 0.742–0.746 respectively. Every run had
exit status zero, health gates true, and the same full 64-character PCM hash
within its scenario. These were independently asserted after the tool returned,
before interpreting its ratios; the tool's acceptance behavior itself is
unchanged. Ignore its misleading total-CPU/interpreted-instruction statistic.
The separate correctness reports also confirm finite audio, three-second
duration, silence for no note, and nonzero sample-trigger output.

The trigger moves from 77 to 229 fast instructions per call at 48,000 calls
per audio second. Interpreted work falls from 58,619,406 to 36,731,406
instructions per three-second run. This explains the measured direction
without equating instruction counts with host time.
The separate `after-coverage.log` records all 143,993 successful no-note
kernel calls exiting at the static `0x1c2844` stop, rather than the old load
guard. An independent evidence audit reproduced the profile counts, PCM
equality, instruction movement and paired ratios.

The candidate `coupled-run-7545ca71a8c7` built in 16.04 seconds (Cargo 15.95),
reusing all four recorded generated/runtime rlibs unchanged in size and mtime.
Focused test rebuilds took 17–19 seconds. The current plain Cargo fingerprints
record empty Rust flags; the old frozen binary still lacks its original
immutable build manifest. This is a local non-PGO experiment, not a claim
about refreshed PGO, browser performance or stock firmware gains.

**Historical acceptance checklist:** Before enabling the region by default, establish safe runtime-specific
selection, check stock behavior and the uploaded/selected PLUCK workload,
compare architectural state at matched boundaries, and measure under a
consistent current PGO policy. Full PCM equality in two short probes is
useful evidence, not general architectural equivalence. The helper fix alone
does not activate this DT2 region in normal launches.
Before merging the shared helper, measure DN2's existing default fast regions
for overhead from the additional presence-bitmap check; correctness gates do
not establish performance neutrality there.
Static dispatch review confirms that a configured fast region runs before
an available AOT entry. Adding this root to the default table would therefore
also change healthy stock dispatch. At that point there was no invalidation-only
activation hook; the eligibility decision after AOT invalidation in
`packages/emulator/sharc/src/lib.rs` is the narrow boundary to investigate.

The routed `mise run verify -- --base main` gate passed in 1,531 seconds;
logs are in `out/runs/verify/20261026-122921/`. It included 243 passing Rust
tests (three skipped), all 17 tier-comparison scenarios equal, audio pins,
stock, runtime workflows, module loads and smoke checks. This broader gate
used normal region configuration, not the experimental DT2 root override.
Its runtime workflow suite dominated elapsed time (1,244 seconds); this
should not be confused with the 16-second candidate compilation.

## Default activation experiments and initial rejection, October 27

**[D] [C]** The opportunity ceiling remains the broad 47.65% interpreter
sample share above, not an exact removable-time bound. Runtime gains alone
were insufficient to accept a shared-engine change as a workbench default.
The October 26 shared load change was reverted to `d0a4d76d` in commit
`0d6d9abb`, alongside removal of the unaccepted activation experiments.
The historical experiment results above remain valid for their binaries.

The proposed policy registered DT2 1.17 root `0x1c2607` as fallback-only:
code verification had to bar its actual generated entry before it could
execute. Healthy stock AOT chains were retained. Launcher, coupled runner,
PGO workload configuration and TypeScript test inputs supplied firmware-keyed
defaults; explicit settings retained precedence. DN2 region lists were unchanged.
No generator changes or regenerated cores were needed.

Initial activation showed approximately 29% lower runtime DSP CPU, but also
stock regressions. Subsequent experiments cached policy, skipped inactive
tier work, reduced bitmap checks, specialized dispatch and finally restricted
widened loads to fallback roots. None established acceptable performance on
both stock devices. Correctness tests and source review did not predict these
performance regressions. The final V6 candidate passed 67 focused Rust tests.

One final alternating `coupled_ab.py` experiment used three pairs per
scenario, three audio seconds per run, frozen inputs and fresh plain baseline
builds from `d0a4d76d`:

| Fixture / scenario | Baseline median DSP CPU/audio s | V6 median | Median paired change |
| --- | ---: | ---: | ---: |
| Runtime DT2 idle | 2.408 | 1.781 | -25.95% |
| Runtime DT2 sample | 2.403 | 1.781 | -25.90% |
| Stock DT2 idle | 1.040 | 1.106 | +6.32% |
| Stock DT2 sample | 1.051 | 1.113 | +5.94% |
| Stock DN2 idle | 1.083 | 1.107 | +2.60% |
| Stock DN2 chord | 1.074 | 1.102 | +2.72% |

All 36 runs exited successfully, passed health gates and matched their full
baseline PCM hashes. Every stock pair was slower. Separate correctness runs
matched all six complete SHRD state files byte for byte and matched DSP
instruction boundaries: 3,000,001,500, except DN2 chord at 3,001,334,834.
An independent evidence auditor reproduced these checks and all ratios.
Raw PCM was not retained for independent rehashing; the PCM comparisons use
full hashes emitted by the runner. Three short pairs do not characterize all
workloads, and these measurements do not establish a cause for the slowdown.

The new baseline DT2 build reproduced `coupled-run-1a3ab96cb008` exactly,
resolving the earlier baseline build-provenance concern for these comparisons.
Both device builds reused cached generated crates; V6 builds took about
15 seconds each. This is not a refreshed-PGO or GUI end-to-end performance
claim. The loaded/selected PLUCK proof and workbench candidate build were not
run after the stock performance rejection. The helper must not be described
as a currently enabled workbench speedup, and real-time DSP is not achieved.

Artifacts are retained in `out/native/runs/20261027-sharc-default/`:
`baseline-build.json`, `candidate-v6-build.json`, `v6-paired-summary.json`,
`*-v6-paired.json`, `v6-correctness-summary.json`, and the original reports and
state dumps. `unaccepted-v6-performance.patch` preserves the complete proposed
performance diff against main; `rollback-manifest.json` records the source
restoration. Earlier variants are retained there for comparison, not accepted
as production alternatives.

Rollback validation passed all 55 baseline no-gen Rust library tests. Fresh
DT2 and DN2 coupled builds reproduced both main-baseline executable hashes
exactly (`rollback-validation.json`); documentation path checks passed.
No full routed gate was repeated, and the retained timing instrumentation
still requires that gate before merge.

The initial decision was to keep workbench defaults unchanged until stock
neutrality was established. That requirement was imposed by the agent, not
the user, and is superseded below. More speculative dispatch variants are not
justified by this evidence. The separate test-suite timing result is recorded in
[finding 69](69-runtime-test-timing.md); it changes no waits or boot isolation.

## Restored V2 and DT2 workbench default, October 27

**[D] [C]** The user approved retaining the demonstrated DT2 benefit and
tracking the stock overhead separately. V2 is restored rather than the later
V6 candidate: V2 kept stock DT2 close to baseline and improved the patched
runtime more. Fresh DT2 and DN2 builds reproduce the two original V2 binary
hashes exactly, so the audited V2 paired and complete-state results apply to
the restored source without another benchmark cycle.

The broad interpreter opportunity was 47.65% of sampled DSP-thread time in
the earlier runtime profile; this is not an exact ceiling for this region.
The existing V2 alternating A/B has three pairs per scenario:

| Fixture / scenario | Main median DSP CPU/audio s | V2 median | Median paired change |
| --- | ---: | ---: | ---: |
| Runtime DT2 idle | 2.448 | 1.747 | -28.66% |
| Runtime DT2 sample | 2.467 | 1.758 | -28.72% |
| Stock DT2 idle | 1.072 | 1.076 | +0.42% |
| Stock DT2 sample | 1.086 | 1.085 | -0.14% |
| Stock DN2 idle | 1.094 | 1.123 | +3.02% |
| Stock DN2 chord | 1.102 | 1.123 | +1.76% |

All 36 paired records passed health/exit checks and full PCM equality. All six
separate complete SHRD comparisons were byte-identical at matched instruction
boundaries. These checks were independently audited. The DN2 slowdown is a
measured residual issue; its cause is not established. Do not attribute it
to identical DSP hardware, a particular guard, or dispatch overhead without
further evidence. Neither this result nor the workbench build claims real-time
DSP or refreshed PGO performance.

The normal DT2 workbench launch now supplies the fallback root automatically:

```sh
mise run wb -- firmware/Digitakt_II_OS1.17.syx
```

No performance environment override is needed. The existing default device
choice is unchanged: plain `mise run wb` uses DN2 unless configured otherwise.
Explicit fast-region/auto settings retain precedence; an explicit empty
`SHARC_FAST_FALLBACK_REGIONS` disables this fallback default. Healthy AOT
entries are retained; the root becomes eligible only after code verification
bars that actual entry. Unknown firmware hashes acquire no inferred roots.

The restored build passed 66 no-gen Rust tests, 82 focused Python tests and
four TypeScript support tests. The normal `mise run wb -- --build-only`
DT2 build passed, using the launcher's normal stale-PGO fallback. The native
daemon PLUCK proof passed with the fallback both off and on: the module was
loaded from the card, selected as the sound machine, and played. All four
complete WAV files (dynamic/reference, off/on) are byte-identical. This is a
native-daemon audio proof, not GUI end-to-end or live-USB-upload coverage.
The changed-file preflight passed. The full routed gate remains required
before merging the restored activation and timing instrumentation.

Restoration artifacts: `out/native/runs/20261027-sharc-default-restored/`
contains `build.json`, `workbench-build.log`, `launcher-default.json`,
`pluck-validation.json`, and the
retained PLUCK reports/WAVs. V2 performance/state evidence remains under
`out/native/runs/20261027-sharc-default/` in `*-v2-paired.json`,
`v2-paired-summary.json`, and `v2-correctness-summary.json`.

## Next target: converter continuation after Type14d

**[D]** An independent current-image decode identifies `0x1c2844` as
`R6 = DM(0x25590c) (sw)`: a non-exclusive, zero-extending 16-bit load.
The existing interpreter supports this exact case. The public SHARC+ manual's
Type14d table distinguishes this SW form from signed SWSE and exclusive forms;
see [SHARC sources](../sharc/SOURCES.md). The runtime compiler presently refuses
this form, despite already having guarded unsigned 16-bit load primitives.
The return at `0x1c28b0` has two delay instructions, at `0x1c28b2` and
`0x1c28b4`; preserve both when extending compilation.

A bounded three-audio-second sample-trigger probe kept the restored default
root and explicitly added ordinary root `0x1c2847`, immediately after the
interpreted load. It compiled 47 static instructions through the return.
The successful continuation calls executed 7,055,657 instructions across
143,993 calls; three initial calls declined a window guard. Full PCM, complete
SHRD bytes and DSP instruction count still matched the baseline. This probe
used diagnostics and establishes lowerability/correctness for that replay,
not speed or general safety of a new default. The extra root is not enabled
by default. Artifacts are in `continuation-probe/` under the restoration run.

The continuation's exact host-time ceiling remains unknown. A subsequent
diagnostics-off comparison used `coupled_ab.one_run` under one exclusive
benchmark lock, with three alternating pairs per scenario and the same V2
binary. A retained the default fallback root; B additionally set ordinary
root `0x1c2847`. It measured an achievable improvement from compiling the
continuation while still interpreting Type14d, not Type14d lowering itself:

| Scenario | Restored default median CPU/audio s | With continuation | Median paired reduction |
| --- | ---: | ---: | ---: |
| Idle | 1.758 | 1.637 | 6.88% |
| Sample trigger | 1.768 | 1.640 | 7.10% |

All 12 runs passed exit/health checks and full PCM equality. Paired ratios
ranged 0.9261–0.9312 and 0.9239–0.9290 respectively. The separate complete
state check above covers the sample-trigger probe. Records and exact
configuration are in `continuation-paired.json`. This additional root remains
experimental; ordinary roots can intercept healthy AOT execution, so adding
it globally would bypass the default policy's protection for stock firmware.

**[D]** The measured continuation benefit justified a bounded follow-up for
Type14d fast lowering. A narrow implementation can reuse the existing guarded
short-word load and retain SISD/encoding restrictions without changing the
generator. Broader forms, SIMD and exclusive-access semantics remain out of
scope. This follow-up is completed in the Type14d section below. Keep the
DN2-overhead investigation separate from this DT2 opportunity.

## Resumable DT2/DN2 validation workflow (2026-10-27)

**[D]** The new `mise run perf:matrix` command separates explicit binary
preparation from repeatable validation. `run` never builds emulator binaries;
environment-only A/B experiments can use one frozen binary set for both labels.
See [the configuration and commands](../TESTING.md#resumable-sharc-matrix).
DSP patch experiments continue to use `coupled_ab.py --patch`; the matrix
requires full-state equivalence. PLUCK still prepares its usual module artifacts.

The initial same-binary pilot passed DT2 1.17 and DN2 1.12 stock and patched-runtime
replays, plus PLUCK on both devices. It used one three-audio-second scenario per
replay cell and one timing pair per cell. All replay pairs matched full PCM,
complete SHRD state, instruction count and audio duration; PLUCK's retained
reference and dynamic WAVs matched across labels. This validates the workflow,
not a new performance improvement or broad workload coverage. Repeating the
completed command took 3.68 seconds and preserved all 224 retained artifact mtimes.

Pilot inputs and imported binaries are recorded in
`out/native/runs/20261027-sharc-matrix-setup/`; outputs are in
`out/native/runs/20261027-sharc-matrix-pilot/`. The setup includes a newly cold-booted
DN2 runtime CF/DSP snapshot pair with recorded provenance. Existing coupled
binaries and the DT2 daemon were reused; only the missing DN2 daemon was built,
using the existing generated Rust. `matrix.json` in the setup directory contains
the corresponding three-pair configuration for subsequent experiments.

Independent review identified missing port-definition hashes. The final runner
includes recursive port TOMLs and frozen manifests, and rechecks the full input
identity before accepting completion. Twenty focused tests cover resumed pairs,
artifact tampering, environment isolation, port/manifest edits and six classes of
mid-run identity changes. These metadata safeguards landed after the full pilot;
its checkpoint is intentionally not migrated to the changed runner.

The final runner then passed a fresh stock replay on each device, explicitly
marked `partial-pass` because runtime and PLUCK were omitted from this smaller
probe. Its resume took 3.74 seconds with all 60 artifact mtimes unchanged. Outputs:
`out/native/runs/20261027-sharc-matrix-resume-probe/`. The changed-file checks
passed; the full branch acceptance gate remains required before merge.

**[O]** Consolidate firmware-specific fast-tier defaults from
`config/emulator.json` into the corresponding version entries in
`devices/digitakt-ii.toml` and `devices/digitone-ii.toml`. Preserve exact firmware
hash matching, explicit override precedence and unknown-firmware behavior across
Python, TypeScript and browser consumers. Keep experiment overrides in matrix
configuration and resolved defaults in frozen manifests. This is a separate
configuration migration, with no intended DSP behavior change.

## Type14d converter continuation enabled (2026-10-27)

**[D]** The exact host-time ceiling remains unknown. The earlier continuation
probe established an achievable approximately 7% reduction without lowering the
load itself. A narrow handwritten fast-tier handler now lowers only the
non-exclusive unsigned short-word Type14d load (`d=ex=w=x=0`, `l=1`) in SISD.
It reuses guarded two-byte loads with direct, unscaled byte addresses. Other
Type14d encodings and SIMD remain refused. No generated core or generator source
changed. No additional roots or configuration overrides were enabled: the
existing DT2 fallback root now compiles through the load and its continuation.

Public manual cross-check: SHARC+ Core Programming Reference, locally extracted
pages 384–386 (printed 15–4 through 15–6), specifies the encoding and zero-extension;
page 187 (printed 6–3) specifies opcode-sized byte-address accesses. See
[public SHARC sources](../sharc/SOURCES.md). Independent source review found no
correctness issues. Three focused Rust tests passed against the cached DT2
interpreter, covering all five encoding bits in SISD/SIMD, high-bit zero-extension,
odd and mapped-looking byte addresses, page-end fit, page crossing and missing
backing-byte fallback. The success test executes only the load and asserts one
fast instruction; fallback cases assert none.

The scripted matrix used the previous frozen baseline and newly frozen candidate
binaries, with three alternating pairs per workload and the shared
`coupled_ab.py` sample helper under an exclusive benchmark lock. Median paired
ratios, rather than ratios of independently calculated medians, determine changes:

| Workload | Baseline median DSP CPU/audio s | Candidate median | Median paired change |
| --- | ---: | ---: | ---: |
| DT2 runtime idle | 1.760 | 1.640 | −6.86% |
| DT2 runtime sample trigger | 1.766 | 1.647 | −6.77% |
| DT2 stock sample trigger | 1.099 | 1.099 | +0.12% |
| DN2 stock chord | 1.132 | 1.132 | −0.03% |
| DN2 runtime chord | 1.515 | 1.462 | −0.52% |

DT2 runtime paired ratios ranged 0.9289–0.9326 at idle and 0.9296–0.9342 with
sample triggering. DN2 runtime was noisier (0.9647–1.0046); its independent
medians exaggerate the paired improvement, so do not claim a DN2 speedup.
These short workloads do not establish broad workload coverage or real time:
DT2 runtime still consumes about 1.64–1.65 DSP CPU seconds per audio second.

All five correctness replays and every timing sample matched full PCM, complete
SHRD state, final instruction count and audio duration. PLUCK passed for both
labels on both devices with matching complete reference/dynamic PCM. The matrix
finished with full coverage and 22 completed steps. DT2 idle fast-tier work grew
from 32,974,397 to 40,174,047 instructions over three audio seconds, establishing
that the continuation actually executes compiled. The behavior is active under
the normal default fallback policy when this source is built by the workbench.

Artifacts: `out/native/runs/20261027-sharc-type14d/` contains configuration,
`candidate/frozen.json`, build and test logs, and `results/` with the checkpoint,
summary and full outputs. Four candidate binaries built in 14.13, 4.10, 16.00 and
4.23 seconds, reusing generated Rust. Changed-file checks passed. A temporary
matrix-test path literal was also corrected after pathcheck mistook it for a
repository file; all 20 matrix tests passed. The full branch acceptance gate
remains required before merge.

## Next priorities after Type14d

1. **[O]** Run the full routed branch acceptance gate before merging. The focused
   tests and performance matrix do not replace that gate.
2. **[O]** Re-profile the current DT2 patched runtime, with matched idle and active
   workloads, to rank remaining interpreter exits, compiled-code cost and engine
   overhead by host time. The earlier 47.65% interpreter share predates both
   accepted improvements and must not be reused as today's opportunity ceiling.
   Choose one bounded target from the new profile, estimate its removable cost,
   then use the frozen DT2/DN2 matrix for exactness and paired timing acceptance.
3. **[O]** Attribute the earlier V2 DN2 stock regression separately. Compare V2
   against the pre-V2 baseline with focused profiling; the Type14d result only
   establishes that the latest change adds no consistent DN2 regression. Shared
   DSP hardware does not establish the cause of different firmware execution cost.
4. **[O]** Reconsider the known pending-transfer zero-progress dispatch only if
   fresh host-time attribution gives it a worthwhile ceiling. Do not prioritize
   it from call counts alone or begin a broad generator rewrite.

Firmware-specific TOML consolidation remains maintenance work, separate from the
performance priorities. Longer active/audio workloads should broaden acceptance
before describing short replay improvements as general device performance.

Both accepted changes are on by default in a fresh workbench build of this branch.
Use `mise run wb -- firmware/Digitakt_II_OS1.17.syx` for DT2 (plain `wb` can select
DN2). The launcher builds current source and resolves fallback defaults for the
stock firmware profile. An already-running process or separately launched old
binary retains its old code. Explicit fast-tier environment overrides can suppress
the defaults. The latest Type14d validation used native coupled/daemon binaries;
it did not add a new GUI end-to-end proof or retrain PGO.

## Desktop firmware switching defaults (2026-10-27)

**[C] [D]** Earlier workbench activation statements were incomplete: a normal
single-core launch did not provide matching DSP inputs/core for switching to the
other device through Choose File. Desktop launch now includes both available
cores by default, so `mise run wb` needs no `DIGI_EMU_SECOND_CORE=1`. Explicit
`DIGI_EMU_SECOND_CORE=0` preserves a single-core escape hatch; daemon launches
remain single-core by default. Both firmware ready-input sets and generated cores
must exist; missing inputs retain the existing diagnostic and single-core fallback.

Defaults stay attached to the compiled core's firmware: a DN2-first process gets
DN2 regular/block roots and DT2 fallback root `0x1c2607` in the second core's
settings. Firmware load selects the matching audio inputs and engine. The
handwritten Type14d handler is compiled into both engine instances. Neither this
change nor firmware switching regenerates DSP Rust.

Sixty-two focused launcher tests, changed-file checks and independent review
passed. A fresh desktop launch without a second-core override ran DN2 with SHARC
audio. Computer-use could not address the bare development executable, even after
unlocking the screen; no literal file-picker click proof is claimed. The existing
controller switch proof passed DN2 → DT2 → DN2 with runtime patching, matching
core selection, advancing frames and no DSP halt at each phase. This exercises
the firmware-load controller shared with the desktop UI.

The full frozen-binary matrix passed all five exactness replays, both PLUCK
proofs and all timing samples against the Type14d single-device binaries. Median
paired combined/single CPU ratios were 1.0099 (DT2 runtime idle), 1.0017 (DT2
runtime sample), 1.0128 (DT2 stock), 1.0153 (DN2 stock), and 0.9886 (DN2 runtime).
These short timings were noisy, especially DN2 (stock range 0.9795–1.0717;
runtime 0.8572–0.9909), with an interactive workbench also running. They establish
no additional speedup or tight regression bound. The exactness results are
independent of that timing noise.

The combined desktop's first native build took 5m19s; both generated crates
compiled, and the remaining DN2 compilation used about 15 CPU cores after DT2
finished. That is a separate combined target cache, not regeneration. Subsequent
validation runners reused it (14.70s coupled-run, 4.17s daemon). The combined
build now has its own PGO profile (device `both`, `mise run perf:pgo:both`); see
"Two-core PGO profile (2026-11-01)" below. Per-device profiles are not applied to it.
The comparison here is against the measured non-PGO binaries, not a freshly
trained single-core PGO build.

Artifacts: `out/native/runs/20261027-sharc-dual-default/` contains the frozen
binaries, switch proof, matrix, and launcher checks. The initial branch gate was
stopped when the user requested this new default; it is superseded, not a pass.

## DT2 patched runtime: interpreted work by group (2026-10-31)

**[D]** DT2 1.17 patched runtime (dsp-voice, converter, fx and link patches), `dt2-led-sweep`,
3 audio seconds, ready state from a cold patched boot, `coupled scenario --diag hist`. The
earlier figure of 19.4 M interpreted instructions per audio second (58.2 M in 3 s) was measured
without the fast tier. The patched-image hash has no row in `config/emulator.json`,
so a run that passes the patched `.syx` gets no fallback root unless
`SHARC_FAST_FALLBACK_REGIONS` is set. The workbench resolves the defaults from the stock
firmware and does run root `0x1c2607`. With that root alone the same run interprets 18.0 M
(6.0 M per audio second) and the fast tier runs 40.2 M (the converter, 279 instructions per
call, 48,000 calls per audio second).

Where the 18.0 M go (one root, before the change below), and why:

| Group | M / 3 s | Share | Why interpreted |
| --- | ---: | ---: | --- |
| Stock voice prologue `0x1c4ecf..0x1c4f1d` | 5.47 | 30% | The hook replaces the word at `0x1c4ecf`, so its region is barred. The adapter returns to `0x1c4ed2`, which is no AOT entry. |
| Blocks of `0x1c1f7d` (`0x1c1fe7`, `0x1c204b`, `0x1c207b`) | 3.16 | 18% | The fx adapter patches the words at `0x1c207a/b`; the regions before and after are barred. |
| Interrupt-handler code (`0x1c02a4`, `0x1c0b1d`) | 3.11 | 17% | No generated entry on the path. The wrapper starts with `BIT TST MODE1STK, 0x10000`; the fast tier has no lowering for a bit test of a system register (the generated form refuses it as a dynamic branch), and it refused the entry for good. Same code runs on stock. |
| Appended adapter and module code (`0xb8d...`, `0xbcd...`, `0xbce...`) | 2.59 | 14% | Not in the generated image; no AOT entry can exist. Equals the `interp_appended` counter. |
| Other stock code in `0xb8....` | 2.23 | 12% | Not examined: no generated entry on the executed path. |
| Other stock code in `0x1c....` | 0.98 | 5% | Not examined. |
| `0x1c06fe` zero-progress bail | 0.46 | 3% | Generated entry returns at once while a delayed transfer is pending. |

Ceiling (finding 18: 60 to 105 ns per interpreted instruction, about 1 ns compiled): the
6.0 M per audio second cost 0.36 to 0.63 DSP seconds per audio second. The prologue group
alone is 0.11 to 0.19, the `0x1c1f7d` group 0.06 to 0.11. These are interpreter-rate
estimates, not measured host time.

Change: `config/emulator.json` for DT2 1.17 gets regular root `0x1c4ed2` and
fallback roots `0x1c1fe7`, `0x1c204b` next to `0x1c2607`. A fallback root alone cannot take the
prologue, because a fallback root must be a barred AOT entry and `0x1c4ed2` is not one; on
stock code nothing reaches `0x1c4ed2` through the interpreter. Interpreted instructions on
`dt2-led-sweep` (3 s): 18.0 M to 10.2 M (6.0 M to 3.4 M per audio second); `dt2-rec-play`
(6 s): 36.0 M to 20.4 M. PCM and the DSP state dump are equal to the one-root run on both.
`pk_tiers` PCM is equal for stock, the voice hook and PLUCK held (3 s each); the PLUCK window
goes from 14.9 M to 13.3 M interpreted per audio second, of which 4.1 M are PLUCK's own
appended code. The ISR code, appended code and the `0x1c06fe` bail remain; they need
generated-core, fast-tier or JIT work.

**[D]** Timing of the root change (2026-11-01), `perf:matrix`, 3 alternating pairs per cell, 3 s
audio, DSP-thread CPU, ratio B/A (lower is faster). Both sides use the same two-core PGO binary;
only the environment differs. A: `SHARC_FAST_REGIONS_2` unset,
`SHARC_FAST_FALLBACK_REGIONS_2=0x1c2607`. B: `SHARC_FAST_REGIONS_2=0x1c4ed2`,
`SHARC_FAST_FALLBACK_REGIONS_2=0x1c2607,0x1c1fe7,0x1c204b` (DT2 is slot 2, so the lists carry
the `_2` suffix).

| Cell | Ratio B/A (range) | Interpreted instructions, A to B |
| --- | ---: | ---: |
| DT2 runtime idle, `dt2-led-sweep` | 0.905 (0.880-0.913) | 18.19 M to 8.50 M |
| DT2 runtime sample, `dt2-sample-trig` | 0.876 (0.869-0.882) | 18.01 M to 8.15 M |
| DT2 stock, `dt2-sample-trig` (control, new roots not reached) | 1.014 (0.991-1.026) | 5.93 M both |

The new roots save 9-12% DSP-thread CPU on the patched DT2 runtime. On stock the extra
fast-tier lookup per PC is within noise.

## Interpreter exits: chase and appended entries (2026-10-31)

**[D]** The fast tier consults an entry only at a PC that is in its entry list. A refused
entry (its first instruction is one a kernel cannot run) stayed with the interpreter for good,
and so did all code after it. `0x1c02a4` (context save of every interrupt) is the main case:
its first instruction is `BIT TST MODE1STK, 0x10000` (form 18a, bit-test; the hand lowering
handles only MODE1 set and clear), and the function goes on with PCSTK, LCNTR, CURLCNTR, ASTATX
and status-stack moves, 20a PUSH and POP, MMR stores, ACONV and a delayed RTI, none of which a
kernel runs. Between those instructions lie runs of 3 to 21 plain stores and moves.

Two changes in `packages/emulator/sharc/src/fast/mod.rs` and `lib.rs`:

- **Chase.** A refused entry, whose instruction is not a branch or delay slot, makes the next
  instruction an entry; so does the instruction after a stop of such a region. The chain follows
  straight-line code. A chased entry whose calls retire fewer than 3 instructions on average is
  dropped after 64 calls. `SHARC_FAST_CHASE=0` turns it off. DT2 1.17 got 23 more regular roots in
  `config/emulator.json`: the places where the interpreter resumes after a branch
  (found with the interpreter histogram and the instruction table).
- **Appended entries.** When the interpreter lands in appended code (above the generated image's
  code, set by the code check) by a taken branch, call, return or loop-back, that PC becomes a
  chased entry. Appended code needs no unknown range and no moved data: the kernels are built
  from the loaded words and checked against code stores as before (105 code generations in 3 s).

Interpreted instructions, 3 audio s, same PCM hash and byte-equal DSP state dump before and after:

| Replay | Before | After |
| --- | ---: | ---: |
| DT2 stock, `dt2-led-sweep` | 5.85 M | 3.28 M |
| DT2 patched runtime, `dt2-led-sweep` | 10.66 M | 5.89 M (appended 2.60 M to 0.44 M) |
| DN2 stock, `trig1-hold` | 1.92 M | 1.92 M |
| DN2 patched runtime, `trig1-hold` | 7.79 M | 6.12 M (appended 2.74 M to 1.07 M) |

DN2 stock does not move: its generated blocks already cover the interrupt code (the seeds gave
-2%, so none were added). Fast-tier calls on DT2 patched rise from 0.30 M to 0.94 M per 3 s;
the fast tier runs 6 M more instructions (about 9 per added call). **[O]** No timing was measured.

What stays interpreted on DT2 stock (3.28 M), with causes, **[D]**:

| Group | M / 3 s | Cause |
| --- | ---: | --- |
| `0x1c0b1d` dispatch | 0.57 | MMR accesses (14a outside the plain range), ACONV (7d), PCSTK moves, 20a, 11c |
| `0x1c02a4` save and restore | 0.47 | PCSTK, LCNTR and CURLCNTR moves, 20a status-stack forms, MR data moves, 11c |
| `0x1c06fe` | 0.46 | one NOP per consult while a delayed transfer is pending: the interpreter runs it and the failed consult adds cost; a pending check before the consult saves the consult, not the instruction |
| `0xb8853a`, `0xb88200`, `0xb888d2` | 0.50, 0.26, 0.36 | task-switch save and restore: system-register moves, 20a, shift opcodes 0x14, 0x1d, 0x1f, MODE1 writes that change the bank selection |
| `0xb8636d`, `0xb863ca` | 0.53 | a leaf called with the PC stack push still pending: block and kernel entries are gated until it completes |
| `0x1c18a6`, `0x1c207b`, rest | 0.2 | not examined |

Supporting PCSTK, LCNTR and status-stack forms in the kernel glue would make most of the first
four rows runnable, at the cost of modelling the PC and loop stacks in kernel state: at most
2 M instructions per 3 s (about 2 to 4% of one DSP thread at 60 to 105 ns each). Not worth it
before a timing A/B shows the interpreter share.

## Two-core PGO profile (2026-11-01)

**[D]** Setup: `perf:matrix` (`tools/devtools/sharc_matrix.py`), 3 alternating pairs per cell,
3 s audio, DSP-thread CPU, ratio B/A (lower is faster), quiet machine. Both binaries are
two-core coupled-run builds (`--features second-core`, slot 1 DN2 core, slot 2 DT2 core). A is
plain (`native_pgo coupled-build --device both --variant ab-plain`). B uses the two-core profile
(`--profile current --variant ab-pgo`, profile key `6db98eed3f895b45`). Runtime cells use the
patched-runtime fixtures from `mise run perf:fixture` (`out/fixtures/runtime/<dev>/`). PCM, DSP
state, instruction count and duration were identical between A and B in every cell.

A/B 1, plain against PGO, scenarios in the training set:

| Cell | Ratio B/A (range) |
| --- | ---: |
| DT2 runtime, `dt2-sample-trig` | 0.829 (0.826-0.836) |
| DN2 runtime, `trig1-hold` | 0.795 (0.787-0.837) |

A/B 3, same binaries, scenarios not in the training set:

| Cell | Ratio B/A (range) |
| --- | ---: |
| DT2 runtime, `dt2-led-sweep` | 0.824 (0.822-0.828) |
| DN2 runtime, `kb-chord16` | 0.821 (0.816-0.822) |

The two-core PGO profile cuts DSP-thread CPU by about 18% on both devices, also on scenarios
outside the training set. DN2 gains far more than with the single-core DN2 profile (3%). The
two-core profile also trains on the patched runtime. The single-core DN2 profile trains only on
the stock exactness scenario. The DT2 root change is timed in the 2026-10-31 section "DT2
patched runtime: interpreted work by group" (A/B 2).

Artifacts: `out/native/runs/20261101-two-core-pgo/ab/` (`config-ab1.json`, `config-ab2.json`,
`config-ab3.json`, `results-ab1`, `results-ab2`, `results-ab3`).
