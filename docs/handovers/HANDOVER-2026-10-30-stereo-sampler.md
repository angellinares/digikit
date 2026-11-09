# Stereo sampler: numerical routing and memory diagnostics

Primary worktree: `/private/tmp/digikit-dsp-sampler`, sampler discovery branch.
Current checkpoints include the private combined observer, read-only store inventory
and fail-closed runtime cleanup (integration/publication is coordinator-owned).
The ongoing source of truth is
[the plan](../plans/dsp-sdk-and-sampler.md); this handover is a short resume guide.
Other sessions may own the original checkout. Do not reset it or stop their runs.

## Constraints and next decisions

DT2 1.17 and DN2 1.12 remain the analysis/acceptance targets. The user requires
full stereo, sixteen tracks sharing deduplicated project backing, long samples,
and no arbitrary ten-second or 16 MiB product limit. Selecting a sample stops
old notes on that track. Modules use shared SDK contracts; device adapters ship
with the runtime. Existing module cleanup in other sessions must be preserved.

No production sample reservation, public stereo machine ABI, hardware flashing,
PCM pin change, merge or Python instruction-core regeneration is approved by
these diagnostic checkpoints. DN2 configured 512 MiB geometry is not a free-pool
proof. Bounded quiet observations do not establish ownership.

The bounded stock DT2 paired-input pan audit agrees with diagonal left-A/right-B
scaling; it does not prove UI or whole-pipeline equivalence. The local
DN2 reset/helper map and bounded reset replay now pass. Next resolve production
capture/commit stability, cleanup and route integration. The immediate AudioTask
caller chain has no local exclusion proof; do not assume it serializes all writers. Memory work must establish pre-reclaim reader retirement and
coexistence, not just search for an interval that appears quiet. DT2 release/reset
can reclaim before header clear; existing findings 81/82 already demonstrate why
header invalidation alone is inadequate.

## Completed and independently reviewed

- [Finding 96](../findings/96-stereo-machine-processing.md): all ten identified
  post-source families have bounded numerical examples, plus the source
  continuation. The selected composed route adds original scratch blending and
  output splitting. All 44 named replay tests pass without skips, including actual reset helpers. The candidate
  output mapping remains a bounded synthetic route, not full stock integration.
- Optional `core/dsp-stereo-proof`, DN2 1.12 only: 39 model cases and both native
  identity/polarity modes pass. Each native mode records 2,400 render/shadow calls,
  one mirrored reset and zero mode errors/nonfinite observations/mismatches/fallbacks.
  Mode 1 observes 76,799 nonzero opposite-sign audio and 4,800 opposite-sign live
  history relations, with numeric MODE1 `0x39011cf8`. Both stock/patched 100 ms,
  4,800-frame stereo WAVs retain identical hashes and zero shift. This is finite
  metamorphic independence evidence, not an independent numerical oracle.
  Original left state/output remain authoritative; private L2 data occupies
  4,020 bytes, with no DDR reservation or default activation.
  U/Y caller/context audit is complete: both early/late calls share the same
  per-slot record/history and require both hooks plus the local reset. A bounded
  mode-0 interpreter profile positively observes early (2,400) and reset (one),
  with 2,850 helper entries across all callers, zero AOT/fast/traps and unchanged
  stock WAV. Late is unexercised here; gate stability is unproved. Implementation
  approach is now approved and implemented with persistent right histories across
  placements. Both owner model files pass 75 cases (39 first-stage +36 U/Y);
  combined two-mode native acceptance passes. Each stage records 2,400 calls and
  one reset mirror per mode, all errors zero; mode 1 observes 76,799 opposite audio
  and 4,800 opposite history relations at each stage. All stereo WAVs retain the
  stock hash, 4,800 frames/100 ms and zero shift. The appended source preserves the
  first-stage source/instruction prefix. Combined private data is 6,760 bytes,
  code 5,830 bytes/1,375 decoded instructions; the exercised U/Y model depth is
  176 bytes. Minimum native entry minus that model depth leaves 3,128 bytes in
  this scenario, not a native store watermark or universal stack bound. Both-stage
  native gates require zero errors/nonfinite/mismatches/fallbacks, U/Y reset and
  mode-1 opposite audio/history. The later held-note BW.RT route profile now positively covers both placements.
  Full-route, production backing ownership and hardware acceptance remain open.
- [Finding 97](../findings/97-ddr-access-diagnostic.md): opt-in first-attempt DDR
  monitor, compiled native guard fallback, ordered diagnostic RPC, both native
  device acceptance cases, and 49 focused Wasm/V8 cases from real lowering.
  Wasm uses a checked simulated wasm32 layout; full browser-engine fallback/RPC
  and hardware behavior are not proved.
- Three flat-access tests became four with the Wasm export fixture. Native
  Cranelift guard-block ordering was fixed separately at `aed78721`.
- The RPC daemon is immutable at
  `out/runs/ddr-monitor-rpc/native-build/stable/digi-emud-6850e27b98d9`.
  Both current firmware inputs are hash-bound in the acceptance artifacts;
  legacy generated profile labels do not identify firmware versions.

RECIPS and RSQRTS use the measured seed tables of the core (finding 102), so the
Python-model routines that divide are exact. The sampler branch changed no
instruction semantics.

## Validation and resume commands

