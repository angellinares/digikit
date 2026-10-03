# DN2 native/browser audio continuation — 2026-10-03

Latest stopping-point handover:
[DSP PGO and region-cap results](HANDOVER-2026-10-03-dn2-dsp-pgo-and-region-cap.md).
The region-cap experiment below is now complete: both regenerated trees were
rejected as replacements for the current core. All owned jobs have finished.
The user's latest instruction is **no further commits**; the final handover
and findings update are intentionally uncommitted.

## Active underrun work: DSP throughput (2026-10-03)

The user confirmed that the actual GUI pre-sink WAV is a clean tone while
live playback is noisy, and explicitly asked to fix underruns. DSP effective
throughput below real time is already established; do not spend the next
iteration re-establishing it or claim status polling is the primary cause.
Commit `49f7125` contains the validated default audio/display/capture work.

A bounded scheduler experiment requested interactive QoS on the coupled DSP
worker, reusing the existing native live helper. Five same-binary off/on
pairs preserved every coupled PCM/state assertion, 1,139 frames and zero
missing blocks. All requests succeeded, but both settings still needed roughly
2 s for 0.759333 s audio. Results varied, and every pair ran off before on,
so run order limits attribution. The toggle was removed rather than promoted
as a throughput fix. Raw logs: `/private/tmp/dn2-qos-20261003/`.

The retained source improvement simplifies only `_astatx_define` in
`native/sharc/src/rt/bnd.rs`: merge known-bit masks and result bits directly,
preserving the old value when the merged mask is zero. CACC/forget semantics
remain untouched. This is grounded in the improved profile's repeated flag
updates; the full ASTAT family share (8.839%) is not the expected saving.
Independent review accepted the algebra and differential coverage, including
noncanonical unknown values and wide input truncation. Seven alternating pairs
preserved all coupled PCM/state assertions and each was faster. Median paired
DSP wall ratio 0.978344 (2.166% less), workload wall ratio 0.978559
(2.144% less), total-process user+system CPU ratio 0.986885 (1.311% less).
Candidate median 1.757835 s for 0.759333 s audio is still 2.315x slower than
real time. Retain this modest improvement; do not claim underruns fixed.
Artifacts: `/private/tmp/dn2-astat-20261003/summary.json` and
`paired-analysis.json`; baseline/candidate executables are preserved. The
standalone helper differential test and all fourteen coupled checks passed.

Secondary host cleanup replaces the periodic full diagnostic/sync/hash request
with session-validated `emu_audio_status`, using nonblocking poll/report.
Explicit diagnostic export retains its existing barrier. Native pump gap
metrics now measure accepted adjacent responses and invalidate across lifecycle
and input completion; they are omitted for WASM, which lacks that native
invalidation contract. Focused Node checks passed 9/9 and actor no-runtime,
stale-session and CF-only-null checks passed. The retained DSP/helper and host-cleanup changes are committed as `d35d76d`.
Final Astro check (zero errors/warnings, one style hint), frontend build and
native release embedding passed after the timing runs.

The bounded PGO trial is complete and demonstrates a repeatable gain. Its
original copied baseline used Homebrew Rust; this was caught before accepting
comparisons, and a new uninstrumented control was compiled with the exact same
pinned rustup compiler, explicit target, source, generated tree and release
settings as the candidate. Only profile-use/warning flags differ. The compiler
is Rust 1.98.1 commit 48a229cea, LLVM 22.1.8, aarch64-apple-darwin. The matching
LLVM tools were installed through an approved CLI component installation.
Training used four uniquely named QA profiles; holdouts were excluded. Nonzero
profile coverage includes all three hottest generated regions. Missing-profile
warnings remain in untrained code; there were none for generated image blocks.

Seven alternating pairs passed exact QA CF/DSP/PCM assertions, 1,139 SPORT
blocks, 36,448 sample frames and zero missing blocks. Median paired use/control
ratios: workload wall 0.791387, DSP wall 0.789116, total-process user+system CPU
0.857616, retired instructions 0.923736 and cycles 0.857875. Whole-process
counters include CF, DSP and setup; they do not establish DSP IPC or a cache
bottleneck. Median workload 1.754423 -> 1.386955 s produces 0.759333 s audio:
20,775 -> 26,279 sample frames/s, still 1.827x slower than real time.
PGO is a proven private native performance candidate, not yet installed in the
normal launcher and not a Windows/WASM profile or an underrun fix.

The existing ignored audition test now prints CF/DSP state hashes and explicit
instruction counters, PCM hash/count and a separately synchronized held window.
Timing remains optional. Both short and delayed 1.2B-before-press/100M-held
holdouts matched every state/clock/PCM/count check, but both were silent; this
does not validate the user's audible held-note scenario. Requiring nonzero PCM
correctly failed on the delayed control. No input-parameter sweep was attempted.
The no-profiling invocation also passed. Independent review found no residual
test issue. Private rustc harness attempts were abandoned after link failures;
Cargo rebuilt only the desktop tests and reused SHARC for the final gates.
Artifacts: `/private/tmp/dn2-pgo-20261003/summary.json`, `bench.tsv`, and logs.

