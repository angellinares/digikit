# DN2 DSP performance and live audio — stopping-point handover

Date: 2026-10-03. Workspace: `/Users/em/src/digi/digitakt2`.
Branch: `work/sharc-emulator`.
HEAD: `eecf7b8a498df2c11c7ffd4181db4b5a4d1034a0`.

## User instruction and stop state

The goal remains faithful real-time audio across ColdFire and SHARC, in the
native emulator and eventually browser/WASM and Windows. **It is not achieved.**
The latest user requested a handover and brief progress update at a natural
stopping point, ending with **“No commits.”** That final instruction overrides
the earlier request to commit. No further commits were made after it. This
handover and the matching continuation/findings updates are uncommitted.
All owned build, benchmark and agent jobs have finished; no experiment needs
resuming. Do not cancel user-owned applications or builds.

Earlier authorized commits remain:

- `4660aee`: native audio integration and cross-link diagnostics.
- `dc6863f`: measured unknown-loop fallback, generator version 11.
- `49f7125`: default native/browser audio, startup/display/panel fixes and capture.
- `d35d76d`: measured ASTAT helper improvement and lightweight native audio status.
- `eecf7b8`: controlled PGO findings and stronger ignored audition-test gates.

Preserve these pre-existing dirty files; they are unrelated to the final update:

- Modified `docs/HANDOVER-2026-10-01-rust-desktop-browser.md`.
- Untracked `docs/HANDOVER-2026-10-02-sharc-audio-checkpoint.md`.
- Untracked `docs/HANDOVER-2026-10-03-dn2-audio-realtime.md`.

Read the active status at the top of
`docs/HANDOVER-2026-10-03-dn2-native-audio-continuation.md` and the latest entries
in `docs/findings/07-emulator.md` for additional historical detail.

## What works, and what still fails

Default native audio is wired up. `mise run emu` uses local ready inputs and
the stable ignored `out/native/dn2-audio/gen` version-11 cache. It does not need
`--coupled`. `--no-audio` mutes but retains DSP coupling; `--cf-only` is an
explicit diagnostic mode. Native launches skip the browser WASM prebuild.
The retained ASTAT source improvement is in the normal source; **PGO is still
a private candidate and is not enabled by the normal launcher.**

The user's actual GUI pre-sink WAV was clean, near 263 Hz, while physical live
playback sounded noisy with many underruns. Analysis found 99.9903% of non-DC
power around the fundamental, active RMS 0.21863, peak 0.693, no clipping or
32-frame discontinuities. This localizes the fault to live delivery/output for
that run. Starvation is strongly supported; do not claim every audible artifact
has been independently proved to be starvation. The callback inserts zeros
when the ring is short; underrun events count affected callbacks, not missing
audio duration. Private capture: `/private/tmp/dn2-audio-listening-20261003/`.

Coupled measurements establish a DSP throughput bottleneck. An earlier
0.759333-second audio workload needed about 1.7 seconds DSP wall, versus roughly
0.009 seconds SPI and 0.006 seconds SPORT. Wait timings overlap; do not add them.
ColdFire/DSP already pipeline on separate threads with dependent N-to-N+1 work.
Unrestricted frame/voice parallelization is not justified.

Browser audio is default in the coupled build. The validated WASM ABI run
produced 72,896 PCM values, 1,139 SPORT blocks and no missing blocks, canonical
PCM SHA-256 `38d2a3224d10e0b59cafa602ba1b8f8be31562efa07491b963ff4e127f3ff1f8`.
Artifact `packages/web/public/emulator-core.wasm` SHA-256:
`4b1321270c853c8c2667cef166c3562fdb29d369a65161d3df2145b9eddc169b`.
Physical browser playback and real-time performance were not validated.

## Best measured result: native PGO candidate — preserve and integrate next

The accepted native PGO candidate is available locally and must be carried
forward, rather than retrained on the rejected region-cap trees:

- Candidate test executable:
  `/private/tmp/dn2-region-regs-20261003/accepted-current/pgo-enhanced-tests`.
- Training profile: `/private/tmp/dn2-pgo-20261003/merged.profdata`.
  SHA-256: `a49a6b4a737ac0067a0f3ce35dd95ecc2e27398a45f856d121a183c3108b6787`.
- Accepted generated core: `out/native/dn2-audio/gen`.
- Results and provenance: `/private/tmp/dn2-pgo-20261003/`.

The candidate executable and profile were verified present at handover update.
These are temporary private artifacts; preserve them in an ignored durable
location before scratch cleanup. The executable is a test harness, not the GUI
application. Integration into normal native launches still requires an actual
application profile-use build, as described in next step 1 below.

Seven alternating control/profile-use pairs preserved the fixed nonzero QA
ColdFire/DSP/PCM assertions, 1,139 SPORT blocks, 36,448 stereo sample frames
and zero missing blocks. Median **paired** candidate/control ratios:

| Measurement | Ratio | Reduction |
| --- | ---: | ---: |
| Workload wall | 0.791387 | 20.861% |
| DSP wall | 0.789116 | 21.088% |
| Whole-process user + system CPU | 0.857616 | 14.238% |
| Whole-process retired instructions | 0.923736 | 7.626% |
| Whole-process cycles | 0.857875 | 14.212% |

Median workload time fell from 1.754423 to 1.386955 seconds for 0.759333 seconds
of audio: approximately **20,775 to 26,279 sample frames/s**, versus the required
48,000/s. The best candidate still takes **1.827 times real time** and needs
approximately **45.25% further execution-time reduction, plus headroom**.
This is a measured step forward, not a completed underrun fix or an ETA.
Do not multiply gains from different historical workloads into a total.

The valid pair used identical production source, accepted generated cache,
root release settings (16 codegen units), explicit `aarch64-apple-darwin`, and
the pinned **rustup** Rust 1.98.1 / commit 48a229cea / LLVM 22.1.8 distribution.
Only profile-use/warning flags differed. A copied Homebrew baseline was caught
and rejected before accepting comparisons: equal version/commit strings do not
make those compiler distributions interchangeable.

Four uniquely named nonzero QA profiles trained PGO; holdouts were excluded.
An initial default-filename overwrite was caught before merging. All three
hottest generated regions have nonzero counts; missing-function warnings remain
in untrained code but do not identify generated image blocks. The merged profile
is `/private/tmp/dn2-pgo-20261003/merged.profdata` (14,532,560 bytes).
Profile-use flags were:

```text
-Cprofile-use=/private/tmp/dn2-pgo-20261003/merged.profdata -Cllvm-args=-pgo-warn-missing-function
```

Records: `/private/tmp/dn2-pgo-20261003/{summary.json,bench.tsv,paired-analysis.json}`
and `logs/bench-*.log`. **The scratch control target was subsequently reused.**
Use the immutable accepted binaries below rather than assuming its current
Cargo output is still the accepted control:

| Artifact under `/private/tmp/dn2-region-regs-20261003/accepted-current/` | SHA-256 |
| --- | --- |
| `control-enhanced-tests` | `875e2331d4eb4f32553abd9f816b71a0b9c4f3ecf806277342c9b6b36bfeab5f` |
| `pgo-enhanced-tests` | `e212f6b6bb4045b329f1e16ed5ba56d9e0ffeed001fadda493a85dcd3f73037c` |

PGO is native Mac/workload-specific evidence. There is no Windows or WASM PGO
gain claim. Whole-process counters do not identify DSP-only IPC, core placement
or instruction-cache misses.

## Completed negative result: register-cap experiment

Matched instruction samples in the prior DSP replay identified 360 stack-access
samples among 6,415 (5.61% overall; 360/1,255 in the three hottest regions).
That supports a register-pressure hypothesis, not proof of removable spill cost.
Generated region leaves accounted for 82.245% of that earlier replay profile;
interpreter/fallback cost is now small. Family labels are leads, not projected
speedups. Profile artifacts: `/private/tmp/dn2-postgain-profile-20261003/`.