Use the repository's shared benchlock at the original checkout's `out/.benchlock`
for native builds/runs; never overlap a gate with another build. Owner commands
and exact environments/logs are retained under `out/runs/ddr-monitor-rpc/`,
`out/runs/ddr-monitor-native/`, `out/runs/ddr-monitor-wasm/` and
`out/runs/dsp-stereo-proof/commands.json`. The last records the exact exclusive
benchlock/native invocation and immutable daemon/patchkit hashes. Current results
are `out/runs/dsp-stereo-proof/native/4c8488cb-3275-4c6e-a6f6-0a9dccef49d2/result.json`
and `out/runs/dsp-stereo-proof/native/cefabd29-f00f-413c-a368-f8db89f1b179/result.json`;
`out/runs/dsp-stereo-proof/native-polarity-acceptance.log` records both passes.
The earlier identity-only run remains separately recorded in provenance.
U/Y coverage has its own `uy_coverage` provenance entry, profile pair
`out/runs/dsp-stereo-proof/uy-interpreter-profile.e45298-1` and
`out/runs/dsp-stereo-proof/uy-interpreter-profile.e45298-2`, log
`out/runs/dsp-stereo-proof/uy-interpreter-profile.log` and result
`out/runs/dsp-stereo-proof/native/fd1c110f-a444-4e5f-ad50-42d2b4a4d11d/result.json`.
That run intentionally filters mode 1; no absence claim follows from fast-profile
missing call rows or this scenario's missing late row.
Combined observer acceptance is in
`out/runs/dsp-stereo-proof/native/5e1a8014-6b17-47b1-b3c6-c1d26d167f99/result.json`
and `out/runs/dsp-stereo-proof/native/a49fdec1-2e03-42a1-a0d0-58989c6c9ede/result.json`,
with `out/runs/dsp-stereo-proof/native-uy-acceptance.log`. Late-site positive
coverage was open for those Pre-only runs; the later panel-switch profile below closes that scenario gap.

The private per-stereo-frame fetch experiment now passes 59 model tests, with no
runtime/core policy changes: both channels' packed taps/scalars are copied in one
admitted masked scope, count is released, and private compute/store/one cursor
advance follows exact mask restore. Denial zeros the remaining block without epoch
relatch. Actual DDR aliases and stock page geometry replace represented overlap
spans for this proof; the original DT2 guard helper supplies exact ten-byte stores.
All representative arrivals reduce to checked fetched-frame classes; a split-lock
control tears a real frame. Reader masked spans 80/81 and totals 5,172/5,204 are
model instructions only. Cursor/scratch must remain callback-owned after backing
count zero; production lifetime, IRQ/cache/coexistence and ownership remain open.
See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md) and ignored
`out/runs/stereo-frame-fetch/commands.json` for scope, commands and evidence.

**[C][D]** AudioTask requests 1,000 four-byte units, not `0x1000` bytes:
`0x1ca009` loads `0x3e8`; creator shift `0xb88840` precedes allocation at
`0xb88853`. For `T=DM32(0x268a84)`, `S=DM32(T+0x30)`, allocation is
`[S,S+4000)`. Current combined native entry minimum minus the maximum
model-exercised 176-byte store depth leaves 3,128 bytes in this scenario.
Historical first-stage-only 172/3,132 observations remain in the findings. This is not a native store
watermark or worst-case interrupt/scheduler headroom. See
[finding 83](../findings/83-sample-retirement-design.md).

The normal changed-files wrapper encounters the existing worktree pnpm
hoist/bootstrap layout for JS steps. Scoped installed-tool lint, formatting,
related-host checks and regression TypeScript checks passed. Python/docs scoped
changed-file checks, pathcheck and layercheck pass. Full workspace types and the
pre-merge routed gate are not claimed. No dependencies were changed to bypass it.

Numerical tests use immutable patchkit
`out/native/target-wt/digikit-dsp-sampler/nogen/stable/patchkit-344b899674ac`
through `tools.patchkit.patchkit._BUILT`, then run the named file
`tests/patchkit/test_dsp_stereo_shadow_replay.py`. This avoids rebuilding patchkit.
The original firmware bytes load at runtime from ignored section artifacts;
no firmware or sample audio is committed.

The secondary `/private/tmp/digikit-sampler-gain-proof` worktree retains uncommitted
proof files developed from an older base. All current additions were integrated
and checked in primary. Do not apply its whole diff again or discard it blindly.
Use only configured agents with fresh context, read-only reviewers, one writer
per worktree, and root-owned decisions/integration/publication.


The raw containing-word fetch is now reusable as the eight-instruction
`DK_SAMPLE_PCM16_FETCH_WORDS` macro in `sdk/dsp/sample_pcm16_fetch.inc`; only the
private frame fixture is refactored to use it. The SDK documents scalar/linear,
nonwrapping full-word and caller-protection preconditions. Existing 58 proof
cases retain their metrics, plus a focused shared-index-register/final-DDR-word
edge case. Old PCM kernels, public resource/output ABI and production exclusion
policy are unchanged. See [the SDK contract](../../sdk/dsp/README.md) and
[finding 99](../findings/99-stereo-frame-fetch-exclusion.md).


The current native test defines Pre and held-note BW.RT route-switch variants in both
modes; all four pass. The normal panel path is FLTR twice to 2/2 and calibrated ENC G
events (+1,+8,+32 at 200/300/400 ms), with visible PRE/POST FLTR pixel readback and all
sixteen byte gates checked at 390/500 ms. The existing automatic NO dismissal is
recorded, TRIG 1 stays held, and initialized nonzero private histories retain one reset
count through the switch. No routing-memory control write is used. Each route variant
preserves the exact stock 800 ms stereo WAV (38,400 frames, zero shift), with 19,200
calls per stage and zero errors. Mode 1 observes 614,399 opposite audio and 38,400
opposite history relations per stage.