Matched instruction samples also confirm compiler stack accesses in the three
hottest replay regions: 360/6,415 samples (5.61% overall, 360/1,255 within those
regions). This supports investigating region register pressure, not claiming
all stack traffic is removable or that instruction-cache misses dominate.
The original full-generation command for the current 746-region cache is
unavailable; adjacent manifests produce different trees. A one-knob region cap
trial used a separately regenerated BASE and candidate with the same recorded
inputs/flags, then compared BASE to the current artifact as well.
Do not silently compare a newly generated tree to the old cache and attribute
the whole difference to the cap. Preserve the 0x1c253f unknown fallback.

The completed trial changed only `--region-regs 36` to `28` between new trees.
Seven alternating pairs passed exact coupled gates; median paired DSP wall
ratio was 0.974966 (2.503% less), with variable results. However, three pairs
comparing regenerated BASE to current unprofiled showed a median paired DSP
ratio of 1.479871 (47.987% more). The isolated cap gain does not recover that
regression: median workload BASE/cap was 2.792952/2.732294 s for 0.759333 s
audio. Neither tree replaces the current cache, and no cap/PGO combination was
trained or accepted. Artifacts: `/private/tmp/dn2-region-regs-20261003/`.
The original PGO/control binaries were preserved in its `accepted-current/`
directory before reusing the scratch Cargo target; use those immutable copies.

## Previous continuation: default audio, startup fixes and live sound capture

Committed before profiling: `4660aee` (native audio integration) and `dc6863f`
(verified unknown-value loop fallback). Default-audio/UI/capture work and its validation are recorded in the
continuation commit containing this update. The user authorized commits and direct work; preserve
the three pre-existing dirty handovers listed below. Real-time audio is still
not achieved. Do not claim that exact replay PCM validates the factory-init
sound on hardware.

Normal `mise run emu` now uses local ready inputs plus
`out/native/dn2-audio/gen` without `--coupled`. The stable ignored cache is a
byte-identical copy of the measured version-11 candidate. Firmware selection
compares against an independent known ready profile before automatically
restoring its continuation. `--no-audio` retains DSP coupling while muting;
`--cf-only` is the explicit CF diagnostic escape. Native launcher and Tauri
frontend builds skip the browser WASM prebuild. Browser setup is visible by
default; normal WASM builds include SHARC when the local core is available,
but private image/state inputs must still be selected and audio started by
a browser gesture. The fresh default coupled WASM build and ABI smoke passed: 72,896 PCM
values, canonical `38d2a322...3ff1f8` hash, 1,139 frames and no missing blocks.
The normal artifact SHA is
`4b1321270c853c8c2667cef166c3562fdb29d369a65161d3df2145b9eddc169b`;
its first separate-cache build took 276.69 s. This is ABI/correctness validation,
not browser playback or real-time performance validation.

The first default build now includes 77 MiB / 2.08M lines of generated DSP
Rust. Progress staying at one crate does not mean a hang. Follow-up native
builds took 14–17 s and reused SHARC; do not cancel the user's active build
or launch competing heavy jobs. Native and WASM caches are separate.

Fixed two restored-display losses: preserve saved `emitted_revision` for
byte-exact save/load, with a nonserialized once-only frame replay flag; cache
the actor's initial load reply for `emu_startup`, since main consumes it before
the frontend exists. Tap now preserves the latest frame while holding across
250k chunks. Fixed faceplate disappearance: the inserted audio row had consumed
the old two-row grid's sized panel region. Flex-column layout retains the
remaining panel height. Private actor startup and nonzero LCD tests, existing
100M CF/DSP/PCM exactness, cross-chunk Tap, launcher 7/7, browser worker/audio
6/6, Astro check and scoped review passed. Native GUI showed the LCD and panel
after pad/audition interaction. Host CPAL smoke passed outside the sandbox;
the sandbox-only CoreAudio OSStatus did not imply missing physical speakers.

Fresh improved DSP profile is complete and independently audited. Marker scope:
6,415 uniquely joined samples; generated-block leaves 82.245%, generated-core
helpers 6.516%. Hot regions: `r_1C399A` 9.930%, `r_1C3862` 4.832%, `r_1C364F`
4.801%. The retained fallback is only 0.826% leaf cost. Whole-scope sampled
families are numeric/address/bit-conversion 12.814%, memory 11.473%, and
ASTAT/status 8.839%; these labels are investigation leads, not causal costs or
projected speedups. Data/scripts are under
`/private/tmp/dn2-postgain-profile-20261003/`. The cost map supersedes the
pre-improvement profile. Next performance work must identify a concrete cost
in generated bodies, then use paired exactness/performance controls.

Latest user issue: startup → choose DN2 firmware → Audition Trig 1, no other
controls, continuous noise while held. An immediate short restored desktop
test captured 0.302667 s of exact zero PCM; delayed/keyboard tests were also
zero. The QA+note reference is a different workload (NO/NO/encoder) with
nonzero ~263 Hz PCM, peak 0.693 and no clipping. Do not conclude the user's
noise is solely underruns from the short silent test.

Added opt-in bounded live capture: `DIGI_EMU_PCM_F32LE=PATH mise run emu`.
It records pre-player f32LE stereo for at most 10 source seconds, resets the
file on a new coupled session, batches file writes outside the CPAL callback,
and reports capture errors separately from device errors. Unset means no
recorder/file/buffer. Helper tests and the normal coupled suite passed (7,
with 5 private tests ignored). The newest release binary includes the recorder.

