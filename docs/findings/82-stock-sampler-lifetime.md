# Stock OneShot selection and replacement observations

2026-10-29. DT2 OS 1.17 only. **[D]** means an executed observation or source read; **[O]** means open.
This follows [the dependency map](81-stock-sampler-dependencies.md). No new [V], hardware or performance claim.
The existing immutable-bank proof on both devices remains valid; this does not complete a borrowed-view API.

## Scope and reproduction

The focused TypeScript discovery is `stock-sampler-lifetime.test.ts` under `tests/regression/runtime/`.
It uses existing session, scheduled input, stock Transfer RPC, read-only CF/DSP peek and PCM APIs. It retains
artifacts even on success under `out/runs/stock-sampler-lifetime/dt2-*`. It performs no firmware-state pokes,
SDK/runtime changes, dependency changes or core regeneration. UI frames establish OneShot selection.

The deterministic mono 48 kHz PCM16 files are ten seconds each: 330 Hz `a`, 990 Hz `b`. Both are uploaded to
`/stockproof` and independently verified through the firmware's content hash. The scenario distinguishes a
selected-slot change from REPLACE of the same project slot. Equal-length files cannot establish replacement
by a length/READY check alone: readiness requires known header bytes, 960,000-byte PCM length and exact B
source **head** (the first 64 PCM bytes) at the newly resolved slot address, independently of any retained voice
pointer. This identifies the source and checks its published length; it does not compare the entire 960,000-byte
DSP payload. Transfer verification separately covers the file stored on +Drive.

Run one named file with the preflighted current-image daemon, under the shared CPU-budget lock:

```sh
DIGI_EMUD_DT2=out/native/target-wt/digikit-dsp-sampler/g-e7aa50f9/release/digi-emud \
uv run python -m tools.devtools.benchlock run --exclusive --label stock-sampler-lifetime -- \
node node_modules/vitest/vitest.mjs run --config tests/regression/vitest.config.ts \
  --project runtime tests/regression/runtime/stock-sampler-lifetime.test.ts --maxWorkers 1
```

Use the worktree-local UV/cache environment and current `out/ready-pk-dt2-117` inputs described in the plan.
The scenario asserts the original stock input hash and `injected=false`, with no runtime identity. No daemon
or module is built by this command. Test wall durations are not device loading latency or throughput.

## Patched-image discovery: executed counterexample to assuming retirement

**[D]** The initial kept-session observation used stock OneShot **inside the emulator patched image**, not
unmodified firmware. Base SHA-256 was `26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5`;
loaded image SHA-256 was `7733711cbee629130eb204f1528fda1dd8eed27e8642f39b16f61db3fb30e50a`, with
`injected=true`. The patch closure includes runtime, machines/params, input/UI, DSP-link and DSP-voice hooks;
therefore these observations alone do not establish unmodified-stock behavior. There were no dynamic modules.

Retained evidence: `out/runs/stock-sampler-lifetime/kept-ui/`, including the exact regenerated image/plan in
`provenance/`, `reload-tool-report.json` (module closure, daemon path, guard report, status), Transfer verification,
UI PNGs, `a-active-before-navigation.json`, `active-a-to-b-replace.json`, `after-b-settle.json`, and the continuous
`a-active-and-replace.wav`. The regenerated image hash matches the loaded identity above.

| Stage [D] | Observed state |
| --- | --- |
| Selection only | Stored voice binding changed `0x8422ba08` → `0x84243a08`, matching slot 1's `0x18040` offset plus `0x8422b9c8`. CF sample allocations stayed present. Post-note binding alone does not prove active reading. |
| First REPLACE: shared stock file → A001/a | CF block index 3, observed reference-count candidate 1, DSP metadata PCM length 960,000 bytes. The previous shared block's candidate count fell from 5 to 4; other references retained it. |
| Actual a note | Both voice pointers were `0x84273a08`, active byte 1. Raw cursor whole fields advanced 576 → 2976 over a stepped 100 ms. The track/source binding was explicitly restored after leaving the assignment browser. |
| Before unique a → b commit | Same pointer, active 1, raw cursor whole `0x110b0`, block index 3 and candidate reference count 1. Known 64-byte source head matched a. |
| 25 ms after commit | Same pointer, active 1, cursor `0x11310`; known 64-byte source-head bytes matched b, while slot metadata was cleared/unready. |
| Another 100 ms | Same pointer, active 1, cursor `0x11c70`; the 64-byte source head still matched b and slot metadata was still unready. |
| Settled / new trigger | Slot metadata returned with 960,000-byte length. A new trigger had active/advancing state and finite PCM; no DSP halt was reported. |

The block record/hash changed with the replacement while the observed index/extent was reused. Coarse snapshots
did not capture a transient zero reference count or prove the exact free/coalesce ordering. The retained active
address, advancing cursor and changed backing bytes reject the assumption that slot-header invalidation alone
retires the old reader. They do not establish a universal schedule, race freedom or hardware behavior.

## Unpatched reproduction status

**[D]** Initial stock-only attempts confirmed the original image hash with `injected=false`, selected-slot binding,
verified Transfer and loading a ten-second sample. They failed before completing the B replacement because
assignment/browser navigation was assumed incorrectly. One failure also exposed the insufficient same-length
readiness predicate; the scenario now checks actual B bytes at the slot-derived address.

