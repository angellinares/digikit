# Bounded sample-bank implementation and proof status

2026-10-29. Targets: Digitakt II OS 1.17 and Digitone II OS 1.12.
**[D]** means source read or local measurement; **[O]** means incomplete. No new firmware claim is marked [V].
This records implementation evidence following [the lifetime investigation](105-sample-loading-lifetime.md);
the [plan](../plans/dsp-sdk-and-sampler.md) retains decisions, build sessions and detailed history.

## API and ownership

**[D]** API 1.3 appends `sample_bank`, `sample_load` and `sample_status`. The public `<dk/sample.h>` declaration
`DK_SAMPLE_BANK(LOCAL_ID, BANK, DESCRIPTOR, CAPACITY)` describes one module-owned DSP BSS bank of 1–8,192
mono float32 frames and four initialized zero descriptor words: READY, normal-word source pointer, frame count,
sample rate. The packer and runtime validate ownership, bounds and descriptor initialization; modules do not
submit arbitrary DSP addresses. The optional DSP section v5 bank record is versioned independently.

**[D]** Resolve an opaque handle during init, after checking the appended API table size. A load from init or a
service callback copies the path and queues UI-task work. Status is UNUSED=0, QUEUED=1, LOADING=2, READY=3,
or an existing negative runtime error. Only an unused bank accepts a job. Once delivery starts its destination
remains pinned until restart, including after failure. Retriggers reset voice cursors; there is no eviction or reuse.

Sources: `sdk/sdk.toml`; `sample.h` in `sdk/include/dk/`; `sample.rs` and `sample_job.rs` in
`modules/core/runtime/sdk/src/`; runtime `sample.rs` in `modules/core/runtime/src/rs/src/`;
`packages/patchkit/src/dkm.rs` and `packages/patchkit/src/dkmdsp.rs`.

## Source acquisition and publication

**[D]** Input is a complete canonical mono 48 kHz PCM16 native sample, nonempty and at most 8,192 frames
(16,464 bytes with header/trailer). The proof's float bank occupies 32 KiB; code, descriptor and voice state are
additional. This is a proof limit, not a product sample-length promise. Stereo and oversize input fail explicitly.

**[D]** DT2 reads an absolute +Drive path, such as `/proof/test`, through read-only filesystem bindings.
The proof excludes simultaneous Transfer writes. DN2 reads `samples/proof/test` from the digikit store into a
private snapshot with metadata/hash checks. DN2 sampling is new digikit functionality, not a stock sample service.
Transfer destinations remain unchanged. Per-device ColdFire function tables hold four DT2 source bindings and
DN2 zeros, so optional source support does not demand absent firmware anchors.

**[D]** Private ColdFire source storage survives asynchronous link reads. The job serializes with existing DSP
module/registry work: payload first, immutable view fields next, READY last, with acknowledgements between phases.
A lost final acknowledgement may follow successful publication, so failure cannot authorize destination reuse.
An ambiguous active transfer retains/quarantines its source until quiescent rather than freeing a live pointer.
This is not a universal rollback safety proof for existing module-row cleanup.

Sources: `sample_source.rs`, `sample.rs` and `dsp.rs` in `modules/core/runtime/src/rs/src/`;
`modules/core/runtime/sdk/src/store.rs`; `devices/fw/storage.toml`.

## Evidence and limits

**[D]** Parent checks passed: 47 SDK tests and nine fit tests. Worker checks passed: five decoder/snapshot,
seven lifecycle, 127 patchkit, 11 dkneeds and eight SDK-generation tests, plus host and ColdFire builds.
`module:check test/sample-proof` reports 29 passed and five skipped in total; package/fit covers both devices.
These checks do not establish end-to-end sample publication or audio through a booted device.

**[D]** The [fixture](../../tests/fixtures/modules/sample-proof/README.md) bridges machine-adapter JUMP entry
to the block routine's CJUMP ABI, preserving I12/I13 and the exact exclusive entry-I7 boundary. Tests cover actual
adapter entry, stack ownership, numerical playback, unready silence, completion and retriggering. DN2 enters at
frame zero; the stock helper's frame value 18 applies after its return.