The parent reproduced firmware selection and Audition in a private native app
copy. `gui-pre-sink.f32le` under
`/private/tmp/dn2-audio-listening-20261003/` holds 10 s, nonzero finite PCM,
peak 0.693, no clipping, whole-capture RMS 0.08646, SHA
`b50148499af2b3f3c27d5243f3c05dab34f04ec36a3bdf05ef4ed94335fc7b16`.
Analysis confirms a clean sine-like tone near 263 Hz: 99.9903% of non-DC power
around the fundamental, no clipping or repeated 32-frame discontinuities.
The unnormalized `gui-pre-sink-active-1p6s.wav` is the gap-free comparison.
The reported noise now points to playback starvation/host output rather than
this captured pre-sink waveform. The user listened to the gap-free WAV and
confirmed it is a clean tone while live playback is noisy. That comparison
localizes the audible fault to live delivery/output; the exact contribution
of starvation versus other host output faults is still to be measured. The initial private UI
check app was paused; do not restart or close a user-owned emulator implicitly.
Source ownership is with parent; signal analysis is complete. A subsequent
uninterrupted native profiling session ran with `DN2_PROFILE_LINK=1`, but
Computer Use timed out after clicking Export diagnostics, so no timing report
was obtained; do not treat that attempt as a measured GUI performance result. Browser connector
discovery returned no available browser;
native GUI was checked with the Computer Use skill instead.

The previously proposed host measurement should separate IPC step response time from active
wall outside steps. Existing `host` metrics include pauses and load time, so
subtract load time and use an uninterrupted run. Periodic native audio status
formerly requested full diagnostics every second, including a blocking
`audio.sync()` and a SHA-256 of the 3,192,192-byte main image. This is a concrete
avoidable observation cost, but its playback impact has not been measured.
Do not assume a timer clamp or call the hash/sync the main cause. A dedicated
nonblocking audio status endpoint has now been implemented (see the active section); the
headless fixture already establishes the remaining DSP throughput deficit.