The paired interpreter profile positively records early/late/reset counts 10,160/9,040/1
with MODE1 `0x39011cf8`, zero AOT/fast/block execution and zero traps. Late U/Y
execution is now observed in this held-note scenario; full-route lifecycle, writer
exclusion and production backing/lifetime remain open. New evidence is
`out/runs/dsp-stereo-proof/native-route-acceptance.log`,
`out/runs/dsp-stereo-proof/route-interpreter-profile.log` and `route_extension` in
`out/runs/dsp-stereo-proof/commands.json`; [finding
96](../findings/96-stereo-machine-processing.md) lists exact result/profile paths.
TypeScript, oxlint and formatting checks pass; this extension changes only tests/docs,
with no DSP/SDK/ABI change.


For this four-file extension, the coordinator's changed-files wrapper passes
pathcheck/layers but its JS wrappers hit the existing pnpm hoist layout
(`node_modules/.pnpm` is not a real directory). Direct regression TypeScript,
oxlint and formatting pass; the direct related-host selection finds zero test
files, which is not host coverage. No dependency changes or full-gate pass are
claimed. Next establish adapter render-return/state-reuse lifetime independently
of backing-reader count before production integration.

Callback-lifetime audit now distinguishes module return from adapter/output
completion; backing count zero authorizes neither private-state nor output reuse.
Next audit selection/row reuse with boot-pinned callback storage and binding
updates at the existing audio callback boundary. This is an open proposal, not a
non-reentrancy fix; optional bounded tracing cannot establish scheduler-wide safety.
See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md).


Failed DSP-load cleanup now fails closed: uncertain row invalidation or CF drain
retains module/DSP/CF memory, publication ownership and queued transfer sources in
sticky internal retained state until reboot. Ten actual-runtime host tests pass;
only firmware/link/info/IRQ and host-incompatible CF pointer side effects are
substituted. The fresh runtime ColdFire object builds/re-decodes and is current.
DT2/DN2 pluck artifacts are under `out/runs/dsp-cleanup-retention/modules/` for the
coordinator's `tests/regression/modules/pluck.test.ts` normal-loader acceptance.
Commands/provenance are in `out/runs/dsp-cleanup-retention/commands.json`.
Host cleanup evidence does not prove physical DSP ACK/block-tail quiescence or
runtime non-reentrancy; those lifetime assumptions remain separate.

For this runtime delta, `mise run test:changed` passes path/layer checks; its JS
host/format/types wrappers stop at the existing `node_modules/.pnpm` hoist layout
problem. Python checks are skipped because no Python source changed. The named
Rust cleanup tests and runtime object source-current check pass independently.

The nested standalone `dkrt` crate now participates in the existing `rust-tests`
job after selected workspace tests. Its runtime sources and local dependencies
select it; default/whole runs include it. The direct owner is
`mise run test:runtime:unit` (shared lock, pinned host toolchain, worktree-aware
`nogen/host-dkrt` target, no generated core or native fixtures). The underlying
explicit check remains:

```sh
cargo test --manifest-path modules/core/runtime/src/rs/Cargo.toml \
  --target-dir out/native/target/dkrt-cleanup-host \
  --lib cleanup_tests -- --test-threads=1
```

under the shared benchlock/environment recorded in `commands.json` above.
Routing/execution unit checks cover standalone-only, mixed/default, empty/desktop
selection and failures. Runtime sources and the compiled object are unchanged by
this routing delta; no native run or full gate was repeated.

Coordinator normal-loader acceptance passes both DT2 1.17 and DN2 1.12 cases
(235.08 s total; 80,037/154,845 ms respectively), using the fresh runtime object
and dedicated pluck packs. This covers upload/persist/select/reboot, `MOD_DSP0`,
nonzero DSP extent and voice row/type, with no fault; it is neither audio correctness
nor native failure injection. Exact invocation/environment and whole-file hashes
are in `out/runs/dsp-cleanup-retention/commands.json`, with
`out/runs/dsp-cleanup-retention/native-pluck-acceptance.log` recording exit zero.

The routing delta passes 252 named Python checks, ten direct runtime host tests,
and `mise run test:changed`. The explain-only gate plan retains the existing single
Rust job. Commands/results are under `routing` in the cleanup provenance JSON;
no full gate or native acceptance is repeated for this tooling-only change.

DT2 source consistency now has a separate filesystem subsection in
[finding 81](../findings/81-stock-sampler-dependencies.md). The original 1.17 audit
finds per-read lookup/locking and a nonexclusive open bit, plus a bounded RPC
writer path that can ignore busy deletion before opening the existing ID. This
establishes no immutable source lease; it is not an observed race or a complete
outer-admission audit. Next inspect mutation-under-mutex seams for partial-load
invalidation, without selecting hooks, ABI or production policy.