**[D]** Selected-fixture DN2 boot now passes after the compiler-helper fix below. Local log:
`out/runs/dsp-sdk-checks/sample-dev-dn2.log`. This establishes boot only, not sample playback or performance.
**[D]** First DN2 TypeScript run (session 6653) passed module upload/reboot, oversize rejection (-13), zero
descriptor, no-fault and fresh-boot checks. Unready audio then failed after selecting MIDI row 4, a navigation
error rather than a DSP playback result. Live inspection confirmed SAMPLE PROOF at row 5 after four audio rows
and MIDI; correct selection shows its START/END/REV controls. The scenario now selects row 5, closes with NO,
saves framebuffers and checks the adapter's actual `_dv_c` type against the runtime-assigned type before silence.
**[D]** Session 39860 failed dispatch verification (expected type 16, actual 0) despite zero unready peak. The saved
panel remained in the machine menu on MIDI: a fixed 500 ms wall wait raced queued keys. The selector now polls
`inputTap.release_at` and `status.elapsed` every 50 ms, bounded to 20 s, and fails clearly on missing clock fields.
It follows harness timings: 20 ms hold, 5 ms pre-scan/release, 200 ms open, 80 ms navigation, 300 ms YES. Parent's
known-panel NO → menu → row 5 → YES sequence confirmed SAMPLE PROOF controls. Failed timing evidence remains in
`out/runs/dsp-sdk-checks/sample-playback-dn2-input-timing.log`, artifact run `run-F9Pyr7`.
**[D]** Completed DN2 reruns confirm actual type 16 dispatch and exact-zero unready PCM. The subsequent
valid source load fails with -35 instead of READY. Artifact `run-15D8JZ/publication-state.json` records sender
`dl_link` state 3, error 100 (timeout), acked 0, and a zero/unpublished descriptor. The runtime log is
`out/runs/dsp-sdk-checks/sample-playback-dn2.log`. This is partial acceptance, not successful playback.

**[D]** A fast `dk:dev` reproduction reports `native-trap: no decoded instruction at 0x254aa1` on the first
TRIG 1, before sample transfer (`out/runs/dsp-sdk-checks/sample-note-dn2.log`). **[O]** The later link timeout
is likely downstream of the halted DSP; that inference does not establish a transport defect. The note fault's
call-frame failure was isolated by bounded live PCDUMP traces at the second `DK_CALL` delay slot: Type 3b `u=0`
pre-modify leaves I7 unchanged and writes zero at I6-8, while expected return slot I6-4 retains `0x254aa0`.
The epilogue loads that stale value into I12 and returns to `0x254aa1`. Public PRM pages 13-18/13-19 confirm
`u=1` is post-modify. Artifacts: `out/runs/dsp-sdk-checks/unready-module-trace-window/pcdump.csv` and
`out/runs/dsp-sdk-checks/unready-stack-alias-trace/stack-rows.json`.

**[C]** Earlier Python numerical/conformance passes did not validate this manual return slot: with an unknown
saved target, the abstract call-stack path returned through recorded call metadata. The native execution read
real stale RAM instead. The assembly source's PC-UREG store encoded pre-modify; changing that bit alone would
still rely on an unavailable PC value. No core semantics were changed for the correction.

**[D]** Within Em’s approved proof scope, the coordinator chose the explicit-label call: `DK_CALL(fn, return_label)` emits CJUMP, the saved-I6
push, and a Type 16a immediate post-modify push of `return_label - 1`, then defines that unique continuation.
All repository SDK callers now supply distinct labels; the unsafe one-argument form is rejected. Assembly/link
experiments proved repeated and nested calls with distinct ABS32/addend -1 return relocations. Numeric local
labels were rejected and `__COUNTER__` was unresolved, so no new preprocessing feature or dependency edit was
introduced. The cached fixture's store at byte 396 is now Type 16a and relocates to `_sp_after_linear - 1`.