The original generation invocation for the accepted 746-region cache could not
be reconstructed. Available adjacent manifests produce different trees. We
therefore generated a matched fresh BASE and cap candidate with exactly these
inputs and flags, varying only `--region-regs 36` versus `28`:

```text
.venv/bin/python tools/sharc_rsgen.py dn2-1.11
  --coverage /private/tmp/digi-r1-pipeline-a/prof/merged-final
  --entries /private/tmp/digi-r1-pipeline-a/prof/merged-final.entries
  --transitions /private/tmp/digi-r1-pipeline-a/prof/merged-final.trans
  --model-safe --explicit-memory-model 0 --chain
  --exclude 0xb88a49:0xb88abc --region-insns 120
  --region-regs 36|28 --unknown-fallbacks 0x1c253f
  --work <separate-scratch-work> --out <separate-scratch-output>
```

These are multiline command notes, not a directly executable shell script.
Full records: `/private/tmp/dn2-region-regs-20261003/` (`summary.json`,
`provenance.txt`, `input-SHA256`, `regen.tsv`, `cap.tsv`, logs, generated trees).
BASE/cap have 490/545 regions, both 1,277 blocks and the retained fallback.

Three alternating current-unprofiled/BASE pairs showed median paired BASE/
current DSP wall **1.479871** (47.987% more). That measures the whole regenerated
configuration change, not the cap. Seven alternating BASE/cap pairs showed
median paired cap/BASE DSP wall **0.974966** (2.503% less), workload wall
**0.975261** (2.474% less). Five of seven workload pairs improved; two regressed.
Median workload BASE/cap: 2.792952/2.732294 seconds for the same 0.759333 seconds
audio, about 13,050/13,340 sample frames/s. **Both replacements were rejected.**
No normal-cache edit or PGO training on these slower trees was accepted.

All coupled exactness gates passed. Short holdouts and a delayed cap holdout
matched state, instruction and PCM gates but were silent. Python fallback
checks passed 9 tests with one slow check skipped; the explicit slow budget/
mask/trap rollback test separately passed. Immutable trial executables:

- `executables/base-tests`: SHA-256 `58c0b68a8e119859334dcb852fd27c7242f0b187ee63669ee5a77dd2e51835dd`.
- `executables/cap28-tests`: SHA-256 `b82e5083a707b81b3f47d30503f45497dcbc36981c81993de17f4f1de2c247a4`.

A shared scratch Cargo output was overwritten before BASE was copied. This was
repaired by preserving cap immediately and rebuilding BASE once. In future,
copy and hash every executable before changing generated inputs or flags.

## Next work, in priority order

1. **Make the proven PGO candidate usable in the actual native application.**
   Use an ignored durable host-specific profile such as
   `out/native/pgo/dn2/<host-target>/merged.profdata` with a provenance manifest
   covering its hash, generated-core hash, native Rust inputs, Cargo manifests/
   lockfile, compiler distribution/build and target. The launcher should use
   fixed derived flags and a manifest-keyed target cache only when validation
   succeeds, respecting existing user flags. Stale/missing profiles should
   fall back to ordinary release with a brief notice. A fresh actual-app build
   is needed at that durable path; do not copy a scratch test executable into
   the normal launch path. Native source/generated changes invalidate the
   profile; UI-only Astro changes need not. No Windows/WASM assumptions.