The new read-only `fw stores` command inventories recorded SHARC store operands
through explicit-root CALL-only reach, retaining interior targets and unresolved
edge/pointer evidence. The DN2 1.12 explicit-blob report is in
`out/runs/dn2-store-inventory/report.json`: root `0x1c9fe7` normalizes to containing
function `0x1c9e76`, with 93 functions/2,278 operands, including 1,002 stack
candidates; 23 CALL and 16 indirect rows remain unresolved. No runtime bound,
decoder completeness, alias footprint or backing ownership follows. Command and
provenance semantics are in [Instrumentation](../INSTRUMENTATION.md); coordinator
review will select the next named gaps rather than treating prior counts as goldens.

The original-byte root audit now confirms actual AudioTask entry `0x1c9fe7`
through task creation/initial context. Database owner `[0x1c9e76,0x1ca000)` merges
adjacent dispatch code, so whole-function reach overapproximates around that entry;
unfollowed edges also leave reach incomplete. All 23 missing-target-function CALL
rows have concrete destinations (22 unique containing ranges, one unowned).
Final tool review closed the entry-identity P2. The bounded `0x1c125e` helper
body audit is now complete: no stores/further calls in its followed ranges, with
adjacent entry/other callers excluded. The subsequent R4 audit closes caller
`0xb80e54`'s `DM(I5+2)` destination as voice-state word 2, at
`0x2535e8 + 0x48*v` for valid slots 0..15 (`0x48` bytes = 72 decimal).
It does not dereference the separate word-16 external ring pointer. External ring
ownership, corrupted state and entry/dispatch precision remain open, with no
policy expansion or reservation conclusion. Auxiliary `0x1ca59f` setup now
resolves SPI1 DMA24 TX/DMA25 RX fixed circular buffers and two-byte count/modify
configuration. Valid nonzero counts bound configured spans only; XCNT=0 is a
configuration error, not an empty/inactive transfer. The completed bounded static
lifecycle audit links the published auxiliary handle to callback shutdown/control,
but not actual creator entry/N, a valid external trigger or complete error cleanup.
Main SPI2's R4=`0xabc` is separate and must not be used as auxiliary N. Next resolve
external/computed creator entry or exact-entry R4 observation capability; negative
reference searches do not prove dead code or every-boot/perpetual activity. See [finding 106](../findings/106-dn2-ddr-configuration.md).

The canonical packed-PCM16 parser already exists, but the current source/job
contract is LE float32. Direct substitution would violate the consumer contract.
Production wiring awaits explicit representation/destination byte extents and
source consistency; no integration or public ABI is selected.

The bounded DT2 reserve/alternate-reclaim audit now closes those two mutation
families: write/delete-only hooks miss reservation and recursive/rollback reclaim,
and delete's single argument does not cover every affected child/parent ID.
File identity exists before the first mutation but is absent from lower bitmap
helpers. Create/ID reuse, reset/remount and remaining writer coverage are the
concrete residual work. This is CF source consistency, not DSP pool ownership;
no hooks or selected-file generation implementation are chosen. See
[finding 81](../findings/81-stock-sampler-dependencies.md).

The DT2 create/ID-reuse slice is now audited: identity precedes bitmap/node writes,
all four recorded allocator calls hold the FS mutex, and failed linking can free
an ID without delete/reclaim wrappers. Existing incarnation-like node metadata
has no audited wrap rejection. Directory links, reset/remount and I/O completion
versus queued work remain the concrete next checks, with no hook/public API or
complete generation scheme chosen. See
[finding 81](../findings/81-stock-sampler-dependencies.md).

Bounded DT2 format/mount/init auditing now identifies separate global boundaries:
format checks open bits, mount does not locally do so, and init resets the mutex
without locking it. Shared init is too late for sole invalidation; mounted state
is not a generation. Successful block read/write paths wait for three events before
completion, with no deferred flush in the checked cache paths. Early errors skip
waits and FS wrappers ignore block status; the bounded error follow-up below
finds no local abort/drain in the audited arms. Init/indirect triggers, directory links, balanced open ownership and full
source consistency remain open; no hooks, public API or backing policy selected.
See [finding 81](../findings/81-stock-sampler-dependencies.md).

The bounded DT2 error ISR audit is complete: event signaling and channel-59
interrupt clear do not establish stopping/draining; command errors and write data
errors can return before the DMA event. Block errors are observable but ignored
by FS wrappers, and error globals are not sticky. The completed pointer audit
below resolves full-block DMA storage as stock cache/bounce, not caller input;
no late write was observed.
Controller-state proof or conservative uncertain-completion handling remains open,
with no hook/public API/backing policy selected. See
[finding 81](../findings/81-stock-sampler-dependencies.md).

The final read-pointer audit closes direct runtime-input DMA exposure for this
filesystem route: stock cache/bounce receives DMA, then clamped chunks are copied
to the caller, including full-block requests. The final one-byte probe uses the
same reader but returns before cache/copy at EOF. No after-free write was observed,
and this path alone does not justify retaining the runtime input buffer. Ignored
block errors can instead yield stale/incomplete/potentially changing cache copies
with filesystem success counts. Source validation therefore needs lifecycle/
content-change evidence plus an independently latched low-level storage fault;
counts/native headers alone are insufficient. This is a design requirement, not
selected hooks or public ABI. Recovery mechanisms/full coverage remain open;
uncertain completion is not assumed retry-safe. See
[finding 81](../findings/81-stock-sampler-dependencies.md).

A proposed storage-fault record must not reset on a new load, and its installation
must cover relevant cache I/O history or establish fresh/quiescent initialization.
A new clear token cannot prove health after an unobserved failure. Implementation
mechanisms remain open; the user has accepted the first-release policy below.