**[D]** `test_dsp_call_frames.py` in `tests/patchkit/` checks actual assembled bytes with Python architectural stacks
and the current fingerprint-checked native core using runtime decoding. Poisoned return slots prevent unknown
fallbacks; repeated and nested calls on low-alias and L2-byte-alias stacks must reach actual continuation PCs,
write the expected frame/return slots, and restore I6/I7 exactly. The call-frame, generic-kernel, sample-kernel
and fixture-consumer suites passed **85 tests with no skips**. The fixture's actual `module:check` passed
**29 tests with five skips**; both device packs were refreshed, each needing 33,984 DSP bytes and fitting an
empty device. This does not prove coexistence or playback through either booted emulator.
Logs: `out/runs/dsp-sdk-checks/sdk-call-frame-tests.log`, `sdk-call-module-check.log`,
`sdk-call-module-build.log`, `sdk-call-pack-dn2.log` and `sdk-call-pack-dt2.log` in the same directory.
**[D]** Parent's corrected fast DN2 first-note `dk:dev` reproduction passes without a DSP halt (12.38 s total),
logged in `out/runs/dsp-sdk-checks/sample-note-fixed-dn2.log`. This bounds the first-note fault; it does not
complete transferred playback acceptance by itself.

**[D]** The subsequent full DN2 OS 1.12 scenario passes: one passed test and one DT2-filtered skip, 235.36 s
wall time and 228.197 s scenario time (`out/runs/dsp-sdk-checks/sample-playback-fixed-dn2.log`, session 7984).
It verifies an intact uploaded 9,000-frame source rejected with -13 and an unready descriptor/no fault, a clean
restart, actual type 16 dispatch with exact-zero unready PCM/no fault, and the uploaded 4,096-frame source
published as exact float32 bytes with READY=3. Replacement is refused with -10; playback and retrigger captures
are audible and finite, and no fault is reported. The native core executes the current DN2 1.12 image; the existing
historical DSP-core label/provenance caveat remains unchanged. Suite wall times are not loading-latency or
performance measurements. The passing `run-fMhyJt` temporary artifacts were removed by the test's normal
cleanup, so exact ready/retrigger peak values are not retained in this finding.

**[D]** Independent review found no concrete issues in the explicit-label macro and native return-frame
regression. The DT2 current-worktree daemon build completed in about 90 s
(session 47443), logged in `out/runs/dsp-sdk-checks/sample-dt2-daemon-build.log`. The both-device passes establish bounded emulator
acceptance only, not stock-quality equivalence, universal module coexistence, performance or hardware behavior.

**[D]** The first DT2 scenario attempt failed the type assertion (expected 16, observed 1), although its panel
showed SAMPLE PROOF and START/END/REV and its DSP stayed healthy. The assertion incorrectly used DN2's
layout: DT2 writes the dispatched type to `_dv_g + 4`, while `_dv_c` word 0 is its note-on flag; DN2 writes
type to `_dv_c` word 0. The test now resolves the appropriate symbol from the exact patch plan and records its
address while retaining the exact assigned-type assertion. No DSP/runtime code changed; the passing retry
resolves this diagnostic-layout failure.

**[D]** DT2 OS 1.17 retry passes: one passed test and one DN2-filtered skip, 94.25 s wall time and about 67 s
scenario time (`out/runs/dsp-sdk-checks/sample-playback-dt2-retry.log`). It verifies the same assertions as DN2:
9,000-frame oversize rejection (-13), zero descriptor/no fault, clean restart, actual type 16 dispatch, exact-zero
unready PCM and healthy DSP, exact 4,096-frame float32 bank bytes with READY=3, replacement refusal (-10),
audible finite playback/retrigger and no faults. Bounded emulator proof acceptance is complete on both current
OS images. These suite durations are not loading-latency or performance measurements. Use git log for the final
implementation checkpoint commit.

**[O]** Em requested focused stock DT2 OS 1.17 OneShot dispatch → note/render → sample access/ownership tracing.
Finding 104 covered metadata/pages; finding 15 concerns 1.16. **[D]** Current database candidates are render dispatch
`0x1c6b00` → `0x1c4ecf`, stretch branch `0x1c6af1` → `0x1c5576`, and Type 16a immediate post-modify return
stores as a comparator. These are database observations, not [V] byte audits or a completed ownership path.