**[D]** The corrected stock-only retry **passes** (one test, 78.96 s suite wall time):
`out/runs/stock-sampler-lifetime/unpatched-binding-retry.log`; retained records are in
`out/runs/stock-sampler-lifetime/dt2-iXElPL/`. The image is the original stock SHA-256 above, with
`injected=false` and no runtime identity. The existing current-image daemon runs the native core; no core or
daemon build was performed. Restoring the track/SRC selection after assignment avoids triggering in an inherited
browser context. The bounded root-navigation guard uses this run's observed SAMPLES title rather than blind NO
counts. The earlier failed attempts are retained as control/readiness corrections, not firmware failures.

The successful trial records:

| Snapshot [D] | Stored pointer | Active candidate | Cursor whole (raw units) | Known source / slot metadata |
| --- | --- | ---: | ---: | --- |
| Before b commit | `0x84273a08` | 1 | 160464 | Exact a head (64 bytes); slot length 960000 |
| After commit | `0x84273a08` | 1 | 162352 | Exact b head (64 bytes); slot cleared/unready |
| After a further scheduled ≥100 ms and RPC work | `0x84273a08` | 1 | 165936 | Exact b head; slot ready again |
| B ready capture | `0x84273a08` | 1 | 167024 | Exact b head (64 bytes) and known ready slot view |

All source-head observations have zero unknown bytes. Both observed voices have active candidate 1 and the
listed progressing cursor fields. The controlled a block's reference-count candidate was 1
before commit; the reused block index stayed 3 while its raw record hash changed `0x97bc4ac5` → `0xbee98893`
at the same observed extent. The continuous
capture `active-a-to-b.wav`, raw `replacement-observation.json`, separately resolved `b-ready-slot.json` and new
trigger records are retained. A new trigger binds the published B slot and yields finite, nonzero PCM (recorded
peak 0.026728345081210136). No fault was reported. `executed.json` explicitly leaves unload, project reload,
start/end/reverse and reader-quiescence proof false.

An independent artifact audit supports these narrow facts and provenance. **[D]** Free-running reads are
sequential: voice capture precedes the source-head read by about 34 ms in the audited case. Thus they do not
prove uninterrupted voice activity, the exact address being read at each instant, exact read/write ordering or
exact 25/100 ms transition timing. The paused kept-session captures have the same elapsed clock within each
snapshot, but retain their patched-image qualification.

This unpatched current-image observation independently removes the patched-image qualification from this
particular observed transition. It still does not prove every schedule, exact read-versus-write interleaving,
complete allocator behavior or a hardware outcome. No stock bug or whole-lifecycle completion is claimed.

## Decision and remaining work

**[O]** A future borrowed-view SDK needs explicit consumer retirement **before source overwrite or release**.
A generation guard must also reject stale queued notes. A DT2 adapter must account for externally initiated
slot replacement and project reset/reload, not merely changes through our own picker. Do not publish a borrowed
view API until the required hooks and barrier are proven. Clearing READY or acknowledging a transport write
is insufficient. Keep the immutable-bank proof available while this protocol is designed.

The independently written shared packed-PCM16 reader/cursor/region contract remains a separate proposal.
DN2 already has a working playback seam and private source acquisition; its larger backing/delivery ownership
contract remains unresolved. This result does not restart DN2 playback feasibility or imply DT2 pool ownership
transfers to DN2.

- [x] Establish current-image OneShot UI, selected-slot binding, distinct Transfer fixtures and a ten-second loaded view.
- [x] Record a bounded active a → b replacement observation in the identified patched image.
- [x] Complete the stock-only critical replacement reproduction; retain original-image identity and exact B-head readiness evidence.
- [ ] Prove reader retirement and stale-note rejection before designing an exposed borrowed-view API.
- [ ] Capture removal/unload and project reset/reload, including external operations and extent reuse.
- [ ] Prove start/end/reverse mappings and PCM boundaries. Single-event knob attempts left UI and raw fields unchanged;
  they are not coverage. Existing burst-input helpers can support a later bounded attempt.

No whole-lifecycle completion, stock-quality equivalence, pool/coexistence guarantee or hardware claim follows.

## Validation and limits of the checks

The focused original-image scenario passes one test. Direct Prettier, oxlint and the regression TypeScript
project check pass. Scoped pathcheck and `git diff --check` pass. `test:changed` reports the same ten baseline
stale references in five unchanged files; its host-related/lint/format/types commands are additionally blocked
by pnpm attempting dependency installation into the shared hoist-directory symlink, despite the package-manager
version-management opt-out. No manifest or lockfile changed. Direct existing-Node fallback checks complete the blocked preflight
validation equivalents: related host Vitest exits zero with no related host tests selected; TypeScript 5.9.3 checks
pass for control-client, dk-session, test-support, desktop UI/tests and regression using the existing Node types.
Direct oxlint and Prettier checks also pass. The aggregate command still fails on the ten baseline references
and pnpm setup, rather than a failing test. The full gate has not run; parent owns final verification and publication.