Accepted first-release recovery policy: missing, invalid or changed samples show
an error, discard the unpublished load and leave the selected track silent; another
selection is allowed. An uncertain low-level storage fault with unproved DMA
completion blocks new sample loads until reboot or independently proven recovery.
The loader leaves other published bindings alone, without guaranteeing underlying
device health. No automatic retry/recovery is required for the first release.
Next technical work is fault capture/history before cache use plus a lifecycle/
content-change source witness and complete hook coverage. The policy is accepted,
not implemented by this documentation update, and proves neither DMA stopping nor
production pool readiness. Future automatic retry requires a separate abort/drain,
cache-invalidation, FS-revalidation and bounded error-injection scope. No new
reservation or public ABI is approved.

The private optional DT2 storage-fault observer now has **[D]** evidence: 39
assembled instructions, three passing compiled-source model tests, actual-image
segment/BSS/loader verification, five passing host watch-validator tests and a
completed independent hook/model review. It is shared runtime storage
infrastructure, with the sampler as first consumer and reuse intended for the
file manager/importer; it is not a production gate or DN2 implementation proof.

Native run
`out/runs/storage-fault-proof/native/40cd90b5-0945-43af-9223-6626e5945f16`
failed because the second same-image load returned `main_changed: false`. The
assertion now expects `boot === 1` and has not been rerun; cold/cache/hash and
observer assertions remain unchanged. Both traces show loader zeroing before FS
initialization; the first readiness latch was zero, and host seed `0x00310000`
survived one million ColdFire steps. Zero observer writes means IRQ capture is
still unproved. The native runtime uses `CompletionPolicy::Oracle` and board
`complete()` directly posts DMA/data/command semaphores and clears status;
`drain_board()` counts events. Default native completion does not establish stock
ISR execution. The full native fault-history acceptance remains **failed/open**
because zero observer writes were observed.

The opt-in [controlled ISR fixture](../../packages/patchkit/tests/storage_fault_isr.rs)
now passes 36 sequence/SR combinations: eleven single-interrupt cases and one
five-interrupt sequence at three SR values, giving 48 interrupt invocations per
image (stock and patched). It loads hash-bound stock DT2 1.17 at runtime and
executes the actual ISR, real no-waiter signal helpers and RTE, with assembled
hooks installed only in memory after verifying the original sites. Synthetic
vector/level injection and a controlled W1C IRQSTAT double preserve matching
context, exact MMIO/nonobserver write traces and status/signal globals; sticky
union includes simultaneous error/completion. The
[adapter README](../../modules/core/adapters/storage-fault-proof/README.md) gives
the `DIGI_STORAGE_FAULT_ISR_SYX` opt-in command. Missing input skips only when the
environment variable is unset; a configured invalid input fails.