**[D]** The focused current DT2 1.17 database pass identifies sample-pointer work at `0x1c4f15` and
`0x1c504a`, a 64-output loop at `0x1c5096`, six signed-short taps using a Q31 coefficient table at `0x25d940`,
and a subsequent `0xb80000` decimator from 64 to 32 samples. This is evidence that the stock reader's quality
path contains filtering beyond a plain linear read; it is not an exact kernel extraction, cost comparison or
output-parity proof. Reverse and boundary details remain largely historical finding 06 observations with mixed
marks, rather than a current-image runtime proof. Stock source ownership is separate from the new DN2 service
and cannot be transferred to it by copying the reader. Keep the first linear proof bounded while ownership and
any future quality extraction are investigated separately.

**[D]** The scenario now records daemon status, session faults and the PCM summary in `unready-state.json`
immediately after capture, then calls `expectNoFault('unready proof-machine note')` before valid loading.
`publication-state.json` also includes status and faults. Formatting and TypeScript checks pass; the full DN2
scenario above and the DT2 retry now exercise these assertions successfully.

**[D]** Partial `test:changed` passes Ruff, layercheck and the two selected Python files
`tests/devtools/test_changed_tests.py` and `tests/devtools/test_verify.py`. The ten known baseline stale
references still block repository pathcheck. Host checks were blocked by pnpm 12's automatic install attempt
and unavailable offline cache; no dependency changes were made. Local log:
`out/runs/dsp-sdk-checks/sample-test-changed.log`. The later owner host related-test command exits zero with
no selected host tests (`out/runs/dsp-sdk-checks/sample-host-related.log`); oxlint passes using existing Node
binaries. This does not establish a full gate pass.
The SDK-call changed check subsequently reported eleven stale references; splitting a temporary test path
into equivalent `join` calls removed the introduced source-reference false positive. Repository pathcheck now
confirms the ten known baseline references in five unchanged files, with changed source/documents clean:
`out/runs/dsp-sdk-checks/sdk-call-pathcheck-after-test-path.log`. No production behavior changed. The full routed gate has not run.

**[O]** Next proposed decision: production memory and lifetime for longer samples, safe replacement while
voices are active, and a sample picker. None is implemented. The current limit is 8,192 mono frames at 48 kHz
(about 171 ms), with session-pinned immutable source storage and fixed proof paths. Continue the OneShot
investigation separately. Hardware timing/coherence, arbitrary module coexistence, concurrent DT2 replacement,
stereo, longer files and product controls remain outside this evidence. No hardware test has run.

## Compiler-helper failure and retained diagnostics

**[D]** The selected boot initially failed at PC zero after stack corruption. The emitted weak compiler builtin
`__bswapsi2` called itself; `valid_bank`'s `from_le_bytes` reached it before module init. Return-address/argument
values matched corrupted registers, and the last recorded bus PC identified context restoration by RTE.
The build now links a strong private ISA_C BYTEREV cdecl helper before the builtin archive and hashes its source.
This fixes generated ColdFire code; no emulator instruction semantics changed.

**[D]** The actual cross-compiled endian regression in `packages/patchkit/tests/build.rs` runs five inputs on
`ColdFireCpu`, bounded to 100 instructions, checking reads/writes, cdecl stack/kept registers and private helpers
without outgoing relocations. It passed; independent review found no issues. MCF5441xRM printed page 97,
Table 3-4 confirms the operation, matching emulator behavior. This is not hardware proof.

**[D]** Retained developer diagnostics in `packages/emulator/coupled/src/cold.rs` write `boot-fault.json` with
the original error, CPU registers, at most 128 RAM-only stack bytes, last recorded bus PC and at most 32 RAM-only
code bytes. `packages/emulator/boot/src/runtime.rs` exposes that existing recorded PC; it is not a full trace.
Artifact-write failure preserves the original boot failure. Build sources: `rust.rs` and `cf_rust_helpers.s` in
`packages/patchkit/src/build/`. Local helper build log: `out/runs/dsp-sdk-checks/cf-helper-runtime-build.log`.