2. **Capture a reproducible audible GUI checkpoint instead of sweeping delays.**
   The ignored test
   `desktop_runtime::coupled::tests::desktop_trig1_capture_without_qa_script`
   now records CF/DSP hashes and explicit instruction counters, PCM hash/count,
   synchronized held-window timing, and an optional nonzero requirement.
   Short and delayed 1.2B-before-press/100M-held PGO holdouts were exact but
   silent; they do not validate the user's audible held note. The real GUI
   capture has PCM but no paired event/state checkpoint. At a known audible
   event, atomically preserve CF save-state plus DSP export/DT2DSP01 continuation
   through the actor, including clocks, ready/input_ready, pending IRQ and
   input boundary. Replay that exact checkpoint/event headlessly. This gives
   an independent representative correctness/performance fixture.

3. **Profile the best accepted coupled DSP candidate before further changes.**
   The previous replay profile is a lead, not the post-PGO cost map. Identify a
   specific remaining cost and explain why a proposed source change targets
   it. Reconstruct accepted generator provenance before whole-tree regeneration;
   the rejected fresh BASE shows why this matters. No remaining change has yet
   demonstrated the additional roughly 45% reduction required. Do not keep
   sweeping register caps or treat stack samples as guaranteed savings.

4. **Validate live playback after integrating throughput gains.**
   Measure sustained native production rate, ring occupancy/starvation and
   input/UI responsiveness on the actual audible fixture. The native frontend
   currently pumps 250k ColdFire chunks over IPC then schedules with
   `setTimeout(0)`; timer-clamp overhead has not been established. Status polling
   cleanup and gap metrics are secondary instrumentation, not evidence that
   host scheduling is the main bottleneck. Browser/Windows need their own
   correctness and performance validation.

Source boundaries relevant to the audible-fixture discrepancy:
`native/boot/src/runtime.rs:588` defines structural `ready`; `input_ready` is a
separate condition, initially false and set when queued input service validates
UART/DMA input. See approximately lines 422, 543, 631 and 1392. GUI and ignored
test both use button 25, but continuous pumping/event context differs. This is
a boundary difference to capture, not a proven cause of silent holdouts.
The QA workload also includes NO/NO/encoder actions and is distinct from plain
startup plus Audition Trig 1.

A possible later source lead is an explicitly guarded binary32 arithmetic fast
path at the emitter boundary. Do not globally replace shared f64 helpers:
true double operations, partial values, NaN payload/order, zero/subnormal and
flag semantics matter. Any attempt needs differential bit/flag tests and paired
coupled gates; classification overhead may erase a small gain. `V` already has
u32 fields, and generated code already specializes known masks. No blanket
“replace i128 V” or “add known-value specialization” optimization is justified.

## Execution and validation conventions

Use configured fresh-context scout/worker/reviewer roles when delegation is
authorized; no child agents. Parent owns decisions, integration and final
verification. Every handoff states cwd, edit authority, paths, constraints,
acceptance criteria, validation and stop conditions. One writer per worktree.
The synchronous hook was reviewed/trusted earlier; do not bypass role/fork
denials or alter hook policy. Preserve the user's latest no-commit instruction.

Keep heavy builds and timed runs serialized. Rust 1.98.1 edition 2024 is pinned;
use the same rustup distribution in both controls. Root release uses 16 codegen
units (standalone SHARC configuration differs). Large generated Rust explains
multi-minute cold compiles; a stationary crate counter is not evidence of a
hang. Do not restart builds merely because they are expensive.

Use exact CF/DSP state, instruction counters, PCM hash/count and missing-block
gates alongside alternating performance pairs. Preserve executable/input hashes
and run order. Do not hide slowness by reducing guest budgets/clocks, stretching
time, substituting tones or increasing buffer latency. Private firmware,
generated cores, profiles and captures remain ignored/uncommitted. Use scoped
rustfmt; avoid unrelated malformed/pre-existing files such as `frames.rs`.

Previously completed retained-code checks: ASTAT reference differential test;
Node 9/9; actor no-runtime/stale-session/CF-only checks; Astro check with zero
errors/warnings and one style hint; frontend/native release embedding; WASM
coupled ABI exactness. PGO test changes were independently reviewed. The final
stopping-point edits are documentation only and need `git diff --check`, not
another DSP build or timing run.