See the [profile, startup and live sound records](findings/07-emulator.md#improved-dsp-profile-and-default-audio-startup-2026-10-03).

## Latest continuation: verified improvement (2026-10-03)

The user subsequently authorized direct work in this checkout (“would it be
quicker if we just worked here?” / “ok”). This supersedes the historical
subagent-only execution instruction below. The user has now requested committing
the verified work, then profiling and an audible emulator path in separate
agent lanes. Parent owns integration and commits; performance workloads stay
serialized. Real-time audio is still not achieved.

The retained change compiles an unknown-value fallback for DSP region
`r_1C253F`, selected by `--unknown-fallbacks 0x1c253f` in `sharc_rsgen.py` or
`sharc_dn2_aot.py`. Source profiling plus entry-bail instrumentation proved that
unknown R0/R15 masks were rejecting the existing five-instruction AOT loop.
Seven alternating replay pairs saved 21.6% measured CPU with unchanged PCM,
state and instruction counts. Both private coupled fixtures passed; integrated
0.759333 s audio still took 1.816159 s elapsed. Portable Rust; browser/Windows
performance remains untested. See the
[findings and verification record](findings/07-emulator.md#unknown-value-dsp-loop-fallback-measured-cpu-saving-2026-10-03).

Private optimized application cache:
`/private/tmp/dn2-audio-investigation-20261003/unknown-loop-fallback/candidate-gen`.
Set `SHARC_GEN_DIR` to that directory when using opt-in native coupled audio.
The `selected-gen` sibling is a one-block emitter check and must not replace
this application cache. Generator version is now 11; preserve the immutable
version-10 baseline for paired controls. Original dirty native integration is
retained. Inspect and preserve current changes rather than restoring to HEAD.

Next: profile the improved binary with the same workload marker before picking
another optimization. Cross-link SPI cost remains about 9 ms per coupled
fixture; DSP execution still determines throughput. Do not claim sustained
real time or add reply-wait time to overlapping DSP time.

## Historical handover: read this first

**Current operator request: stop the validation/orchestration loop, write this
handover for a new context, and make NO commits.** The next context should start
from this file, not reconstruct the many failed handoffs below.

The goal remains genuine sustained real-time audio in native desktop (`mise run
emu`) and browser/WASM. **That goal is NOT achieved.** We delivered diagnostic
observability and implemented a native opt-in coupling seam, but spent too much
time on orchestration, repeated preflight misunderstandings and an external
auto-fixer. Be direct: one narrow process lane, focused checks, no more broad
scouting or repeated gate bureaucracy.

Commands/builds/tests/profiling must run in subagents. Keep the parent for design,
interpretation, exact edit specifications, orchestration and acceptance. Use
scout for source reading and worker for fully specified source edits. Serialize
builds and performance runs. Do not run concurrent writers in this checkout.
Do not silently switch to an external/foreground agent protocol after failures.

## Repo / Git / ownership

- Repo/cwd: `/Users/em/src/digi/digitakt2`, main checkout.
- Branch: `work/sharc-emulator`.
- Last known HEAD: `7eb90592407626fd0def7c57dcc0f47891ca3391` (`7eb9059`).
- That commit was explicitly requested earlier and already made. **No further
  commit was made or is authorized by the current request.**
- Index was empty at every successful worker/cleanup check.
- This handover is a new, uncommitted file.
- Native integration/error remediation are uncommitted working-tree changes.

### Pre-existing dirt: preserve it; it is NOT a failed preflight

These files were already modified/untracked before this work and stayed
byte-identical through implementation and cleanup:

```
 M docs/HANDOVER-2026-10-01-rust-desktop-browser.md
?? docs/HANDOVER-2026-10-02-sharc-audio-checkpoint.md
?? docs/HANDOVER-2026-10-03-dn2-audio-realtime.md
```

**“Unchanged handovers” means unchanged from this dirty baseline, NOT clean
against HEAD.** A process agent incorrectly blocked because these files were
modified/untracked. Do not repeat that mistake. The new continuation handover
also naturally appears untracked.

Firmware, generated DSP code, snapshots, PCM and other derived artifacts must
remain ignored/private. Never commit them. Never name/copy/quote private vendor
DSP toolchain files. Findings belong in `docs/findings/` and are indexed from
`docs/FINDINGS.md`; this file is an operational handover, not new verified
firmware findings. A second agent must check image bytes before a finding gets
the project's `[V]` mark. Do not run the emulator for static questions; bound
runtime workloads. Do not lower DSP budgets or enlarge buffers as a fake
real-time solution.

## What was committed: diagnostic round

Commit `7eb9059`: **Add audio-health diagnostics and repair desktop lockfile**.
It contains exactly these 11 files:

```
docs/FINDINGS.md
docs/findings/07-emulator.md
packages/desktop/src-tauri/Cargo.lock
packages/web/package.json
packages/web/public/pcm-worklet.js
packages/web/src/audio.ts
packages/web/src/components/CoupledAudio.tsx
packages/web/src/runtime-metrics.ts
packages/web/src/runtime-worker.ts
packages/web/src/runtime.ts
tests/test_audio_health.mjs
```

The lock repair added boot's local `periph` dependency reference and changed
`cssparser-macros` to refer to the already-locked `syn 2.0.119`; no registry
versions/checksums changed in that diagnostic commit. Web changes are
observational: emitted-PCM rate, step-time histogram, queue high-water, underrun
duration, connection/flow indicators and Export audio health. Scheduling,
buffer defaults and firmware semantics were not changed.

### Passed before that commit

- Full `pytest tests -q --slow`: **2,282 passed, 29 skipped, 1 xfailed,
  278 subtests passed**.
- Four Node audio accounting/worklet tests passed.
- Astro check: zero errors/warnings, one existing hint.
- `git diff --check` passed.
- Fresh diagnostic reviewer: OK with notes.

Full-suite environment (only rerun the entire suite before a later requested
commit; use focused tests while working):

```
SHARC_NATIVE_LIB=/private/tmp/digi-r1-int-t-dt2lib/release/libsharc_native.dylib
RUSTC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustc
RUSTDOC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustdoc
DYLD_LIBRARY_PATH=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/lib
CARGO_NET_OFFLINE=true UV_OFFLINE=1
uv run python -m pytest tests -q --slow
```

Precommit report:
`/Users/em/.pi/agent/sessions/--Users-em-src-digi-digitakt2--/subagent-artifacts/outputs/56c11cfd-ef67-460d-9ce7-7f5aa839f2e9/validation/precommit.json`.
Logs: `/private/tmp/dn2-audio-continuation-20261003/{pytest,web-audio,web-check,diff-check,preflight,final-status}.log`.

## Current uncommitted implementation: ten paths

```
packages/desktop/src-tauri/src/desktop_runtime.rs  (new)
packages/desktop/src-tauri/src/main.rs
packages/desktop/src-tauri/Cargo.toml
packages/desktop/src-tauri/Cargo.lock
native/boot/src/sharc_peer.rs
native/boot/src/pcm_play.rs
native/live/src/audio.rs
tools/native_emu.sh
tests/test_native_emu_launcher.py                  (new)
tests/test_lint.py
```

### Native integration (initial nine-file change)

- Desktop `coupled-audio` Cargo feature enables boot `sharc` + `play`.
  Ordinary startup remains CF-only.
- `tools/native_emu.sh` selects that feature only for explicit `--coupled` and
  checks for a local `SHARC_GEN_DIR` before building. No AOT regeneration.
- CLI coupled startup requires a startup SYX and `--dsp-image FILE`,
  `--dsp-state FILE`, `--cf-snapshot FILE`. Optional `--audio-buffer SECONDS`
  accepts finite 0..10, defaults to 0/Live. `--no-audio` is an explicit
  headless diagnostic option.
- `desktop_runtime.rs` owns the CF core and optional audio session on the
  existing actor thread. Only DN2 OS 1.11 is accepted for this coupled seam.
- Restore requires a complete `DT2DSP01` DSP sidecar: 8-byte magic, saved
  EMUCLK tick (u64 LE), cumulative DSP instruction count (u64 LE), then
  canonical DSP state. Bare DSP state is not accepted for this desktop seam.
  CF `load_state` checks firmware identity/hash. Inputs are expected to be a
  matching capture pair; there is no new cryptographic pair manifest.
- Uses SSI period **96,000**, DSP **DEFAULT_PERIOD=666,667**; these budgets
  were not reduced. Creates the engine inside `ThreadedPeer`'s worker.
- Added `ThreadedHandle::poll()` (`commit_ready(false)`) for nonblocking
  collection after host steps. Snapshot/diagnostic barriers call `sync()`.
- Sends PCM to the existing `PcmPlayer`, then clears host-only PCM/raw/frame
  histories. Cumulative peer counters/halt information remain. Production
  history does not grow indefinitely. The player's original unbounded intake
  channel policy was NOT redesigned.
- Added `PlaybackStats`: stream-start state, received source frames, pending
  feeder frames, device ring frames, underrun callback EVENTS, maximum callback
  size and error. Maximum callback size is NOT a rendered-frame count.
- `PcmPlayer::finish()` still drains explicitly. Drop cancels and joins,
  including full-ring/below-threshold cases, so stopped/replaced sessions
  don't leave a detached feeder.
- Actor session tokens/rejection/replacement/restart, card read errors and
  250,000-instruction step cap were preserved.
- Coupled diagnostics add `native_audio`; CF-only diagnostics retain their
  ordinary shape. Missing device leaves the coupled core running and reports
  the output failure. Session-wide PCM/wall ratio includes pauses; do not
  mistake it for an isolated performance benchmark.
- Cargo.lock adds **17 packages** for optional coupling/playback. Offline
  resolution in a scratch fixture retained every previous registry package
  identity/version/checksum. No broad upgrades.

### Tests added

- Launcher default vs coupled feature selection / missing generation preflight.
- CLI option validation and default CF-only behavior.
- DSP continuation header/payload/counters.
- Player cancellation at full ring / below threshold, late `start()` failure.
- Ignored private-fixture ready+100M exactness and fake missing-device tests.
- Later remediation adds a successful-start-THEN-error, latched-error,
  non-hanging `finish()` regression (not yet run; see next section).

Worker matched the initial nine-file exact content plan, with one authorized
Python import-order correction. Launcher/lint checks passed: **5 tests**,
ruff check and format check clean.

## Initial native gates PASSED; later error fix is UNVALIDATED

Initial process agent ran all six sequential gates successfully:

1. Pinned rustfmt check on four initial Rust paths.
2. Native boot `--features play --lib pcm_play::` headless tests.
3. Desktop default release tests.
4. Desktop `--features coupled-audio` release tests.
5. Ignored ready-fixture exactness + fake missing-device tests.
6. Final diff/scope/hash/index checks.

The ready test reuses the existing QA+note script, bounded to at most 1000
250,000-instruction chunks, ending after ready+100M. It verified:

```
CF digest   fbace0f0cf9fbbb6f5551e5834e8d74b2dfd6c84e81503693924b0aff1a2cb2c
DSP export  05ac2ac2e0e270aefffa207dda09017360eee499bb1851d30271d3c78f304e19
PCM f32 LE  38d2a3224d10e0b59cafa602ba1b8f8be31562efa07491b963ff4e127f3ff1f8
interleaved samples = 72,896; missing SPORT blocks = 0
```

This proves the initial coupled execution/PCM-capture path, NOT physical
speaker output, GUI usability, fidelity or sustained real time.

Initial source-bound evidence:
`/private/tmp/dn2-audio-continuation-20261003/evidence/native-gates.json`.
Full logs in that directory's `logs/01-rustfmt.log` through
`06-diff-check.log` (including `05-ready-fixtures.log`).
Gate report:
`/Users/em/.pi/agent/sessions/--Users-em-src-digi-digitakt2--/subagent-artifacts/outputs/4903f3b4-680d-4401-b181-062edcf3aa86/validation/native-gates.json`.

### Real review blocker: post-start CPAL errors

Fresh reviewer `35acf623-9aea-4f17-b5a2-dae47440be6c` found a valid P1:
initial error telemetry caught probe/start failure, but the real CPAL error
callback after successful stream start only printed to stderr. The feeder
could stay alive, health could say no error, and draining could stall.

Review:
`/Users/em/.pi/agent/sessions/--Users-em-src-digi-digitakt2--/subagent-artifacts/outputs/4903f3b4-680d-4401-b181-062edcf3aa86/review/native-integration.md`.

The exact two-file remediation IS APPLIED:

- `native/live/src/audio.rs`: `StreamErrors` wraps shared atomic events;
  error callback records then retains stderr details; added
  `PendingDevice::start_with_errors`; old `start` stays compatible.
- `native/boot/src/pcm_play.rs`: Opener error accessor; CPAL shares the signal;
  feeder latches error and returns during normal/drain/grace loops; regression
  fake opener succeeds first then receives the same error signal as real CPAL.
- Normal render callback unchanged; no render-callback mutex added.
- Detailed error text remains stderr; health exposes an event-count/error
  message, not the original CPAL error object.

Expected current SHA-256 (confirmed through cleanup):

```
5d63fb829beb63b336ee1700c3b8efff0f7f8d190c693bc21380a56766ad4fe4  native/live/src/audio.rs
8ff174280530d52313b1819427d91d2965a9892fedb92cdd4bd121b739bd87d8  native/boot/src/pcm_play.rs
```

**No compiler/test gate has run on this remediation yet. No reviewer recheck
has happened. P1 remains OPEN.** Subsequent agents repeatedly stopped at
preflight without running tests; don't interpret those as test failures.

## External auto-fix incident: restored, not an open source-scope problem

After the remediation writer's tool/format stage, external semantic auto-fixes
appeared in five unrelated files. This was not ordinary rustfmt (examples:
chunks_exact_mut -> as_chunks_mut, loops -> fill). A scout could not establish
the owning automation. The worker initially restored them, they reappeared,
and it correctly failed scope acceptance rather than approving them.

Operator explicitly chose: **pause auto-fixer; restore captured changes**.
Worker `8818fad8-3213-481f-ae7d-3a85c0efb690` then:

- Verified current five-file diff exactly equaled the captured patch.
- Applied its exact inverse only to those five files.
- Verified all five equal immutable `7eb9059` blobs.
- Verified intended integration/remediation and all handovers unchanged.
- Confirmed empty index and clean diff-check. No test/build/formatter/config
  edit/commit in cleanup.

Restored paths and after hashes:

```
bf0a2b27544441516c58975eb50bc2547f969fa1014bcf74904c84c3f6d9985d  native/boot/src/common.rs
aa4cc713ee329eec1416fdaba020ed87bd487e8e93973b8f7c6efb01bfd4b11e  native/boot/src/main.rs
5302f2c326cf3cdb7e90322cd4293f639777b7ccd801c0cb554427943875b624  native/boot/src/runtime.rs
faaf4f33f8718d9b68696533517eba5633f7561ed014856ad047c8d919cbcf46  native/live/src/player.rs
ad15f59319d32523321b8e26a5a8fa632f4c6550303d85af2153e80b62add2a3  native/live/src/sharc_source.rs
```

Captured patch SHA:
`3ef04a6cc062cca71907e6dffa7c82d8d0f2e838dfcb18e9d03089568d75cbe5`.

Cleanup report:
`/Users/em/.pi/agent/sessions/--Users-em-src-digi-digitakt2--/subagent-artifacts/outputs/b1229e89-bd7f-4858-9519-868e5261c3f3/implementation/autofix-cleanup.md`.
Scratch `autofix-cleanup-before.*`, `autofix-cleanup-after.*`,
`autofix-cleanup-protected-before.json` and `stream-errors-unexpected.diff` are
under `/private/tmp/dn2-audio-continuation-20261003/`.

**Clean restored files are the REQUIRED successful state.** A follow-up agent
incorrectly thought their empty diff meant cleanup had not happened. Do not
apply the captured patch again, reverse the cleanup again, or require the
cleanup packet's BEFORE condition during validation. Verify the AFTER state.

If external mutations reappear, capture the diff and stop; do not overwrite a
newer owner change. The automation's identity remains unknown; the operator's
pause confirmation was used, not a persistent Pi configuration change.

## Shortest useful next steps — do these, not another broad workflow

1. Start a **fresh-context process agent**, not the repeatedly confused retained
   gate agent. Give it these explicit facts: cleanup DONE; dirty/untracked
   handovers are expected pre-existing dirt; no source edits; commands only
   in the child. Check only actual mismatches, not dirty status itself.
2. Run the affected headless tests serially using existing targets:

```
# Common environment:
RUSTC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustc
RUSTDOC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustdoc
DYLD_LIBRARY_PATH=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/lib
CARGO_NET_OFFLINE=true UV_OFFLINE=1
# Use cargo from that toolchain's bin directory.

# SHARC_GEN_DIR unset, CARGO_TARGET_DIR=/private/tmp/digi-r1-int-t-live:
cargo test --release --offline --locked --manifest-path native/live/Cargo.toml --lib
cargo test --release --offline --locked --manifest-path native/boot/Cargo.toml --features play --lib pcm_play:: -- --nocapture

# CARGO_TARGET_DIR=/private/tmp/dn2-audio-triage-20261003/desktop-target
# SHARC_GEN_DIR=/private/tmp/digi-r1-int-gen-dn2
# NOTE_EVENTS=trig
# DIGI_COUPLED_FIXTURES=/Users/em/src/digi/digitakt2/snapshots/dn2-audio-ready-2026-10-03
# DIGI_COUPLED_SYX=/Users/em/src/digi/digitakt2/Digitone_II_OS1.11.syx
cargo test --release --offline --locked --manifest-path packages/desktop/src-tauri/Cargo.toml --features coupled-audio desktop_runtime::coupled::tests:: -- --ignored --nocapture --test-threads=1
```

Use bounded subprocess deadlines (e.g. 600s builds, 120s cached fixture run);
there is no shell `timeout` binary. No hardware/GUI launch. Reuse prior passing
default desktop/CLI/continuation checks: their source did not change. Don't
rerun the full Python suite for this two-file error plumbing fix.

3. Check final ten-path source scope + expected handovers/new handover, the five
   restored files clean, remediation hashes, empty index, `git diff --check`.
   Hash actual rebuilt artifacts and retain logs. No giant repeated inventory.
4. Have the original fresh reviewer recheck ONLY its P1 against the actual fix
   and new logs. Status then resume known reviewer run
   `35acf623-9aea-4f17-b5a2-dae47440be6c` if eligible; use labeled same-role
   fallback only if resume is rejected. Parent adjudicates evidence.
5. Once accepted, document the software result in findings with honest evidence
   limits. Then move directly to trustworthy throughput profiling below.
6. **No commit until operator asks again.** Real-time/audibility/fidelity remain
   separate unresolved deliverables even if the integration fix passes.

## Throughput baselines already measured (older artifacts; not new-build proof)

Separate launch, compute, link and sink. Smooth menus do not prove audio.

For about 0.759 seconds of PCM from the ready+100M note workload:

| Lane | Wall / rate | Interpretation |
|---|---|---|
| CF-only diagnostic `DSP_PERIOD=1` | ~0.8s | Little CF headroom; not real DSP audio |
| DSP replay measured frames [5400,5600) | 0.314s for 0.133s audio; ~213M non-idle instr/s | DSP alone misses real time |
| Coupled native threaded | ~2.1s, 2.8x slower | Both cores coupled |
| Coupled native synchronous | ~3.0s, 3.9x slower | Threading helps |
| Headless Node WASM | ~5.8s, 7.6x slower | Worse than native |
| Chromium worker -> AudioWorklet | 1.825s PCM / 15.538s wall = 0.117 audio s/wall s | Severe production starvation |

Chromium smoke: running AudioContext, PCM received/consumed, 15 underruns,
~12s post-start starvation/rebuffer silence, 2,738 frames, no halt/missing SPORT
blocks/errors. Diagnostic buffer 0.1s; UI default 2s was not changed. This was
headless flow proof, not listening quality. Native GUI launcher built and stayed
alive 8s before SIGTERM after lock repair; **no firmware loaded**, not UI/audio
validation. A nonfatal macOS libLLVM/rust-objcopy load-path warning appeared.

Standalone replay PCM:
`581339b332e936fa9204926f5087b66d9495f530b71272bf0aa24c7ff889de6d`.
Replay states: frame 5400 `97263059...`, 5500 `d47203a5...`, 5599 `7dee6163...`,
5600 `eeeeda3a...`. Full hashes in `dsp-replay.log`/earlier handover.
CF-only digest prefix `4b569189`; coupled digests above.

Existing triage logs/results/browser smoke harness:
`/private/tmp/dn2-audio-triage-20261003/`.

## Private inputs / provenance / cheap profiling path

Hash-identical backups exist in ignored
`snapshots/dn2-audio-ready-2026-10-03/`:

```
digi-audio-loop2-fresh.bin
digi-audio-dn2-image.bin
digi-audio-m5.snap
digi-audio-m5.snap.dsp
digi-audio-note.dt2cap
```

Originals are under `/private/tmp/` with those names. Firmware is
`Digitone_II_OS1.11.syx` in repo root. DT2's `sections/.source-sha256` is NOT
DN2 provenance; DN2 extraction is `out/sections/dn2-1.11/`. New native gates load
SYX and fixtures directly, not `sections/`.

Current generated DN2 tree:
`/private/tmp/digi-r1-int-gen-dn2`, fingerprint starts `e0e97d72`, whereas old
handover says `0e73c5c1`. Origin/config difference remains unknown. Old
`/private/tmp/digi-r1-pipeline-c/manifest.json` is generator 9; current generator
is 10. Do not treat that old manifest as measured-binary provenance. Reuse the
current generated tree for cheap source-bound builds; do not start the ~30min
`tools/sharc_dn2_aot.py` regeneration pipeline casually.

Tree hash convention in `tools/sharc_dn2_aot.py`: sorted top-level `*.rs` and
`*.bin`, hash UTF-8 lines `basename SHA256\n`. Exclude JSON timing reports.
`/private/tmp/digi-audio-current-aot-hashes.json` is NOT JSON despite its suffix.

Older artifacts (not authoritative for current source):

```
/private/tmp/digi-r1-int-t-live/release/examples/sharc_live                       cfb4bbe0...
/private/tmp/digi-r1-int-t-dn2/release/libsharc_native.dylib                       70e69f17...
/private/tmp/digi-r1-int-t-wasm/wasm32-unknown-unknown/release/elektron_native_boot.wasm  b53c378d...
```

Web public `emulator-core-sharc.wasm` matched that older WASM hash. Do not overwrite
it just to profile a new build; load new artifacts via scratch harness/server.

### Exact profiling preparation already scouted; do not repeat reconnaissance

Report (latest read-only follow-up overwrote prior preparation):
`/Users/em/.pi/agent/sessions/--Users-em-src-digi-digitakt2--/subagent-artifacts/outputs/56c11cfd-ef67-460d-9ce7-7f5aa839f2e9/preparation/performance.md`.

`tools/sharc_dn2_replay.py` CLI:
`--lib`, `--image` (default dn2-1.11), `--state`, `--capture`, `--out`,
`--start=4600`, `--end=5600`, `--check=5599`, `--meas=5400,5600`,
`--gap=667000`, `--clock-base=573627620`, `--no-blocks`, `--no-skip`,
`--profile PREFIX`, `--pcm PATH`.
Frame shape: `cap.load(path).dspi2_frames`; TX is `bytes(frame.tx)`.

Native `native/sharc/examples/dn2_replay.rs` expects a scratch frames file:
u32 LE count, then repeated u32 LE byte-length + TX bytes. Run image/state/frames
with `4600 5600 5400 5600`; measured indices [5400,5600), end frame included in
overall replay. It reports busy/idle subtraction and wall/thread CPU timing.
Use windowed deltas, not cumulative restored DSP counts divided by wall time.

Cheap source-bound native build pattern (in a process child only):
`SHARC_GEN_DIR=/private/tmp/digi-r1-int-gen-dn2`, external CARGO_TARGET_DIR,
pinned toolchain; cargo build native/sharc release example `dn2_replay`, offline.
Check correct lock policy for that crate. Record sources/gen/toolchain/flags and
artifact hashes. Run 3 serial repeats, verify PCM/state gates before comparing
timing. Then sample/profile the DSP alone to rank generated-block vs interpreter,
bank/memory/peripheral/dispatcher costs. Historical SipHash ~6% is not proof of
today's bottleneck and cannot explain the whole gap by itself.

Native/sharc release profile: opt-level 3, codegen-units 64, debug false,
panic abort, incremental false. Native/boot has no local release profile.
Profiling/debug/instrumented artifacts must be distinguished from ordinary
throughput artifacts; interpreter option 26 (`--profile`) adds overhead.

**No matching prior headless ready+100M WASM benchmark script was found.**
`wasm.log` exists with the expected coupled digest, but its script is missing.
`browser-health.mjs` is fixed-15s flow smoke, not the exactness/throughput harness.
Other `/private/tmp/digi-audio-*wasm-probe.mjs` scripts only run 100k raw DSP
instructions. Don't substitute them and claim equivalence.

For fresh WASM, build native/boot directly with
`--release --offline --locked --lib --target wasm32-unknown-unknown --features
sharc` and SHARC_GEN_DIR, external target. `tools/native_wasm.sh --sharc` uses
that build but copies to the web public path; avoid that copy while profiling.
Worker normally fetches fixed `/emulator-core-sharc.wasm`; a scratch server can
serve the new artifact at that URL, or parent can specify a minimal ABI harness
after reading exact signatures. No speculative source/harness rewrite yet.

## Local packets / reports / run identities

Scratch root: `/private/tmp/dn2-audio-continuation-20261003/`.

- `EDIT_SPEC.md`, `edit-plan.json`, `apply.patch`, `planned/`: initial native
  exact content packet, plus documented one import-order correction.
- `STREAM_ERROR_EDIT_SPEC.md`: exact two-file post-start error remediation.
- `AUTOFIX_CLEANUP_SPEC.md`: completed cleanup packet; BEFORE is historical.
- `NATIVE_GATE.md`: broad initial gate packet. Prefer the reduced instructions
  in this handover for restart; it was misread by retained process agents.
- `stream-errors-before/after.{hash,diff}`, cleanup evidence, initial native
  gate logs/reports as above.
- Blocked preflight reports (NO tests executed):
  `clean-stream-error-gates/blocked-preflight.json` (clean means not cleaned
  mistake), `post-cleanup-native-gates/preflight.json` (pre-existing handover
  dirt mistaken for new edits).

Mission: `e59e005a-822c-44e4-a5a5-7a63ee2784fa`; goal still open.

Useful retained roles (inspect exact status before resume; use latest IDs):
- Latest worker cleanup: `8818fad8-3213-481f-ae7d-3a85c0efb690` (complete).
- Original independent native reviewer: `35acf623-9aea-4f17-b5a2-dae47440be6c`
  (complete, P1 BLOCK; no subsequent recheck).
- Latest performance scout: `28a2e1c3-b49b-4865-a863-2d79094be0fa` (complete).
- Latest confused gate agent: `faf9c464-ea90-4bd9-8099-0825e0707813` (complete,
  no tests); **prefer a fresh compact process task, not more accumulated
  ambiguous retained gate context**.

All validation workflows finished/blocked; no further managed validation or
performance lane was intentionally left active when this handover was requested.
Check the fleet once on restart; do not poll/sleep awaiting ordinary native
notifications. Historical pi-lens says six deferred targets didn't finish within
its budget; that is not an exact-current-source failure result. Explicit focused
tests above are still needed.

Todo state before writing this handover:
- #10 diagnostic validation/commit: complete.
- #8 native coupled integration: pending, blocked by #11; initial implementation
  exists, final remediation acceptance incomplete.
- #9 source-bound DSP/WASM profiling/real-time gap: pending; preparation exists.
- #11 post-start sink error remediation acceptance: in progress until handover
  request, then paused/pending; fix exists, new checks/review missing.
- #12 unexpected auto-fix ownership/cleanup: complete via operator-approved
  captured restoration; automation identity unknown.
- #13 this handover: finish once written/verified. No commit.

## Why this took too long (avoid repeating)

1. Parent spent too long drafting while a worker waited for supervisor input;
   request timed out. An older workflow also stopped after extension reload.
2. Too many serialized metadata/preflight/review handoffs for a narrow seam.
3. Retained low-reasoning process agent twice misinterpreted expected state
   and ran zero tests (clean restored files; known dirty handovers).
4. External auto-fix mutation distracted from the actual audio error fix.
5. Real-time throughput optimization has not started; integration plumbing is
   not a performance improvement.

Restart with a small explicit executable check task, not another full pipeline
or ownership investigation. Keep the old evidence and known dirty baseline
visible. If actual tests fail, return the first concrete compiler/assertion
failure for exact parent-authored fixes. If they pass, resolve the one P1 and
move to measured DSP/native/WASM performance work. Be honest about the remaining
gap and the lack of listening validation.