That earlier **[D]** no-waiter component result does not establish controller
delivery, DMA completion, waiter/scheduler behavior or native-boot IRQ validation;
the later controlled delivery/waiter result is recorded below. Fault history,
whole-bootstrap/reset coverage and source witnesses remain open. Keep native
assertions intact; no emulator semantics change or wholesale Device-mode switch
is justified by this result. The earlier all-changed
`test:changed` wrapper failed on pnpm dependency layout; the later scoped check
for the ISR fixture and four documentation files passed pathcheck/layers. The
full merge gate was deferred and has not run. These observations remain **[D]**, not **[V]**.
See the [next proof](../plans/dsp-sdk-and-sampler.md#next-proof-private-early-dt2-storage-fault-observer)
and [finding 81](../findings/81-stock-sampler-dependencies.md).

**[D] Default runtime boundary:** ordinary `Machine::step` retains its existing
configured DMA59 route. Oracle Board completion posts semaphores and the daemon
counts events; full native fault-history acceptance remains **failed/open**.
The explicit opt-in storage-pair proof below does not change default runtime behavior.

**[D/O] User-approved Stage A is partially implemented:**
[finding 107](../findings/107-storage-interrupt-emulation-scope.md) records the
bounded Device component and remaining gates. Immediate host events accept only
`0x00310033`; raw completion/error bits latch independently through IRQSTATEN,
with enable-clear effects, W1C and a separate IRQSIGEN gate on the derived shared
line. Device commands emit CC and data readiness, with explicit TC rather than
TC on every command. Contained big-endian 1/2/4-byte writes have these semantics;
crossing writes are rejected. Oracle behavior and snapshot layout are preserved
by the code and component tests, with no queued fault state. This is not a full
default native regression or controller model boot proof.

The 21 targeted eSDHC manual/event/DMA tests passed; the constructor reset-value
case alone was rerun after extending it to both policies and passed. Independent
static review reported no issues. The coordinator's scoped `mise run test:changed`
for the three Rust and three documentation files passed pathcheck/layers; the full
merge gate was deferred and has not run. Only the documented IRQSTAT effects of
individual RSTC/RSTD are implemented. Full RSTA remains unresolved: manual p0637
explicitly includes RW in the cleared types, while the generic IRQSTATEN reset
value is `0x117f013f`; no post-RSTA enable state is guessed. RSTA, including
combined reset writes, retains legacy self-clear behavior unchanged and is not a
hardware-faithful full reset.

**[D/O] Reset investigation checkpoint (2026-10-31):** full setup and immediate
command helpers on DT2 1.17 / DN2 1.12 show IRQSIGEN (`0xfc0cc038`) OR
`0x00310003`, with no explicit IRQSTATEN initialization in the audited paths.
The expanded DT2 MAIN raw scan `0x40000400..0x4031ff60` decoded 1,007,317
instructions at 96.81% coverage, with zero absolute references to IRQSTATEN
`0xfc0cc034`. Computed/indirect accesses remain outside the scan; this is not
absence proof. Finding 107 records the exact helper addresses and source limits.

The public errata's SECF215 RSTA clock glitch/workaround and SECF218 SDIO entry
do not resolve enable reset values in the bounded search. Linux's ColdFire reset
path explicitly rewrites both enables after reset; the audited U-Boot path
suggests reliance on nonzero status enables but cannot distinguish preservation
from reset defaults. No trusted hardware readback was found. See
[finding 107's reset investigation](../findings/107-storage-interrupt-emulation-scope.md#reset-investigation-and-controlled-checkpoint-decision)
for the primary driver links and limitations. Neither preserving, restoring the
reset default nor zeroing IRQSTATEN after RSTA is proved. No component tests,
builds or emulator runs were repeated for this evidence checkpoint.

**[D/O] Controlled Stage B storage-pair checkpoint is complete:** the user
approved explicitly initialized registers while deferring full RSTA fidelity.
`Machine::step_storage` uses Device Board/Time and the shared INTC bank for source
31/vector 223 and source 0/vector 192 (DMA59 within the DMA56–63 group). Higher
level wins, then higher source: level 3 ties deliver 223 then 192; DMA level 4
reverses the order, subject to IMR/ICR and CPU IPL. Device byte SIMR/CIMR covers
source 0 and 31. Other pending group channels fail explicitly; configured or
pending legacy delivery fails before mutation. `step_esdhc` stays isolated and
rejects live group requests. Ordinary stepping, Oracle and daemon defaults remain
unchanged.

Device Board sets INTH bit 27 only for DMA59 `done && major_interrupt`, and TC
only after actual data DMA completes. Device CINT NOP/CAIR/selected-channel and
contained INTH/INTL W1C preserve unrelated channels and DSPI/SSI dispatch; crossing
writes reject before mutation. Requests survive CPU entry until acknowledgement.
The new route avoids the legacy pending boolean and invalidates queued DMA writes
before execution/exception entry. Existing Board snapshot backing round-trips
pending state without a format change; Stage C runtime-policy compatibility
remains unknown.

All **28 machine cases** pass (pair 10, eSDHC 9, runner 4, SSI 5), and all **three
actual-firmware tests** pass with both exact DT2 1.17 and DN2 1.12 inputs set.
The new pair fixture performs a genuine guest SERQ59/CMD18 **512-byte copy** from
deterministic synthetic media, checked before IRQ entry and after both handlers.
Four cases per target/stock-patched combination produce **16 pair runs, 32 actual
ISR/RTE invocations and 48 waiter insertions**, bounded to 4,096 instructions per
run. These replay counts derive from fixed source loops/assertions, not individual
counts printed in the log. Older tests add 96 no-waiter and 16 isolated-delivery
invocations: **144 total across all three tests, not per image**.

Actual DMA CINT and eSDHC W1C acknowledge both lines. Vector-entry/RTE order,
context/status, semaphore coalescing, the circular three-node bidirectional ready
queue and scheduling-request bit 13 are checked; this pair fixture never executes
the scheduler.
Stock/patched selected observables match, with sticky raw errors retained by the
observer. Error bits are injected only after the copy completes: this is not
partial/corrupt DMA recovery. DN2 hooks are in-memory test-only; the packaged
manifest remains DT2-only. Independent static reviews reported no issues; no
**[V]** upgrade is claimed.

Logs: `out/runs/storage-fault-proof/stage-b-pair-tests.log` and
`out/runs/storage-fault-proof/stage-b-pair-firmware-tests.log`. The ignored
`out/runs/storage-fault-proof/stage-b-pair-invocation.json` records the actual
successful command/environment overrides including both opt-ins, fixture and
firmware hashes, shared lock, target and logs. It is recorded from the invocation;
the log alone cannot establish that opt-in inputs were configured. Exact hashes
and the command with both environment variables are in the
[adapter README](../../modules/core/adapters/storage-fault-proof/README.md).

Stage A remains incomplete on full RSTA. The later explicit software-force and
initialized-task continuation checkpoint below extends the pair proof. Physical
DMA quiescence, full scheduler/timer integration and native cold-boot fault
history remain open; full native fault-history acceptance is **failed/open**.
**Stage C is unstarted.** No public ABI, pool, SHARC, recovery, daemon integration
or hardware-flashing work follows. Runtime integration confidence remains low;
earlier calendar estimates remain withdrawn.

The coordinator's scoped ten-file `mise run test:changed` passed pathcheck and
layer checks. The full merge gate remains deferred and has not run. Documentation
pathcheck and diff checks covered that earlier prose/provenance update.


## Latest bounded storage/scheduler checkpoint (2026-10-31)

`step_storage_with_forced` explicitly combines the actual storage lines with all
software force sources across three banks (higher level, lower bank, higher
source), with IMR/ICR and live IPL. It rejects unsupported controller level masks
and level 7 before mutation. Pair-only/Oracle/default routes are preserved.
The 28 component tests pass; the added bank2-higher-level assertion passes in
its named-test rerun.

All four named actual-firmware tests pass with both exact firmware opt-ins. The
new scheduler test contributes 8 controlled stock/patched DT2/DN2 CC/CC+CTOE runs:
actual storage ISR -> bit-13 request -> actual scheduler -> initialized waiter
context -> ordinary guest marker instruction. Assertions include old-task save,
exact selected SP/SR/PC/register restore, singleton queue/semaphore state, force
clear and W1C; selected stock/patched results match and patched CTOE remains sticky.
Independent static review found no issues. Exact command, input/fixture hashes
and log: `out/runs/storage-fault-proof/scheduler-continuation-invocation.json` and
`scheduler-continuation-tests.log`.

This proves one initialized task continuation, not full scheduler/timer behavior
or native boot. The snapshot audit found Board/Time backing lacks policy and
construction binding; native boot hardcodes Oracle and checks firmware identity.
Runtime execution-policy compatibility remains unaudited, with no claim of an
exploited mismatch. Policy binding, full reset, physical-source integration and
native fault history remain open; no snapshot-format or daemon change is approved.
The coordinator's scoped `test:changed` passed pathcheck/layers before final prose;
final documentation pathcheck is coordinator-owned and the full gate is deferred.


**[D] Legacy runtime snapshot guard complete (2026-10-31):** native runtime
save/restore now rejects Device completion, Device Time or missing Time before
mutation. Both named tests pass on exact DT2 1.17/DN2 1.12: unchanged `DT2SNP01`
magic and byte-identical Oracle roundtrip, plus unsupported save/restore CPU,
register/RAM/deadline and counter preservation. Independent review found no issues.
Exact command/environment, input/final-source hashes and log are under
`out/runs/storage-fault-proof/legacy-snapshot-policy-invocation.json` and
`legacy-snapshot-policy-tests.log`.

This protects legacy Oracle snapshots without completing policy binding.
Reference/Softfloat interchange and topology compatibility remain unknown; Device
snapshots remain unsupported. No format/cache-key change or Device runtime option
was added. Future Device construction must start cold until separately validated.
Full-reset implementation and native integration remain unstarted; the full merge gate is deferred.

## Oracle timer collection prerequisite — 2026-10-31

The shared interrupt work now separates existing PIT/DTIM advancement from
accepted-entry delivery. `Time::collect_oracle_timer_requests` collects raw
Oracle vectors and queues DTIM REF host writes without delivery or SR mutation;
Device policy still rejects it. Thirty named timer/manual/Time tests and three
existing Time cache unit tests pass, including direct idle/blocked cache
invalidation checks without intervening writes. Commands and logs are under
`out/runs/storage-fault-proof/timer-collection*`. Independent final review and
coordinator verification passed; the full merge gate remains deferred.

Next implement a bounded Device event/status/acknowledgement component from the
independent manual audit before timing integration. PIT PIF and DTIM REF/CAP
must remain distinct from enabled requests and CPU entry; current Oracle
compatibility behavior does not establish hardware semantics. Repeated non-1588
DTIM REF rearming remains open. See [finding 107](../findings/107-storage-interrupt-emulation-scope.md)
and [the plan](../plans/dsp-sdk-and-sampler.md). No default-policy, snapshot,
native-boot, SHARC or PCM changes were made.

## Device PIT supplied-event component — 2026-10-31

Device PIT now latches supplied timeout status for all four channels and exposes
raw PIF && PIE requests without consumption, timing or delivery. Aligned word
PCSR writes apply PIF W1C; PMR writes clear PIF independently of OVW. Other
Device PIT write widths/registers reject before register mutation; Oracle remains
unchanged. The typed Time API rejects Oracle policy and invalid event channels.

Twenty-six named component/Oracle regression tests and all four actual-firmware
storage tests pass with both exact images. The initialized scheduler fixture now
seeds PIF with PIE disabled and proves it persists through storage ISR/RTE until
the actual scheduler's `$053f` write clears it. Storage/force arbitration still
excludes physical PIT requests. Commands/hashes/logs are under ignored
`out/runs/storage-fault-proof/pit-device-events*`. Independent review and
coordinator verification passed; the full gate remains deferred.

Next address Device DTIM's event/status contract, including the open repeated
REF rearming question, before automatic timing or shared/native arbitration.
No hardware, snapshot format, default-policy, SHARC or PCM change is included.
See [finding 107](../findings/107-storage-interrupt-emulation-scope.md).

## Device DTIM supplied-reference component — 2026-10-31

Device DTIM now latches supplied REF events and exposes reference-only CPU
requests from REF/ORRI/DMAEN, without timing or delivery. Bounded writes model
DTER REF/CAP W1C, supported ordinary configuration, DTRR storage and DTCN clear.
Capture/advanced modes, reset transitions, live nonzero CLK changes and unsupported
accesses reject. Only Device DTRR initialization changes to `0xffffffff`; Oracle
defaults and behavior remain unchanged. No capture, DMA, reset or rearm simulation.

Twenty-eight component/Oracle regression tests and all five actual-firmware
storage tests pass with both exact inputs. The new vector-99 handler-prefix case
runs four stock-only REF/REF|CAP cases across both images, observes actual DTER3
byte acknowledgement preserving CAP, and stops before JSR `0x40001896` within
64 instructions. It does not execute the queue helper or prove a full ISR/RTE.
Exact commands/hashes/logs are under ignored
`out/runs/storage-fault-proof/dtim-device-events*`. Independent review and
coordinator verification passed; the full gate remains deferred.

Repeated non-1588 REF rearming, timing/reset and shared/native arbitration remain
open. Storage/force stepping still excludes timer requests. See
[finding 107](../findings/107-storage-interrupt-emulation-scope.md).

## Shared supplied-event selection — 2026-10-31

Explicit `Machine::step_device_events` now combines supplied PIT/DTIM reference
requests, storage lines and all software-force bits through one INTC candidate.
It shares storage preflight/entry/cache handling and refuses invalid DTIM modes
before queued effects mutate. Entry preserves status; PIT and force aliases
remain pending until both assertions clear. Existing stepping routes are unchanged.

The targeted 66 tests and all five unchanged actual-firmware tests pass with both
exact firmware inputs. Commands/environment/hashes/logs are under ignored
`out/runs/storage-fault-proof/device-event-arbitration*`. Independent review and
coordinator verification passed; the full gate remains deferred.

This explicit subset advances no timers. Timing/rearming, physical SSI/DSPI,
host input, batching and native integration remain open; the firmware rerun proves
regression preservation rather than new-route firmware acceptance. See
[finding 107](../findings/107-storage-interrupt-emulation-scope.md).

## Device SSI50 latch and shared selection — 2026-10-31

Successful explicitly supplied Device SSI TX50 major completions with original
INT_MAJOR now latch sticky/coalescing INTH bit 18. Only `step_device_events`
includes that raw vector-170 source; CPU entry preserves it until guest CINT or
INTH W1C acknowledgement. Force 191 remains independent. Oracle SSI and the old
storage routes are unchanged; no automatic SSI service or native policy enabled.

Device acknowledgements synchronize SSI diagnostics; CINT NOP also preserves
legacy DSPI pending state. Device RX ranges queue decode invalidation and paired
tags; Machine drains both on successful ownership while rejected preflight keeps
them. All 61 named machine tests and five unchanged firmware tests pass with
both exact inputs. Independent review found no issues; coordinator scoped
path/layer and whitespace checks passed. Correct-target final commands/hashes/logs and the separately archived
nonfinal mistargeted attempt are recorded under ignored
`out/runs/storage-fault-proof/device-ssi-events*`.

Native SSI/service batching, automatic timer semantics, DSPI/host-input delivery
and full fault-history acceptance remain open; full gate deferred. See
[finding 107](../findings/107-storage-interrupt-emulation-scope.md).

## Device INTC mask-command bytes — 2026-10-31

Device SIMR/CIMR now cover all three banks/sources 0–63, SALL/CALL and zero byte
readback. Reserved bit 7 and wrong-width/overlapping accesses reject before Time
register/cache mutation. Oracle passthrough, IPR and controller level masking are
unchanged. All 72 named tests pass (15 INTC, four mask-command, eleven Time,
23 storage pair, nine eSDHC, ten SSI), plus four Time cache unit tests.
Commands/hashes/logs: `out/runs/storage-fault-proof/intc-mask-commands*`.
Independent review found no issues; coordinator scoped path/layer and whitespace
checks passed. No firmware rerun was
required by coordinator decision; prior firmware passes are prior evidence.

Future private mixed-policy diagnostics may retain explicit host peers but must
preserve full fault-history acceptance. Synthetic 86-accepted→65 creation needs
lifecycle/batching proof. Reject USB MIDI injection, including prequeued/active
callbacks, until a distinct admission contract exists: USB fixed level 5 bypasses
ICR/IMR despite sharing vector 156 with DIN; it is not DIN source 28. Full EPORT
is not a prerequisite; full RSTA remains open. No native admission implementation,
Oracle USB, snapshot or default-policy change is included; full gate deferred.
See [finding 107](../findings/107-storage-interrupt-emulation-scope.md).

[D/O] New manual evidence supports the next ordinary DTIM timing direction:
p1169 FRR restart and p1171–p1172 §39.4.1 repeat five REF polls with DTER-2
clearing only, after one enable. Table 39-4's generic rearm prose still conflicts;
reading it as EN1588-specific is consistent with §39.2.4 but is interpretation,
not hardware readback. Eq. 39-1 supplies the timeout formula. No clock implementation,
bus-frequency/phase or complete firmware DTXMR-history proof is included.

## Device DTIM supplied completed ticks — 2026-10-31

Ordinary Device restart mode now accepts explicitly supplied post-prescaler ticks,
updating existing DTCN/REF registers only. Manual p1166 gives DTRR+1 intervals;
p1163 FRR resets immediately, supporting completed-tick DTRR=2 phase
0→1→2→0+REF. No subcycle/MMIO-edge or hardware cycle-accuracy claim follows.
Active free-run/external/DMA/advanced modes and DTCN>DTRR reject before mutation;
inactive RST/CLK preserves state, with advanced-mode validation first. ORRI gates
requests only. CAP survives and repeated periods coalesce without new state.

All 60 named tests pass (six peripheral tick, five peripheral event, ten manual,
five machine DTIM, eleven Time, 23 storage pair). Commands/hashes/logs are under
`out/runs/storage-fault-proof/dtim-device-ticks*`. Independent review found no
issues; coordinator scoped path/layer and whitespace checks passed. No firmware/native consumer, clock conversion, implicit
write advancement or snapshot format change is added. Bus rate/prescaler phase,
full firmware DTXMR mode history, reset and native integration remain open; full
gate deferred. See [finding 107](../findings/107-storage-interrupt-emulation-scope.md).
